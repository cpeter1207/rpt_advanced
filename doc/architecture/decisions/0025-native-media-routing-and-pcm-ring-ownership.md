# ADR 0025: Native media routing assigns conversion to PCM rings

Status: Accepted

Amended 2026-09-13 for the receive/transmit split in ADR 0027. The current
USBRadioPlus migration implements the callback split; this record's complete
local/link/telemetry ring ownership remains pending.

## Context

Native-rate radio-port output receives program audio from asynchronous linked
peer networking, speech synthesis, sound-file preparation, native telemetry
generation, and local radio paths. Those producers have independent scheduling
and may use different sample rates. The design needs one explicit owner for
rate conversion and clock-drift recovery without putting codec, network, file,
or speech work in either radio audio worker.

## Decision

The shared rate-adjusting PCM ring is the sole owner of inbound sample-rate
conversion and clock-drift correction between asynchronously scheduled PCM
program producers and the transmit worker. The input-driven local receive
worker performs DSP squelch, CTCSS/DCS decode, deemphasis, and local receive
processing, then writes its processed audio to a dedicated local receive
inbound PCM ring. Each linked peer has a separate inbound ring, and station
telemetry prepared by speech or sound-file playback uses a telemetry playout
ring (also called the telemetry-program ring). Even nominally equal capture
and playback rates use that local ring for independent-clock recovery, not a
second raw-capture converter. Internal PCM is canonical normalized `f32`;
codec/Asterisk representation conversion remains at the boundary under ADR
0029. Outbound codec-rate conversion is distinct from inbound conversion.

The local receive inbound ring is also the sole squelch-delay line. In
independent-clock mode, its normal reserve accounts for unavoidable
receive-to-transmit delay; the configured squelch-delay value adds reserve to
that same ring rather than creating another buffer. The receive worker
publishes processed PCM with sample-associated effective COS/CTCSS
qualification. When the transmit worker reads delayed local PCM, it mutes
samples until the configured COS and/or CTCSS requirements are asserted for
those samples. Once qualification falls, the delayed tail is therefore muted
before it reaches either the local transmitter or the program-audio loopback
for connected peers. This removes the available portion of the receiver's
squelch crash; additional configured delay permits removal of the remaining
crash without a separate output-stage gate.

Each linked-peer inbound PCM ring is also a DTMF-muting delay line. It uses the
same configured delay as the local squelch-delay line, not a separate queue or
timer. When DTMF is detected for that peer, its ring output is muted
immediately under ADR 0023's existing command-lifetime policy. The delay gives
the detector time to gate the buffered leading samples before they reach local
or peer routing. A delay that is too short may still permit an initial few
milliseconds of DTMF before the gate takes effect.

An adapter-proven shared ADC/DAC clock permits ADR 0027's back-to-back
receive/transmit fast path and removes the need for adaptive local drift
correction. It does not bypass the local ring: its target reserve is exactly
zero plus the configured squelch-delay value, so a zero delay imposes no
additional local-ring delay while retaining output gating. This retains the
same routing/ownership graph; independent link and telemetry rings still
perform their necessary conversion and recovery.

The radio-port transmit worker writes one native-rate peer-audio PCM block to
the **program-audio loopback ring** before local CTCSS or DCS generation. The
**link-audio dispatcher** is the sole consumer of that ring. It fans each block
into a transmit-program queue for every connected peer. A **linked-peer
transmit worker** serially owns that peer's codec encoding and packet send.
Encoding and network transmission never occur in either radio audio worker.
The split preserves existing duplex, source qualification, mix-minus, and
per-link routing; it must not reflect a peer's own audio back to that peer or
send locally generated CTCSS/DCS onto network links.
All telemetry is local-transmitter-only. The dispatcher queues a destination
block only for local receive or another active forwarding peer. Local courtesy
tones and transmitter hang are likewise excluded from peer program audio. A
destination's own input alone does not qualify its mix-minus output.
Logical linked-peer workers may run on a common worker pool, but no two receive
or transmit jobs for the same peer run at once.

