# Radio signaling and output interlock implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Implement and test the software contracts in ADRs 0033–0034 without changing installed CM119 operation or claiming physical interlock qualification.

**Architecture:** Extend the existing radio and GPIO descriptor tails. Reuse the radio signaling state machine and GPIO service-owner pattern; validate their composition with a test-only normalized hardware transport. Do not create another library or an installed simulated-hardware mode.

**Tech Stack:** Rust 2024, existing versioned C descriptors and dynamic DSP providers, Rust unit tests, C shared-object contract tests, native Debian 13 quality containers.

**Spec:** `docs/superpowers/specs/2026-10-16-appliance-signaling-interlock-design.md` **at commit `3f4bb52`**, approved 2026-10-17. The working copy currently contains an unrelated cleanup note. Preserve it; read the approved design with `git show 3f4bb52:docs/superpowers/specs/2026-10-16-appliance-signaling-interlock-design.md` until the owner resolves that edit.

## Global Constraints

- CTCSS encode and decode independently select disabled, DSP, or external. Encoder-only hardware is valid. No activity-scoped decode.
- Preserve CM119 discovery, HID, EEPROM, parallel I/O, audio levels, filters, routing, duplex behavior, and existing DCS behavior. Reuse existing DSP and controller CTCSS policy.
- Native PCM is normalized interleaved `f32` at **48,000 Hz**. RX/TX frame counts are independent, bounded at setup, and advance timers by samples.
- No fixed software port-count limit. Allocate endpoint state during control setup. One physical interlock covers only its own configured endpoints.
- Hardware capabilities are explicit. Do not invent arm feedback, mutes, watchdogs, or decoder inputs for USB/parallel devices that lack them.
- Only the GPIO context's serial non-audio owner calls transport functions. Audio publication/snapshot calls never allocate, wait, log, perform I/O, or acquire a lock.
- Creation/shutdown are disarmed. Fault recovery requires fresh readiness and an explicit new arm request; resumed watchdog activity never arms outputs.
- Keep existing **radio ABI 4 / SONAME 4** and **GPIO ABI 1 / SONAME 1** prefixes unchanged. This approved additive extension does not retain a duplicate implementation. Stop for an ABI decision if the extension cannot be safe.
- No new consumer runtime configuration or production cutover in this phase. No releases, node deployment, physical pin mapping, Hamlib work, sensors, fan control, general alarm engine, or REST/UI work.
- Developer docs and targeted tests accompany each implementation commit. Operator documentation is updated before a PR. No PR or release is implicitly authorized by executing this plan.
- Pull fresh `:latest` quality images, record digests, and use project-labeled cleanup launchers. Native Debian 13 amd64/arm64 tests; 100% production line/branch coverage on amd64 only. No automatic Debian 12 or local QEMU gate.
- Record targeted tests and covered source parts in ignored `.work/quality-progress.md`. Do not repeat full reports while closing individual gaps. Push preflight is format/lint/static analysis; full hosted gate precedes merge.

## Review Focus

1. Encoder without decoder, or decoder without encoder: validate only the selected inputs/outputs, never silently weaken RX qualification — Task 1.
2. CTCSS inhibit changes while PTT remains up: tone/enable must change independently, without adding a second hang timer — Task 2.
3. A hardware fault races fresh/stale audio intent or arm feedback: no old request may reassert after recovery — Task 4.
4. Successful I/O without readback, or failed safe-output submission: status must not falsely claim physical release — Tasks 3–4.
5. More than four ports and unrelated hardware contexts: no global port mask, callback allocation, or cross-device fault propagation — Tasks 3–5.

---

## Current code and change map

Paths below are relative to `C:/Users/ChrisPeterson/Documents/Codex/2026-08-30/`.

