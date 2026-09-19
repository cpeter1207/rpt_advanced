# ADR 0034: Appliance hardware output interlock

Status: Accepted

## Context

The appliance can key transmitters and control isolated site outputs. A
software reset, stalled process, failed normal rail, or resumed watchdog pulse
must not leave an RF or site output asserted. Codec-register mute and
serial-audio disable do not prove an inactive analogue transmit path.

## Decision

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

The radio core's transmit worker under the pending ADR 0027 split continues
to produce abstract prepared PTT and
signaling actions. The selected appliance hardware adapter reports output-arm
and safety faults through its status contract, but neither the audio callback
nor the portable radio core owns the latch or takes a lock to operate it.

## Consequences

The appliance adapter must explicitly arm only after a controlled hardware
startup or recovery and must surface an arm failure to the control plane. It
must not retry a safety fault by merely resuming watchdog traffic. Other
hardware compositions remain free to provide their own equivalent physical
safe-state mechanism. This decision adds no native-tick allocation, lock, or
blocking operation.
