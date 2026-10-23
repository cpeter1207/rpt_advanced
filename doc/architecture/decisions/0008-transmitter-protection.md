# ADR 0008: Per-node transmitter protection

Status: Implemented

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
normal conversation with pauses. The watchdog immediately removes traffic
PTT demand on expiry.
Recovery still requires both lockout expiry and the active source to clear. A
kerchunk has zero transmitter hang time and suppresses its courtesy tone and
every-release announcement. When it is the last active input, its unkey clears
the latched traffic hang and release-message opportunity. Every source unkey
resets the watchdog. Other active traffic,
playing telemetry, and due identifiers retain their independent transmit
demand. A later normal source release retains its configured hang and release
messages. Local receiver and each link peer retain their own rising-edge
timestamp.

## Consequences

All limits can be overridden per node or disabled with zero. The controller
does not synthesize timeout telemetry. Watchdog and kerchunk decisions are
made from already available audio-worker timestamps without locks or allocation.

## Message playback exception — 2026-10-23

Release tails, scheduled messages and manually selected configured messages
retain their PCM and physical PTT demand across a traffic timeout or lockout.
The traffic watchdog continues running: exempt playback cannot carry timed-out
receive audio or regenerated traffic onto RF. Site inhibition, hardware faults,
interlocks and half-duplex restrictions still apply. A finished message retains
only the existing short natural release margin, not an additional traffic hang.
