# ADR 0031: Five-port appliance power and thermal budget

Status: Superseded by ADR 0032

## Context

The appliance supports five radio ports, five USB-C CAT ports, optional GPS
timing, and a 1U enclosure. Its power input, protection, external supply, and
cooling need an explicit common ceiling before component and PCB decisions can
be safely made.

## Decision

The fully populated appliance has a maximum normal DC input budget of 60 W and
a 75 W continuous input-path rating across the 9--36 V range. At 9 V, the
input path is designed for at least 10 A; at the normal 13.8 V site supply, an
external supply must provide at least 6 A. Each USB-C CAT port is limited to
5 V / 0.9 A, for a combined 22.5 W external USB-load budget.

The cooling system continuously rejects 35 W of appliance-generated heat at
the PRD's +55 °C ambient with all five radio positions populated, all CAT ports
at budgeted load, and GPS fitted. USB output power counts in the input budget,
but only its conversion loss counts as appliance heat. All components must
remain within their manufacturer ratings under this condition.

The allocation is a design ceiling, not an unverified component measurement.
DVT requires a selected-component and five-port-fixture validation; a value may
change only through a synchronized PRD, hardware-architecture, ADR, and
wishlist decision.

## Consequences

The electrical design reserves input, connector, protection, trace, and fuse
capacity for the 75 W worst case. Thermal design and production validation use
35 W sustained enclosure dissipation, rather than incorrectly treating all USB
load power as internal heat. Software may expose power and thermal sensors, but
does not rely on them to compensate for an under-sized hardware design.
