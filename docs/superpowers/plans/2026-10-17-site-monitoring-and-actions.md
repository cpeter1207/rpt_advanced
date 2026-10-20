# Site Monitoring and Actions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Provide configured site measurements, digital I/O, alarms and fan control, with localized read-only reports and safe device ownership.

**Architecture:** Put pure site policy in the shared controller, consume bounded observations on its existing control owner, and perform physical I/O in device owners outside control and audio. Reuse USBRadioPlus's existing CM119/parallel owner and add independently packaged external-library adapters only for libsensors and libgpiod.

**Tech Stack:** Rust, existing control/lifecycle providers, Fluent and the existing station telemetry producer, Linux hwmon/libsensors, libgpiod, kernel IIO, native Debian 13 quality images.

**Spec:** [Approved site-monitoring design](../specs/2026-10-17-site-monitoring-and-message-macros-design.md), approved 2026-10-17. Message rendering uses the [companion implementation plan](2026-10-17-message-composition-and-tail-playback.md).

**Status:** Native implementation approved 2026-10-18. The message prerequisite
is locally implemented and tested. S1 configuration/measurement and S2 pure
alarm/output/fan policy and S3 existing-owner bridge pass focused tests. S4 IIO,
external adapters and selected-provider composition pass scoped tests. S5 state
transfer is locally tested; runtime acquisition and S6–S7 remain. No node changes or deployment
are authorized by this document.

## Global Constraints

- Standalone and the Asterisk adapter share policy and status through the product; no per-adapter site implementation.
- No fixed port/sensor count, assumed board pinout, per-chip userspace driver, or thread per sensor. Only configured endpoints and their required capabilities are acquired.
- No device I/O, allocation, logging, blocking, or policy evaluation on audio callbacks. Control receives bounded observations; slow device work cannot block another device or node.
- One physical output owner. Site operations cannot claim radio PTT, CTCSS, clipping-indicator, or interlock outputs already in use. Hardware interlocks remain authoritative.
- `[kind]`, `[kind node]`, `[kind node name]` inherit field by field for `sensor`, `input`, `output`, `alarm`, and `fan`. Defaults do not create instances. Identities and DTMF selectors are instance-only.
- Sensors default disabled, precision `1`, poll `1000 ms`, stale `5000 ms`, smoothing `0`, calibration `none`. Stale time must exceed the positive poll interval. Apply driver unit conversion once, then optional two-point calibration.
- Inputs default active-high and `50 ms` debounce. Outputs default startup/safe `off` and pulse `500 ms`; pulse returns to its prior logical state.
- Alarms default disabled, trip/clear delays `0`, nonlatched, repeat `0`; fan run-on defaults `120000 ms`. Thermal thresholds/curves are installation-specific and required when selected.
- Missing/failed readings never become zero or refresh freshness. Unknown does not clear a threshold alarm. Failed temperature input requests full cooling if the output remains available; physical safety cannot be guaranteed by software after disconnect/power loss.
- Public DTMF `link_command_site_status = 76` queries configured canonical positive decimal selectors only. No DTMF configuration, output administration, arbitrary expression, or shell execution is added.
- All voiced reports/alarms use the companion plan's receive-driven Morse selection. Preserve exact-source replies and local-only unsolicited alarms. Protective actions do not wait for message playback or admission.
- Use ADR 0019 warning/default resolution; reject reload only when a safe deterministic candidate cannot be built. A configured protection with a failed source is faulted, not silently normal.
- Preserve unchanged ownership, pulse prior state/deadline, fan run-on and sensor extrema across reload. Changed/removed endpoints complete safe handoff before cancellation/retirement.
- Developer documentation before commits; operator docs before PR. No new REST, WebSocket, historical database, remote sensor protocol, or other unfinished wishlist capability.

## Review Focus

