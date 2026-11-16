# Explicit Standalone Analysis Reset Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Permit genuine speech to requalify after mapped loss using allocation-free explicit analysis reset while retaining programme continuity.

**Architecture:** Extend the neutral RNNoise descriptor with reset, then reuse radio's existing standalone reset slot behind a feature bit. Keep radio-session/implicit resets untouched. Activate only in a qualified SDK/controller/provider cohort.

**Tech Stack:** Rust/C descriptors, RNNoise 0.2, samplerate ABI2, Earshot 1.2.2, native Debian 13 amd64/arm64.

**Spec:** `docs/superpowers/specs/2026-11-16-analysis-reset-design.md`

## Global Constraints

- Scope is explicit standalone reset only; no changes to implicit jumps, radio-session carrier/replacement behavior or programme RNNoise bypass.
- No detector input from PLC or fabricated silence; no replacement pools, new workers, PCM heuristics or callback allocation/free/locks/I/O.
- Keep programme ring/hold/admission/source-age ownership unchanged on successful reset.
- Preserve active ABI prefixes and SONAMEs; reject mismatched providers at setup without silent fallback.
- No production implementation until the preceding task's independent review passes.
- No publication, releases, node access, RF operation or unrelated cleanup.
- Preserve concurrent controller work and untracked `target-speech`; never stage unrelated files.
- Use current repo instructions, freshly pulled labeled quality images, bounded serial focused checks, and command-local `codex <codex@localhost>` attribution.
- Record exact source commits, DSO hashes, package versions, architecture, image digest and test logs. Local candidate qualification is not release or hardware acceptance.

## Review Focus

- Failed reset midway through a chain must never publish stale success; pin with Task 2 stage-failure/retry tests.
- A short descriptor may have adjacent readable memory but not a valid reset field; pin with Task 1/3 prefix-size tests.
- Fractional converter phase must not leak old source tags after nonzero-origin reset; pin with Task 2 phase tests.
- A ready partial 480/256 frame may bridge loss despite an empty score; pin with Task 2 partial-frame tests.
- Repeated loss and rejected sink credit must not repeatedly reset/advance unchanged successor input; pin with Task 3 controller retry tests.

---

### Task 1: Prove and implement the RNNoise dependency reset

**Checkout:** `/home/codex/rpt-development/shared-components/rptadv-rnnoise-adapter`, audited base `5968c12`; isolate implementation after rechecking clean state.

**Files:** `src/ffi.rs`, `src/lib.rs`, `src/tests.rs`, `include/rptadv_rnnoise_adapter/rptadv_rnnoise_adapter.h`, `tests/descriptor_smoke.c`, `debian/tests/install-check`, `Makefile`, `QUALITY.md`; add `tests/reset_allocation_guard.c` only for native instrumentation. Align `Cargo.toml`, `Cargo.lock`, `debian/changelog` and package metadata when assigning an actual candidate version.

**Interfaces:** Consume upstream `rnnoise_init(*mut DenoiseState, *mut RnnModel) -> c_int`. Produce appended ABI1 `reset(*mut Denoiser) -> c_int`, and `RPTADV_RNNOISE_ADAPTER_DESCRIPTOR_RESET_MIN_SIZE`.

- [ ] Inspect the checksum-verified 0.2 release archive and existing Debian patches without changing upstream. Record generated `init_rnnoise`, layer helpers and amd64/arm64 architecture selection/build flags. Resolve whether the NULL/default-model path is bounded and allocation/free/lock/I/O-free; stop on contrary evidence.
- [ ] Add failing fake-table tests: `reset_preserves_handle_without_processing`, `reset_clears_scratch_and_default_model_history`, `reset_rejects_null_and_reports_init_failure`, `reset_failure_blocks_process_until_success`. Assert no create/destroy/process call during reset, NULL model, stable handle, repeated success and failed-reset recovery.
- [ ] Run `cargo test --locked reset -- --test-threads=1` in the prepared project environment. Record the expected missing-operation/test failure.
- [ ] Add the single upstream init binding and descriptor reset. Preserve existing create warmup and process/destroy contract; no reset warmup. Enforce failure state only as needed to prevent processing an unsuccessful initialization.
- [ ] Extend C smoke to validate the checked reset tail and exercise dirty/reset/genuine-input behavior. Keep existing non-reset prefix usage valid; do not change its size macro to mean the larger tail.
- [ ] Add real-DSO reset equivalence and allocation/free guard runs. Arm after setup, dirty state, reset and process first genuine frame, repeat many times; ensure a deliberately allocating/freeing control proves the guard is active. Prove on native amd64 and arm64 before claiming this dependency RT contract.
- [ ] Run `cargo test --all-targets --locked`, `make test install-check`, `make lint static-analysis`; record which native/package/full-coverage stages remain pending. Follow existing labeled launcher/image requirements, not a new runner.
- [ ] Update concise contract/quality/package metadata with actual candidate identity; no release or invented published floor. Commit only this task's files locally.
- [ ] Independent review gate: source and native-init evidence, failure semantics, ABI layout, unchanged bypass behavior, real C guard coverage. Do not start Task 2 until accepted.

