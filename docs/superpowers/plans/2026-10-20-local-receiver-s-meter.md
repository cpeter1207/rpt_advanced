# Local Receiver S-meter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provide a calibrated local receiver S-meter, with flat-FM DSP or external RSSI input and a localized spoken `*76` query.

**Architecture:** Reuse the radio core's noise-filter arithmetic, existing external-sensor acquisition, and the controller's status/telemetry owners. Carry typed receive observations through the current dynamic boundaries; retain the command's actual receive-burst reading before unkey. No additional PCM ring, device owner, squelch policy, or telemetry renderer.

**Tech Stack:** Rust 1.85, stable sized C descriptor tables, existing rtrb queues, Linux IIO/hwmon, Fluent, current native Debian 13 quality images.

**Spec:** [Approved local receiver S-meter specification](../specs/2026-10-20-local-receiver-s-meter-design.md).

## Global Constraints

- Native PCM remains 48 kHz; noise integration is exactly 960 samples/20 ms across arbitrary callback partitions.
- Defaults: disabled; source `disabled`; S9 `-93` dBm; attack `20` ms; decay `500` ms; stale age `1000` ms; no inherited `dtmf_id`.
- One S-unit is 6 dB. No RF calibration means no invented dBm/S-unit reading. Relative quieting is `clamp(100 * (1 - noise_rms / reference), 0, 100)`.
- Shared libraries remain dynamic. Reuse descriptor tails; never enlarge an unsized result in place or validate an old prefix with the size of the newest SDK table.
- Preserve audio, gains, COR/CTCSS decisions, existing site-query timing and source-only telemetry routing. No unrelated wishlist implementation.
- Callbacks perform only bounded, preallocated DSP/publication: no allocation, locks, device reads, text formatting or TTS.
- Preserve the user's unrelated appliance-spec edit. Use existing checkouts; no reset, cleanup, or extra worktrees unless isolation becomes necessary.
- Targeted RED/GREEN checks first; track evidence in each repository's ignored `.work/quality-progress.md`. Use freshly pulled maintained images through the labeled launcher. No local full gate or QEMU.
- Developer docs accompany implementation commits. Operator docs are updated immediately before PR creation. Full native Debian 13 amd64/arm64 gates and amd64-only 100% production coverage remain merge requirements.
- This plan authorizes no live configuration change, service restart, link operation, release or installation. Obtain separate approval for those steps.

## Review Focus

1. Right-channel reception or TX-blanked zeros must not appear as a fresh strong signal: Task 1.
2. Old SDK/provider prefixes and missing descriptor tails must fail safely without a buffer overrun: Task 3.
3. A short burst between ADC polls must not reuse a pre-key reading: Task 4.
4. A completed `#` command followed by fading, delayed unkey delivery, or another station must retain the correct burst: Task 4.
5. RF calibration must not survive a relevant device/gain change or silently extrapolate past full quieting: Tasks 2 and 5.

## File and Interface Map

Repository paths below are relative to `2026-08-30`; `rpt_advanced/` is the current checkout.

| Boundary | Principal files | Responsibility |
| --- | --- | --- |
| Raw measurement | `librptadvradio/src/{receive_frontend,receive_path,session,lib}.rs`, public header | Normalized noise, clean-window validity, sized descriptor-tail observation |
| Radio bridge | `usbradioplus/rust/radio/src/{lib,raw,session}.rs`, `rust/station/src/{media,runtime,update}.rs` | Enable and copy observation on the existing serial RX owner |
| USB product bridge | `usbradioplus/rust/product/src/{lib,native}.rs`, public header, `rust/asterisk/src/host/channel.rs` | Extend direct registration, preserve device lease and stop/reload lifetime |
| Meter model/config | New `rpt_advanced/rust/core/src/signal_meter.rs`, `signal_meter_tests.rs`, `config/signal_meter.rs`; existing config exports/scope/schema | Calibration, source quality, smoothing, status and inherited settings |
| Host bridge | Controller product header, `services.rs`, `worker.rs`; standalone `radio_host.rs`, `radio_activation.rs`, `host_services.rs`; Asterisk `radio.rs`, `product.rs` | Sized observer registration and callback-to-control publication |
| Query/report | Core `runtime/{dtmf,aggregate,composition}.rs`, product `host.rs`, `host_site.rs`, `messages/en-US.ftl` | Burst association, typed immutable reply, localized source-only playback |

Do not split or rewrite existing large modules merely to implement this feature. New tests follow their owner's existing layout.

## Verification Commands

Run commands below from the named repository, inside its prepared native Intel container. Launch with:

