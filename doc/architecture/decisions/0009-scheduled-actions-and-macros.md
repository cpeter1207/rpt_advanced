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

The module ticker queues one captured control task each second. Runtime
calendar-minute de-duplicates event occurrences, so a second-level tick cannot
repeat an event. The FIFO control executor retries retained older occurrences
before later ones. This preserves chronological and same-minute configuration
order when speech preparation or a link operation temporarily occupies the
control executor, while allowing configured post-window quiet-time deadlines to
expire without an extra minute of delay. A successful reload queues a fresh
current-time check after the new runtime is active. A successful reload may
discard queued or in-progress telemetry rather than replay it; the associated
macro remains at-most-once. Event identity across reload is the owning node,
section label, and trigger, so changing message text or a macro does not make an
already-fired event eligible again. The civil-time discontinuity policy is
defined by [ADR 0017](0017-scheduler-route-lifecycle-and-civil-time.md).

## Consequences

No macro can execute shell code or launch a process. Scheduler wall-clock work,
template expansion, message preparation, and macro dispatch cannot occur in an
audio callback. A bounded IAX dial may delay later serialized control work but
cannot delay audio; scheduler health and control-queue delay remain observable.
Additional triggers or substitutions require a new requirement and ADR update.
