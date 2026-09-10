# ADR 0003: Shared rate-adjusting PCM ring

Status: Accepted

## Context

Independent media and hardware clocks drift. Duplicate elastic-buffer
implementations in rpt_advanced and USBRadioPlus caused unnecessary divergence.

## Decision

Use the separately released `rate_adjusting_pcm_ring` shared library for
lock-free PCM playout buffering, controlled rate recovery, and shortfall
observability. rpt_advanced and USBRadioPlus link against the same public ABI.

## Consequences

Consumers must declare and install the shared-library dependency. Changes to
the ring's public ABI require coordinated consumer builds and releases.
