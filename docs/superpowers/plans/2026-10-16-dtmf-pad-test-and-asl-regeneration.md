# DTMF pad test and ASL regeneration implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provide the approved local pad test, local-transmitter DTMF regeneration, and ASL-compatible addressed command forwarding in both controller hosts.

**Architecture:** Extend the existing dynamic DTMF provider through the existing dynamic tone provider. Keep command policy on serialized control, and use the existing node fanout worker to stream regenerated tones through a separate local-only PCM ring. Both host transports continue to supply events to the same controller implementation.

**Tech Stack:** Rust, Fluent, rtrb, versioned C descriptors, released shared PCM ring, native Debian 13 quality containers.

**Spec:** [Approved design](../specs/2026-10-15-dtmf-pad-test-and-asl-regeneration-design.md), approved 2026-10-16.

## Global Constraints

- Scope is `rpt_advanced` and `librptadvdtmf`; reuse `librptadvtone` without changing its behavior. No USBRadioPlus, radio-processing, IAX wire-format, or unrelated feature changes.
- Preserve detector muting. Actual local unkey ends pad capture and ordinary commands, but does not exit selected remote mode; literal `#` exits remote mode but is data during pad capture.
- Pad test defaults to public local-RF `*82`, captures the first 127 symbols, and reports only after actual unkey. Loss/reload cancels incomplete capture.
- Regeneration defaults: disabled, 200 ms tone, 50 ms gap, -12 dBFS combined peak; idle start delay 500 ms. Duration/gap accept 1–1000 ms, level -60–0 integer dBFS. Invalid values warn and inherit; ambiguous commands remain errors.
- Regenerate eligible addressed link digits only, never local RF or transit digits. Replace local voice-band output for tone/gap spans, without delaying ordinary media or modifying peer PCM/parrot capture.
- All generated PCM is mono F32, 48 kHz. No synthesis, allocation, locks, waits, logging, or I/O in audio callbacks. Existing transmitter safety inhibits remain effective.
- Preserve generation ownership and exact originating connection for replies. Claimed text source is not authentication. No new replay/acknowledgment protocol.
- Use the existing descriptor-tail convention and dynamic packages. Preserve DTMF ABI/SONAME 1; fail safely when the required generator tail is unavailable.
- Use TDD and targeted checks; record completed cases in ignored `.work/quality-progress.md`. No local full gate. Before push, formatting/lint/static analysis pass; full PR gate requires native Debian 13 tests on amd64/arm64 and production coverage on amd64.
- Developer documentation accompanies implementation commits. Operator documentation is updated before a PR. Release, publication, node installation/restart, and external links require separate authorization.
- Preserve the pre-existing, unrelated empty `docs/superpowers/plans/2026-10-13-adr0037-peer-ingress.md`; do not stage or restore it.

## Review Focus

1. Literal `#`, timeout, actual unkey, and event loss must remain distinct after long pauses and across reloads — Task 2.
2. Different claimed origins through one peer, or a replacement connection using the same name, must not inherit command/admin state or stale replies — Task 4.
3. A zero-valued gap is active replacement audio, but an empty/failed producer must not suppress voice or hold PTT — Tasks 5–6.
4. A full tone ring or expensive speech preparation must not stall peer fanout; retirement must stop old-generation tone production — Task 5.
5. Maximum pad capture and long translations must remain bounded and use one serialized response, with working Morse fallback — Task 3.

## Files and ownership

Paths below are relative to `rpt_advanced`, unless prefixed with `../librptadvdtmf`.

