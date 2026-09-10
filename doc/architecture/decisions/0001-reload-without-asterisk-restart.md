# ADR 0001: Reload without an Asterisk restart

Status: Accepted

## Context

Radio nodes need configuration changes without interrupting the Asterisk
process or unrelated calls.

## Decision

`app_rpt_advanced` validates a complete candidate configuration before
replacing live node workers. A failed parse, validation, or startup leaves the
previous running configuration intact. A valid replacement stops and cleans up
the prior worker set before activating the new one.

## Consequences

Reload paths must have complete ownership cleanup and rollback coverage.
Configuration changes cannot rely on process-global initialization that only
occurs at Asterisk startup.
