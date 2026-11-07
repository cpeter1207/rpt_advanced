# Shared Hamlib Remote Base Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans for native execution, or superpowers:subagent-driven-development if selected by the user. Execute task-by-task with focused TDD and recovery commits.

**Goal:** Supply shared Hamlib and parallel-port remote-base control, configuration-only memories, scanning, and complete operational DTMF access in standalone and USBRadioPlus.

**Architecture:** One versioned remote-base library owns portable state, limits, tune transactions, memories, and scanning. The existing per-radio hardware owner serializes physical calls through the Hamlib adapter or existing parallel GPIO operations. Both hosts use the shared USBRadioPlus product; neither implements another rig controller.

**Tech Stack:** Rust 2024, sized C descriptor ABIs, dynamic libhamlib, existing GPIO/audio/radio providers, existing crossbeam-queue/rtrb patterns, native Debian 13 packages and quality images.

**Spec:** [Approved design](../specs/2026-11-04-hamlib-remote-base-design.md), approved 2026-11-04. Read that design and applicable AGENTS/ADRs before execution.

## Global constraints

- Reuse the existing USBRadioPlus and `rpt-codecs-text` checkouts. Do not create additional worktrees or reset away work. Begin implementation on a clean feature branch based on verified current main, retaining these approved documents.
- One rig per node; no fixed software rig/port/memory limit. Size state from configuration and resources.
- Native PCM remains 48 kHz normalized F32. Do not add DSP graphs, resamplers, protocol adapters, REST listeners, or a speech detector in this work.
- Memories and scan lists are configuration-only. No save/delete commands, persistent live overrides, or proprietary radio-memory synchronization.
- Dwell is 500 ms after tune/settle; resume delay is 2,000 ms after activity ends; priority scanning defaults off. Stop retains the current member; restart selects startup memory, not a live scan.
- The default remote-base prefix is `*6`, remappable per node. Existing link commands and `*722` retain their behavior.
- Actual RF PTT must be off before tuning. Explicit tuning may interrupt receive after its command source unkeys; automatic scanning holds for receive or linked activity. Non-FM linked transmit must not wait for unreliable/open receive squelch.
- Scan TX uses only a fully applied, permitted current member. Do not retune to a home memory or add an audio queue. Inspection found no pre-key retune allowance: add zero buffering/grace. If demand begins during an in-flight scan tune, reject that burst until its sources unkey; count it and never replay its old audio later. Detect the overlap with the existing intent publication's rising-edge epoch, not a new timer.
- Rig and GPIO I/O stay on the existing non-audio physical owner. Audio callbacks use existing atomics/preallocated handoffs only. Other rigs and controller executors must not wait on a blocked rig.
- Preserve external ASL `SETFREQ`/`SETCHAN` behavior; move existing implementations and remove replaced ownership/types. Parsed-but-unimplemented RTX tone/power fields are not hardware capabilities.
- Keep every project boundary dynamic. Project alpha compatibility follows ADR 0040; update all affected consumers and artifact/ABI checks together without compatibility-only paths.
- No node install, restart, RF operation, link change, repository publication, merge, or release is authorized by this plan. Obtain those separately at the appropriate concrete handoff.
- Run focused tests for each change and record evidence in ignored `.work/quality-progress.md`. Fast formatting/lint/static checks precede push; GitHub's full native PR gate precedes merge. Coverage is production-only, Debian 13 amd64; Debian 12 remains manual-only.
- Use current `latest` quality images with verified digests and labeled cleanup. If local Docker is unavailable, use authorized focused hosted tests; do not change the live node to obtain a test environment.

## Review focus

1. A linked key arriving during a scan tune must never key on a partial or wrong state (Tasks 3, 6, 8).
2. Open non-FM receive must not deadlock intentional tune or linked PTT (Tasks 3, 8).
3. Off-thread completion, reload, or front-panel changes must not bypass limits or revive a retired rig (Tasks 3, 4, 8).
4. DTMF argument separators, fractional values, and authorization must not change existing commands or acknowledge rejected changes as successful (Task 7).
5. Parallel write-only hardware and unavailable Hamlib readback must report uncertainty honestly while preserving existing signal ownership (Tasks 2, 4, 8).

## Files and ownership

