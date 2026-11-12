# Phone Patch Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans for native execution. Independent repository tasks may be delegated; preserve the task's test-first cycle and use a fresh whole-change reviewer before integration.

**Goal:** Implement the approved phone-patch, DTMF-macro and half-duplex receive-opportunity design through the existing peer and radio owners.

**Architecture:** Transport and control remain outside audio callbacks. Phone
legs use the existing inbound rings, shared processing graphs and network fanout;
only RF-bound programme receives the additional bounded presentation pacing.
Generation-scoped control owns call admission, private replies and commands.

**Tech Stack:** Native Rust/C ABI providers, existing FFmpeg/SWR, Earshot,
Tokio, versioned IAX/codec/jitter/ring providers, native Debian 13 QA.

**Spec:** [Approved design](../specs/2026-11-09-phone-patch-and-half-duplex-design.md).

**Execution:** Native tools. On 2026-11-09 the owner explicitly prohibited and
uninstalled Hybrid Coder. Do not invoke its skills, plugin, model-routing policy,
contract runner or Temporal workflow. Its availability is not a prerequisite.

## Global constraints

- Preserve P01–P18 in the approved spec. Defaults: manual or VAD admission,
  five-second receive deadline, 1000 ms VAD hang, one attachment/call, one reverse
  call/node, 2000 ms comma pauses, 4096 symbols and 120 seconds per expansion.
- Target added local age below 250 ms normally, 500 ms transiently. Include
  hidden conversion/DSP/queue delay; retain source time and discard when needed.
- Hardware output stays 48 kHz. Default RF catchup is at most 1.02 times normal;
  the converter ratio is output/input, so faster presentation uses `1/speed`.
- No extra inbound clock estimator, copied DSP, PBX dependency, ASL handshakes on
  phone legs, or public numeric node identity invented for a call.
- Macro expansions cannot contain login/unlock credentials. Grants are acquired
  interactively and checked per step. Lock/revoke may clear only the same source.
- Keep current Rust 1.85 behavior/build claims until a separately verified
  dependency/toolchain decision changes them; no silent compiler-floor change.
- Preserve repository formatting/static/docs/native package/install gates and
  100% production line/branch coverage on amd64 before merge. Freshly pull test
  images; use labeled launchers and clean only owned containers.
- No live-node installation, phone-account mutation or RF test is authorized by
  design approval. Prepare concrete native candidates and report acceptance gaps.

## Review focus

1. Source replaced by another call with the same mailbox/node: no old grant,
   reply, queued voice or delayed macro reaches the replacement. Tasks 3–7 test it.
2. A private reply enters ordinary mixed peer audio: command-only callers must
   never create RF demand or broadcast credentials/responses. Tasks 3/4/6 test it.
3. DTMF arrives while a macro step or RF window is due: live input wins, tagged
   audio stays muted and OOB control remains usable. Tasks 2/5/7 test it.
4. Pauses recur faster than slight acceleration can recover: deadline/discard
   bounds include backend history, not only FIFO length. Tasks 1/5/7 test it.
5. Configuration reload fails midway through preparation: the old coherent
   generation continues and new contexts are reclaimed off audio. Tasks 1–7 test it.

## Baseline and sequence

RPT starts at `1f06fdc5d5e6e4b443621d196090f9dffa1addb5` plus the owner's
approval record. The samplerate dependency pin is
`55e09c0152afd44aa6cae164006186172bd06249`; its current local filter-work branch
contains unrelated changes and must be preserved. Ring, radio and SDK likewise
need explicit released-cohort pins before consumer promotion.

Native prerequisite checkpoint, 2026-11-09: samplerate `v0.2.0-alpha3`, Radio
`v0.1.0-alpha.11`, and ring `v3.0.0-alpha5` are merged, fully gated and
released. Signed APT indexes and both architectures' runtime/dev bytes match
their release assets. Controller consumption and RF feedback/scheduling remain
separate work; no live station acceptance is implied by these provider releases.

Tasks 1 and 2 can proceed independently. Tasks 3/4 establish phone identity and
transport. Task 5 depends on the prepared media capabilities, Task 6 on typed
call/control sources, and Task 7 integrates them. Task 8 governs publication and
manual acceptance. Do not label the whole feature implemented after a leaf task.

### Task 1: Prepared manual RF pacing and media metadata

