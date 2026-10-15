# Resource-scaled peer egress implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Isolate outgoing peer congestion and station-control stalls without a fixed total-peer ceiling; measure sustained operation at and above 1,000 peers.

**Architecture:** Move the existing mix-minus dispatcher to one media thread per node. Retain the existing per-peer Tokio owners, add source-timed bounded media handoff and socket readiness, and preserve reliable IAX ordering. No new protocol, codec, queue algorithm, or generic executor.

**Tech Stack:** Rust, existing rtrb 0.3.2 and Tokio 1.48.0, dynamically loaded IAX/samplerate libraries, Debian 13 native quality containers.

**Spec:** [Approved design](../specs/2026-10-14-scalable-peer-egress-design.md).

## Completion status

Tasks 1–7 are implemented and locally committed. The final whole-change review
found one congestion defect in the idle-marker path; its transport and product
regressions failed before repair and pass afterward. The affected suites pass:
911 controller/core/adapter tests and 293 IAX tests. Strict checks and developer
documentation are warning-free. The hosted PR gate and approved live-node
acceptance remain outstanding; no push, release, or deployment was performed.

Sustained loopback runs retained all 1,000, 2,000, and 3,000 peers for 120 seconds.
On the 12-logical-CPU test host, 1,000 peers had 0–1 program drops and 0–6 late
callbacks across two runs; 2,000 had mild scheduling overload; 3,000 saturated
the host. These measurements are not deployment capacity guarantees or limits.
The fixture bulk-admits peers, so it does not establish join-storm performance.

## Global constraints

- 1,000 peers is a validation goal, not a product limit; retain real IAX namespace and per-peer queue bounds.
- Native PCM remains 48 kHz F32. Preserve mix-minus, command-source replies, DTMF, CTCSS, codec choices, and Asterisk behavior.
- Uncommitted outgoing media expires at age **200 ms**. Committed reliable frames use their separate bounded window/retry rules.
- Keep two recycled program blocks. No allocation, lock, network operation, Tokio call, or new clock read in radio callbacks.
- One media thread per node; no OS thread per peer, new runtime, or task per packet. No awaited sequential send across peers.
- Preserve absolute send cadence, source-time gaps, generation ownership, and safe reload/unload.
- No inbound jitter/PLC/filter changes, node deployment, public-node load test, release, or unrequested wishlist work.
- Repositories: `rpt_advanced` (abbreviated R) and sibling `librptadviax2` (I). Paths below are relative to the named repository.
- Preserve existing unrelated edits to R's ADR0037 and `docs/superpowers/plans/2026-10-13-adr0037-peer-ingress.md`; never stage their preexisting changes incidentally.
- Follow AGENTS.md/QUALITY.md. Targeted TDD first; record checks in ignored `.work/quality-progress.md`. No local full gate. Full PR verification remains native Debian 13 amd64/arm64, with production coverage on amd64.
- Update Rustdoc/C ABI comments with code. Operator documentation waits until PR preparation. No implicit permission to push, open a PR, release, or deploy.

## Review focus

1. Delayed stream activation and reload must not timestamp fresh audio from generation-preparation time (Tasks 3–5).
2. Wraparound at an IAX timestamp epoch and an ACK received before first successful send must not lose the establishing full frame (Tasks 1–2).
3. A peer dying while an old dispatcher wakes it must not access a freed registration or stall other peers (Task 4).
4. Very small/variable native callbacks and a full metadata queue must preserve sample/descriptor alignment (Task 3).
5. Missing/short descriptor tails and failed admission must fail before retaining callbacks, leaving existing connections untouched (Tasks 2, 5–6).

## File and ownership map