Labels below are independent repositories, not workspace path dependencies:

- **REMOTE**, proposed `librptadvremote`: `src/{lib,config,capability,limits,session,scan,ffi}.rs`, `include/rptadvremote/rptadvremote.h`, `tests/descriptor_smoke.c`, and ordinary packaging/workflow callers. This is the portable operating engine; no Hamlib/Asterisk/GPIO device access.
- **HAMLIB**, proposed `rptadv-hamlib-adapter`: `src/{lib,ffi,model,catalog,rig}.rs`, `include/rptadv_hamlib_adapter/rptadv_hamlib_adapter.h`, `build.rs`, `tests/descriptor_smoke.c`, and ordinary packaging/workflow callers. Only this component imports Hamlib.
- **USB**: `rust/core/src/{station_config,config_document,channel_configuration,native_configuration}.rs`; `rust/asl3/src/control.rs`; `rust/driver/src/hardware_host.rs`; `rust/station/src/{lib,hardware}.rs`; `rust/product/src/{lib,native,native_configuration,maintenance}.rs`; `rust/product/include/usbradioplus_product.h`. Add narrowly scoped `rust/driver/src/remote_base.rs` and a thin `rust/remote` dynamic client rather than enlarge the main driver file with another implementation.
- **RPT**: `rust/core/src/command.rs`, `runtime/{dtmf,aggregate}.rs`, `config/{schema,settings}.rs`; `rust/product/src/{radio_control,services}.rs` and its ABI mirror; `rust/standalone/src/{providers,native_configuration,radio_activation,radio_host,local_radio}.rs`; current Asterisk host bridge, message bundles, install/package tests, and applicable architecture/wishlist records.
- **Workflows**: existing shared-library workflow templates and USB/controller workflow repositories. Keep production repositories' workflow files as thin callers.

## Contract decisions

Proposed independent SONAMEs/packages are `librptadvremote.so.1` (`librptadvremote1`, `librptadvremote-dev`) and `librptadv_hamlib_adapter.so.1` (`librptadv-hamlib-adapter1`, `librptadv-hamlib-adapter-dev`). Only the Hamlib adapter depends on libhamlib. Disabled or parallel-only rigs do not require loading that selected adapter.

The portable Rust API is owned by REMOTE:

```rust
RemoteBase::new(config: RemoteBaseConfig, capabilities: Capabilities) -> Result<RemoteBase, Error>
RemoteBase::request(&mut self, request: RigRequest, now_ms: u64) -> Result<RequestId, Error>
RemoteBase::advance(&mut self, now_ms: u64, activity: Activity, backend: &mut dyn RigBackend) -> Result<(), Error>
RemoteBase::snapshot(&self) -> RigSnapshot
```

`RigRequest` has typed `Set(OperatingPatch)`, `Recall(MemorySelector)`,
`Scan(ScanRequest)`, `Inhibit(bool)`, `Tune`, and `Query(Query)` variants.
`Activity` copies qualified local/linked reception, command-source completion,
physical PTT, and generation identity. `RigSnapshot` distinguishes requested,
applied, verified, unknown, inhibited, pending, and failed state with the first
cause. `RigBackend` supplies `capabilities`, `read_state`, `apply_step`, and
`set_ptt`; all execute only on the physical owner. Exact foreign records use
fixed-width fields and sized spans; Rust enums/traits do not cross a DSO.

The REMOTE C descriptor exposes `create`, `destroy`, `request`, `advance`,
`snapshot`, `catalog`, and completion drain. Backend operations cross a sized
C callback table containing opaque context, read/apply/PTT operations, and
explicit statuses. Calls into one context are serialized by the physical
owner; the library does not start another device thread or own sockets.

HAMLIB exports `rptadv_hamlib_api_v1()` with `models`, `open`, `close`,
`capabilities`, `read_state`, `apply_step`, and `set_ptt`. Its context contains
one `RIG*`, only usable by that rig's owner. Backend type/data never escape.

