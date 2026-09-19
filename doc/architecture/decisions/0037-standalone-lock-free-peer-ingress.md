# ADR 0037: Standalone peer ingress uses lock-free single-owner serialization

Status: Accepted

## Context

The planned standalone controller must remain responsive with hundreds or
thousands of connected peers. Concurrent packet producers must not contend on
a mutex around a peer's jitter buffer, decoder, or inbound PCM ring. The shared
PCM ring is single-producer/single-consumer; calling its producer concurrently
does not become safe merely because its cursors are atomic.

This decision applies to the planned standalone application. It does not
require changing locks, threads, or behavior in the current Asterisk-hosted
`app_rpt_advanced`. ADR 0036 separately governs independence from Asterisk's
thread model.

## Decision

Prefer exclusive ownership, message passing, atomics, and immutable snapshots
over locks, mutexes, spinlocks, or equivalent contention gates wherever a
thread-safe alternative exists. Do not replace a mutex with a spinlock or
retry loop that waits for another producer to release the same resource.

Use **single-owner serialization with bounded SPSC fan-in** for standalone
peer ingress:

1. A bounded set of ingress producers hands packets to a bounded set of media
   workers. Assign each connected peer one stable media owner, independent of
   the peer count; do not create a thread or task executor per peer. Bound
   producer, worker, peer, and queue-slot resources at setup; reject admission
   rather than exceed available capacity.
2. Each producer-to-owner pair has its own preallocated SPSC packet queue.
   Its producer handle belongs to exactly one execution context. Concurrent
   producers never share a producer handle, a reservation cursor, or the PCM
   ring's writer. These queues carry packet records, not a second PCM buffer.
   Multiple producer queues may carry packets for the same peer. Standalone
   transport execution contexts are registered before publishing; an arbitrary
   callback thread must not borrow another producer's SPSC handle.
3. The owner fairly drains ready input queues with bounded work per turn. An
   empty queue or a producer paused before publication must not stop other
   ready queues. It alone inserts a peer's packets into that peer's jitter
   buffer, advances its decoder, and writes its inbound PCM ring.
   Service peer playout deadlines even when no packets arrive; never drain a
   busy input indefinitely before checking other inputs or due playout.
4. Preserve each producer's packet order. Packets from different producers
   retain their protocol sequence/timestamps and arrival metadata; the peer's
   existing jitter/ordering logic resolves network reordering. Do not invent a
   global packet sequence or wait for every producer before making progress.
   Order-sensitive peer control packets use the same owner and protocol-order
   rules as media, not a second concurrent path into peer state.
5. Queue publication and consumption never block or allocate. Packet payload
   ownership remains valid until consumed, using bounded preallocated storage.
   Each record owns its bounded payload and peer/session generation; it never
   retains a pointer into a callback's reusable receive buffer.
   When a queue is full, reject the new packet and count the drop; never wait,
   grow without bound, overwrite a consumer-owned slot, or mutate the PCM ring
   from the producer. Queue occupancy and drops remain observable.
6. Lifecycle and routing changes use the generation/quiescence rules in
   ADR 0026. A retired peer's queued packet cannot reach a replacement peer
   that reuses its slot. A live peer never acquires a second media owner.

Use the existing project handoff where it meets this contract; for the Rust
packet queues, select the established `rtrb` SPSC implementation under its MIT
license rather than writing a new queue algorithm. It is an internal Rust
implementation dependency under ADR 0018, not a replacement for the released
rate-adjusting PCM shared object. No new PCM ABI or multi-writer API is needed.
The upstream project documents wait-free operations and bounded storage;
integration must preserve those properties by keeping blocking work and
allocating payload destructors out of queue operations.

Queue topology scales with the fixed producer/worker counts, not one queue or
thread per possible producer per peer. Peer jitter/codec/ring state still
scales with actual connected peers. Use readiness-driven nonblocking network
I/O and bounded service turns; no lock-free requirement mandates idle spinning.
The native audio tick remains independent of packet arrival and queue drains.

## Consequences and verification

The standalone ingress-mutex exception in ADRs 0002, 0025, and 0026 is removed.
That compatibility exception remains available only in the Asterisk-hosted
adapter; this decision does not authorize rewriting the deployed module.

Test concurrent producers, a producer paused before publication, full queues,
wraparound, per-producer order, protocol reordering, fair service, and peer
retirement with queued packets. Verify one writer per peer PCM ring, no lost
ownership or torn payloads, no unbounded growth, and continued progress for
unrelated ready sources. Exercise hundreds and thousands of simulated peers
and report CPU, memory, queue drops, and service latency. These are scaling
validation requirements, not a claim that every hardware target can carry
thousands of simultaneous audio streams.

This records the implementation method; the standalone ingress is not yet
implemented. No current Asterisk behavior, package dependency, or shared PCM
ABI changes as a result of documenting this decision.

## Reference

[rtrb API documentation](https://docs.rs/rtrb/latest/rtrb/) and
[its upstream MIT license](https://github.com/mgeier/rtrb)
describe the selected bounded wait-free SPSC primitive. The producer-to-owner
fan-in and ownership policy above are this project's design.