**Files:** samplerate `src/lib.rs`, `src/tests.rs`, public descriptor header,
`tests/swr_contract.c`, `tests/descriptor_smoke.c`; ring descriptor/consumer
implementation; radio `src/vad.rs` and descriptor; RPT
`rust/product/src/link/ring.rs`, `rust/product/src/host.rs`,
`rust/core/src/link/audio.rs`, `rust/core/src/link/peer.rs`; ADRs 0003/0022/0025/0035.

**Interfaces:** Preserve existing samplerate ABI2 `create/process/reset/destroy`
and its nominal ±1000 ppm contract. Append a sized optional `create_pacing`
constructor taking input/output rates, maximum frame counts, absolute minimum/
maximum output/input ratios, and `out_converter`. Accept only finite ordered
bounds containing nominal within the prepared safety envelope; ordinary handles
retain ordinary limits. Consumers require the descriptor tail before using it.
The same process/reset/position operations serve the explicitly paced handle.

Expose a sized ring timed-render result containing actual accepted source
positions and admission anchors, not diagnostics relabeled as timestamps.
Propagate it through `InboundConsumer`/`ProcessedPeer`/`PeerInput`. Preserve the
existing untimed descriptor prefix and report absent capability explicitly.
Expose a prepared shared Earshot analysis handle for source-rate mono PCM with
instantaneous voice positions separate from admission hang. Detector conversion
is analysis-only; programme conversion remains in its inbound ring.

- [x] Amend ownership ADRs before code: intentional RF presentation scaling is
  distinct from inbound source conversion/clock recovery; final DSP stays shared.
- [x] Add failing samplerate tests for `1/1.02` acceptance on pacing handles,
  rejection on ordinary handles, bounds/NaN/count errors and null output on setup
  failure; retain existing backend-injection cases and descriptor prefix checks.
- [x] Implement the optional constructor with prepared bounded storage. Warm
  the complete ratio range and scrub stale history on reset without allocation.
- [x] Prove actual input/output counts, direction, partition invariance, rapid
  ratio changes and live source-position behavior against real SWR. Extend the
  existing C allocation guard to pacing and reset, not just Rust mocks.
- [x] Add failing timed-ring and shared-classifier tests, then implement the
  sized capabilities. Test source gaps, partial acceptance, discontinuity/reset,
  source-rate conversion and raw-voice versus hangtime independently.
- [x] Add phone latency policy to both `InboundPolicy` and
  `ReceiveState::should_render`; preserve ordinary 260 ms peer targets. Tests must
  show the phone path does not inherit those targets or stack another delay ring.
- [x] Run targeted native provider tests, then formatting/lint/static/docs and
  versioned ABI/package/install checks before provider PRs. Commit each provider
  with its developer documentation; promote only released dynamic dependencies.

Focused commands in their prepared native environments: samplerate `make test`
and `make lint static-analysis`; RPT `cargo test --locked -p rptadv-product
link::ring` and `cargo test --locked -p rpt-advanced-core link::peer`.

### Task 2: Direct-trigger DTMF macros for existing sources

**Files:** create `rust/core/src/dtmf_macro.rs` with paired tests and
`rust/core/src/config/dtmf_macros.rs`; modify config scope/schema/warnings,
`command.rs`, `runtime/dtmf.rs`, `runtime/links.rs`, `runtime/aggregate.rs`,
product host/lifecycle pump tests, localized messages, ADR 0023.

**Interfaces:** `MacroStep::{SelectTarget(String), Command(String), Pause(u64)}`;
`CompiledDtmfMacro { name, trigger, protected, steps }`; a pure compiler consuming
resolved settings, limits, the effective command map and target classification.
`ActiveDtmfMacro` owns its source/generation, target, next step, start/due/deadline
and invocation token. It returns ordinary source-tagged command operations.

Completed in controller `v0.1.0-alpha23` after the native amd64/arm64 full gate,
100% production line/branch coverage and signed package publication. The
checklist below records the implemented current-node RF/IAX scope; phone and
verified private remote targets remain dependent work.

- [x] Add failing config/compiler cases for `[dtmf_macro label]` and
  `[dtmf_macro node label]`, direct `trigger`, `protected`, `commands`, global/node
  inheritance, invalid fallback and unchanged existing typed `[macro]` entries.
  Unconditionally redact command-string diagnostics, including rejected strings.
