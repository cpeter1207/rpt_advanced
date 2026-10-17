# ADR 0033: Appliance direct-codec radio-signaling capability profile

Status: Accepted

Amended 2026-10-17: CTCSS encode and decode sources are independent; GPIO
capabilities belong to the selected hardware, not only the appliance.

## Context

The appliance radio-port electrical contract has RX, TX-A, TX-B, isolated PTT,
hardware COR, external CTCSS encoder-enable, and external CTCSS decoder status.
It has no dedicated analog CTCSS pin. DSP-generated CTCSS may travel in TX-A or
TX-B audio; an external encoder instead receives an enable action. A radio may
provide an encoder without a decoder, or a decoder without an encoder. Its
capabilities must not couple the software's encode and decode choices.

## Decision

An appliance direct-codec port selects the `appliance_direct_codec` signaling
capability profile. The profile describes available interfaces, not a mandatory
external CTCSS source.

- CTCSS encode and decode each select disabled, DSP, or external independently.
  All combinations supported by the selected hardware are valid. In particular,
  external encode with DSP decode does not require an external decoder.
- DSP encode uses the existing native generator and mixes CTCSS into a suitable
  selected TX audio path. External encode produces only an encoder-enable action;
  it does not also generate CTCSS audio. Disabled encode produces neither.
- DSP decode uses the existing native detector. External decode uses the
  already-published hardware status input. Disabled decode adds no CTCSS
  receive-qualification requirement. Raw hardware status remains observable
  independently of the configured qualification source.
- COR uses either the configured direct hardware COR input or the existing DSP
  carrier detector. The profile does not change that existing selection.
- The appliance electrical contract has no discrete DCS interface. When DCS is
  configured, the portable radio core remains its DSP decoder and native-audio
  encoder; its tone is part of the selected TX audio path rather than a new
  appliance pin.

The radio core exposes this as a capability profile rather than testing for an
appliance model throughout DSP or media-routing code. The PortAudio/ALSA
appliance adapter owns only the SAI/PCM stream. The separate GPIO/actuator
adapter publishes available COR/CTCSS and hardware-safety snapshots and applies
prepared PTT/CTCSS-enable actions. GPIO may come from appliance hardware, a USB
interface such as URIXB on a Raspberry Pi, CM119 GPIO, or a parallel port. Each
backend declares its actual inputs, outputs, directions, and safety facilities;
not every backend provides every signal. Neither adapter owns signaling policy.

ADR 0042's optional activity-scoped CTCSS encode policy applies to either
encoding source. It gates native generation or external encoder-enable using
the same traffic, command-response, CTCSS-hang, identifier, and courtesy rules.
It does not gate receive CTCSS qualification or hardware status sampling.
Activity-scoped decode is not planned.

Under the receive/transmit split in ADR 0027, receive DSP/decode executes in
the input-driven local receive worker. DSP CTCSS/DCS generation or external
CTCSS-enable intent executes through the output-clocked transmit owner. The
split does not change the independently selected signaling sources.

## Consequences

The generic media-routing diagram means “add generated CTCSS or DCS when the
selected source uses DSP.” Configuration validates each selected source against
the audio/GPIO capabilities. A missing required external input/output must not
silently select DSP or bypass receive qualification; the port remains safely
inactive with a useful error. Unused capabilities need not exist.

An external encoder-enable output does not configure frequency, deviation, or
turn-off waveform. Those belong to the radio/encoder. Native generation settings
must not be presented as controls for that external hardware; unsupported phase
or tail-tone requests cannot be simulated by an on/off GPIO. DSP encode retains
its existing frequency, level, routing, and turn-off behavior. The appliance
hardware repository remains the source for electrical implementation and safe
state requirements; this ADR supplies the software capability contract.
