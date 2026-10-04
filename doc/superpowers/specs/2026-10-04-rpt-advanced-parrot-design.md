# RPT Advanced parrot with level report

Status: Design approved by the product owner on 2026-10-04; awaiting written
spec review before implementation planning.

## Purpose and scope

Add an optional RPT Advanced parrot that records one mixed burst of processed
local-receiver and connected-peer audio, announces its level, and replays the
recording. This is RPT Advanced controller behavior. Do not restore the
retired USBRadioPlus native parrot or change USBRadioPlus behavior.

This specification covers capture, statistics, localized speech preparation,
serialized replay, and fanout to the local transmitter and connected peers.
It does not add operator commands, a configurable recording duration, or a
new link protocol.

## Approved behavior

- `parrot_enabled` is a per-node setting with `[general]` inheritance and a
  default of `no`.
- Start one mixed recording while any local receiver or connected peer is
  receive-active. Mix the processed local PCM and all receive-active peer PCM
  after their normal inbound processing. The recording is mono normalized
  F32 at the controller's fixed 48 kHz native rate.
- When every input source unkeys, queue one response. A peer disconnect is
  equivalent to that peer unkeying. Retain the first 30 seconds; continue
  tracking the burst until all sources unkey. Calculate peak and RMS over the
  retained samples only, in dBFS, rounded to the nearest integer.
- Play an i18n-backed spoken peak/RMS report followed by the retained audio.
  Use the node's resolved speech voice, speed, and level. If speech cannot be
  synthesized, play the recording alone.
- Send the response to the local transmitter and every still-connected peer
  with an outbound audio channel, including the origin peer. Parrot fanout
  overrides the normal monitor/local-monitor transmit selection for that
  response only. Other telemetry routing remains unchanged.
- Serialize playback through the existing station telemetry producer and
  telemetry PCM ring. The radio transmit worker consumes PCM only. A dedicated
  parrot-audio output plane carries only this response to the link dispatcher;
  ordinary local telemetry remains local/source-scoped under ADR 0023.
- If new receive activity occurs before playback, discard the queued response
  and record the new mixed burst. If new receive activity starts during
  playback, stop immediately, discard its remaining audio, and record the new
  burst. At most one response is active or queued per node.
- Capture storage is preallocated and bounded; callback code performs no
  allocation, speech/file work, formatting, logging, blocking, or locking. A
  bounded handoff transfers completed capture storage to control/media owners.
  If no capture slot is available, discard the new parrot burst rather than
  block or allocate in an audio callback.
- Do not capture the node's own outbound parrot samples. Ordinary link PCM has
  no origin marker, so a different parrot-enabled node may replay this audio;
  that remote-loop limitation is accepted and will be documented.

## Architecture and ownership

The current receive ring outputs are the capture tap: they already contain
normal receive processing and squelch/CTCSS qualification. `LinkAudio` owns
the post-ring local and peer frame mix and supplies a bounded capture buffer
to the radio transmit owner. The controller owns burst boundaries and the
30-second retained prefix. All concurrent active sources contribute to the
same capture; mix output is clamped to normalized F32 before both recording
and level measurement.

At final unkey, audio ownership moves through a bounded lock-free handoff to
the station-control/media path. Statistics, Fluent formatting, and Piper
synthesis run outside radio callbacks. The station telemetry producer streams
the localized report, when available, then the retained samples into the
existing telemetry PCM ring. Its existing serialized playback behavior keeps
parrot media mutually exclusive with other telemetry. New activity cancels
queued/active parrot media; stale media is generation-tagged and cannot play
after reload.

The transmit owner receives an additional preallocated parrot-output slice.
It copies the ring samples classified as parrot while it composes local audio.
`LinkAudio` adds that slice to the common peer program block before per-peer
mix-minus so the response reaches its origin and other connected peers. The
slice is zero for every non-parrot media source. No direct peer queue writes
are introduced in the radio callback.

Allocate two fixed 30-second capture buffers per node generation: one may be
recorded while the other is owned by control/media preparation. This bounds
memory to approximately 11 MiB per node and allows an interrupted response to
be replaced without callback allocation. The no-free-slot policy is the
bounded discard rule above.

## Localization and level reporting

Add one typed Fluent message with the required `.text`, `.tts`, and `.morse`
attributes, following ADR 0041. The TTS form receives integer peak and RMS
dBFS values. English wording is supplied in `messages/en-US.ftl`; catalog
validation and formatting remain on the control plane. The Morse attribute is
catalog completeness only and is not a fallback: the requested response is
spoken when speech is available, and otherwise contains the recording alone.

## Configuration

Add `parrot_enabled` to the existing general/node schema. Resolve the node
value first, then `[general]`, defaulting to `no`; malformed values follow the
existing configuration warning/default policy. Add the option, default, and
meaning to the shipped example and operator documentation before the pull
request. There is no duration option; the retained prefix is fixed at 30
seconds.

## Error and lifecycle behavior

- Speech preparation failure does not suppress the recording.
- A full capture handoff drops the incoming parrot burst and never stalls an
  audio owner.
- A generation change cancels any queued or active parrot response; retired
  capture buffers are reclaimed only on control after their audio owner is
  quiescent.
- Playback is interrupted by receive activity from any captured source; no
  suffix of the prior response is replayed.
- Disconnected peers receive no queued response after disconnect. Remaining
  connected peers still receive fanout.
- Existing IDs, courtesy tones, command replies, and other telemetry keep
  their established serialization, routing, and cancellation policies.

## Verification

Add focused tests for settings inheritance/default, one mixed burst across
overlapping sources, peer disconnect/unkey, the 30-second prefix and continued
burst tracking, exact retained-sample peak/RMS rounding, speech success/failure,
localized arguments, bounded-slot exhaustion, pre-play cancellation,
mid-play cancellation, generation retirement, local-only non-parrot telemetry,
and fanout to all connected outbound peers regardless of monitor mode. Verify
callback paths remain allocation-free and lock-free. Include unequal callback
partitions and test 48 kHz playback through the existing PCM ring without
hardware. Run affected source tests during iteration, then the complete
Debian 13 quality gate before release. Hardware validation is a separate
install/restart test on node 524950 after the full gate.

## Explicitly excluded

- Changes to USBRadioPlus native/parrot code, duplex settings, or audio
  processing.
- A remote parrot-origin protocol marker or a guarantee against replay loops
  across multiple independently configured nodes.
- New DTMF, CLI, REST, or WebSocket controls.
- Configurable recording duration, recording storage, or persistent files.
