# ADR 0004: Hamlib remote-base boundary

Status: Accepted

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
is rejected. A non-FM mode on a single-sideband-capable radio requires full
duplex so a linked peer can key the transmitter when RF squelch is unreliable.
Reject the request when the selected radio and audio path cannot provide full
duplex.

AM and single-sideband speech squelch uses stateful 16 kHz Silero VAD through
ONNX Runtime. A configurable fixed SSB passband in the shared FFmpeg graph is
the universal fallback; capable I/Q sources may add adaptive
pre-demodulation selection. If speech detection is unavailable, fall back to
conventional noise squelch; if that is unavailable, leave receive open.

Remote-base control is available to authenticated REST users and authorized
Asterisk CLI users. Per-node DTMF remote-base control is open,
unlock-code-protected, or disabled. Its default configurable command map
extends familiar AllStarLink commands with frequency, mode, receive/transmit
VFO, split/offset, transmit power, CTCSS/DCS, remote-base status, and scan
operations. Named frequency/mode/tone memories and named scan lists are
required remote-base capabilities; their detailed scan and persistence policy
remains separately configurable.

## Consequences

Rig control and audio selection are separate configuration concerns. A radio
model that Hamlib controls but that lacks a usable configured audio source
cannot become an operational remote base. The accepted model-name catalog must
be validated against the installed Hamlib version. DTMF authentication and
command-map rules are defined by ADR 0023.
