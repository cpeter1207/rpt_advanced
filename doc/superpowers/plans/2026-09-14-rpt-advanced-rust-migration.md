# rpt_advanced Rust Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace every substantive rpt_advanced production C implementation with behavior-equivalent, documented Rust while retaining only an unavoidable Asterisk module-metadata shim.

**Architecture:** A Rust controller core owns policy and state. A versioned Rust Asterisk-adapter shared object implements the current module boundary and calls public Asterisk APIs through generated FFI; a tiny C module links that adapter and supplies only Asterisk's macro-generated metadata. Existing released shared components remain dynamically linked. Migration proceeds behind characterization tests, then deletes each replaced C implementation instead of retaining parallel paths.

**Tech Stack:** Rust 2024 (MSRV 1.85), Cargo, C ABI at external boundaries, Asterisk public headers, `rate_adjusting_pcm_ring2`, existing rpt_advanced adapter packages, C only for Asterisk module metadata, Rustdoc, Clippy, LLVM coverage, pytest for process-level Asterisk integration.

**Spec:** `doc/architecture/README.md`; ADRs 0001--0003, 0005, 0011--0015, 0018--0023, 0025--0029, 0035--0038, and 0040.

## Global Constraints

- Preserve every currently implemented behavior and external AllStarLink/Asterisk interoperability contract; do not implement `WISHLIST.md` entries.
- Remove project-alpha compatibility-only code under ADR 0040; do not port dead, unreachable, duplicated, or retired implementation.
- Internal PCM is normalized interleaved `f32` at fixed 48,000 Hz. Conversion exists only at actual Asterisk, codec, or hardware boundaries.
- Audio paths are bounded, preallocated, lock-free, allocation-free, nonblocking, non-logging, and panic-contained.
- Dependencies point inward; core code imports no Asterisk, ASL3, PortAudio, Hamlib, FFmpeg, Piper, or other external-library types.
- Use released dynamic shared objects for separately released components. Do not vendor or statically link their implementations.
- A valid reload replaces a complete prepared generation without restarting Asterisk; a failed reload retains the running generation.
- Rust code uses owned domain types, enums, `Result`, RAII, structs with private state, and composition. `unsafe` is confined to audited ABI/buffer modules.
- Current Asterisk-hosted peer ingress may retain its narrow non-audio mutex. No radio audio worker takes it.
- Document Rust with Rustdoc and the retained C shim/header with Doxygen. Require warning-free formatting, lint, static analysis, build, tests, packaging, and install checks.
- Debian 13 amd64 and arm64 are supported. Production line and branch coverage is 100% on Debian 13 amd64 only.

---

### Task 1: Establish the Rust workspace and parity harness

**Files:**
- Create: `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`
- Create: `rust/core/Cargo.toml`, `rust/core/src/lib.rs`
- Create: `rust/asterisk/Cargo.toml`, `rust/asterisk/build.rs`, `rust/asterisk/src/lib.rs`
- Create: `tests/reference/README.md`, `.work/rust-migration-progress.md`
- Modify: `Makefile`, `.gitignore`, `QUALITY.md`, `doc/testing.md`

**Interfaces:**
- Produces: crate `rpt_advanced_core`; adapter crate scaffold `rptadv_asterisk_adapter`; test commands `make rust-check`, `make rust-coverage`, and existing `make check`.
- The C implementation remains the reference only until each later task deletes its replaced source.

- [ ] Add a failing smoke test that imports `rpt_advanced_core::NATIVE_SAMPLE_RATE_HZ` and expects `48_000`.
- [ ] Run `cargo test -p rpt-advanced-core native_rate_is_fixed` and confirm it fails because the crate/API does not exist.
- [ ] Add the minimal workspace, crate metadata, `#![deny(warnings, missing_docs)]`, and documented constant:

```rust
/// Fixed native controller and radio sample rate.
pub const NATIVE_SAMPLE_RATE_HZ: u32 = 48_000;
```

