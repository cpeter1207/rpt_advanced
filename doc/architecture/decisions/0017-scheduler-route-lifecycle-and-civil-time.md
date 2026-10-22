# ADR 0017: Scheduler route lifecycle and civil-time semantics

Status: Implemented

## Context

Configured permanent links, local-time replacement windows, scheduled events,
and hub recovery share one serialized control plane. They must continue to
respect the lock-free audio boundary while reconciling an asynchronous link
transport lifecycle with wall-clock scheduling and monotonic receive activity.

The scheduler review identified ambiguity at the boundary between an ended peer
and its retry record, a retry that races a replacement window, topology-loop
retries, live reload, and civil-clock discontinuities. This record defines the
policy for those boundaries. It supplements [ADR 0009](0009-scheduled-actions-and-macros.md)
and [ADR 0010](0010-configured-permanent-links-and-replacement-windows.md).

## Decision

### Configured-route ownership and recovery

Runtime owns configured-route policy. The link hub owns a route's transport
lifecycle, but its scheduler-visible ownership is continuous from an attached
port through ending, retained retry, and a replacement connected port. The
scheduler reconciles that ownership in both directions: it does not attach a
route still hub-owned, and it withdraws a hub-owned route that current policy
suppresses before attaching a replacement.

Every scheduler attachment and retained hub retry must pass the same final,
lock-free `still desired` gate immediately before it publishes a peer. The gate
checks current route policy, schedule generation, reservation nonce, and any
operator hold. A retry callback must not acquire `runtime_lock`; that would
violate the control-plane lock ordering and can deadlock a live reload.

A route refused for a topology loop enters an explicit topology-blocked state,
rather than an ordinary rapid retry state. It remains blocked until fresh
peer-advertised topology changes the relevant topology generation, a
configuration reload changes the route policy, or an operator explicitly
retries it. Locally generated lifecycle changes alone must not repeatedly
unblock the route. Members of a configured priority group are the specific
exception: permanent and scheduled-group routes bypass advertised-topology loop
rejection and remain eligible for silent background retry and priority
selection. This exception does not bypass self-link checks, direct duplicate
checks, allow/deny policy, or final current-policy validation, and does not
apply to routes outside a configured group.

Hub-visible local identity is stable hub-owned storage or is published through
the hub routing synchronization. A reload may not update a pointer observed by
the hub under an unrelated lock.

Post-window receive activity has an explicit observed/not-observed state. A
valid monotonic timestamp of zero must never be interpreted as no activity.

Group selection and availability telemetry use the scheduler's effective
desired routes, including post-window inactivity grace and operator holds.
Inactive schedule items do not announce a shared peer's failure. When disjoint
schedules reuse a peer, its callback selection belongs to the currently desired
group rather than the first configured occurrence of that peer.

### Civil time, elapsed time, and lifecycle

The scheduler evaluates event triggers and window membership in the host's
local civil time. Post-window inactivity is elapsed monotonic time measured
only from local-receiver or linked-peer activity.

- A daily, weekly, or once event runs at most once for a local civil minute. A
  repeated daylight-saving fall-back minute is one occurrence; a skipped
  spring-forward minute has none.
- Replacement windows follow the current local civil clock. A partial window
  in a repeated hour may open and close on both physical occurrences; a skipped
  interval does not occur.
- Wall-clock corrections have no catch-up behavior. A forward correction skips
  missed zero-time events. After a backward correction, new event occurrences
  remain suppressed until civil time reaches new territory. Window membership
  immediately follows the corrected local time.
- A `once` event means once per running scheduler instance. It is intentionally
  not durable across an Asterisk restart.
- Event identity across reload is the owning node, section label, and trigger.
  Changing message text or a macro preserves its fired state; changing that
  identity creates a new eligible event. A successful live reload intentionally
  cancels queued or in-progress telemetry rather than replaying it.

The window grammar accepts `24:00` only as an exclusive `end_time` boundary
and represents it internally as minute 1440. `24:00` is never a start time.
Cross-midnight windows remain unsupported until a single explicit overnight
window semantic is approved; two same-route sections are not an emulation.
At cold start, a post-window inactivity grace calculation must also examine the
most recent selected ended window so a quiet interval may cross midnight or
unselected days. The remaining grace becomes a monotonic deadline; warning
lead times use that same deadline, not a reconstructed pre-start process time.
The shared schedule provider searches at most seven weekday offsets or the
validated explicit-date list. Active windows do not yield a historical end.

