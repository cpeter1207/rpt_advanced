# ADR 0003: Shared rate-adjusting PCM ring

Status: Accepted

## Context

Independent program-audio and radio-hardware clocks drift. Duplicate
elastic-buffer implementations in rpt_advanced and USBRadioPlus caused
unnecessary divergence.

## Decision

Use the separately released `rate_adjusting_pcm_ring` shared library for
lock-free PCM program buffering, controlled rate recovery, and shortfall
observability. rpt_advanced and USBRadioPlus link against the same public ABI.
All newly migrated ring payloads use canonical normalized `f32` PCM under
[ADR 0029](0029-canonical-f32-internal-pcm.md). The released signed-16 ABI
remains a transitional compatibility interface until its separately released
`f32` successor and SONAME migration are complete.

The ring is the sole owner of inbound sample-rate conversion and clock-drift
correction whenever asynchronously scheduled or short-lived PCM producers feed
native-rate transmit consumption. Under the accepted 2026-09-13 split in ADR
0027, the current USBRadioPlus migration implements the independent callback
entry points, while the pending ring-ownership tranche has the local receive
worker write processed audio
after squelch, CTCSS/DCS decode, deemphasis, and receive DSP to a **local receive
inbound PCM ring**. This ring owns capture-to-playback drift correction; no
additional converter precedes receive DSP. Each connected peer has a
**receive-program ring** after its receive worker has passed packets through its
jitter buffer and decoder. The ring converts decoded peer program audio to the
node's native radio-port rate. One serialized **station-telemetry audio
worker** writes speech and sound-file samples at their source rate to a
**telemetry-program ring**, which likewise produces native-rate audio for
the native transmit mixer. The telemetry-program ring is also called the
telemetry playout ring; these names do not describe two buffering stages.

Ring ownership is fixed and single-producer/single-consumer:

| Ring | Producer | Consumer |
| --- | --- | --- |
| Local receive inbound PCM ring | Local receive worker | Radio-port transmit worker |
| Linked-peer inbound / receive-program ring | That peer's serialized receive worker | Radio-port transmit worker |
| Telemetry playout / telemetry-program ring | Station-telemetry audio worker | Radio-port transmit worker |
| Program-audio loopback ring | Radio-port transmit worker | Link-audio dispatcher |

The link-audio dispatcher is outside the rate-adjusting-ring boundary. It
fans the program-audio loopback block into per-peer transmit-program queues;
the corresponding linked-peer transmit workers encode and send those queues.
Neither radio-port worker waits for peer transmission work or manages
peer-specific block ownership.

Network jitter buffering happens before decode and is not a replacement for
rate recovery. Native-rate Morse and tone generation within the radio-port
transmit worker does not need a ring. The ring never owns codec packetization,
jitter policy, RF signaling, or hardware I/O.

The DAC/adapter-clocked transmit worker requests a setup-bounded native frame count.
Its ring consumers render exactly that count while retaining their existing
clock-recovery state; they do not impose a 20 ms block size. ADR 0027 defines
the workers and adapter I/O contract. Ring outputs are already at the native
transmit rate; the transmit mix and direct adapter output add no resampling or
drift-correction stage. Outbound codec conversion and detector-private analysis
decimation remain distinct under ADR 0035.

If the adapter declares a verified common ADC/DAC clock and aligned delivery
under ADR 0027, the local receive ring supports synchronous unity-rate
pass-through. Receive and transmit can run back-to-back and exchange the same
call's processed samples without adaptive correction, a redundant converter,
startup prefill, or occupancy-induced latency. The ring keeps its named
producer/consumer ownership; no second handoff is introduced. This is not the
default for unknown clock relationships and does not disable independent
peer/telemetry ring recovery or remove inherent device/DSP latency.

## Consequences

Consumers must declare and install the shared-library dependency. Changes to
the ring's public ABI require coordinated consumer builds and releases.
Local receive inbound, linked-peer receive-program, telemetry-program, and
program-audio loopback rings require observable occupancy, shortfall, and
clock-recovery statistics.