- [ ] Run the smoke test and `cargo fmt --all -- --check`; record exact output in `.work/rust-migration-progress.md`.
- [ ] Add Make targets without removing the C reference build yet; commit the scaffold separately.

### Task 2: Port configuration syntax, document ownership, settings, and schema

**Files:**
- Create: `rust/core/src/config/{mod.rs,document.rs,parse.rs,schema.rs,settings.rs}`
- Create: `rust/core/src/config/tests.rs`
- Delete after parity: `src/config*.c`, `src/document.c`, `src/schema.c`, `src/settings.c`
- Retain only while C consumers remain: corresponding `src/*.h`
- Port behavior from: `tests/test_config*.c`, `tests/test_document.c`, `tests/test_schema.c`, `tests/test_settings.c`

**Interfaces:**
- Produces: `ConfigDocument::parse`, `Schema::validate`, and `ResolvedNodeSettings::resolve` with owned strings and typed values.
- Unknown names/values return warnings plus inherited defaults; only irrecoverable input/structure returns `ConfigError`.

- [ ] Port one literal, table-driven test per distinct parser, inheritance, warning-default, structural-error, and allocation-ownership behavior; run each before its implementation and confirm the expected failure.
- [ ] Implement the smallest owned model:

```rust
pub struct ConfigDocument { entries: Vec<ConfigEntry> }
pub struct Resolution<T> { pub value: T, pub warnings: Vec<ConfigWarning> }
impl ConfigDocument { pub fn parse(text: &str) -> Result<Self, ConfigError>; }
impl ResolvedNodeSettings {
    pub fn resolve(document: &ConfigDocument, node: &NodeId) -> Result<Resolution<Self>, ConfigError>;
}
```

- [ ] Delete compatibility-only `sample_rate_hz`/`codec` recognition and its dedicated warning path; ordinary unknown-option fallback remains.
- [ ] Compare Rust test vectors with the C reference tests, then remove replaced C objects from the build.
- [ ] Run targeted Rust config tests, Rustdoc, Clippy, and the still-relevant C integration tests; commit.

### Task 3: Port deterministic scheduling and command primitives

**Files:**
- Create: `rust/core/src/{access.rs,command.rs,schedule.rs,template.rs,time.rs}`
- Create: `rust/core/src/schedule/tests.rs`
- Delete after parity: `src/link_access.c`, `src/link_command.c`, `src/message_template.c`, `src/scheduled_action.c`, `src/scheduled_event.c`, `src/scheduled_window.c`, `src/time_announcement.c`
- Port behavior from the matching `tests/test_*.c` files.

**Interfaces:**
- Produces: `AccessPolicy`, `DtmfCommandMap`, `MessageTemplate`, `ScheduledAction`, `ScheduledEvent`, `ScheduledWindow`, and `TimeAnnouncement`.

- [ ] Port literal tests for deny precedence, command collection/termination, mapping conflicts, civil-time matching, DST/local-time edge behavior already covered, template substitutions, and 12/24-hour speech/Morse formatting; verify each new test fails first.
- [ ] Implement typed enums and parsers, for example:

```rust
pub enum ScheduledAction { Connect { node: NodeId, permanent: bool }, Disconnect { node: NodeId } }
pub struct MessageTemplate(String);
impl MessageTemplate { pub fn parse(text: &str) -> Result<Self, TemplateError>; }
```

- [ ] Preserve existing schedule behavior only. Do not add fallback links, schedule warnings, REST, WebSocket, or configurable future DTMF operations.
- [ ] Remove replaced C sources and duplicate parsing branches; run focused tests, Rustdoc, and Clippy; commit.

### Task 4: Port real-time signal and telemetry primitives

**Files:**
- Create: `rust/core/src/audio/{mod.rs,dtmf.rs,link_queue.rs,morse.rs,playback.rs,tone.rs}`
- Create: `rust/core/src/policy/{duplex.rs,identifier.rs}`
- Create: focused Rust tests beside each module
- Delete after parity: `module/dtmf.c`, `src/duplex.c`, `src/identifier.c`, `src/link_audio.c`, `src/morse.c`, `src/playback.c`, `src/tone_sequence.c`