1. Successful polling of an old cached GPIO value can disguise a failed physical read: S1/S3/S5 test observation timestamps and stale health.
2. Reload during an output pulse can leave it on, extend it, or toggle PTT: S2/S3/S5 test original deadlines, reserved masks, and safe handoff.
3. Alarm repeat or telemetry backpressure can duplicate or delay protective actions: S2/S6 test transition-only actions independently of playback.
4. Unit conversion applied twice, stale values, or formatted rounding can produce false alarms: S1/S2 test raw normalization, unrounded comparisons and quality.
5. A slow/unavailable backend can stall all nodes or falsely claim cooling: S4/S5 test isolated owners, bounded work and requested/submitted/observed state.

## Execution and verification

Paths are relative to `rpt_advanced` except those explicitly starting `../`. Read each changed repository's `AGENTS.md` before implementation. Preserve the unrelated edit to `docs/superpowers/specs/2026-10-16-appliance-signaling-interlock-design.md`.

Use the maintained `../rpt_advanced-workflows/tools/run-in-quality-container.sh` launcher with freshly pulled `ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest`. Record its digest. The launcher owns labeled stale-container cleanup and exit cleanup. Required released adapter packages must be installed in the image; do not vendor implementation code or substitute a local static build.

Run named failing tests, implement the narrow task, rerun its tests, update Rustdoc and commit explicit paths. Keep results and completed coverage items in ignored `.work/quality-progress.md`. A filter matching zero tests is not a pass. Fix affected source coverage/docs first; no repeated local full gate. Before an authorized push run format/lint/static analysis once. The final hosted PR gate runs Debian 13 amd64/arm64 tests and 100% production coverage on amd64; Debian 12 remains manual-only.

## File and interface map

| Owner | Files |
| --- | --- |
| Effective site configuration | New `rust/core/src/config/site.rs`; existing `config/{mod,scope,schema,settings}.rs` |
| Pure readings/policy | New `rust/core/src/site/{mod,measurement,alarm,output,fan}.rs` and focused sibling test files |
| Acquisition and lifetime | New `rust/product/src/site/{mod,worker,iio}.rs` and tests; existing `host.rs`, `lifecycle.rs`, `services.rs` |
| Reports/actions | Existing core `command.rs`, `runtime/{aggregate,dtmf,composition}.rs`, product `host.rs`, `messages/en-US.ftl` |
| Owned CM119/parallel access | Existing USBRadioPlus `rust/driver/src/hardware_host.rs`, product `src/{native,lib}.rs` and product header |
| External C adapters | New workspace crates `rust/hwmon-adapter` and `rust/gpiod-adapter`, patterned on the resolver adapter |
| Packaging/composition | `Cargo.toml`, `Cargo.lock`, `Makefile`, `debian/control`, adapter `.install` files; existing loader manifests/tests |

### S1: Resolve site definitions and model trustworthy measurements

**Files:** Create `rust/core/src/config/site.rs`, `rust/core/src/site/{mod,measurement,measurement_tests}.rs`. Modify `lib.rs`, `config/{mod,scope,schema,settings}.rs` and existing scope/schema tests.

**Interfaces:** `ResolvedSiteSettings::resolve(document: &ConfigDocument, node: &NodeId) -> Result<Resolution<Self>, ConfigError>` returns ordered named definitions and resolved endpoint identities. `ReadingQuality::{Valid,Stale,Unavailable,Fault}`; `Observation<T> { value: Option<T>, observed_at_ms: Option<u64>, quality: ReadingQuality }`. `SensorState::observe(&mut self, reading: Observation<f64>, now_ms: u64)` accepts adapter-normalized source units. `SensorState::snapshot(&self, now_ms: u64) -> SensorSnapshot` returns calibrated current, smoothed, extrema/time and health; formatting is separate.

- [x] Add `site_config_inheritance_and_identity`, asserting instance > node > global defaults, no instance from defaults, disabled missing endpoints, one sensor/input name namespace, duplicate selector handling and no fixed count. Unknown values warn/fall back; conflicting physical output ownership is a true error.
- [x] Add `site_measurement_scale_once`: adapter input `2.0 V`, calibration `0..5 V -> 0..20 A`, yields `8.0 A`; precision changes no stored value. Test distinct finite calibration points and rejection/defaulting of NaN/infinity.
- [x] Add fake-clock `site_measurement_quality_extrema`: valid sample at `1000`, failed read at `2000` does not refresh its time or report zero; stale at the `5000 ms` age boundary. Extrema use unsmoothed valid values and retain their times. Unchanged reload preserves extrema; changed source/calibration resets them. Compare elapsed-time smoothing across unequal polling intervals.
- [x] Run `cargo test --locked -p rpt-advanced-core --lib site_` and confirm the new assertions fail. Implement resolved definitions and measurements without I/O or a formula parser. Re-run to PASS with nonzero test count.
- [x] Document units, time/freshness and reset contracts in Rustdoc. Commit this independently testable policy foundation.

