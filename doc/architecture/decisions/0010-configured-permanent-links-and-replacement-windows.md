# ADR 0010: Configuration-owned permanent links and replacement windows

Status: Accepted

## Context

rpt_advanced needs direct links that are recreated from configuration and a
simple way to substitute a scheduled-net peer for one of those links. The
handoff must not create a temporary topology overlap, delay or lock the audio
path, or make link state depend on an Asterisk restart.

## Decision

Use `[permanent node label]` with one required `remote_node` for each
configuration-owned permanent direct peer. On module start and a successful
configuration reload, runtime derives the desired permanent-peer operations
from those sections and uses the existing permanent link recovery behavior. A
permanent route may name an ordered fallback list. Runtime attempts a fallback
only when the primary cannot be reconnected, and withdraws an active fallback
before reconnecting a recovered primary so the two never create a topology
overlap.

Use `[schedule node label]` to replace one same-node permanent link during a
bounded local-time window. Its settings are `remote_node`,
`replace_permanent`, `days`, `dates`, `start_time`, `end_time`, and
`end_inactivity_ms`. The schedule names an existing same-node permanent label,
uses an inclusive-start/exclusive-end same-day local window, and may select
either weekdays or explicit Gregorian dates, not both.

At a window start, a schedule may first disconnect all links, temporary links,
permanent links, or no existing links before it attaches its scheduled peer.
Normal topology admission remains the final conflict gate.

While a window is active, runtime first withdraws the named permanent route and
then attaches the replacement. At the end, it withdraws the replacement before
restoring the named permanent route. Zero `end_inactivity_ms` restores at the
window end. A nonzero value means the window has stopped requiring the
replacement, but it remains connected after local or linked receive activity
until the configured quiet interval expires. The runtime
reads that activity lock-free and performs all time evaluation and link work on
the serialized control plane. A cold start within the quiet interval after a
selected window retains the replacement for only the wall-clock interval still
remaining; later qualifying activity replaces that conservative estimate.

Each copied configured-link transition carries its schedule generation, route
slot, and one-use reservation nonce. The runtime validates that identity before
preparing, attaching, retaining, or withdrawing a link. `*806` disconnects and
holds permanent links only, allowing permitted manual links to remain usable.
`*816` withdraws, re-evaluates, and reconnects permanent links only. A
replacement configuration can still withdraw an issued route its current
policy suppresses. This protects scheduler transitions from stale operations.
The final hub-retry gate and continuous route-ownership rule are defined by
ADR 0017.

ADR 0017 requires a final lock-free current-policy gate for direct scheduler
attachment and retained hub retry immediately before a peer is published. It
also requires continuous hub ownership through an ending port and its retry
record, plus a topology-blocked state instead of a one-second retry. Those
recovery hardening changes are deliberately documented separately from this
route and window model.

## Consequences

Configuration reload reevaluates current local-time membership without
redialing an unchanged issued route. Configuration errors reject self-links,
duplicate configured remote identities for a node, unknown replacement labels,
and a replacement that names its permanent peer again. Multiple matching
replacement windows intentionally form a union: every matching or deferred
replacement is requested and every named primary is suppressed. They have no
exclusive arbitration beyond normal direct-link topology admission; an overlap
must not be used to select one replacement route. Warning timing and civil-time
behavior are defined by ADRs 0009 and 0017.
