# Message Composition and Tail Playback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Play composed, localized messages and release-triggered tails, with reliable Morse-only playback during local or linked receive activity.

**Architecture:** Extend the existing shared controller and single station telemetry producer. One composition remains one routed, generation-owned playback item; it is not a queue of independently scheduled messages. Reuse the released file, speech, tone, Morse, and PCM-ring libraries.

**Tech Stack:** Rust 1.85, existing Fluent catalogs, existing dynamic media providers and shared PCM ring, native Debian 13 test images.

**Spec:** [Approved site-monitoring design](../specs/2026-10-17-site-monitoring-and-message-macros-design.md), approved 2026-10-17, including all-message receive interruption.

**Status:** Approved for native implementation 2026-10-18. M1 commit `197f7e7`
passed targeted checks and approved native installation on 524950. The operator
confirmed the local interruption/repeat/courtesy test. Peer-source behavior has
automated coverage, not a new live-link test. M2 is implemented and targeted tests,
Clippy, formatting and Rustdoc pass. M3 is implemented with targeted producer,
ring, controller, runtime, Clippy, formatting and Rustdoc checks passing. M4 is
implemented with targeted command/configuration, runtime, allocation, host,
Clippy, formatting, and Rustdoc checks passing. M5 is implemented; targeted tail,
controller, configuration, runtime, producer, Clippy, formatting, and Rustdoc
checks pass. M6 shared-host integration remains. The full hosted gate has not run.
No further deployment is implied by implementation approval.

## Global Constraints

- Preserve standalone/Asterisk behavior through their shared product. No new Asterisk or ASL3 dependency and no duplicate renderer.
- All telemetry synthesis, decoding, expansion, allocation, and reclamation stays off audio. Audio consumes generation-tagged normalized F32 PCM at 48 kHz without locks or waiting.
- Any qualified local receive OR any connected peer receiving selects Morse, including a peer whose audio is not selected for RF. Keep source-unkey admission, destination, half-duplex, timeout, and interlock rules.
- Cancel buffered primary audio and late writes; preserve the same snapshot and Morse latch through an interrupted message. A genuinely new attempt gets fresh values.
- Content tokens: literal text, `{message_macro_name}`, `{message}`, `{media}`, `{tone_sequence}`; escaped braces are `{{` and `}}`. Tokens are not executable expressions. References are acyclic and checked before expansion allocation.
- Catalog forms remain independently bounded to 127 UTF-8 bytes per segment; the complete composition is not limited to 127 bytes. Never silently truncate.
- Node/global/instance inheritance and ADR 0019 warning/default behavior apply. Disable only an affected unsafe content definition when a sensible fallback cannot resolve it.
- New DTMF defaults: `link_command_message = 77`; site-query `76` is implemented by the companion plan. Selectors are not remembered link destinations.
- Tails: interval 0 means each qualifying traffic release; positive intervals are cooldowns since successful playback, not periodic idle triggers. Delay 0 retains PTT at hang expiry; positive delay starts at the resulting unkey. ID maximum-wait deadlines override both timers.
- Preserve the unrelated modified `2026-10-16-appliance-signaling-interlock-design.md`. Stage explicit owned paths only.
- Developer docs before each implementation commit; operator docs before a PR. Fast format/lint/static checks before push; full hosted PR gate before merge. Debian 13 amd64/arm64 tests, production coverage only on amd64. No local full gate and no Debian 12 automation.
- No node access, install, restart, link connection, PR, merge, or release without the corresponding approval.

## Review Focus

1. Reused media generations can lose a preselected Morse fallback when `start()` advances again: M1 tests repeated playback under receive.
2. A successful file segment can hide following macros, or a rate change can lose final samples: M3 tests ordered, mixed-rate drain and one completion.
3. A sensor/translation string containing braces can be treated as code: M2/M4 test that bound data is never reparsed.
4. Partial, failed, or cancelled playback can satisfy an ID/tail: M1/M5 test explicit success and unchanged deadlines on failure.
5. Receive during preparation or simultaneous hang/ID/tail deadlines can leak voice or postpone identification: M1/M4/M5 test those exact boundaries.

## Execution and verification

All paths below are relative to `rpt_advanced` unless noted. Start from the reviewed branch containing this plan; inspect existing changes and reuse an available checkout. Reports belong in ignored `.work/quality-progress.md` and `.work/`, never the committed tree.