### S2: Implement deterministic alarms, output pulses and fan demand

**Files:** New `rust/core/src/site/{alarm,alarm_tests,output,output_tests,fan,fan_tests}.rs`; extend `site/mod.rs` and `config/site.rs`.

**Interfaces:** `SiteController::advance(&mut self, now_ms: u64, observations: &SiteObservations, ptt: &BTreeMap<NodeId, bool>) -> Vec<SiteEffect>` runs on control. `SiteObservations` maps stable configured sensor/input names to S1 observations. `SiteCommand` covers output on/off/pulse, alarm acknowledge/reset, extrema reset, fan automatic/manual demand and explicit protective inhibit. `SiteController::command(&mut self, command: SiteCommand, now_ms: u64) -> Result<Vec<SiteEffect>, SiteError>` is an already-authorized internal operation. `SiteEffect` distinguishes output requests, fan demand, transmit inhibition, named existing action macros and message requests. It never performs I/O or admission itself.

- [x] Add `site_alarm_transition_and_unknown` table cases for above/below/active/inactive/unavailable, hysteresis, trip/clear persistence and current/smoothed selection. Test an unrounded `13.96` against `14.0` even when displayed as `14.0`. Unknown interrupts continuity of persistence but never falsely clears an active threshold alarm.
- [x] Add `site_alarm_ack_reset_repeat`: trip actions run once, repeated messages do not repeat equipment actions, acknowledgment only silences repeats, latched state needs healthy clear plus reset. Reset during an unsafe condition fails and never resets a physical interlock.
- [x] Add `site_output_pulse_prior_state`: default pulse at `1000 ms` ends at `1500 ms`, restoring both initial-on and initial-off cases. A repeated pulse extends from the latest command; explicit on/off cancels it. Safe-state/fault wins immediately. Preserve the original deadline/prior state on unchanged reload.
- [x] Add `site_fan_demand_and_failure`: PTT run-on ends exactly `120000 ms` after release; temperature hysteresis and ordered duty curves; combined mode selects greater demand. Missing temperature requests full cooling; missing control reports fault. Test optional RPM/spin-up monitoring only where capabilities exist and manual commands cannot override protection.
- [x] Run core `site_` filters red, implement the pure state machines with monotonic fake-clock time, and rerun green. Do not reuse legacy GPIO pulse behavior that returns to a baseline instead of the prior logical state. Update Rustdoc; commit.

### S3: Expose spare CM119/parallel I/O through the existing owner

**Files:** In `../usbradioplus`, modify `rust/driver/src/hardware_host.rs`, `rust/product/src/{native,lib}.rs`, `rust/product/include/usbradioplus_product.h` and existing `rust/driver/src/tests/hardware_host_tests.rs`, `rust/product/src/tests/native.rs`. In this repo modify `rust/standalone/src/{radio_activation,host_services}.rs`, `rust/asterisk/src/{radio,services}.rs`, `rust/product/src/services.rs`, `rust/product/include/rptadv_product.h` and their contract fixtures. Use the existing `../rptadv-gpio-adapter` operations without adding a second device handle.

**Interfaces:** A control-only radio site-I/O bridge borrows its existing radio handle. Define `SiteDigitalRequest { generation: u64, output_mask: u32, high_mask: u32 }` and `SiteDigitalSnapshot` with capabilities/reserved masks, actual input observation time/quality and separate requested/submitted/readback-valid output state. Operations are `site_publish(request) -> Result<(), SiteError>` and `site_snapshot() -> Result<SiteDigitalSnapshot, SiteError>`; CM119 and parallel banks are explicit endpoint identities. The shared product's host-service table forwards them to the current native/channel owner. C records include size/version, no Rust containers, and no external library types.