### Task 2: Implement the explicit standalone radio reset

**Checkout:** `/home/codex/rpt-development/radio-speech-chain`, audited base `2eec2cd`.

**Files:** `src/analysis_frontend.rs`, `src/vad.rs`, `src/speech_analysis.rs`, `src/lib.rs`, `include/rptadvradio/rptadvradio.h`, `tests/descriptor_smoke.c`, `tests/vad_allocation_guard.c` if needed, `debian/control` and existing candidate metadata.

**Interfaces:** Consume Task 1 checked RNNoise reset. Produce `FrontEnd::reset() -> Result<(), i32>`, `SpeechVad::reset_analysis_history() -> Result<(), i32>`, existing C reset success for accepted configured chains, and capability `RPTADV_RADIO_SPEECH_ANALYSIS_RESET_CAPABILITY = 4`.

- [ ] Stage Task 1's exact candidate DSO/header through the project's isolated supported dependency path. Record dynamic linkage; never replace host/node libraries or static-link/vendored duplicate code.
- [ ] Add failing `explicit_analysis_reset_accepts_successor_origin`: 80 zeros at 1000, reset returns 0, 160 zeros at 1240 return 0/inactive/no last voice for flags plain, bandpass-only, RNNoise-only and combined.
- [ ] Run `cargo test --locked explicit_analysis_reset -- --test-threads=1` and record retirement failure against the old implementation.
- [ ] Add checked reset pointer to radio's neutral RNNoise client; reject short/missing reset providers during RNNoise-selected setup.
- [ ] Implement FrontEnd reset exactly over owned converter/FIR/RNNoise/framing/map state in the spec. Retain coefficients, allocations and processing delay.
- [ ] Implement a dedicated Active history reset with no `predict_f32` calls and no RNNoise process calls. Reset partial neural frame, detector, downstream SRC and coordinate/scratch state.
- [ ] Implement `SpeechVad::reset_analysis_history` and route only `speech_analysis::reset` to it. Invalidate decision/expected-next before fallible work; permit reuse only after all stages succeed. Leave `reset`, `rearm_admission`, session replacement and implicit discontinuity semantics untouched.
- [ ] Add tests `explicit_analysis_reset_drops_partial_frames_and_old_speech`, `explicit_analysis_reset_preserves_fractional_source_mapping`, `explicit_analysis_reset_has_no_synthetic_predictions`, and `explicit_analysis_reset_failure_requires_successful_retry`. Cover 8/16/48 kHz plus 11025 Hz, partial 480/256 frames, nonzero origins, repeated losses, every fallible reset stage, silence after dirty speech then genuine positive speech with successor-only timestamps. Do not assume identical SRC phase to fresh allocation.
- [ ] Extend descriptor smoke and update explicit-reset documentation/capability. Retain implicit-jump retirement tests; replace only assertions that explicitly call standalone reset.
- [ ] Require `RPTADV_REQUIRE_ALLOC_GUARD=1` and the real C interposer for reset plus successor processing. Run focused `speech_analysis::tests` and the new `explicit_analysis_reset` filter, then existing carrier-reopen/session/prepared-replacement tests and `make lint static-analysis test`.
- [ ] Record actual RNNoise candidate minimum in build/runtime package metadata, or mark release-floor publication blocked if no final version is assigned. Commit locally and obtain independent scope/RT/ABI review before Task 3.

