# SIP PLC Continuity and Telephone Courtesy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Continue already-open telephone speech with provenance-safe PLC across current SIP codecs and play configured local-only courtesy after a genuine held speech close.

**Architecture:** Reuse existing codec/jitter decisions, exact source-credit leases, shared ring fallback and the phone's original-time ledger. Extend existing prepared phone metadata and courtesy scheduling, not audio ownership or provider ABIs. Implement serially because the private media and rendered-close contracts are coupled.

**Tech Stack:** Rust 1.88.0 controller workspace; released dynamic codec, jitter, ring, radio speech-analysis and Morse providers; native Debian 13 amd64/arm64 quality gates.

**Spec:** `docs/superpowers/specs/2026-11-16-phone-plc-courtesy-design.md`

## Global Constraints

- Owner correction: "Packet loss concealment should not be treated as a gap."
- No synthetic initiation of RF or DTMF, including a synthetic preroll prefix retroactively covered by later genuine VAD.
- No new global PLC duration; retain original hold, immutable source-age and existing provider bounds.
- Current SIP codecs only; no Opus/FEC negotiation, music/tone classifier, new worker, PCM queue or provider ABI.
- No publication, live-node access, deployment, RF operation or node configuration during planning.
- Preserve clean original `phone-local-speech-retention`, baseline `296b78a220570f58130ba8059b9a40e60b29a089`.
- Serial constrained validation: freshly pulled maintained image, 1 CPU, 1536 MiB, 20-minute bounded batches. Record evidence; never weaken tests or remove caches/sources to make room.
- Safety/security/lifecycle correctness is core, not deferred edge-case work. Update in-source docs and relevant ADRs with implementation; operator docs immediately before a separately authorized PR.

## Review Focus

1. One-frame original/concealed alternation inside one receipt bucket must retain exact provenance across backpressure (Task 3).
2. Later real speech must not retroactively let earlier PLC start RF through preroll (Task 3).
3. A processing tail or empty callback must not fabricate a speech-close event (Tasks 3/4).
4. Rekey/retirement between courtesy selection and first/next sample must cancel only that phone generation (Tasks 2/4).
5. Valid-sized corrupt iLBC can return native success; explicit recovery evidence must not be overstated as complete internal provenance (Task 1).

## Workspace and recovery ledger

Planning worktree: `/home/codex/rpt-development/phone-plc-courtesy`.
Branch: `codex/phone-plc-courtesy-20261116`.
Native desktop worktree tools manage the Windows chat host, not the SSH repository, so the established development-server sibling-worktree layout was used. Creation started from the explicit requested baseline, not current main. No setup/build ran for this documentation-only task.

Create `.work/quality-progress.md` with each targeted run: exact HEAD/diff, command, fresh image digest, dependency cohort, start/end, outcome, log path, and remaining failures. Do not call a focused pass a full gate. Keep the plan's task checkboxes aligned with verified commits.

Recovery points, each independently reviewable:
1. Documentation/design checkpoint.
2. Explicit decode provenance and bounded diagnostics.
3. Reusable phone courtesy configuration/planner, not yet emitted by private media.
4. Provenance-safe bounded PLC and explicit rendered speech lifecycle.
5. Telephone courtesy integration and composed regression evidence.

Do not rebase over uncommitted evidence. Later, before an authorized publication phase, fetch/rebase current main under AGENTS.md, resolve conflicts serially, and rerun affected checks. Do not push from this planning assignment.

## Validation command envelope

The canonical launcher exists at /home/codex/rpt-development/rpt_advanced-workflows/tools/run-in-quality-container.sh (checkout 4aec0ee1aa5cfce8a5be89d314001fca313ef73d). The established resource-reporting launcher also exists and was inspected at /home/codex/rpt-development/workflow-resource-reporting/tools/run-in-quality-container.sh (checkout 9032bb8915da2cebe1f6c179b2b5478f7af9f8bc): its constrained profile forces a fresh pull, 1 CPU/1536 MiB, serial builds/tests and a 20-minute timeout. These are verified local revisions, not a claim about remote main.

Use that known launcher with the **whole development root** mounted, so the coherent provider cohort and shared target cache remain available:

```sh
RPTADV_CONTAINER_PROFILE=constrained RPTADV_CONTAINER_PULL=1 \
  /home/codex/rpt-development/workflow-resource-reporting/tools/run-in-quality-container.sh \
  ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest \
  /home/codex/rpt-development \
  bash /workspace/phone-plc-courtesy/.work/run-focused.sh \
  cargo test --locked -p rpt-advanced-standalone phone::call_media::tests -- --test-threads=1
```

