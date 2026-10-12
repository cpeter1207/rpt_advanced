# Functional Library Separation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move nine existing implementations into independently released dynamic libraries, preserving behavior, and update their consumers and ADR status.

**Architecture:** Keep the current product and radio integration libraries. Move leaf implementations and tests into narrow C-descriptor `cdylib` providers; consumers keep only forwarding clients and application policy. Adopt each provider only after its required gate and release.

**Tech Stack:** Rust 1.85/edition 2024, C-compatible descriptors, Debian 13 amd64/arm64, existing shared-library GitHub workflows and signed package index.

**Spec:** [Approved extraction design](../specs/2026-10-12-functional-library-separation-design.md).

Status: Prepared for implementation-plan review; implementation has not started.

## Global Constraints

- Move the implementations, their developer documentation, and their tests.
- Add or change code only where a shared-object boundary requires it.
- Each new component starts with ABI/SONAME 1.
- The radio's existing session descriptor and public ABI 4 can remain unchanged.
- Consumers use the released dynamic object, with minimum-compatible dependencies.
- No vendoring, static archive, or Cargo path/git dependency embedding a released implementation.
- No algorithm, arithmetic, rate, level, timing, configuration or routing changes.
- Processing remains allocation-free, lock-free and nonblocking.
- Do not implement other wishlist features or new DTMF PCM generation.
- Do not install anything on 524950 during this work.
- Full gates run in GitHub; 100% production line/branch coverage is required on Debian 13 amd64 only.
- Developer documentation precedes commits; operator documentation precedes PRs.

## Review Focus

1. Same stream split into different callback sizes must retain identical PCM and signaling transitions (Tasks 2–4, 6–8).
2. Rejected input must preserve the same state/output behavior; an opaque pointer must not accidentally alias a copied detector (Task 2).
3. Prepared reload must preserve acquired decode, oscillator phase and independent RX/TX ownership (Tasks 3–4).
4. Caller-supplied time and strings must retain boundary semantics, including midnight, UTF-8 lengths and optional selectors (Tasks 5, 9–10).
5. A package must load only the expected released ABI, never a test copy, build-tree fallback or statically embedded implementation (Tasks 1–12).

## Working layout and common verification

All repository paths below are siblings of `rpt_advanced` under `2026-08-30`.
Reuse suitable existing checkouts; preserve outstanding work and the eight
existing ADR-status edits. Inspect the detached radio checkout before placing
it on a branch; do not reset or delete its untracked packaging outputs.

New repositories are named `librptadv{component}`, for components `squelch`,
`ctcss`, `dcs`, `signaling`, `tone`, `morse`, `dtmf`, `template`, `schedule`.
Each initial implementation release uses Cargo/version `0.1.0-alpha.1`, tag
`v0.1.0-alpha.1`, Debian `0.1.0~alpha1-1`, SONAME `librptadv{component}.so.1`,
runtime package `librptadv{component}1`, and development package
`librptadv{component}-dev`.