| Owner / files | Existing responsibility and planned change |
| --- | --- |
| `librptadvradio/src/lib.rs`, `include/rptadvradio/rptadvradio.h` | ABI-4 descriptor; append profiled operations and sized records only |
| `librptadvradio/src/session.rs` | Prepared RX/TX owners, warm-up, updates, DSP, events; reuse these for profiled sessions |
| `librptadvradio/src/profile.rs` (new) | Immutable source/capability validation and conversion to existing radio policy |
| `librptadvradio/src/transmit_signaling.rs` | Existing CTCSS/DCS/PTT timing; add external encoder selection without a second state machine |
| `rptadv-gpio-adapter/src/lib.rs`, `src/parallel.rs`, public header | Existing single service owner, atomically published intent/input; append factual capability queries without changing backends |
| `rptadv-gpio-adapter/src/context.rs` (new) | Preallocated normalized multiport context, safe-output admission, readiness/arm epochs, snapshots |
| `rptadv-gpio-adapter/src/context_tests.rs` (new, test-only) | Deterministic simulated transport, fault injection and concurrency tests |
| Both providers' `tests/descriptor_smoke.c`, `Makefile`, developer docs | C/Rust agreement, preserved descriptor prefix, dynamic installation/archive checks |
| `rpt_advanced/tests/test_radio_interlock.c` (new), `Makefile`, `.github/dependencies.json` | Hardware-free cross-DSO contract test using immutable candidate packages |
| `rpt_advanced/doc/architecture/`, `WISHLIST.md` | Accurate software-only completion evidence; retain physical qualification and unrelated site-I/O work |

Before implementation, reread each repository's `AGENTS.md`, inspect branch/remote status, and preserve existing work. Current provider checkouts are `refactor/radio-provider-abi2` and `extract/signaling-adoption`; names do not prove they are the right execution bases. The radio checkout also contains untracked Debian build outputs. Do not stage those. Reuse a suitable checkout after checking ancestry; do not reset or delete work to establish a base.

## Task 1: Profile validation and independently selected receive qualification

**Files:** radio `src/profile.rs` (new); `src/session.rs`; `src/lib.rs`; public header; `tests/descriptor_smoke.c`.

**Interfaces:** Keep existing config, ports, RX/TX input and result layouts. New records have `struct_size`, use C integer enums/flags, and contain no Rust ownership:

- `Profile`: kind (`native_audio` / `appliance_direct_codec`), COR source (`hardware` / `dsp`), CTCSS encode source and decode source (each `disabled` / `dsp` / `external`).
- `ProfileCapabilities`: normalized hardware COR, external decoder input, external encoder output, and usable DSP-tone audio route. These describe actual hardware, not user wishes.
- `ProfileReceiveInput`: existing `ReceiveInput` plus normalized external COR/CTCSS values and explicit validity. `ProfileReceiveResult`: existing `ReceiveResult` plus raw external CTCSS value/validity.
- Append `session_create_profiled(config, ports, profile, capabilities, out_session) -> result`, `session_prepare_profiled_update(session, config, ports, profile, capabilities, out_update) -> result`, and `session_receive_profiled(session, input_f32, output_f32, frame_count, controls, out_result) -> result`. Use the existing pointer types and result codes for unchanged parameters.
- Rust `ValidatedProfile::new(profile: &Profile, capabilities: &ProfileCapabilities, config: &Config) -> Result<ValidatedProfile, c_int>` owns validation. Profile/capability changes require stopped-session replacement; same-profile prepared updates reuse existing apply/destroy operations.

- [x] Add table-driven `profile::tests` for the nine encode/decode source combinations with sufficient capabilities; encoder-only external TX + DSP RX succeeds, missing selected input/output fails, and missing unused capabilities succeeds. Assert DSP decode/encode requirements independently.
- [x] Add session tests: `profiled_external_receive_qualification`, `profiled_disabled_decode_keeps_carrier_gate`, `profiled_dsp_and_dcs_receive`, `profiled_update_rejects_source_change`, and `profiled_invalid_snapshot_is_unqualified`. Assert hardware COR/CTCSS assertion and loss, raw decoder visibility when not selected, DSP COR, existing DCS selection, and unchanged state after rejected updates. Reject unsupported simultaneous CTCSS/DCS requirements explicitly.
- [x] Run `cargo test --locked profile` in the radio quality container; record the expected missing-profile/operation failures.
- [x] Implement validation and the additive operations by sharing existing `session::create`, RX worker preparation and receive qualification. Do not copy a receive pipeline. Invalid required snapshots close qualification; they never select another source. Leave existing unsized results unchanged.
- [x] Run the new tests plus `cargo test --locked session::tests` and `make test`. Compare existing native output/qualification for identical PCM; verify short descriptor/record rejection and unchanged old descriptor offsets in the C smoke test.
- [x] Update Rustdoc/header ownership and lifetime comments; record targeted results; commit only this deliverable.

