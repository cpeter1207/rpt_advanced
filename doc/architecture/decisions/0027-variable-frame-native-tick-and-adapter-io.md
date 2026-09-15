# ADR 0027: Separate receive and transmit workers with adapter-owned PCM I/O

Status: Accepted

Amended 2026-09-13: the combined native tick is split into input-driven local
receive and output-clocked transmit workers. The current USBRadioPlus migration
implements the independent callback entry points. The local/link/telemetry
inbound-ring topology, shared-clock fast path, and two-owner generational
lifecycle remain pending; none was shipped in USBRadioPlus alpha18.

Native-rate scope is narrowed to 48 kHz by
[ADR 0035](0035-fixed-48khz-native-audio.md). The variable-frame and elapsed-sample
contracts below remain in force.

## Context

The original USBRadioPlus paths processed a fixed 20 ms, 960-frame native block
at 48 kHz. That assumption appeared in native DSP, radio signaling, renderer
workspaces, and Asterisk-facing frame delivery. It did not match the
PortAudio callback contract, which supplies the callback frame count at run
time, and made ASL3 compatibility adapters treat temporary partial device
I/O as an all-or-nothing native block.

The radio-port audio engine must remain deterministic and lock-free while
the retained ASL3 adapter preserves its compatibility interface. The direct
PortAudio/ALSA adapter needs to pass
its callback frame count without imposing a 20 ms core assumption.

## Decision

### Two independently paced workers

The radio-port audio engine has two serial real-time execution owners. A
worker is an execution responsibility, not a requirement to create an extra
operating-system thread: the selected adapter may invoke each worker directly
from its corresponding callback.

1. The **local receive worker** is called when audio input is available. It
   owns DSP squelch, CTCSS/DCS decoding, deemphasis, and local receive audio
   processing. It advances receive DSP and qualification on input samples,
   then writes processed canonical `f32` audio to the **local receive inbound
   PCM ring**. Its input cadence does not wait for playback readiness.
2. The **transmit worker** runs on DAC demand, or the output cadence supplied
   by the selected audio adapter. It consumes the outputs of the local receive
   inbound ring, every connected link's inbound PCM ring, and the telemetry
   playout ring, and mixes them under the existing routing and duplex policy.
   It owns transmit processing, native telemetry generation, PTT timing, and
   transmit oscillator phase. After the program mix, it adds the selected DCS
   or CTCSS signal where the hardware profile requires generated signaling
   (ADR 0033), and writes directly into the adapter-supplied output buffer.
   For PortAudio this is PortAudio's output callback buffer, not an
   intermediate output ring.

The transmit worker operates entirely at the adapter's native stream sample
rate. Its call supplies a `frame_count` in time frames, not bytes or
interleaved sample words, and it fills exactly that many complete output
frames. The receive call independently supplies its available input count;
there is no requirement that receive and transmit counts or callback times
match. Neither worker performs device reads/writes, codec or network I/O.

All source-to-transmit sample-rate conversion and clock-drift correction
belong to the inbound rings, including the local receive and telemetry rings.
The local receive ring corrects capture-to-playback drift **after** receive
DSP; do not also resample raw capture before that worker. The transmit worker
sees only native-rate ring output: it neither resamples the mix nor runs an
independent timer or output clock-recovery loop. Outbound codec conversion and
detector-private analysis decimation are distinct boundaries, not additional
inbound rate converters (ADR 0035).

### Verified shared-clock fast path

An audio adapter that knows ADC and DAC have no relative clock drift may
declare a shared-clock capability at stream setup. A known common disciplined
reference, such as the appliance clock topology, permits this declaration
when the adapter can also supply the corresponding input and output frames
in one bounded call. Equal nominal sample rates or a coincidental zero-ppm
measurement are not proof of a shared clock.

For that mode, the adapter may invoke local receive and then transmit
**back-to-back** in the same callback/readiness turn. Receive publishes the
current processed samples; transmit immediately consumes those samples through
the same local receive inbound-ring interface and fills the supplied output
buffer. Configure that local ring for synchronous unity-rate pass-through:
no adaptive drift correction, unnecessary unity-ratio resampler, startup
prefill, target-occupancy delay, or wait for another callback. The worker split
and local handoff add no buffering latency. This does not claim zero hardware,
device, DSP/filter, or codec latency, nor remove those algorithms' required
history or framing.

This fast path keeps the receive and transmit DSP owners separate and runs
only on adapter output demand, not an independent timer. A paired call uses
one coherent runtime generation under ADR 0026. A local ring still has exactly
one producer and one consumer even when they run on the same thread; do not
run paired and independent callbacks concurrently against the same handles.
If the timing relationship is unknown or input delivery requires elastic
buffering, retain the asynchronous mode rather than claiming zero added
latency. Losing an established clock guarantee requires safe recovery or a
controlled mode handoff, not callback-time ring reconfiguration.

Only the proven synchronous local path gets this mode. Connected-link and
telemetry rings continue their source-specific conversion, buffering, and
clock recovery where needed. Common appliance clocks do not discipline a
remote network peer or an asynchronously prepared telemetry source.

### Stream setup and worker state

The native sample rate and normalized PCM layout are stream setup properties.
They cannot change while a stream is open. Changing either requires closing
the current stream and completing the controlled radio-device handoff defined
by ADR 0026 before opening a replacement stream. Current CM119 adapters use
their established native layout; adapters normalize hardware-specific channel
mapping before calling the core. ADR 0029 defines canonical `f32` PCM and
limits sample-format conversion to the Asterisk and physical-hardware
boundaries.