### Scheduling pressure and overlaps

Scheduled zero-time actions are best effort at their exact wall-clock instant:
a missed captured minute or failed control-task submission is not replayed.
The scheduler must expose health counters and rate-limited warnings for failed
task submission and control-queue delay. A bounded IAX dial may occupy the
serialized control queue for its dial budget, delaying later control work but
never audio; that delay must remain observable in status and logs.

The product records actual executor submission failures, failed live scheduler
cycles, last and maximum enqueue-to-start delay in microseconds, and tasks
delayed by at least one second. This threshold is one calendar-check interval;
shorter delays remain visible in last/maximum values. Retired-generation
rejection during reload or shutdown is not a failed submission or live cycle.
Counters belong to the shared executor, survive reload, and reset when the
product starts again. Status samples each atomic field independently.

Admission and cycle warnings share a monotonic one-minute rate limit; queue
delay warnings have an independent one-minute limit. Counts are never
rate-limited. Existing notice callbacks encode command completion or directory
reason codes; neither can carry these warnings without changing its contract.
The required host-services ABI 7 `scheduler_notice` callback
forwards control-only borrowed UTF-8 to standalone stderr/journald or Asterisk's
warning logger. Product ABI 5, control-executor ABI 1, and DSO SONAMEs are
unchanged. Rebuild and install the product and both applicable hosts together;
older or incomplete host tables are rejected before use. No alpha compatibility
table, new configuration parameter, or audio-callback logging is added.

Multiple matching replacement windows form a union. Every active or deferred
replacement is requested and each named primary is suppressed. There is no
exclusive winner-selection policy; normal direct-link topology admission is the
only conflict gate.

Warning leads are consumed once per calendar occurrence and qualifying
receive-inactivity period. Recalculating a civil-time deadline in monotonic
milliseconds must not reset that identity: normal control-tick scheduling jitter
is not a new warning. Crossing the window end preserves consumed leads through
its inactivity grace. Only a new calendar occurrence or actual receive activity
that resets an enabled inactivity timer makes those leads eligible again.

## Consequences

The implementation requires deterministic interleaving tests for ended-port to
retry ownership, retry-versus-replacement suppression, topology-blocked
recovery, activity at monotonic zero, `24:00`, overlap, DST transitions, clock
steps, reload identity, telemetry cancellation, and cold-start grace across
midnight. Tests must use explicit civil-time and `tm_isdst` fixtures rather
than host timezone assumptions.

These rules deliberately favor predictable current-time behavior over replaying
past RF or link operations. Persistent once-ever events, overnight windows, and
durable telemetry delivery are separate requirements.

## Implementation status — 2026-10-21

The group-specific topology exception, final current-policy gate, continuous
route ownership, topology-blocked retry, input-activity idle handoff, and
scheduled replacement lifecycle are implemented and covered by runtime and
integration tests. Same-peer schedule reuse is allowed only across disjoint
windows; overlapping duplicate scheduled peers are rejected by configuration
validation, with boundary and integration coverage.

The shared scheduler retains ordinary end-time grace across calendar boundaries,
including a `23:59` end, five-minute grace, and `00:01` startup. Provider and ABI
tests use independent expected elapsed values; controller tests verify route
ownership, monotonic expiry, and warning deadlines. The provider's ABI 1 and
SONAME are unchanged. Before an upcoming window, warnings still use its future
end rather than the newly visible historical end.

Product lifecycle tests cover real submission rejection, reload retirement,
live-cycle failure, rate-limited host reporting, reload-persistent counters, and
status output. Both host adapters validate and forward the required warning sink.

Isolated-process tests set an explicit POSIX timezone and check `tm_isdst` for
both physical fall-back hours and the spring-forward gap. They exercise event
deduplication, repeated window membership, skipped windows, forward/backward
clock steps, monotonic inactivity, once-event lifetime, and reload identity.
These checks do not add catch-up, overnight syntax, durable events, or a new
scheduling policy.
