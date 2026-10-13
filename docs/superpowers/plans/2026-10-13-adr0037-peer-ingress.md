# ADR 0037: Tokio per-connection packet ingress implementation plan

> **For agentic workers:** Use `superpowers:executing-plans` or
> `superpowers:subagent-driven-development` to implement this plan task-by-task.
> Steps use checkboxes for tracking. Implementation is approved; publication
> and node deployment require separate authorization.

**Goal:** Receive and process packets for many standalone connections
concurrently, serializing only work for the same connection while audio
callbacks continue reading each inbound PCM ring without waiting.

**Architecture:** Tokio drives asynchronous socket reception and per-connection
packet handlers on one bounded shared worker pool. Each connection has its own
serialized protocol, jitter-buffer, decoder and PCM-producer state; the
hardware-paced transmit callback remains the independent lock-free PCM consumer.
SpeexDSP's generic adaptive packet jitter buffer is accessible only through a
separate versioned jitter-adapter shared object. Do not serialize all connections
through one media worker or control executor.

**Tech stack:** Rust, Tokio (`net`, `rt-multi-thread`, `sync`, `time`), the existing
versioned `librptadviax2` provider and codec boundary, a Rust `cdylib` jitter
adapter dynamically linked to released SpeexDSP, and the released rate-adjusting
PCM ring. Select a Tokio release compatible with the workspace's
Rust 1.85 floor and record it in the lockfile; do not silently raise the floor.

**Spec:** The owner's 2026-10-13 clarification below revises the implementation
method in [ADR 0037](../../../doc/architecture/decisions/0037-standalone-lock-free-peer-ingress.md).
ADRs 0002, 0018, 0021/0022, 0025/0026, 0035/0036 and 0038 retain their audio,
dynamic-boundary, lifetime, rate and control-path requirements except for the
specific standalone ingress-serialization amendment described here.

## Clarified intent and chosen serialization

- Receipt of a datagram schedules asynchronous processing; the socket receiver
  must not await completion of that peer's handler before receiving another
  peer's packet. Tokio drives futures rather than invoking a C callback itself.
  The adapter invokes the asynchronous packet handler when work is ready.
- Resolve the connection using the UDP endpoint, IAX call identifiers and
  session generation, not just a node number or source IP. Parse/validate the
  envelope, route the packet to that connection's jitter/session state, then
  decode due voice payload into that peer's inbound PCM ring. Protocol parsing
  and codec decoding remain distinct operations.
- Different peers may execute concurrently. A stalled handler, full queue, or
  same-peer serialization wait must not impose head-of-line blocking on another
  peer. This is an ownership/scheduling guarantee, not unlimited CPU capacity or
  a hard real-time guarantee from Tokio or UDP.
- **Preferred minimal serializer:** one bounded packet mailbox and one async
  handler task per live connection. Concurrent receive callbacks submit owned
  packet records using non-waiting admission; the handler consumes them serially.
  A task is not a dedicated OS thread or a runtime per peer. This avoids sharing
  a PCM producer or depending on the execution order of separately spawned
  packet tasks.
- A narrowly scoped **per-connection asynchronous mutex is permitted** by the
  owner's clarification if direct concurrent handler entry makes it necessary.
  It must suspend only the contending task, never block a runtime worker thread,
  protect another peer, or be acquired by an audio callback. Prefer the mailbox
  owner above; do not add both serializers without a concrete need. A mutex's
  FIFO acquisition alone does not establish datagram arrival order.
- No unbounded task-per-packet spawning, global decode mutex, busy-spin gate,
  per-peer OS thread, or await-on-full admission in a shared socket receive loop.
  Bound peer admission, retained datagrams and active work. A full peer mailbox
  rejects/counts the new packet while other peers keep making progress.
