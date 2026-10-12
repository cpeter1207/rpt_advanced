# ADR 0011: Stable functional components use narrow versioned shared-library boundaries

Status: Implemented (initial existing-function extraction)

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

These are public extracted-component boundaries. A deliberately separated
private Rust component within one product is instead governed by ADR 0020: it
uses `dylib` and is built with its owning product rather than becoming an
independently released SONAME contract.

`librptadvradio` remains the radio-core integration layer. Its initial
extraction order is squelch, CTCSS, DCS, then GPIO/parallel-port signaling.
CTCSS encode and decode are one component, as are DCS encode and decode. GPIO
and parallel-port components implement signaling semantics through
adapter-supplied pin I/O. CM119 GPIO and parallel-port GPIO are supported
site-I/O sources. These components do not directly own Linux device access.
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

The following further boundaries are tracked separately from the initial
extraction. This extraction does not establish their implementation status:

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
- `librptadviax2` owns IAX2 framing, negotiation, control messages, and media
  packet handling for standalone interoperability. Its protocol engine is
  separate from socket I/O; a standalone network adapter supplies and sends
  datagrams through a bounded interface. Codec adapters are separate from the
  protocol engine and use released codec libraries where available. The
  current Asterisk-specific link implementation is not extracted prematurely.
- The control-path execution adapter owns only taskprocessor submission,
  serialized execution, and stop/drain. ADR 0038 selects Asterisk's
  taskprocessor as its current backend; scheduling and controller policy are
  not part of this adapter. Its versioned boundary follows ADR 0022.

Controller orchestration, node-specific configuration resolution, identifiers,
announcements, duplex policy, link topology policy, and Asterisk channel/media
adapters remain in the application because they intentionally evolve together.

## Initial extraction status

The nine existing-function providers were released after their native PR gates
and published to the signed package index. Their runtime/development packages,
released SONAMEs, and direct consumers are:

| Provider release | SONAME | Debian runtime / development packages | Direct consumer |
| --- | --- | --- | --- |
| [squelch v0.1.0-alpha.2](https://github.com/cpeter1207/librptadvsquelch/releases/tag/v0.1.0-alpha.2) | `librptadvsquelch.so.2` | `librptadvsquelch2` / `librptadvsquelch-dev` | `librptadvradio.so.4` |
| [CTCSS v0.1.0-alpha.2](https://github.com/cpeter1207/librptadvctcss/releases/tag/v0.1.0-alpha.2) | `librptadvctcss.so.2` | `librptadvctcss2` / `librptadvctcss-dev` | `librptadvradio.so.4` |
| [DCS v0.1.0-alpha.1](https://github.com/cpeter1207/librptadvdcs/releases/tag/v0.1.0-alpha.1) | `librptadvdcs.so.1` | `librptadvdcs1` / `librptadvdcs-dev` | `librptadvradio.so.4` |
| [signaling v0.1.0-alpha.1](https://github.com/cpeter1207/librptadvsignaling/releases/tag/v0.1.0-alpha.1) | `librptadvsignaling.so.1` | `librptadvsignaling1` / `librptadvsignaling-dev` | `librptadv_gpio_adapter.so.1` |
| [tone v0.1.0-alpha.1](https://github.com/cpeter1207/librptadvtone/releases/tag/v0.1.0-alpha.1) | `librptadvtone.so.1` | `librptadvtone1` / `librptadvtone-dev` | `librptadv_product.so.1`, `librptadvmorse.so.1` |
| [Morse v0.1.0-alpha.1](https://github.com/cpeter1207/librptadvmorse/releases/tag/v0.1.0-alpha.1) | `librptadvmorse.so.1` | `librptadvmorse1` / `librptadvmorse-dev` | `librptadv_product.so.1` |
| [DTMF v0.1.0-alpha.1](https://github.com/cpeter1207/librptadvdtmf/releases/tag/v0.1.0-alpha.1) | `librptadvdtmf.so.1` | `librptadvdtmf1` / `librptadvdtmf-dev` | `librptadv_product.so.1` |
| [template v0.1.0-alpha.1](https://github.com/cpeter1207/librptadvtemplate/releases/tag/v0.1.0-alpha.1) | `librptadvtemplate.so.1` | `librptadvtemplate1` / `librptadvtemplate-dev` | `librptadv_product.so.1` |
| [schedule v0.1.0-alpha.1](https://github.com/cpeter1207/librptadvschedule/releases/tag/v0.1.0-alpha.1) | `librptadvschedule.so.1` | `librptadvschedule1` / `librptadvschedule-dev` | `librptadv_product.so.1` |

Radio adopted the ABI-2 squelch and CTCSS packages and ABI-1 DCS in
[v0.1.0-alpha.8](https://github.com/cpeter1207/librptadvradio/releases/tag/v0.1.0-alpha.8);
the hardware GPIO adapter adopted signaling in
[v0.1.0-alpha.3](https://github.com/cpeter1207/rptadv-gpio-adapter/releases/tag/v0.1.0-alpha.3).
The controller product dynamically links the five controller components, and
Morse dynamically links tone. The initial extraction moves existing DTMF
detection only. DTMF PCM generation remains a later requirement in the same
component; generic audio-device control and the further directions above are
outside this extraction.

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
