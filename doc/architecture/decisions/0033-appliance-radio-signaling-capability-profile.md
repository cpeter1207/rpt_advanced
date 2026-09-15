# ADR 0033: Appliance direct-codec radio-signaling capability profile

Status: Accepted

## Context

The appliance radio-port electrical contract has RX, TX-A, TX-B, isolated PTT,
hardware COR, external CTCSS encoder-enable, and external CTCSS decoder status.
It deliberately has no analog CTCSS output. The generic native-media routing
record describes CTCSS/DCS generation after program mixing, which would be
incorrect when an appliance radio uses its external CTCSS encoder.

## Decision

An appliance direct-codec port selects the `appliance_direct_codec` signaling
capability profile.

- CTCSS encode is an abstract external encoder-enable action on the isolated
  port output. The transmit worker does not mix an analog CTCSS tone into TX-A or
  TX-B for this profile.
- CTCSS decode is the already-published isolated hardware status input. A
  configured flat-discriminator path may still use DSP carrier detection; it
  does not replace the external CTCSS status.
- COR uses either the configured direct hardware COR input or the existing DSP
  carrier detector. The profile does not change that existing selection.
- The appliance electrical contract has no discrete DCS interface. When DCS is
  configured, the portable radio core remains its DSP decoder and native-audio
  encoder; its tone is part of the selected TX audio path rather than a new
  appliance pin.

The radio core exposes this as a capability profile rather than testing for an
appliance model throughout DSP or media-routing code. The PortAudio/ALSA
appliance adapter owns only the SAI/PCM stream. The separate GPIO/actuator
adapter publishes direct COR/CTCSS and hardware-safety snapshots and applies
prepared PTT/CTCSS-enable actions. Neither adapter owns signaling policy.

Under the pending 2026-09-13 split in ADR 0027, receive DSP/decode executes in
the input-driven local receive worker, and generated DCS or external CTCSS
enable executes through the output-clocked transmit owner. The split does not
override this profile's external CTCSS selection or add analog CTCSS injection.

## Consequences

The generic media-routing diagram means “add generated CTCSS or DCS when the
selected hardware profile requires it.” For an appliance direct-codec port,
only DCS can be generated in the audio mix. Configuration and tests must reject
a request to inject native CTCSS when this profile is active. The appliance
hardware repository remains the source for electrical implementation and safe
state requirements; this ADR supplies the software capability contract.
