# Incoming SIP Source Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recover an already-authorized incoming SIP receive path after a proved
adaptive cut using a fresh generation, never old media or synthetic authority.

**Architecture:** A cause-specific ordered cut can arm one revision/epoch-bound
owner request. Fresh original source evidence plus ordinary current collector
and policy approval enables exact private graph replacement/preparation/adoption.
Old input,
hold and courtesy remain permanently fenced.

**Tech Stack:** Existing Rust controller/standalone, sized C source ABI1,
versioned native jitter/ring/speech providers and current constrained launcher.

**Spec:** `docs/superpowers/specs/2026-11-16-incoming-source-recovery-design.md`

**Status:** Review-only draft, base057cd668. STOP before production edits until
independent design review and root approval cover the spec's explicit scope and
optional ABI-tail gates. A fresh worker is to implement, not this author.

## Global constraints

- Incoming SIP active-origin adaptation only; IAX/outgoing contracts unchanged.
- No fabricated digit/CollectionEnded, end_command shortcut, old-input revival,
  synthetic requalification, receipt restamp, hold transfer or arbitrary washout.
- No new PCM queue, worker, recovery framework, jitter settings or RF permission.
- Development radio.onionring.rocks only; no node/RF/accounts/settings/push/release.
- Use existing exact-cohort focused runner, freshly pulled labeled maintained
  image, serial1CPU/1536MiB/20min,2GiB free-disk floor. Preserve immutable runtimes,
  headers, caches and evidence; new interface builds use a distinct candidate.
- Strict SIP RED remains an observed blocker, not permission to weaken assertions.
- Full native amd64/ARM64/package/coverage/hosted/hardware qualification remains.

## Review focus

1. Unknown receipt plus native skip in one turn must stay generic/nonrecoverable.
2. A new DTMF onset between control approval and owner adoption must win.
3. Same-node detach/reattach must reject an old queued recovery ticket.
4. Fresh original silence may prepare but cannot open RF or create old P.
5. Host replacement preserves only exact approved recovery; explicit retirement
   cancels it.

---

### Task1: Prove and carry only the native adaptation cause

**Files:** `rust/product/include/rptadv_product.h`,
`rust/product/src/services.rs`, `rust/product/src/services_time_tests.rs`,
`rust/product/src/link/private_phone.rs` (same hard-cut handler only),
`rust/standalone/src/phone/call_media.rs`,
`rust/standalone/src/phone/call_media_tests.rs`,
`rust/standalone/src/host_services/iax_phone.rs` and its sink tests;
affected literal sink initializers in standalone IAX tests and
`rust/peer-support/tests/support/host_media.rs`; existing C ABI probe and ADR0044.
Do not change jitter ABI/algorithm.

**Interfaces:** optional appended
`rptadv_peer_source_adaptation_v1(void*, uint64_t, uint32_t) -> int32_t`;
crate-private `SourceDispatch::AdaptationCut { start: u64, count: usize }`
and `PeerIo::read_source_causal_at(completion, reference, dispatch)`.
Public SourceInput and read_source[_at] stay unchanged, sharing validation and
mapping adaptation to generic Discontinuity for public callers. One private
SourceReset identity survives refused dispatch across reads.

- [ ] First preserve a before-probe against057cd668: change only the existing
  actual-native/composed-seam characterization expectation to require readiness
  after fresh original PCM without a digit. Expect assertion RED at no rearm.
  Save the probe patch/log, then restore the generic-cut negative oracle. This
  cause-free baseline probe is not the final cause-specific regression.
- [ ] Add failing `proved_native_adaptation_is_ordered_before_exact_gap_credit`:
  actual native no-gap skip160, exact interval, one callback before any later
  credit; zero/refused callback retains identical cause/range.
- [ ] Add `ambiguous_or_unknown_source_cut_never_claims_adaptation`: known
  positive prefix without matching exact advance evidence, missing receipt plus
  real skip, overflow/reset, insertion, startup and old-size/null-tail sinks.
  Generic hard cuts stay generic; insertion causes no source gap.
- [ ] Add `steady_cadence_media_adaptation_survives_real_put`: extend the actual
  no-gap native trace through production Media packet admission, not direct
  jitter.put fixture injection. At poll880 packet840 ends860, skip160 frames,
  next due900; put packet900 before get900. Require exact cause/range callback
  before gap credit and correct retry identity despite normal occupancy/delay
  gauge changes and skipped-packet cleanup. This is the mandatory Task1 positive
  gate before any owner recovery implementation.
