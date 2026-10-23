# Scheduled Message Playout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Play clock-scheduled or DTMF-selected composed messages with bounded polite delivery, configurable routing/repeat, and prompt cancellation.

**Architecture:** Extend the existing scheduler and shared message recipes. One telemetry producer streams every source through the existing ring; the output callback applies prepared policy without locks, allocation, synthesis, or I/O. Keep generic calendar calculations in the released schedule provider and controller delivery policy in the product.

**Tech Stack:** Existing Rust workspace, released schedule/tone/Morse/DTMF/template shared libraries, existing file/Piper adapters, rtrb and atomics.

**Spec:** `docs/superpowers/specs/2026-10-23-scheduled-messages-design.md`

**Execution status — 2026-10-23:** Tasks 1–5 implemented. Hosted core run
40016478749 passed all 671 tests; product run40011556542 passed393 tests with
one pre-existing ignored test. Real shared producer/ring integration through
both hosts passed40013156461. Warning-denied Rustdoc and push preflight passed.
The final review's opening-cancellation and legacy-selector findings are fixed
with RED/GREEN regressions. Final production coverage/platform verification
is pending; no scheduled-message code has been installed on the node.

## Global Constraints

- Preserve unrelated user edit in `docs/superpowers/specs/2026-10-16-appliance-signaling-interlock-design.md`.
- No node installation/restart/link/config changes or release. The passive disconnect capture continues independently through 12:30 America/Chicago.
- Reuse installed/released dynamic dependencies; do not vendor or statically duplicate scheduler/media code. Do not change a public ABI without proving it necessary.
- Native internal PCM remains normalized f32 at 48 kHz. Producers and cancellation/reclamation stay off audio.
- Clock alignment is civil-minute based; retain ADR 0017 DST/no-catch-up/reload identity semantics.
- `polite_maximum_wait_ms` defaults to 60000; zero means immediate eligibility or expiry. `polite_expiry` defaults to skip; `polite_expiry_busy` defaults to skip.
- Each message may assign `dtmf_id`, `repeat_during_playback` (default yes), and `playback_destination` (default local, alternative local_and_links).
- Default cancel command is remappable `*78`, public on local RF without administrative unlock. It cancels the active attempt, not unrelated future schedules.
- Use focused tests/source coverage and Rustdoc while iterating; record results in ignored `.work/quality-progress.md`. No local full gate. Update operating docs only before a PR.

## Review Focus

1. Cancel while the producer is opening or replacing Morse: stale PCM must never return and repeat must resume.
2. A delayed/full admission queue must not refresh a scheduled deadline or create repeated playback.
3. A long exempt message must not let continuously keyed, timed-out traffic pass through.
4. A receive-only/fallback peer must not receive broadcast audio merely because it is connected.
5. A mixed composition with one missing Morse substitute must stop, not play a misleading partial fallback.

## Task 1: Clock-aligned event selectors using the current provider

**Files:** `rust/core/src/schedule.rs`, `rust/core/src/schedule/tests.rs`, `rust/core/src/runtime/scheduler.rs`, `rust/core/src/runtime/scheduler_tests.rs`.

**Interfaces:** Add controller-facing `EventTrigger::parse(at: &str, days: Option<&str>) -> Result<EventTrigger, ScheduleError>` and `EventTrigger::occurrence(&self, local: &CivilTime) -> Option<u64>`. It is Clone/Debug/PartialEq/Eq and retains existing `ScheduledEvent` primitives and optional existing `Window` weekday selection. `EventSpec.trigger` uses this compiled trigger.

- [x] Add failing table tests: existing once/daily/weekly; `every 1 minute`, `every 15 minutes`, `every 60 minutes`, `every 2 hours`, `every 24 hours`; singular/plural; Monday-Friday; invalid/zero/out-of-range intervals.
- [x] Run focused `cargo test --locked -p rpt-advanced-core schedule::tests` and observe the missing/new-behavior failure.
- [x] Compile stepped selectors to existing daily/weekly provider records with one outer configured-event identity. Use provider occurrence keys and window matching; do not add a second scheduler or duplicate Gregorian/DST arithmetic.
- [x] Preserve one reservation per configured entry rather than one independent event per compiled minute. Test once-per-civil-minute, repeated/skipped DST minutes, backward/forward corrections, reload identity, and no restart catch-up in existing scheduler suites.
- [x] Run the focused schedule/scheduler tests, Rustfmt check and affected Rustdoc; record evidence and commit only these files.