- Tokio and any ingress serializer are non-real-time implementation details;
  do not describe their internals as formally lock-free. **Audio callbacks and
  PCM-ring exchange remain strictly lock-free, bounded and allocation-free.**
  Audio owns the consumer independently; it neither takes the producer's
  mutex nor enters Tokio, waits for a packet, or decodes network data.

```text
Tokio UDP readiness -> receive / identify connection / bounded admission
                         |                         |
                 connection A handler      connection B handler ...
                 protocol validation       protocol validation
                         |                         |
              jitter-adapter context A   jitter-adapter context B
                 (SpeexDSP private)        (SpeexDSP private)
                         |                         |
                 due codec decode          due codec decode
                         |                         |
                 A inbound PCM ring        B inbound PCM ring
                         +------------+------------+
                                      |
                         native transmit callback
                         lock-free read / mix / output
```

## Selected jitter algorithm and adapter boundary

- Adopt **SpeexDSP's generic adaptive jitter buffer**, not its speech codec,
  a custom jitter algorithm, PJMEDIA, or the full WebRTC NetEq pipeline. It
  orders opaque media packets by timestamp/span and adapts delay using recent
  arrival timing and the cost of lateness versus buffering.
- Add `rust/jitter-adapter/` to the existing workspace, following the resolver
  adapter's separate-artifact pattern. Export `librptadv_jitter_adapter.so.1`,
  capability `rptadv.jitter`, through
  `rptadv_jitter_adapter_descriptor_v1`. Ship separate runtime/development
  packages. This is a dynamically replaceable capability, not an internal
  helper that lets the protocol library call SpeexDSP directly.
- Only this adapter owns SpeexDSP headers, FFI types, `jitter_buffer_*` calls,
  allocation/destruction and error translation. Controller, protocol, network,
  codec and PCM-ring components see only its neutral C-compatible descriptor.
  No raw SpeexDSP handle, control opcode, Rust layout, Tokio type or generic
  native-API escape hatch crosses that boundary. Do not vendor SpeexDSP or
  statically link it into consumers.
- The descriptor identifies ABI and structure size and supplies `create`,
  `destroy`, `reset`, `put`, `get`, `advance` and `observe` operations over opaque
  per-connection handles and explicit bounded buffers. `put` copies borrowed
  payloads before returning. `get` writes to caller-owned storage and returns
  a neutral `packet`, `missing`, `insertion` or `not_due` result, with timestamp,
  duration and useful offset metadata; errors are separate from media results.
  `advance` accounts for elapsed playout and downstream buffered duration once,
  without exposing SpeexDSP's tick/update convention to callers.
  Downstream duration means decoded lookahead not yet published to the PCM ring;
  it excludes that ring's reserve, target and total occupancy. A proposed delay
  exceeding 500 ms resets the packet epoch, counting discarded packets/spans
  and the reset; ordinary in-bound queued media is preserved.
- Options use media-clock units for packet timestamps, spans, delay margin,
  adjustment step and concealment quantum; use monotonic arrival/deadline
  metadata, explicit maximum delay and bounded packet storage. The IAX owner
  reconstructs full/mini timestamp wrap before admission. Protocol sequence
  handling remains outside the adapter; it must not discard recoverable media
  reordering before the jitter buffer can use it. Retain receiver arrival
  metadata through async queues so executor batching is not mistaken for wire
  jitter; test the translation to SpeexDSP's playout-relative timing model.
- The serialized peer task exclusively owns that peer's context, including
  insertion, due retrieval, delay updates and reset. Independent contexts run
  concurrently without a global adapter lock. No jitter operation, allocation,
  destructor or library loading runs in an audio callback.
- The adapter reorders packets and supplies playout decisions only. Codec
  decoding stays behind the existing codec boundary; source-rate conversion,
  drift correction and selected PLC stay with the shared PCM ring. Missing and
  delay-insertion spans must survive integration explicitly: neither append
  later audio over their time slots nor add a second PLC implementation.