```sh
sh ../rpt_advanced-workflows/tools/run-in-quality-container.sh \
  ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest "$PWD" \
  sh -lc 'COMMAND'
```

Use the corresponding maintained USBRadioPlus quality image where its manifest requires one. Record the pulled digest. Candidate dependencies are installed as built Debian SDK/runtime packages inside disposable containers; never copy implementation source into a consumer. Where a hosted gate requires published dependencies, pause for release approval instead of inventing package versions.

### Task 1: Publish a trustworthy raw noise observation

**Files:** Radio-core files/header above; `librptadvradio/tests/descriptor_smoke.c`; existing receive/session tests.

**Interfaces:** Append `session_set_noise_measurement(void *session, uint32_t enabled)` and `session_noise_observation(void *session, struct rptadv_radio_noise_observation *output)` to the radio descriptor. Both return existing radio status codes and run only on the serial RX owner. The sized ABI-1 record carries generation/config revision, completed-window sequence, first/end native sample indices, normalized `noise_rms`, and explicit validity flags; it does not contain RF units. Retain radio ABI/SONAME 4 through additive tail admission.

- [ ] Write failing `noise_meter_*` tests: selected right input versus unrelated left audio; both COR choices; 960 versus two 480 calls; blanking/clipping invalidation; a clean recovery window; unchanged carrier/audio outputs with metering enabled versus disabled.
- [ ] Run `cargo test --locked noise_meter_`; verify a real missing-behavior failure before implementation.
- [ ] Preallocate measurement history in session setup. Share FIR arithmetic, but retain separate state when raw tap/channel differs from the existing carrier detector. Configure measurement from the RX owner, not control borrowing a live session. Accumulate before program conditioning; invalidate affected windows rather than treating muted zeros as quieting.
- [ ] Implement the sized observation copy and descriptor tail. Leave existing `rssi_peak`, unsized results and their consumers unchanged. Complete windows by sample count and preserve actual sequence/time through repeated copies.
- [ ] Run `cargo test --locked noise_meter_`, affected `receive_frontend`/`receive_path`/`session` filters, then the descriptor smoke test. Test truncated output records, absent tails and nonfinite input explicitly.
- [ ] Update Rustdoc/header docs and commit only the radio change after its affected checks.

### Task 2: Implement meter configuration and pure measurement policy

**Files:** New core model/config/tests above; `rust/core/src/{lib.rs,config/mod.rs,config/scope.rs,config/schema.rs}` and their existing tests.

**Interfaces:** `ResolvedSignalMeterSettings::resolve(&ConfigDocument, &NodeId) -> Result<Resolution<Self>, ConfigError>`; `CalibrationCurve::map(f64) -> Option<CalibratedLevel>`; `SignalMeterState::new(ResolvedSignalMeterSettings, String) -> Self`; `observe(&mut self, SignalObservation, u64)`; `snapshot(&self, u64) -> SignalMeterSnapshot`. `CalibratedLevel` retains `dbm: f64` and `SignalRange::{Below, InRange, Above}`. Reuse site `ReadingQuality` and real observation times rather than inventing a second health vocabulary.

`SignalObservation` owns `generation: u64`, `sequence: u64`,
`observed_at_ms: u64`, `value: Option<f64>`, `quality: ReadingQuality` and
`native_window: Option<(u64, u64)>`. Native windows are half-open first/end
sample indices; external sensor reads have no invented native window. The
configured source determines units. `SignalMeterSnapshot` retains this raw
observation, calibration identity/state, optional calibrated level/relative
quieting and independently smoothed display; none of its timestamps mean query
time. Host code translates the radio's sample clock through the existing epoch.

- [ ] Add failing `signal_meter_*` configuration/policy tests. Assert global/node inheritance, explicit selectors, positive finite reference, unknown-value warnings, calibration monotonicity, bounds, and selector conflict with site sensors/inputs.
- [ ] Run `cargo test --locked -p rpt-advanced-core signal_meter_`; confirm RED.
- [ ] Resolve the exact approved settings/defaults. Parse `calibration_points` as comma-separated `source_value:dbm` pairs. Allocate the finite table on control; require distinct monotonic finite coordinates, decreasing dBm for increasing DSP noise. Do not add a fixed software node limit or new dependency.
- [ ] Implement interpolation inside measured range; return an explicit RF bound outside it. Pin fixtures: `(0.01, -93), (0.02, -99), (0.04, -111)` give `-99` at `0.02`, `-105` at `0.03`, S9 at `-93` and S8 at `-99`. Reference `0.1`, noise `0.02` gives relative quieting `80`, not dB/S units.
- [ ] Produce a versioned canonical calibration fingerprint from resolved source/device/channel, known frequency/bandwidth, ADC gain, filter and tap. Use unambiguous canonical fields, not Rust's unstable/default hashing. It qualifies both the RF curve and the explicit no-carrier noise reference. A mismatch disables calibrated reporting, not the radio. Test restart-equivalent identities and downstream EQ changes preserving calibration.
- [ ] Apply asymmetric elapsed-time smoothing only for new valid sequences; `0` smoothing means immediate. Stale age must exceed source observation cadence; invalid configuration follows the existing warning/default rules. Test repeated timestamps, source failure, NaN, saturation and gain-change invalidation.
- [ ] Run targeted tests, affected Rustdoc and core lint/static checks; commit the self-contained model/config change.