| Area | Files | Responsibility |
| --- | --- | --- |
| IAX transport | I `src/client.rs`, `src/network.rs`, `src/async_driver.rs` | Retained reliable sends, writable readiness, peer deadlines |
| IAX ABI | I `src/ffi.rs`, `src/async_ffi.rs`, `include/rptadv_iax2_client.h` | Prepared ULAW and wake/deadline capabilities |
| Admission | I `src/ingress.rs`, `src/server.rs`; R `rust/product/src/host.rs`, `link/session.rs` | Dynamic bookkeeping, bounded operation queues, call exhaustion |
| Core fan-out | R `rust/core/src/audio/link_queue.rs`, `link/audio.rs` | Source spans, existing mix-minus, bounded destination batches |
| Media owner | New R `rust/product/src/link/dispatcher.rs`; existing `host.rs`, `worker.rs` | Per-node thread, prepared/active/retiring generation lifetime |
| Peer owner | R `rust/product/src/link/session.rs`, `link/egress.rs`, `services.rs` | Expiry, conversion, prepared payload, absolute media deadlines |
| Host boundaries | R `rust/product/include/rptadv_product.h`, `rust/standalone/src/iax.rs`, `iax/executor.rs`, `host_services.rs`, `rust/asterisk/src/services.rs` | Dynamic contracts; keep Asterisk PCM path |
| Evidence | Existing tests beside those files; new R `tests/peer_egress_load.py` | Regression tests and local sustained load orchestration |

## Verification commands

Run Cargo commands from the repository root, not `rust/`. Execute them inside the
maintained freshly pulled amd64 quality image using:

```sh
../rpt_advanced-workflows/tools/run-in-quality-container.sh \
  ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest "$PWD" \
  cargo test --locked -p rpt-advanced-core --lib audio::link_queue::tests
```

Use the same launcher with the appropriate command and repository for each task.
For I, omit `-p rpt-advanced-core`. The launcher records the pulled digest and
cleans its own labeled containers. Do not run another container directly.
Build the candidate I shared object/package first and install that package inside
R's disposable test container, not on the host or node. Transfer only the built
package to R's ignored `.work/dependencies/`; do not copy library sources into R.
Use the normal dependency manifest for hosted candidate checks when authorized.

Every task follows red → minimal implementation → green → scoped recovery commit.
Run only its named tests while iterating, then the affected module's tests and
documentation. Record commands, revisions, outcomes, and deferred checks once.

### Task 1: Make transport congestion nonfatal without sequence gaps

**Files:** I `src/client.rs`, `src/network.rs`, `src/client_unit_tests.rs`, `src/client_tests.rs`.

**Interfaces:** Add `MediaSend { Accepted, WouldBlock }` and
`IaxPeer::send_prepared_ulaw(&mut self, payload: &[u8], timestamp_ms: u32) -> Result<MediaSend, DialError>`.
Reuse `G711Ulaw`, the existing full/mini builders, reliable queue, and retry schedule.

- [x] Add `egress_congestion_preserves_sequence_and_epoch`: inject WouldBlock
  before the first full voice send; assert Accepted, exactly one retained sequence,
  no eligible mini before its full frame sends, and identical bytes on retry.
  Repeat across the 16-bit mini timestamp rollover and 8-bit sequence rollover.
- [x] Add `egress_ack_cannot_retire_unsent_frame` and
  `egress_mini_would_block_is_uncommitted`: assert an early ACK retains an unsent
  frame; a blocked mini leaves sequence/voice epoch unchanged; a permanent error
  remains terminal only for that peer.
- [x] Run `cargo test --locked --lib egress_`; confirm failures identify the
  missing retention behavior, not setup/dependency errors.
- [x] Reserve reliable capacity before sequence mutation; retain committed bytes
  before attempting UDP. Record whether each frame was actually sent, start retries
  then, and retain a first-send failure deadline using the existing terminal retry
  horizon. Drain bounded protocol replies without dropping committed frames or
  returning before ingress/ACK processing. Do not reset retry age on WouldBlock.
- [x] Rerun those tests plus `cargo test --locked --lib interop_`; document the
  ownership/result rules and commit `fix: isolate IAX send congestion`.

### Task 2: Expose prepared media and readiness-driven peer scheduling

