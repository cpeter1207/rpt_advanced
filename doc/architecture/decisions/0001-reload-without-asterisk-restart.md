# ADR 0001: Reload without an Asterisk restart

Status: Accepted

## Context

Radio nodes need configuration changes without interrupting the Asterisk
process or unrelated calls.

## Decision

`app_rpt_advanced` validates and prepares a complete candidate operating
generation before changing the active runtime. A failed parse, validation, or
preparation leaves the current generation intact. Once ready, the candidate is
published atomically. The local receive and transmit owners adopt it
independently at their next call boundaries while hazard protection keeps the
replaced generation alive until every protected callback and tagged work item
has quiesced.

A change of radio-device identity, native rate, or hardware adapter uses a
controlled RF-safe handoff rather than an ordinary generation publication. The
station first deasserts PTT, quiesces both audio owners and the old adapter,
then transfers the device lease. [ADR 0026](0026-generational-real-time-runtime-lifecycle.md)
owns the detailed publication, adoption, retirement, unload, and device-handoff
contract.

## Consequences

Reload paths must cover candidate rollback, independent owner adoption,
hazard-protected retirement, controlled device handoff, and unload. A reload is
not complete merely because a candidate pointer was published. Configuration
changes cannot rely on process-global initialization that only occurs at
Asterisk startup.