### Task 3: Carry observations across the existing dynamic radio lease

**Files:** USBRadioPlus radio/station/product/host bridge files above and their tests; controller header/services/standalone/Asterisk host files; `tests/test_product_abi.c`, SDK/admission fixtures and dependency manifests.

**Interfaces:** Append `radio_set_receive_observer(context, radio, const struct rptadv_radio_observer_v1 *registration)` to controller host services, before the existing `radio_activate` is called. Registration is sized, retains an opaque context and `observe(context, const struct rptadv_radio_noise_observation *)` callback, plus enablement. Extend sized `UrpAstDirectCallbacks` registration ABI 3 to 4. Host ABI 6/product ABI 5 and their SONAMEs need not change for appended admitted tails; update actual supporting package floors together, not guessed release numbers.

- [ ] Write failing bridge tests proving one lease/one RX owner, ordered observer-before-PCM delivery, disabled behavior and required-tail admission. Assert the old unsized radio receive result remains the same size.
- [ ] Run USBRadioPlus `cargo test --locked -p usbradioplus-station` and controller `cargo test --locked -p rptadv-product worker`; confirm the new assertions fail.
- [ ] Retain observer registration alongside existing direct endpoints. The serial RX owner sets measurement enablement before processing and copies the completed observation immediately before its normal PCM callback. The observer copies scalars only; no callback-time control submission, allocation or formatting.
- [ ] Forward the same typed registration through standalone's borrowed USB product and the acknowledged Asterisk channel option. Do not replace activation, create a second hardware owner or put radio DSP in an adapter. Reject an enabled meter when its selected observation contract is absent; never synthesize fallback RF data.
- [ ] Validate only the required field-end size/non-null tail operations. Update every exact direct-registration fixture/caller to ABI 4; do not retain obsolete alpha-only registration branches. Reload/stop synchronously quiesces registration contexts before freeing them.
- [ ] Run affected radio/station/product/Asterisk tests, standalone `radio_` tests and `make product-abi-check`. Exercise truncated provider tables, null functions, failed activation, disable/re-enable, same-device reload and stop-after-registration.
- [ ] Update boundary developer docs and exact candidate dependency checks; commit each repository's bridge change separately.

### Task 4: Retain the command's local receive-burst observation

**Files:** Core `runtime/dtmf.rs` and its tests; core `signal_meter.rs`; product `worker.rs`, `worker_tests.rs`, `host.rs`, `host_site.rs`; core aggregate/link tests.

**Interfaces:** Extend the existing copied event/metadata handoff rather than create a PCM queue. `SignalQueryCapture` identifies generation, source burst and its finalized `SignalMeterReport`. Add `Runtime::observe_signal_meter(local: &str, observation: SignalObservation, now_ms: u64) -> Result<(), RuntimeError>` on station control. Local query resolution retains its burst key; peer resolution captures the current local observation at admission.

Represent capture as `SignalQueryCapture::{LocalPending { generation: u64,
burst: u64 }, Frozen(SignalMeterReport)}`; the ordinary bounded request owns
that value. Freeze validity at the actual capture boundary, not after a long
telemetry-queue wait. Generation/connection retirement still cancels delivery.