- SpeexDSP's generic buffer adjusts delay through insertion/skipping, not
  smooth audio time-stretching. Prefer delay reduction between transmissions;
  validate necessary active-speech changes. Enforce the approved latency bound
  in the adapter because SpeexDSP has no simple maximum-delay control. Its
  fixed 200-packet storage is an implementation ceiling, not a latency policy.
- Approved starting policy: 40 ms minimum, 60 ms initial, **500 ms maximum**,
  with SpeexDSP's automatic lateness trade-off. These are not ring settings.
- Standalone's manifest requires the jitter descriptor and retains the DSO until
  peer tasks and contexts are quiescent. Missing/incompatible providers fail
  startup; failed reload keeps the previous complete generation. Replacement
  of the standalone DSO requires a controlled process restart. Ordinary
  configuration reload revalidates the retained descriptor and never loads
  replacement code; no running peer or audio callback replaces an adapter.
  Asterisk keeps its existing jitter path and gains no SpeexDSP dependency.

## Current code and necessary changes

Paths are relative to the sibling repositories under `2026-08-30`.

| Current boundary | Change required |
| --- | --- |
| `librptadviax2/src/network.rs`, `UdpEndpoint` | Replace readiness polling with Tokio socket readiness; retain separation from protocol parsing. |
| `librptadviax2/src/server.rs`, `InboundIaxListener` | Route established calls without waiting for a peer; separate potentially slow new-call authorization from socket draining. |
| `librptadviax2/src/ingress.rs` | Reuse owned bounded datagram records and call routing; retain arrival time and validate the session generation before processing. Replace the SPSC producer topology where concurrent callbacks require it. |
| `librptadviax2/src/client.rs`, `IaxPeer::poll_transport_event` | Move protocol/codec work into the serialized per-connection handler; service timers without waiting for packet arrival. |
| New `rpt_advanced/rust/jitter-adapter/` and provider `librptadviax2/src/jitter.rs` | The separate adapter alone calls SpeexDSP; the provider's neutral client owns per-peer descriptor handles and supplies due payloads to the existing decoder. |
| `rpt_advanced/rust/standalone/src/{iax,host_services,foreground}.rs` | Replace synchronous polling bridges for standalone with explicitly retained asynchronous media registration and safe detachment. |
| `rpt_advanced/rust/product/src/link/session.rs`, `PeerIoWorker` / `PeerSession` | Remove standalone receive dependence on the single round-robin `rpt-peer-io` thread; reuse event, keying, DTMF, egress and redirect logic under the connection's serial ownership. Preserve the Asterisk execution path. |
| `rpt_advanced/rust/product/src/{host,services}.rs` | Bind the producer to the exact peer generation and retain callback/library lifetimes through stop and reload. |
| `rpt_advanced/rust/product/src/link/ring.rs` | Reuse the noncloneable producer and separate consumer; no multi-writer PCM API or replacement ring. |

Three prerequisites must not be hidden by naming:

1. The current standalone provider decodes accepted voice on arrival. Its IAX
   sequence validation and packet queue are **not a timed jitter buffer**.
   Add per-connection SpeexDSP-backed state through the selected jitter adapter;
   do not label the elastic PCM ring or an arrival FIFO a jitter buffer. The
   algorithm/adapter choice and starting 40/60/500 ms limits are approved.
   Record and test bounded overflow behavior without silently borrowing ring
   reserve/target as packet-jitter settings.
2. Existing IAX and host-services descriptors expose synchronous polling and
   borrowed callbacks. In particular, `PeerIo::read` passes stack-owned callback
   state. It must not be retained for later async delivery. Define a versioned
   registration/detachment contract with explicit context lifetime if the current
   ABI cannot represent it. Keep futures and Tokio types inside the Rust adapter;
   the external boundary remains a C-compatible descriptor. Update all affected
   consumers and package checks together.