USB appends typed `remote_submit`, `remote_status`, `remote_catalog`, and
`remote_completion` operations to its sized product descriptor. Submission is
nonblocking and returns a request ID or explicit full/unsupported status;
results are drained on control, not waited for by a controller executor.
Do not overload `UrpAstChannelCommand`'s mixer/maintenance fields. Existing
descriptor ABI is 1; native configuration ABI is 3. Prefer complete optional
tails; change native configuration ABI only if its actual layout must change.
Record the exact resulting ABI/SONAME/package minimums before changing both
consumer admission tables and image checks in Task 8.

## Task 1: Portable remote-base model and installed contract

**Files:** REMOTE files above; use existing shared-library packaging/gate templates. No external publication yet.

**Interfaces:** Produce `RemoteBaseConfig`, `Capabilities`, `OperatingState`, `OperatingPatch`, `RigRequest`, `RigSnapshot`, `RigBackend`, and the descriptor above. Configuration carries named memories/lists, stable positive selectors, integer-Hz ranges, modes, power constraints, and explicit signaling sources.

- [ ] Write `remote_base_rejects_unsafe_complete_state` as a table test: effective RX/TX includes offset/RIT/XIT; out-of-policy RX/TX, unsupported mode, duplicate selectors, missing member, missing required power enforcement, and conflicting signal sources are errors; the valid complete state passes.
- [ ] Run `cargo test --locked -p rptadv-remote remote_base_rejects_unsafe_complete_state`; record RED, then implement the minimal owned types, common validator, and fixed-width descriptor translation. Do not implement another INI parser.
- [ ] Run the named test and `descriptor_smoke`; require round-trip opaque lifecycle, exact size/version rejection, no Rust layout/panic crossing, and no Hamlib/Asterisk ELF dependency. Follow existing package templates rather than copying their unrelated capabilities.
- [ ] Add concise Rustdoc/header documentation, run affected fast checks, record evidence, and commit `feat: add shared remote-base operating contract`.

## Task 2: Parallel radio operations through one shared owner

**Files:** USB `control.rs`, `hardware_host.rs`, new driver `remote_base.rs`, thin remote client; existing GPIO adapter calls remain unchanged.

**Interfaces:** Consume Task 1; `SETCHAN` becomes typed channel selection, `SETFREQ` becomes a complete operating request. Preserve `HardwareOutputs::{select_channel,configure_remote_radio,service_remote_radio}` and existing pin reservation/service ordering.

- [ ] Add `remote_parallel_matches_existing_asl_frames` in driver hardware tests and `remote_parallel_is_available_to_native` in product tests. Assert identical active-low bits 4–7, existing two 20-bit MSB-first RTX words, PTT clearing, reservations, and ordering; native and ASL requests have equal results.
- [ ] Run `cargo test --locked -p usbradioplus-driver remote_parallel` and the focused product counterpart; record RED.
- [ ] Move ASL-owned reusable request types into the neutral boundary and route both ingress paths to existing implementations. Replace only the relevant owned control handoff with the project's existing `crossbeam_queue::ArrayQueue` pattern where a new remote request would otherwise take the current `SyncSender` lock; retain bounded explicit admission and do not rewrite unrelated EEPROM/calibration protocols.
- [ ] Run focused RED/GREEN plus `cargo test --locked -p usbradioplus-asl3 remote`; assert parsed RTX tone/H/L fields do not become advertised writable capabilities without a real implementation. Full queues and pin conflicts report failure, not success.
- [ ] Update developer docs, record the parity rows and tests, and commit `refactor: share parallel remote radio control`.

## Task 3: Tune transactions, admission, and generations

**Files:** REMOTE `limits.rs`, `session.rs`, tests; USB owner integration.

**Interfaces:** Implement `request/advance/snapshot`, ordered apply steps, completion IDs, and transmit admission. The physical owner alone invokes `RigBackend`.

- [ ] Write `remote_tune_waits_for_ptt_but_can_preempt_open_receive`, `remote_partial_apply_retains_inhibit`, and `remote_retired_generation_cannot_apply`; use a fake backend and monotonic clock. Assert explicit command-source completion, actual-PTT exclusion, coherent commit, first-error retention, safe restore, and write-only uncertainty.
- [ ] Run `cargo test --locked -p rptadv-remote remote_`; record RED, then implement validate/inhibit/apply/readback/commit using one shared failure path. A failed restore leaves TX inhibited; readback is required only for capabilities the backend supplies.
- [ ] Add `remote_stalled_rig_does_not_block_other_rig` in USB owner tests, including PTT-off priority and no controller-thread synchronous wait. Derive and expose a finite service deadline from configured backend timeout/retry values; synchronous calls are not interruptible.
- [ ] Run both focused suites. Assert front-panel out-of-policy RX closes receive, unsafe/unknown TX inhibits PTT, and queued work cannot revive a stopped generation. Review callback call sites for new I/O, locks, allocation, or waits.
- [ ] Update developer docs/evidence and commit `feat: enforce shared remote-base tune admission`.