- [ ] Add failing `signal_meter_query_*` tests. Test a request completed by `#` while still keyed, the same burst completed by unkey, a delayed control pump, immediate subsequent key-up, queue overflow, retired generation and disconnected/replaced peer.
- [ ] Run `cargo test --locked -p rpt-advanced-core signal_meter_query_` and affected product worker tests; confirm RED.
- [ ] Associate observations with existing local receive/qualification epochs on the serial RX owner. Carry timestamped bounded scalar metadata in source order with DTMF events. Finalize the last wholly eligible window at unkey even when `#` arrived earlier. Control observes it before retiring that burst; subsequent bursts cannot overwrite a captured reply.
- [ ] Derive retention from existing bounded command/reply ownership. Metadata loss/retirement yields unavailable, never a different burst. Preserve existing `Lost` handling and expose lost-observation accounting without logging on audio.
- [ ] For external RSSI, reuse the named sensor's actual acquisition time. Require the reading to belong to the requested burst after the hardware's calibrated settling period and within freshness limits. Hardware qualification must establish that the receiver RSSI is usable at the selected acquisition cadence; no automatic settling estimator is added. A fresh pre-key read or a burst missed by polling is unavailable. Do not change the sensor's configured poll interval automatically.
- [ ] For peer requests, use local receiver activity/measurement only; no local signal produces the appropriate typed report. Preserve exact originating connection and wait for its unkey. Ordinary site sensors still capture after unkey as before.
- [ ] Run burst/overflow/generation tests, existing `site_report_captures_after_unkey` regression and source-routing tests. Commit with updated developer docs.

### Task 5: Expose status and localized spoken reports

**Files:** Core `runtime/{aggregate,composition}.rs`, message/catalog tests, `messages/en-US.ftl`; product `host_site.rs`, status/host tests; config integration tests.

**Interfaces:** Add a typed signal-meter recipe/report to the existing recipe catalog, not a changing-unit numeric `SiteReport`. Reuse `queue_site_report(local, selector, reply)` to select it. `SignalMeterReport` distinguishes calibrated hardware/estimated DSP, in-range/bounds, relative quieting, uncalibrated, unavailable and no-local-signal results; `SignalQueryCapture` supplies its frozen value.

- [ ] Add failing `signal_meter_report_*` tests for `*769#` with selector `9`, command-prefix remapping, duplicate selector rejection, public read-only authorization, locale substitution and TTS failure/Morse fallback.
- [ ] Run `cargo test --locked -p rpt-advanced-core signal_meter_report_`; confirm RED.
- [ ] Register the meter in the existing `*76` selector namespace. Add separate Fluent `.text`, `.tts` and `.morse` messages for every approved report state. Pass numeric values as localized arguments; do not build English words in Rust. Below S1 say "below S one"; above S9 report plus dB. Speech uses nearest integer S-unit/dB without losing a calibrated-range qualifier.
- [ ] Feed the prepared result to the existing telemetry producer/ring after origin unkey. Retain source-only routing: local RF to local TX, peer only to that live connection. Never broadcast. CLI status remains on its control interface and does not key RF.
- [ ] Append raw source/units, valid acquisition age/sequence, calibration fingerprint/state, dBm/range or relative quieting and smoothed display to current status. Use the existing snapshot interval; queries/repeated polls cannot refresh acquisition age. No new REST/WebSocket transport.
- [ ] Run message, query, status and source-isolation regressions. Include no calibration/reference, saturated noise floor, blanked receiver, stale sensor and a source already active when configuration reloads. Commit developer-doc/test changes.

### Task 6: Integrate, review and prepare the validation handoff

**Files:** Affected public headers/developer ADRs; `WISHLIST.md`; dependency manifests/build/package assertions; operator docs/examples only at PR preparation.

- [ ] Compose all changed DSOs through built candidate Debian packages in disposable native containers. Verify `ldd`/`readelf` dynamic dependencies, real descriptor admission and identical standalone/Asterisk RF observations. No vendored or statically duplicated radio implementation.
- [ ] Run targeted replay/integration fixtures with varied modulation and callback partitions. Confirm PCM, carrier, tone qualification and levels are unchanged with the meter disabled and enabled. Physical RF accuracy remains unclaimed until generator validation.
- [ ] Finish per-part test/coverage/doc evidence, then run each changed repository's `make lint static-analysis` once in its fresh maintained image. Do not run a full local coverage/gate cycle. Request one independent whole-change review; fix only findings in scope.
- [ ] Before a requested PR, update concise operator configuration/query instructions, ADR implementation status and wishlist truthfully. Do not mark deployment or hardware qualification complete. Preserve the unrelated appliance-spec edit.
- [ ] Commit the completed implementation in recovery-sized repository commits. On approval to publish, push and use GitHub's full PR gates; release prerequisites in radio -> USB product -> controller order only with approval and update quality images from real released package versions. Merge/release/install are separate handoffs, not implied by this plan.

## Completion Evidence

Record targeted RED/GREEN commands, per-part coverage and developer-doc results,
provider descriptor versions/package candidates, dynamic-link checks and review
findings under ignored `.work/`. Do not commit transient reports. The feature is
implemented only after the query, status, both sources and reload lifetime are
covered; it is hardware-qualified only after an approved RF-generator test.
