# ADR 0037: Standalone peer ingress uses Tokio and per-connection ownership

Status: Implemented; hosted and live validation pending

Amended 2026-10-13: readiness-driven Tokio tasks replace the prescribed SPSC
producer-to-worker topology. SpeexDSP supplies adaptive packet jitter buffering
only through a separately versioned adapter. Implementation and local composed
validation are complete; this is not a deployed or release-verified change.

## Context

The planned standalone controller must remain responsive with hundreds or
thousands of connected peers. Work waiting on one connection must not serialize
unrelated connections or block audio callbacks. The shared PCM ring is
single-producer/single-consumer; calling its producer concurrently does not
become safe merely because its cursors are atomic.

This decision applies to the planned standalone application. It does not
require changing locks, threads, or behavior in the current Asterisk-hosted
`app_rpt_advanced`. ADR 0036 separately governs independence from Asterisk's
thread model.

## Decision

Prefer exclusive ownership, message passing, atomics, and immutable snapshots
over locks, mutexes, spinlocks, or equivalent contention gates wherever a
thread-safe alternative exists. Do not replace a mutex with a spinlock or
retry loop that waits for another producer to release the same resource.

Use **Tokio readiness and one serialized asynchronous owner per connection**:

1. A bounded shared Tokio worker pool handles socket readiness and asynchronous
   packet processing. There is no runtime or OS thread per peer, unbounded
   task-per-packet spawning, global media worker, or busy-spin polling loop.
2. Resolve a packet by endpoint, IAX call identifiers and session generation.
   Admit owned, bounded records to that connection's bounded mailbox without
   waiting for its handler or for mailbox space. A full mailbox rejects/counts
   the new packet; another peer must continue receiving and making progress.
   Records preserve arrival time and protocol order metadata, never a pointer
   into a reused socket buffer.
3. One asynchronous task exclusively owns that connection's session, jitter
   context, decoder and PCM producer. Concurrent callbacks may submit records
   but may not write PCM. Bound work per turn and service playout, retry,
   negotiation, DTMF and egress deadlines even while no packet arrives.
4. A narrow per-connection asynchronous mutex is permitted when necessary for
   direct concurrent handler entry. It suspends only the contending task; it
   cannot block an executor thread or guard other connections. Prefer the
   mailbox owner rather than adding both mechanisms. Mutex acquisition order
   is not proof of packet arrival order. No global lock is held through media
   processing, authorization, DNS or an await.
5. Network I/O, protocol parsing, adaptive packet playout, codec decoding and
   controller policy remain distinct boundaries. Reliable control ordering
   must not discard recoverable out-of-order voice before jitter admission.
   Protocol and timer events use the same connection owner, not a second writer.
6. Audio callbacks independently consume PCM without entering Tokio, acquiring
   ingress locks, decoding packets or waiting on producers. Their work remains
   bounded, allocation-free and strictly lock-free. Tokio and non-real-time
   ingress serialization are not claimed to be formally lock-free internally.
7. Retained callbacks require explicit registration, detachment and quiescence;
   do not retain a synchronous borrowed callback for future async delivery.
   Gate admission and timers, invalidate stale generations, await peer task
   quiescence off audio, then release contexts and libraries under ADR 0026.
   A migrating task must not retain a thread-specific hazard across an await.

Reuse established Tokio readiness, bounded mailbox and timer facilities; do not
write a new executor or queue algorithm. Tokio is an internal Rust dependency
under ADR 0018. Existing real-time SPSC queues retain their single-owner rules.
The independent control-path adapter in ADR 0038 does not schedule media work.

### Adaptive jitter adapter

Use SpeexDSP's generic adaptive jitter buffer through a neutral packet-jitter
capability under ADR 0022. Only the separate Rust `cdylib` adapter dynamically
links SpeexDSP, owns its FFI/context and translates errors. Its versioned C
descriptor exposes bounded timestamped packet admission, due retrieval,
playout advancement, reset, diagnostics and lifecycle; it exposes no native
SpeexDSP types, raw control opcodes or API escape hatch. One context belongs
to each peer owner; contexts do not share a global processing lock.

