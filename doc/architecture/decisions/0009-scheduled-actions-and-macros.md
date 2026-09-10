# ADR 0009: Scheduled actions and macros stay in the control plane

Status: Accepted

## Context

rpt_advanced needs scheduled zero-time actions, named macros, and rendered
message templates without allowing arbitrary process execution or affecting
lock-free audio processing.

## Decision

Schedule actions and expand templates on a control-plane worker. Macros resolve
only to validated controller operations. Initial triggers are scheduled
zero-time events only. Events due at the same instant execute in configuration
order; their prepared messages use the existing serialized telemetry path.

Templates accept exactly `day_of_week`, `date`, `time`, `greeting`,
`link_status`, `node`, and `callsign`. Unknown or malformed substitutions make
configuration invalid. Validation also proves that a worst-case expansion fits
the 127-byte scheduled-message payload; node and callsign values are bounded to
the 63-byte peer-identity transport limit.

When an event has both a queueable message and a macro, its message must first
be accepted by the serialized telemetry queue. A rendered message with no
Morse-representable character is intentionally not queueable, so its macro
proceeds without a telemetry-queue dependency. The scheduler settles the
dispatch before it releases the runtime lock, then executes the macro without
waiting for RF playback. This gives an event at-most-once control semantics across reload:
the external operation may be skipped if a reload invalidates it immediately
before execution, but it is never duplicated. A full queue leaves both portions
pending, preserving configuration order.

The module ticker queues one captured control task for each wall-clock minute it
observes. Each task snapshots all due events in its captured minute before it
dispatches any. The FIFO control executor retries retained older occurrences
before later ones. This preserves chronological and same-minute configuration
order when speech preparation or a link operation temporarily occupies the
control executor. A successful reload queues a fresh current-minute check after
the new runtime is active.

## Consequences

No macro can execute shell code or launch a process. Scheduler wall-clock work,
template expansion, message preparation, and macro dispatch cannot occur in an
audio callback. Additional triggers or substitutions require a new requirement
and ADR update.