The run-focused.sh helper is implementation-time ignored evidence, **not created or run by planning**. Build it from the verified reference /home/codex/rpt-development/phone-local-speech-retention/.work/radio-2eec2cd-cohort/run-v3.sh, preserving these inputs (container paths below):

- Work in /workspace/phone-plc-courtesy; use /workspace/phone-codec-analysis-union/.work/coherent-phone-qualification for the verified cohort, its providers, runtime baseline and support/sofia.
- Check providers/qualified-inputs.sha256 and the baseline runtime against cohort/controller-artifacts.sha256 before/after runs. Retain the existing minimum 2147483648-byte free-space guard.
- Set URP_PRODUCT_INCLUDE=$providers/include, BINDGEN_EXTRA_CLANG_ARGS=-I$providers/include, RPTADV_SOFIA_PREFIX=$cohort/support/sofia, and CARGO_TARGET_DIR=/workspace/phone-codec-analysis-union/.work/target-union. Preserve CARGO_INCREMENTAL=0, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0, and CC=clang.
- Reuse verified radio input /workspace/phone-local-speech-retention/.work/radio-2eec2cd-cohort/radio, preserving its checksum and provider SDK checksum. Preserve LIBRARY_PATH=$radio:$providers:$RPTADV_SOFIA_PREFIX/lib:$CARGO_TARGET_DIR/debug.
- Prepare a **new inactive** runtime at /workspace/phone-plc-courtesy/.work/runtime, with only required unchanged, verified cohort DSOs and symlinks. Never overwrite either baseline runtime. After building this feature's product and standalone, install their artifacts into this private runtime before composed tests.
- Set LD_LIBRARY_PATH=$radio:$cohort/support/dtmf:$RPTADV_SOFIA_PREFIX/lib:$providers:$runtime:$CARGO_TARGET_DIR/debug, RPT_ADVANCED_LIBDIR=$runtime, and RPTADV_SIP_TEST_PROVIDER=$runtime/librptadv_sip_adapter.so.1, matching the reference's coherent order. Check ldd proves intended radio/product/SDK resolution; do not let tests silently load the baseline product.
- Run one affected build or test command per bounded batch and preserve resource measurements/digest evidence. Do not blindly rerun the reference's entire all-suite sequence.

Do not invoke bare Cargo against default image headers/libraries: the older radio headers previously produce E0609, and default library discovery can exercise the wrong product. If cohort provisioning, verified dynamic resolution or disk space blocks a run, record the exact setup failure; never weaken tests, change permissions or substitute an unqualified provider.

## Task 1: Explicit decode provenance and bounded SIP observations

**Files:** Modify `rust/standalone/src/phone/codec.rs`, `phone/call_media.rs`, `phone/call_media_tests.rs`; extend existing `host_services/sip_phone.rs` and call-lifecycle owner only for snapshot exposure. Read the pinned codec-client test `rust/client/src/tests.rs::decoder_validates_native_counts_and_recovers_only_malformed_packets_once` in the separate codec-adapters repository; no edit there is needed unless a separately reviewed shared API change becomes essential.

**Interfaces:**
- Add private `DecodeProvenance { Original, Concealed, Missing }`.
- Change `AudioCodec::decode(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<DecodeProvenance, MediaError>`.
- Add `AudioCodec::conceal(&mut self, frames: u32, pcm: &mut [f32]) -> Result<DecodeProvenance, MediaError>`, delegating once to existing `Decoder::conceal`; zero becomes Missing, exact frames become Concealed, invalid counts/errors remain errors.
- `Pending::Audio` retains provenance alongside immutable coordinates/birth; `Media::read` forwards the existing concealed flag unchanged on every retry.
- Add private fixed-size `MediaObservation` containing existing jitter `Observation` plus decode-recovery/native-PLC/fallback frame counters; `Media::observe(&self) -> Result<MediaObservation, MediaError>` uses existing `Jitter::observe`. Update counters once when a decision is established, not for each refused sink read. No RTP packet-loss percentage or RTCP claim.

