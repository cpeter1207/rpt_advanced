# Streaming speech and file telemetry design

## Goal and scope

Decode sound files and synthesize speech away from the radio transmit worker.
Stream canonical mono `f32` source-rate chunks through the released
`rate_adjusting_pcm_ring3` into native 48 kHz playback. `Playback::render`
consumes that ring without waiting. Start feeding the ring as soon as the
decoder or speech engine supplies usable chunks; do not wait for the complete
asset. Morse and tone continue to be generated directly by the transmit worker.

Preserve telemetry sequencing/routing, gain and ducking, cancellation on
interruption, and file-to-speech-to-Morse fallback when a source fails before
it emits PCM. This design does not change USBRadioPlus, configuration syntax,
external telemetry wording, or node deployment.

## Ownership and data flow

One station-telemetry worker per node owns file/speech producer jobs and each
shared-ring producer endpoint. It executes the replaceable file and speech
adapter operations serially. A stream job publishes its source rate first; the
worker creates the shared ring off the audio path and hands its consumer
endpoint to the active `Playback` through a bounded generation-tagged SPSC
handoff. It then streams chunks directly into the ring, splitting writes at the
ring's producer bound and applying backpressure only on the station-telemetry
worker when the ring is full. The worker never holds a lock needed by an audio
callback.

The ring is configured for the source rate and fixed 48 kHz output rate, with
PLC disabled and zero reserve/target: media has no independent input clock, and
playback should begin as soon as converted samples are available. Its bounded
capacity limits producer lead; it is not an intentional playback delay. The
released ring remains the only source-rate converter. The worker supplies the
ring's required end padding and reports source completion after the final
chunk. `Playback::render` reads available converted samples and treats an
empty ring as temporary until the producer reports completion; it reports
finished only after completion and drain.

The station-control owner creates/retains immutable job descriptions and
generation-scoped work handoffs. The station-telemetry worker owns each ring
until both endpoints are stopped; the generation reclaimer destroys it only
after worker and callback quiescence. Audio callbacks neither create rings,
allocate, block, spawn work, nor perform file, process, or speech operations.
Generation tags and the existing quiescence protocol prevent a producer or
callback from touching a replaced generation's ring. Interrupting media
cancels its producer and retains the existing in-transmit-worker Morse fallback
behavior.

## Streaming adapter contract

Replace whole-audio `PreparedAudio` results for file and speech playback with
an adapter-neutral stream contract. The adapter reports a validated source
sample rate before invoking its chunk sink, then sends bounded chronological
normalized `f32` samples and a terminal result. FFmpeg decoding and Piper
synthesis must write chunks as output becomes available. The streaming calls
run only on the station-telemetry worker. File decoding may emit WAV/raw PCM to
a pipe; speech uses Piper's raw streaming output and applies speech level to
each chunk. No FFmpeg or Piper operation occurs in `Playback` or a radio audio
worker.

The separate file and speech adapter descriptors remain separate. Their
streaming contract advances both private media descriptor ABIs to version 2,
and the product descriptor clients advance in the same change; no parallel
whole-file compatibility path is retained. Source format,
finite samples, cancellation, child reaping, subprocess timeout, and error
mapping remain validated at the adapter boundary. The released shared ring ABI
does not change.

If a file fails before its first sample, try speech; if speech fails before its
first sample, use the existing Morse fallback. If a source fails after PCM has
already been emitted, drain the accepted samples, terminate that playback, and
report the producer error; do not restart another source from its beginning
mid-message. An explicit cancellation discards the remaining stream and follows
existing interruption behavior.

## Playback and transmit behavior

`Playback` owns a consumer interface backed by the shared ring for file/speech
media, while retaining direct Morse and tone renderers. Its render call drains
the consumer into caller-provided output and performs no allocation, blocking,
logging, or external calls. Producer-not-ready and temporary ring shortfall
produce no media sample but do not mark EOF. Transmit demand is asserted only
when samples are available, avoiding keying a silent transmitter while a
subprocess initializes; the controller may still retain the selected telemetry
item while the producer prepares its first chunk.

The media worker exposes producer completion/error through generation-owned
atomics or bounded SPSC events. It does not write controller policy or PTT
state. The transmit worker remains the sole owner of media mixing, PTT, tone,
and Morse rendering.

## Validation

Tests must demonstrate:

- file and speech chunks reach `Playback::render` before producer EOF;
- `Playback` reads only from the ring and preserves exact sample ordering,
  rate conversion, gain, and completion after drain;
- empty-before-first-chunk and intermittent empty reads do not falsely finish
  media or key PTT; EOF plus drain finishes exactly once;
- full-ring writes apply bounded backpressure outside the audio path;
- file failure before first output falls through to speech, then Morse;
- speech failure before first output falls through to Morse;
- post-output failure drains already accepted PCM and reports an error without
  replaying the message from its beginning;
- interruption cancels the producer, preserves existing Morse fallback, and
  cannot write into a retired generation;
- adapter callbacks stream incrementally, enforce source format/range, and
  release child processes and ring endpoints on success, failure, timeout, and
  cancellation;
- no audio callback performs allocation, waits, process/file I/O, or media
  preparation.

Run focused media/core/product tests first, then the required full quality gate
before declaring the implementation complete. Update in-source documentation
and the architecture/current-gap documentation with the implementation.