- [x] First test a reserved PTT/CTCSS/interlock/clip bit request is rejected without a hardware write, while a spare bit changes without altering any reserved bit. Assert ordinary input acquisition cannot open a second HID or parallel owner.
- [x] Add `site_owner_read_failure_is_not_fresh`, `site_owner_stale_generation_cannot_write`, `site_owner_reload_no_output_glitch`, and `site_owner_no_fabricated_readback`. Reuse native lifecycle/poll-failure fixtures and the existing radio interlock tests.
- [x] In USBRadioPlus's fresh native quality image run `cargo test --locked -p usbradioplus-driver --lib hardware_host::tests` and `cargo test --locked -p usbradioplus-product --lib tests::native`. In RPT Advanced's image run `make radio-interlock-check` and affected service-contract tests. Verify new tests fail before changes. Extend the existing service command/snapshot machinery and merge authorized masks only in that owner. Timestamp successful physical acquisition, not a later status read. Never execute I/O from this bridge's caller or audio.
- [x] Use existing typed command/descriptor extension points where possible; document any required descriptor/host revision and update exact version assertions/consumers together. ADR 0040 forbids a compatibility-only alternate implementation. Do not claim an unimplemented bridge works via a preexisting native function: current native API has only create/start/stop/destroy.
- [x] Re-run contract/lifecycle/interlock tests, document the ownership boundary and commit each component separately. A released provider dependency is required before the consumer PR gate: obtain release approval when ready, then update the pinned dependency manifest/image. No live changes are implied by this task.

S3 verification: USBRadioPlus alpha24 and GPIO alpha4 providers are released.
RPT host ABI6 bridge checkpoint `14763b1` passed targeted contracts, seven-port
interlock composition, package combinations, strict lint and developer docs.
Dependency-only PR43 passed the full hosted gate and merged; refreshed native
quality images contain the required providers. S5 will consume the bridge.

### S4: Add the two external adapters and direct IIO acquisition

**Files:** Create `rust/{hwmon-adapter,gpiod-adapter}/{Cargo.toml,build.rs,src/lib.rs,src/tests.rs}`, `rust/hwmon-adapter/include/rptadv_hwmon_adapter.h`, and `rust/gpiod-adapter/include/rptadv_gpiod_adapter.h`. Create `rust/product/src/site/{iio,iio_tests}.rs`. Modify root `Cargo.toml`, `Cargo.lock`, `Makefile`, `debian/control`; add `debian/librptadv-{hwmon,gpiod}-adapter{1,-dev}.install` using the resolver adapter packaging pattern. Update retained provider manifests in `rust/standalone/src/providers.rs` and selected shared-product initialization, not inside audio.

**Interfaces:** Export `rptadv_hwmon_adapter_descriptor_v1` and `rptadv_gpiod_adapter_descriptor_v1`, with capabilities `rptadv.hwmon` and `rptadv.gpiod`. Each returns its version-1 descriptor with complete size/capability validation and an opaque context. The tables expose `open(const endpoint_v1 *, void **context) -> int32_t`, `read(void *context, observation_v1 *) -> int32_t`, `write(void *context, const output_v1 *) -> int32_t`, `capabilities(void *context, uint32_t *) -> int32_t`, and synchronous `destroy(void *context)`. Use capability-specific `rptadv_hwmon_`/`rptadv_gpiod_` record prefixes. Endpoint records own copied length-delimited stable device/channel identities plus input/output direction; GPIO records also declare active level and startup/safe state. Observation records distinguish finite numeric source units or logical digital state from unavailable readback; output records specify supported numeric control/duty or logical state. Size/version and explicit result/quality codes bound every record. Host callers serialize each context, and providers translate errors without leaking external types or panics.

`IioReader::open(root: &Path, device: &str, channel: &str) -> Result<Self, SiteError>` and `IioReader::read(&mut self) -> Result<f64, SiteError>` live in the acquisition owner; test root is an isolated fixture directory. No generic sensor library or OS facade is added.

