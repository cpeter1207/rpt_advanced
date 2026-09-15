# ADR 0039: Retire USBRadioPlus native mode and native parrot

Status: Accepted — candidate implementation under verification (2026-09-13)

## Context

USBRadioPlus provides a driver-owned native-rate software local-repeat mode
and native parrot recording/playback. Neither is needed by rpt_advanced,
which owns its controller routing and uses the shared radio/audio components.
The product owner has ended future ASL3 support for these modes.

The word "native" also describes ordinary 48 kHz DSP, callback rendering,
diagnostics, and the `RadioPlusAdvanced` controller transport. Those are
distinct from the operating modes being retired.

## Decision

Remove USBRadioPlus's native software local-repeat mode, including native
parrot. Do not retain either as an optional ASL3 feature, hidden setting, or
rpt_advanced prerequisite. In the current implementation, software local
repeat is selected by `duplex_local_repeat_mode=software`; native parrot uses
that route with echo enabled. There is no separate `native_mode` config key.

The implementation must remove mode-specific routing, recording/playback
storage and state, admission/PTT special cases, configuration selections,
tuning controls, diagnostics, and tests that exist solely to support those
modes. Audit consumers before removing shared component functions; do not
remove reusable radio DSP merely because its identifiers contain `native`.

Preserve these separate capabilities:

- The ordinary ASL3/app_rpt 8 kHz channel interface and controller-owned
  routing, and its legacy echo path rather than driver-native parrot.
- Hardware local-repeat support; it is not the native software-repeat mode.
- The shared 48 kHz DSP, squelch, CTCSS/DCS, emphasis, processing, PCM rings,
  audio adapters, and useful callback/audio statistics.
- rpt_advanced's native audio engine, including ADR 0027's separate receive
  and transmit workers and verified shared-clock fast path.
- The currently separate `RadioPlusAdvanced` transport until its own adapter
  migration. Its native format does not enable USBRadioPlus local repeat or
  parrot, and this decision does not retire that transport or change its ABI.

Silently ignore `duplexmode` and the retired `duplex_local_repeat_mode`
selection, regardless of their values. They must not select software repeat,
native parrot, or disable configured hardware repeating. This specific
explicit rule supersedes the general unknown-setting warning policy.
`duplex3` (the configured `duplex_local_repeat_level`) controls hardware local
repeat only, as with the other ASL3 channel drivers. Preserve its existing
level, device support, controller-transport restrictions, and supported tuning
behavior. Apply the same rule on initial load, reload, and tuning persistence;
do not restore a mode selector when saving settings.
Update examples, manuals, tuning help, release notes, and migration tests in
the implementation release, not as a claim that alpha18 has already changed.

Under ADR 0040, backward compatibility with earlier initial-alpha artifacts is
not required. Remove shared native-repeat/parrot operations with no remaining
current consumer instead of preserving compatibility-only descriptor slots or
implementation. Update current consumers and version/package metadata together
so incompatible artifacts are rejected safely. Do not change unrelated DSP.

## Consequences and verification

USBRadioPlus no longer duplicates these controller features for ASL3.
rpt_advanced and the standalone appliance do not depend on their presence.
The appliance's native PCM, clocks, hardware signaling, and safety boundaries
are unchanged; "native mode removed" does not mean native DSP removed.

The implementation gate must show that obsolete mode selections are silently
ignored, `duplex3` remains hardware-only, no native-parrot allocation/producer remains in
USBRadioPlus, and ordinary app_rpt audio, legacy echo, hardware local repeat,
signaling, diagnostics, reload, and rpt_advanced transport still work. Retain
the current external interoperability, packaging, and full quality requirements.

The USBRadioPlus and shared-library candidate sources implement the removal;
integration and release verification remain in progress. Released alpha18,
installed configurations, and running nodes remain unchanged. A candidate test
does not authorize deployment or replace the full release gate.
