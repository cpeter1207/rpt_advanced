# Incoming Text Media Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Speak supported incoming human messages at local RF while independently forwarding eligible text and preserving all live audio.

**Architecture:** Protocol classification produces an origin-tagged neutral message. Two bounded synthesis workers stream through per-origin finite-media rings, and transmit mixes those rings into the local-only bus. Existing speech and ring adapters do the rendering and rate conversion; telemetry scheduling is not reused.

**Tech Stack:** Existing Rust product/core/media-support crates, released speech/ring adapters, `rtrb`, bounded `crossbeam-queue` 0.3.12 queues, existing generation publication/retirement.

**Spec:** [Approved design](../specs/2026-10-27-protocol-neutral-codecs-and-text-media-design.md); [integration and execution order](2026-10-27-protocol-neutral-codecs-and-text-media.md).

## Global Constraints

- Inherit integration-plan constraints. No new protocol implementation, outgoing composition UI, RF test, deployment or release.
- Text speech is local-only incoming media, mixed even with its sender's live audio. No politeness, Morse fallback, peer speech, parrot capture or fabricated remote key state.
- Retain configured voice/model/speed/level and existing RF/duplex/timeout/inhibit behavior. Treat text as literal data, not substitutions, shell, markup or commands.
- Maximum 4096 UTF-8 bytes/local rendition; four admitted jobs/origin including active; 64 admitted jobs/node; reject newest local job on saturation. Forwarding remains separately admitted.
- Two synthesis workers/product, one active job/origin, fair progress between origins. Reuse configured process timeout for stdin, reads, full-ring waits and cancellation. No new operator knobs for these bounds.
- Dedicated source-rate finite-media ring/origin, one producer/one consumer, `InboundPolicy::Media`, no PLC/clock target. Only the ring converts to 48 kHz.
- Forwarding is text-only, protocol-capability/audience constrained, never reflected to ingress. Unknown/control text is never automatically spoken.

## Review Focus

1. A control-looking or invalid UTF-8 payload cannot become RF speech: T1.
2. A child that never reads stdin or exits mid-speech cannot block shutdown: T2/T3.
3. Oversized UTF-8, full local queues or a stalled origin cannot stop text forwarding or peer audio: T3/T4.
4. Empty-but-not-EOF rings and same-peer concurrent audio cannot stick PTT or leak text to peer/parrot: T2/T5.
5. Disconnect/reload/source-number reuse cannot revive stale speech or free a live ring on audio: T6.

---

Paths are relative to `rpt_advanced/`. This subplan can establish its classifier,
finite-stream helpers and executor independently of new audio codecs. Integrate
the host-facing classifier only after codec task C8 establishes host8/product6;
do not create a second host-table revision for this feature.

### Task T1: Classify supported human text without weakening ASL signaling

**Files:** Modify `rust/core/src/link/{mod.rs,protocol.rs}`, `rust/product/src/link/session.rs`; create `rust/core/src/link/text.rs`, `rust/core/src/link/text_tests.rs`, `tests/fixtures/human_text.json`; retain existing protocol parser tests. Add a focused `text_` test filter, not a second parser framework.

**Interfaces:** New neutral types in `link/text.rs`:

```rust
struct TextOrigin { station_generation: u64, connection_generation: u64,
                    peer: String, protocol: u32 }
enum TextAudience { Broadcast, Node(String) }
struct HumanMessage { origin: TextOrigin, source: String,
                      audience: TextAudience, body: std::sync::Arc<str> }
enum ClassifiedText { Control(Protocol), Human(HumanMessage), Unknown }
fn classify_asl(origin: TextOrigin, bytes: &[u8]) -> ClassifiedText;
```

`protocol=1` identifies the existing AllStarLink adapter, not an audio-format
bit. `source` is validated message attribution, never authentication; the
retained connection in `origin` determines admission and cancellation. In
product session events add `HumanText(HumanMessage)` alongside existing control
events. Bind the origin to the current authenticated session before publication.