- [ ] Add failing tests that record the currently discarded `concealed: u32` argument in call-media Sink. Pin original vs explicit recovered delivery and unchanged flag/PCM on zero credit, partial credit and overlap trimming.
- [ ] Run `cargo test -p rpt-advanced-standalone phone::call_media::tests -- --test-threads=1`; record specific failing assertions before implementation.
- [ ] Reproduce explicit recovery using the existing descriptor-script fixture pattern without production-only injection hooks. The existing client test proves status=Malformed recovery exactly once. Record real Opus evidence separately as mechanism evidence only; corrupted iLBC native-success fixtures are not that evidence.
- [ ] Implement the private decode provenance return using `Decoder::decode`, falling back to `conceal` only on Malformed and keeping inspection/count validation. Keep current behavior of hard source gaps in this first commit; native Missing decision integration belongs to Task 3.
- [ ] Add observation tests for duplicate/late/full, missing vs insertion/skipped, native/fallback counters, exact reset boundaries and refusal not double-counting. Expose snapshots through the existing serial media owner/control diagnostic path, never audio-thread logging. One payload-free aggregate at normal close is acceptable; do not add periodic hot-path strings.
- [ ] Verify `cargo test -p rpt-advanced-standalone phone::codec::tests -- --test-threads=1` and affected call/diagnostic tests in separate batches. All assertions must pass.
- [ ] Record unresolved invisible iLBC native-success recovery as the spec's evidence limit. If detector safety actually depends on a missing provider distinction, stop and return exact evidence rather than extending ABI ad hoc.
- [ ] Run nonmutating formatting/static checks for affected source, then commit `fix(phone): preserve explicit decoder recovery provenance`.

## Task 2: Reusable configured phone courtesy, without a source event yet

**Files:** Modify `rust/core/src/config/schema.rs`, `settings.rs`, associated `schema_tests.rs`/`tests.rs`; `runtime/prepare.rs` and `prepare_tests.rs`; `controller/courtesy.rs`, `controller/mod.rs`, existing controller tests. Update architecture documentation for this internal prepared identity boundary.

**Interfaces:**
- Extend `CourtesySettings` with `phone: Option<PreparedMedia>`; prepare via existing `courtesy_media`.
- Add a small Copy `PhoneCourtesyKey { ingress: u64, media_generation: u64 }` in the core private-phone domain. Nonzero ingress and unique prepared media generation identify exact ownership; no string conversion.
- Extend planner source keys with a typed phone variant, preserving public receiver/link selection.
- Add `CourtesyPlanner::schedule_phone(key: PhoneCourtesyKey, due: u64)` and `cancel_phone(key: PhoneCourtesyKey)`.
- Add controller `phone_speech_closed(key: PhoneCourtesyKey, kerchunk: bool)` and `phone_speech_rekeyed(key: PhoneCourtesyKey)`; reuse `receive_unkeyed`, current delay and prepared playback. Exact-generation invalidation uses the same cancellation path. Methods do not grant call authority.
- Prepare the call/media liveness fence with the input slot on control; audio checks the retained existing token without allocating or destroying objects.

- [ ] Add failing config tests: `input=phone` resolves a prepared P, absent assignment is off, receiver/link assignments coexist, duplicate phone assignment and phone+`remote_node` follow existing structural rules, malformed media fields keep inherited warning/fallback behavior.
- [ ] Run `cargo test -p rpt-advanced-core config:: -- --test-threads=1`; confirm new phone recognition test fails before implementing.
- [ ] Add planner/controller tests: exact key cancellation, unrelated sources untouched, default/configured delay, no tone on kerchunk, stale generation cannot play, same-source rekey cancels pending and active phone playback. Assert public link rekey still cancels only pending as before.
- [ ] Implement configuration, prepared selection and typed planner methods; use existing `Playback::cancel` for the specifically active phone item and clear that active selection without successful-completion credit. Do not add a new generator or telemetry queue.
- [ ] Run core configuration, runtime preparation and controller courtesy tests separately. Verify local-only tone routing, site inhibit, timeout and duplex tests remain green.
- [ ] Commit `feat(courtesy): prepare typed telephone speech courtesy`. Do not emit it from a generic private unkey yet.

## Task 3: Bounded PLC continuity and truthful rendered speech lifecycle

**Files:** Modify `rust/standalone/src/phone/call_media.rs` and tests; `rust/product/src/link/{private_phone,phone_media,phone_speech}.rs` and their tests; `rust/core/src/link/audio.rs` only for the defaulted source-state accessor; `rust/product/src/services.rs`/history wrappers if required to forward the accessor. Existing ring/provider ABI remains unchanged. Amend ADR 0044's blanket concealment-retirement statement and ADR 0003 only where caller policy changes.