- [x] Add dynamic-descriptor fixtures for missing/short/wrong-version tables, null handles, failed open/read/write, unsupported capability and synchronous cleanup. Add `site_hwmon_source_units`, `site_gpiod_stable_line_and_polarity`, `site_iio_scale_offset_once`, and `site_pwm_requires_real_capability`. Device absence must not silently bind the next enumerated device.
- [x] Run failing tests for the two new crate packages and product `site_iio_` filters. Inspect the released Debian 13 library headers/documentation when writing bindings; use existing bindgen/package conventions rather than implementing sensor/chip drivers.
- [x] Implement separately versioned `librptadv_hwmon_adapter.so.1` and `librptadv_gpiod_adapter.so.1`. Use hwmon/library capabilities for actual measurements and supported writable controls; if writable PWM is absent, report it unsupported. Libgpiod supplies digital lines, not software PWM. IIO applies the selected kernel channel's scale/offset exactly once.
- [x] Add package/header/install and descriptor-loader tests. Only selected capabilities are mandatory: a node with no hwmon/libgpiod endpoints must not fail because an unselected provider is absent. Selected incompatible providers fail composition/reload safely; do not silently switch backend.
- [x] Run the focused adapter/IIO suites, package staging assertions and Rustdoc. Commit code/developer docs. Update workflow image dependency definitions with released libraries and verify derived native manifests before consumer checks; do not change workflow implementations in the production repository.

Local recovery checkpoints: `7c5557c` (gpiod), `1a23654` (hwmon), `df40018`
(provider lifetime/admission). Strict scoped tests, package checks and developer
documentation passed in the current native image. Full production coverage and
hosted branch validation remain S7; no physical sensor hardware was tested.

### S5: Wire bounded acquisition, state transfer and safe shutdown

**Files:** New `rust/product/src/site/{mod,worker,worker_tests}.rs`; modify `rust/product/src/{lib,host,lifecycle,services}.rs`, core `runtime/{aggregate,generation}.rs` and their tests.

**Interfaces:** `SiteService::prepare(settings: ResolvedSiteSettings, generation: GenerationWork, backends: SiteBackends) -> Result<Self, SiteError>` acquires only configured endpoints. `SiteService::poll(&mut self, now_ms: u64) -> SiteObservations` submits due bounded requests and drains results without waiting. `SiteService::publish(&mut self, effects: &[SiteEffect])` queues owner commands. `SiteService::retire(&mut self)` initiates safe handoff and waits for owner quiescence only during non-audio teardown. `SiteBackends` is the selected retained provider set, not an additional public plugin framework.

- [ ] Add `site_slow_device_isolation`: one fixture read remains pending while another device/node continues; pending polls coalesce rather than create an unbounded backlog. No sensor gets its own thread. State snapshots distinguish desired, submitted and observed outputs.
- [ ] Add `site_reload_preserves_pulse_and_fan_deadlines`: pulse begun at `1000`, ending at `1500`, reloaded at `1200` still ends at `1500`; PTT fan run-on likewise retains its original deadline. Removed output attempts safe state before retirement; failure remains visible and rejects unsafe replacement ownership.
- [ ] Add `site_stale_work_ignored` and `site_shutdown_no_replayed_write`: generation validation before physical dispatch and before result application, and a synchronized ownership barrier that prevents old work racing a new owner. Ignoring an old result is not enough to prevent an old physical write.
- [ ] Run product `site_` tests red. Use one bounded serial acquisition owner per physical device, shared by its sensors, or reuse the already-running CM119/parallel owner. Feed its bounded results into existing `Host::pump` through the existing control executor. Do not add another station-policy thread or make control wait for a read. Respect library-global serialization inside the adapter without propagating it to audio or other backends.
- [ ] Preserve unchanged owners and transfer state; safe-handoff changed endpoints before canceling their timers. Reuse `GenerationWork` and `Engine::run` lifecycle validation. A stuck device can fault its own owner but must not permit a second writer or block unrelated station service. Re-run tests plus existing control rejection/drain and reload suites; update Rustdoc and commit.

### S6: Connect reports, actions and status without bypassing safety