## Task 2: External encoder intent from the existing transmit state machine

**Files:** radio `src/transmit_signaling.rs`, `src/session.rs`, `src/profile.rs`, `src/lib.rs`, public header, descriptor smoke.

**Interfaces:** `ProfileTransmitResult` is sized and contains the existing `TransmitResult` plus `PreparedOutputs { logical_ptt: u32, external_ctcss_enable: u32 }`. Append `session_transmit_profiled(session, output_f32, frame_count, controls: *const TransmitInput, result: *mut ProfileTransmitResult) -> result`. Return actions and PCM from the **same** call; no later query of mutable TX state.

- [x] Add `profiled_external_encode_has_no_tone_pcm`: silent program, external encode selected, native tone configuration absent, PTT requested; assert external enable is true, logical PTT true, and both PCM channels have zero native CTCSS. Disabled encode yields neither tone nor enable; DSP encode preserves configured waveform/level/routing.
- [x] Add `profiled_encode_follows_live_inhibit`: key with enable, inhibit while keeping PTT, then re-enable without a PTT edge. Assert both DSP and external paths obey inhibit, independently of PTT. Existing `select_start_ctcss` depends on a nonzero native frequency; external encode must not. Verify current active-PTT inhibit handling before changing it.
- [x] Add `profiled_encode_release_and_partitioning`: normal release and tone removal before PTT release; split the same stream into 960, 480 and irregular valid counts. Assert equivalent elapsed state and no enable without effective PTT. Reject external phase/tail-tone and native frequency/deviation/forced-tone controls; preserve DCS turn-off behavior.
- [x] Run `cargo test --locked profiled_` and `cargo test --locked transmit_signaling::tests`; capture red failures before implementation.
- [x] Extend the existing signaling object with its selected encoder source; reuse its current timers and private render spans. Final prepared actions publish at the call boundary, as ADR 0027 permits. Do not add public sample-offset action arrays, a second oscillator, or a second timer. On a valid-output error path return released actions and RF-safe output.
- [x] Run the same tests and `make test`; add invalid/unwarmed/oversized/provider-failure cases for safe prepared actions without out-of-bounds writes. Preserve existing native tests, and document any required live-inhibit correction as part of the approved encode policy.
- [x] Update developer docs and commit the tested TX deliverable.

## Task 3: Hardware capabilities and a preallocated normalized context

**Files:** GPIO `src/lib.rs`, `src/parallel.rs`, new `src/context.rs` and test-only `src/context_tests.rs`, public header, descriptor smoke.

**Interfaces:** Append after `parallel_clear_rtx_transmit`; preserve the existing ABI-1 prefix. New sized records:

- `PortConfig { port_id: u64, capabilities, required_signals }`; port IDs are unique within a context, not bit positions. Arrays have pointer/count contracts and are copied during setup.
- `PortCapabilities`: actual normalized input/output signals, pin directions, available output readback and hardware safety facilities. Raw parallel pins do not imply normalized COR/CTCSS until explicitly assigned.
- `ContextConfig`: configured ports, selected safety contract (`ordinary` / `appliance_interlock`), transport-provided arm timeout and maximum snapshot age. Reject an appliance contract missing required facilities. These are backend setup values, not invented physical timings or new operator settings.
- `Transport`: borrowed context plus control-only `sample(context, now_ns, safety_out, ports_out, port_count) -> i32`, `request_arm(context, epoch) -> i32`, `apply_outputs(context, ports, port_count) -> i32`, and `safe_outputs(context) -> i32`. `safe_outputs` requests disarm, PTT/encoder release, both TX-leg mutes, and covered site-output disable. Each sample reports validity/age, current arm-feedback epoch, required health and optional confirmed outputs.
- Append `device_capabilities(device, out_caps)`, `parallel_capabilities(device, out_caps)`, `context_create(config, transport, out_context)`, `context_get_port(context, port_id, out_port)`, `context_snapshot(context, out_safety)`, `port_snapshot(port, out_status)` and `context_destroy(context)`. Results use existing status codes or documented new enum values without changing their old meanings.