| Area | Files | Responsibility |
| --- | --- | --- |
| DTMF provider | `../librptadvdtmf/src/{lib,dtmf,generator,tone_client}.rs`, provider header/tests/build/package files | Reuse frequency map and tone DSO; extend opaque generator descriptor |
| Provider client | `rust/core/src/audio/{dtmf,dtmf_tests}.rs`, `rust/core/build.rs` | Checked generator binding, no copied DSP |
| Command policy | `rust/core/src/command.rs`, `rust/core/src/runtime/{dtmf,links,aggregate}.rs`, their existing tests | Distinct unkey, pad capture, selection and source-scoped outcomes |
| Configuration | `rust/core/src/config/{schema,settings,settings_tests}.rs` | Inherited command mapping and regeneration settings |
| Pad report | `messages/en-US.ftl`, `rust/core/src/messages.rs`, `rust/core/src/runtime/{aggregate,prepare}.rs` and tests | Bounded localized forms and existing streamed local telemetry |
| Addressed forwarding | `rust/core/src/link/{hub,protocol}.rs`, `rust/product/src/{host,lifecycle}.rs`, `rust/product/src/link/session.rs` and tests | Direct-preferred routing and intact transit text |
| Regeneration producer | new `rust/product/src/link/regeneration.rs` and `regeneration_tests.rs`; existing `link/{mod,dispatcher,ring}.rs` | Bounded digit queue, finite stream, existing fanout owner |
| RF selection | `rust/core/src/link/audio.rs`, `rust/core/src/controller/mod.rs` and existing tests | Consume signaling PCM and replace only local RF output |
| Build/integration | provider and controller package metadata, `.github/dependencies.json`, `tests/test_rust_product_surface.py`, existing host tests | Dynamic dependency, staged smoke checks, both hosts |

Do not split or rename unrelated files. New `generator.rs` and `regeneration.rs` separate provider waveform ownership from controller queue/lifecycle policy.

## Validation setup

Read current `AGENTS.md`, architecture index, relevant ADRs 0011/0019/0023/0025/0026/0027/0040/0041, and the spec before execution. Read the TDD skill and its test guidance. Reuse these existing checkouts after accounting for their local changes.

Run the commands below **inside a freshly pulled native quality image**, through the labeled launcher, not in Windows or a hand-made container. Controller example from its checkout:

```powershell
& 'C:/Program Files/Git/bin/bash.exe' ../rpt_advanced-workflows/tools/run-in-quality-container.sh ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest . cargo test --locked -p rpt-advanced-core dtmf
```

For provider checks, use its existing `tools/run-in-quality-container.sh` with the image named in its current quality caller. Inspect the launcher's arguments before invoking it. Record image digest and exact commands in `.work/quality-progress.md`; launcher cleanup must run on success and failure.

For candidate-provider integration, build Debian packages in the provider checkout, copy the resulting artifacts into ignored controller `.work/`, and install those candidate packages **inside the disposable controller test container only**. This is testing, not a vendored source or static dependency. Before a consumer PR can merge, obtain approval to release the passed provider and use its signed released package.

Product tests need the existing fixture DSOs: run `make rust-build` once after relevant source changes, then run tests with `LIBRARY_PATH=/workspace/target/release`, `LD_LIBRARY_PATH=/workspace/build`, and `RPT_ADVANCED_LIBDIR=/workspace/build`, preserving any existing path entries. Do not replace targeted testing with `make check` or a full local coverage run.

---

### Task 1: Add finite PCM generation to the shared DTMF provider

**Files:** Provider files in the map, `src/generator_tests.rs`, `src/abi_tests.rs`, `tests/descriptor_smoke.c`, `Cargo.toml`, `Cargo.lock`, `Makefile`, `debian/control`, `debian/changelog`, `rptadvdtmf.pc.in`; controller `audio/dtmf.rs` and tests.

**Interfaces:**
- Reuse `DtmfDigit::frequencies() -> (f32, f32)` and the tone descriptor's sequence create/render/destroy functions. Copy the narrow descriptor-client pattern from `librptadvmorse/src/tone_client.rs`, not its implementation of media.
- Provider `DtmfGenerator::new(digit: DtmfDigit, duration_ms: u32, gap_ms: u32, level_dbfs: i8) -> Result<Self, GeneratorError>` and `render(&mut self, output: &mut [f32]) -> Result<usize, GeneratorError>`; zero returned samples means finished. `GeneratorError` is a unit error type.
- Append C descriptor functions: `generator_create(uint8_t digit, uint32_t duration_ms, uint32_t gap_ms, int32_t level_dbfs, struct rptadv_dtmf_generator **out) -> int32_t`; `generator_render(handle, float *out, size_t capacity, size_t *written) -> int32_t`; `generator_destroy(handle) -> void`. Preserve existing detector-prefix offsets.
- Controller wrapper `audio::DtmfGenerator::new(digit: char, duration_ms: u32, gap_ms: u32, level_dbfs: i8) -> Result<Self, DtmfError>` and `render(&mut self, output: &mut [f32]) -> Result<usize, DtmfError>`.

