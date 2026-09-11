# ADR 0011: Stable functional components use narrow versioned shared-library boundaries

Status: Accepted

## Context

rpt_advanced and USBRadioPlus contain stable radio and controller functions
that should not need modification when unrelated behavior changes. The project
already uses a separately released rate-adjusting PCM ring. The same isolation
is required for discrete signal, controller, configuration, and future
standalone-protocol functions.

## Decision

Every extracted functional component has its own source repository, versioned
shared-library ABI, normal SONAME compatibility policy, and Debian runtime and
development packages. A consumer declares a minimum compatible ABI rather than
an exact build. This applies to the existing rate-adjusting PCM ring and every
future extracted library.

`librptadvradio` remains the radio-core integration layer. Its initial
extraction order is squelch, CTCSS, DCS, then GPIO/parallel-port signaling.
CTCSS encode and decode are one component, as are DCS encode and decode. GPIO
and parallel-port components implement signaling semantics through
adapter-supplied pin I/O. They do not directly own Linux device access.
Portable radio components have no OSS or PortAudio dependency; host adapters
retain direct audio and hardware-I/O ownership.

Controller components are extracted in this order: generic tone generation,
Morse, DTMF, message templating, then generic scheduling. Morse owns character
encoding and timing while using the tone component for waveform production.
DTMF detection and generation are one component. The scheduler provides only
recurrence and due-event calculation; controller policy for links,
announcements, warnings, and macros remains in `app_rpt_advanced`. The message
template component only parses and expands caller-supplied key/value data; the
controller defines substitutions and authorization-sensitive values.

The following further boundaries are approved directions, not current runtime
dependencies:

- `librptadvconfig` owns only INI syntax reading and the owned document model.
  Schema, inheritance, and settings resolution remain controller policy.
- `librptadvspeech` owns offline speech-engine lifecycle and audio-file
  preparation outside real-time audio. Identifier and announcement fallback
  policy remains in the controller.
- `librptadvdirectory` will own static-file/DNS directory lookup and peer
  source verification when standalone IAX work removes its Asterisk allocation
  dependency.
- `librptadvaccess` becomes appropriate when RF, AllStarLink, EchoLink, and
  REST access control share a substantial verified-identity policy.
- `librptadviax2` will own IAX2 framing, negotiation, control messages, and
  media transport for standalone interoperability. The current
  Asterisk-specific link implementation is not extracted prematurely.

Controller orchestration, node-specific configuration resolution, identifiers,
announcements, duplex policy, link topology policy, and Asterisk channel/media
adapters remain in the application because they intentionally evolve together.

## Consequences

An extracted component is independently testable, documented, packaged, and
released. Its narrow ABI limits the set of consumers affected by a change.
Components used on an audio path preserve lock-free operation and do no
allocation, blocking I/O, configuration reload, or process execution from an
audio tick.

The project will not create libraries solely for file layout or for tiny,
single-use helpers. Each extraction must preserve existing behavior and carry
its own tests, Doxygen, packaging, and quality gate before a consumer replaces
its local implementation.