## Task 4: Hamlib dependency adapter

**Files:** HAMLIB files above; USB backend composition and selected adapter manifest.

**Interfaces:** Implement `rptadv_hamlib_api_v1()` and the `RigBackend` bridge. Resolve canonical `RIG_MODEL_*` names using a generated table from installed headers; no numeric model configuration.

- [ ] Write `hamlib_dummy_supports_typed_roundtrip`, `hamlib_rejects_unsupported_capability`, and `hamlib_transport_error_preserves_first_cause`. Use Hamlib dummy plus controlled loopback/serial fixtures, never an external rig.
- [ ] Run `cargo test --locked -p rptadv-hamlib-adapter hamlib_`; record RED. Implement dynamic C API calls for frequency/mode/bandwidth/VFO/split/offset/RIT/XIT, PTT/DCD, CTCSS/DCS and supported typed levels/functions; use capabilities and actual operation errors together.
- [ ] Generate stable adapter-neutral setting descriptors: selector, symbolic name, value kind, units, access, range/enum, and readback support. Tone value and enable are distinct. Do not expose raw CAT or a Hamlib pointer escape hatch. Advertise no operating capability without a corresponding typed request and test.
- [ ] Verify model catalog, serial/network/USB configuration pass-through, finite timeout/retries, unknown values, write-only controls, and close/unkey failures. Assert Hamlib is the only component importing Hamlib types; adapter absence fails an enabled selected composition before acquiring hardware.
- [ ] Run adapter tests, installed C smoke and package/ELF checks; document LGPL/GPL-compatible linkage, record evidence, and commit `feat: add Hamlib radio-control adapter`.

## Task 5: Shared configuration and memory recall

**Files:** USB common configuration files/tests, station builders, product native resolver; REMOTE config/session; RPT only passes source sections through its existing shared native resolution boundary.

**Interfaces:** Add inheritable `[remote_base]`, `[remote_base node]`, `[remote_memory node name]`, and `[remote_scan node name]`. Both hosts resolve the same `RemoteBaseConfig`. Preserve disabled defaults and existing radio settings.

- [ ] Write `remote_configuration_is_identical_across_hosts` with global/per-node inheritance, symbolic model names, stable selectors, all signal-source combinations, invalid/missing references, and unchanged non-remote defaults.
- [ ] Run focused `usbradioplus-core remote_configuration` tests; record RED. Implement one resolver using existing warning/default behavior. If no safe complete configuration exists, retain the old generation on reload and leave an unsafe new radio inactive.
- [ ] Write/run RED/GREEN for `remote_memory_recall_is_one_transaction` and `remote_memory_definitions_are_configuration_only`. Assert startup selects the configured memory; live tuning never writes configuration/state files; reload changes definitions transactionally; ordinary named recall respects all limits.
- [ ] Run focused native/channel configuration and station-builder tests. Assert independent encode/decode sources, selected audio path, no duplicate owners, and no hardcoded gains or power values.
- [ ] Document the schema in source, record evidence, and commit `feat: add shared remote-base configuration and memories`.

## Task 6: Named scan lists

**Files:** REMOTE `scan.rs`, `session.rs`, tests; USB per-rig service scheduling.

**Interfaces:** `ScanRequest::{Start(selector),Pause,Resume,Stop,Direction(direction),Skip,SetDwell(ms),SetResumeDelay(ms),SetPriority(optional_selector)}`. Changes other than list definitions are live only.