- [x] Implement the separate resolver/compiler. Use explicit `#` boundaries;
  do not replay the human collector, which auto-executes some commands. Whitespace
  is insignificant, commas occur only after completed commands, repeated commas
  add checked durations, and selector `#` is consumed by selection.
- [x] Test exact 4096-symbol/120000-ms boundaries, overflow, missing terminator,
  remapped selectors/unlock/login, prefix collisions, nested invocation rejection,
  leading digits and short dial strings. Compile before generation publication.
- [x] Factor complete-command authorization from human collection. Preserve the
  live source grant; `for_source()` must not be used to clone it. Implement a
  macro self-revoke using the resolved bare lock operation, clearing only that
  source without storing a lock credential. Test lock then protected command.
- [x] Admit direct triggers through the combined command namespace. A protected
  macro requires live unlock on admission and after every pause; unprotected
  macros cannot elevate contained commands. Macro admission/pauses do not renew
  unlock expiry. Reject credential-bearing expansions after remapping/resolution.
- [x] Advance bounded work from the existing monotonic control pump after fresh
  input. Preserve actual RF-unkey survival; cancel on new live input, Lost,
  disconnect/replacement, permissions, reload, unload or explicit cancellation.
  Carry the invocation token through queued dispatch to reject canceled work.
- [x] Test fake-clock expiry, independent sources, no blocking during commas,
  unchanged human target selection, cancel-after-queue and same-process reload.
  Run focused core/runtime/product tests, then commit the integrated macro feature.

Focused commands: `cargo test --locked -p rpt-advanced-core dtmf_macro`, existing
`runtime::dtmf_tests`/`runtime::links_tests`, and affected product pump tests.

### Task 3: Typed private phone source and command-only reply path

Native alpha24 foundation checkpoint: typed call/source and compatible C metadata,
command-only ownership, private status/login streams, salted mailbox PIN
verification, exact retirement fences and the pure telephone grammar are
implemented. Mailbox and timed-consumer leaves have native production coverage;
the combined candidate still requires its full hosted gate. Other private
report families, provider ownership, audio attachment/DSP and remote
capability negotiation remain dependent work. This checkpoint is not complete
phone support or complete Task 3.

**Files:** `rust/core/src/link/identity.rs`, `runtime/links.rs`, `runtime/dtmf.rs`,
`rust/product/include/rptadv_product.h`, `services.rs`, `link/session.rs`, host
services/attachments, standalone/Asterisk adapters, ADRs 0023/0026/0037/0038.

**Interfaces:** Distinct provider account, mailbox ID, opaque call ID and call
generation; a typed `Phone` protocol/capability rather than decimal node identity.
An explicit control-only call reply sink is independent of RF audio attachment.
Operations preserve actual ingress and generation through dispatch and replies.

- [x] Write failing tests admitting private phone sources with no ASL handshake,
  directory/topology publication, accidental local/trusted `reply=None`, or RF
  demand. Retain ordinary ASL/Echo behavior and stale-connection tests.
- [x] Implement typed source admission and source-private control/reply routing
  using sized host ABI extensions only where existing extension points cannot
  carry it. Update host/table compatibility checks, package cohort and adapters.
- [x] Add command-only calls to the source-scoped collector/grant lifecycle.
  Mailbox login cannot unlock node administration. No claimed origin is identity.
- [ ] Negotiate remote call-origin, private-reply and retirement capabilities;
  retire collectors/grants when a call ends. Test bounded-origin exhaustion and
  cleanup; reject unsupported protected/private legacy operations explicitly.
- [ ] Run existing private-reply/replacement tests plus new call tests on both
  host paths; document compatibility and commit before enabling phone macros.

### Task 4: Native SIP/IAX provider sessions

**Files:** keep the owned parser and shared provider prerequisites in
`rust/peer-support`, with host composition in the existing standalone/Asterisk
boundaries; extend the existing released IAX provider for general account/registration only where needed;
reuse product codec, jitter, async-owner and egress interfaces; dependency manifests,
test-only provider fixtures and transport ADR.

**Interfaces:** Serial account/connection owners submit typed call events and
canonical source PCM to Task 3. Existing per-call ingress/egress own packet
order, jitter, codec state, prepared packets and bounded deadlines.

