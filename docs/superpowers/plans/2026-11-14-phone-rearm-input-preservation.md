# Phone rearm input preservation implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Preserve ordinary queued audio and receive history when an unrelated phone source rearms.

**Architecture:** Reuse the existing hazard-free adapter publication transfer. Prepare read-only ordinary input reservations, then move the unique live input and receive state at commit. Preserve the serial producer and source state; replace outgoing mixed history with an exact-generation acknowledged outbound redirect.

**Tech Stack:** Existing Rust core/product, released PCM ring and DSP providers.
**Spec:** ADR0044, approved phone-patch design, and the scoped 2026-11-14 phone-only rearm repair.

## Global constraints
- Only phone-only refresh with unchanged ordinary peer membership and settings.
- No second PCM queue, cloneable consumer, provider API or timing/age/target change.
- Phone retirement and outbound discard/reset remain mandatory.
- No live or public changes in this increment.

## Review focus
- Full-set mismatch and callback Busy must precede every ownership move.
- Old/duplicate/wrong-connection ACK cannot release a new graph.
- Prepared candidate drop cannot end the retained producer.
- Primed receive, DSP, pending input and source history survive rearm.
- Retired phone media never becomes valid again.

## Tasks
- [x] Add real host regression: 160ms ordinary audio, phone source gap/rearm, then 140ms. Observe RED at lost source identity/occupancy and later require ordered actual output.
- [x] Add focused Busy/mismatch/drop, receive-history and exact ACK regressions; independently reproduce the stale same-name successor redirect as behavioral RED.
- [x] Extend the existing guarded adapter transfer with whole-set validation and infallible swaps; reserve ordinary input via observer only.
- [x] Add outbound-only serial redirect and exact graph/connection acknowledgment, preserving normal full redirects.
- [ ] Run focused core/product tests, strict lint/docs, then unchanged original/existing composed cases in the serialized constrained environment.
- [x] Obtain independent source review and update ADR0044. Save a local recovery commit after targeted tests and strict checks; full/composed qualification remains pending.

## Verification checkpoint

Both original ownership loss and the reviewer-found same-name successor race were
observed as behavioral failures before their production repairs. Seven focused
checks now pass. All-target Clippy, Rustdoc and Rustfmt checks pass for the affected
core/product/standalone boundary. This local checkpoint is not full qualification:
unchanged composed original/existing calls and complete core/product suites remain.