**Interfaces:**
- Produces preallocated `DtmfDetector`, `LinkAudioQueue`, `MorseRenderer`, `Playback`, `ToneSequence`, `DuplexPolicy`, and `IdentifierPolicy` operating on `&mut [f32]`.

- [ ] Port tests with hand-derived expected samples/state and run them against missing Rust APIs to establish RED.
- [ ] Implement constructors that allocate/precompute outside real-time calls; tick methods accept borrowed slices and never resize:

```rust
pub trait AudioSource { fn render(&mut self, output: &mut [f32]) -> usize; }
pub struct DtmfDetector { /* fixed state */ }
impl DtmfDetector { pub fn process(&mut self, receiving: bool, audio: &mut [f32]) -> Option<DtmfDigit>; }
```

- [ ] Prove frame-partition invariance and DTMF muting through behavioral tests. Use checked conversion only in boundary tests; do not retain internal `i16` paths.
- [ ] Delete the C implementations and any now-unused public structure fields; run focused tests, Rustdoc, and Clippy; commit.

### Task 5: Port the node controller aggregate

**Files:**
- Create: `rust/core/src/controller/{mod.rs,announcement.rs,courtesy.rs,telemetry.rs,timeout.rs}`
- Create: `rust/core/src/controller/tests.rs`
- Delete after parity: `src/controller.c`
- Port behavior from: `tests/test_controller.c`, controller portions of `tests/test_runtime.c`, `tests/test_link_hub.c`, and `tests/test_worker*.c`

**Interfaces:**
- Produces `NodeController`, the node-policy aggregate root, with elapsed-sample audio methods and control-only operation methods.

- [ ] Port existing sequence tests for half/full duplex, hang time, identifier priority/satisfaction, polite ID deferral, receive interruption to Morse, courtesy cancellation, announcement ordering, ducking, timeout/lockout, and telemetry serialization; observe RED before implementation.
- [ ] Implement composition rather than one procedural state structure:

```rust
pub struct NodeController {
    duplex: DuplexPolicy,
    identifiers: IdentifierPolicy,
    telemetry: TelemetryPlanner,
    timeout: TimeoutPolicy,
}
```

- [ ] Keep control preparation outside `process_audio`; use preallocated queues and atomic snapshots at its boundaries.
- [ ] Delete the C controller and redundant helpers; run controller/audio tests and commit.

### Task 6: Isolate and port speech and file-media preparation

**Files:**
- Create: `rust/media-adapter/Cargo.toml`, `rust/media-adapter/src/lib.rs`
- Create: `rust/core/src/media.rs`
- Delete after parity: `module/assets.c`, `module/speech.c`
- Port behavior from: `tests/test_assets.c`, `tests/test_speech.c`, `tests/test_speech_process.c`

**Interfaces:**
- Produces a versioned descriptor for prepared PCM and speech lifecycle; core consumes an adapter-neutral `MediaPreparer` port.

- [ ] Write failing tests for successful file conversion, Piper synthesis, missing file fallback, unavailable/failed Piper fallback, cancellation, timeout, invalid output, and temporary-file cleanup.
- [ ] Implement a narrow descriptor and safe Rust owner; use direct argument vectors and bounded subprocess lifecycle only outside real-time code:

```rust
pub trait MediaPreparer {
    fn prepare_file(&self, request: &FileRequest) -> Result<PreparedAudio, MediaError>;
    fn prepare_speech(&self, request: &SpeechRequest) -> Result<PreparedAudio, MediaError>;
}
```

- [ ] Preserve the existing file → speech → Morse policy in `NodeController`; the adapter owns mechanics only.
- [ ] Remove the C process implementation, add ABI/Rustdoc/package checks, run focused real-process fixture tests, and commit.

### Task 7: Port Asterisk codec, radio-channel, and connection boundaries