Reuse `librptadvradio`'s build/package scaffold, not its session implementation:
`Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `Makefile`, `Doxyfile`,
`debian/`, `containers/quality.Dockerfile`, thin `.github/workflows/` callers,
`tests/descriptor_smoke.c`, `tests/test_packaging.py`,
`tests/test_container_launcher.py`, and the labeled launcher. Remove unrelated
radio dependencies/checks. Keep the common repository rules synchronized.

Each provider adds `src/lib.rs` (descriptor/lifecycle), the moved source files,
`include/rptadv{component}/rptadv{component}.h`, and `rptadv{component}.pc.in`.
The export is `rptadv_{component}_descriptor()`. Its header uses the existing
`struct_size`, `abi_version`, `capability_name` prefix. The capability is
`rptadv.{component}`. Owned state is opaque; buffers have explicit lengths;
booleans cross as `uint32_t`; existing operation failures use documented status
values. Lifecycle operations are setup-only `create(..., handle **out)` and
`destroy(handle *)`. Keep existing Rust method signatures in consumer wrappers.

For every component task:

- Start by recording the source commit/path and running its existing tests.
- Write/run the descriptor and dynamic-consumer regression before adding the
  wrapper; the new boundary must fail because it does not exist yet, not because
  the toolchain or an unrelated dependency is missing.
- Move existing implementation/tests, then add minimal glue. Keep a source-move
  ledger separating unchanged moves, necessary translation and deleted copies.
- Run targeted moved tests, boundary tests and affected consumer tests. Store
  results in ignored `.work/quality-progress.md`; do not rerun completed reports.
- Run `make lint static-analysis` before pushing and `make docs` before the
  implementation commit. Use the freshly pulled maintained quality image through
  `rpt_advanced-workflows/tools/run-in-quality-container.sh IMAGE WORKSPACE COMMAND`.
- Update package/operator documentation, open and attach a PR, require
  `quality / Required quality gate`, merge, tag and verify the release artifacts.
  Do not tag the bootstrap commit or bypass required checks.
- Switch the consumer only after release. Re-run its affected tests and update
  its package dependencies; commit/push after its fast checks. No node install.

Use `cargo test --lib --locked FILTER` in providers and
`cargo test --locked -p rpt-advanced-core FILTER` in the controller. A test that
exists only to call `AudioSource` stays with the controller wrapper, not the
independent provider. Preserve ordinary composition tests after moving private
algorithm tests.

Comparison assertions use the captured baseline from the same architecture;
do not compare platform-specific sine implementations across architectures:

```rust
assert_eq!(actual_written, baseline_written);
assert_eq!(actual_pcm.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
           baseline_pcm.iter().map(|x| x.to_bits()).collect::<Vec<_>>());
