# ADR 0017: Scheduler route lifecycle and civil-time semantics

Status: Accepted

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
unblock the route.

Hub-visible local identity is stable hub-owned storage or is published through
the hub routing synchronization. A reload may not update a pointer observed by
the hub under an unrelated lock.

Post-window receive activity has an explicit observed/not-observed state. A
valid monotonic timestamp of zero must never be interpreted as no activity.

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

The window grammar will accept `24:00` only as an exclusive `end_time` boundary
and represent it internally as minute 1440. `24:00` is never a start time.
Cross-midnight windows remain unsupported until a single explicit overnight
window semantic is approved; two same-route sections are not an emulation.
At cold start, a post-window inactivity grace calculation must also examine the
immediately preceding selected window so a quiet interval may cross midnight.

### Scheduling pressure and overlaps

Scheduled zero-time actions are best effort at their exact wall-clock instant:
a missed captured minute or failed control-task submission is not replayed.
The scheduler must expose health counters and rate-limited warnings for failed
task submission and control-queue delay. A bounded IAX dial may occupy the
serialized control queue for its dial budget, delaying later control work but
never audio; that delay must remain observable in status and logs.

Multiple matching replacement windows form a union. Every active or deferred
replacement is requested and each named primary is suppressed. There is no
exclusive winner-selection policy; normal direct-link topology admission is the
only conflict gate.

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
