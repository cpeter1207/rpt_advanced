# ADR 0040: Initial alpha does not require backward compatibility

Status: Accepted (2026-09-13)

## Context

USBRadioPlus, rpt_advanced, and their owned shared components are still alpha
releases of the initial version. The product owner clarified that backward
compatibility with earlier alpha artifacts is not a requirement.

## Decision

Do not retain obsolete implementation, settings, descriptor slots, adapters,
or tests solely to support earlier project alpha releases. Remove retired
features without adding compatibility shims. This supersedes backward-
compatibility requirements elsewhere in the architecture and development
instructions for these initial-alpha project-owned interfaces.

Preserve the currently required behavior unless the user authorizes changing
it. Keep contracts required to interoperate with external systems such as
Asterisk, ASL3, FFmpeg, and hardware. A C-compatible calling convention is an
interoperability requirement, not a promise to retain an old project ABI.

Update current consumers, headers, tests, and package dependencies together.
Identify incompatible artifacts through the descriptor version and appropriate
SONAME/package metadata so a mixed installation fails safely instead of calling
the wrong function. This detects incompatibility; it does not preserve it.
Dynamic dependency, quality-gate, and deployment-authorization rules remain.

## Consequences

The native-repeat/parrot removal in ADR 0039 need not preserve unused shared
operations for old USBRadioPlus binaries. The explicit instruction to silently
ignore `duplexmode` and `duplex_local_repeat_mode` remains required behavior,
with `duplex3` hardware-only. This policy does not authorize new features or
changes to installed nodes.