## Task 2: Common message options and optional complete Morse

**Files:** `rust/core/src/config/messages.rs`, `config/tails.rs`, `config/settings.rs`, `config/schema.rs`, `config/tests.rs`, `composition.rs`, `composition_tests.rs`, `runtime/composition.rs`, `runtime/composition_tests.rs`, `runtime/prepare.rs`.

**Interfaces:** Add `PlaybackDestination { Local, LocalAndLinks }`, `PlaybackPolicy { repeat_during_playback: bool, destination: PlaybackDestination }`, and `PoliteDelivery { maximum_wait_ms: u64, expiry: PoliteExpiry, busy: PoliteBusy }` in the existing controller telemetry boundary; `PoliteExpiry` is Skip/Morse, `PoliteBusy` is Skip/Wait. Resolvers return these typed settings; defaults are the spec values. Retain a distinct required-Morse validation choice for IDs.

- [x] Write failing config tables for message/ID/tail/event fields, node/global inheritance, selector collisions across source families, invalid-value warnings, explicit empty optional Morse, and unchanged legacy event fields.
- [x] Write composition tests for file-only, speech-only, Morse-only and ordered mixtures; interruption of a composition lacking one content substitute must report unavailable whole-program Morse. Decorative tones can be omitted. IDs must still require complete Morse.
- [x] Run affected config/composition tests to confirm the failures, then reuse `ResolvedMessageSettings::from_scopes` and the existing recipe compiler. Do not create another decoder, renderer or template language.
- [x] Register tail, ID, event and named-message recipes in one selector namespace. Keep current named-message queries working.
- [x] Run focused tests, Rustfmt/Clippy/Rustdoc for the component, log results and commit.

## Task 3: Bounded scheduled delivery through message recipes

**Files:** `rust/core/src/runtime/aggregate.rs`, `runtime/aggregate_tests.rs`, `runtime/scheduler.rs`, `runtime/scheduler_tests.rs`, `controller/telemetry.rs`, `controller/tests.rs`, `runtime/prepare.rs`.

**Interfaces:** Extend existing `PreparedMedia` with typed delivery/playback metadata rather than adding another queue. Add `PreparedMedia::with_playback_policy(self, policy: PlaybackPolicy) -> Self` and `with_delivery(self, deadline_sample: u64, policy: PoliteDelivery) -> Self`. Extend existing runtime event state with an optional compiled recipe index and original occurrence/deadline identity. Keep `Runtime::next_event`, `queue_event`, and `complete_event` as the host entry points.

- [x] Write failing tests: due time becomes pending; busy input delays primary; idle within 60000 ms admits it; skip/Morse expiry; zero-wait; no-Morse expiry skips; occupied slot uses per-message skip/wait; failed/full admission does not refresh deadline.
- [x] Add deterministic tests for one latest unstarted successor, actual-attempt substitutions, no duplicate after cancellation/reload, and unchanged at-most-once action macros. Manual media selection never executes an event's action macro.
- [x] Observe failures, then route composed events through `MessageRecipes`/`ControllerControl::prepare_message`. Convert the original remaining wait to the controller's monotonic sample clock once, not each pump.
- [x] Use existing bounded pending/completed ownership. Expiry/cancel is terminal for that occurrence; a playing attempt is not cut off by its start deadline. Preserve existing serialized priorities and hardware/half-duplex rules.
- [x] Run focused runtime, scheduler and controller deadline tests plus targeted docs/checks; log and commit.

## Task 4: Output policy, routing and traffic-timeout exemption

**Files:** `rust/core/src/controller/mod.rs`, `controller/telemetry.rs`, `controller/timeout.rs`, `controller/tail.rs`, `controller/tail_tests.rs`, `controller/tests.rs`, `link/audio.rs`, existing link/controller audio tests.

**Interfaces:** Carry the active `PlaybackPolicy` with the existing prepared source. Extend existing tagged per-sample program/broadcast output in `ProgramSample`/`ParrotOutput` only as needed; keep ordinary command replies source-scoped and parrot ownership distinct. No new PCM ring or public host ABI is needed.

