# ADR 0004: Hamlib remote-base boundary

Status: In progress; decision accepted (shared-control design approved 2026-11-04)

## Context

Remote-base operation needs common control of many amateur radios while their
audio interfaces vary among external CM119 devices, onboard USB sound devices,
and capable streaming-radio back ends.

## Decision

Use Hamlib through its versioned external-dependency adapter as the primary
radio-control implementation. Replace each legacy USBRadioPlus radio-control
feature with its Hamlib equivalent when one exists; retain only capabilities
Hamlib cannot provide. Configure one rig per node, support every Hamlib control
transport, and select its model by a canonical enumerated model name rather
than a numeric Hamlib model ID. Additional radios use additional nodes.

Remote-base configuration uses inheritable `[remote_base]` and
`[remote_base node]` sections. `hamlib_model` is a canonical model name,
`audio_source` selects `cm119`, `alsa`, or `hamlib_stream`, and
`hamlib_option_*` supplies named backend options. Audio selection makes a best
effort to use CM119 for conventional radios, Linux-supported onboard USB audio
when available, and a supported streaming API when the radio provides one.
COS, CTCSS, and DCS remain USBRadioPlus DSP or hardware-signaling
responsibilities unless a practical Hamlib replacement exists.

Decoder programming is separate from tone qualification. Generic Hamlib can
set receive codes but does not provide independent CTCSS/DCS-present status.
Use the configured DSP or external GPIO qualifier. An explicit
`decode_control_source = hamlib` may program the radio while GPIO provides
its qualified signal; otherwise programming follows the selected decoder.
Never infer decoded tone from configured code, TSQL/CSQL enable, raw DCD, or
audio level. Unsupported qualification is rejected without source fallback.

On the appliance, a direct-codec radio port is the conventional-radio audio
source and its adjacent USB-C CAT port carries native USB CAT or a USB-to-serial
adapter. Hamlib network transports use the appliance Ethernet or Wi-Fi path.
This appliance mapping does not change the portable `cm119`, `alsa`, and
`hamlib_stream` source categories.

Hamlib is reached only through the radio-control adapter. GPIO-sourced radio
signals and site I/O belong to the GPIO adapter, while PCM and raw audio
statistics belong to the PortAudio/ALSA audio adapter. ADR 0028 defines the
resulting removal of `res_usbradio` ownership and the remaining signal-source
selection decisions.

Each node configures permitted receive and transmit frequency ranges, modes,
transmit-power limits, and receive-only ranges. A request outside those limits
is rejected. Non-FM operation keeps controller transmit admission independent
of receive qualification so a linked peer can key the transmitter when RF
squelch is unreliable. A physically half-duplex radio interrupts receive
while transmitting; simultaneous RF receive/transmit is not required. This
corrects the earlier conflation of controller duplex operation with physical
radio duplex capability.

AM and single-sideband speech squelch will reuse the shared radio's Earshot
VAD with 16 kHz analysis. The local receiver implementation supports Off,
Kerchunk filter and On; connecting future Hamlib sources remains planned.
A configurable fixed SSB passband in the shared FFmpeg graph is
the universal fallback; capable I/Q sources may add adaptive
pre-demodulation selection. If speech detection is unavailable, fall back to
conventional noise squelch; if that is unavailable, leave receive open.

Remote-base control is available to authenticated REST users and authorized
Asterisk CLI users. Per-node DTMF remote-base control is open,
unlock-code-protected, or disabled. Its default configurable command map
extends familiar AllStarLink commands with frequency, mode, receive/transmit
VFO, split/offset, transmit power, CTCSS/DCS, remote-base status, and scan
operations. Every supported remote-base operating function has a DTMF
representation; none is restricted to CLI/REST for implementation convenience.
The default remappable `*6` family extends familiar ASL remote commands without
changing existing link commands or `*722`.

Use one versioned remote-base shared library for both standalone and
USBRadioPlus. It owns limits, tune transactions, memories, and scanning;
Hamlib remains a separate dependency adapter. Preserve existing parallel-port
channel selection and RTX programming through the same physical GPIO owner.
Only Asterisk channel/framing and other genuinely Asterisk-specific functions
are exempt from standalone parity.

Named frequency/mode/tone memories and scan lists are configuration-only.
DTMF recalls/queries memories and operates scans but does not edit or save
their definitions. Scan dwell defaults to 500 ms after tune/settle, resume to
2,000 ms after activity ends, and priority scanning to disabled. Scan activity
holds the current member. Transmit uses only its fully applied, permitted
state; stop retains that member, and restart selects the configured startup
memory rather than resuming a live scan. Rig I/O is serialized per physical
owner outside audio callbacks; one blocked rig cannot block other rigs.

Activation means the physical worker has admitted the rig's capabilities and
committed its initial DSP state. Spawning a worker does not establish readiness.
Reload retains the candidate, previous configuration and callback owners while
the existing control pump polls activation. It publishes the new generation
only after every candidate is ready. Failure triggers checked retirement and
asynchronous restoration of the old generation. Failed restoration leaves that
node RF-inhibited and retains the original diagnostic. Other radios and peer
service continue while the transaction waits. Both hosts retain their staged
configuration until the shared product reports completion or safe rollback.

The approved [design](../../../docs/superpowers/specs/2026-11-04-hamlib-remote-base-design.md)
and [implementation plan](../../../docs/superpowers/plans/2026-11-04-hamlib-remote-base.md)
specify the contract, DTMF, safety, and validation details.

## Implementation status

The candidate implements the shared control engine, separate Hamlib adapter,
retained parallel backends, configuration-only memories and scan lists, complete
remote-base DTMF operations, independent signaling, and asynchronous activation
and rollback in both radio hosts. Each Hamlib context retains its own copied
capability record through native cleanup so transport options cannot change
another rig's model defaults. Native layouts remain private and are generated
from the installed headers.

The [remote engine PR](https://github.com/cpeter1207/librptadvremote/pull/1) and
[Hamlib adapter PR](https://github.com/cpeter1207/rptadv-hamlib-adapter/pull/1)
are merged and released after passing the full native Debian 13 gates, including
production line and branch coverage. Both providers have signed Debian packages.
The shared [radio SDK alpha28](https://github.com/cpeter1207/USBRadioPlus/releases/tag/v0.1.0-alpha28)
also passed its full native gates and has signed Debian packages. Focused
controller tests cover readiness, retained generations, rollback,
source-specific replies, and the shared physical owner. Controller integration
requires its full native PR gate before merge and release. Physical-radio
qualification remains model-specific. No node has been changed for this
implementation.

Current audio paths are CM119 and general PortAudio/ALSA. Streaming-radio
audio, human-speech squelch, and adaptive I/Q selection remain planned. REST and
the full CLI follow ADR 0023; this work does not implement those interfaces.

## Consequences

Rig control and audio selection are separate configuration concerns. A radio
model that Hamlib controls but that lacks a usable configured audio source
cannot become an operational remote base. The accepted model-name catalog must
be validated against the installed Hamlib version. DTMF authentication and
command-map rules are defined by ADR 0023.
