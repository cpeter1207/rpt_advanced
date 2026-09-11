# ADR 0008: Per-node transmitter protection

Status: Accepted

## Context

Each node needs an independent transmitter watchdog and kerchunk control
without blocking its lock-free audio path or applying one input's short
transmission policy to unrelated link sources.

## Decision

Use controller-owned per-node settings: a 180-second watchdog for one keyed
interval without a receive unkey, a 30-second post-timeout lockout, and a
500-millisecond kerchunk threshold. The watchdog timestamp starts when PTT
keys and resets at every local-receiver or individual direct-link falling edge,
even when hang time, telemetry, or another source keeps PTT asserted. It
therefore protects against a source that never unkeys rather than ending a
normal conversation with pauses. The watchdog immediately drops PTT on expiry.
Recovery still requires both lockout expiry and the active source to clear. A
kerchunk suppresses only that source's courtesy tone and every-release
announcement, but its unkey also resets the watchdog. Local receiver and each
link peer retain their own rising-edge timestamp.

## Consequences

All limits can be overridden per node or disabled with zero. The controller
does not synthesize timeout telemetry. Watchdog and kerchunk decisions are
made from already available audio-worker timestamps without locks or allocation.
