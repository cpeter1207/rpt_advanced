# Shared Radio Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Execute continuously with focused TDD, task review, and the final hosted gate.

**Goal:** Close all seven audited USBRadioPlus/standalone gaps through shared implementations.

**Architecture:** USBRadioPlus's existing shared product owns radio configuration, physical resources, DSP and maintenance. Thin ASL3 and standalone adapters expose those operations. One tuner and its calibration algorithms use composition-specific configuration and transport adapters.

**Tech Stack:** Existing Rust workspaces and versioned C descriptors; existing dynamic radio/audio/GPIO/processing providers; local Unix-domain control transport; native Debian 13 quality images.

**Spec:** `docs/superpowers/specs/2026-11-03-shared-radio-parity-design.md` (approved 2026-11-03).

## Global constraints

- Reuse the current `refactor/shared-radio-parity` branches and existing checkouts. No new worktrees or repositories.
- Move implementations; remove replaced code. No duplicate DSP, radio builder, calibration, EEPROM codec, device ownership or meter computation.
- Preserve composition defaults, native 48 kHz normalized F32, callback safety, single-device ownership and configuration fallback rules.
- Preserve published descriptor prefixes and existing radio/GPIO contracts. Use sized tails first; document any unavoidable coordinated ABI change and reject mismatched providers before use.
- Standalone EEPROM remains opt-in; valid enabled tuning has existing USBRadioPlus precedence. Startup never writes EEPROM.
- The only new operator transport is the approved root/service-only local Unix socket. No TCP, REST, WebSocket, Hamlib, remote-base or other wishlist implementation.
- No live-node install, configuration change, restart, link or RF operation is authorized by this plan.
- Before every code commit update meaningful developer documentation and run affected checks. Pushes require fast preflight; PRs require the full hosted gate. Debian 12 remains manual-only.
- Record targeted test, source coverage and documentation closures in ignored `.work/quality-progress.md`; do not repeat closed reports. Full coverage applies only to production code, on Debian 13 amd64.

## Review focus

1. Explicit native settings must not silently acquire ASL3 defaults or EEPROM tuning when disabled.
2. Client loss during a keyed calibration must clear only its temporary key/tone state and preserve controller intent.
3. Same-device edits must not open another HID/audio owner or interrupt callback/PTT operation; failed updates retain the active generation.
4. Named profile selection, invalid values and missing profiles must use the existing shared resolver's warnings and inherited fallbacks.
5. Native diagnostics must report actual physical measurements, not substitute controller ring levels or invented successful operations.

## Task 1: One radio and processing configuration owner

**Files / ownership:**
- USB `rust/core/src/config_document.rs`, `station_config.rs`, and a focused native-composition mapping module.
- USB `rust/station/src/lib.rs` and `hardware.rs`: retain the existing common radio/session and hardware plan builders.
- USB product `native_processing.rs`, `native.rs`, descriptor/header: shared configuration resolution boundary.
- RPT `rust/core/src/config/{settings,schema,parse,document,scope}.rs`, product `configuration.rs`, standalone `configuration.rs` and `native_configuration.rs`.

**Interfaces:** Expose side-effect-free resolution from borrowed configuration text plus node identity to a copied/visited resolved native request. Use shared `ChannelConfiguration`, `radio_session_config` and `hardware_plan`; explicit composition defaults are data/policy in that shared owner. The controller forwards text/selectors and consumes the public descriptor rather than importing USB private Rust crates.

- [x] Write failing parity cases for native defaults, asymmetric CTCSS mappings, independent CPU savers and explicit named processing profiles. Preserve an existing raw-graph/gain case.
- [x] Observe the focused RED failures and log their exact commands.
- [x] Move native radio option interpretation/defaults and construction into the shared owner. Keep only composition field aliases in adapters.
- [x] Preserve all selected named profiles and node selectors in forwarded configuration.
- [x] Replace controller radio/session construction with shared-provider delegation; delete `rust/product/src/radio_configuration.rs` and obsolete mirrored types/parsing/tests once callers are replaced.
- [x] Run affected configuration/station/ABI tests and developer docs. Verify equivalent explicit settings yield equivalent radio/hardware plans and invalid/missing settings preserve shared fallback semantics.
- [ ] Review the source move and ABI ownership, then commit the coherent slice.

## Task 2: Shared native physical lifecycle, maintenance and diagnostics

**Files / ownership:**
- USB `rust/driver/src/hardware_host.rs` and its existing fake-provider tests.
- USB `rust/station/src/{runtime,control,media}.rs` and the existing prepared update implementation.
- USB `rust/product/src/{lib,native}.rs`, a focused maintenance module if needed, public product header and native tests.

**Interfaces:** Native status/command forwarding on the existing product descriptor, backed by the same semantic operations as channel status/commands. Reuse current mixer, tone, EEPROM, signaling and radio snapshots; retain native link observation. Native startup consumes the shared hardware plan, selected identity and preflight result.

