# Shared Product Boundaries Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans
> for implementation, with independent reviews of each dynamic boundary.

**Goal:** Share existing radio/controller implementation dynamically while keeping
USBRadioPlus, app_rpt_advanced, and standalone independently installable.

**Architecture:** Extract USBRadioPlus's existing adapter-neutral driver into
`libusbradioplus_product.so.1`; keep Asterisk integration outside it. Move shared
controller configuration/directory policy into the existing `librptadv_product`.
Standalone consumes both product descriptors instead of duplicating their code.

**Tech Stack:** Rust, C-compatible descriptors, Cargo/Make, Debian 13 packages,
existing native adapters and deterministic quality containers.

**Spec:** [Approved design](../specs/2026-10-09-shared-product-boundaries-design.md).

## Global constraints

- Preserve current behavior and every pre-existing dirty file; no node changes.
- Canonical normalized F32, native 48000 Hz, app_rpt boundary 8000 Hz.
- No extra audio queue, converter, lock, callback allocation, or native I/O.
- Reuse current published provider contracts; revise product contracts only
  where necessary and update all callers and package checks together.
- No standalone tuner/UI parity, protocol expansion, or wishlist component split.
- Use targeted tests first; record evidence in ignored `.work/quality-progress.md`.
- Pull maintained `latest` quality images and use the project-labeled launcher.
- Fast checks before push; full native Debian 13 PR matrix, coverage on amd64.
- Developer docs before implementation commits; operator docs before a PR.

## Review focus

1. Extraction must not repeat the recent gain, preemphasis, notch, or route loss.
2. Reload failure and callback overlap must retain the previous live generation.
3. Native standalone must not inherit ASL3 echo, delivery pacing, or frame assembly.
4. Missing/mismatched product DSOs must fail before hardware opens or PTT keys.
5. Package splitting must leave standalone and USBRadioPlus installable separately.

## Baseline

Repositories are siblings under `C:/Users/ChrisPeterson/Documents/Codex/2026-08-30`.
Paths below are relative to the named repository. USBRadioPlus was clean at
inspection; rpt_advanced contained the prior standalone fixes, and librptadviax2
contained the tested ANSWER-handshake repair. Preserve those as input, not cleanup.

`rust/asterisk/src/lib.rs` in USBRadioPlus already exposes opaque driver/channel/
link handles without invoking Asterisk. Its `host/*` modules own actual Asterisk
calls. The controller already shares `librptadv_product`; standalone's direct
core dependency duplicates configuration code, not a second running controller.

### Task 1: Extract the existing USBRadioPlus product boundary

**Files (USBRadioPlus):** add `rust/product/{Cargo.toml,src/lib.rs,include/usbradioplus_product.h}`;
move neutral tests from `rust/asterisk/src/tests.rs` and provider fixtures;
modify workspace manifests, `rust/asterisk/{Cargo.toml,src/lib.rs,src/host/lifecycle.rs}`,
`src/chan_usbradioplus_shim.c`, `Makefile`, and package/install tests.

**Interfaces:** export `usbradioplus_product_descriptor_v1()` returning the
extracted C-compatible driver/channel/link table. Retain its existing opaque
handle, explicit buffer, command/status, reload, and callback semantics. Keep
the Asterisk loader descriptor, raw peer-channel option payload, and external
Asterisk bindings in the adapter. The final product descriptor uses capability
`usbradioplus.product1`, ABI 1, and SONAME `libusbradioplus_product.so.1`.

- [x] Add an executable artifact test: load the product with immediate symbol
  resolution in a process with no Asterisk symbols, create/destroy a configured
  driver through the descriptor, and reject a mismatched descriptor before setup.
- [x] Run that test; establish failure because the independently loadable product
  is absent, rather than a missing unrelated provider.
- [x] Move the existing implementation/tests intact. Replace the Asterisk host's
  direct `product_descriptor()` binding with a validated dynamic descriptor client;
  the metadata shim passes the linked descriptor in the loader manifest, as the
  controller shim already does. Update that loader ABI and validate every required
  operation before creating a driver. Preserve existing internal type names.
  Do not link the product implementation as a Cargo production path dependency.
