# ADR 0025: Native media routing assigns conversion to PCM rings

Status: Accepted

Amended 2026-10-01: every telemetry source, including Morse and tone sequences,
is produced outside the transmit worker and streamed through the telemetry PCM
ring. The RPT Advanced implementation status is tracked in `WISHLIST.md`.

## Context

Native-rate radio-port output receives program audio from asynchronous linked
peer networking, telemetry synthesis and decoding, and local radio paths. Those
producers have independent scheduling
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
telemetry from files, speech, Morse, and tones uses a telemetry playout ring
(also called the telemetry-program ring). Even nominally equal capture
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
Telemetry routing follows ADR 0023. Identifiers and local courtesy tones stay
on the local transmitter. A command response goes only to its command source:
local receive replies use the local transmitter, linked-peer replies use only
that peer's egress, and CLI/REST replies stay on the requesting interface.
Response routing is carried explicitly with the prepared telemetry; it is not
inferred from whichever peer happens to be active when playout begins.

The optional rpt_advanced parrot is a separate, explicitly tagged exception to
ordinary telemetry routing. When enabled for a node, the radio worker captures
one mixed burst from processed local receive and every active linked-peer
input, retains at most the first 30 seconds at 48 kHz, and publishes the
preallocated clip to the control owner only after all inputs unkey. Control
measures the retained samples and queues a localized spoken peak/RMS report
followed by the recording through the station-media producer and telemetry
playout ring. Missing speech skips the report, not the recording. A new
receive discards a queued response or interrupts an active one; its remainder
is not replayed. The tagged parrot bus reaches the local transmitter and every
still-connected outbound peer, including the originating peer and monitor or
fallback destinations. It is kept separate from ordinary telemetry, so ADR
0023's source-scoped command/status routing is unchanged. Parrot output is
excluded from receive capture, preventing local recapture. A remote parrot-
enabled node cannot identify this audio as a parrot transmission, so remote
parrot replay loops remain possible.

`parrot_enabled` controls startup/reload state. The DTMF enable and disable
commands change that state live without rebuilding the node generation. Capture
storage is allocated by the station-control owner only when enabled and is
transferred to the radio worker through a bounded SPSC lifecycle queue. On
disable, capture and queued/active parrot playback stop at the next callback
boundary. The worker returns the two bounded capture buffers through a second
SPSC queue; control frees them only after receiving that acknowledgement.
Playback media interrupted this way follows the existing completed-media
handoff and is reclaimed off the callback. Thus the callback neither allocates
nor frees parrot storage.

Implementation status — 2026-10-05: live DTMF parrot controls and the bounded
control/audio-owner buffer lifecycle are implemented. Remote parrot replay
loops remain the documented limitation above.

The dispatcher queues a destination block for local receive, another active
forwarding peer, or a command response explicitly addressed to that destination.
Local transmitter hang is excluded from peer program audio. A destination's
own input alone does not qualify its mix-minus output.

The former all-local telemetry restriction was an intermediate implementation,
not a replacement for ADR 0023. The owner reaffirmed ADR 0023 on 2026-09-17.
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
source conversion and drift correction. The transmit worker does not synthesize
any telemetry; Morse, tone, speech, and decoded file audio arrive only as PCM
from the telemetry playout ring. It adds generated CTCSS or DCS only
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

One station-telemetry audio worker per node generation owns every telemetry
source, because only one announcement source may play at a time. The worker
starts with the node generation, renders Morse and tone sequences, and
owns speech synthesis and sound-file decoding. It opens and reads one source at
a time, pushing bounded canonical-`f32` chunks to the telemetry-program ring as
generation proceeds. Source selection and fallback (file, speech, tone, then
Morse as configured) stay on this worker; receive interruption switches to the
producer-rendered Morse source. The ring performs source-rate conversion and
produces native-rate samples. A bounded, generation-tagged handoff publishes
its consumer to playback; `Playback::Render` reads only available samples and
never waits for or synthesizes for the producer. Producer streams are canceled
and retired away from the audio callback. A failed source falls through to the
next configured source before producing audio; a failure after audio begins
ends that source without restarting its fallback.

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
| Parrot capture completion/recycle queues | Radio-port transmit worker / station-control thread | Station-control thread / radio-port transmit worker |
| Prepared parrot playback queue | Station-control thread | Radio-port transmit worker |

## Consequences

Jitter buffering, decoding, rate recovery, mixing, codec encoding, RF
signaling, station-control events, and hardware signaling have distinct
owners. The Asterisk compatibility ingress mutex cannot delay either radio
audio worker; standalone ingress instead uses ADR 0037's lock-free handoff.
All PCM rings remain
observable for occupancy, shortfall, and rate-adjustment diagnosis. This record
supplements the lock-free audio rule in ADR 0002 and the shared-ring rule in ADR
0003.