- [x] Write failing native tests for physical snapshots, typed mixer/tone/EEPROM commands, parallel transport and same-device ownership.
- [x] Observe RED before changing implementations.
- [x] Factor shared physical status and maintenance operations from the existing channel implementation; keep ASL-only echo/frame/DTMF transport outside native records.
- [x] Reuse parallel-plan construction and the existing GPIO service; unconfigured parallel hardware remains unopened.
- [x] Reuse EEPROM preflight/read/apply before immutable preparation, including existing live-owner reads for reload. Reuse EEPROM save/read/write and conversion code.
- [x] Factor the existing prepared forward/reverse update and independent RX/TX acknowledgment primitive for native updates; do not copy its transaction.
- [x] Test disabled/valid/invalid/unreadable EEPROM, precedence, no startup writes, update rollback, no second device open, and no-stream-interruption updates.
- [x] Test temporary manual PTT/CTCSS intent merging with the direct controller callbacks, cleanup and stopped/stale handles.
- [ ] Run targeted tests/docs/coverage for the changed owner components, review, and commit.

## Task 3: Standalone maintenance bridge and local control adapter

**Files / ownership:**
- RPT standalone `radio_activation.rs`, `radio_host.rs`, `host_services.rs`, `runtime.rs`, `foreground.rs` and one focused local-control module.
- RPT product `services.rs` and existing serialized Engine command patterns; public host/product tables only as required for typed forwarding.
- Standalone service install artifacts for the owned runtime directory/socket.

**Interfaces:** `ActiveRadio` retains validated native maintenance/snapshot operations beside lifecycle/site methods. A bounded local Unix listener translates versioned typed maintenance requests to the existing control executor. SDK/domain implementation remains in USBRadioPlus.

- [x] Write failing forwarding tests with fake descriptors and a local socket test harness; no hardware or live-node dependencies.
- [x] Observe RED.
- [x] Add native operation forwarding, generation/lease validation and lifecycle quiescence using the existing site-operation bridge as the pattern.
- [x] Add `/run/rpt-advanced/radio-control.sock`, mode 0600, through the owning service's runtime directory. Bound frames/admissions; never accept arbitrary shell commands or configuration paths.
- [x] Tie temporary maintenance key/tone ownership to a connected session. Test disconnect, cancellation, stale generation, reload and shutdown cleanup while normal controller PTT remains asserted when appropriate.
- [x] Forward physical snapshots and shared graph observations alongside existing controller status. Do not compute measurements twice.
- [x] Test permissions, malformed/oversized/unknown requests, unavailable/disabled nodes, saturated admission and service cleanup.
- [ ] Run targeted tests/docs/coverage, review, and commit.

## Task 4: One tuner frontend for both compositions

**Files / ownership:**
- USB `rust/tune/src/{menus,catalog,control,arguments,repository,policy,main}.rs` and focused backend modules.
- USB Debian/install artifacts to make the utility installable without the Asterisk channel adapter.

**Interfaces:** Keep one `TunerApp` and one set of calibration loops. Add a configuration-resolution/edit-destination port and a standalone runtime backend; keep the existing Asterisk backend. The selected backend controls prerequisite checks, configuration path/scopes and runtime application.

- [x] Run the existing calibration scenarios against a second fake composition and observe failures for wrong destinations/backend assumptions.
- [x] Add backend selection and standalone configuration destinations without duplicating the menu catalog, shared resolver/defaults or calibration algorithms.
- [x] Add the Unix client for the Task 3 protocol and reuse existing radio status parsing/typed command semantics.
- [x] Preserve file atomicity, restore points, rollback, bounded convergence, clipping checks, reference levels and unkey/tone-off cleanup.
- [x] Prove RX noise/squelch, RX voice, RX CTCSS, TX voice/tone/auxiliary calibration, status/meters, EEPROM save and supported live reloads through both backend adapters.
- [x] Keep ASL-specific controls conditional; do not add native echo or hardware repeat to standalone.
- [x] Split packaging only as needed so the same tuner executable works without Asterisk. Verify upgrade ownership and prerequisites.
- [ ] Run targeted tests/docs/coverage, review, and commit.


## Task 5: Coordinated admission, documentation and full verification

**Files / ownership:** Affected dependency manifests, SDK/version checks, package combinations and install checks in the production/workflow repositories; ADRs 0028/0036 and the corresponding architectural inventory.

- [ ] Audit all seven original gaps against concrete paths and tests, including removal of replaced implementations.
- [ ] Update developer documentation before commits and operator docs/examples before PRs; describe current operation plainly.
- [ ] Complete targeted production coverage/source closures and record them once.
- [ ] Run fast formatting/lint/static preflight using freshly pulled `latest` images and the maintained labeled launcher; clean task-owned containers.
- [ ] Push validated feature code for hosted checks; satisfy separately released dynamic dependency requirements through an approved concrete release step if required.
- [ ] Run the full hosted Debian 13 amd64/arm64 gate, with production-only 100% line/branch coverage on amd64, then verify the actual artifact and exact tested source tree.
- [ ] Perform independent whole-branch review for shared ownership, callback safety, default preservation, maintenance cleanup and duplicate/dead code.
- [ ] Report tested completion and any physical qualification limits. Do not merge, release or activate node code without authority for that step.