Network I/O is asynchronous. In the Asterisk-hosted compatibility adapter,
for each received peer packet, that peer's
**linked-peer receive worker** serializes concurrent callback entry with an
ingress mutex, inserts the packet into the peer jitter buffer, and, when
playout is available, decodes it and writes the decoded samples to that peer's
receive-program rate-adjusting PCM ring. The mutex exists only to maintain the
ring's conceptual single producer. Neither radio audio worker takes it.

For standalone operation, ADR 0037 replaces this mutex with bounded SPSC
packet queues from each ingress producer to the peer's assigned media worker.
That worker alone owns the peer jitter buffer, decoder, and PCM-ring writer.
No producer writes the PCM ring directly and no thread per peer is required.

The DAC/adapter-output-clocked transmit worker calls a bounded radio-program
mixer that consumes the requested native frame count from the local receive
inbound ring, each linked-peer inbound ring, and the telemetry playout ring.
All of those rings render at the same native output rate, owning their own
source conversion and drift correction. Tones and Morse may be generated
directly at native rate in the transmit worker. It adds generated CTCSS or DCS only
when the selected radio capability profile requires an analog generated signal,
then writes directly to the supplied adapter output buffer (PortAudio's output
buffer for that adapter). There is no transmit-mix resampler or additional
output clock-recovery ring. The adapter converts only at its physical hardware
boundary. ADR 0027 assigns hardware submission and any backend-specific
partial-I/O staging to that adapter. ADR 0033 defines the
appliance direct-codec profile, which uses external discrete CTCSS and therefore
does not inject a native CTCSS tone.

A per-node active-traffic CTCSS policy may independently restrict CTCSS encode
and decode. When enabled, a live local-receiver or connected-peer transmission
qualifies the policy; hangtime alone does not. Identifiers and courtesy tones
never qualify it. Command-response telemetry qualifies it from command receipt
through response playout, including any deferred interval before that playout.
This is source-aware signaling policy, not an inference from physical PTT
alone, and it applies equally to native generated CTCSS and profile-selected
external CTCSS enable.

Exactly one reserved station-telemetry audio worker owns speech synthesis
and sound-file playout, because only one announcement source may play at a
time. It writes decoded source samples at their original rate to the
telemetry-program ring after converting its source representation to canonical
`f32`; that ring produces the native-rate samples used by the mixer.

Each audio worker has an **RF-signaling edge publisher** for its owned state:
receive qualification/decoder status in local receive, and physical PTT and
transmit signaling in transmit. Publishers perform only immediate prepared
RF actions, atomic snapshots, and fixed-size prompts to their own bounded SPSC
event queues; they never share a producer handle. The **station-control event dispatcher** is
part of the station-control thread and consumes those prompts. It owns
telemetry decisions, scheduling consequences, macros, topology, logging, and
link-control operations. It never runs in either radio audio worker.

Queue ownership is fixed:

| Queue or ring | Producer | Consumer |
| --- | --- | --- |
| Local receive inbound PCM ring | Local receive worker | Radio-port transmit worker |
| Linked-peer inbound / receive-program ring | That peer's receive worker | Radio-port transmit worker |
| Telemetry playout / telemetry-program ring | Station-telemetry audio worker | Radio-port transmit worker |
| Program-audio loopback ring | Radio-port transmit worker | Link-audio dispatcher |
| Linked-peer transmit-program queue | Link-audio dispatcher | That peer's transmit worker |
| Receive RF-signaling event queue | Receive edge publisher | Station-control event dispatcher |
| Transmit RF-signaling event queue | Transmit edge publisher | Station-control event dispatcher |

## Consequences

Jitter buffering, decoding, rate recovery, mixing, codec encoding, RF
signaling, station-control events, and hardware signaling have distinct
owners. The Asterisk compatibility ingress mutex cannot delay either radio
audio worker; standalone ingress instead uses ADR 0037's lock-free handoff.
All PCM rings remain
observable for occupancy, shortfall, and rate-adjustment diagnosis. This record
supplements the lock-free audio rule in ADR 0002 and the shared-ring rule in ADR
0003.