**Interfaces:**
- Add `Ingress::concealed(&mut self, start: u64, samples: &[f32], birth: Option<SourceBirth>) -> Result<usize, Error>` for native PCM.
- Keep Ingress::gap(start, count) as the counted source-span seam, but permit InboundProducer::gap fallback continuity only for proven mapped same-epoch loss with a valid original-history continuity lease. First distinguish all producer causes below; the callback itself has no reason/birth field. Actual SourceInput::Discontinuity, invalid timing and unqualified causes continue to retire, using the existing discontinuity route rather than silently treating them as packet loss.
- Extend existing `Shared` bounded source metadata with exact original/synthetic classification and conservative inherited bound. Allocate storage at preparation from existing source-capacity bounds; account for arbitrary one-frame fragment alternation. Use per-source-slot bits/records or an equally exact bounded extension of the existing ledger, not one boolean per 20 ms bucket. Publish metadata before PCM credit and reclaim only after real playout, preserving existing racing-snapshot fail-closed semantics.
- Add `PhoneSpeechState { key: PhoneCourtesyKey, phase: PhoneSpeechPhase }` and `PhoneSpeechPhase { Open, Draining, Closed, Unavailable }` as Copy core types. Default `PeerInput::phone_speech_state(&self) -> Option<PhoneSpeechState>` returns None; the phone input supplies it, processing wrappers forward it. Closed means verified original hold exhaustion plus drained qualified output, never a generic inactive/error return.

- [ ] Enumerate every counted-gap producer and pin its classification in tests before changing shared gap admission: call_media.rs jitter Kind::Missing; Pending::Audio.prefix from positive packet offset or source_end/adaptation skip; Pending::Gap synthesized when packet receipt lookup fails; and product services.rs callback forwarding to private_phone.rs::SourceInput::Gap, including existing IAX producers. Only known-timed same-epoch loss is recoverable. Failed receipt lookup, reset, epoch mismatch and unqualified skip remain hard discontinuities; insertion still creates no source gap. Reuse existing source-discontinuity callbacks for hard causes. Do not broaden IAX semantics merely because the product callback is shared, and do not infer a missing birth from current time.
- [ ] Add table-driven red tests at 8/16 kHz: loss before onset, already-open speech, exactly at hold boundary, exhausted immutable age, expired generation, lost timing, reset and command mute. Assert explicit concealed classification, no new original voice decision, unchanged `last_voice/voice_until`, and no synthetic-only RF start.
- [ ] Add `later_voice_does_not_promote_synthetic_preroll_to_rf_onset`: require genuine qualifying audio to reach output before any synthetic continuation is admitted. Source-side VAD qualification alone is insufficient.
- [ ] Add `mixed_single_sample_credit_preserves_plc_origin`: alternate one original/one concealed sample inside a bucket, fragment acceptance, force zero credit and mixed fractional conversion boundaries; assert no sample becomes falsely original or entire original regions get mislabeled synthetic.
- [ ] Add `native_loss_runs_once_and_fallback_is_not_stacked`: iLBC30 loss uses native conceal for supported whole units, unsupported partial spans use fallback, and subsequent real PCM resumes correctly. Every current SIP profile participates in the data-driven catalog test; no new negotiation.
- [ ] Add late/duplicate/skipped/insertion and missing-receipt cases. Insertion must not advance source cursor or create loss; missing receipt and genuine reset remain hard cuts.
- [ ] Run standalone call-media and product phone-media tests separately to confirm new behavior fails against the baseline.
- [ ] Implement pending native conceal/fallback once per exact leased decision. Do not overwrite retained packet PCM while preparing a positive missing prefix; process the prefix before decoding the retained packet, or use existing prepared scratch with separate proven ownership. Bound large missing spans by existing prepared frame capacity and preserve suffix credit.
- [ ] Implement native/fallback ingress, metadata and rendered continuity eligibility. Native concealed PCM goes through `write_tagged` with exact overlay; ring fallback retains provider PLC flags. Derive synthetic age conservatively from earlier real history; never current time. Track output-open qualification separately from original-source detection so preroll cannot create synthetic onset.
- [ ] Verify the existing speech-analysis source jump behavior. Reset only detector analysis through the existing provider reset if needed and supported, not programme admission; never classify synthetic waveform or freeze source time. Prove post-loss genuine speech can qualify without joining pre/post-loss detector frames.
- [ ] Implement source lifecycle evidence: original hold end enters Draining; only post-DSP no-qualified-tail makes Closed. Retire/mute/unknown timing becomes Unavailable. Preserve the existing exact processed-tail retention that fixed clipped syllables.
- [ ] Run `cargo test -p rptadv-product link::phone_media::tests -- --test-threads=1`, `link::private_phone::tests`, `link::phone_speech::tests`, `services_history`, and standalone source-credit suites separately. Include existing allocation fixtures; no new callback allocations/locks.
- [ ] Commit `feat(phone): retain bounded provenance-safe PLC continuity`. This is manual milestone 1 after qualified build and separately authorized deployment; no automatic node work.

