# ADR 0008: Per-node transmitter protection

Status: Implemented

## Context

Each node needs an independent transmitter watchdog and kerchunk control
without blocking its lock-free audio path or applying one input's short
transmission policy to unrelated link sources.

## Decision

Use controller-owned per-node settings: a 180-second watchdog for one keyed
interval without a receive unkey and a 500-millisecond kerchunk threshold.
The watchdog timestamp starts when PTT
keys and resets at every local-receiver or individual direct-link falling edge,
even when hang time, telemetry, or another source keeps PTT asserted. It
therefore protects against a source that never unkeys rather than ending a
normal conversation with pauses. Its timeout and recovery sequence is defined
below; there is no fixed post-timeout lockout.

A kerchunk has zero transmitter hang time and suppresses its courtesy tone and
every-release announcement. When it is the last active input, its unkey clears
the latched traffic hang and release-message opportunity. Every source unkey
resets the watchdog. Other active traffic,
playing telemetry, and due identifiers retain their independent transmit
demand. A later normal source release retains its configured hang and release
messages. Local receiver and each link peer retain their own rising-edge
timestamp.

## Consequences

Both limits can be overridden per node or disabled with zero. Watchdog and
kerchunk decisions use already available audio-worker timestamps without locks
or allocation. Timeout wording and media preparation remain on station control,
outside the audio workers.

## Spoken timeout and receive-idle recovery — 2026-10-26

On watchdog expiry, mute local and linked receive audio and regenerated traffic
at the local RF output, then play the local-only notice, "Repeater timed out
after [duration]." Once that notice finishes, drop PTT and wait until every
local and connected-peer input is inactive, including non-selected group
members. The first all-idle observation admits "Repeater timeout cleared."
Complete that notice before restoring received traffic, even if an input
rekeys during it, then begin a fresh traffic-watchdog interval. There is no
30-second delay or `timeout_lockout_ms` setting. An obsolete configuration key
follows ADR 0019: warn and ignore it without rejecting valid settings.

These notices have priority over ordinary local telemetry. They do not wait
for the triggering receive activity to end, and muted activity does not select
Morse instead of speech. The existing speech-unavailable Morse fallback remains.
Other local playback is suspended and restarted after recovery, without
overlapping the notices. Physical site inhibition, hardware faults and
interlocks still apply. Notice completion uses only the existing short natural
release margin, never a traffic hang interval.

Use two reusable station-control-registered media streams and the existing
serialized producer, telemetry playout ring and completion handoff. Fluent
owns notice wording and the existing localized duration formatter owns its
duration; round non-integral milliseconds upward to whole seconds. Audio
workers consume prepared PCM and advance bounded state without rendering,
allocation, blocking, another worker or a new PCM ring.

The timeout protects this node's local RF transmitter. Keep peer connections
and between-peer routing intact. Continue receive-ring consumption, all
detectors, DTMF processing and input-state publication throughout the muted
interval so recovery observes every input. Full configuration reload retains
the existing controller-generation semantics.

Release tails, scheduled messages and manually selected messages do not trip
the traffic watchdog merely by playing. Their ordinary PCM/PTT exemption
remains, but a timeout caused by received traffic gives the notices priority
and suspends their local playback until recovery. Exempt playback cannot carry
timed-out receive audio or regenerated traffic onto RF.
