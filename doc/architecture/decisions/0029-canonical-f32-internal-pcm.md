# ADR 0029: Canonical f32 internal PCM

Status: Implemented (current radio and controller PCM interfaces)

## Context

Before migration, USBRadioPlus carried signed 16-bit PCM at several internal
boundaries while individual DSP stages used a mixture of floating-point working
formats. That tied internal precision to the CM119 and Asterisk sample
format and would discard precision before a future 24-bit audio interface can
use it.

The controller, shared radio core, rate-adjusting PCM rings, and adapters need
one explicit representation that does not make hardware or Asterisk formats
part of their contracts.

## Decision

All internal PCM interfaces use interleaved IEEE-754 binary32 (`f32`) samples.
The full-scale reference range is `-1.0` through `+1.0`. This applies to radio
core input/output (the separate local receive and transmit workers under
ADR 0027), DSP and mixer interfaces, rate-adjusting
PCM rings, link-media handoffs, telemetry handoffs, and program-audio
loopback. It does not change stream sample-rate ownership or channel-layout
rules.

Asterisk and hardware are the only PCM-format boundaries. Their adapters
perform explicit conversion between their negotiated or device-native format
and canonical `f32`, with clipping and quantization confined to those
boundaries. Current Asterisk signed-linear frames therefore convert at the
Asterisk adapter boundary. A PortAudio/ALSA adapter opens its callback stream
as `paFloat32`, so its core-side callback exchanges canonical `f32` directly;
PortAudio/ALSA performs any CM119 S16 or future hardware-format conversion
below that callback. Channel mapping remains adapter-owned and is not a sample
format conversion.

The current composition uses the released canonical-`f32`
`rate_adjusting_pcm_ring` ABI 3 and SONAME 3 described in ADR 0003.
Initial-alpha compatibility follows ADR 0040: mismatched descriptors and
packages fail safely rather than preserving the retired signed-16 ABI.
No new internal signed-16 PCM interface may be introduced.

## Consequences

New internal media code receives a format independent of current S16 CM119
hardware and can preserve the precision of future 24-bit devices. External
adapters remain responsible for their own format conversion, while the core
and rings remain independent of Asterisk and device sample representation.

Changes to a published shared-ring ABI require a released package, consumer
rebuilds, ABI tests, and documented SONAME compatibility handling.
Tests for every boundary must prove correct
full-scale conversion, channel mapping, and the absence of an avoidable
conversion in a `paFloat32` PortAudio callback.

## Implementation status — 2026-10-16

Radio and controller workers, local/peer/telemetry ring clients, and adapter
callback interfaces use `f32`. The product validates the ring's ABI 3 and
`rptadv.rate-adjusting-pcm-ring.f32` capability before creating a ring; Debian
dependencies require the matching runtime and development packages. Asterisk
frame and hardware representations remain adapter-boundary conversions.
This does not mark ADR 0027's optional shared-clock adapter as implemented.