Run each listed Cargo command through the maintained launcher, with the required released development packages in the image:

```sh
../rpt_advanced-workflows/tools/run-in-quality-container.sh \
  ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest "$PWD" \
  cargo test --locked -p rpt-advanced-core --lib TEST_FILTER
```

The launcher must pull `latest`, record its digest, and clean labeled test containers. A test filter matching zero tests is not verification. Record red/green results for each task and rerun only affected tests/coverage until the final hosted gate. Rustdoc checks likewise target the changed crate.

## File and interface map

| Owner | Existing files / planned additions |
| --- | --- |
| Inherited configuration | `rust/core/src/config/{scope,schema,settings,mod}.rs`; new `config/messages.rs` and `config/tails.rs` |
| Forms and composition | `rust/core/src/messages.rs`, `runtime/render.rs`; new `composition.rs` and `composition_tests.rs` |
| Per-attempt preparation | `runtime/{prepare,aggregate}.rs`, `controller/telemetry.rs`; new `runtime/composition.rs` |
| Producer and consumption | `media.rs`, `audio/{mod,playback}.rs`, `rust/product/src/media/worker.rs` |
| Timing | `controller/{mod,announcement}.rs`, `policy/identifier.rs`; new `controller/tail.rs` |
| Command selection | `command.rs`, `runtime/{dtmf,aggregate}.rs`, `rust/product/src/host.rs` |

New types below are private Rust product interfaces, not public C ABI additions. Existing `MediaSource` describes alternatives; `MediaProgram` describes ordering. Do not change the released literal-template ABI to interpret structural tokens.

### M1: Make receive interruption and completion reliable

**Files:** Modify `rust/core/src/{audio/mod.rs,audio/playback.rs,controller/mod.rs,controller/telemetry.rs}` and `rust/product/src/media/worker.rs`. Tests: existing `rust/core/src/audio/playback_tests.rs`, `rust/core/src/controller/tests.rs`, and `rust/product/src/media/worker_tests.rs`.

**Interfaces:** Keep `PcmStreamReader::{start,render,cancel}`. Replace the internal Boolean fallback result with `MorseSwitch::{Selected,Pending,Unavailable}` so retirement backpressure is not treated as missing Morse. Add `PlaybackOutcome::{Active,Succeeded,Failed,Cancelled}` and `Playback::outcome(&self) -> PlaybackOutcome`. Existing callers of `is_finished` may use termination for cleanup, but only `Succeeded` satisfies an ID or tail.

- [x] Write `morse_global_receive_routes`, a table-driven test covering local/peer receive, local/peer-routed status, IDs and announcements, initial activity and mid-playback activity. Assert no primary samples after the boundary, preserved reply target, and no restart on subsequent key/unkey changes.
- [x] Write product regressions `morse_reused_stream_restart`, `morse_pending_retirement_gates_primary`, and `morse_late_primary_writes_rejected`. Reuse fixture streams; assert retired/current converter history cannot leak, and a second invocation started in receive stays Morse. Add `playback_failure_does_not_satisfy_identifier`.
- [x] Run the failing named filters with `cargo test --locked -p rpt-advanced-core --lib morse_`, `... playback_failure_`, and `cargo test --locked -p rptadv-product --lib morse_`. Confirm assertion failures rather than missing libraries.
- [x] Use existing all-source receive state, not RF-selected link state, for message form selection. Keep parrot/courtesy cancellation separate. Reuse the ready/retired generation handoff; pending retirement gates primary output until retry succeeds. Ensure `start` preserves selection across reused generations. Stop producer failure from becoming successful EOF. Render/retire remains bounded and allocation-free.
- [x] Re-run the named filters and the existing `audio::playback::tests`, `controller::tests`, and `media::worker::tests` suites. Assert successful sample tails still drain and failures release demand without satisfying IDs. Update Rustdoc and ADR 0025's cancellation contract; commit the focused correction.
- [x] Obtain separate deployment approval and operator confirmation of local receive interruption on 524950. Linked-source interruption has automated coverage; no additional live-link test was authorized. Automated dependent work can proceed.

### M2: Compile ordered content and additive message catalogs

