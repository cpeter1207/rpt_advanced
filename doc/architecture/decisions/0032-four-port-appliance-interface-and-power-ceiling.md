# ADR 0032: Four-port appliance interface and power ceiling

Status: Accepted

## Context

The approved appliance originally allowed five independent direct-codec radio
ports. Detailed review established a feasible four-interface DART mapping
(SAI2, SAI3, SAI5, and SAI6), but not a five-interface mapping compatible with
the carrier and boot-pin constraints. The design uses one dedicated SAI per port so each module can retain its own
codec, isolated analog interface, and serial clocking.

## Decision

The appliance carrier supports one through four field-installable direct-codec
radio-port modules and four associated host-only USB-C CAT ports. It uses SAI2,
SAI3, SAI5, and SAI6; the selected DART assembly is No-AC, and startup design
holds SAI6 boot-strap pins electrically inactive through the documented POR
sampling window. It uses four
buffered 10 MHz reference outputs when the optional GPS timing hardware is
fitted. The DART-MX8M-MINI remains the selected conditional SoM.

The fully populated four-port appliance has a maximum normal DC-input budget of
54 W, a 75 W continuous 9--36 V input-path rating, and a 33 W sustained
enclosure-heat budget at +55 degrees C ambient. Each CAT port remains limited
to 5 V / 0.9 A, for a combined 18 W external USB-load budget. The 75 W rating
remains deliberately conservative for protection, connectors, wiring, and
future costed design margin; it does not imply a fifth radio position.

## Consequences

The KiCad architecture, product requirements, power allocation, radio-GPIO
reservation, validation fixture, and software appliance requirements all use a
four-port maximum. A later expansion beyond four ports requires a new explicit
architecture decision and a different audio topology or SoM; it is not an
uncommitted option in this carrier design.