**Files:** I `src/ffi.rs`, `src/async_ffi.rs`, `src/async_driver.rs`, `src/client.rs`,
`src/network.rs`, `include/rptadv_iax2_client.h`; existing
`src/ffi_tests.rs`, `src/async_driver_tests.rs`, `tests/abi_v1.rs`,
`tests/abi_header_smoke.c`, `tests/ulaw_media.rs`.

**Interfaces:** Append fields without moving existing descriptor fields:

```c
/* Client table: caller owns encoded storage; one ULAW byte per sample. */
int32_t (*prepare_ulaw)(const float *, size_t, uint8_t *, size_t);
int32_t (*send_prepared_ulaw)(void *, const uint8_t *, size_t, uint32_t);
uint32_t (*elapsed_ms)(const void *peer);
/* Async table: signal-only wake; deadline called only by the peer owner. */
void (*peer_wake)(void *registration);
int32_t (*peer_set_deadline)(void *peer, uint64_t wake_after_ns);
```

`send_prepared_ulaw`: 0 Accepted, 1 uncommitted WouldBlock, negative permanent
error. `prepare_ulaw`: 0 success, negative invalid buffer/input. Empty payload
does not construct voice. `elapsed_ms` exposes the existing call clock for source
mapping. `UINT64_MAX` clears the product deadline; zero makes it due. Retain ABI 1
and SONAME 1 for additive tables; new consumers require the complete needed tail.

- [x] Add `egress_descriptor_tail_and_prepared_codec`: check exact existing field
  offsets, prefix-size reads, null/short buffers, unchanged failed outputs, and
  prepared bytes equal the current codec's output. No codec implementation in R.
- [x] Add `egress_wake_deadline_and_blocked_peer_isolation`: wake coalescing,
  wake-before-wait, a product expiry without ingress/writability, jitter playout
  without packets, and another peer progressing while one remains blocked.
  Use injected time/readiness; assert no busy loop on a persistently blocked peer.
- [x] Run `cargo test --locked --lib egress_` and
  `cargo test --locked --test abi_v1`; confirm red.
- [x] Share the existing socket through `Arc<tokio::net::UdpSocket>` after async
  attachment. Use `writable()` with `try_send_to()`, not shared `poll_send_ready()`.
  Select stop, ingress, coalesced product wake, blocked-send readiness, and the
  earliest product/jitter/protocol deadline. Bound work and yield between batches;
  retain the existing synchronous path for non-async callers.
- [x] Rerun named tests, `cargo test --locked --test ulaw_media`, and async-driver
  tests. Add ABI comments and commit `feat: drive peer egress by readiness`.

### Task 3: Carry source time through bounded PCM fan-out

**Files:** R `rust/core/src/audio/link_queue.rs`, `link_queue_tests.rs`,
`rust/core/src/link/audio.rs`, `audio_tests.rs`, affected exports/call sites.

**Interfaces:**

```rust
AudioSpan { generation: u64, burst: u64, source_start: u64 }
ReadSpan { span: AudioSpan, frames: usize }
SourcePosition { generation: u64, sample: u64 }
LinkAudioQueue::new(sample_capacity: usize, span_capacity: usize) -> Result<Self, LinkQueueError>
LinkAudioProducer::write(&mut self, span: AudioSpan, samples: &[f32]) -> usize // rejected count
LinkAudioConsumer::read_span(&mut self, output: &mut [f32]) -> Option<ReadSpan>
LinkAudio::process(&mut self, controller: &mut NodeController, receiving: bool,
    audio: &mut [f32], source: SourcePosition) -> Result<bool, AdmissionError>
LinkDispatcher::dispatch_batch(&mut self, maximum_destinations: usize,
    on_written: impl FnMut(usize)) -> DispatchProgress
DispatchProgress { destinations: usize, completed_blocks: usize, pending: bool }
```

