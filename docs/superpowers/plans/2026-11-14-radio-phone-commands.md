# Radio-origin telephone commands implementation plan

> Execute test-first in this isolated continuation; reuse the existing source reviewers and serialized constrained build slot.

**Goal:** Originate and terminate the existing peer-free telephone call from configured local-radio DTMF actions.

**Architecture:** Extend the existing command catalog and collector, then route a typed local-only effect into the current outgoing claim and dial job. Resolve the node account, destination and claim together on serialized control. Retain the same SIP reactor, RTP socket, media-only role and exact cancellation owner.

**Tech stack:** Rust core/product/standalone, existing C provider interfaces and Sofia SIP adapter. No new provider ABI.

**Spec:** `docs/superpowers/specs/2026-11-09-phone-patch-and-half-duplex-design.md` (P13-P18), ADR0044 and WISHLIST phone patch.

## Constraints and defaults

- Add configurable `link_command_phone_dial` and `link_command_phone_hangup`. Their shipped defaults are empty, consistent with optional remote-base/Echo actions; no current macro or command acquires new meaning.
- Add inherited `phone_outgoing_account`, empty by default. Validate an explicit identifier with the existing PhoneAccountId parser; malformed overrides warn and inherit. Missing/disabled/ineligible account rejects before INVITE.
- Fixtures use ASL-style dial 6 and hangup 0 after confirming no shipped collision. Custom mappings remain supported. No live configuration is changed.
- Decimal destination bytes stay literal, bounded to 63 and validated through existing SIP URI rules after combination with the configured account server. No last-node substitution, country rewriting, emergency exception or duplicated provider cost policy.
- First route is local RF only and peer-free for the exact outgoing call. No incoming caller or ordinary peer gains telephone origination/control authority.
- Source-only result routing applies. Callee receives neither login/control prompts nor RF command-result telemetry. Acceptance is not answer completion.
- Preserve incoming 10-second phone timing and ordinary 3-second RF/peer collection timing. Hardware output and a real provider call remain separate manual validation.

## Review focus

- Literal0 and leading zeros must not recall or overwrite an ordinary remembered link.
- A reload between parsing/reserving/answer must not combine old account configuration with a new call generation.
- A delayed hangup/failure result must not cancel or announce success for a successor claim.
- Peer/phone-origin commands must be rejected without transmitting digits, numbers or prompts to another endpoint.
- Cancellation before answer and source resets after answer retain existing no-INVITE/current-source and media-only guarantees.

## Task 1: Configured typed commands

Files: core command.rs, config/settings.rs and schema.rs, runtime/dtmf.rs and links.rs; adjacent tests.

- [x] Record behavioral RED for configured key recognition and literal phone number collection using existing APIs.
- [x] Add PhoneDial/PhoneHangup mappings; typed LinkEffect with local-only source checks. Bypass ordinary last-node semantics, not ordinary collector timing.
- [x] Add inherited optional account selector using existing identifier validation. Test empty/default, override, invalid inheritance and prefix/macro collisions.
- [x] Run focused command/config/source checks and strict core checks; source review and recovery commit.

## Task 2: Existing claim and job bridge

Files: product lifecycle.rs and lifecycle_radio_phone.rs, host_phone_outgoing.rs, phone_outgoing.rs; existing outgoing lifecycle tests.

- [x] Record behavioral RED for actual RF command reserving before factory and exact local hangup.
- [x] Resolve account/server and reserve in one captured Engine revision turn. Share existing asynchronous launch body with the administrative path; no new queue/thread.
- [x] Capture the exact pending/active claim for hangup; asynchronous completion reports only the current actual outcome. Source-only LOCAL result handling must not retire a callee.
- [x] Test refusal, disabled/missing account, peers present, wrong source, reload/cancel and successor custody using existing fixtures.
- [x] Run focused product/host tests and strict checks; source review and recovery commit.

## Task 3: Actual controller wire proof

Files: standalone product_sip_outgoing_tests.rs and existing synthetic local-input helper, ADR0044 and this plan.