**Files:** New `rust/core/src/{composition.rs,composition_tests.rs,config/messages.rs}`; modify `rust/core/src/{lib.rs,messages.rs,messages_tests.rs,messages_file_tests.rs,config/mod.rs,config/scope.rs,config/schema.rs,config/settings.rs}` and their tests. Extend the existing shipped catalog `messages/en-US.ftl`.

**Interfaces:**

- `ResolvedMessageSettings::resolve(document: &ConfigDocument, node: &NodeId, name: &str) -> Result<Resolution<Self>, ConfigError>`: owned `content`, optional `message_id`, bindings, media fields and optional DTMF selector.
- `Composition::compile(content: &str, definitions: &BTreeMap<String, ResolvedMessageSettings>) -> Result<Composition, CompositionError>` produces checked ordered segments and reference bindings; construction occurs on control.
- `BindingSource`: `Callsign`, `Node`, `Date`, `Time`, `DayOfWeek`, `Greeting`, `LinkStatus`, `Sensor(String)`, `Input(String)`, `TimeRemaining`. The last requires an invocation context.
- `MessageArgument::{Number(f64), Forms(MessageForms)}` and `MessageCatalog::format_named(&self, id: &str, args: &BTreeMap<String, MessageArgument>) -> Result<MessageForms, CatalogError>`. The existing typed `format` shares the same per-attribute formatting/fallback implementation.

- [x] Write `composition_literal_multiple_macros`, `composition_file_then_macros`, `composition_attention_positions`, and compiler error tests for cycles/overflow. M4 admission owns warning/disable handling. Example assertion:

```rust
assert_eq!(expanded_kinds, ["tone", "time", "tone", "temperature", "tone"]);
assert!(!rendered_measurement.contains("node,"));
assert_eq!(rendered_node, "node,5,2,4,9,5,0");
```

- [x] Write additive-catalog tests: an administrator adds one message without replacing shipped IDs; malformed/missing translated attributes fall back independently; 127-byte forms pass and 128-byte forms fail; a composition of two valid 100-byte forms is not rejected as one 200-byte message. Test numeric Fluent arguments, escaped braces, repeated references, invalid contextual binding, unknown names and node overrides.
- [x] Run the new composition and message tests to see assertions and missing-interface failures before implementation.
- [x] Implement one parser/compiler, preserving configuration order. Count expanded segments/bytes with checked arithmetic before checked allocation; do not add a fixed node/macro count or an expression engine. Invalid/cyclic definitions cannot recurse at runtime. Load shipped catalogs plus overlays, not the current replace-the-catalog behavior. Keep literal `[template]` and action `[macro]` semantics unchanged.
- [x] Re-run targeted tests and configuration scope/schema tests. Document developer contracts and planned status in ADRs 0009/0023; commit the compiler/catalog slice.

### M3: Stream one logical composition through the existing producer

**Files:** Modify `rust/core/src/media.rs`, `runtime/prepare.rs`, `audio/playback.rs`, `rust/product/src/media/worker.rs`, their existing tests and all `StationMediaSession` fixture implementations. Add `rust/product/src/media/composition_tests.rs` if the worker test file needs a focused sibling.

**Interfaces:** `MediaProgram { primary: Vec<MediaSource>, morse: Vec<MorseSource> }`; `MediaProgram::single(source: MediaSource) -> Self`. Add `StationMediaSession::register_program(&mut self, program: MediaProgram, one_shot: bool) -> Result<Box<dyn PcmStreamReader>, MediaError>` and make single-source registration use the same implementation. `PreparedMedia` and its route remain one item for the whole program.

- [x] Write `composition_stream_one_completion`, `composition_segment_rates_keep_final_samples`, `composition_file_success_keeps_following_macro`, and `composition_receive_restarts_full_morse`. Use fixture files at 22050 Hz and generated media at 48000 Hz; assert ordered output, all final samples, and exactly one successful completion. Inject pending producer work between segments; PTT must not drop between admitted segments.
- [x] Assert attention tones occur before/between/after messages while idle; ordinary speech failure preserves those positions. Forced Morse restarts the complete message and excludes decorative tones/files but retains each media segment's Morse substitute.
- [x] Run `cargo test --locked -p rptadv-product --lib composition_`; observe failure before implementation.
- [x] Iterate segments in the existing station producer. Reuse provider streams and shared-ring conversion for each source rate. Drain a completed segment before handing off its successor; do not report item EOF between segments, drop resampler tails, or apply gain twice. The reader stays one logical stream, with bounded ready/retired handoff and no callback destruction. On receive cancellation discard the old sequence rather than draining it audibly. No new provider ABI, converter, or renderer is expected.
- [x] Run the new filters plus existing worker/ring tests. Cover file and speech failure before/after samples, no-Morse content, cancellation, retirement backpressure, and reused programs. Update Rustdoc and commit.

