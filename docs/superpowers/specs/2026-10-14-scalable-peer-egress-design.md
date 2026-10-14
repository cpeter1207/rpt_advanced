# Resource-scaled peer transmit path

Date: 2026-10-14
Status: Proposed; conversational approach approved, written review pending.

## Outcome and scope

Send live audio to connected peers on time on a best-effort basis. Waiting for
one peer must not stall other peers, station control, or radio callbacks.
One thousand peers is a validation goal, not a product limit. Admit additional
peers as resources and the protocol permit; do not substitute another arbitrary
total-peer ceiling for the current 1,024 limit.

Keep ASL3 interoperability, native 48 kHz PCM, codec choices, mix-minus, source
qualification, DTMF policy, source-scoped telemetry, and reload behavior intact.
Do not change inbound jitter settings, PLC, filters, or hardware configuration.
No deployment, release, or external-node load test is authorized by this spec.

The primary scaling target is standalone. The shared product improvements must
preserve the Asterisk adapter; its host-owned blocking channel APIs are not
claimed to have standalone Tokio scheduling guarantees.

## Current constraints

- `LinkDispatcher` already fans program PCM into independent peer queues.
  It performs mix-minus but no codec or network I/O.
- `Host::pump` currently invokes it after control work. Two reusable program
  blocks bridge the radio callback and dispatcher; delayed control can exhaust
  that pool and lose audio for all destinations.
- Standalone already has one retained Tokio owner per peer. It services a
  synchronous bounded step, then waits for ingress or a relative 10 ms timer.
- The peer sockets are nonblocking, but a temporary `WouldBlock` becomes a
  fatal-looking send error. Reliable full-frame construction advances sequence
  state before sending; a retry must not construct a second frame.
- The product and listener both impose 1,024-peer admission ceilings. Several
  bounded operation queues share that number even though they are not limits
  on established connections.

## Architecture

```text
radio transmit callback
  -> preallocated program-audio loopback
  -> independent per-node media distributor
  -> bounded per-peer PCM queue with media-position metadata
  -> existing per-peer Tokio owner
  -> persistent conversion / encoding / IAX packetization
  -> nonblocking UDP with bounded pending-send state
```

Retain the existing dispatcher and single-producer/single-consumer ownership.
Move its execution to one product-owned media thread per radio node, independent
of the control executor. This adds no OS thread per peer and no second Tokio
runtime. The distributor never performs network operations, encodes audio, or
waits for a destination queue. It wakes peer owners outside the audio callback
when new transmit work is admitted. Its idle wait follows the existing bounded
native-block cadence rather than busy-spinning or depending on `Host::pump`.

Bound distributor work between lifecycle checks. Preserve progress across
destination batches so stopping or replacing a generation need not wait for an
unbounded all-peer sweep. Recycle a program block only after its destinations
have been handled. Do not add an unbounded program-block pool to hide stalls.

This is one serial distributor per radio node, not a process-wide media lock.
Its sample copying remains proportional to peers and samples. Profile that
cost explicitly. Use existing bulk queue operations and avoid needless inactive
destination work where those preserve behavior; do not introduce a new queue
algorithm, shared encoded-media cache, or speculative parallel mixer.

Each peer keeps its existing serialized Tokio owner on the shared worker pool.
No task is spawned per packet. Conversion, codec state, jitter state, protocol
state, and PCM producers remain exclusive to that owner. Ingress, outgoing PCM
notifications, writable readiness, stop, and the earliest real protocol/media
deadline drive its next turn. Bound work per turn so an ingress flood cannot
starve outgoing audio or another peer.

Keep the recent sample-clock and absolute transmit-cadence fixes. A late wake
must not shift every subsequent deadline or trigger an unbounded catch-up burst.
Actual playout/protocol deadlines still run when no packet arrives; removing
idle transmit polling must not stop jitter playout or reliable-frame retries.

## Congestion and stale media