The adapter uses packet timestamp/span and arrival timing to balance buffering
delay against lateness and recover reordered packets before playout deadlines.
It returns explicit packet, missing-span or insertion decisions, not decoded
or synthesized PCM. Initial/minimum/maximum delay, bounded overflow and delay
adjustment policy must be recorded and tested before media cutover. SpeexDSP's
fixed packet capacity is not the latency bound, and its native full-buffer
eviction is distinct from the mailbox's reject-new policy.

The approved initial policy is 40 ms minimum, 60 ms initial delay and 500 ms
maximum, with SpeexDSP's automatic lateness trade-off. These are packet-jitter
limits, not the shared PCM ring's reserve, target or capacity.

The existing shared PCM ring remains the sole source-rate converter, drift
corrector and selected PLC owner. Missing/insertion spans must preserve their
position among earlier and later PCM; omitting a write or appending zeros is
not a loss-span contract. Do not duplicate PLC or silently compress the media
timeline. The owner approved an ABI-3 descriptor-tail `ring_producer_gap`
operation to append counted missing source samples in order, without breaking
existing callers. New consumers require its presence; prove placement through
converter lookahead and pending buffers. The approved samplerate ABI-2 tail
reports source-position delay, including FIR lookahead and compensation, so
the ring can place each gap at the correct output position without flushing
the converter or moving PLC into it. Both existing descriptor prefixes and
SONAMEs remain unchanged.

Prefer reducing jitter delay between transmissions and validate any adjustment
during speech; the generic buffer does not itself perform smooth audio
time-stretching.

Standalone peer diagnostics use the optional host-services `peer_observe` tail.
The product calls it only from that peer's serialized owner step, copies the
versioned source-rate jitter and ingress counters to atomics, and reads those
copies for control-plane status. No status request borrows the live IAX peer,
and no audio callback observes the jitter adapter or an ingress queue. Older
host tables and Asterisk continue without this optional observation group.

The complete standalone manifest requires the jitter adapter. Startup fails
for a missing/incompatible provider; configuration reload revalidates retained
providers and preserves the old generation on failure. Adapter code replacement
requires a controlled process restart after tasks and contexts have stopped.
The Asterisk composition keeps its current jitter implementation/dependencies.

## Consequences and verification

This supersedes the earlier mandatory standalone SPSC fan-in and blanket
non-audio mutex prohibition in ADRs 0002, 0025 and 0026. Their lock-free audio
and single-writer PCM requirements remain unchanged. The Asterisk-hosted
ingress exception remains; this decision does not rewrite the deployed module.

Test overlapping callbacks, a stalled peer, full mailboxes, timestamp wrap,
reordering, missing-span placement, adaptive delay, timer-only progress and
peer retirement with queued work. Verify one writer per peer PCM ring,
nonblocking audio consumers, bounded storage and independent peer progress.
Exercise hundreds and thousands of simulated peers and report CPU, memory,
queue drops and service latency. These are scaling
validation requirements, not a claim that every hardware target can carry
thousands of simultaneous audio streams.

The standalone Tokio/jitter cutover and its ordered-gap prerequisites pass local
composed tests. New and changed production scopes have 100% line and branch
coverage; synchronous Asterisk boundary tests also pass. Hosted native-platform
validation, dependency publication and live-node testing remain outstanding;
no release-readiness claim is made.

## Reference

[Tokio UDP](https://docs.rs/tokio/latest/tokio/net/struct.UdpSocket.html),
[bounded mailbox](https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html), and
[async mutex](https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html)
document the selected non-real-time primitives.
[SpeexDSP's generic jitter API](https://github.com/xiph/speexdsp/blob/SpeexDSP-1.2.1/include/speex/speex_jitter.h)
is private to the jitter adapter. The per-peer ownership and timeline handoff
above are this project's integration contract.
