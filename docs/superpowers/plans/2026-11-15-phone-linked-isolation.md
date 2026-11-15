# Single-call Linked-peer Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox syntax.

**Goal:** Keep ordinary links and controls connected while a single outgoing SIP call isolates their audio before dialing and until exact call release.

**Architecture:** Prepare the existing shared PCM graph with immutable ordinary-peer suppression. Replace and acknowledge existing ingress/egress owners before dialing, without adding queues, clocks, provider contracts or threads. Transfer an admitted telephone command from obsolete graph custody to its original source permit, configuration revision and exact call claim.

**Tech Stack:** Existing Rust core/product/standalone and released dynamic providers.

**Spec:** docs/superpowers/specs/2026-11-09-phone-patch-and-half-duplex-design.md, P13/P14; owner clarification 2026-11-15 makes simultaneous calls, IAX and Asterisk hosting stretch goals.

## Global Constraints
- Preserve one call per initiating node; conference expansion is not this milestone.
- No live changes, external calls or remote connections.
- Keep original source/grant deadlines, configuration revision, source timestamps, local RF safety and revocable phone generations.
- No new PCM queue, worker, provider ABI or downstream expiry guarantee.
- Fresh constrained quality image, one heavy job at a time, existing isolated warm target and copied exact-source DSOs.
- Isolation precedes all-peer playout unless the exact call has bypass; bypass lifetime is the call, not an invented unlock-expiry timer.
- P14/configuration follows the default isolation checkpoint; mappings remain optional and unassigned.

## Review Focus
- Topology replacement during ringing must not cancel an admitted call, but macro cancellation/deadline or reload still must.
- Buffered peer audio and converter history must be retired before provider dial; callback/redirect delay must never become success.
- New/retrying ordinary peers retain control and publish only under the active isolation policy.
- Failure, hangup or stale completion releases only the exact call and cannot affect a successor.
- Peer-only media must neither key RF nor reach the telephone/other peers; local and callee media keep their ordinary route.

### Task 1: Transfer admitted source ownership
**Files:** core runtime/links.rs and links_tests.rs; product lifecycle.rs, lifecycle_radio_phone.rs, lifecycle_phone_outgoing_tests.rs.
**Interface:** Consume PhoneCommand after serialized validation and exact claim reservation into a cloneable admitted source permit retaining its original DigitOperation and pending_source_current predicate. Call jobs also retain their original Engine revision and exact OutgoingClaim.
- [x] Add a graph-refresh-during-ringing row to the actual Engine/factory matrix and observe behavioral RED.
- [x] Drop only GenerationWork after admission. Use the original admitted permit for current, ringback and failed-spawn checks together.
- [x] Test macro cancellation/deadline, rejected admission, reload/stop, and stale successor; run affected tests and commit.

### Task 2: Publish default shared-graph isolation
**Files:** core link/audio.rs and audio_tests.rs; product host.rs, host_phone_outgoing.rs, host_phone_worker.rs, phone_outgoing.rs and adjacent host/lifecycle tests.
**Interface:** LinkAudio::with_peer_isolation(bool) sets prepared ordinary-media policy. GenerationControl records exact isolated claim ID. Host::prepare_outgoing_isolation(&Arc<OutgoingClaim>, RuntimeClock) returns readiness only for that current policy after exact endpoint ACKs/retirement. Ordinary Engine reconciliation restores released policy through the same graph boundary.
- [ ] Add shared-mixer input/output/control cases and retained-peer reservation/publication REDs.
- [ ] Suppress ordinary input activity/mix and ordinary media destinations while retaining transport owners, source processing and control.
- [ ] Replace existing rings/queues at acquisition/release, wait existing redirect ACK before dial, and allow new/retry peer publication under current policy.
- [ ] Prove Busy/preparation failure, delayed ACK, buffered revocation, failure/hangup restoration and successor identity with existing fixtures.
- [ ] Run targeted tests, independent review and a clean recovery commit.

### Task 3: Qualify single-call checkpoint
**Files:** existing standalone composed fixture and ADR0044 append.
- [ ] Extend existing actual SIP/ordinary-peer fixture with default isolation, unchanged one-call capacity, local/callee audio, ordinary control and restoration.
- [ ] Run affected full core/product/standalone and strict checks, record exact counts and limits.
- [ ] Deliver exact commit/bundle to parent for separately authorized hardware milestone. Do not activate.

### Follow-on P14/configuration
After the default milestone, use optional configured action and the existing exact-source node unlock grant. Capture the exact call at serialized admission; Open remote-base policy, mailbox login and callee media role grant no bypass. Reuse Mode's independent transmit/forward bits for local versus otherwise-eligible all-peer playout; do not add another mixer. No live mapping or multi-call semantics are introduced.