Proposed outbound age budget: **200 ms**, reusing the current native egress
queue's duration. This bounds uncommitted voice, not reliable full-frame lifetime,
target latency, or inbound jitter. Send ready audio immediately when due.

Carry generation, burst, and source sample position through the bounded PCM
handoff. Derive age from the native sample clock and an off-audio monotonic time
mapping. Do not call Tokio, allocate, lock, or add network work in radio callbacks.
Distinguish genuine quiet PCM from missing or expired media.

- A full peer queue never holds the distributor. Count rejected samples/blocks.
- The peer consumer discards expired queued voice before conversion and
  sequence-mutating packet construction. Preserve the media timeline across
  drops; do not make later audio appear contiguous with omitted samples.
- Retain bounded unsent datagram state per peer. `WouldBlock` means temporary
  local backpressure, not success at the remote peer and not an immediate
  disconnect. Try again after asynchronous writability or the relevant deadline.
- Keep servicing ingress, control, cancellation, and retry deadlines while a
  send is pending. Never exclusively await a send in a loop over all peers.
- An expired ordinary mini-voice packet may be dropped with a counter. Once an
  IAX full frame consumes a sequence number, retain that exact frame under the
  existing bounded reliable-window/retry rules. Never silently discard it,
  rebuild it with a new sequence, or reorder it behind dependent signaling.
  Reserve reliable capacity before committing sequence state. Begin retry timing
  on actual transmission, while retaining a bounded failure deadline for a socket
  that never becomes writable. An ACK must not retire a never-transmitted frame.
  Do not send minis for a new epoch before its initial full voice frame is sent.
- Preserve burst-start, final-audio, and unkey ordering. Genuine transport errors
  and exhausted reliable retries retain explicit per-peer failure handling.

Use Tokio's supported shared-socket readiness and nonblocking send operations.
Do not place a mutex around a shared UDP socket or use a single serial awaited
sender for all peers. Socket/NIC capacity and CPU are still shared resources;
this design prevents software head-of-line waiting, not physical contention.

## Admission and lifetime

Remove arbitrary live-peer caps from standalone admission, peer bookkeeping,
and listener routing. Allocate state off audio for the peers actually admitted,
using checked sizes and fallible reservation before retaining callbacks or
publishing a new routing generation. A failed admission must leave existing
connections and the running generation intact.

Keep finite per-peer queues, bounded pending-operation queues, protocol windows,
and real IAX call-number constraints. A full operation queue may reject that
operation without imposing a total connected-peer limit. Exhausted call numbers
must produce an observable admission failure, never an allocator panic or reuse
of a still-live identity. Do not invent protocol extensions or automatic CPU/RAM
capacity prediction.

The node owns one long-lived media thread. Each generation owns a dispatcher
context transferred to that thread; it can service the active and retiring
contexts under the existing one-retiring-generation rule. Reserve registration
resources before generation publication. Control requests stop and reclaims a
context only after callback and distributor quiescence, not merely peer redirect
acknowledgments. An old block cannot enter a new connection or replacement ring.
Allocation, joining threads/tasks, and freeing state occur off audio. Keep one
active owner for each SPSC endpoint throughout redirect, reload, and detach.

## Interfaces and architecture records

Changes belong in `rpt_advanced` and `librptadviax2`; reuse the released dynamic
samplerate and ring libraries. Network readiness stays in the transport layer;
packet sequencing stays in the IAX session; conversion/encoding stays at egress;
controller routing stays in the product.

The existing retained-peer descriptor lacks a nonterminal wake operation, and
the PCM send operation does not communicate discontinuous source positions.
Add a signal-only peer wake and prepared encoded-media submission with source
timestamp and explicit accepted/would-block/error outcomes. Product retains one
uncommitted media item with its original expiry; the provider owns only committed
reliable frames. A retry does not re-encode or renew the media age. Keep readiness
and protocol retries inside the provider, without another media queue or sender
task. Reuse descriptor-tail extension and size validation where possible. Update
headers, consumers, provider checks, package minimums, and mismatch tests
together; do not repurpose existing fields or export Tokio/Rust layouts.