### M4: Resolve values per attempt and expose read-only macro commands

**Files:** New `rust/core/src/runtime/composition.rs`; modify `runtime/{aggregate,prepare,render,dtmf}.rs`, `controller/telemetry.rs`, `command.rs`, `config/{schema,settings}.rs`, and `rust/product/src/host.rs`. Tests: new `runtime/composition_tests.rs`, existing `runtime/{dtmf,prepare,render}_tests.rs` and command/config tests.

**Interfaces:** `MessageValueSnapshot` owns civil time, node/callsign, localized link forms, and named sensor/input `MessageArgument` values with quality/observation metadata. `Composition::prepare(&self, catalog: &MessageCatalog, values: &MessageValueSnapshot) -> Result<MediaProgram, CompositionError>` resolves content once. Each compiled owner retains its existing resolved speech/Morse defaults, file/fallback and explicit overrides; no duplicate media-default argument is needed. Reuse the existing gain application, not a second gain model.

For configured recurring media, use a bounded preallocated request/completion handoff: `MessageAttemptId { generation: u64, sequence: u64 }`, `MessageRequest { attempt: MessageAttemptId, content_index: usize }`. The transmit-side scheduler publishes only this request. Control resolves the current snapshot and returns `PreparedMedia` tagged by the same attempt through existing ownership-transfer machinery. New receive may latch Morse while preparation is pending; adoption must honor that latch. No strings or catalog work on audio.

Add `ControllerControl::take_message_request(&mut self) -> Option<MessageRequest>` and `ControllerControl::complete_message(&mut self, attempt: MessageAttemptId, result: Result<PreparedMedia, ControllerError>) -> Result<(), MessageCompletion>`; `MessageCompletion` owns the rejected attempt/result for control-side retry or reclamation. A failed preparation must also complete the attempt so it cannot hold PTT or wait forever. Reuse the current bounded ownership queues rather than an audio task submission or new executor.

Implementation refinement: a prepared reader facade uses the existing playback
scheduler to publish these requests. Each registered recipe has one bounded
request/completion/retirement credit. Control unwraps prepared streams before
publication and reclaims them afterward. This keeps completion delivery independent
of queued command replies; it adds no second scheduler, renderer, or public ABI.

- [x] Write `composition_attempt_refresh_and_continuation`: two new attempts see updated time/values; receive-to-Morse retains the first attempt snapshot. A deferral before any sample starts a fresh attempt. Assert preparation completing after receive cannot emit voice, and preparation failure releases the pending attempt. A stale generation/attempt result is reclaimed off audio. ID-priority continuation belongs to M5, where tail preemption is introduced.
- [x] Write `composition_values_are_data`: sensor text containing `{message_macro_x}` remains literal; stale measurement replaces only its segment. Assert natural measurement/date/time versus digit-wise node pronunciation using independent `.tts`/`.morse` values.
- [x] Write `composition_dtmf_selector_preserves_last_link`: `*771#` selects macro 1 without altering remembered destination; `*770#` does not recall the last link. Test source unkey, timeout, remapping/collisions, peer disconnect/replacement, queue-full retry and exact reply route. No hardware action is admitted through this public command.
- [x] Run core `composition_` and DTMF test filters red, then implement the request handoff and `LinkAction::Message` with a selector-specific branch before the existing destination/`0` recall logic. Do not duplicate the collector. Extend recurring IDs to request values at playback, not configuration reload; M5 wires the new tail source. Use existing control clock/status data; the companion plan supplies site observations.
- [x] Re-run affected config/command/runtime tests and allocation/routing fixtures. Verify missing site bindings resolve as unavailable/disabled with warnings until the site subsystem is configured, not fabricated zero values. Commit.

### M5: Add release-triggered tails and explicit ID deadline ordering

Completed 2026-10-18. Evidence: `.work/message-composition/m5-final.log`.
The named timing tests, per-tail delays, half-duplex/all-source admission,
preemption restart, success-only cooldown, runtime value refresh, and callback
allocation guard pass. Existing periodic announcements retain their timing.

