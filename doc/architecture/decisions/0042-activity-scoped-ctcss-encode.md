# ADR 0042: Activity-scoped CTCSS transmit control

Status: Accepted (2026-10-05)

## Context

USBRadioPlus already accepts live transmit CTCSS enable/inhibit changes. The
RPT Advanced native callback returned only PCM and PTT, so it could not apply
an independent activity policy to the tone encoder. Coupling CTCSS to PTT or
the transmitter hang timer would incorrectly encode during IDs and courtesy
tones and would not preserve CTCSS during command-response playout.

## Decision

Add the node settings `ctcss_encode_on_input` and `ctcss_hang_ms`, inheriting
from the general scope. Both default to disabled/zero to preserve current
behavior. When activity-scoped encode is enabled, transmit CTCSS follows live
local-receiver or connected-peer activity and a separate CTCSS hang interval.
The CTCSS hang interval is milliseconds, defaults to zero, and must not exceed
`transmit_hang_ms`. A pending command response qualifies from accepted command
receipt through its actual playout, including time waiting for the transmitter.
Identifiers, courtesy tones, and other non-response telemetry do not qualify.

The native transmit callback returns two independent decisions: PTT keyed and
CTCSS enabled. USBRadioPlus applies the CTCSS decision through its existing
live inhibit control; the app_rpt compatibility path and existing USBRadioPlus
`TXCTCSS` text command retain their behavior. CTCSS decode scoping is separate
from transmit control and is not included in this design.

## Compatibility

The added callback output changes the direct callback ABI from 2 to 3. The RPT
Advanced product and host-services tables advance from ABI 2/3 to 3/4 because
they retain or pass that callback. Incompatible alpha combinations are rejected
as a set; there is no compatibility shim under ADR 0040. The descriptor export
names and shared-library SONAMEs do not change. USBRadioPlus ABI 3, RPT Advanced
product ABI 3, and host-services ABI 4 must ship together.

## Consequences

The activity policy remains in the portable controller. The fixed transmit
callback passes its result to the radio adapter alongside PTT, without locks,
allocation, or control-plane work. Tests cover inherited defaults, hang bound
validation, active input, independent CTCSS hang, command-response qualification,
and suppression during identifiers and courtesy tones.

## Implementation status — 2026-10-05

Configuration, controller policy, and the direct callback contract are
implemented. Activity-scoped CTCSS decode is not planned.
