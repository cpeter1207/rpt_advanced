# JSON/REST implementation progress

Plan: [updated checklist](2026-11-19-json-rest-implementation-checklist.md).
Authority: [design](../specs/2026-11-18-json-rest-configuration-design.md),
including the 2026-11-19 instruction to warn and ignore unknown JSON keys and
unsupported values.

## Execution environment and inputs

- Development host: `radio.onionring.rocks`, source workspace
  `/home/codex/rpt-development/json-rest-configuration`, branch
  `codex/json-rest-configuration-20261119`.
- Controller base: `8188d5f0e6a738ac38e04e6fea2ba64c150e5ce9`, package
  `0.1.0-alpha24`, Rust 1.88, product descriptor ABI 7 / product DSO 2.
- Current compatible USBRadioPlus input: release `v0.1.0-alpha35`, source
  `b7dc3d35a242c4bdfea7166b6f959539d500a363`, package `0.1.0~alpha35`, product
  ABI 1, native request ABI 4 and direct-callback ABI 6. Its public INI format
  remains supported. The older alpha34 checkout is not the release input.
- Current schedule input: manifest source
  `9f4c39c2f1f0956c2c8ae3afbb34edf9dd30edc9`, release tag
  `v0.1.0-alpha.2` at `1635b0130b271f26f0e913ee46511259a709ce49`, ABI 1.
- Other provider source/package pins remain authoritative in
  `.github/dependencies.json`; no guessed future provider release or minimum
  version is introduced. The typed radio and hron extensions must be released,
  verified and pinned before their consumers are activated.
- Maintained workflow source: `rpt_advanced-workflows` revision `3110bc4`.
  Its constrained launcher pulls the latest image and records its digest on
  each run. Initial baseline image:
  `ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest` at
  `sha256:c3800ed0094d0d01284b70481d3189a76044042b2900594115db2e2a41f2d0fa`.
- Use the existing one-CPU/1536-MiB constrained runner, serial tests, reduced
  debug information and no incremental compilation. The host initially has
  3.9 GiB of free disk. Do not prune unrelated worktrees, images or containers.
- Iteration logs, RED/GREEN evidence and the execution ledger live under the
  ignored `.work/json-rest/` directory on the development host.

## Preflight decisions

| Boundary | Finding and implementation decision |
| --- | --- |
| Aggregate schema / modular models | The aggregate schema remains a logical inventory. Extract resource-local profiles; do not deserialize its operation envelope as disk state or claim it is the HAL API. Empty ordinary override objects are valid. |
| JSON input / canonical schemas | Warn, ignore and fall back for unknown keys and unknown/invalid/unsupported setting values. Canonical schemas describe normalized accepted/output values. Malformed JSON, duplicate members and unsafe structural/lifecycle relationships are errors when no safe deterministic candidate exists. Core collects value-free warnings; hosts own logging. |
| Scope identity / settings inheritance | Names, local keys, UUIDs and parent selectors stay local. `enabled`, `callsign` and `language` inherit. Arrays replace; named definition objects merge recursively. |
| Protocol defaults / live instances | Defaults do not attach protocols. EchoLink, AllStarLink and phone patch have independent optional instances; shared peer processing is separate. |
| Mailbox override / deletion | A local mailbox shadows a complete identity. Removing it reveals the complete same-key site identity if present, otherwise disables it; never merge or reuse the removed local hash/profile. No tombstone is introduced. |
| Controller / provider input | Remove controller INI at the coordinated cutover. Preserve USBRadioPlus INI; reuse typed native fields and add only the missing checked preparation inputs. Do not serialize controller JSON back through INI. |
| Provider compatibility | Preserve required supported consumers, not compatibility-only obsolete alpha code. Check descriptor sizes/versions and package cohorts before activation. |
| Scheduler transition policy | Recurrence pairing, overlap, missed checks and old permanent-route ownership remain explicit checklist prerequisites before scheduler implementation. No scheduler behavior is changed by the first resource-model slice. |

The DTMF preflight compared all 78 schema defaults with the published inventory:
77 code-derived mappings plus phone-only `*99`; the inactivity default is
3000 ms. No mapping changes are needed for this foundation.

## Code ownership map

| Responsibility | Existing implementation owners |
| --- | --- |
| Configuration and effective settings | `rust/core/src/config/{document,parse,scope,schema,settings}.rs` and domain files `site.rs`, `echolink.rs`, `phone_mailboxes.rs`, `dtmf_macros.rs`, `messages.rs`, `tails.rs` |
| Shared lifecycle and inspection | `rust/product/src/{lifecycle,configuration,configuration_radio,host}.rs` |
| Standalone startup and radio preparation | `rust/standalone/src/{foreground,configuration,native_configuration,radio_activation,radio_host,providers}.rs` |
| Asterisk composition | `rust/asterisk/src/{lifecycle,product,services}.rs` and its CLI modules |
| Local maintenance/API replacement | `rust/standalone/src/local_control.rs`, product coordinator/control boundaries and CLI/tuner consumers |
| Directory resolution | Product `directory.rs`, standalone `directory.rs`, Asterisk `link/directory.rs` |
| Radio provider preparation | USBRadioPlus `rust/product/src/{native_configuration,native_processing,native}.rs` and `rust/product/include/usbradioplus_product.h` |
| Scheduling | Core `schedule.rs`, `runtime/scheduler.rs`, `runtime/link_schedule.rs`; `librptadvschedule` provider header/implementation |
| Commands and media | Core `command.rs`, `dtmf_macro.rs`, `phone_dtmf.rs`, `messages.rs`, `composition.rs`; product `control.rs`, `radio_control.rs`, `media.rs` |
| Packaging and quality | Controller `Makefile`, `debian/`, thin GitHub workflow callers; maintained implementations in `rpt_advanced-workflows` |

## First implementation milestone

Implement the control-plane JSON resource foundation with TDD: owned JSON
decoding, warning diagnostics, local scope metadata and sparse inheritance.
Retain the existing live configuration path until the resource store and typed
provider preparation are complete together. This foundation must not restart
connections, interrupt local/phone audio or alter jitter/PLC behavior.

Baseline verified on the development host: **112 existing configuration tests
passed**, with no production edits, using `cargo test --locked -p
rpt-advanced-core --lib config::` in the maintained image.

Implementation and milestone verification are in progress. Unfinished checklist
items remain unchecked. Node milestones build/test in a home-directory
workspace without changing installed binaries or live configuration.