- [ ] Add `adaptation_witness_has_one_packet_lifetime`: Accepted puts/NotDue
  preserve only the specified counter projection; put-side checked late_drops
  increase is permitted but never establishes cause. Duplicate/full puts,
  Missing/Insertion, overlap, reset, observation error/decrease/saturation,
  second advance and mismatched successor invalidate. Exercise a non-Packet get
  followed by puts, including the native >20 unsuccessful-fetch resync sequence.
  Do not compare whole Observation: queued_packets/buffered_span/delay vary.
- [ ] Add `reset_retry_keeps_exact_identity_across_reads`: refusal then identical
  retry completes once; changed range/cause and generic-to-adaptation substitution
  fail closed; later PCM gets zero credit; original owner snapshot is not retaken.
- [ ] Add actual old-layout allocation fixtures for both SIP and production IAX,
  plus null callback and each one-byte-short field boundary. A full new struct
  with smaller declared size is not this memory-safety oracle. Public SourceInput
  exhaustive-match consumer compiles unchanged.
- [ ] Run focused tests and preserve actual assertion RED separately from build
  errors. Existing `adaptive_delay_reduction_preserves_exact_source_gap_credit`
  and signed-offset tests remain unconditional.
- [ ] Implement the sized callback, validated service dispatch and bounded
  exact-native skip evidence; dispatch private AdaptationCut through the shared
  hard-cut
  handler with NO recovery yet. This compiles the interface before Task2's
  behavioral RED; do not implement recovery policy in this task.
  Check field-size boundaries before raw reads; never construct a full enlarged
  Rust sink reference over old memory. Use named old digit-tail and new callback
  minimum sizes in SIP and IAX. No hidden use of existing fields.
- [ ] Verify C prefix/tail layout/short sinks, callback retries/invalid ranges,
  actual Media steady-cadence real-put gate, skip credit and signed offsets.
  If the gate fails, stop before Task2 and report the minimum necessary contract
  change rather than substitute a buffer-only fixture. Update ADR0044 and interface
  artifacts; identify exact candidate header/DSO identities without overwriting
  reviewed cohort artifacts. Local coherent commit after targeted checks.

### Task2: Add the bounded fresh-source request and real control approval

**Files:** `rust/product/src/link/private_phone.rs`,
`rust/product/src/link/phone_media.rs`, `rust/product/src/host.rs`,
`rust/product/src/phone.rs`, `rust/product/src/link/phone_adaptation_tests.rs`,
`rust/product/src/phone_tests.rs`,
`rust/product/src/host_phone_attachment_policy_tests.rs`, and
`rust/product/src/link/session/tests.rs` (bounded cause-specific callback
fixture alongside unchanged generic source_reset).
Touch `host_phone_worker.rs` only if integration requires an existing-call-site
adjustment; keep the ordinary Engine/refresh/policy path.

**Interfaces:** private `RecoveryTicket` and one `RecoveryCut`;
`recovery_request(&self) -> Option<RecoveryTicket>`;
`approve_recovery(&mut self, &PeerReply, RecoveryTicket) -> bool`;
`PrivateCommand::Recover(RecoveryTicket)`. Extend existing allocated
source-ready shared state with packed request and checked cancellation epoch.
No public Rust API. Real consumed_revision remains digits-only.
Crate-private `PhoneCalls::retire_for_graph_replacement(&mut self,
&PhoneSource, &PeerReply) -> Result<(), Error>` replaces the host's unconditional
public retire call. Exact approved recovery preserves its ticket; all other
cases retain cancellation. Prepared/adopted media binds immutable origin epoch.

- [ ] Extend057cd668 characterization with a cause-tagged active-origin branch:
  actual native cut; old input immediately inactive; fresh independently timed
  original PCM, no new digit; actual collector-idle reconciliation; expect a
  distinct ready/adopted generation and later independent genuine output.
  Preserve generic-cut branch requiring normal new digit/release.
- [ ] Run against Task1's cause-aware but nonrecovering implementation: expect
  readiness/admission RED, not native-fixture, compiler or timing failure.
  Preserve this exact base/log separately from057cd668's before-probe.
- [ ] Implement active-origin ticket arming; capture real cut reference and
  immutable active-origin epoch,
  cancel on generic cut/onset/digit; publish only after positive original credit
  and actual detector/raw-tail drain. Synthetic-only/pre-cut receipt cannot arm.