- [x] Add `context_capabilities_are_hardware_specific` and `context_preallocates_configured_ports`: ordinary CM119/parallel subsets work without arm/mute feedback; appliance selection without required safety signals fails. Create **seven** ports, including an encoder-only port, and a separate ordinary context. Duplicate IDs, unavailable required endpoints, invalid sizes and setup allocation failure are safe errors. Unconfigured empty slots are not faults.
- [x] Run `cargo test --locked context_` and record red failures.
- [x] Implement factual capability queries and setup-owned `Vec`/boxed port storage using fallible allocation. Reuse existing device facts; do not change HID/parallel writes or invent URIXB pin mappings. Store control-only transport state separately from atomic callback-visible state. Creation submits safe outputs; failure returns no usable context and reports the failure. Ordinary contexts admit prepared intent only after initialization/readiness and valid required inputs; they neither call an unsupported arm operation nor report physical arm confirmation. Their existing device entry points remain unchanged.
- [x] Add requested-versus-confirmed snapshots. Test that successful writes without readback remain **confirmation unavailable**, not physically confirmed. No observation from another context may influence these values.
- [x] Run `cargo test --locked context_` and existing CM119/parallel tests with their test transports, then `make test`. Verify the previous descriptor member offsets, including the 208-byte prefix on supported 64-bit targets, remain unchanged.
- [x] Update header/Rustdoc ownership, pointer lifetimes and capability descriptions; commit.

## Task 4: Explicit arming, latched faults and stale-intent rejection

**Files:** GPIO `src/context.rs`, `src/context_tests.rs`, `src/lib.rs`, public header, descriptor smoke.

**Interfaces:** Control owns `context_initialize(context, now_ns)`, `port_ready(port, epoch)`, `context_request_arm(context, epoch, now_ns)`, `context_disarm(context)`, `context_recover(context, now_ns)`, `context_service(context, now_ns)`, and `port_retire(context, port_id, epoch)`. Audio owns only `port_publish(port, action)` and bounded snapshot reads. `OutputIntent` contains one caller-observed epoch plus PTT, external CTCSS-enable, mute and allowed site-output intent; valid combinations are checked before admission. `SafetySnapshot` exposes state, epoch, health, latched reason, arm request/feedback, and transport/safe-submission errors. `PortSnapshot` exposes present/ready/retired, requested/confirmed outputs and readback validity.

- [x] Add a data-driven `context_arm_and_fault_matrix`: appliance cold start is Disarmed; valid initialization plus current-epoch ready for **every configured port** permits an explicit request; state remains Arming until fresh matching feedback; then Armed. Missing/stale ready, invalid health, failed arm submission, stale feedback, timeout and a fault during Arming never admit outputs. Reject snapshots older than the backend's maximum age, timestamps in the future, and backward service time without inventing a physical deadline.
- [x] Add `context_recovery_never_replays_intent`: fault/disarm/retirement invalidate requests, readiness and pending actions before transport calls. Health restoration/watchdog pulses alone stay unarmed. Explicit recovery, fresh readiness and a new arm request are required, followed by fresh output intent. A pre-arm asserted intent must not become active merely because feedback later arms the context.
- [x] Add `context_safety_fault_releases_all_covered_outputs`: inject each required module/power/reset/watchdog/transport failure while all seven ports are keyed. Assert requested safe state for every covered PTT, external encoder, TX-A/TX-B mute and site output; the unrelated context stays operational. Failed safe submission remains visible and never claims confirmed release.
- [x] Add `context_stale_epoch_races` and `context_shutdown_quiesces`: deterministically interleave fault with publish, ready and acknowledgment. Assert no torn epoch/action pair, no old assertion after rearm, and no freeing of live port handles. Healthy unchanged-session reload is not an arm operation; profile/hardware replacement disarms and requires new readiness. Near-maximum epoch faults instead of wrapping.
- [x] Run `cargo test --locked context_` before implementation to capture failures.
- [x] Implement the appliance's four states (Disarmed/Arming/Armed/Faulted) in the sole service owner. Pack each port's epoch and bounded output flags in one `AtomicU64`; checked epoch advance must fail closed, never reuse a token. Publish context state/epoch/admission coherently, not as independently torn safety fields. Atomically revoke admission before safe-output I/O. Service validates epoch and current admission again before accepting a publication. Issue a fresh output-admission epoch on confirmed arm so pre-arm intents cannot become eligible; retain readiness only for the successfully validated arm transaction. Snapshot readers use bounded atomic loads, not retry-until-stable spin loops. Keep radio readiness and arm requests generation-specific.
- [x] Add test allocation guards around warmed publication/snapshot calls and a stalled-service test. The fake physical interlock must independently remove outputs when watchdog validity is lost, even when software service is not running. This proves simulation behavior only, not real hardware safety. Ordinary contexts retain their declared safe-output contract and do not emulate a hardware arm latch.
- [x] Run new tests plus `make test`. Document that port retirement/destruction is control-only after audio-owner quiescence and that callbacks/context/DSO must outlive all users. Commit.