- [x] Write table-driven `generator_all_symbols_match_frequency_level_and_duration` and `generator_partitioning_matches_one_render` tests. For each existing digit, collect generator output using a large buffer and then arbitrary partitions:
  ```rust
  assert_eq!(pcm.len(), (200 + 50) * 48);
  assert!(pcm[200 * 48..].iter().all(|sample| *sample == 0.0));
  assert!(pcm.iter().all(|sample| sample.abs() <= 10_f32.powf(-12.0 / 20.0) + 1e-6));
  assert_eq!(partitioned, pcm);
  ```
  Assert both expected frequency bins dominate unrelated DTMF bins; each component amplitude is half the configured combined peak. Test repeated digits, zero-length output, finish, all invalid bounds, and descriptor null/missing-tail failures. Extend C smoke test to render a digit and still use the detector prefix.
- [x] Run `cargo test --locked generator` in the provider: expect compile failure for missing generator. Record it.
- [x] Implement the generator as one tone sequence, e.g. `697+1209/200,silence/50`; validate once at creation, render incrementally, destroy off audio. Append the descriptor tail and the checked controller wrapper. Require the full tail before reading its function pointers; retain detector-prefix validation separately.
- [x] Prepare next provider alpha `0.1.0-alpha.2` / Debian `0.1.0~alpha2-1`, if not already taken; retain SONAME 1. Add dynamic tone build/runtime dependencies and pkg-config linkage. Include new sources in archives; no new thread, oscillator, or hardware dependency.
- [x] Run provider `cargo test --locked generator`, `cargo test --locked abi`, and `make test install-check debian-package-check`; expect PASS. Run controller `cargo test --locked -p rpt-advanced-core audio::dtmf` with candidate provider installed: old/short descriptor rejected, new generator works. Update Rustdoc and C header comments.
- [x] Commit only affected provider files; retain the controller binding for its consumer commit in Task 2. Do not publish a release yet.

### Task 2: Resolve settings and separate DTMF control outcomes

**Files:** `config/{schema,settings,settings_tests}.rs`, `command.rs`, `runtime/{dtmf,dtmf_tests,links,links_tests,aggregate}.rs`, `audio/{dtmf,dtmf_tests}.rs`, associated package/build minimum checks.

**Interfaces:**
- Add `ResolvedDtmfRegenerationSettings { enabled: bool, duration_ms: u32, gap_ms: u32, level_dbfs: i8 }` and `ResolvedNodeSettings::dtmf_regeneration`.
- Add `LinkAction::PadTest`, command-map entry `link_command_pad_test`, default `82`; update action enumeration and map counts together.
- Add `DigitEvent::Unkey { now_ms: u64 }`. Preserve `Digit` for actual symbols and existing NUL timeout, and `Lost` for invalidation.
- Define `PadTestResult { digits: String, overflowed: bool }`, always at most 127 ASCII symbols, and `DigitOutcome::{Operation(DigitOperation), RemoteCandidate(DigitOperation), PadTest(PadTestResult), Regenerate(char)}` in `runtime/dtmf.rs`.
- Change `DtmfCommands::feed(event: DigitEvent) -> Option<DigitOutcome>` and the existing `NodeLinkControl::{digit,peer_digit}` / `Runtime::digit` return types accordingly. `for_source()` creates a peer collector that cannot capture pad tests.
- Add `CommandCollector::remote_candidate() -> Option<String>` to expose only the in-progress remappable Command destination; `select_remote()` clears its completed collector while retaining selected mode.

- [x] Add `dtmf_defaults_inherit_and_invalid_values_warn`, `pad_capture_waits_for_real_unkey`, and `remote_selection_survives_unkey_not_hash`. Use the existing config and digit-feed fixtures. Capture `*82`, then `*#ABCD0123456789`, pause past 3 seconds, and unkey:
  ```rust
  assert_eq!(capture.digits, "*#ABCD0123456789");
  assert!(!capture.overflowed);
  assert!(timeout_outcome.is_none());
  assert_eq!(overflow_capture.digits.len(), 127);
  assert!(overflow_capture.overflowed);
  ```
  The named variables are outcomes from the fixture sequence, not production helpers. Test empty capture, 127/128 boundaries, repeated `*`, peer rejection, Lost/reload cancellation, and unkey after timeout. Verify ordinary command completion and existing muting tests unchanged. During command collection no `Regenerate` outcome; idle peer digits may produce one, local RF digits may not.