**Files:**
- Create: `rust/asterisk-adapter/src/{bindings.rs,codec.rs,connection.rs,radio.rs}`
- Create: `rust/asterisk-adapter/wrapper.h`
- Port: `tests/test_asterisk_media.c`, `tests/test_connection.c`, `tests/test_radio.c` to Rust adapter tests or keep only C ABI fixture coverage
- Delete after parity: `module/media.c`, `module/connection.c`, `module/radio.c`

**Interfaces:**
- Produces Rust owners for Asterisk channel reservation, `slin48` frame exchange, runtime codec candidate selection, and translation validation.

- [ ] Add failing adapter tests using the existing deterministic Asterisk fixture API for reservation cleanup, unsupported formats, candidate ordering, translation failure, voice/silence/carrier exchange, and fixed 48 kHz local PCM.
- [ ] Generate an allowlisted binding surface from installed public Asterisk headers in `build.rs`; keep Asterisk types inside this crate.
- [ ] Implement RAII owners for channels, formats, translators, and frames. Convert Asterisk PCM to/from canonical `f32` exactly once at this boundary.
- [ ] Delete the replaced C modules, run adapter tests and the synthetic radio integration, then commit.

### Task 8: Port link peer protocol and routing

**Files:**
- Create: `rust/core/src/link/{mod.rs,hub.rs,peer.rs,protocol.rs,topology.rs}`
- Create: `rust/asterisk/src/link/{directory.rs,peer_io.rs}`
- Delete after parity: `module/link_directory.c`, `module/link_hub.c`, `module/link_peer.c`
- Port behavior from: `tests/test_link_directory.c`, `tests/test_link_hub.c`, `tests/test_link_peer.c`, `tests/test_link_integration.py`

**Interfaces:**
- Core produces `LinkManager`, `TopologyManager`, and strict `L`/`K?`/`K` parsers; Asterisk adapter supplies IAX channel, codec, directory, and frame I/O.

- [ ] Write failing tests for admission, allow/deny, self/duplicate/transitive loop rejection, monitor/transceive/permanent state, retry policy, remote DTMF, topology publication, keyed-source query, mix-minus, codec translation, and teardown.
- [ ] Implement protocol/state in core and external I/O in adapter. Keep the allowed Asterisk-only ingress mutex outside radio/audio workers.
- [ ] Use one released `rate_adjusting_pcm_ring2` instance per peer; do not recreate elastic buffering or libsamplerate logic.
- [ ] Remove replaced C implementations and obsolete S16 queues; run focused unit, concurrent-ingress, and Asterisk link integration tests; commit.

### Task 9: Port runtime, scheduling lifecycle, and replaceable control execution

**Files:**
- Create: `rust/core/src/runtime/{mod.rs,generation.rs,node_host.rs,scheduler.rs}`
- Create: `rust/core/src/control.rs`
- Create: `rust/control-asterisk-adapter/Cargo.toml`, `rust/control-asterisk-adapter/src/lib.rs`
- Delete after parity: `module/runtime.c`, `module/worker.c`
- Port behavior from: `tests/test_runtime.c`, `tests/test_worker.c`, `tests/test_worker_thread.c`, relevant integration tests

**Interfaces:**
- Produces long-lived `NodeHost`, immutable `RuntimeGeneration`, separate receive/transmit hazard slots, and `ControlExecutor` accepted/rejected ownership semantics.

- [ ] Write failing tests for complete candidate validation, failed-reload retention, receive-first/transmit-first adoption, stale work, retirement, stalled-owner observability, hardware-handoff rollback, FIFO control execution, rejected ownership, drain, and unload.
- [ ] Implement pre-registered hazard slots and generation-tagged work without callback allocation/refcounting:

```rust
pub struct NodeHost { /* atomic active generation plus fixed hazard slots */ }
pub trait ControlExecutor { fn submit(&self, task: ControlTask) -> Result<(), RejectedTask>; fn stop_and_drain(&self); }
```

- [ ] Move Asterisk taskprocessor calls into the control adapter. Core scheduling remains backend-neutral and never calls control submission from audio.
- [ ] Remove the C runtime/worker implementation; run focused lifecycle/concurrency/reload tests and commit.

### Task 10: Replace the Asterisk module with the Rust adapter

