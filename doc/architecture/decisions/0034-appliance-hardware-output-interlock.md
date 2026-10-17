# ADR 0034: Appliance hardware output interlock

Status: Software contract implemented; physical interlock qualification pending

## Context

The appliance can key transmitters and control isolated site outputs. A
software reset, stalled process, failed normal rail, or resumed watchdog pulse
must not leave an RF or site output asserted. Codec-register mute and
serial-audio disable do not prove an inactive analogue transmit path.

## Decision

GPIO is hardware-neutral. USB interfaces, parallel ports, and appliance
hardware expose only the capabilities they actually provide. The appliance
interlock below is mandatory for that appliance, not a requirement that every
GPIO device emulate arm feedback, watchdogs, or physical TX mutes. Missing
optional capabilities are not faults; a missing capability required by the
selected hardware safety contract or signal assignment fails closed.

Every direct-codec appliance port is covered by the independent appliance
hardware output interlock: one appliance-level safety gate plus separate
physical TX mutes for each port's TX-A and TX-B legs. A low-current `3V3_SAFE`
rail powers only the supervisor,
fault-latch, passive timing network, and high-impedance power-good monitors.
It never powers an actuator. A normally-off gate derives
`SAFE_ACTUATOR_3V3` from `3V3_SYS` only when protected input and main power are
good, SoM reset is released, the watchdog is valid, and the hardware
`OUTPUT_ARM` latch is set.

Any fault asynchronously clears `OUTPUT_ARM`. Firmware can set it only after
a successful post-reset radio initialization. A resumed watchdog kick cannot
set it. Loss of the interlock forces both post-DAC TX legs into their specified
attenuating/disconnected state, releases PTT and external CTCSS-enable
contacts, and disables DB-25 output drive. The physical interlock is specified
and qualified in the appliance hardware repository.

The radio core's transmit worker under the ADR 0027 split continues
to produce abstract prepared PTT and
signaling actions. The selected appliance hardware adapter reports output-arm
and safety faults through its status contract, but neither the audio callback
nor the portable radio core owns the latch or takes a lock to operate it.

## Consequences

The appliance adapter must explicitly arm only after a controlled hardware
startup or recovery and must surface an arm failure to the control plane. It
must not retry a safety fault by merely resuming watchdog traffic. Other
hardware compositions retain their own declared safe-state mechanisms; software
must not claim physical interlock protection on hardware that lacks it.

The software has no fixed port-count limit. It allocates endpoint state during
control setup from configured hardware and available resources. The appliance's
four-port board limit does not constrain USB, parallel-port, or other hardware
compositions. One interlock context covers exactly the ports and site outputs
connected to that physical gate. Its fault disarms them all, not unrelated
hardware contexts. This decision adds no native-tick allocation, lock, or
blocking operation.

## Implementation evidence

The GPIO adapter's normalized context owns the disarmed, arming, armed, and
latched-fault states. Its sole control owner samples transport health, requests
physical arm, applies prepared intent, and submits safe outputs. Audio owners
only publish complete epoch-tagged intent or read bounded atomic snapshots.
Every setup allocation can fail without publishing a handle or aborting the
process. Port count is resource-bounded, not a fixed bitmap.

Revocation invalidates readiness and observations before transport I/O. Recovery
repeats initialization but does not arm. Fresh radio warm-up/readiness, matching
new arm feedback, and a new output epoch are required; stale PTT/encoder intent
cannot replay. Epoch exhaustion fails closed. Successful writes are not treated
as physical readback. Failed safe submission is a visible latched fault.

Provider tests cover each health input, stale/future snapshots, failed I/O,
deadline and epoch exhaustion, allocation failure, retirement, and recovery.
The cross-library test uses seven covered ports plus an independent ordinary
context. Its simulated physical gate releases outputs during a software-service
stall; callback publication remains allocation-free and nonblocking.

These tests validate software contracts only. No simulated transport is exposed
by an installed service. They do not qualify watchdog timing, wiring, post-DAC
attenuation, power-loss behavior, or the physical appliance interlock.