- [x] Run `cargo test --locked -p rpt-advanced-core dtmf` and `cargo test --locked -p rpt-advanced-core command`: expect the new cases to fail before implementation.
- [x] Implement inherited settings with exact spec defaults/bounds. Publish actual unkey on every receive falling edge, even after the timeout marker; do not change the detector. Implement bounded pad capture on control and typed outcomes. Update all exhaustive matches without treating an unkey as a literal digit.
- [x] Update product callers to preserve and dispatch typed outcomes; initially report unsupported new outcomes safely until Tasks 3–5 wire them. Update consumer DTMF package floor to the candidate alpha and tests/build checks. Never claim the feature works at this intermediate commit.
- [x] Rerun the targeted tests plus `cargo test --locked -p rpt-advanced-core config::settings`; expect PASS. Record and commit affected controller files, including source docs and dependency metadata.

### Task 3: Stream the localized pad-test response

**Files:** `messages/en-US.ftl`, `messages.rs` and existing tests; `runtime/{aggregate,aggregate_tests,prepare}.rs`, `product/{host,lifecycle}.rs` and existing tests.

**Interfaces:**
- Add `Message::{PadTestEmpty, PadTestOverflow, DtmfSymbol { symbol: char }}`; all three catalog IDs require `.text`, `.tts`, `.morse`.
- `MessageCatalog::format_pad_test(digits: &str, overflowed: bool) -> Result<MessageForms, CatalogError>` composes already-bounded fragments. Reject non-DTMF symbols and input longer than 127; use at most 128 fragments with a single separator, at most `128 * 128` bytes per form.
- `Runtime::queue_pad_test(local: &str, report: &PadTestResult) -> Result<(), RuntimeError>` uses existing `queue_status_media`, default speech/Morse settings, and `reply=None`.

- [x] Add `pad_test_forms_are_localized_and_bounded` and `pad_test_queues_one_local_stream_after_unkey`. Assert exact English `star`/`pound`/A–D wording, empty `no digits`, overflow before retained symbols, and a translated long-fragment 127-symbol case:
  ```rust
  assert!(forms.tts.len() <= 128 * 128);
  assert_eq!(queued_status_count, 1);
  assert_eq!(peer_reply_count, 0);
  assert!(morse_played_when_speech_unavailable);
  ```
  Use the existing runtime fake station/media fixtures to capture queued sources and fallback playback; include failure/queue-full without replaying stale capture.
- [x] Run `cargo test --locked -p rpt-advanced-core pad_test`: expect missing formatter/queue path or failing routing assertion.
- [x] Add the catalog entries and formatter; build one bounded complete speech/Morse form from fragments without increasing the ordinary 127-byte catalog-message limit. Feed the existing station producer, not a new playback thread or a sequence of separately queued status messages. Dispatch `DigitOutcome::PadTest` only from local RF completion.
- [x] Run the same tests, `cargo test --locked -p rpt-advanced-core messages`, and the affected product pad-test dispatch test; expect PASS. Verify the real media adapter accepts the composed text and Morse fallback rather than testing the formatter alone. Update developer docs and commit.

### Task 4: Match ASL addressed command selection and forwarding

**Files:** `runtime/{dtmf,links}.rs`, `link/{hub,protocol}.rs`, `product/{host,lifecycle}.rs`, `product/link/session.rs`; their existing test files; host ingress tests under `rust/asterisk/src/link/tests.rs` and `rust/standalone/src/host_services.rs`.

**Interfaces:**
- `NodeLinkControl::peer_text` gains the existing `PeerReply` connection identity. Add `LinkEffect::Digit(DigitOutcome)` for destination consumption and `LinkEffect::ForwardDigits(Vec<(String, String)>)` for non-advisory addressed sends.
- `LinkManager::relay_digit(ingress: &str, message: &Protocol) -> Vec<(String, String)>` reuses `eligible` and direct-preferred routing; empty ingress denotes locally originated traffic. Preserve destination, source, sequence and digit.
- Add `DigitOperation::origin: Option<String>` for a claimed relayed source label. Peer collector keys become `(origin, PeerReply)`; existing `reply` continues to carry the authenticated ingress connection. Native peer-digit events use the direct peer name as origin.
- Destination selection consumes `RemoteCandidate(DigitOperation)`: probe existing directory lookup on control, select only if valid/current, leave not-found prefixes collecting, and emit no text. Outgoing digits allocate sequence numbers once on shared node control, not separately per relay/peer session.

