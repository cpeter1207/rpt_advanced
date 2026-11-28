# ADR 0019: Configuration resolves unknown input through warnings and defaults

Status: Implemented for current configuration parsing and resolution; ongoing policy

## Context

Configuration files commonly outlive individual module versions. They can
contain legacy options for retired behavior, future options introduced by a
newer version, or operator mistakes. Rejecting an otherwise usable file solely
for an unknown name or value makes upgrades and recovery unnecessarily fragile.

## Decision

Treat an unknown section or parameter name as a warning and ignore it. Treat an
unknown, malformed, or unsupported parameter value as a warning and resolve
the setting through its normal inheritance chain and documented sensible
default. The warning identifies the file, section, name, supplied value when
safe to display, and resolved fallback.

Configuration loading reports an error only when it cannot construct a safe,
deterministic runtime configuration after applying defaults—for example, an
unreadable source or irrecoverable structural ambiguity. A warning never
prevents startup or reload. A successful reload activates the fully resolved
candidate configuration; an actual error retains the running configuration as
defined by ADR 0001.

Every configurable setting must have a documented sensible default or a safe
disabled behavior, so an unsupported value has a deterministic fallback.

## Consequences

Legacy and forward-unknown configuration remains operational while operators
receive actionable diagnostics. Parsers and schema resolvers must distinguish
warnings from errors, test both paths, and expose resolved effective values to
the existing configuration/status interfaces. New settings require a default
before they are accepted.

## JSON implementation clarification (2026-11-19)

[ADR 0045](0045-modular-json-and-hal-management.md) applies this same warning and
inherited/default fallback policy to modular JSON reloads and API configuration
writes. Ignore only the unusable field, not its valid siblings or parent.
Canonical schemas describe normalized output. Core diagnostics and host logs
must not contain supplied values or credentials. Syntax, duplicate-member
ambiguity and unsafe structural/lifecycle relationships remain errors when no
safe deterministic candidate exists.
