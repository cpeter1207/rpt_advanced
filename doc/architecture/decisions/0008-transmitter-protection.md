# ADR 0008: Per-node transmitter protection

Status: Accepted

## Context

Each node needs an independent transmitter watchdog and kerchunk control
without blocking its lock-free audio path or applying one input's short
transmission policy to unrelated link sources.

## Decision

Use controller-owned per-node settings: a 180-second continuous-PTT watchdog,
a 30-second post-timeout lockout, and a 500-millisecond kerchunk threshold.
The watchdog immediately drops PTT. Recovery requires both lockout expiry and
the active source to clear. A kerchunk suppresses only that source's courtesy
tone and every-release announcement. Local receiver and each link peer retain
their own rising-edge timestamp.

## Consequences

All limits can be overridden per node or disabled with zero. The controller
does not synthesize timeout telemetry. Watchdog and kerchunk decisions are
made from already available audio-worker timestamps without locks or allocation.