Reuse sample rtrb plus a descriptor rtrb; production bounds are 9,600 samples and
64 span descriptors per peer. Descriptor exhaustion counts like PCM exhaustion.
Reserve a descriptor before PCM publication, publish it after the accepted PCM
prefix, and consume only descriptor-governed samples. Partial reads retain the
descriptor offset; `ReadSpan.span.source_start` advances by that offset. No hidden
cross-span concatenation, per-sample metadata, or queue allocation during writes.
`source.sample` is the first sample's position on the existing stream timeline,
not a counter restarted for each configuration generation. Track each destination's
burst number when its ordinary audio or targeted reply becomes eligible after idle.

- [x] Add `egress_span_partition_and_overflow`: callback partitions 1, 240, 480,
  960, and 4096 samples; partial PCM admission and descriptor exhaustion; assert
  accepted samples match descriptors and rejected suffixes remain source-time gaps.
- [x] Add `egress_batched_mix_minus_preserves_routes`: retain existing routing
  assertions, dispatch one destination at a time, assert exactly two recycled
  program blocks, no early recycle, and full peers do not stop healthy destinations.
- [x] Run the core `audio::link_queue::tests` and `link::audio::tests`; confirm red.
- [x] Store current block/destination cursor in `LinkDispatcher`; count skipped
  destinations toward each batch budget. Invoke `on_written(index)` only after
  accepted media. Stamp blocks from the worker's existing sample timeline and
  generation identity; advance source position even when publication is dropped.
- [x] Rerun affected core tests; update Rustdoc and commit
  `refactor: preserve source spans through peer fanout`.

### Task 4: Run generation-owned dispatchers independently of control

**Files:** New R `rust/product/src/link/dispatcher.rs` and `dispatcher_tests.rs`;
existing `link/mod.rs`, `host.rs`, `host_tests.rs`, `worker.rs`, `worker_tests.rs`,
`link/session.rs`, and core lifecycle comments where ownership changes.

**Interfaces:** `MediaWorker::start() -> io::Result<Self>`;
`prepare(&mut self, dispatcher: LinkDispatcher, wakes: Vec<PeerWake>) -> Result<PreparedDispatch, crate::services::Error>`;
`PreparedDispatch::activate(&self, generation: u64)`;
`request_stop(&self)`; `is_quiescent(&self) -> bool`.
`PeerWake::notify(&self)` signals only; `close(&self)` prevents future token access;
`is_quiescent(&self) -> bool` reports wake-call completion. Keep these private to
the product, with no generic executor abstraction.

- [x] Add `egress_dispatch_survives_control_pause` and
  `egress_retirement_waits_for_dispatch_and_redirect`: stop control pumping while
  callback publication continues; then reload with a partially dispatched block
  and delayed redirect ACK. Assert healthy delivery and no premature reclamation.
- [x] Add `egress_wake_close_race`: race notification with peer retirement;
  assert no token use after detach and stale wake handles become no-ops.
- [x] Run `cargo test --locked -p rptadv-product --lib egress_`; confirm red.
- [x] Give each node one long-lived worker. Prepare inactive registrations before
  publication; activate without allocation. Alternate active/retiring contexts in
  batches of at most 64 destinations; use a 1 ms parked idle wait, not busy-spin.
  Remove dispatcher execution from `Host::pump`; control keeps lifecycle handles.
- [x] Use a private atomic closed-bit/in-flight-count gate for `PeerWake`, not a
  mutex or raw token copied into endpoints. A notifier increments the count,
  calls the signal-only provider only if the closed bit was clear, then decrements.
  Reaping sets closed and waits through later control passes for count zero before
  detaching. Late increments observe closed and never dereference the token.
  Use sequentially consistent ordering and document that lifetime proof.
- [x] Reclamation requires callback, dispatcher, and redirect quiescence.
  Stop/join/free off audio and off peer callbacks. Test failed preparation and
  delayed stream activation using the actual sample-clock origin, not preparation
  wall time. Rerun dispatcher/host/worker/session tests and commit
  `refactor: isolate peer media distribution from control`.