assert_eq!(actual_events, baseline_events);
assert_eq!(actual_error, baseline_error);
```

Record baseline samples/events under ignored `.work/`; do not retain a second
production algorithm in a test fixture. The moved original tests remain the
permanent regression suite alongside ABI and composition cases.

## Task 1: Establish dependency publication and initial repository gates

**Files:** Companion workflow `README.md`, `rptadv-shared-library-workflows/.github/workflows/release.yml`, `USBRadioPlus-Workflows/actions/dispatch-package-index/action.yml`, `USBRadioPlus-Workflows/actions/install-shared-dependencies/action.yml`, `USBRadioPlus-Workflows/.github/workflows/packages.yml`, `usbradioplus/.github/workflows/packages.yml`, and their existing workflow tests.

**Interfaces:** Reuse `quality.yml(code_ref, full)` and `release.yml(code_ref, tag, package_target)` in `rptadv-shared-library-workflows`. Existing `package_target` is `debian-package-check`; no alternate release system.

- [ ] Add failing workflow tests for the nine approved repository identities and rejecting unrelated dispatch sources. Assert that index publication consumes released packages, not arbitrary source refs.
- [ ] Run the workflow repositories' existing test/lint targets; confirm the new allowlist/publication expectations fail specifically.
- [ ] Extend the existing dispatch and package-download lists, reusing secret names without printing values. Shared-library release dispatches only after successful artifact publication. Keep workflow-only pushes independent of production gates.
- [ ] Run `python3 -m unittest discover -s tests` in shared/USB workflow repositories, `python3 tools/test_release_artifact_path.py` and `python3 tools/test_radio_product_dependencies.py` in the controller workflow repository, plus Actionlint and applicable ShellCheck/Ruff. Use the labeled launcher for container checks. Commit/push and merge validated workflow changes without requiring a production gate to fix workflows.
- [ ] Create each approved repository with synchronized rules, license and thin callers. Establish main/PR protections and Pages publication using the existing repositories as templates. Put production implementation in an implementation branch/PR, never the bootstrap main commit.
- [ ] Verify source callers grant the reusable workflows' required permissions. If index credentials are unavailable, stop and request setup rather than publishing an unsigned or alternate repository.

## Task 2: Move squelch; adopt it in the radio library

**Files:** Move `librptadvradio/src/micor_squelch.rs` to `librptadvsquelch/src/micor_squelch.rs`; create `librptadvsquelch/src/vox.rs`; modify `librptadvradio/src/receive_frontend.rs`, `receive_path.rs`, `lib.rs`, and add `src/squelch_client.rs`.

**Interfaces:** Provider `noise_update(handle, previous_closed, sample_power: f64, open_level: u32, hysteresis: u32) -> bool`; `vox_advance(handle, native_offset: u32, qualified: bool, hang_frames: u32)` and `vox_finish(handle, native_frames: u32) -> bool`. The VOX handle retains the existing remaining-frame count and prior offset. The consumer still computes envelope/PCM qualification with the existing rounding helper.

- [ ] Baseline `micor_squelch::tests::`, `receive_frontend::tests::`, and `receive_path::tests::vox_`. Add `shared_squelch_matches_baseline_for_irregular_partitions` comparing gates for partitions 1, 7, 480 and 960.
- [ ] Add boundary tests asserting invalid frontend parameters/input leave history, output, gates and detector behavior unchanged; zero spans and VOX bypass do not advance MICOR.
- [ ] Move comparator coefficients/update/tests unchanged. Move VOX countdown/reset/trailing advancement, leaving metering and threshold conversion in the radio owner. Resolve the opaque state only at setup; cache operation pointers.
- [ ] Keep one preallocated opaque MICOR handle outside the frontend's `Copy` scalar state. Retain complete parameter/PCM prevalidation before advancing it. All subsequent sample conversions see the same validated immutable input; no new rejection, copy API or scratch detector is needed. Preserve the handle through control-only updates and the existing scalar commit at span completion.
- [ ] Run the same filters and new boundary tests; release the provider using the common sequence. Replace old implementation ownership with `squelch_client.rs`, keeping frontend/radio composition tests and radio ABI 4.

## Task 3: Move CTCSS encode/decode

**Files:** Move `librptadvradio/src/{ctcss_receive,ctcss_transmit}.rs` into `librptadvctcss/src/`; add radio `src/ctcss_client.rs`; update `receive_path.rs`, `session.rs`, `lib.rs`.

**Interfaces:** Separate RX/TX handles. RX retains `configure(tone_mask:u64, relax:bool)`, `enabled()`, `decoded()->i16`, `tail_tone_active()`, `clear_tail_on_carrier_loss(bool)`, `process(samples:*const f32,count:u32,carrier:bool)->i16`. TX retains `legacy_frequency(f64)->f64` and the current generate parameters `(output, frames, frequency_hz, peak, enabled, phase_shift_degrees)`, with phase owned by its handle.

- [ ] Baseline `ctcss_receive::tests::`, `ctcss_transmit::tests::` and radio `session::tests::prepared_update_` tests. Add `shared_ctcss_matches_baseline` for acquisition, missing tone, phase reversal and 55 Hz tail using the existing fixtures.
- [ ] Move both modules and their common frequency table unchanged. Move detector-private assertions out of radio tests; do not export test hooks. Keep `ctcss_render_state.rs` and conditioning in radio.
- [ ] Add descriptor tests for independent RX/TX lifetime and chunked-versus-whole output. Assert a prepared radio update retains acquired decode and TX phase.
- [ ] Run targeted tests, release CTCSS, replace owner fields/calls with thin clients and remove the original algorithms. Keep 8 kHz conditioned RX and 48 kHz TX unchanged.

## Task 4: Move DCS encode/decode

**Files:** Move radio `src/{dcs_receive,dcs_transmit}.rs` into `librptadvdcs/src/`; add radio `src/dcs_client.rs`; update `receive_path.rs`, `session.rs`, `lib.rs`.

**Interfaces:** RX `configure(code:i32,inverted:u32)`, `valid()`, `enabled()`, `process(stereo:*const f32,frames:u32)->bool`. TX `configure(code:i32,inverted:u32)`, `update_config_from(prepared)`, `generate(output,frames,peak:f32,enabled:u32,turnoff:u32)`. Preserve `code_supported(i32)->bool` and `configured_code(i32)->i32`.

- [ ] Baseline `dcs_receive::tests::`, `dcs_transmit::tests::`; add `shared_dcs_reload_preserves_phase_and_bit_accumulator` using the current prepared-update fixture.
- [ ] Move correction tables, polarity, qualification, PCM conversions and both oscillators intact. Keep `dcs_turnoff.rs` in radio. A fixed 48,000 Hz constant does not require extracting a timer library.
- [ ] Test all existing correction/polarity/tail cases through moved tests; compare stereo-left RX and mono TX output with the baseline across irregular spans.
- [ ] Release DCS, adopt it, and run radio `prepared_update_`, `dcs_normal_and_turnoff_`, and `dsp_noise_gate_and_dcs_mode_` tests. Remove the old implementation source.

## Task 5: Move portable GPIO/parallel signaling

**Files:** Move pulse state/publication from `rptadv-gpio-adapter/src/lib.rs` to `librptadvsignaling/src/pulse.rs`; RTX/binary selection from `src/parallel.rs` to `librptadvsignaling/src/parallel.rs`; add hardware-adapter `src/signaling_client.rs`.

**Interfaces:** Opaque pulse owner exposes `publish_timed(mask:u32,duration_ms:u32,invert:bool)`, `publish_scheduled(schedule:u32,cancel:u32,duration_ms:u32)`, and `evaluate(now_ns:u64,baseline:u32)->u32`. Nanoseconds are adapter-provided elapsed monotonic time from that handle's origin. RTX operations retain receive/transmit frequency, output byte, transmit selection and adapter-supplied `write(context,u8)->i32`/`delay(context,u32)` callbacks. Preserve the shared first-program delay state and existing full-program/clear ordering.

- [ ] Baseline pulse and RTX/binary tests. Add `shared_signaling_matches_transport_trace`, asserting identical pin writes, delays and output masks for the existing fixtures.
- [ ] Move atomic publication and pulse application in their current order. Translate `Instant` deadlines to caller-supplied elapsed nanoseconds only at the ABI seam, retaining sub-millisecond service timing and observe-time expiry origin.
- [ ] Move RTX word/bit sequencing without transport code. Keep hardware service threads, actual clock/delay acquisition, EEPROM, USB reports, raw I/O, discovery and errors in the adapter.
- [ ] Assert same-bit replacement, disjoint overlapping pulses, cancellation, expiry exactly at deadline, first delay 200/subsequent 4 and partial write failure ordering. Release/adopt and retain fake-transport integration tests.

## Task 6: Move tone generation

**Files:** Move core `audio/tone.rs` and `tone_tests.rs` into `librptadvtone/src/`; core `audio/tone.rs` becomes the thin client. Add provider `src/oscillator.rs` for only the waveform primitive already shared by tone/Morse arithmetic.

**Interfaces:** `ToneSequence::new(&str,i8)`, `rendered_samples()->usize`, `render(&mut[f32])->usize` remain the Rust-facing client API. The provider also exposes opaque oscillator `create(frequency_hz:f32,amplitude:f32)`, `reset()`, and `render(output:*mut f32,count:usize)` for Morse; its arithmetic is exactly the current phase/sine sequence.

- [ ] Baseline `audio::tone::tests::`; add `shared_tone_matches_baseline_bits` using `1100/1,1100/1`, dual-tone and silence fixtures, comparing `f32::to_bits` and written counts for whole/7-sample spans.
- [ ] Move the parser/renderer. Retain `AudioSource` implementation and trait-only test in the controller. Preserve 256 segments, 60,000 ms per segment, 5,760,000 total samples, level bounds and grammar.
- [ ] Exercise waveform phase reset and empty output spans. Release tone before Morse links against it; adopt the released tone client and remove the original renderer.

## Task 7: Move Morse, dynamically using tone

**Files:** Move core `audio/morse.rs` and `morse_tests.rs` into `librptadvmorse/src/`; add provider `src/tone_client.rs`; core `audio/morse.rs` becomes the thin client.

**Interfaces:** Preserve `MorseRenderer::new(text:&str,speed_wpm:u32,frequency_hz:f32,level_db:i8)`, `render(&mut[f32])->usize`, and `restart()`. Consume only Task 6's released oscillator descriptor operations.

- [ ] Baseline `audio::morse::tests::`; add `shared_morse_matches_baseline_bits` for the supported alphabet, fractional WPM timing, partitions 1/7/960, restart and element phase resets.
- [ ] Move alphabet/segment/gap/fractional arithmetic unchanged. Replace only sine/phase production with tone calls, preserving precomputed step/amplitude arithmetic. Keep gaps and keyed reset decisions in Morse; never translate into tone-sequence strings.
- [ ] Verify `readelf -d` requires `librptadvtone.so.1` and the package declares its released dependency. Release/adopt Morse and preserve controller `AudioSource` behavior tests.

## Task 8: Move DTMF detection

**Files:** Move core `audio/dtmf.rs` and `dtmf_tests.rs` into `librptadvdtmf/src/`; core `audio/dtmf.rs` becomes a thin detector/digit client.

**Interfaces:** Preserve `new(muting:bool)`, `set_muting(bool)`, `suppressing()->bool`, `process(receiving:bool,audio:&mut[f32],emit:FnMut(DtmfDigit))`, and `DtmfDigit::frequencies()->(f32,f32)`. ABI digit events are standard ASCII bytes through a borrowed callback/context valid only for the process call.

- [ ] Baseline `audio::dtmf::tests::`; add `shared_dtmf_matches_baseline_events_and_samples` using existing keypad, twist, short-gap and carrier-loss fixtures.
- [ ] Move detector constants, filter/state, digit table and tests unchanged. Only translate callback/digit representation. Leave `runtime/dtmf.rs` untouched except imports if necessary.
- [ ] Assert large/small partitions preserve every digit and mutated PCM, including the 612-sample detector interval and carrier-loss completion. Release/adopt; do not add generation, command mappings or authorization changes.

## Task 9: Move bounded message-template mechanics

**Files:** Move scanner/bounded expansion from core `template.rs` into `librptadvtemplate/src/template.rs`; keep core's `MessageTemplate`, `TemplateValues`, errors and seven-key policy as a thin client.

**Interfaces:** Provider `validate(text:&str, keys:&[(&str,usize)],maximum:usize)->Result`; `render(text:&str,values:&[(&str,&str)],output:&mut[u8])->Result<usize>`. C entries use explicit byte-span/key-value records; no parser tree, clock, translation bundle or controller values live in the provider.

- [ ] Run existing template/config/render tests; move generic assertions into provider tests. Add `generic_template_preserves_literal_and_byte_limit_behavior`: unknown/malformed placeholders fail; ordinary `$` and `}` stay literal; multibyte strings count bytes; output 127 succeeds and 128 fails under the controller's bound.
- [ ] Move traversal and checked-length logic, replacing only the enum-to-policy lookup with caller-supplied keys. Keep controller maxima 32/10/8/14/106/63/63 and total 127. Actual rendered values remain subject to total bound, not a new per-value bound.
- [ ] Compare existing render/error results; release/adopt and rerun `template` filters plus affected config/schema and runtime-render tests. No Fluent/i18n rewrite.

## Task 10: Move generic civil scheduling

**Files:** Move generic portions of core `schedule.rs` and `schedule/tests.rs` into `librptadvschedule/src/schedule.rs` and tests; retain controller `ScheduledAction` enum/parser and policy in `runtime/{scheduler,link_schedule}.rs`.

**Interfaces:** Preserve `CivilTime::new(u16,u8,u8,Weekday,u8,u8)`/`components()`, `ScheduledEvent::from_str`/`occurrence`, and `ScheduledWindow::parse(days:Option<&str>,dates:Option<&str>,start:&str,end:&str)`, `matches`, `overlaps`, `elapsed_after_end`, `next_start`, `next_end`. Preserve the four existing selector/clock validators. Use explicit C civil/event records, an opaque window, clone/equality operations needed by callers, and presence flags for optional results/selectors; do not expose Rust enums/layouts.

- [ ] Baseline `schedule::tests::`; move all calendar/window tests except controller action vocabulary. Add ABI comparisons for `None` versus empty selectors, midnight end, leap day, weekday/date intersections, seconds 59/60, and next-boundary `(date identity, remaining milliseconds)` ordering.
- [ ] Move parser/date calculations unchanged. Keep Sunday-zero weekdays, current date/year limits and exclusive window end. Translate records in wrappers without reparsing or implementing duplicate calculations.
- [ ] Release/adopt; rerun `schedule::tests::`, `runtime::scheduler` and `runtime::link_schedule` filters. Retain controller actions, clocks, retries, duplicate suppression and reload behavior.

## Task 11: Complete downstream artifact integration

**Files:** Affected consumers' `Cargo.toml`, build/link settings, `Makefile`, `debian/control`, package tests; `rpt_advanced-workflows/.github/actions/install-shared-dependencies/action.yml`, `.github/workflows/images.yml`; corresponding USB dependency/image files from Task 1.

**Interfaces:** Direct runtime dependencies are radio→squelch/CTCSS/DCS, GPIO adapter→signaling, controller product→tone/Morse/DTMF/templates/scheduling, Morse→tone. USBRadioPlus and standalone receive the radio dependencies transitively; do not link every product to every new library unnecessarily.

- [ ] Extend existing package-combination and artifact tests to require the direct SONAME dependencies and forbid old embedded implementations or build-tree RPATHs.
- [ ] Update dependency floors to actual released versions and publish their signed index entries. Refresh and verify native amd64/arm64 quality-image manifests before downstream gates consume them.
- [ ] Run affected package/link tests, existing product ABI tests and isolated adapter load/reload/unload tests in containers. Assert no Asterisk/ASL3 dependency enters standalone or the new portable libraries.
- [ ] Run consumer fast checks, update operator package docs, commit/push; create/attach PRs and wait for their full hosted gates before merging where dependency release requires it. Keep production coverage in each implementation-owning repository, not duplicated in consumers.

## Task 12: Finish evidence and ADR statuses

**Files:** `rpt_advanced/doc/architecture/README.md`, `decisions/README.md`, ADRs `0011`, `0018`, `0020`, `0021`, `0022`, relevant `WISHLIST.md` status entries and the prior eight status edits.

- [ ] Review every change against the move ledger: unchanged moved code, necessary ABI translation, removed implementation, or required test/package/docs wiring. Remove unrelated modifications.
- [ ] Record released repositories/packages and dynamic-link evidence for all nine components. Mark their initial ADR 0011 scope implemented without claiming its explicitly future directions are done.
- [ ] Correct public `dylib` terminology to `cdylib` where the stable C boundary is meant; retain ADR 0020's private Rust-dylib decision. Preserve supersession relationships and unrelated pending requirements.
- [ ] Run `git diff --check`, architecture index/status consistency checks, final affected fast checks and developer-documentation checks. Have a fresh reviewer check behavior preservation and dynamic ownership.
- [ ] Commit/push the remaining authorized changes. Report exact commits, dependency releases, hosted gate results and any outstanding PRs honestly. Do not release consumer products or deploy them merely to demonstrate completion.