- [ ] Implement collector/auth/attachment-gated approval through existing queue.
  Owner validates after reading source, without end_command or suppression
  clearing. Queueing is same-ticket idempotent. Use acknowledged recovery
  revision only for fresh media preparation.
- [ ] Implement cancellation-epoch invalidation on explicit retire/detach before
  existing media retirement; stale queued approvals and prepared inputs fail
  closed. Repeated eligible cuts replace one ticket, not append work.
- [ ] Implement exact private graph replacement in host.rs/phone.rs: keep public
  retire_media canceling, separate its shared generation-retire primitive from
  cancellation. Reload/policy invalidation cancels before graph preparation.
  Bind epoch at candidate creation, retain through owner queue and adoption;
  mismatches retire, never recapture a newer epoch from an old origin.
- [ ] Add `host_refresh_recovers_exact_approved_generation_after_busy`: use actual
  Host::refresh_inputs, verified idle control and original evidence; Busy/drop
  retires unpublished candidate, exact current ticket can retry with a distinct
  generation. Successful adoption consumes ticket; external retirement, detach
  or reload before retry makes it fail closed.
- [ ] Add `cancel_epoch_invalidates_active_origin_before_retirement`: pause control
  between epoch invalidation and shared-media retirement, offer an adaptation
  cut from still-active old origin; it cannot capture the new epoch or recover.
  Also cancel after approval and before queued candidate adoption.
- [ ] Add compact negative/race tests covering all spec classes: origins/protocols;
  source time/provenance/credit; collectors/commands; repeated cuts/queue full;
  exact call/account/attachment/config and candidate replacement/exhaustion.
  Specifically interleave DTMF after approval before adoption, and detach then
  reattach same node before queued approval reaches owner.
- [ ] Assert distinct generation, fresh analyzer, no old hold/courtesy transfer.
  Genuine silence stays Unavailable/no RF/no P; later genuine speech can Open,
  then its own original hold and actual DSP tail reach Closed. Keep existing
  held-close waveform and old-token cancellation oracles unchanged.
- [ ] Run guarded owner/phone/host-policy/speech/courtesy regressions; record
  bounded state size and no callback allocation/free/lock/I/O changes.
  Focused format, Clippy and affected ABI checks; local coherent commit.
  Independent implementation review before composed acceptance.

### Task3: Exact-cohort strict composed acceptance and honest handoff

**Files:** existing `.work/sdd-phone-plc-courtesy/` evidence/candidate scripts,
`.work/quality-progress.md`, applicable ADR/interface metadata.
Do not modify strict SIP assertions or positive courtesy oracles to pass.

- [ ] Build a distinct candidate product/standalone/header cohort from exact
  reviewed commits; verify all released native providers, dynamic links and
  immutable original88cb product/658a jitter manifests remain unchanged.
- [ ] In one bounded serial batch run the unchanged strict composed filter:
  `cargo test --locked -p rpt-advanced-standalone --lib real_product_sip_call_ -- --test-threads=1 --nocapture`
  through `run-task4-focused.sh` with the verified candidate selection.
  Record both existing-link and phone-origin outcomes; do not retry for green.
- [ ] If RED remains, retain exact assertion and available source chronology.
  No old-close fabrication, silence washout, adaptation disabling or automatic
  trial loop. A new diagnosis/capture needs explicit scope; do not claim the
  original failure was reproduced or solved from aggregate counters.
- [ ] Return exact base/head, test/ABI/provider identities, focused diff, RED/
  GREEN logs and review status. Distinguish recovery proof, strict SIP result,
  async station behavior and unrun full gates. Explicitly report the
  prepared/inactive attachment gap and any unproved witness cases. No live deployment.

## Reproducible focused command wrapper

Use the existing authorized Windows BatchMode SSH route, then on development:
```sh
cd /home/codex/rpt-development
RPTADV_CONTAINER_PROFILE=constrained bash rpt_advanced-workflows/tools/run-in-quality-container.sh \
 ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest /home/codex/rpt-development \
 bash /workspace/phone-plc-courtesy/.work/sdd-phone-plc-courtesy/run-task4-focused.sh \
 cargo test --locked -p rptadv-product --lib link::private_phone::tests::adaptation -- --test-threads=1 --nocapture
```
Use the actual fully qualified test/module filter if renamed; verify nonzero
selected tests. The current module is `link::private_phone::tests::adaptation`
and current test name includes `native_adaptation_cut_`. Candidate cohort
selection must be prepared explicitly for the new header, not silently reuse
the old immutable runtime. For read-only design validation only: diff check,
verify referenced files/links and confirm a docs-only local commit.