## Task 5: Cross-DSO simulated radio/interlock proof

**Files:** controller `tests/test_radio_interlock.c` (new), `Makefile`, `.github/dependencies.json`. Provider smoke tests as needed. Reuse the existing `test_jitter_ring.c` pattern; do not add production C or change standalone device selection.

**Interfaces:** The test consumes the new radio/GPIO tails using their public headers. New target `make radio-interlock-check` builds/runs only this test against dynamic `librptadvradio.so.4` and `librptadv_gpio_adapter.so.1`. The fixture supplies normalized `Transport` callbacks and borrowed radio processor/program ports. It owns no DSP/signaling replacement.

- [x] Add the composed test first: create/warm real profiled radio sessions, initialize seven simulated endpoints, confirm radio-ready only after warm-up, arm explicitly, then drive exact RX/TX frame counts. Pass normalized GPIO input into radio; publish the same-call radio action into GPIO; service it off audio.
- [x] Assert external encode + DSP decode, DSP encode + external decode, disabled directions and native DCS; existing controller-supplied `ctcss_inhibit` qualifies either encoder. Verify no native CTCSS PCM for external encode, decoder loss closes required qualification, and missing unused capabilities do not fail startup.
- [x] Assert fault while keyed, stale action/feedback rejection, explicit recovery, safe shutdown, no automatic rearm on unchanged reload, and unaffected unrelated context. The simulated independent gate—not a radio callback—must remove outputs during service stall.
- [x] Run against the existing installed headers/providers and record expected missing-tail failure. Build candidate Debian packages from the two provider revisions with `make debian-package-check`; place only these outputs in ignored `.work/adr0033-0034/packages/`. Install inside the disposable controller quality container, not Windows or a node. Verify package architecture and actual DSO paths before rerunning `make radio-interlock-check`.
- [x] Add the test to `check`, C formatting/static checks and `DIST_FILES`. Use the existing immutable candidate-dependency builder and add the **actual committed provider SHAs** plus runtime/dev package names to `.github/dependencies.json`. Its existing schema already allows both repositories, so no workflow or allowlist expansion is needed. Do not use moving refs, placeholders, vendored source, static archives, or a new release-download path.
- [x] Use `readelf -d` to prove the test uses the two versioned DSOs and providers retain their existing dynamic DSP/signaling dependencies. The new test must fail if required descriptor tails are missing; no silent skip. Do not raise released runtime dependency minima before provider releases exist.
- [x] Run the composed test and affected manifest/packaging checks, update test documentation, and commit.

## Task 6: Quality evidence and truthful completion status

**Files:** affected provider `Makefile`, `QUALITY.md`, Rustdoc/header docs and package-test metadata; controller architecture map, ADRs 0033/0034 and `WISHLIST.md`; operator-facing documentation immediately before an authorized PR.