- [x] Historical only: resolve and build the disposable native dependency probe
  for the former `rsipstack 0.8.2` signaling candidate and media leaf crates.
  Preserve its recorded compatibility evidence below, not candidate selection or
  production readiness. The current choice is owned in-house SIP signaling,
  beginning with the bounded parser prerequisite; vendor repairs are out of scope.

  Historical probe: `rsipstack 0.8.2`, `sdp 0.8.0`, `rtp/rtcp 0.13.0`,
  `webrtc-srtp 0.15.0` and `webrtc-util 0.11.0` compiled in the earlier
  compatibility probe. Newer leaf crates were rejected. SIPp scenarios covered
  only the disposable probe, not production readiness. Current workspace builds
  require Rust 1.88; the independent jitter-client wrapper retains Rust 1.85.
  No Rust 1.91 upgrade or unverified 1.85 production-graph claim is made.
- [x] Implement the experimental [owned bounded parser prerequisite](2026-11-12-bounded-sip-parser.md)
  in shared `rptadv-peer-support`, with RFC-derived framing/header/URI tests,
  independent torture fixtures and measured memory/allocation bounds. The
  2026-11-12 selection rejects rsipstack's unbounded state, RVoIP's runtime/
  resolver/toolchain ownership, and unmodified oSIP's allocation-failure cleanup.
  No vendor patching or third-party signaling stack is selected. This leaf
  alone does not supply a transaction engine or a usable SIP provider.
- [ ] Write failing independent-provider REGISTER/digest, INVITE/ACK, answer,
  CANCEL/BYE, reject/timeout and resource-cleanup scenarios. Implement signaling
  outside callbacks, retaining separate transport/media/control boundaries.
- [ ] Implement PCMU/PCMA 8 kHz mono, 20 ms RTP with the existing dynamic G.711
  profiles. Verify SDP payload/clock/ptime and RTP timestamp wrapping. Add optional
  Opus only after its profile/framing fixtures pass; no IAX-format parity claim.
- [ ] Add bounded RFC 4733 events/end repetition/dedup with in-band detection.
  For SRTP explicitly enable replay protection and separate inbound/outbound
  contexts; verify key negotiation and fail-closed behavior. A crypto context is
  not a complete SRTP session. Make clear which provider security profiles work.
- [ ] Extend generic IAX account username/secret, provider registration and
  call lifecycle; do not reuse ASL HTTPS registration as provider registration.
- [ ] Run in-process socket fixtures and independent SIPp/compatible IAX tests;
  commit selected locks and documented supported profiles. Real provider/NAT/
  account-policy acceptance remains an explicit later gate.

### Task 5: RF-only pacing FIFO and receive-opportunity arbiter

**Files:** `rust/core/src/link/audio.rs`, focused RF opportunity module/tests,
controller policy, product prepared/TX generation, SDK/radio callback feedback and
both host adapters; ADRs 0003/0025/0035/0042/0043 as affected.

**Interfaces:** A prepared pacing port consumes source-timed programme and
renders exactly the requested DAC count. Structured radio feedback carries
physical duplex, request/service/observation validity and generation, STE/settle/
rekey phase. `transmit_keyed` alone is requested state, not observed off state.

- [ ] Add failing fake-clock/partition tests for RF FIFO pause/resume, oldest
  source age, `1/speed`, 500 ms cap including converter history, silence trimming,
  bounded discard/smoothing and current versus queued source demand.
- [ ] Insert the FIFO after fresh peer processing/mixing and before controller
  local output. Preserve fresh dispatcher timestamps and network mix-minus during
  RF windows. Keep telemetry, regeneration, signaling and final PCM outside it.
- [ ] Add feedback through the existing sole radio owner; audio never polls
  control/site state or calls hardware/network APIs. Reject absent/unknown off
  evidence as a completed opportunity. Test both host paths and all generations.
- [ ] Implement sample-clocked programme/warning/STE/off/settle/listen/RF-turn/
  rekey states. Plan backward from usable listening by five seconds. Respect
  minimum listening and natural-pause extension, with no invented maximum timer.
- [ ] Integrate instantaneous Earshot pauses, VAD hang/pre-roll and independent
  manual admission; manual commands never override VAD. Local RF retains its
  entire qualified turn; full-duplex capability omits windows.
- [ ] Test telemetry/mandatory ID/safety reservations, impossible timing and
  missed-check diagnostics. Revoke queue and backend history on DTMF, cohort/
  permission changes, discontinuity or retirement. Destroy only after quiescence.