- [ ] Write `remote_scan_uses_completed_tune_deadlines`: at settle+499 ms retain member; at settle+500 ms advance. Activity holds the member; at unkey+1,999 ms hold; at +2,000 ms resume. No priority excursion during a transmission.
- [ ] Write `remote_scan_tx_uses_only_current_permitted_member`: pause before TX; never key a partial/receive-only member; a transmit edge during an in-flight tune rejects that burst until unkey and increments its counter; zero new buffering/grace and no stale audio replay. Test the owner/intent overlap with a blocked fake backend and explicit intent epochs in Task8.
- [ ] Run `cargo test --locked -p rptadv-remote remote_scan`; record RED, then implement monotonic owner-driven sequencing. A failing member is reported/skipped; all failing stops scanning safely. Open/unqualified receive cannot pretend to be usable automatic activity detection.
- [ ] Extend focused tests for direction/skip/priority, pause/resume, stop retaining current member, explicit recall stopping scan, failed tunes, reload retirement, and restart restoring startup memory rather than resuming a scan.
- [ ] Run the focused suite, update developer docs/evidence, and commit `feat: add shared remote-base scan lists`.

## Task 7: Complete DTMF operating surface

**Files:** RPT command/collector/runtime/config files above, new focused `rust/core/src/remote_base_command.rs`, controller dispatch/completion handling, shipped message bundles; USB ASL translation remains thin.

**Interfaces:** Produce `RemoteCommand` plus typed `RigRequest` conversion. Do not put rig arguments in `Command.node`. Generalize existing administrative helpers without changing their success-only timeout renewal. Keep ordinary `*4` addressed forwarding/source identity intact.

### Exact argument rules

- `#` or real source unkey completes a remote request; ordinary timeout remains 3 seconds. The existing total bound remains 127 symbols. Internal `*` separates arguments only after recognizing the remote prefix. Outside it, existing reset semantics remain unchanged.
- `*61<MHz>*<fraction>*<offset>#` uses 1–5 fractional digits, right-padded to five (10 Hz resolution); `offset` is 1 minus, 2 simplex, 3 plus, using the configured offset magnitude. Exact-Hz tuning uses the generic setting operation below.
- `*62<whole>*<tenth>#` and `*63<whole>*<tenth>#` encode CTCSS in tenths of Hz; the single fractional digit is mandatory. Validate against the selected DSP/rig capability. Do not reverse the actual ASL RX/TX meaning to match its erroneous comments.
- Signed fixed-point values use explicit sign `0` nonnegative / `1` negative, unsigned integer `digits`, and decimal `places` 0–6. A value is `sign * digits / 10^places`; enum/bool selectors require sign 0 and places 0. Numeric fields use checked parsing; reject overflow and unsupported precision without changing state.
- Selectors are positive decimal values; zero is reserved only where explicitly noted. Query pages start at 1 and contain at most five entries. Presentation names are localized; stable selectors are not Hamlib flags.

### Extended commands (in addition to the approved familiar-ASL table)

| Command | Typed operation |
| --- | --- |
| `*690<setting>#` | Query a declared setting/capability. |
| `*691<setting>*<sign>*<digits>*<places>#` | Set a typed scalar/enum operating value. |
| `*692<list>#` | Start a named configured scan list. |
| `*693#`, `*694#`, `*695#` | Pause, resume, stop scanning. |
| `*696<0-or-1>#` | Scan direction, 0 down / 1 up. |
| `*697#` | Skip current member for this live scan. |
| `*698<memory>#` | Query one configured memory. |
| `*6990<page>#` | Query the capability catalog. |
| `*6991<page>#`, `*6992<page>#` | Query memory/scan-list catalogs. |
| `*6993<list>#` | Query scan-list configuration/state. |
| `*6994<0-or-1>#` | Clear/set administrative transmit inhibit; never override a safety inhibit. |
| `*6995<ms>#`, `*6996<ms>#` | Live dwell and resume-delay changes. |
| `*6997<memory-or-0>#` | Live priority member; 0 disables. Member must belong to the list. |
| `*6998#` | Current complete rig/scan status. |
| `*6999<action>#` | Invoke a declared argument-free operating action. |

