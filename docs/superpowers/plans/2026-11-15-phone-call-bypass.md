# Single-call Bypass and Playout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox syntax.

**Goal:** Add optional unlock-qualified bypass for the exact outgoing SIP call, and inherited local/all-peer telephone playout, without changing the one-call limit.

**Architecture:** Reuse the existing protected source collector and serialized telephone admission. Latch bypass on the existing call claim. Prepare one immutable exact-call policy in the existing mixer graph, preserving the already-qualified isolation cut and serial ACK custody. Reuse Mode's independent transmit/forward bits for telephone playout.

**Tech Stack:** Existing Rust core/product/standalone and released dynamic providers.

**Spec:** docs/superpowers/specs/2026-11-09-phone-patch-and-half-duplex-design.md P13/P14; ADRs 0023 and 0044. The owner limited core scope to one-call SIP softphone operation on 2026-11-15.

## Global Constraints
- Preserve the qualified IVR/default-isolation base 1285f8f. This P14 increment is local-only until a separately authorized native/hardware checkpoint.
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
- [x] Add actual Engine/Host REDs for unlocked exact-call bypass, pending/failed cut, stale successor, expired/foreign grant and unavailable call.
- [x] Keep prior same-call failed/unresolved transition custody authoritative even when desired isolation changes. Readiness requires exact desired policy plus existing callback/serial ACK completion.
- [x] Mutate only the captured current claim. Roll back only pre-publication refusal; a post-publication pump error preserves the exact committed policy and latch custody, reports failure, and cannot bypass pending ACK checks. Release/hangup/reload destroys the latch. Do not reacquire a fresh source permit.
- [x] Renew only the successful original source's administrative grant using existing confirmation methods. Preserve existing solicited result routing without giving the callee control.
- [x] Run focused lifecycle/host/source tests, independent review and recovery commit.

### Task 3: Composed qualification
**Files:** existing ordinary-link/SIP outgoing fixture and ADR0044 append.
- [x] Extend the retained-link fixture with a configured unused bypass key and unlock, proving default isolation then exact-call bypass with local/all-peer playout. The Host exact-successor matrix separately proves a new call cannot inherit bypass.
- [x] Preserve original immediate greeting, continuous media, no echo, control and hangup assertions. Use only synthetic loopback parties.
- [x] Run affected aggregate/strict and the linked-IVR union regression under parent scheduling. Record exact results and the outgoing cases deliberately not repeated.
- [ ] Deliver the selected clean leaf/bundle for separately authorized hardware validation. Do not activate or publish.


## Implementation and source-result policy
- Configuration, exact-source authorization, immutable call policy, Engine admission and result routing are implemented and independently reviewed. The cancellation-after-publication regression passed with the final exact-current postcondition.
- Capture the original operation/reply before admission. Only a still-current source can receive confirmation or renew its collector. A full ordinary-ring cut retires the issuing ordinary reply; do not remap it to a new peer generation or renew that replacement collector.
- RF human results are LOCAL only; authenticated phone results retain their private exact reply. Private errors use the existing outer failure path once.
- Retain failure/ACK custody after a committed graph publication even if the subsequent pump fails. Roll back only a request that never published.

## Focused recovery qualification
- bypass-focused-green3.log: all five selected core tests passed, including live/retired ordinary-source confirmation and actual targeted PCM. Actual Engine bypass, grant renewal and LOCAL result passed, as did exact-call cancellation, committed-policy error and successor tests.
- bypass-private-strict-green.log: rendered private success/failure-once/no-RF/retired matrix passed; all five outgoing isolation regressions passed; core/product all-target Clippy and formatting passed.
- The first private test attempt inspected lazy speech before rendering. Its final version uses the existing finite private-stream helper, requiring successful completion and no second reply before counting opened speech. No production adjustment or privacy relaxation was needed.
- This evidence qualified the independent recovery leaf de2f042. Its subsequent integration and composed/aggregate checks are recorded below; it did not by itself qualify a PR, hardware or release.


## Integrated composition checkpoint
- Isolated replay onto exact 1285f8f produced e478913. The only conflict was append-only Host methods; both the exact IVR forwarding block and exact P14 block were retained.
- bypass-union-wire.log exited 0 using freshly rebuilt, hashed own product/SIP DSOs: real RF unlock/bypass Local case 82.97 s, All case 82.59 s, unchanged linked-IVR/isolation case 72.54 s, then three core and eight product focused tests. The existing 90-second case bounds were unchanged.
- Both new cases preserve default isolation before bypass, then test callee/RF/ordinary audience, exact mix-minus, no command/result PCM to caller or ordinary peer, bypass after the two-second unlock expires, and same-transport restoration/teardown. Exact successor reset is additionally covered by the Host matrix.
- One review finding added the ordinary-peer zero-PCM assertion before clearing its command-phase observation. Local case reported two skipped synthetic callbacks; the other two reported zero. No timing or privacy assertion was relaxed.
- Final local aggregate/static qualification completed below. No P14 deployment or hardware acceptance is claimed.

## Final local qualification
- Exact source/test checkpoint f506b858 on base 1285f8f: full core 823 unit plus 94 integration tests and product 883 unit plus 5 integration tests passed; one pre-existing product test remains ignored. Product duration was 418.79 seconds.
- Standalone base passed 324 unit, 2 binary, 30 integration and 1 documentation test, including the incoming and ordinary-link compositions. Thirteen outgoing compositions were excluded from this base run. The two new Local/All cases and existing linked-IVR case passed separately above; the remaining ten retain their prior 1285 qualification and were not rerun in this P14 increment.
- Fresh own product/SIP libraries passed the C ABI harness. All-target Clippy and warning-denying Rustdoc passed for core, product, standalone, Asterisk adapter, peer support and SIP adapter; formatting and diff checks passed.
- Both bounded aggregate launchers exited zero. They used freshly pulled quality image digest 4e62696d8e6809b7b716c3c81d70b59ed9ab5b109a7539dc97c855dc7d41e844 and unchanged locked dependencies. No codec/G.722 or RNNoise candidate was included.
- Independent review maps the production replay and corrected privacy fixture to f506b858. This final documentation record changes no production or test code.
- Remaining gates are the separately selected native/hardware milestone and full repository PR requirements, including native platform packaging/installation and production coverage. No push, merge, activation or hardware qualification is claimed here.
