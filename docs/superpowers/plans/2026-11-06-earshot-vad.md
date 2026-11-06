# Earshot VAD Implementation Plan

> **For agentic workers:** Use subagent-driven development with the ownership below. Preserve existing working node operation while building the matched candidate.

**Goal:** Add the requested three-mode local Earshot VAD, preserve speech onset using the existing ring, and install Kerchunk mode on node524950.

**Architecture:** One detector in the released radio receive owner; explicit raw RF versus speech admission; bounded pre-roll in the existing delay; shared typed callbacks and configuration.

**Tech stack:** Rust, Earshot1.2.2, released samplerate ABI2, current SDK/RPT lifecycle, Debian13 native builds.

**Spec:** ../specs/2026-11-06-earshot-vad-design.md

## 1. Detector and policy — agc_detector

Files: radio `src/vad.rs`, `src/samplerate_client.rs`, their focused tests. Coordinate module declarations and Cargo metadata with the radio integration owner.

- [ ] Record exact scalar/argument/return contracts and retained matching Qwen attempts before nonmechanical authorship; use existing deterministic tools for boilerplate.
- [ ] Run meaningful RED for Off, first-speech latch, RF rearm, On AND/sole-voice behavior, and 48,000-sample hang.
- [ ] Implement actual SRC counts, fixed 256-sample analysis, reset plus three zero predictions, native timestamps, finite analysis and failure handling.
- [ ] Verify real speech/noise/silence, rapid rekey, variable partitions, failure bounds, no allocation, and appropriate production coverage.

## 2. Radio integration and ABI — inspect_usbradio

Files: radio `src/session.rs`, `src/lib.rs`, receive qualification/tests, C header, Cargo/build/package/toolchain metadata and docs; exclude the detector owner's files.

- [ ] Register Earshot and released SRC dependency; alpha10, ABI/SONAME5, Rust minimum1.87 and matching package checks.
- [ ] Insert undelayed analysis after squelch; preserve Off; retain every RF/blanking/duplex/disabled-source guard.
- [ ] Add raw eligibility, speech admission, fresh decision and burst metadata; reuse internal delay safely for ASL and preserve external-ring ownership for standalone.
- [ ] Preserve same-config state, live update/rollback, bounded work and control-owned reclamation. Verify actual header/DSO contracts and targeted/full applicable tests.

## 3. Shared settings and tuner — inspect_hybrid

Files: USB core station/native configuration and tests, `rust/radio` typed ABI client, station translation, tuner catalog/menus, owned operator docs. Coordinate with root on SDK header/product files.

- [ ] Add `vad_mode` with Off default and `vad_hang_ms` default1000 to the proper receive/radio scopes; preserve existing VOX settings.
- [ ] Add mode and hang controls to both tuner backends. Normalize unsupported values through existing warnings/default rules and document Kerchunk without RF qualifiers.
- [ ] Translate radio ABI5 fields and internal/external delay ownership. Update required provider/package bounds and verify source-based manuals/examples.
- [ ] Run scoped RED/GREEN, ABI mismatch, parser inheritance, menu/edit/reload, formatting, lint, docs and coverage checks.

## 4. SDK callbacks and standalone ring — root

Files: USB public SDK/product/runtime callback bridge and tests; RPT canonical callback binding, host-services/native radio control query, product worker/ring and tests; package/architecture records.

- [ ] Add sized RX metadata/direct ABI6 and native request ABI4 with early mismatch rejection; preserve TX metadata and legacy controls.
- [ ] Prepare the effective reserve through a control-plane receive-policy query; use host ABI10 for changed RX callback semantics.
- [ ] Keep raw burst/reset token separate from epoch-matched speech admission. Continue ring render during pending speech and gate delivery/keying after render.
- [ ] Detect partial writes and gaps; fail closed, reset/re-prime and require fresh speech. Prove no old-burst replay, no kerchunk keying, preserved first-syllable PCM, and hang on zero-valued admitted audio.
- [ ] Preserve the installed Peak Limiter, all mandatory meters, AGC timing, calibration and PTT behavior. Complete independent source review before matched builds.

## 5. Native installation and verification — root

- [ ] Build exact released radio5, SDK31 and RPT22 artifacts in fresh verified amd64 QA and the private arm64 builder; use real compiler packages and normal dependency checks.
- [ ] Keep the just-installed peak cohort and current configs for rollback. Re-read live settings before adding Kerchunk/1000ms.
- [ ] Install the matched local cohort under existing authorization, verify every installed/running payload and both settings, then inspect real callback health and VAD diagnostics.
- [ ] Continue broader tests as appropriate; record honest scope and any manual RF-validation limits. Clean owned containers and private build root when all authorized work is finished. No push, PR or publication.