- [x] First add failing tests for repeat suppression with local and peer traffic, continued local command decoding, and unconditional Morse-or-stop receive interruption even when repeat is disabled.
- [x] Add source-isolation tests: local-only versus local-and-links; receiving peer mix-minus remains correct; receive-only/non-selected fallback peers remain ineligible; no self-echo or locally generated CTCSS on links.
- [x] Add long-tail/scheduled tests crossing transmit timeout and lockout: message PCM/PTT remain valid while expired traffic remains muted. Preserve site inhibit, half-duplex, ID priority, no recursive tails, and clean final PTT release.
- [x] Observe failures, then apply repeat/routing after input processing and ring consumption. Distinguish exempt message demand from traffic timeout instead of disabling the timeout globally. Change both PCM admission and PTT policy together.
- [x] Run focused tail, timeout, controller and link tests, including prior kerchunk/overlap/telemetry regression cases. Log and commit.

## Task 5: Manual play and reliable current-message cancellation

**Files:** `rust/core/src/command.rs`, `command_tests.rs`, `config/settings.rs`, `config/schema.rs`, `runtime/aggregate.rs`, `runtime/composition.rs`, existing command/runtime tests, `controller/telemetry.rs`, `controller/mod.rs`, `controller/tests.rs`, affected product command dispatch tests.

**Interfaces:** Add `LinkAction::CancelMessage` and its inherited `link_command_cancel_message` mapping, default `78`. Add `ControllerControl::cancel_message(&self)` using a preallocated atomic handoff observed by the audio owner. Reuse `LinkAction::Message` and its existing selector lookup for all selectable message sources.

- [x] Write failing tests for remapping/collision checks, `*77<id>` selection of ID/tail/event, public local `*78`, normal source-unkey admission, full queue rejection, and no associated event action execution.
- [x] Test successful manual ID completion through normal priority satisfaction, successful manual tail cooldown, and manual scheduled content leaving future calendar eligibility intact.
- [x] Add cancel tests during primary, between segments, while opening, while awaiting/replacing Morse, and when idle. No stale PCM may resume, no cancelled attempt is marked successful or immediately rearmed, repeat returns, and unrelated future schedules remain.
- [x] Observe failures. Implement the typed command and atomic current-attempt cancellation; reuse off-audio retirement/reclamation. No cancel confirmation may overlap the current message.
- [x] Run affected command/runtime/media tests and developer checks, log evidence and commit.

## Task 6: Integration, architecture and handoff

**Files:** affected Rustdoc, `doc/architecture/README.md`, ADRs 0008/0009/0017/0023/0025, `WISHLIST.md`, this plan, ignored `.work/quality-progress.md`. Operating docs/examples are deferred until a PR is authorized.

- [x] Compose real shared dependencies in focused hardware-free product tests: scheduled and manual file/TTS/Morse, clock boundary, input interruption, timeout and cancel. Verify standalone and Asterisk adapters share the same product policy without new Asterisk dependencies.
- [ ] Run focused tests for each changed source part and targeted Rustdoc; close measured production line/branch gaps incrementally using existing ignored checklist conventions. Do not rerun a complete report after every fix.
- [x] Update ADRs and wishlist to match only implemented behavior, explicitly preserving remaining unrelated planned work. Review architecture traceability and source ownership.
- [x] Run fresh whole-diff correctness and simplification review, formatting, lint and static analysis. Do not claim the full platform gate passed locally.
- [ ] Commit scoped implementation/docs; report exact checks and remaining hosted-gate/node acceptance work. No PR merge, release or node change is authorized by this plan alone.

## Deterministic verification commands

Use `rpt_advanced-workflows/tools/run-in-quality-container.sh` with freshly pulled
`ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest`, recording the digest and
keeping test containers labeled/cleaned by that launcher. This image name is
verified against the maintained quality workflow. In the container, run focused forms:

```sh
cargo test --locked -p rpt-advanced-core schedule::tests
cargo test --locked -p rpt-advanced-core runtime::scheduler::tests
cargo test --locked -p rpt-advanced-core runtime::composition::tests
cargo test --locked -p rpt-advanced-core controller::tail_tests
cargo test --locked -p rpt-advanced-core command_tests
cargo fmt --all -- --check
cargo clippy --locked -p rpt-advanced-core --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p rpt-advanced-core --no-deps
```

Use the repository's existing shared-library search/test environment from the
Makefile. If Docker is unavailable, preserve that evidence and use maintained
GitHub verification only after push authority is confirmed; do not run tests
inside the live radio process or start ad-hoc untracked test containers.