3. `InboundProducer::write` currently publishes contiguous PCM only; the product
   binding has no timestamped missing-span operation. Prove how packet loss and
   delay-insertion spans reach the existing ring's PLC at their correct position
   even while older PCM remains buffered. Simply skipping a write, writing
   zeros, or waiting one timer period is not proof. Use the approved compatible
   descriptor-tail `ring_producer_gap(ring, input_samples, accepted)` extension.
   Preserve ABI 3, its prefix and SONAME; new consumers require the added field.
   Prove ordering through converter-held/pending samples before integration.

## Global constraints

- Standalone only: preserve Asterisk-hosted behavior, public protocol behavior,
  authentication, codec negotiation, link policy and current configuration.
- Keep network I/O, IAX protocol state, codec implementation and controller policy
  in their existing boundaries. No new codecs, EchoLink, telephony or wishlist
  features. Tokio is an internal Rust dependency of its owning artifact under
  ADR 0018, not a new general-purpose service or control-path backend. SpeexDSP
  is an external C dependency confined to its separate ADR 0022 adapter.
- Jitter/decode produces canonical F32 at the negotiated codec rate. The existing
  peer ring alone converts to native 48 kHz and handles drift/selected PLC. Do not
  add another resampler, PCM staging ring, PLC algorithm, or change ring settings.
- Preserve receive-tail drainage, courtesy timing, `NEWKEY` negotiation, source
  identity, DTMF, mix-minus, topology and reliable IAX retransmission behavior.
- All packet records own their bytes, arrival metadata and generation. No borrowed
  reusable receive buffers, concurrent producer calls, stale-session publication
  or state reclamation while a callback/task still holds it.
- Same-peer protocol and media operations are ordered. Minimal routing may be
  shared, but no global registry lock is held through decode, jitter work or an
  await. Async authorization/DNS cannot hold up established media reception.
- Bounded work per async turn; service jitter deadlines, reliable retries and
  existing media/control timers while no packet arrives. Yield between bounded
  batches so a busy peer cannot monopolize the shared executor. No blocking
  network/DNS/file work in a packet handler or audio callback.
- Debian 13 amd64/arm64 tests; 100% production line/branch coverage on amd64 only.
  Use maintained `latest` quality images. Targeted checks locally, full gate on
  GitHub before merge. No node change or external connection without approval.

## Review focus

1. Same-peer overlap versus cross-peer progress: hold A's handler and prove B
   processes packets and both PCM consumers still return without waiting.
2. Packet ordering and final tails: reordered/duplicate/full/mini frames, timestamp
   wrap, missing/insertion spans and an unkey must not scramble, compress, double
   conceal or replay buffered voice on the next key.
3. No new packet: jitter playout, retransmission, negotiation, DTMF expiry and
   current egress timing must continue at their deadlines.
4. Reload/disconnect with pending work: no stale generation or borrowed callback
   context may survive detachment into a replacement connection/ring.
5. Overload: one peer's full mailbox or slow authorization cannot fill unbounded
   tasks or stop unrelated peers; drops and occupancy remain observable.

## Task 1: Align contracts and record the jitter/ABI prerequisites

Implementation checkpoint (2026-10-13): all six code slices are present on the
working branches and local targeted validation passes; an unchecked compound
step below can still include an outstanding commit, publication, or hosted check.
It does not mean the corresponding implementation needs to be written again.

- [x] Amend ADR 0037 and its references in ADRs 0002, 0025, 0026, 0038, the
  architecture/index and standalone wishlist section. Replace the mandatory
  producer-to-worker SPSC topology and blanket non-audio mutex prohibition with
  the scoped Tokio/per-connection contract above. Retain strict audio guarantees
  and mark this implementation pending, not implemented.
- [x] Record the per-connection jitter playout policy and approved values; resolve
  the policy prerequisite above before changing timing behavior. Add the approved
  SpeexDSP-through-jitter-adapter decision to ADRs 0022/0037 and the architecture
  map; do not describe it as a direct protocol-library dependency.