### Task 5: Wire source-timed expiry, encoding, and peer wake across hosts

**Files:** R `rust/product/src/link/session.rs`, `session/tests.rs`, `link/egress.rs`,
`egress_tests.rs`, `services.rs`, `services_tests.rs`, `fixture.rs`,
`rust/product/include/rptadv_product.h`; R standalone IAX/executor/host-services
files and tests; `rust/asterisk/src/services.rs`; `tests/test_product_abi.c`.

**Interfaces:** Extend host-services ABI 5 with a complete async-egress tail:
`peer_prepare_audio(context, peer, pcm, count, encoded, capacity)`;
`peer_send_prepared(context, peer, encoded, count, timestamp_ms)`;
`peer_elapsed_ms(context, peer)`; `peer_step_wake(context, registration)`;
`peer_set_deadline(context, peer, wake_after_ns)`.
Use the corresponding C types/results from Task 2. Current standalone supplies
the whole group; current Asterisk supplies null for it and keeps `peer_write`.
Reject a partial group or truncated field. Do not substitute an old-provider fallback.

- [x] Add `egress_expiry_preserves_media_timeline`: 199 ms media sends; 200 ms
  media expires before conversion/packet construction. A later accepted span
  retains the missing source interval. Repeat for metadata overflow, partial
  conversion output, FIR delay, burst transitions, and a generation replacement.
- [x] Add `egress_prepared_retry_encodes_once` and
  `egress_late_wake_keeps_absolute_cadence`: repeated WouldBlock retains one encoded
  item with its original expiry; recovery neither re-encodes nor compresses time;
  send work stays bounded to one ordinary media block per owner turn.
- [x] Add `egress_host_contract_and_asterisk_pcm`: short/partial tails fail safely;
  current Asterisk PCM writes and unkey remain unchanged; standalone forwards
  the wake/deadline and accepted/would-block distinction correctly.
- [x] Run product `egress_` and standalone IAX/host-service targeted tests; confirm red.
- [x] Keep one preallocated encoded buffer per session. Map source position to
  the provider's call clock off audio; preserve fractional sample arithmetic and
  explicit gaps. Reset the existing converter on discarded discontinuous input
  so hidden old FIR samples cannot leak into a fresh span. Preserve continuous
  stream conversion and normal final-tail behavior.
- [x] Derive remaining wait from absolute media/expiry deadlines; pass it through
  the new tail. Wake after PCM/commands arrive. Continue ingress/control during
  WouldBlock; preserve key/final-audio/unkey order and no stale generation sends.
- [x] Run product link/services/host tests, standalone IAX/host-services tests,
  and `make product-abi-check`; update in-source docs and commit
  `feat: expire stale peer media without blocking healthy peers`.

### Task 6: Remove arbitrary admission ceilings

**Files:** R `rust/product/src/host.rs`, `link/session.rs`, their tests;
I `src/ingress.rs`, `ingress_tests.rs`, `src/server.rs`, `server_tests.rs`.

**Interfaces:** Replace `PeerIngressRouter::new(peer_limit, queue_capacity)` with
`new(queue_capacity)`; remove total count checks and eager 1,024 reservations.
Change `next_available_call` to return `Option<u16>`; handle exhaustion as a local
admission failure. Rename the product pending-operation bound to
`PENDING_ATTACH_CAPACITY = 1024`; it is not an established-peer limit.

- [x] Add `egress_admission_above_1024` in both repositories: admit 2,000
  lightweight peers/routes, verify all identities, release/reuse one, and keep
  existing per-peer queue bounds. No real network or timing threshold is needed.
- [x] Add `egress_admission_failure_keeps_existing_peers`: injected reservation,
  registration, and exhausted inbound call-number failures retain existing peers
  and publish no stranded callback. Test call reuse only after ownership release.
