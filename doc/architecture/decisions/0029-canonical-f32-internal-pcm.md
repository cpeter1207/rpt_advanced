# ADR 0029: Canonical f32 internal PCM

Status: Accepted

## Context

Current USBRadioPlus code carries signed 16-bit PCM at several internal
boundaries while individual DSP stages use a mixture of floating-point working
formats. That ties internal precision to today's CM119 and Asterisk sample
format and would discard precision before a future 24-bit audio interface can
use it.

The controller, shared radio core, rate-adjusting PCM rings, and adapters need
one explicit representation that does not make hardware or Asterisk formats
part of their contracts.

## Decision

All internal PCM interfaces use interleaved IEEE-754 binary32 (`f32`) samples.
The full-scale reference range is `-1.0` through `+1.0`. This applies to radio
core input/output (the separate local receive and transmit workers under the
pending ADR 0027 amendment), DSP and mixer interfaces, rate-adjusting
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

The released `rate_adjusting_pcm_ring` signed-16 ABI remains compatible for
existing consumers during migration. A canonical-`f32` ring ABI requires a
new compatible public API and SONAME transition before a consumer can claim
full compliance with this record. It must not silently reinterpret existing
signed-16 buffers as `f32`.

Existing components migrate at their normal versioned boundaries. Until a
component has migrated, its signed-16 internal path is a documented
transitional exception, not a new canonical interface. No new internal
signed-16 PCM interface may be introduced.

## Consequences

New internal media code receives a format independent of current S16 CM119
hardware and can preserve the precision of future 24-bit devices. External
adapters remain responsible for their own format conversion, while the core
and rings remain independent of Asterisk and device sample representation.

The shared-ring migration changes a published ABI and therefore requires its
own released package, consumer rebuilds, ABI tests, and documented SONAME
compatibility handling. Tests for every migrated boundary must prove correct
full-scale conversion, channel mapping, and the absence of an avoidable
conversion in a `paFloat32` PortAudio callback.