- [x] Replace host tests' private imports of product/provider fixtures with local
  boundary fixtures; retain the real dynamic integration test. Define direct PCM
  callback ABI types at the header boundary so the adapter need not link station.
- [x] Run moved product tests and existing Asterisk host lifecycle/reload tests.
  Verify the product has no undefined Asterisk symbol and the adapter resolves
  its product implementation through the new DSO.
- [x] Update Rustdoc/header ownership and stage a focused recovery commit only
  after affected checks; preserve unrelated changes.

### Task 2: Share native station preparation and lifecycle

**Files (USBRadioPlus):** `rust/product/src/`, `rust/driver/src/{hardware_host,factory,link}.rs`,
`rust/station/src/{lib,control,media,runtime,update,program}.rs`,
`rust/runtime/src/processing.rs`, `rust/core/src/ffmpeg_graph.rs`.
**Consumers (rpt_advanced):** `rust/standalone/src/{processing,radio_config,
radio_generation,radio_session,radio_activation,audio_callbacks,audio_stream,
program_audio,gpio_device,radio_host}.rs` and their tests.

**Interfaces:** retain existing reserve/start/stop/destroy, direct receive/transmit
callback, reload preparation/activation/finish, command, and status operations.
Provide native station preparation as a typed C-compatible request to the same
product owner, without requiring Asterisk queue/digit callbacks. Its normalized
configuration covers only current radio settings and explicit prepared-graph
inputs; Rust configuration types remain internal. The ASL3 compatibility bridge
continues owning 8 kHz conversion, echo, DTMF delivery, and frame assembly.

- [x] Characterize both current paths using existing configuration fixtures:
  explicit -2 dB input/-6 dB output, disabled optional chains, configured filter
  graphs, CTCSS/DCS gating/tails, output A/B routing, and PTT/CTCSS independence.
  Add a failing native descriptor test that processes without Asterisk services.
- [x] Extract the native station owner and graph preparation from the current
  USBRadioPlus implementation. Preserve standalone's explicit raw graph where
  present instead of replacing its filter response with USBRadioPlus defaults.
- [x] Replace standalone's parallel preparation/device lifecycle with descriptor
  calls; delete the superseded implementations and move their behavioral tests.
- [x] Run native station, gain/filter, variable-frame, reload/rollback, partial
  startup, and stop/quiescence tests. Verify one owner per callback and no added
  ring or latency. Keep device identity exclusive and output RF-safe on failure.
- [x] Update developer docs and record the tested extraction checkpoint.

### Task 3: Remove standalone's embedded controller configuration policy

**Files (rpt_advanced):** add `rust/product/src/configuration.rs`; modify
`rust/product/{src/lib.rs,include/rptadv_product.h}`,
`rust/standalone/src/{lib,foreground,registration,secrets,providers,runtime}.rs`,
`rust/standalone/Cargo.toml`, and affected wrappers/descriptor tests.

**Interfaces:** add a synchronous `inspect_configuration` product-table operation
accepting configuration bytes and a C-compatible visitor/context. Emit resolved
host requirements and diagnostics with borrowed strings valid only during the
visitor call. Records cover enabled node/channel, native station preparation,
effective IAX port, registration URL/interval, and existing secret resolution.
Secure file opening and permissions remain standalone I/O. Use typed records,
not a generic configuration-query language or exposed Rust object layout.

- [x] Add descriptor-level fixtures proving global/per-node inheritance, warnings,
  disabled-node handling, hardware-free check mode, and redacted secret errors.
  Run them before adding the missing product operation.
- [x] Move resolution into the product using the existing core parser/schema.
  Replace standalone's config imports with copied ABI records; coordinate its
  radio request with Task 2 rather than adding another normalization layer.
- [x] Remove the standalone production dependency on `rpt-advanced-core`.
  Preserve its existing configuration paths, warnings, and check-only behavior.
