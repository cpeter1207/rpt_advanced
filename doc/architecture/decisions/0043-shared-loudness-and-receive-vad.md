# ADR 0043: Shared loudness processing and local receive speech qualification

Status: Implemented, released, and installed (2026-11-07); recorded 2026-11-08

## Context

The owner requested BS.1770 gain metering toward -23 LUFS in the existing AGC,
separate integrated-loudness and true-peak output meters, a final Peak Limiter,
and Earshot speech qualification immediately after local receiver squelch.
These capabilities must use the existing processing graphs, radio owner, PCM
rings, and lifecycle. A control measurement window is not an audio delay line.

[EBU R 128 version 5.0](https://tech.ebu.ch/publications/r128) supplies the
programme-loudness reference. It does not prescribe this live gain rider or
its timing. Jeff DePolo's
[Audio Processing In Mobile Radio](https://www.repeater-builder.com/tech-info/audio-processing.html)
describes a 1–10 dB/s wideband-AGC range; it does not prescribe the project's
exact attack/recovery pair or hold. These records do not claim full R 128,
G.169, physical-radio, or RF-path qualification.

## Decision

### Shared source graphs and gain control

Local receive, per-peer incoming link, and final voice/telemetry processing
reuse the dynamically linked USBRadioPlus resolver and FFmpeg graph. The
existing Rust LADSPA AGC uses full-program BS.1770 K-weighted loudness from
`ebur128` 0.1.11. There is one gain application, with the existing smooth ramp,
gain bounds, activity qualification, and deadband; the controller adds no
second AGC or processing parser.

The common AGC is disabled until enabled by configuration. Its defaults are
-23 LUFS, a 400 ms rolling window configurable from 400 to 3000 ms, 6 dB/s gain
reduction (attack), 2 dB/s gain increase (release/recovery), and the existing
500 ms continuous-activity hold before gain increases. The measurement window
and gain-change rates are separate controls. Gain holds during inactivity;
sample history still advances with silence, including PTT release. A separate
10 ms raw RMS activity detector remains, as do existing peak/RMS diagnostics.
Replacing the AGC's gain-target measurement does not replace those functions.

### Fixed limiter tail and mandatory output meters

Each source graph ends with the configurable **Peak Limiter**, followed by
integrated-LUFS and true-peak measurement of the final processed digital
programme. The limiter's position is fixed. Its common defaults remain
disabled, -3 dBFS ceiling, 4x oversampling, 5 ms lookahead, and 100 ms release.
At the native 48 kHz rate, the limiter processes at 192 kHz and returns to
48 kHz; other supported boundary rates scale by the configured factor.
Output-peak verification includes the return to the boundary rate.

The output meters remain active for unconfigured, bypassed, and limited
graphs, and cannot be disabled through a stage-menu control. Integrated
output loudness verifies the result; it does not drive the rolling AGC.
Statistics retain validity, sample counts, and meter epochs. Silence can
legitimately leave an integrated reading unavailable. These digital programme
measurements precede radio signaling/hardware and do not measure RF deviation
or post-DAC peaks. Retired RMS-target/window and meter-enable options are not
compatibility aliases; configuration warnings follow ADR 0019.

Existing owners advance normally clocked idle silence, including inactive
legacy Asterisk LinkHooks. Their existing timer/framehook handles bounded
quanta and fractional samples. A stalled channel reader coalesces expirations
and resumes with at most one current quantum; this is not a promise of exact
elapsed aging across a stalled reader. See ADR 0036 for those lifetime rules.

### Local receive speech qualification

Earshot 1.2.2 runs in the shared radio immediately after squelch. The released
samplerate adapter supplies 256-sample 16 kHz analysis frames while programme
PCM remains at the native rate. Configuration selects:

- **Off**: the software default; preserve the existing receive path.
- **Kerchunk**: require a fresh voice decision for each raw RF-qualified
  opening, then admit that burst until its RF qualification closes. If no
  closing RF qualifier is configured, normalize to On with a warning.
- **On**: use speech or its configurable hang, default 1000 ms, together with
  every enabled COS/CTCSS/DCS requirement. With those RF requirements disabled,
  speech qualification determines receive activity. The hang applies to On;
  Kerchunk remains latched until the RF burst closes.

Enabled VAD uses the existing delay owner with an effective reserve of
`max(squelch_delay_ms, 160 ms)`, retaining a larger valid configured delay.
Legacy AppRpt8k retains the delay inside the radio; standalone and the
Asterisk controller use the existing tagged local inbound PCM ring. The
optional future shared-clock adapter must retain this reserve too. Only VAD
Off with zero configured delay permits the no-added-local-ring-delay case.

Raw RF eligibility, speech admission, and source/burst identity remain
distinct. Retain eligible PCM so the decision can admit its buffered onset;
raw RF loss supplies silence and prevents a previous burst's tail from
replaying. Current source/burst/handoff metadata must still match after
rendering. Processing faults or discontinuities cancel admission and require
fresh qualification; they do not open receive. This local failure rule is
separate from ADR 0004's future Hamlib-source noise/open fallback.

### Ownership and matched artifacts

ADRs 0003, 0025, 0026, and 0027 retain single-writer rings, generation adoption,
and control-side preparation/reclamation. No extra raw-input owner, hardware
owner, PCM delay queue, or DSP-building operation is added to audio callbacks.
Standalone link processing stays after the peer ring and before mixing;
Asterisk retains its existing upstream hook without processing twice.

The validated release cohort is
[Radio alpha10](https://github.com/cpeter1207/librptadvradio/releases/tag/v0.1.0-alpha.10),
[USBRadioPlus alpha32](https://github.com/cpeter1207/USBRadioPlus/releases/tag/v0.1.0-alpha32),
and [controller alpha22](https://github.com/cpeter1207/rpt_advanced/releases/tag/v0.1.0-alpha22).
It uses Radio ABI/SONAME 5, SDK native configuration ABI 4/direct callbacks
ABI 6, controller host-services ABI 10, and product descriptor ABI 7/SONAME 2.
Update consumers and artifact checks together under ADR 0040.

## Validation and remaining acceptance

The cohort passed native Debian 13 amd64/arm64 tests, packaging and staged
installation, with the required production source coverage. Controller
[PR 61](https://github.com/cpeter1207/rpt_advanced/pull/61) and SDK
[PRs 47](https://github.com/cpeter1207/USBRadioPlus/pull/47)
and [48](https://github.com/cpeter1207/USBRadioPlus/pull/48) retain that evidence.
Output-meter fixtures include independent post-processing peak measurements.

The 2026-11-07 installation on 524950 selected Kerchunk and enabled -2 dBFS
Peak Limiters as node overrides, preserving configuration and calibration.
Its recorded 128-second settled interval had no new processing failures,
xruns, clipping, late callbacks, or restarts. That bounded passive check is
not representative listening, VAD false-admission/onset/hang/rekey acceptance,
or final hardware/RF-path qualification. Those checks remain necessary for
the intended station and conditions. Future Hamlib audio-source integration,
streaming/IQ selection, and the verified shared-clock adapter remain in their
existing wishlist entries; local VAD does not complete them.
