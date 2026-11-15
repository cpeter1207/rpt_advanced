# Single-call Bypass and Playout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox syntax.

**Goal:** Add optional unlock-qualified bypass for the exact outgoing SIP call, and inherited local/all-peer telephone playout, without changing the one-call limit.

**Architecture:** Reuse the existing protected source collector and serialized telephone admission. Latch bypass on the existing call claim. Prepare one immutable exact-call policy in the existing mixer graph, preserving the already-qualified isolation cut and serial ACK custody. Reuse Mode's independent transmit/forward bits for telephone playout.

**Tech Stack:** Existing Rust core/product/standalone and released dynamic providers.

**Spec:** docs/superpowers/specs/2026-11-09-phone-patch-and-half-duplex-design.md P13/P14; ADRs 0023 and 0044. The owner limited core scope to one-call SIP softphone operation on 2026-11-15.

## Global Constraints
- Default isolation on dc02689 is a separate qualified packet; do not delay its IVR integration or alter live 2155.
- Optional link_command_phone_bypass is empty/unassigned by default. No live key assignment.
- Incoming explicit attachment forwarding remains unchanged. Outgoing playout is the resolved configuration snapshot for that call; ordinary rearm does not resample another configuration, and committed reload retires the call.
- phone_outgoing_playout defaults to local; all selects otherwise eligible peers. Isolation takes precedence until exact-call bypass.
- An unlock code is always required. RemoteBase Open, mailbox login, remote selection, CLI access and outgoing callee media permission do not supply this grant.
- Existing per-source unlock expiry applies at admission; successful bypass lasts only that call, including after the unlock grant expires.
- Preserve existing ordinary controls, exact connection identities, macro cancellation/deadlines, Engine configuration revision, source timestamps, RF safety and mix-minus.
- No new PCM queue, provider API, worker, persistent owner map, concurrent-call policy or network expiry guarantee.
- One heavy constrained test job at a time; own existing target and regular copied DSOs.

## Approach
The existing source collector, call claim and prepared graph already own all required state. Extending their typed values avoids a second authorization store or mixer. A broad node-level bypass flag would outlive the call or affect a successor and is rejected. A separate forwarding queue would duplicate the proven media path and is rejected.

## Review Focus
- A source cannot borrow another local/peer/phone collector's grant, including under RemoteBase Open.
- Pending/failed isolation ACK custody cannot disappear when the desired bypass bit changes.
- Queued work admitted for call A cannot mutate call B at the same node.
- Failure to publish bypass must not report success or silently allow ordinary media.
- Local-only versus all-peer playout must preserve RF output, telephone return, no echo and ordinary mode/group/ended gates.

### Task 1: Configured protected command and existing mixer mode
**Files:** core command.rs, config/{settings,schema,mod}.rs, runtime/{dtmf,links}.rs, link/hub.rs and adjacent tests.
**Interfaces:** LinkAction::PhoneBypass is a no-argument optional action. PhonePlayout::{Local,All} resolves phone_outgoing_playout. Mode::LOCAL_TRANSCEIVE has transmit=true, forward=false. Core PhoneCommand carries the original operation/work only after the existing exact collector consumes administrative authorization.
- [x] Add configuration/action and local/peer protected-source REDs using real command collection. Cover default disabled mapping, inheritance, malformed fallback, Open-without-unlock rejection, expiry and distinct source grants.
- [x] Implement the optional schema/action and typed playout setting using existing resolver patterns; use the existing protected action authorization path without broadening PhoneDial/PhoneHangup.
- [x] Add/verify a shared mixer mode matrix proving local RF plus telephone return remain active but ordinary peers receive phone audio only for All and no isolation.
- [x] Run focused core tests and independent source review; included in the recovery checkpoint below.

### Task 2: Exact-call policy publication
**Files:** product phone_outgoing.rs, host.rs, host_phone_outgoing.rs, lifecycle_radio_phone.rs and existing outgoing/isolation tests.
**Interfaces:** A small prepared policy stores {call_id, isolated} independently; no-call is None. OutgoingClaim stores a bypass latch. The current Engine phone admission turn validates source/configuration, captures the exact claim and requests its bypass. Host publishes the desired immutable policy through the existing full cut.
- [ ] Add actual Engine/Host REDs for unlocked exact-call bypass, pending/failed cut, stale successor, expired/foreign grant and unavailable call.
- [x] Keep prior same-call failed/unresolved transition custody authoritative even when desired isolation changes. Readiness requires exact desired policy plus existing callback/serial ACK completion.
- [x] Mutate only the captured current claim. Roll back only pre-publication refusal; a post-publication pump error preserves the exact committed policy and latch custody, reports failure, and cannot bypass pending ACK checks. Release/hangup/reload destroys the latch. Do not reacquire a fresh source permit.
- [x] Renew only the successful original source's administrative grant using existing confirmation methods. Preserve existing solicited result routing without giving the callee control.
- [ ] Run focused lifecycle/host/source tests, independent review and recovery commit.

### Task 3: Composed qualification
**Files:** existing ordinary-link/SIP outgoing fixture and ADR0044 append.
- [ ] Extend the retained-link fixture with a configured unused bypass key and unlock, proving default isolation then exact-call bypass with local/all-peer playout; next call starts isolated.
- [ ] Preserve original immediate greeting, continuous media, no echo, control and hangup assertions. Use only synthetic loopback parties.
- [ ] Run affected aggregate/strict and final IVR union checks under parent scheduling. Record exact results and limits.
- [ ] Deliver the selected clean leaf/bundle for separately authorized hardware validation. Do not activate or publish.


## Current checkpoint
- Focused schema/source tests and the actual shared-mixer matrix pass in bypass-next.log. Source review covers these exact changes.
- Four Host policy/custody tests pass: exact successor, pending ACK, failed/reaped owner, and post-publication pump failure. The separate cancellation-after-publication test reproduced false success; its final exact-current postcondition is saved and independently source-reviewed, with GREEN pending.
- Engine admission is not implemented. Its test setup now uses the existing finite probe and parked callback-owner handoff; the previous wait incorrectly assumed this fixture invokes radio callbacks autonomously. No behavior claim is based on those setup failures.
- Exact-source result policy: capture original operation/reply before admission. Only a still-current source can receive confirmation or renew its collector. A full ordinary-ring cut retires the issuing ordinary reply; do not remap it to a new peer generation or renew that replacement collector. RF human results are LOCAL only; authenticated phone results retain their private exact reply. Private errors use the existing outer failure path once.
- P14 stays isolated from the selected IVR/default-isolation packet. Native staging has build priority.


## Focused recovery qualification
- bypass-focused-green3.log: all five selected core tests passed, including live/retired ordinary-source confirmation and actual targeted PCM. Actual Engine bypass, grant renewal and LOCAL result passed, as did exact-call cancellation, committed-policy error and successor tests.
- bypass-private-strict-green.log: rendered private success/failure-once/no-RF/retired matrix passed; all five outgoing isolation regressions passed; core/product all-target Clippy and formatting passed.
- The first private test attempt inspected lazy speech before rendering. Its final version uses the existing finite private-stream helper, requiring successful completion and no second reply before counting opened speech. No production adjustment or privacy relaxation was needed.
- This is a local recovery checkpoint, not full PR, composed, hardware or release qualification. Integration with selected IVR/default isolation, composed bypass/local-all proof and complete affected gates remain open.