- [ ] Run core audio/controller, product worker/dispatcher/lifecycle, SDK/radio
  and standalone/Asterisk adapter tests. Commit after all targeted checks pass.

### Task 6: Mailboxes, DTMF grammar, attachments and call policy

**Files:** focused core/product phone configuration/control modules and tests,
source-private message catalog, command mappings, host call lifecycle and routing.

**Interfaces:** Invited/dialing → authenticating → command-only → attached →
closing. Call-scoped isolation claims and unlock-authorized bypass are separate
from the provider account and mailbox authentication.

- [x] Test/implement salted mailbox/PIN records with first name/callsign, bounded
  attempts, private prompts and teardown. Every incoming caller authenticates.
- [ ] Test/implement ordinary telephone digits, `*3` default/explicit attachment,
  local/remote `*4`, `**` one-symbol quoting, bare command-mode `#` exit and quoted
  target terminator. Test IVR star/pound menus, timeout and reverse no-echo.
- [ ] Implement call-local manual key/unkey actions and remapping/collision
  checks; VAD mode reports them unavailable. Enable macro execution for phone
  sources only through Task 3's verified identity/private sink.
- [ ] Implement explicit target allowlists, attachment limits and busy responses
  without replacement. Default one node/call and one reverse call/node; account
  incoming limits remain with the provider. Verify mix-minus conferences.
- [ ] Acquire bidirectional peer isolation before outgoing dialing. Reference-
  count per-call claims, revoke buffered media, and release only that call's
  claim on failure/hangup. Bypass always requires the initiating node unlock,
  even under remote-base Open. Test overlapping calls and revoked bypass.
- [ ] Implement local/all-eligible-peer playout and independent discontinuous
  voice admission; preserve OOB control during complete DTMF media suppression.
- [ ] Preserve provider destination strings/short numbers; no duplicate cost,
  international/premium or E911 policy engine. No automatic emergency macro.
- [ ] Run complete call/control/macro/routing lifecycle fixtures and commit.

### Task 7: Composed integration and performance

- [ ] Exercise real composed components through a test-only radio/provider:
  authenticate, control privately, connect explicitly, alternate RF/caller turns,
  enter commands/macros, bypass with unlock, hang up and reconnect/reload.
- [ ] Prove first detected DTMF removes buffered leading/trailing audio, OOB
  commands survive and private replies never become ordinary mixed programme.
- [ ] Exercise continuous speech with 200+100+200 ms switching costs, forcing
  discard when slight speedup is insufficient. Measure admission-to-DAC age,
  first-syllable latency, count/partition invariance and callback allocations.
- [ ] Exercise provider loss, codec failure, both call directions, transport
  reordering/loss/DTMF duplication, reload rollback, unload and same-ID replacement.
- [ ] Record native amd64/arm64 results and dependency identities. Do not claim
  physical switching, STE, NAT/provider or listening acceptance from fixtures.

### Task 8: Documentation, quality and manual acceptance handoff

- [ ] Update manuals/examples/messages/ADRs/wishlist to match actual supported
  behavior, compatibility and remaining manual acceptance, just before PRs.
- [ ] Run fast preflight, obtain fresh independent correctness and ownership
  review, fix actionable findings with focused failing/passing tests.
- [ ] Run hosted native Debian 13 PR gates, packaging/staged installs and amd64
  100% production coverage before merge. Cross-repository consumers use released
  dynamic prerequisites; verify exact input commits and dependency pins.
- [ ] Prepare reviewable provider and physical-radio test configurations with
  immutable package/source identities. Obtain any missing deployment authority
  before node/account/RF side effects. Verify physical OFF before RF work.
- [ ] Report completed software, published packages, actual installation and
  physical/provider acceptance separately. Keep unmet acceptance visible.

## Requirement coverage

| Requirements | Tasks |
| --- | --- |
| P01–P02 | 3, 4, 7, 8 |
| P03–P06 | 3, 6, 7 |
| P07–P12 | 1, 5, 7, 8 |
| P13–P17 | 3, 4, 6, 7 |
| P18 | 2, 3, 6, 7 |

Plan self-review: all P01–P18 have implementation and composed verification
tasks; the five review-focus failures are assigned above. Dependency adoption
requires a real build probe. Interface details not public in the approved design
are builder decisions recorded in the implementation ledger before changes.
