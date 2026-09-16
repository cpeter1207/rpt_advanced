# ADR Divergence Alignment Design

## Scope

Resolve only the six present-day divergences identified by the 2026-09-15
architecture audit. Do not implement incomplete or future wishlist features.
Preserve current observable radio, link, configuration, and packaging behavior
except where an accepted ADR explicitly requires different behavior.

## Decisions

### Reload and aggregate ownership documentation

Amend ADR 0001 so a valid reload prepares and atomically publishes a complete
generation, then retires the prior generation after both real-time owners have
adopted the replacement. A failed candidate leaves the active generation
unchanged. ADR 0026 owns the detailed lifecycle.

Amend ADR 0014 to reflect the evolved implementation: `RuntimeNode` is the
station-policy aggregate root and the current `NodeController` is the bounded
audio/transmit-policy aggregate. Do not move working policy merely to preserve
obsolete type names.

### Scheduler edge semantics

Implement only ADR 0017's missing edge rules:

- accept `24:00` only as an exclusive schedule end;
- represent observed activity separately from timestamp value so monotonic zero
  is valid activity; and
- retain topology-rejected automatic routes in an explicit blocked state until
  topology generation, configuration, or operator action permits another try.

Do not add schedule warnings, fallback links, new schedule actions, or other
wishlist behavior.

### Rust ownership

Complete the existing USBRadioPlus Rust boundary by moving substantive
Asterisk-facing lifecycle, channel, delivery, reload, DTMF, link, and CLI work
behind the existing Rust adapter contract. Retain only the minimum C required
to expose Asterisk module metadata and enter the Rust adapter. Preserve the
current external Asterisk channel behavior and current adapter ABI unless an
unavoidable incompatible change is proven.

### PortAudio scheduling

At stream startup, attempt the highest permitted FIFO priority without changing
callback-time behavior. If no elevation succeeds, retain inherited scheduling,
record the limitation, and start normally. Genuine PortAudio/device failures
and failure to restore a scheduling change that actually succeeded remain
fatal. Add focused injected-scheduler tests before changing implementation.

### Shared-ring compatibility removal

Remove the unused ABI-1/S16 facade from `rate_adjusting_pcm_ring`: legacy source,
header, tests, pkg-config data, build targets, Debian packages, documentation,
and archive/install assertions. Keep ABI 2, its SONAME, normalized-F32 behavior,
and current consumers unchanged. Verify that USBRadioPlus and rpt_advanced use
only ABI 2 before removal.

## Verification

Use test-first targeted checks for each behavior change. Run repository-specific
formatting, lint, and static analysis before any push. Full Debian 13 amd64/arm64
quality gates remain pull-request gates and are not run locally merely to prepare
a push. No node deployment, configuration change, link connection, release, or
wishlist implementation is part of this work.