- [x] Add `remote_candidate_does_not_forward_prefix`, `remote_digits_route_direct_or_relay`, and `remote_command_origins_do_not_share_authorization`. Existing hub/session fixtures capture text deliveries:
  ```rust
  assert!(destination_entry_sends.is_empty());
  assert_eq!(direct_delivery, "D 3000 1000 17 5");
  assert_eq!(transit_delivery, "D 3000 1000 17 5");
  assert!(!sent_to_ingress_or_source);
  assert_eq!(transit_execution_count, 0);
  assert_eq!(duplicate_iax_digit_count, 0);
  ```
  Also test direct-only preference, disconnected routes, malformed envelopes, unauthorized ingress, two origins through one ingress, expired/replaced connection replies, real `#` exit, unkey/timeout persistence, and addressed-only regeneration eligibility. Claimed source must never bypass ingress policy or gain another collector's admin unlock.
- [x] Run `cargo test --locked -p rpt-advanced-core remote_` and `cargo test --locked -p rptadv-product remote_`: expect failures at current transit discard, synthetic `#`, and direct-only selection.
- [x] Pass intact validated `D` text from `PeerSession::text` to shared control instead of consuming/dropping it there. Route transit without local execution; use existing `Host::text(..., advisory=false)` for addressed traffic. Update selection and peer-origin keying, preserving independent auth/reply identity. Remove obsolete duplicate session-level addressed-digit routing.
- [x] Run those tests and both host ingress test filters; expect PASS with identical controller outcomes for Asterisk and standalone. Do not change their network/channel ABIs. Document shared routing and commit.

### Task 5: Pump regeneration on the existing fanout worker

**Files:** new `product/src/link/{regeneration,regeneration_tests}.rs`; existing `link/{mod,dispatcher,dispatcher_tests,ring}.rs`, `host.rs`, `lifecycle.rs` and tests.

**Interfaces:**
- New private `RegenerationProducer` owns the unique digit consumer, `InboundProducer`, generator, timing, and native-sized scratch; `pump(&mut self, now: Instant) -> Result<(), Error>` performs at most one render/write chunk per call, preserving partially accepted PCM.
- `MediaWorker::prepare` gains `regeneration: Option<(ResolvedDtmfRegenerationSettings, InboundProducer, usize)>`, with the last field the generation's maximum native frame count.
- `PreparedDispatch::queue_digit(&mut self, digit: char) -> bool` writes to a 127-entry generation-owned queue; `regeneration_dropped(&self) -> u64` exposes cumulative drop-new overflow. Share only the counter, never mutable endpoint ownership.
- Prepare `InboundRing::open(48000, InboundPolicy::Media)` only when enabled. The consumer goes to Task 6; the producer stays on `Dispatch`. Existing prepared/active/stopping/quiet lifecycle owns both queue and worker.

- [x] Add `regeneration_waits_500ms_then_streams_without_unkey`, `regeneration_backpressure_does_not_block_fanout`, and `regeneration_retires_with_generation`. Drive `pump` using explicit `Instant` values and the existing ring/fanout fixtures:
  ```rust
  assert_eq!(samples_published_before_500ms, 0);
  assert_eq!(retained_queue_capacity, 127);
  assert_eq!(drops_after_one_excess_digit, 1);
  assert!(peer_batches_advance_with_full_tone_ring);
  assert_eq!(writes_after_quiet, 0);
  ```
  Test disabled mode, 1/1000 ms boundaries, partial write retry without repetition, queued digits without another 500 ms delay, fresh idle start, generation replacement, and generator failure/clean completion. No PLC or stale pending tone may hold output active.
- [x] Run `cargo test --locked -p rptadv-product regeneration`: expect missing queue/pump failures.
- [x] Implement the small producer and call it once per ACTIVE dispatch turn alongside bounded fanout. Keep the idle-delay timer in the producer; create/destroy finite generators there, off audio. Do not use the serial speech producer or add a thread. Wire eligible shared-control outcomes to the active generation's `queue_digit`.
- [x] Expose the drop counter through the existing per-node status/snapshot path without introducing a new status transport. Run the same tests and affected dispatcher/lifecycle tests; expect PASS. Update source docs and commit.

### Task 6: Select generated PCM only for local RF output

**Files:** `core/src/link/{audio,audio_tests}.rs`, `core/src/controller/{mod,tests}.rs`, `product/src/link/ring.rs`, `product/src/host.rs`.