Task 4's published catalog covers every supported operating setting and action,
including VFO, exact RX/TX Hz, split/offset, RIT/XIT, mode/passband, power,
tones/sources, levels, preamp/attenuator, antenna, and tuner. Define its stable
selector table alongside the descriptor before implementing these bindings;
every advertised entry must round-trip through query/set or invoke. Complex
changes still enter one validated `OperatingPatch`, not a raw Hamlib command.
Low/medium/high and slow/medium/fast shortcuts select configured presets;
unsupported/unconfigured presets return a localized error. Scan shortcuts act
on the selected list and fail explicitly if none is selected. The initial
implementation's `tune_duration_ms` is inherited, defaults to 1,000 ms, and
accepts 1 through 5,000 ms; the tune also stops on source/session loss or any
safety inhibition. This duration bounds manual tune, not ordinary PTT, and
does not impose an audio or RF level.

- [ ] Add `remote_dtmf_preserves_existing_commands`, `remote_dtmf_roundtrips_catalog`, and `remote_dtmf_enforces_source_authorization` table tests covering every approved and extended command, signed/fractional boundaries, separators, remapping/collisions, overflow, `#`/unkey/timeout, and partial input.
- [ ] Run `cargo test --locked -p rpt-advanced-core remote_dtmf`; record RED. Implement the typed parser/dispatch and open/locked/disabled policy through existing grants, preserving missing-code behavior and successful-completion-only renewal.
- [ ] Add/run `remote_reply_waits_for_origin_and_stays_source_scoped` with two peers and local RF. Apply-completion success/error replies use existing speech/Morse policy and shipped i18n strings; CLI replies remain on their interface. No duplicate app_rpt telemetry.
- [ ] Assert every catalog operation has a DTMF representation and config definition changes have none. Run focused controller tests and update source docs/evidence.
- [ ] Commit `feat: add full remote-base DTMF operation and status`.

## Task 8: Host parity, lifecycle, and ABI/package admission

**Files:** USB product/header/native/station/tuner/driver files, RPT product host services/mirrors/standalone/Asterisk bridge, existing package and surface tests, dependency manifests and workflow image ABI checks.

**Interfaces:** Complete the typed asynchronous descriptor tail from the contract section. RPT submits/drains through its existing leased radio host. Native and Asterisk request/status objects have the same meaning; no new device handle is opened.

- [ ] Write/run RED for `remote_host_paths_share_one_owner`, `remote_reload_retains_working_generation_on_failure`, and `remote_descriptor_rejects_incomplete_tail`. Test partial/exact sizes, disabled backends, wrong versions, missing required provider operations, and proper minimum package admission before use.
- [ ] Implement both host bridges and physical intent/snapshot wiring. Preserve independent CTCSS source/activity rules and callback timing. Non-FM linked audio can preempt receive on physically half-duplex rigs; explicit tuning cannot deadlock on open RX.
- [ ] Make CM119 ownership optional in shared `rust/station/src/hardware.rs` and the physical service/mixer preflight for an explicitly configured non-CM119 audio path. Reuse the existing general PortAudio stream and onboard USB discovery. Preserve the present CM119 branch; let selected Hamlib/DSP providers supply signaling and keep parallel GPIO independent. Unsupported CM119 EEPROM/mixer/health operations are unavailable, not successful defaults. Test a generic USB codec without HID alongside unchanged CM119 preflight, both host paths, and generic mixer capabilities; do not build another PCM backend.
- [ ] Close the parity checklist: parallel controls, USB-device reassignment, actual hardware local-repeat controls, shared configurable diagnostics interval, existing tuner/calibration/EEPROM/GPIO/processing behavior. Reuse existing shared operations; do not port legacy native parrot/software repeat back in. Parameterize the host checks, not the implementation.
- [ ] Run focused product/driver/station/standalone tests, `tests/test_rust_product_surface.py` affected cases, and the isolated Asterisk lifecycle cases. Inspect ELF dependencies and test standalone-without-Asterisk, USBRadioPlus-without-controller, and combined adapter installs.
- [ ] Record the exact ABI/SONAME/new package floors in one dependency table and update every consumer and image assertion to it. No guessed ABI bumps or stale package floors. Review the complete diff, update developer docs/evidence, and commit `feat: integrate shared remote base across radio hosts`.

## Task 9: Documentation, publication prerequisites, and acceptance handoff

### Current artifact contracts

These are coordinated candidate minimums, not a release or deployment record.