At stream setup, an adapter declares finite maximum receive and transmit frame
counts. The core preallocates each worker's renderer, DSP, FFmpeg, RNNoise,
ring-converter, and signaling workspaces for its maximum. Valid worker calls
have a count in the closed range `1..maximum`; neither worker allocates,
resizes, or reconfigures state. An adapter splits an oversized host block
before calling the appropriate worker or rejects it with an atomic fault
counter and requests non-real-time recovery. Rejected input is counted as
dropped; rejected output demand still receives a complete RF-safe output block.

Before publishing a prepared DSP generation, setup exercises its actual graph
instances with silent native blocks and discards the output. It retains warmed
workspaces and silent filter history, without running either live worker,
consuming program audio, publishing RF events, or counting warmup as live measurements.
Callback duration, excessive start gaps, and xrun notification timestamps are
published through lock-free diagnostics; callbacks do not log those events.

Receive DSP and detector state are private to the receive worker; mixer, PTT,
and encoder state are private to the transmit worker. Receive qualification
and already-prepared hardware/transmit intent cross through bounded lock-free
snapshots or generation-tagged handoffs, never shared mutable DSP state.
Qualification timing remains associated with the buffered receive audio
through ring conversion, rather than applying the latest decoder state to
older queued samples. Each worker has its own hazard protection and RF-event
publisher under ADR 0026.

All timing is elapsed-audio-time based. Partitioning one PCM sequence into
different valid frame counts must preserve its DSP and signaling behavior,
apart from bounded numeric rounding. Existing configuration values expressed
in 20 ms frame units retain their wall-clock meaning; implementation converts
them to elapsed samples or time on the responsible worker's clock rather than
decrementing once per callback. Capture shortfall cannot block transmit output;
ring shortfall/drop policy remains bounded, RF-safe, and observable. Loss of
capture must not leave stale qualification holding PTT indefinitely.

### Adapter responsibilities

The single ASL3 compatibility implementation under ADR 0028 owns Asterisk PCM
representation conversion, fixed 20 ms frame assembly, and lock-free delivery
queues. Inbound rate conversion uses the source's PCM ring; outbound Asterisk
rate conversion remains at egress. Adapters may aggregate or split device I/O
and retain partial output independently of either worker. Neither worker
depends on Asterisk types, locks, or frame timing.

The direct PortAudio/ALSA adapter is the hardware callback implementation. It
opens `paFloat32` input and output callbacks. Input calls the local receive
worker directly; output calls the transmit worker with its bounded frame count
and the actual PortAudio output buffer. The buffer is borrowed only for that
call and is completely initialized before return, including silence for any
unfilled source contribution. This makes the core-side PCM handoff
sample-format-conversion-free; PortAudio/ALSA converts at the physical device
boundary. All supported CM119 paths use this adapter under ADRs 0005/0028.
An adapter with the verified shared-clock capability may instead use a
full-duplex callback that invokes the two workers back-to-back as above.

Adapters never spin or block the audio engine while handling a partial output
write. Only a backend with partial writes uses a preallocated staging queue,
sized from the device's reported latency or queue depth when available. If no usable capacity is
reported, the fallback capacity is two maximum native blocks. A partially
submitted block is never truncated. On sustained congestion, the adapter
discards the oldest complete, unsubmitted output blocks until its derived
target is restored, minimizing latency while making every discard and
congestion condition observable. This is device-submission staging, not a
second clock/rate-matching stage; the PortAudio callback path has no such
output queue. Actual device I/O errors retain their normal safe recovery path.

### Publication cadence

COR, CTCSS, DCS, PTT, and GPIO edge notifications publish immediately after
the receive or transmit call that detects them. Each publisher has its own
bounded SPSC event queue to the control owner. Meter, FIFO, and periodic status
snapshots use a separately configured per-node interval with a global fallback;
the default is 50 ms. Each worker accumulates its elapsed samples and publishes
its own fields on the first call ending at or after its deadline. Control
combines timestamped snapshots; it must not mistake independent receive and
transmit publications for simultaneous observations.

## Consequences

The former combined native-tick contract becomes separate receive and
transmit entry points. Introduce them through the established versioned ABI
and compatibility policy; do not silently reinterpret a released combined
callback or its buffer contract. Asterisk compatibility remains in adapters.

Tests must prove frame-partition invariance, maximum-frame preallocation,
oversized-frame recovery, partial input/output assembly, staged-output
congestion and oldest-block discard behavior where applicable, elapsed-time
signaling, and periodic publication rounding. Also prove independent RX/TX
progress with unequal block sizes and drifting clocks; all local/link/telemetry
inputs rendered through their sole inbound converter; native-rate direct
output with correct post-mix signaling; no duplicated or missing output span;
and safe reload/unload with both workers active. No test may require either
worker to lock, allocate, or wait for the other worker or control plane.

Shared-clock tests additionally prove receive-before-transmit ordering,
same-cycle consumption without a ring-added sample/block delay, no correction
or redundant unity converter, and coherent generation use across a paired
call. Account separately for device and required DSP latency. Verify unknown
clock topology selects asynchronous recovery and that peer/telemetry recovery
is unaffected by a synchronous local path.