Amend ADRs 0025/0026/0037 and the architecture map to record independent
distributor execution, readiness-driven egress, stale-media policy, resource-based
admission, and lifetime rules. Do not mark planned capabilities implemented
until tested. Preserve the unrelated existing ADR0037 and plan edits. Update
Rustdoc and affected ABI comments with implementation commits; operator docs
wait until PR preparation under repository rules.

## Validation and completion

Use test-driven development and existing test harnesses. Keep targeted results
in ignored `.work/quality-progress.md`; do not repeatedly run full reports.

Deterministic tests must prove:

1. A full peer PCM queue or blocked UDP send does not stop other peers; stalled
   control does not stop fanout; audio callbacks never wait on either owner.
2. Timely peers retain sample order, timestamps, mix-minus, source-scoped replies,
   and burst/unkey behavior during another peer's congestion.
3. `WouldBlock` recovery, stale mini-voice expiry, and reliable-frame retry do not
   create sequence holes, duplicate frames, disconnect healthy peers, or compress
   the media timeline. Uncommitted media respects the age budget; committed
   reliable frames obey their separate bounded window and failure deadline.
4. Stop, redirect, reconnect, reload, and allocator/admission failure preserve
   ownership and reject stale-generation work without use-after-free or stranded
   registrations.
5. More than 1,024 peers can be admitted when resources permit. Real call-number
   exhaustion fails gracefully. Per-peer and operation queues remain bounded.
6. Ingress-heavy, send-heavy, and idle peers make progress without task-per-packet
   creation or one OS thread per connection.

Add a parameterized sustained full-path load test using local simulated IAX
peers, real conversion/encoding, and UDP, with no public-node traffic. Run
120-second samples after warm-up at 1,000 and 2,000 peers where the host permits, then
increase the requested count to identify the measured saturation point. Include
one persistently congested destination, mixed incoming/outgoing activity, and
control/topology work. Resource failures are reported, not hidden as a hard cap.

Report per-peer send lateness (p50/p95/p99/max), oldest queued media age, source
and peer drops, socket congestion/retries, queued reliable frames, CPU, resident
memory, task/thread counts, dispatcher duration, program-block drops, and callback
duration/misses. Distinguish scheduler lateness,
kernel/network drops, and application queue drops. Do not claim remote delivery
from a successful local UDP send.

Use deterministic assertions for correctness and isolation. Real-time benchmark
numbers are hardware-specific evidence, not fragile CI wall-clock pass/fail
thresholds. A loaded small node is not promised 1,000 active streams. Any shared
software bottleneck found in this transmit scope must be identified and removed
where practical before reporting the scaling work complete.

Run focused tests and fast preflight in freshly pulled, project-labeled native
quality containers. Follow the established GitHub PR gate for full Debian 13
amd64/arm64 tests and amd64 production coverage; no local full-gate reruns or
live-node installation without separate authorization.

## Alternatives not selected

- Leaving fanout in station control preserves an avoidable all-destination stall.
- A central async sender that awaits each destination creates head-of-line delay.
- A new task/thread per packet or peer scales scheduler overhead and memory.
- A generic executor ABI solely to run one distributor adds a boundary without
  parallelizing its work. Keep Tokio in the existing peer transport execution.
- Re-encoding once for all peers is not generally valid because mix-minus and
  command-reply routing differ. Encoded-media sharing is not part of this change.

## References

- [Tokio UDP readiness](https://docs.rs/tokio/latest/tokio/net/struct.UdpSocket.html)
- [Tokio runtime fairness](https://docs.rs/tokio/latest/tokio/runtime/index.html#detailed-runtime-behavior)
- [ADR 0025](../../../doc/architecture/decisions/0025-native-media-routing-and-pcm-ring-ownership.md)
- [ADR 0026](../../../doc/architecture/decisions/0026-generational-real-time-runtime-lifecycle.md)
- [ADR 0037](../../../doc/architecture/decisions/0037-standalone-lock-free-peer-ingress.md)
