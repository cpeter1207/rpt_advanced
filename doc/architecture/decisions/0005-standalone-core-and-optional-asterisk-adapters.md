# ADR 0005: Standalone core and optional Asterisk adapters

Status: Accepted

## Context

rpt_advanced must initially interoperate with ASL3 while also becoming a small,
fast, standalone radio controller with no Asterisk dependency. USBRadioPlus is
currently an Asterisk channel driver, but standalone operation needs direct
audio I/O and an Asterisk-free radio interface.

## Decision

Keep the controller core, I/O boundaries, and inter-thread communication
lock-free and independent of Asterisk. Build standalone rpt_advanced and
USBRadioPlus as dynamically linked shared objects with a lockstep ABI.
Standalone operation uses PortAudio for audio I/O. Asterisk/ASL3 support is
provided by thin optional adapters over that shared core and may be retired
without changing standalone behavior.

## Consequences

No Asterisk type, lock, callback, or lifecycle rule may enter the shared core.
The Debian packages for rpt_advanced and USBRadioPlus must use exact matching
versions. IAX2 and ASL HTTP-registration support belong to standalone
rpt_advanced; adapter code remains limited to translation at the boundary.