- [x] Drive configured DTMF through actual radio input and the product Engine, preserving the existing answered SIP/RTP two-way marker assertions.
- [x] Prove RF hangup, callee no-login/control, and source-only command-result output. Retain bounded child watchdogs and exact isolated provider DSOs.
- [x] Import staging's reviewed incoming source-only fixture adaptation verbatim, then run outgoing/incoming/ordinary-link compositions, affected suites and strict checks under serial bounded jobs.
- [x] Save reviewed commit and bundle; report native/manual and full linked-isolation gates separately. No live call or deployment.

Estimated remaining active development at start: 3-5 hours, excluding shared-slot waits, native staging and manual destination/cost approval.

## Incremental verification (2026-11-15)

The configured command/literal-number/source-boundary regressions first failed behaviorally, then all four passed. The existing command filter also passed 51 unit and 6 integration cases. Product admission first rejected the configured radio action. Its shared-job implementation then passed all six outgoing lifecycle cases, including the existing administrative owner tests.

A separate held-Host regression proved that capturing time before serialized admission let an expired macro hangup cancel its call. Capturing time inside that same admission closure made the exact regression pass. The first attempt at this test timed out because its control fixture executes synchronously; only the corrected actual Host-lock assertion is behavioral RED evidence.

These are targeted recovery-checkpoint results, not full qualification. Expanded outcome/race coverage, strict checks and the actual radio wire fixture remain in progress. The uncommitted wire fixture is kept separate from this production checkpoint; it has reached an answered SIP dialog and callee RF media but its finite solicited RF prompt must be separated from the later VOX observation window. No native/manual call or physical delivery proof is claimed.


Actual RF wire checkpoint: the configured 6/0 fixture passed through the existing native receive callback, DTMF decoder and Engine into SIP INVITE/answer/ACK. It retained literal 00123, admitted no incoming mailbox, carried callee VOX to local RF and the known 1 kHz local RF pattern to the callee, denied callee control digits, and ended with RF hangup/BYE. The 46.49-second test passed together with all seven lifecycle cases and the C ABI probe. Solicited LOCAL Morse output is observed to finish before the unchanged voice/VOX window; neither speech nor its tail was lengthened. A separate actual-outcome matrix proves submission is silent, answer/refusal/job-failure results are exact, administrative/macros stay silent, and cancellation/revision fences reject late outcomes independently. This remains synthetic radio I/O and loopback SIP, not a physical/public-provider call.

The reviewed source-only incoming composition and macro assertion adaptation from969c65a was imported unchanged. Its combined execution remains part of the next affected suites.


## Final development qualification (2026-11-15)

The final production/test tree is ede6441d584820263c08349a32317de832e91c95. It includes independently reviewed fixed-category private-command diagnostics 8124af, replayed without conflict. No behavior was changed after the preceding source review. The final verification record is documentation-only.

Fresh serialized constrained runs passed:

- Core: 803 unit tests and 94 integration tests.
- Product: 866 unit tests and 5 integration tests. The pre-existing full-path egress load case remains ignored.
- Standalone: 320 library tests, 2 binary tests, 30 integration tests and 1 doctest. This includes administrative outgoing, actual RF-DTMF outgoing, incoming phone-origin remote connect and incoming attachment to an already connected ordinary peer.
- Asterisk: 66 tests; peer support: 13 unit and 82 integration tests; SIP adapter: 13 unit tests.
- Exact product C ABI probe, six-package all-target Clippy with warnings denied, Rustdoc with warnings denied and workspace formatting checks.

All three final runners ended with launcher exit 0. The development product DSO built from the exact final source has SHA256 cd303b1e24be1a8844ede724f286211e5f0d07e158abd1ad950924ba72c9ef9e; the SIP adapter has SHA256 b3d352592388e358d5d95f7a727c395f0fd9d1592f36fb219f221fa2e1e989f3. Runtime validation must rebuild and pin the matching native cohort, not copy development binaries or reuse a divergent worktree target.

No live configuration, public-provider call, native activation or publication occurred. Native qualification and an explicitly approved peer-free destination/cost test remain separate. Full linked-peer call isolation, telephone-IVR forwarding/manual phone controls and half-duplex delivery/timing milestones are not claimed complete. The repository-wide PR platform/coverage gate has not been run by this increment.