- [x] Run affected unit tests/coverage and Rustdoc/header checks per source part. Turn missing lines/branches into the ignored checklist; close each with targeted tests or remove unreachable code. Test modules must remain excluded from production coverage, not production error paths.
- [x] Run provider `make lint static-analysis`, warning-free Rustdoc/header documentation, `make test`, `make install-check`, `make debian-package-check`, and `make distcheck` at the final verification stage. Use existing source-owned derived quality images and labeled launchers; pull the maintained base before building them. Archive checks must include all new nested/test files.
- [ ] Validate the composed target in both native Debian 13 architectures through the existing PR workflow; require 100% production line/branch coverage on amd64. Run Cppcheck and other independent checks once, not per platform. Record actual job results; targeted success is not full-gate success.
- [x] Update ADRs 0033/0034 to **software contract implemented** only when the required tests pass. Keep real GPIO wiring, physical interlock and post-DAC mute qualification, appliance bring-up, sensors/fan/alarm functions and broader site-I/O work explicitly pending. Do not mark the whole site-I/O wishlist entry complete.
- [x] Reconcile architecture/wishlist wording and relevant unchanged hardware-contract references. Do not edit the dirty hardware repository or the unrelated spec replacement without permission. Confirm no installed service can select the simulated backend.
- [x] Commit developer docs/status with their evidence. Before any later PR, update concise operator docs only where a shipped interface changed; do not claim the physical appliance can now run. Report revisions, tests and remaining physical qualification. No node deployment or release in this plan.

## Validation commands and execution order

Provider targeted commands above run **inside** the native quality container, not Windows Cargo. From each provider checkout, reuse `make quality-image` and then `RPTADV_CONTAINER_PULL=0 sh tools/run-in-quality-container.sh <locally-built-quality-image> <targeted-command>`; the first target pulls the maintained base, the second uses that just-built image and records its identity. Record the base digest too. Never reuse a stale image merely because its tag exists.

For the composed test, use `rpt_advanced-workflows/tools/run-in-quality-container.sh IMAGE WORKSPACE COMMAND ...`, which pulls the maintained image and cleans labeled containers. Install the candidate runtime/dev packages into that disposable environment and run `make radio-interlock-check` there. No test opens a live USB device, keys a radio, or connects a network peer.

Tasks 1–2 and 3–4 can proceed independently after the interface shapes are reviewed. Task 5 joins their **committed** provider contracts; Task 6 follows focused verification. Each task includes tests and developer documentation; do not queue those until the end.

## Plan self-review

- Spec coverage: independent sources and unchanged DSP (1–2); capability-limited hardware and unbounded configured ports (3); readiness/arm/fault/epoch/lifecycle (4); simulated composition and DSO compatibility (5); packaging, evidence and software-only status (6).
- All five review-focus cases have named tests above. Hardware deadlines come from transport setup, not arbitrary new timing defaults.
- Existing unsized result records and descriptor prefixes remain intact; new tails are admitted by size before dereference. Old and new operations share implementation.
- Scope excludes runtime configuration selection, hardware work, releases and installation. Physical fail-safe claims remain blocked on physical qualification.
- Native inline execution approved and software implementation complete. Provider quality, coverage, package, archive, and dynamic composition checks pass locally on Debian 13 amd64. Hosted arm64/full PR validation awaits authorization to push; no node deployment or release occurred.

## Local verification record

- Radio: 194 Rust tests; 27 production source files at 100% line/branch coverage.
- GPIO: 88 Rust tests; five production source files at 100% line/branch coverage; every health input fails closed both while armed and while arming.
- Both providers: strict lint/static analysis, Rustdoc/Doxygen, staged install, Debian package, and unpacked source-archive checks pass.
- Controller: seven-port composed test uses the installed radio/GPIO DSOs; boundary checks, C formatting/static analysis, and source-archive inclusion pass.
- Independent whole-branch review: fault-observation invalidation, fallible setup allocation, DCS routing validation, and GPIO-to-radio fixture findings resolved and tested. No unresolved findings.
- Local source revisions are pinned in `.github/dependencies.json`. Detailed logs and coverage checklists remain in ignored `.work/` directories, not in Git.
- Physical pin mapping, hardware qualification, consumer cutover, operator documentation before PR, and the hosted matrix remain pending.