- [x] Resolve the timestamped missing/insertion-span handoff to the shared ring,
  with a deterministic gap-followed-by-audio test. Record which component owns
  each timeline advance and prove that exactly one PLC path fills each gap.
- [x] Specify provider start/register/detach/stop ownership and event/timer
  delivery in the IAX header (adding the currently omitted server table) and
  product host-services contract. Audit `rptadv_iax2_client.h` and
  `rptadv_product.h`; version only tables actually changed and reject mismatched
  combinations before callbacks.
- [x] Add failing ABI/lifecycle tests: no async use of `PeerIo::read`'s borrowed
  context, no callback after detach completion, and no library unload before
  outstanding work finishes. Reuse descriptor/lifecycle fixtures.
- [x] Run affected ABI tests, implement only that ownership boundary, rerun them,
  update developer documentation and commit the buildable prerequisite.

## Task 2: Deliver the versioned SpeexDSP jitter adapter

**Files:** create `rust/jitter-adapter/{Cargo.toml,build.rs,wrapper.h}`,
`rust/jitter-adapter/include/rptadv_jitter_adapter.h` and
`rust/jitter-adapter/src/{lib,tests}.rs`. Modify workspace/build/install rules,
`debian/control`, new `debian/librptadv-jitter-adapter{1,-dev}.install`,
standalone provider-manifest code/tests, and the existing companion quality
image/package workflows. Reuse the resolver adapter's scaffolding and ABI checks.

**Interfaces:** produce the neutral ABI-1 descriptor/operations above, backed
by dynamically linked system SpeexDSP. The standalone loader retains it and
passes it to the IAX provider at setup; no consumer discovers a second copy or
loads a library while processing a packet.

- [x] Add failing adapter contract tests for ABI/capability/required-function
  rejection, payload-copy ownership, bounded result buffers, reset/destruction,
  independent peer contexts, fake-adapter substitution and dependency absence.
  Add deterministic traces
  for increasing/decreasing jitter, reordering before and after deadlines,
  duplicates, timestamp wrap, partial/overlapping spans and signed start offsets,
  missing/insertion durations and latency bounds. Distinguish reject-new mailbox
  overflow from the adapter's recorded jitter overflow policy; upstream native
  insertion has no result and may evict its oldest packet at capacity.
- [x] Run only the new adapter tests and verify the expected failures; implement
  the descriptor and minimum SpeexDSP translation, then rerun those tests.
  Preserve exact due-time/span results, not just packet counts. No new codec,
  custom adaptive algorithm, resampler or concealment implementation.
- [x] Add staged-install/ABI tests proving only the jitter DSO directly links
  `libspeexdsp`, consumers use its descriptor, standalone requires it, Asterisk
  does not require it, and no task survives context/DSO destruction.
- [ ] Add `libspeexdsp-dev` to the adapter build/quality inputs and the dynamic
  SpeexDSP runtime dependency to its runtime package; ship header and pkg-config
  ABI metadata in the development package. Update actual adapter ABI inputs in
  consumer package checks and freshly published native quality images.
- [x] Run `cargo test -p rptadv-jitter-adapter` in the maintained quality image,
  the affected package checks and fast preflight. Record scoped coverage and
  developer documentation, then commit the independently buildable adapter.

## Task 3: Implement Tokio ingress and per-connection serialization

**Files:** provider `src/{network,server,ingress,client}.rs`, their existing tests,
and `Cargo.toml`/`Cargo.lock`. Keep the protocol engine free of socket operations.

- [x] Add deterministic failing tests using barriers/notifications, not sleep
  guesses: `same_peer_callbacks_have_one_writer`,
  `stalled_peer_does_not_block_other_peers`,
  `full_peer_mailbox_does_not_block_socket_receive`,
  `datagrams_keep_arrival_order_and_call_identity`, and
  `retired_generation_cannot_publish_after_slot_reuse`.