- [x] Run each repository's `egress_admission` tests; confirm red.
- [x] Fallibly reserve growable bookkeeping before retained callbacks/route
  publication, retain bounded attach/command/event queues, and remove the
  call-exhaustion panic. Do not promise recovery from OS OOM or allocations inside
  third-party channels. Dedicated outbound sockets retain their own namespaces.
- [x] Rerun host/session and ingress/server tests; commit each repository's scoped
  change as `refactor: admit peers by resources instead of fixed count`.

### Task 7: Measure the complete path and finish architecture/package integration

**Files:** R product/standalone test modules; new `tests/peer_egress_load.py`;
I transport tests; existing observation records/comments; R
`doc/architecture/README.md`, ADRs 0025/0026/0037; both Debian dependency/check files
and R `.github/dependencies.json` only as required for the new descriptor tails.

**Interfaces:** A test-only ignored Rust entry point
`egress_full_path_load` uses real product fan-out, peer sessions, conversion,
encoding, dynamic IAX provider, and local UDP receivers. Inputs:
`RPTADV_LOAD_PEERS`, `RPTADV_LOAD_SECONDS`, `RPTADV_LOAD_OUTPUT`.
The Python runner accepts `--peers`, `--seconds`, `--output`; it starts no public
connections, labels/cleans test processes, and emits machine-readable results.

- [x] Add deterministic cross-layer tests for stalled control, a congested peer,
  simultaneous ingress, queued reload/stop, and bounded memory. Inject blocked
  sends explicitly: a remote UDP receiver that stops reading does not reliably
  make the local sender return WouldBlock.
- [x] Verify loss/latency counters with known events before using them in the
  load report. Capture send-lateness p50/p95/p99/max, expired/rejected samples,
  queue age, congestion/retries, reliable backlog, dispatcher duration/block drops,
  callback duration/misses, CPU/RSS, and task/thread counts. Keep hot diagnostics
  bounded; compute percentiles/report files outside radio callbacks.
- [x] Run those tests red, implement the test harness/necessary observations,
  then confirm green. Run all affected test modules once, not a full local gate.
- [x] Run 120-second measured intervals after warm-up at 1,000 and 2,000 peers
  where resources permit, with variable callbacks, mixed traffic, topology work,
  a control pause and selected congestion. Increase the requested count until
  saturation/resource failure is observed; report the host, workload, ceiling,
  and limiting resource. Do not hide capacity failure as a passing performance
  result or make noisy wall-clock percentiles mandatory CI thresholds.
- [x] Address measured avoidable transmit bottlenecks within this design; stop
  for a design decision if a larger architectural change is necessary. Verify
  timing fixes and overload behavior again using only affected tests.
- [x] Record the implemented ownership/invariants in ADR0025/0026/0037 and the
  architecture map without absorbing the unrelated existing edits. Update package
  minimums and descriptor-size tests together. Candidate dependency revision must
  identify the actual I commit, not an invented release tag. SONAME stays unchanged
  for append-only descriptors; current consumers explicitly require the tails.
- [x] Run `make -j2 lint static-analysis` in each fresh native container and
  warning-free Rustdoc for touched crates; log exact evidence. Commit scoped
  developer docs/tests/artifacts. Report measured results and remaining limits.
  Push/PR/release/deployment are separate authorization, not automatic final steps.

## Self-review and execution handoff

Tasks 1–2 own transport and ABI; 3–4 own fan-out and lifetime; 5 joins them; 6
removes arbitrary admission; 7 measures the whole path and updates records.
All five review-focus cases have named tests above. The existing Asterisk path
remains distinct because its host owns codec/channel operations, not to preserve
obsolete alpha implementations. No new wishlist feature is included.

**Recommended execution:** Native execution in this session, with one independent
whole-change review at the end. The tasks share several ownership/ABI contracts,
so keeping one implementation context avoids repeated handoffs. Subagent-driven
execution with task-by-task independent reviews is also available.

Implementation begins only after the user reviews this plan and chooses the
execution approach. No implementation or runtime validation has occurred yet.