- [x] Run config, registration selection, secrets, listener reload, and runtime
  rollback tests; verify no hardware opens during configuration inspection.
- [x] Update exact descriptor-version/size checks in both hosts and fixtures;
  mismatched combinations must be rejected without invoking function pointers.

### Task 4: Share directory policy while retaining backend I/O

**Files (rpt_advanced):** add `rust/product/src/directory.rs`; modify
`rust/product/src/{host,services}.rs`, product host header,
`rust/asterisk/src/{services,link/directory}.rs`,
`rust/standalone/src/{directory,host_services}.rs`, and related tests.

**Interfaces:** move the existing `DirectoryResolver` and its backend-result
contract into product. Host callbacks supply static/external records, SRV
records, and address results; Asterisk allocation/config/DNS operations and
standalone filesystem/system-resolution operations stay in their backends.

- [x] First characterize static/DNS/external precedence, malformed records,
  authoritative answers, source-address authentication, and current DNS-error
  fallback differences in each host. Pin outcomes, not source text.
- [x] Move the common resolver/tests once. Keep host-specific error translation
  so extraction does not silently standardize distinct existing outcomes.
- [x] Update the host table/consumers together and remove duplicate resolver
  policy from the adapters. Keep current IAX/codec backends unchanged.
- [x] Run directory, authorization, connection, and recent ANSWER-handshake
  regressions without making external calls.
- [x] Update developer documentation and record targeted verification.

### Task 5: Prove dynamic boundaries and independent installation

**Files:** both repositories' Makefiles and Debian package/install manifests;
USBRadioPlus product header/pkg-config metadata; rpt_advanced
`tests/test_{rust_product_surface,asterisk_independence,package_combinations,
standalone_service}.py`; workflow-owned dependency/image inputs where needed.

**Interfaces:** separate radio product runtime/development packages from the
USBRadioPlus Asterisk module; preserve controller runtime, standalone executable,
and Asterisk module package choices. Declare real compatible package minima and
descriptor requirements, not invented release versions or static source copies.
The radio runtime package includes the required AGC plugin; the tuning frontend
stays with the USBRadioPlus integration package. No radio runtime dependency may
pull in Asterisk or the controller.

- [x] Extend artifact checks to the standalone executable, not just DSOs:
  controller implementation must appear only in its product library; shared
  radio implementation must not be embedded in either consumer.
- [x] Add isolated installation/load checks for standalone without Asterisk,
  USBRadioPlus without controller, and both Asterisk adapters together. Verify
  missing or mismatched DSOs fail safely and no `res_usbradio` dependency exists.
- [x] Run targeted tests, formatting, lint/static analysis, Rustdoc/header checks,
  and independent review. Use `tools/run-in-quality-container.sh` from
  `rpt_advanced-workflows`, freshly pulled native images, and labeled cleanup.
- [x] Update architecture ownership/status with verified results; defer operator
  documentation until PR preparation. Do not claim completion of other wishlist
  entries or full gate coverage from targeted checks.
- [ ] Request deployment approval only after build/install evidence is ready.
  Live acceptance will cover standalone, app_rpt_advanced+USBRadioPlus, and
  app_rpt+USBRadioPlus, returning to the user-selected composition afterward.

## Execution and approval checkpoint

Recommend direct implementation in this session, with focused independent
reviews at dynamic-boundary checkpoints. The tasks share ABI and lifecycle
contracts, so competing implementations would add coordination risk.

The user approved this concrete plan on 2026-10-09. Local implementation and
hardware-free artifact/install verification are complete. Evidence is recorded
in `.work/shared-product-boundaries-progress.md`; this is not a full hosted gate
or live acceptance result. Two unchanged ring assertions fail on the original
USBRadioPlus baseline. USBRadioPlus package lint has no errors; the original
controller loader reproduces the same `library-not-linked-against-libc` error
as the candidate. Those pre-existing gate issues are recorded in the ledger.
No node change, release, or live composition switch is authorized by this approval.