### Task 3: Qualify SDK compatibility and activate the controller

**Checkouts:** SDK `/home/codex/rpt-development/sdk-speech-recovery`; controller `/home/codex/rpt-development/phone-plc-courtesy`. Coordinate with the active controller implementer before editing any production file.

**Files:** SDK `rust/product/src/tests/provider_support.rs`, `rust/radio/src/lib.rs` only if a demonstrable compatibility adjustment is required, SDK `debian/control`; controller `rust/product/src/link/phone_speech.rs`, `rust/product/src/link/private_phone.rs`, `rust/product/src/link/phone_media_tests.rs`, `rust/product/src/link/private_phone_tests.rs`, `debian/control`, binding/install validation; relevant ADR 0022/0043/0044 implementation notes when the boundary is implemented.

**Interfaces:** Existing `SpeechAnalysis::reset(&mut self) -> Result<(), Error>` remains. Its setup requires radio capability bit 4 plus the existing preprocessing capability and complete callbacks. No SDK phone wrapper or new host-table slot.

- [ ] Re-read current concurrent controller edits and accept their reviewed reset-on-loss seam. Do not overwrite or reproduce their source/provenance work.
- [ ] Add failing old-provider tests: missing bit 4, short capability word, missing reset pointer, wrong ABI/name all fail control-side setup. An old provider must never be admitted and fail for the first time on packet loss. Keep unchecked-prefix access impossible.
- [ ] Update the phone loader's requirement and comments; reuse its existing callback/error translation. Preserve successful reset's separation from programme gap, ring flush, hold/admission and epoch/age changes.
- [ ] Extend the exact product regression from the parent, then add real-speech recovery and repeated loss/refused-prefix cases. Assert preserved original hold end/birth/retained programme, no synthetic detector/DTMF calls, no synthetic-only key, and later genuine speech requalifies with true coordinates.
- [ ] Inspect SDK descriptor fixtures against the new radio provider. Change only fixture/metadata expectations actually affected; prove existing carrier rearm and programme RNNoise bypass unchanged. Record explicitly if SDK production code needs no change.
- [ ] Bind exact candidate headers/DSOs in the approved isolated test cohort. Run controller focused `phone_speech`, `phone_media` and `private_phone` test filters with required allocator guards; SDK focused radio/product compatibility tests with their existing runner. Record complete commands and source/DSO identities.
- [ ] Align SDK/controller build and runtime minimums with actual qualified artifacts, without assuming old radio alpha11/RNNoise alpha2 supply the feature. Do not invent a new SDK ABI/version dependency where no contract changed; distinguish rebuild/cohort verification from a mandatory API bump.
- [ ] Update ADR notes and reconcile the earlier PLC plan's disproved no-dependency-change assumption narrowly. Preserve unrelated plan/ledger work. Commit coherent SDK/controller changes separately, then independent review.

### Task 4: Whole-cohort acceptance and evidence handoff

**Deliverable:** Reviewed local commits and exact-cohort verification report; not a release or node installation.

- [ ] Verify exact ordered RNNoise -> radio -> SDK -> controller source/package/DSO identities and that old-provider setup rejection passes.
- [ ] Run required platform-independent checks once and native Debian 13 amd64/arm64 build/tests/staged installs/package checks through maintained runners. Run production-only 100% line/branch coverage on amd64. Keep focused evidence distinct from full-gate success.
- [ ] Confirm no code or tests changed broad implicit reset/session behavior, decoder provenance classification, programme ring/hold/admission, or bypass history. Review the diff from the recorded bases.
- [ ] Report native guard evidence, positive-speech/phase/partial-frame results and any package-floor or architecture blocker to the coordinator. If hardware testing becomes desirable, provide a ready-for-manual-test handoff without accessing/deploying to a node.
- [ ] Stop at the local reviewed cohort. Publication, release, deployment and RF remain separately authorized.

## Planning self-review

All spec state owners and failure modes have a task/test above. The SDK is a compatibility/cohort step, not an assumed phone-analysis owner. The only dependency wire extension is RNNoise's reset tail; radio reuses its slot and capability word. Native RNNoise initialization safety and positive recovery are explicitly pending evidence, not inferred from the all-zero reproduction. No delivery-time promise is made.