| Boundary | Contract | Candidate package minimum |
| --- | --- | --- |
| Remote engine | `librptadvremote.so.1`, `rptadv_remote_api_v1`, ABI 1 | `librptadvremote1` / `librptadvremote-dev` 0.1.0~alpha1-1 |
| Hamlib dependency | `librptadv_hamlib_adapter.so.1`, `rptadv_hamlib_api_v1`, ABI 1 | `librptadv-hamlib-adapter1` / `librptadv-hamlib-adapter-dev` 0.1.0~alpha1-1 |
| Radio DSP | `librptadvradio.so.4`, independent external/open carrier sources | `librptadvradio4` / `librptadvradio-dev` 0.1.0~alpha9-1 |
| Physical audio | `librptadv_portaudio_alsa_adapter.so.2`, exact ALSA resolver tail | `librptadv-portaudio-alsa-adapter2` / `librptadv-portaudio-alsa-adapter-dev` 0.2.0~alpha4-1 |
| Physical signaling | `librptadv_gpio_adapter.so.1`, safe-level site outputs | `librptadv-gpio-adapter1` / `librptadv-gpio-adapter-dev` 0.1.0~alpha4-1 |
| Shared radio SDK | `libusbradioplus_product.so.1`, descriptor ABI 1, native config 3, direct callbacks 5, complete remote/readiness tail | `libusbradioplus-product1` / `libusbradioplus-product-dev` 0.1.0~alpha28-1.deb13 |
| Controller product | `librptadv_product.so.2`, descriptor ABI 7, host services 9, `reload_poll` | `rpt-advanced` / `app-rpt-advanced` / `librptadv-product2` 0.1.0~alpha20-1 |

The remote engine, Hamlib, radio DSP, and physical-audio providers have passed
their native gates and are released in the signed package index. Host code and
readiness rollback pass focused tests and fast checks. SDK alpha28 is released
after its full native gates; controller merge/release and physical qualification
remain pending. Runtime dependencies are dynamic.
Source checkouts and candidate pins do not replace installed production packages.
The existing publisher and native image chain are reused after authorization.

**Files:** Applicable ADRs/wishlist, operator configuration/man pages, shared-library package/readme artifacts, workflow callers/images, and ignored evidence checklist.

- [ ] Map every approved design requirement to its focused test and parity row. Run only unresolved component checks/coverage; preserve closed evidence. Complete remaining production coverage before the final hosted gate; do not repeat a local full gate.
- [ ] Write concise operating docs immediately before PRs: model/transport selection, limits, independent signaling, memory/scan examples, complete DTMF table, authorization, errors, and model-specific qualification limits. Update statuses only to what code and evidence establish; speech-squelch/IQ features not implemented here remain planned.
- [ ] When candidate code/tests are concrete, obtain authorization to create/publish the new repositories and use the maintained shared-library workflow templates. Validate prerequisite libraries through full hosted gates before authorized merge/release; refresh/verify affected native image chains and signed package minimums before downstream gates consume them.
- [ ] Run fast checks before pushing each component. Open PRs in dependency order and require the full hosted Debian 13 gates, including 100% production line/branch coverage on amd64. Do not merge/release without the applicable user authorization.
- [ ] Request separate node/model test authorization with a specific package set and reversible configuration. Do not assume node 524950 has a CAT rig or parallel hardware. Use it for ordinary repeat/time regression only if authorized; Hamlib dummy tests cannot qualify a physical radio. Keep unqualified models/backends explicitly identified.

## Self-review checklist

- [x] Requirements map covers shared code/dynamic separation, Hamlib transports/names/capabilities, parallel parity, independent signals, limits/transactions, non-FM operation, memories, scanning, every DTMF operation, i18n/source routing, reload/retirement, packaging, and host-independent installation.
- [x] Each task has a focused RED/GREEN test and a checkable completion. Signatures and typed names match across tasks; the implementation adds no undeclared raw API escape hatch.
- [x] Review-focus failures each have an owning test. No task restores removed native features, implements another wishlist feature, or permits live-node side effects without authorization.
- [x] The plan uses only the approved shared architecture; no duplicate scan/memory/Hamlib/parallel implementation is embedded in either host.

Plan preparation: self-reviewed against the approved design; implementation checkboxes remain open. Native execution is recommended because these tasks share contracts and existing owners; request plan review before implementation.