**Files:** Modify `rust/core/src/{command.rs,runtime/aggregate.rs,runtime/dtmf.rs,runtime/composition.rs,controller/telemetry.rs}`, product `host.rs`, site files and `messages/en-US.ftl`; extend existing command/runtime/product integration tests.

**Interfaces:** `SiteController::snapshot(&self, now_ms: u64) -> SiteSnapshot` exposes reading age/quality, alarms/latches/acknowledgment, output requested/submitted/observed state and fan demand/RPM/fault. `SiteSnapshot` supplies companion M4 `MessageValueSnapshot` bindings. `LinkAction::SiteStatus` carries a validated selector, not a link destination. `SiteEffect` from S2 uses existing generation-scoped command dispatch and bounded transmit-inhibit intent; it must not reuse CTCSS-only inhibition as a transmitter interlock.

- [ ] Add `site_dtmf_query_scope`: `*761#` reports sensor/input 1 without administrative unlock, does not alter the last link target, and responds only after the exact source unkeys. Zero does not recall a link, duplicate/remapped prefixes are checked, and no query can write hardware. Unknown/stale values get localized unavailable forms.
- [ ] Add `site_alarm_actions_independent_of_telemetry`: saturate the media queue and assert a trip's output/inhibit actions are submitted immediately and once in configured order. Repeat notification only repeats speech; input edges debounce once. Unsolicited alarms stay local and wait for normal admission, then obey receive-driven Morse interruption.
- [ ] Add `site_actions_preserve_interlock`: manual on/fan commands, reset and existing named action macros cannot bypass a configured inhibit or physical interlock. Validate action references/typed arguments at reload, no recursion or arbitrary shell. Ensure a failing action is diagnosed without erasing the unsafe condition.
- [ ] Run these tests red, connect S1/S2/S5 to M4's shared snapshot/message preparation and existing command dispatcher, then rerun green. Keep protective effects separate from the existing `event_command` path that waits for telemetry admission. Add read-only status and existing authorized CLI operations, not new REST/WebSocket transports or DTMF administration.
- [ ] Verify IDs/tails can report site values using the same snapshot and localized message segments. Unavailable measurements do not erase identification text. Update Rustdoc; commit.

### S7: Validate shared compositions, packaging and recorded status

**Files:** `doc/architecture/{README.md,decisions/0009-scheduled-actions-and-macros.md,decisions/0022-versioned-external-c-dependency-adapters.md,decisions/0023-unified-control-and-dtmf-policy.md,decisions/0033-appliance-radio-signaling-capability-profile.md,decisions/0034-appliance-hardware-output-interlock.md}`, `WISHLIST.md`, dependency/version checks and package fixtures. Before PR: configuration manual pages, `doc/configuration.md` and shipped examples.

- [ ] Run deterministic standalone/Asterisk shared-product integration fixtures for configured digital input, calibrated sensor query, alarm action, fan demand, reload and shutdown. Verify callback allocation/blocking guards and unchanged radio/interlock tests. Software fixtures are not physical wiring qualification.
- [ ] Close changed-source test/coverage/Rustdoc issues using the `.work/` checklist; remove unreachable code rather than suppressing coverage. Verify runtime/dev packages, descriptor checks and selected provider dependencies. Run only the fast preflight locally before any approved push; the authorized PR's hosted full gate remains mandatory.
- [ ] Update architecture/status to distinguish implemented software, untested hardware capabilities and unfinished wishlist work. Before PR, write plain operating instructions, disabled examples, endpoint/capability requirements, and explicit unavailable/fault behavior. Do not present arbitrary URI-family wiring as validated.
- [ ] Commit owned paths. Obtain separate approvals for PR/release sequencing and node testing. If representative sensor/fan hardware is unavailable, report that limitation without inventing a physical pass or adding hardware-specific work.

## Handoff

Review both plans before implementation. Complete and confirm the companion plan's M1 playback prerequisite before dependent message behavior. S1/S2 pure policy and S3/S4 adapter fixtures are independently testable; S6 depends on the companion composition path. Use native execution for the coupled ownership changes, then an independent final review. No live-node work is included in this planning approval.