- [x] Run only those provider tests and record the expected failures.
- [x] Add one shared Tokio runtime, readiness-driven UDP receive, bounded
  per-connection mailboxes and serialized asynchronous packet handlers. Reuse
  existing call routing/parser/codec code; remove obsolete polling/queue code
  when no selected path needs it. Preserve bounded packet size and admission.
- [x] Ensure both shared-listener incoming calls and outbound-call sockets use
  the same handler contract; no second producer path for one connection.
- [x] Run the targeted tests, format/lint/static checks and commit.

## Task 4: Drive adapter-backed timed jitter playout into the existing peer ring

**Files:** provider `src/jitter.rs`, media/session/client implementation and tests; product
`rust/product/src/link/{session,ring}.rs` and their existing tests.

- [x] Add failing tests for reordered/duplicate packets, due playout without new
  packets, the recorded bounded late/overflow policy, full/mini timestamp wrap, buffered tail
  drainage after unkey and no stale tail on the next key. Assert PCM sample order,
  one writer, counters and the recorded jitter policy, not merely queue lengths.
- [x] Add tests where a missing packet is followed by available audio while the
  PCM ring is nonempty, jitter delay changes during/between transmissions, and
  the async executor receives packets in a batch. Assert preserved missing-span
  position/duration, single PLC ownership and no double-counted downstream delay.
- [x] Keep jitter/session mutation, decoder state and PCM publication inside one
  connection's serialized handler. A deadline wake uses that same owner; it must
  not become a second writer. Use only the jitter descriptor client for packet
  admission and due retrieval, decode returned payloads, and publish through the
  verified packet/gap handoff without waiting for the consumer. Account for ring
  occupancy in source-clock units; do not let two delay controllers chase the
  same buffered duration. Preserve existing ring overflow/PLC policy and settings.
- [x] Publish current jitter delay, buffered packet duration, late/duplicate/
  overflow drops, missing spans and delay adjustments outside the audio callback.
  Distinguish network loss from admission drops, scheduling lateness and ordinary
  end-of-burst silence; unkey must wait for valid queued media to drain.
- [x] Keep output reads independent. A test holds producer serialization while
  repeatedly invoking `InboundConsumer::render` and proves bounded completion;
  neither the audio callback nor the ring consumer acquires a peer mutex.
- [x] Run affected provider media and product ring/session tests, record scoped
  coverage, update developer docs and commit.

## Task 5: Wire standalone without serializing unrelated peers again

**Files:** standalone `src/{iax,host_services,foreground}.rs`; product
`rust/product/src/{services,host}.rs`, `link/session.rs` and their tests.

- [x] Add failing integration tests proving callbacks reach the correct product
  producer without a shared all-peer polling owner; retain existing synchronous
  Asterisk contract tests and owner-exclusive protocol send behavior.
- [x] Register each prepared peer generation with the async backend and route
  media directly through its serialized handler to that producer. Submit only
  copied controller events to station control; media must not wait for it.
- [x] Preserve existing command, key-negotiation, DTMF-expiry and egress deadlines
  outside packet arrival. Replace standalone's active peer/listener polling,
  not the audio callback cadence or the control-path adapter.
- [x] Gate admission on disconnect/reload, invalidate stale work, detach and await
  non-audio quiescence before releasing contexts/DSOs. An async task may migrate
  threads; do not retain a thread-specific hazard slot across an await. Preserve
  ADR 0026's endpoint-generation and audio adoption guarantees.
- [x] Run standalone loopback-IAX, reload/stop and product session tests plus
  Asterisk boundary regressions. Update required artifacts/packages for the
  actual ABI changes, developer docs and commit.

## Task 6: Prove bounded concurrency and finish documentation