**Interfaces:**
- Extend `PeerInput` with `render_sample(&mut self) -> Option<f32>`; its default reads one sample through `render`, and the released-ring implementation uses its existing precise `InboundConsumer::render_sample`. `None` means no real signaling sample; `Some(0.0)` is a real silent gap.
- `LinkAudio<P>::with_regeneration(self, input: P) -> Self` attaches one optional local-only consumer and preallocates `Vec<Option<f32>>` scratch at the existing maximum size.
- Extend `NodeController::process_audio_with_parrot` and private `process_audio_outputs` with `regeneration: Option<&[Option<f32>]>`; extend private `step` with `regeneration: Option<f32>`. Existing `process_audio` supplies `None`.

- [x] Add `regeneration_replaces_rf_not_peer_program`, `regeneration_gap_is_not_missing_pcm`, and `regeneration_obeys_transmit_inhibits`. Feed the controller and LinkAudio fixtures known local/peer/telemetry/tone samples:
  ```rust
  assert_eq!(rf_during_tone, generated_sample);
  assert_eq!(rf_during_gap, 0.0);
  assert_eq!(peer_program, program_without_regeneration);
  assert_eq!(parrot_capture, capture_without_regeneration);
  assert_eq!(media_position, position_without_regeneration);
  assert_eq!(rf_while_inhibited, 0.0);
  ```
  Verify receive-active full duplex, half-duplex inhibition, transmitter timeout, CTCSS policy, no speech ducking/gain, finite completion, empty/failed ring, variable callback partitions, and ordinary audio resumption without delayed samples. Reject a mismatched scratch span safely using existing frame-bound handling.
- [x] Run `cargo test --locked -p rpt-advanced-core regeneration`: expect missing local signal selection.
- [x] Attach Task 5's consumer in `Host::prepared`; consume it with real-sample identity. Preserve ordinary media advancement and peer mix-minus assembly. Add signaling to local PTT demand under existing safety policy, and select its sample only at the final local RF voice-band composition point before downstream radio processing. A silent gap is not a release event; an empty lane is not silence-to-be-transmitted.
- [x] Rerun targeted controller/link tests and `cargo test --locked -p rptadv-product regeneration`; expect PASS. Verify no audio-side allocations or blocking paths were introduced. Update source docs and commit.

### Task 7: Integrate package boundaries and close the focused test matrix

**Files:** `debian/control`, `rust/core/build.rs`, `.github/dependencies.json`, `tests/test_rust_product_surface.py`; existing provider descriptor smoke and controller host/session/lifecycle tests. Update workflow implementation only if the existing dependency mechanism cannot install the correct candidate/released provider.

**Interfaces:** No new product/host ABI. Controller requires generator-capable `librptadvdtmf1` / `librptadvdtmf-dev` at the Task 1 minimum. Provider requires the existing released `librptadvtone1` / development package. No static/vendored implementation.

- [x] Add staged-package assertions for dynamic `DT_NEEDED`, installed headers/pkg-config, and short/missing generator descriptor refusal through the controller's startup validation function. Exercise shared local capture and local generation plus addressed relay through both host event fixtures; compare captured effects and media routing.
- [x] Run `python3 tests/test_rust_product_surface.py --help` and use its existing package/surface modes, plus `make product-surface product-abi-check install-check`; expect failure until metadata/fixtures match the extended dependency. Run the named new host tests, not every platform suite.
- [x] Synchronize build/pkg-config checks, Debian minimums and dependency manifests; use the actual approved provider revision rather than a guessed SHA. Remove any now-unused local forwarding helpers. Do not weaken checks or exclude production branches to close coverage.
- [x] Run targeted changed-source tests, Rustdoc for changed crates, and affected C-header documentation checks; resolve each recorded gap once. Record results and commit. Run `make -j2 lint static-analysis` in each affected repository before any authorized push; expect zero errors/warnings. GitHub, not the local host, runs the full PR gate.

### Task 8: Review, document, and hand off

**Files:** `doc/architecture/decisions/{0023-unified-control-and-dtmf-policy,0025-native-media-routing-and-pcm-ring-ownership}.md`, `doc/architecture/standalone-iax2-compatibility.md`, `WISHLIST.md`, this plan; operator manuals/examples only immediately before an authorized PR.