**Files:** New `rust/core/src/{config/tails.rs,controller/tail.rs,controller/tail_tests.rs}`; modify `config/{mod,scope,schema,settings}.rs`, `controller/{mod,announcement}.rs`, `policy/identifier.rs`, `runtime/prepare.rs` and associated tests.

**Interfaces:** `ResolvedTailSettings::resolve(document: &ConfigDocument, node: &NodeId, name: &str) -> Result<Resolution<Self>, ConfigError>` owns `content`, `interval_ms`, `delay_ms`, and inherited media settings. `TailState` records original traffic hang deadline, optional post-unkey delay deadline, pending/admitted attempt, and last successful completion. Extend the existing `Source` discriminator with `Tail(usize)`; use the same playback owner and arbitration, not another audio engine.

- [ ] Write `tail_zero_and_positive_delay`: traffic ends at 1000 ms, hang=1000 ms; delay 0 starts tail at 2000 ms without unkey, delay 250 unkeys at 2000 ms and rekeys at 2250 ms. Use partition-independent fake sample clocks, no wall sleeps.
- [ ] Write `tail_id_deadline_precedence` covering expiry before hang, exactly at hang, during delay, and during tail. Assert original hang/delay deadlines do not shift; an expired ID interrupts tail, which restarts with retained snapshot/Morse latch. Normal polite IDs wait for hang; pending polite ID follows tail if not expired.
- [ ] Write `tail_receive_pending_vs_playing`: receive before start cancels the release opportunity; receive after start switches the admitted item to full Morse. Test multiple peers, new release, multiple tails in configuration order, and no false completion on interruption/failure. Each named tail applies its own delay and cannot overtake an earlier eligible tail; a controller-generated unkey starts no new release opportunity.
- [ ] Write `tail_nonrecursive_cooldown`: interval 0 is once per traffic release; interval 1800000 ms is eligible only on a later release after a successful play and never keys from prolonged idle. IDs, courtesy, tails, and announcements cannot manufacture tail opportunities. Existing periodic announcements remain unchanged.
- [ ] Run core `tail_` filters red. Implement the smallest explicit timing state in the existing controller, separating traffic hang origin from telemetry media activity. Use M1 success/Morse outcomes and M4 attempt handoff. Preserve safety/half-duplex and existing courtesy timing.
- [ ] Run `tail_`, `polite_`, announcement, timeout, and variable-frame partition tests green. Document the scheduling state transitions in Rustdoc; commit.

### M6: Verify the complete slice and prepare the review

**Files:** Affected developer docs, `WISHLIST.md`, `doc/architecture/{README.md,decisions/0009-scheduled-actions-and-macros.md,decisions/0023-unified-control-and-dtmf-policy.md,decisions/0025-native-media-routing-and-pcm-ring-ownership.md}`; before PR, `doc/configuration.md`, relevant `man/` pages and shipped examples.

- [ ] Add shared-product integration cases exercising identical config in standalone and Asterisk compositions with deterministic media/receive fixtures. Assert no voice during any active receive source, one item per composition, correct peer destination, and no audio allocation/blocking.
- [ ] Run only affected tests/coverage and Rustdoc while closing gaps; keep the completed-item ledger in `.work/`. Run `make lint static-analysis` once in the fresh image before an approved push. Do not use local `make rust-check` or `make quality` as a substitute for the hosted PR gate.
- [ ] Before an approved PR, update concise operating instructions and examples, including positive-tail-delay rekey behavior and Morse fallback. Preserve the 127-byte per-form versus whole-composition distinction. Mark only verified implemented requirements complete, leaving site acquisition for the companion plan.
- [ ] Commit explicit owned files. On later push/PR authorization, require native Debian 13 tests, package/install checks, and 100% production coverage on amd64 before merge. Obtain separate permission for any package release or node test.

## Handoff

Review this plan together with the [site monitoring implementation plan](2026-10-17-site-monitoring-and-actions.md). Implement M1 first as the shared playback correctness prerequisite; do not claim the live reported defect fixed until its reproduction passes. After the playback slice is confirmed, proceed to the site slice. Recommended execution is native in this session, with one independent final review, to keep tightly coupled interface changes together and minimize repeated context.