## Task 4: Connect only genuine tail-drained speech close to courtesy

**Files:** Modify `rust/core/src/link/audio.rs` and `audio_phone_tests.rs`; product `phone_media_tests.rs`; standalone `host_services/product_sip_vox_tests.rs` and related composed fixture. Update concise architecture/source docs; reserve operator manual/config examples for pre-PR.

**Interfaces:** Consume Task 2 controller methods and Task 3 `phone_speech_state`. Existing prepared slot owns prior state and exact generation fence; no global phone name registry. Use actual held rendered-open duration for existing kerchunk classification.

- [ ] Add failing composed tests: real speech then held close drains retained source/DSP tail and schedules exactly one P; multiple silent callbacks do not repeat it; configured delay starts at completed close.
- [ ] Add loss during open speech, source-starvation, temporary processor pending, hangup/reset/mute and graph-retirement cases. Only valid Closed transition schedules; Unavailable cancels.
- [ ] Add rekey before due and during Morse, plus retirement race before/after first sample. Assert same-generation phone tone is canceled, unrelated sources unaffected.
- [ ] Assert P appears in local RF PCM only, never telephone/peer output or DTMF collection. Keep inhibit/timeout/half-duplex and source-age assertions; test no speech reopen is caused by P.
- [ ] Run `cargo test -p rpt-advanced-core link::audio_phone_tests -- --test-threads=1` and relevant standalone composed SIP VOX suite to capture failures.
- [ ] Wire explicit transitions at the existing sole-render/active-edge loop, not the nameless `link_unkeyed` fallback. Preserve watchdog/hang behavior and existing public source edges.
- [ ] Rerun core/product/standalone targeted suites, affected ABI tests and nonmutating fast checks. Record allocation/resource evidence and exact cohort. Commit `feat(phone): emit local courtesy after held speech drains`.
- [ ] Manual milestone 2 requires qualified build and root-issued advance interruption notice before any approved node deployment. Configure only the requested P assignment, preserving current threshold .6125, onset 2, hold 1000 ms and all other node settings. This planner neither accesses nor changes the node.
- [ ] At a separately authorized publication phase, update operator docs/examples, fetch/rebase, run complete fast preflight, then draft PR only if authorized. Full native Debian 13 amd64/arm64 tests/build/package/install and 100% production line/branch coverage on amd64 remain before-merge gates; no skipped tests or reduced coverage.

## Self-review and handoff

All requested features map to Tasks 1–4. No classifier or new codec negotiation was added. Exact concealed origin and source-age continuity precede the courtesy integration; a generic inactive edge is explicitly insufficient. New private interfaces are defined where consumed. Existing C provider/host ABIs remain unchanged.

The isolated planning tree is ready for the parent to review and execute serially. The parent should choose implementation detail refinements only if they preserve the spec's exact safety contracts. A missing safe provider contract or a failed provenance proof is a substantive blocker to report, not a reason to relax synthetic authority or add an arbitrary timeout. No wall-clock completion estimate is promised: four reviewable code increments plus two separately authorized hardware checkpoints are the milestones.

## Reset dependency correction (2026-11-16)

The earlier no-provider-change assumption does not hold for configured analysis:
its old reset callback fails closed. The separate analysis-history-reset plan
qualifies an append-only RNNoise reset contract and radio capability bit 4.
Controller setup now requires this capability in addition to preprocessing;
no SDK phone API, host table slot, programme queue or radio-session reset change
is needed. Exact local candidate identity is not a published package floor.
The independent codec provenance contract is tracked separately.
