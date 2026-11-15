# Radio-origin telephone commands implementation plan

> Execute test-first in this isolated continuation; reuse the existing source reviewers and serialized constrained build slot.

**Goal:** Originate and terminate the existing peer-free telephone call from configured local-radio DTMF actions.

**Architecture:** Extend the existing command catalog and collector, then route a typed local-only effect into the current outgoing claim and dial job. Resolve the node account, destination and claim together on serialized control. Retain the same SIP reactor, RTP socket, media-only role and exact cancellation owner.

**Tech stack:** Rust core/product/standalone, existing C provider interfaces and Sofia SIP adapter. No new provider ABI.

**Spec:** `docs/superpowers/specs/2026-11-09-phone-patch-and-half-duplex-design.md` (P13-P18), ADR0044 and WISHLIST phone patch.

## Constraints and defaults

- Add configurable `link_command_phone_dial` and `link_command_phone_hangup`. Their shipped defaults are empty, consistent with optional remote-base/Echo actions; no current macro or command acquires new meaning.
- Add inherited `phone_outgoing_account`, empty by default. Validate an explicit identifier with the existing PhoneAccountId parser; malformed overrides warn and inherit. Missing/disabled/ineligible account rejects before INVITE.
- Fixtures use ASL-style dial6 and hangup0 after confirming no shipped collision. Custom mappings remain supported. No live configuration is changed.
- Decimal destination bytes stay literal, bounded to63 and validated through existing SIP URI rules after combination with the configured account server. No last-node substitution, country rewriting, emergency exception or duplicated provider cost policy.
- First route is local RF only and peer-free for the exact outgoing call. No incoming caller or ordinary peer gains telephone origination/control authority.
- Source-only result routing applies. Callee receives neither login/control prompts nor RF command-result telemetry. Acceptance is not answer completion.
- Preserve incoming10-second phone timing and ordinary3-second RF/peer collection timing. Hardware output and a real provider call remain separate manual validation.

## Review focus

- Literal0 and leading zeros must not recall or overwrite an ordinary remembered link.
- A reload between parsing/reserving/answer must not combine old account configuration with a new call generation.
- A delayed hangup/failure result must not cancel or announce success for a successor claim.
- Peer/phone-origin commands must be rejected without transmitting digits, numbers or prompts to another endpoint.
- Cancellation before answer and source resets after answer retain existing no-INVITE/current-source and media-only guarantees.

## Task1: Configured typed commands

Files: core command.rs, config/settings.rs and schema.rs, runtime/dtmf.rs and links.rs; adjacent tests.

- [x] Record behavioral RED for configured key recognition and literal phone number collection using existing APIs.
- [x] Add PhoneDial/PhoneHangup mappings; typed LinkEffect with local-only source checks. Bypass ordinary last-node semantics, not ordinary collector timing.
- [x] Add inherited optional account selector using existing identifier validation. Test empty/default, override, invalid inheritance and prefix/macro collisions.
- [ ] Run focused command/config/source checks and strict core checks; source review and recovery commit.

## Task2: Existing claim and job bridge

Files: product lifecycle.rs and lifecycle_radio_phone.rs, host_phone_outgoing.rs, phone_outgoing.rs; existing outgoing lifecycle tests.

- [x] Record behavioral RED for actual RF command reserving before factory and exact local hangup.
- [x] Resolve account/server and reserve in one captured Engine revision turn. Share existing asynchronous launch body with the administrative path; no new queue/thread.
- [x] Capture the exact pending/active claim for hangup; asynchronous completion reports only the current actual outcome. Source-only LOCAL result handling must not retire a callee.
- [ ] Test refusal, disabled/missing account, peers present, wrong source, reload/cancel and successor custody using existing fixtures.
- [ ] Run focused product/host tests and strict checks; source review and recovery commit.

## Task3: Actual controller wire proof

Files: standalone product_sip_outgoing_tests.rs and existing synthetic local-input helper, ADR0044 and this plan.

- [ ] Drive configured DTMF through actual radio input and the product Engine, preserving the existing answered SIP/RTP two-way marker assertions.
- [ ] Prove RF hangup, callee no-login/control, and source-only command-result output. Retain bounded child watchdogs and exact isolated provider DSOs.
- [ ] Import staging's reviewed incoming source-only fixture adaptation verbatim, then run outgoing/incoming/ordinary-link compositions, affected suites and strict checks under serial bounded jobs.
- [ ] Save reviewed commit and bundle; report native/manual and full linked-isolation gates separately. No live call or deployment.

Estimated remaining active development at start:3-5hours, excluding shared-slot waits, native staging and manual destination/cost approval.

## Incremental verification (2026-11-15)

The configured command/literal-number/source-boundary regressions first failed behaviorally, then all four passed. The existing command filter also passed51 unit and6 integration cases. Product admission first rejected the configured radio action. Its shared-job implementation then passed all six outgoing lifecycle cases, including the existing administrative owner tests.

A separate held-Host regression proved that capturing time before serialized admission let an expired macro hangup cancel its call. Capturing time inside that same admission closure made the exact regression pass. The first attempt at this test timed out because its control fixture executes synchronously; only the corrected actual Host-lock assertion is behavioral RED evidence.

These are targeted recovery-checkpoint results, not full qualification. Expanded outcome/race coverage, strict checks and the actual radio wire fixture remain in progress. The uncommitted wire fixture is kept separate from this production checkpoint; it has reached an answered SIP dialog and callee RF media but its finite solicited RF prompt must be separated from the later VOX observation window. No native/manual call or physical delivery proof is claimed.


Actual RF wire checkpoint: the configured6/0 fixture passed through the existing native receive callback, DTMF decoder and Engine into SIP INVITE/answer/ACK. It retained literal00123, admitted no incoming mailbox, carried callee VOX to local RF and the known1kHz local RF pattern to the callee, denied callee control digits, and ended with RF hangup/BYE. The46.49-second test passed together with all seven lifecycle cases and the C ABI probe. Solicited LOCAL Morse output is observed to finish before the unchanged voice/VOX window; neither speech nor its tail was lengthened. A separate actual-outcome matrix proves submission is silent, answer/refusal/job-failure results are exact, administrative/macros stay silent, and cancellation/revision fences reject late outcomes independently. This remains synthetic radio I/O and loopback SIP, not a physical/public-provider call.

The reviewed source-only incoming composition and macro assertion adaptation from969c65a was imported unchanged. Its combined execution remains part of the next affected suites.