- [x] Exercise 100 and 1,000 simulated connections on capable test hosts, including
  one stalled/flooding peer. Record CPU, memory, per-peer queue drops/occupancy and
  service latency; do not claim inexpensive hardware can sustain all active
  streams. Verify thread count follows runtime workers, not connection count.
- [x] Use deterministic progress assertions for isolation and single-writer
  safety; record timing measurements as measurements rather than flaky CI limits.
- [x] Replay recorded/synthetic variable-jitter traces through the actual
  SpeexDSP-backed adapter and codec/ring chain. Check recovered reorderings,
  bounded latency, lost-span placement, adaptation during continuous speech and
  unkey tail drainage; no live-network dependency in automated tests.
- [ ] Close affected production coverage gaps using scoped reports and the
  ignored `.work/quality-progress.md` checklist. Run relevant Rustdoc and fast
  checks before push; update user documentation before a PR.
- [ ] Run the hosted full gate on both native platforms, independently review the
  combined diff and update ADR implementation status only from verified results.
  Deployment and live-link validation require separate user approval.

## References and planning status

- [Tokio UDP socket API](https://docs.rs/tokio/latest/tokio/net/struct.UdpSocket.html)
  supplies asynchronous readiness/receive; sharing the socket does not require
  a socket mutex.
- [Tokio async mutex](https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html)
  is a permitted peer-local alternative, never an audio-path primitive.
- [Tokio runtime](https://docs.rs/tokio/latest/tokio/runtime/index.html) provides
  the shared executor; bounded nonblocking task work remains our responsibility.
- [SpeexDSP generic jitter API](https://github.com/xiph/speexdsp/blob/SpeexDSP-1.2.1/include/speex/speex_jitter.h)
  defines timestamped opaque packets, missing/insertion results and buffered-span
  accounting. These are private implementation details of the selected adapter.
- [SpeexDSP adaptive jitter implementation](https://github.com/xiph/speexdsp/blob/SpeexDSP-1.2.1/libspeexdsp/jitter.c)
  documents the delay/late-packet tradeoff and bounded packet storage; use a
  released dynamic library rather than copying this implementation.

The owner approved execution, the 40/60/500 ms starting policy, the compatible
ordered-gap ring extension, and the required compatible samplerate-adapter tail.
The latter reports the next output's source position, including FIR lookahead
and compensation; no converter flush or second concealment implementation is
introduced. Existing descriptor prefixes and SONAMEs remain unchanged.

Local prerequisite commits are samplerate adapter `0a5f0f9` and shared ring
`5b2a88c`. Their production coverage is respectively 291/291 lines and 64/64
branch arms, and 1024/1024 lines and 218/218 arms. The SpeexDSP adapter has
553/553 lines and 154/154 arms. A real jitter-to-ring C trace compares seven
960-sample output blocks against a chronological reference and verifies the
intervening missing-packet span reaches only the ring's PLC.

Composed standalone loopback tests exposed a missing inbound CONTROL/ANSWER;
the listener now sends it after authorization and ACCEPT, using the existing
reliable sequence. A route-lifetime regression also proves a raw HANGUP cannot
retire a route before its serialized owner validates it. Existing synchronous
Asterisk boundary tests pass. A 100/1000-peer fixture proves progress with one
stalled/flooded peer on four Tokio workers; it is not a hardware capacity claim.

Consumer coverage closure and final review pass and are tracked in the ignored
`.work/adr0037/` ledger. Product tests pass 227/227; standalone passes 143 library,
2 binary, 11 CLI and 3 registration tests; Asterisk passes 45/45. All changed
product/standalone source parts and new jitter/playout/ingress parts have 100%
production line and branch coverage. IAX passes 267 unit plus 4 integration
tests. These are scoped local results, not a whole-repository gate claim.
Local IAX commit: `ba3b29a`; quality-image prerequisites: `350181f`.

Dependency publication, refreshed native quality images,
hosted Debian 13 amd64/arm64 gates, and live-node validation remain outstanding.
Node changes require separate approval.