**Files:**
- Create: `module/app_rpt_advanced_loader.c`
- Expand: `rust/asterisk-adapter/src/{lib.rs,module.rs,cli.rs,application.rs}`
- Delete: `module/app_rpt_advanced.c` and obsolete `module/*.h`
- Update: `tests/test_asterisk_module.c`, `tests/test_asterisk_integration.py`, `tests/test_asterisk_independence.py`

**Interfaces:**
- Produces versioned `librptadv_asterisk_adapter.so.1` with Rust-owned load, reload, unload, CLI, application, channel, and link callbacks, plus `app_rpt_advanced.so` linked against it.
- C loader exports only macro-generated Asterisk metadata and forwards lifecycle callbacks.

- [ ] Add a failing installed-module test proving load, active channels, reload in the same Asterisk PID, failed-reload retention, CLI operations, unload, and clean re-load.
- [ ] Implement exported non-panicking lifecycle functions and the minimum metadata shim:

```c
extern int rpt_advanced_load_module(void);
extern int rpt_advanced_unload_module(void);
extern int rpt_advanced_reload_module(void);
/* AST_MODULE_INFO... contains no controller or media logic. */
```

- [ ] Register all callbacks from Rust and quiesce callbacks/tasks before unload. Keep ASL behavior but no ASL3 module/header/symbol dependency.
- [ ] Delete the old module implementation, private C interfaces, and C wrappers that add logic; run module and real-Asterisk integration tests; commit.

### Task 11: Remove the C product surface and update artifacts

**Files:**
- Delete: replaced `src/*.c`, `src/*.h`, obsolete C-only unit tests and fixtures
- Retain: `module/app_rpt_advanced_loader.c` and only integration fixtures needed to test Asterisk's C ABI
- Modify: `Makefile`, `Doxyfile`, `README.md`, `QUALITY.md`, `doc/*.md`, `examples/rpt_advanced.conf`, Debian packaging/workflow inputs

**Interfaces:**
- Installs `app_rpt_advanced.so`, `librptadv_asterisk_adapter.so.1`, required external adapter objects, configuration example, copyright, and developer/user documentation; no static C controller archive or legacy controller headers.

- [ ] Add a failing artifact test that rejects substantive production C, static project/external linkage, missing SONAME dependencies, missing Rustdoc, wrong package dependencies, and stale installed C headers.
- [ ] Make Cargo the production build, keep Make as the conventional orchestration entry, and install only current artifacts.
- [ ] Replace Doxygen-only core documentation with warning-free Rustdoc; retain Doxygen only for the metadata shim/any C-compatible public adapter header.
- [ ] Update architecture/status documents to state actual completion, without claiming wishlist features or standalone completion not delivered here.
- [ ] Run targeted artifact, `distcheck`, staged-install, dependency-boundary, and documentation checks; commit.

### Task 12: Close coverage and integration gaps, then run the full gate

**Files:**
- Modify only files identified by targeted reports
- Record: `.work/rust-migration-progress.md`

**Interfaces:**
- Produces a reviewable branch satisfying the repository quality policy; no deployment or release is part of this task.

- [ ] Run per-crate LLVM line/branch coverage on Debian 13 amd64. For each source part below 100/100, add the smallest behavior test after confirming it fails under a realistic mutation; log completed parts in `.work`.
- [ ] Run targeted Rustdoc, Clippy, formatting, C-shim Cppcheck/Clang-Tidy, integration, packaging, and install checks after each repair; do not repeatedly rerun the entire gate.
- [ ] Pull the current `:latest` quality image, record its digest, deterministically clean labeled stale containers, and run the complete local gate once after every targeted item is clear.
- [ ] Run the same supported Debian 13 arm64 build/test/package checks through the maintained native workflow; no Debian 12 automation.
- [ ] Inspect `git diff --check`, production-C inventory, dynamic dependencies, installed-file manifest, Rustdoc output, and full requirement checklist.
- [ ] Dispatch final code review, resolve findings with targeted tests, and report remaining hardware/manual risks without deploying.