- [ ] **Red:** Add `text_classification` assertions:

  ```rust
  assert_eq!(human_body(b"M 2000 0 hello world"),Some("hello world"));
  assert_eq!(human_destination(b"M 2000 1000 hello"),Some("1000"));
  assert!(!is_human(b"L T2000"));
  assert!(!is_human(b"!!DISCONNECT!!"));
  assert!(!is_human(b"T 2000 STATUS,2000,0"));
  assert!(!is_human(b"ordinary unknown text"));
  assert!(!is_human(&[0xff]));
  ```

  Also cover all existing key/DTMF/auth forms, optional single trailing NUL versus embedded NUL, empty body, malformed identities and Unicode body preservation. Assertions use test-local classifier readers, not a new public convenience API.
- [ ] **Run red:** `cargo test --locked -p rpt-advanced-core text_classification`; expected missing classifier/human variants.
- [ ] **Implement:** Preserve strict control parsing. Add only the source-verified `M <FROM> <TO> <BODY>` human form, with `TO=0` broadcast and node-addressed delivery. Pinned ASL `app_rpt.c:1948–1978` and `rpt_cli.c:903,1030` agree on FROM/TO; the [obsolete-wiki manual page](https://allstarlink.github.io/developers/iaxtext/#m-message) reverses them and warns it may be incorrect. Use the pinned implementation as the contract. No copied `app_rpt` implementation, invented wire form or catch-all unknown-as-chat. Future protocols enter through `HumanMessage`, not an IAX-dependent speech API.
- [ ] **Green:** Repeat the filter plus existing `link::protocol` tests and product session classification tests. Current signaling behavior is unchanged; malformed human input is counted/ignored without a generic hangup.
- [ ] **Commit:** Only classifier/events/fixtures/Rustdoc files: `feat: classify supported incoming human text separately from control`.

### Task T2: Reuse finite-stream lifecycle and make speech stdin cancellable

**Files:** Create `rust/product/src/media/finite.rs`, `finite_tests.rs`; modify `rust/product/src/media/{worker.rs,worker_tests.rs}`, `rust/product/src/media.rs`, `rust/media-support/src/process.rs`, existing process tests and `rust/media-support/tests/preparation.rs`. Preserve pending timeout hunks in `worker_tests.rs`; stage only this task's changes.

**Interfaces:** Move, do not duplicate, existing `SegmentConsumer`, `SegmentProgress`, source-duration/flush and off-audio retirement helpers into `media/finite.rs` with `pub(super)` visibility for worker/text owners. Preserve `finished(completed: bool) -> bool` and `render_sample() -> Result<Option<f32>, RingError>` semantics. Add `NativeMediaPreparer::open_speech_stream(&self, request: &SpeechRequest<'_>) -> Result<SpeechStream, MediaError>`, with `SpeechStream::sample_rate_hz() -> u32` and `read(&mut self, pcm: &mut [f32], cancelled: &impl Fn() -> bool) -> Result<usize, MediaError>`; this borrows the retained existing speech table, not collected `PreparedAudio`. Zero is EOF only under its existing stream contract.

- [ ] **Red:** Add `finite_stream_budget_and_stall` and `speech_stdin_obeys_budget`: `assert!(!empty_ring_before_eof().finished()); assert_eq!(drained_finite_stream().played_frames,real_budget()); assert!(stalled_stdin().times_out()); assert!(cancelled_stdin().child_reaped()); assert_eq!(failed_flush().remaining_activity,0);`. Use controllable child/clock fixtures, not long sleeps. Include a child writing stdout while not draining stdin to expose pipe deadlock, and failure after the first audible chunk.
- [ ] **Run red:** `cargo test --locked -p rptadv-product finite_`; `cargo test --locked -p rptadv-speech-adapter speech_stdin_`; expected missing helper or current blocking-write failure.
- [ ] **Implement:** Factor current helpers without changing telemetry scheduling. Keep `ChildStream::spawn(config: &Config, command: &mut Command, input: Option<&[u8]>, cancelled: &impl Fn() -> bool) -> Result<Self, MediaError>`; retain an owned copy of pending input, configure its pipe nonblocking and let reads pump bounded incremental stdin/stdout progress. Close stdin after its final byte. Replace blocking `write_all`; checks use the same original process deadline/cancellation. Do not spawn an unbounded writer thread or reset the deadline on progress. Keep Piper literal stdin and the current model/rate/gain contract. The speech adapter ABI/SONAME stay1; fix its implementation behind the current open/read/cancel table.
- [ ] **Green:** Repeat focused finite/stdin/preparation tests and current telemetry producer tests, including timeout-notice regressions. Verify source duration excludes FIR flush padding, subprocesses are reaped and no collected whole-message PCM is required before streaming begins.
- [ ] **Commit:** Exact helpers/process/tests only: `refactor: share cancellable finite speech streaming`.

### Task T3: Add bounded origin-ordered speech production outside peer/control tasks

**Files:** Create `rust/product/src/media/text.rs`, `text_tests.rs`; modify `rust/product/src/{media.rs,lifecycle.rs}`, `Cargo.toml`, root `Cargo.lock`. Use the existing product generation owner; no separate service or Piper invocation.

**Interfaces:** `TextExecutor::new(node_count: usize, preparer: NativeMediaPreparer) -> Result<Self, MediaError>` prepares two normal-priority workers and checked queue storage for enabled nodes. `TextExecutor::admit(message: HumanMessage, speech: SpeechSource) -> Admission`; `cancel(origin: &TextOrigin)`; `stop()` retires jobs. `Admission` is `Accepted`, `TooLarge`, `OriginFull`, `NodeFull` or `Stopping`. Internally one origin owns its pending FIFO/ring and at most one active worker job. Return a tagged `ReadyText { origin, job: u64, reader: SegmentConsumer }` through existing-style bounded ready/retired queues.

- [ ] **Red:** Add `text_admission_and_fairness`:

  ```rust
  assert!(admit_utf8_bytes(4096).accepted());
  assert_eq!(admit_utf8_bytes(4097),Admission::TooLarge);
  assert_eq!(fifth_same_origin(),Admission::OriginFull);
  assert_eq!(sixty_fifth_same_node(),Admission::NodeFull);
  assert_eq!(maximum_running_syntheses(),2);
  assert!(second_origin_progresses_while_first_stalls());
  assert_eq!(same_origin_completion_order(),admission_order());
  ```

  `admit_utf8_bytes` tests multibyte input at actual byte boundaries. A fake streaming provider controls ready/block/fail states; no timing-based throughput assertion.
- [ ] **Run red:** `cargo test --locked -p rptadv-product text_admission_`; expected missing bounded executor.
- [ ] **Implement:** Reuse T2's preparer/finite helpers. Use released `crossbeam-queue` bounded `ArrayQueue` for non-audio multi-producer admission/completion and existing `rtrb` for audio endpoint handoff, not a homegrown MPMC queue. Checked capacity is 64 times prepared enabled-node count; per-node/per-origin admission is reserved atomically before enqueue and released exactly once on cancellation/completion. Fair ready-origin dispatch prevents an origin monopolizing both workers, with one active job/origin. No queue lock is held across synthesis, ring-full waits or callbacks. Retain stream order, one producer per origin ring and bounded pending text; workers never execute on peer Tokio/control/audio threads. On reload prepare resized admission storage; retire/cancel/join old workers off audio before starting the next pair, without blocking ordinary live media. Configured process budget also bounds ring-full waiting. No new settings solely for these bounds.
- [ ] **Green:** Repeat focused tests and add saturation/cancel/retry-accounting cases. Only newest local admission is rejected; text objects and rings are destroyed off audio. Speech failure before/after PCM is counted/logged once with metadata only, no Morse, automatic retry or gain duplication.
- [ ] **Commit:** Executor/lifecycle/dependency/tests/Rustdoc only: `feat: stream bounded incoming text speech per origin`.

### Task T4: Forward original text independently of local speech admission

**Files:** Create `rust/product/src/link/text.rs`, `text_tests.rs`; modify `rust/product/src/link/{mod.rs,session.rs}`, shared link-control/event dispatch in `rust/core/src/runtime/aggregate.rs` and its focused tests. Keep control text on its existing path. Extend current bounded peer text-send commands instead of adding sockets.

**Interfaces:** `TextRouter::route(&mut self, message: HumanMessage, peers: &[TextDestination]) -> TextDisposition`; `TextDestination { connection_generation: u64, protocol: u32, node: String, can_forward_text: bool }`. `TextDisposition { locally_eligible: bool, destinations_admitted: usize, destinations_rejected: usize }`. The router uses existing authorization/topology and per-peer egress admission; protocol serializers produce destination-supported text, not synthesized audio. Local speech admission follows independently, using T3.

- [ ] **Red:** Add `text_forwarding_independent_of_speech`: `assert_eq!(broadcast().destinations,eligible_peers_except_origin()); assert!(!directed_elsewhere().locally_eligible); assert_eq!(tts_full().forwarded,tts_available().forwarded); assert_eq!(tts_failed().forwarded,tts_available().forwarded); assert!(unsupported_peer().audio.is_empty()); assert_eq!(blocked_peer().unrelated_sends,expected_sends());`. Test original body/source retention, node-addressed routes, local-only destinations and one admission per eligible destination.
- [ ] **Run red:** `cargo test --locked -p rptadv-product text_forwarding_`; expected no human forwarding/local admission.
- [ ] **Implement:** Route only recognized protocol messages and eligible destinations, with no reflection to ingress and no bypass of existing link policy. A local synthesis limit does not become a protocol text-size limit; retain valid messages beyond 4096 bytes for separately bounded forwarding where the destination supports them. Preserve protocol source/destination/duplicate rules; no new cross-protocol ID or fragmentation. Future protocol adapters supply their own supported serialization. Do not claim full EchoLink support or globally exactly-once delivery.
- [ ] **Green:** Repeat focused forwarding/session tests with full local queue, missing speech backend and one blocked egress. Peer audio/control progress remains independent, and only intended addressed/broadcast recipients obtain the original text.
- [ ] **Commit:** Scoped router/session/runtime tests: `feat: forward human text independently of local speech`.

### Task T5: Mix ready text only into local transmit and preserve RF policy

**Files:** Modify `rust/core/src/link/{audio.rs,audio_tests.rs}`, `rust/core/src/controller/{mod.rs,telemetry.rs}`, `rust/product/src/host.rs` (existing `LinkAudio::new` generation builder); create `rust/core/src/link/text_audio_tests.rs`, `rust/product/src/media/text_audio_tests.rs`.

**Interfaces:** Core defines `LocalTextInput: Send { fn render(&mut self, pcm: &mut [f32]) -> bool; }` and `LocalMedia<'a> { samples: &'a [f32], active: bool }`. `LinkAudio::with_text(self, input: Box<dyn LocalTextInput>) -> Self` is composed off audio. Add a `LocalMedia` argument to the existing `NodeController::process_audio_with_parrot` boundary (all non-text callers use empty/inactive media). The product reader owns preallocated origin slots/read buffers and receives T3's tagged ready/retired endpoints. `render` reports active only for playable output, not pending synthesis; it does not mutate peer signals.

- [ ] **Red:** Add `text_local_mix_isolation`:

  ```rust
  assert_eq!(rf_samples(),local_plus_sender_plus_other_peer_plus_text());
  assert_eq!(peer_samples_with_text(),peer_samples_without_text());
  assert_eq!(parrot_samples_with_text(),parrot_samples_without_text());
  assert_eq!(peer_key_state_with_text(),peer_key_state_without_text());
  assert!(!synthesis_pending_without_pcm().local_ptt);
  assert!(!text_finished_or_failed().stuck_ptt);
  ```

  Test same-peer audio overlap, live audio starting mid-speech, empty ring while producer is pending, configured gain exactly once, duplex, timeout and hardware inhibit. Compare all egress/parrot samples, not just flags.
- [ ] **Run red:** `cargo test --locked -p rpt-advanced-core text_local_mix_`; expected no local-only concurrent source.
- [ ] **Implement:** Consume/mix ready text at the local-only voice-band point, before normal local transmit DSP/gain but outside parrot capture, peer mix-minus and command/broadcast buffers. Keep local-media PTT demand under the controller's existing safety/duplex gate; do not simply OR PTT after it. Pass receive activity unchanged to IDs/courtesy/timeout policy; no synthetic link COR or telemetry scheduling. Local text does not wait for unkey or switch to Morse. Preallocate callback buffers/slots for admitted origins; endpoint retirement, allocation, Arc final-drop and logging stay off audio. Reuse existing callback-boundary publication, not a transmit mix ring.
- [ ] **Green:** Repeat focused mix tests and existing source-routing/timeout/parrot/DTMF filters. Under allocation/lock-detection fixtures, text-ready/render/complete callbacks allocate and lock zero times. Local RF includes text while peer and parrot outputs remain unchanged.
- [ ] **Commit:** Scoped local-bus/composition/tests/docs: `feat: mix text speech into local RF without peer leakage`.

### Task T6: Fence lifetime, cancellation and observation across both hosts

**Files:** Modify `rust/product/src/{lifecycle.rs,lifecycle_tests.rs}`, `rust/product/src/media/{text.rs,text_tests.rs}`, existing station generation/diagnostic code and tests, `rust/standalone/src/host_services.rs`, `rust/asterisk/src/services.rs`, `tests/test_rust_asterisk_lifecycle.py`. Use C8's host8 without another ABI change.

**Interfaces:** T1's complete origin plus T3's job sequence tags every ready/completed/retired endpoint. `TextObservation` adds atomic snapshots of admitted/rejected-by-reason, forwarded/rejected destinations, synthesis failures, cancellations, queued/active jobs and per-origin ring occupancy to existing diagnostics. No human body/secret is a log field and no new REST/WebSocket API is introduced.

- [ ] **Red:** Add `text_generation_fences` / `text_lifecycle_faults`: `assert_eq!(late_ready_from_old_connection().played,0); assert_eq!(reused_peer_number().old_jobs,0); assert_eq!(reload().replayed_forwarded_messages,0); assert_eq!(reload().new_dso_loads,0); assert!(all_contexts_outlive_callbacks()); assert!(failure_logs().contain_no_body_or_secret());`. Test disconnect during stdin stall/full ring/render, partial-audio synthesis failure, host unload and repeated enable/start/stop; other audio and forwarding continue.
- [ ] **Run red:** `cargo test --locked -p rptadv-product text_generation_`; `cargo test --locked -p rptadv-product text_lifecycle_`; expected stale-origin/observation failures.
- [ ] **Implement:** Cancel the exact origin generation on disconnect/replacement, drain retirement queues and join bounded workers off audio before unloading speech/provider contexts. Make delayed completions inert without freeing them in a callback. Ordinary reload prepares/publishes generation state; it does not replace DSOs or replay text. Reuse both hosts' event/logging ports, rate-limit repeated metadata-only errors and retain the first failure cause. Do not add errors that tear down otherwise healthy audio calls because local speech failed.
- [ ] **Green:** Repeat focused tests and `python3 -m unittest discover -s tests -p test_rust_asterisk_lifecycle.py`. Verify both hosted and standalone compositions, missing speech capability rejection at preparation, normal synthesis failures with skip-only semantics, endpoint retirement and PTT release. Proceed to integration I1/I2.
- [ ] **Commit:** Exact lifecycle/observation/host/tests files: `fix: fence incoming text lifetime and expose bounded failures`.