- [x] Obtain an independent whole-diff review against the approved spec: local-only generation, all sixteen symbols, source identity, exact `#`/unkey behavior, media replacement, bounded worker progress, ABI loading, and no unrelated changes. Fix findings with focused failing tests before fixes.
- [x] Mark ADR/wishlist items implemented only for verified behavior; preserve unrelated incomplete items. Update developer documentation and the task checklist, distinguishing local evidence from the not-yet-run hosted gate.
- [ ] Before a PR, update configuration reference/examples and operating instructions for `*82`, remapping, four regeneration options, and ASL remote mode. Use plain radio terminology; no history/design narrative. No operator-doc rewrite during earlier code tasks.
- [x] Report commits, targeted evidence, remaining hosted verification, and dependency release prerequisite. Ask separately for any missing publication/deployment authorization. Do not touch 524950 or external links under this plan's current approval.

## Self-review record

- Spec coverage: Tasks 1–2 cover settings/provider and distinct events; Task 3 covers bounded localized pad response; Task 4 covers ASL selection/transit/auth; Tasks 5–6 cover real-time local regeneration; Tasks 7–8 cover both hosts, packages and completion evidence.
- Interface consistency: one typed outcome path, existing `PeerReply` identity, one SPSC producer per media lane, existing dynamic generator/ring boundaries; no new host transport ABI.
- Risk coverage: each Review Focus item has named tests above, including zero PCM versus no PCM, lost events, same-name reconnection, and ring backpressure.
- Scope: no new oscillator, no new thread, no generic protocol framework, no peer-generated PCM, no node change. The unrelated dirty plan stays untouched.
- Execution recommendation: Native execution in this session, followed by one fresh whole-branch reviewer. The tasks share command and lifecycle interfaces, so maintaining one implementation context avoids repeated reconstruction.

## Execution record — 2026-10-16

- Implemented inline in the approved existing checkouts; one read-only whole-diff review. No node, network-link, publication, or release changes.
- Provider checkpoint: `7a3ec75`. Controller tasks share command/lifecycle tests and form one coherent checkpoint rather than committing failing intermediate trees.
- Provider checks: 18 tests, C descriptor smoke, Debian package/staged install, strict lint/static checks, Rustdoc and Doxygen passed.
- Controller checks: 432 core tests passed; 272 product tests passed with one existing manual load test ignored. The final remembered-node retention change passed 17 remote unit tests and three integration tests. Strict workspace Clippy, warning-denied Rustdoc, optimized build, C ABI probe, dynamic artifact checks, staged install and package checks passed. Full logs remain in ignored `.work` evidence.
- Review corrected batched remote selection and bounded per-ingress origin state. Both corrections were reproduced before fixes. Recurring asynchronous test callbacks exercise the standalone host's readiness contract.

### Rulings

- Reuse existing isolated checkouts; read-only helpers may inspect dependencies. No implementer agents or extra worktrees. Cost: one larger implementation context.
- Use targeted native-container checks, not a local full gate, as required by repository/user rules. Cost: hosted architecture/coverage verification remains pending.
- Combine the controller checkpoint because command, routing and lifecycle tests share interfaces. Cost: a larger local commit; the provider remains separate.
- Carry regeneration in the existing per-frame/per-sample bundle rather than adding more method arguments. Same ownership; cost if revisited is a private-bundle change.
- Require Debian pkg-config version `0.1.0~alpha2`, matching the actual packaged version. No new version spelling or ABI change.
- Flush the finite ring's declared eight-sample filter delay at the end, reusing the telemetry producer's rule. Cost: the existing intrinsic filter delay remains; no new reserve.
- Bound retained command origins to 128 per ingress; idle regeneration allocates no collector. Preserve active commands, selected/remembered destinations and admin grants. Cost: excess simultaneous origins on one connection cannot start a new command.
- Exercise the exact startup descriptor validator with short/missing tails instead of a separate old-DSO host fixture. Dynamic package/artifact checks passed; an installed-alpha1 end-to-end startup test is not claimed.
- Run the final package checks on a native Linux temporary copy with Git-recorded 0644 install-manifest modes: the Windows bind mount reports those files executable. No test or package source was changed to bypass the check. Cost: direct bind-mount package-mode checks remain unsuitable.

No review findings were deferred. Operator manuals/examples, provider publication,
and the full hosted gate remain prerequisites for a future authorized PR/release.
