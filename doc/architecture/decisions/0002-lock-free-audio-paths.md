# ADR 0002: Lock-free audio paths

Status: Accepted

## Context

Real-time radio audio must not block on mutex contention, configuration work,
disk I/O, process execution, or network control operations.

## Decision

All audio-path data exchange uses lock-free ownership and atomic state. Work
that can block or allocate unpredictably is prepared outside the audio path
and published as immutable prepared state.

## Consequences

Control-plane and audio-plane boundaries must be explicit. New audio features
need bounded, allocation-free callback behavior and tests that exercise their
concurrent ownership rules.
