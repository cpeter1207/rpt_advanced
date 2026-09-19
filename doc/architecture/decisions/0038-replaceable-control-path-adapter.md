# ADR 0038: Replaceable control-path execution adapter

Status: Accepted

## Context

At acceptance, the module serialized scheduler work, link events, and DTMF commands
through an Asterisk taskprocessor. That behavior is useful, but the controller
must not depend directly on a particular taskprocessor or thread model.
Standalone operation must be able to use another implementation without
rewriting controller policy.

This decision refines ADR 0036: retaining an Asterisk taskprocessor behind a
control-path adapter is explicitly permitted. Removing the Asterisk backend
from the current module is not required.

## Decision

Add a **control-path adapter** as an independently replaceable execution
capability. Its current backend is the Asterisk taskprocessor. A later backend
may use an owned implementation or a suitable third-party taskprocessor, but
must satisfy the same controller-facing functional contract.

The controller owns task meaning, node policy, scheduling decisions, generation
tags, and logical lifecycle. The adapter owns submission to the selected
taskprocessor, its execution context, backend-specific thread handling, and
the resources needed to stop and release that executor. Asterisk taskprocessor
handles, listeners, error codes, and thread types do not cross into controller
code. Backend selection does not alter task ordering or controller behavior.

The minimal contract is:

- **Serialized execution:** one admitted control task runs at a time within
  each logical control executor. Preserve FIFO acceptance order and each
  producer's submission order; concurrent submissions may be accepted in
  either order. Multiple submitters must not cause concurrent controller
  mutation. Preserve the current executor scope rather than introducing new
  per-node or per-peer taskprocessors.
- **Explicit admission and ownership:** submission reports accepted or
  rejected. Accepted work transfers to the executor and is invoked once;
  rejected work stays with the caller for cleanup. Do not execute a task
  inline as a fallback when submission fails. Capacity/rejection reporting
  remains explicit under ADR 0026, with bounded retained work.
- **Control-only work:** scheduler ticks, link lifecycle notifications, and
  decoded-command handling use this boundary. Audio callbacks, peer PCM-ring
  exchange, packet-ingress serialization, and DSP do not submit through or
  wait for this executor. The existing non-audio dispatcher hands off radio
  events before controller work is submitted.
- **Safe stop and drain:** gate producers and stop admission before draining
  accepted tasks. The controller invalidates retired-generation work; a stale
  task still releases its resources without applying stale policy. Drain waits
  for accepted work and active callbacks to finish before releasing executor,
  adapter, or any controller state still reachable by those callbacks.
  Runtime/configuration may be released before drain only after invalidation
  guarantees that remaining tasks cannot access it and can only clean up their
  owned payloads. Retain the revision/lifetime guards until drain completes.
  Never kill a running task, free reachable state, or wait for drain from the
  executor that must perform it.
- **Reload and replacement:** preserve ADR 0026's generation and quiescence
  rules. Backend replacement follows ADR 0022: a controlled reload or restart
  after the old executor has quiesced, not live code replacement. A missing or
  incompatible selected adapter fails preparation; it does not silently
  execute through another backend.

The adapter is not a calendar scheduler, macro engine, telemetry policy
engine, or generic application-services container. Due-time calculation,
retry policy, task generation checks, and node state remain in the controller.
Periodic triggers submit ordinary control work; the selected taskprocessor's
clock or scheduling API does not become a core dependency.

Preserve current caller-side consequences of a rejected or stale task. In
particular, lost DTMF work invalidates the partial command, generation checks
reject obsolete operations, and external dialing occurs outside the runtime
lock with a revision check before attachment. Capture trigger timestamps
before submission and retain immediate load/reload scheduler ticks. These
remain controller responsibilities, not taskprocessor backend features.

Use ADRs 0021/0022's versioned descriptor/function-table and dynamic adapter
contract. The Asterisk taskprocessor backend is separate from the Asterisk
application/channel entry adapter because control execution is independently
replaceable. Both may depend on public Asterisk APIs; neither requires ASL3.
The exact descriptor fields and package names are implementation details and
are not introduced by this architecture-only change.

A standalone composition selects a non-Asterisk control-path backend. It must
meet the standalone lock-free/thread-safe-alternative requirement in ADR 0037
as well as this functional contract. Internal Asterisk taskprocessor locking
does not impose that implementation on standalone operation. The standalone
packet fan-in remains a separate media boundary, not this control executor.

## Consequences and verification

The current backend remains Asterisk. Migration consists of isolating its
existing use behind this adapter, not building a second taskprocessor now.
Controller code can then replace the backend without changing command,
scheduling, telemetry, or reload semantics.

Run the same contract tests against each backend: accepted/rejected ownership,
FIFO and non-overlap with concurrent submitters, no inline fallback, generation
invalidation, stop during producer activity, draining active/queued work, and
safe unload. Exercise existing DTMF, link-event, scheduler, reload, and module
unload integration against the Asterisk backend. Verify no control-adapter
operation is reachable from a native audio callback.

At acceptance, the C module still invoked the taskprocessor directly, and CLI
execution and incoming-link admission mutated runtime state under its lock.
The original decision changed architecture only; the implementation status below
records the later extraction. Synchronous entry points must remain serialized
with queued work. Their Asterisk request/response handling belongs in the entry
adapter, not in taskprocessor-specific policy. Tests must exercise them
concurrently with queued work, reload, and shutdown.

## Implementation status — 2026-09-15

`librptadv_control_asterisk_adapter.so.1` now implements the versioned execution
descriptor. The product-owned client implements core's neutral `ControlExecutor`
contract; neither the controller core nor the product owns an Asterisk
taskprocessor handle. Application/CLI entry handling stays in the separate
Asterisk adapter and forwards into product lifecycle/control operations.

The provider opens one uniquely named executor, transfers accepted task
ownership, reports rejection without inline fallback, gates admission, and drains
accepted work. Its final close releases the sole taskprocessor reference and
waits for its worker to finish; the loader retains the provider DSO through that
barrier. Radio rendering and peer PCM exchange do not use this executor.
The product still owns generation validation, scheduling, retry decisions, and
the periodic trigger. No non-Asterisk backend was added by this migration.

The required contract and integration checks above continue to apply. This
implemented boundary is not a claim that the current working tree has passed
its full release gate or been deployed.
