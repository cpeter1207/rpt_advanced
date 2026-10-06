# Standalone Lock-Free Controller Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` to implement this plan task-by-task.

**Goal:** Ship `rpt-advanced` as a standalone, non-root systemd service that runs the existing controller without Asterisk or ASL3 while preserving optional Asterisk adapters.

**Architecture:** Add a standalone executable and dynamically loaded native host adapters around the existing `rptadv-product` ABI; keep controller policy in the existing product/core. Add a non-Asterisk control executor and IAX2/HTTPS host transport, using bounded lock-free handoffs and the existing single-owner peer media model. Split Debian packaging so standalone, USBRadioPlus, and the deprecated Asterisk adapter can be installed in the approved combinations.

**Tech Stack:** Rust 2024, existing C-compatible versioned product/adapter ABIs, `crossbeam-queue::ArrayQueue` for bounded control MPSC, `rtrb` bounded peer SPSC queues, PortAudio/ALSA and radio-control adapter packages, IAX2/HTTPS, Debian 13 amd64/arm64, systemd.

**Spec:** `docs/superpowers/specs/2026-10-06-standalone-lock-free-controller-design.md`

## Global Constraints

- Standalone has no build-time, package, link-time, or runtime dependency on Asterisk or ASL3.
- `app_rpt_advanced.so` remains an optional, deprecated Asterisk adapter; `chan_usbradioplus.so` remains optional and maintained.
- USBRadioPlus is independently installable without the standalone controller or `app_rpt_advanced.so`.
- Shared production components use dynamic shared libraries and stable, versioned C-compatible descriptors at cross-language boundaries.
- Standalone installs without optional ASL3 adapters, but its selected native audio/device provider packages are required for a runnable configuration.
- All standalone audio I/O and inter-thread handoffs are lock-free, bounded, and preallocated; audio callbacks do no control, network, disk, or configuration work.
- Preserve ADRs 0002, 0005, 0011, 0018, 0020–0022, 0025–0029, and 0035–0038.
- Require warning-free builds, 100% production line and branch coverage on Debian 13 amd64, and successful Debian 13 amd64/arm64 checks; Debian 12 remains manual-only.
- Do not implement other open wishlist entries or remove/replace existing ASL3 adapters.

## Review Focus

1. **Asterisk leakage through executable, symbols, or package metadata:** test ELF dependencies, build inputs, and apt dependency closure on a host without Asterisk.
2. **Audio callback accidentally reaching control or blocking services:** instrument fake adapters to reject callback allocation, locks, blocking calls, and unbounded work.
3. **Slow/stalled peer starving other IAX peers:** test per-peer bounded ingress, full-queue rejection, ordering, and fairness under a stalled producer.
4. **Partial startup or adapter ABI mismatch leaving radio active:** test missing, duplicate, and incompatible providers and verify rollback/clean shutdown.
5. **ASL3 behavior regressed by package split:** install and exercise the optional `app_rpt_advanced` combination separately from standalone.

---

### Task 1: Standalone control executor

**Files:**
- Create: `rust/control-standalone-adapter/Cargo.toml`
- Create: `rust/control-standalone-adapter/build.rs`
- Create: `rust/control-standalone-adapter/src/lib.rs`
- Create: `rust/control-standalone-adapter/include/rptadv_control_standalone_adapter.h`
- Create: `rust/control-abi/include/rptadv_control_adapter.h`
- Modify: `rust/control-asterisk-adapter/include/rptadv_control_asterisk_adapter.h`
- Modify: `Cargo.toml`
- Modify: `Makefile`
- Modify: `rust/product/Cargo.toml`
- Test: `rust/product/src/control_tests.rs`
- Test: `rust/control-standalone-adapter/src/tests.rs`
- Test: `tests/test_rust_product_surface.py`

**Interfaces:**
- Consumes: the shared versioned C control-task descriptor. The existing product client adapts its neutral `ControlExecutor`/`ControlTask` contract to this provider ABI; the provider does not link or duplicate the controller core.
- Produces: `rptadv_control_standalone_descriptor_v1()` returning the shared versioned C descriptor; a preallocated bounded MPSC FIFO, serialized owner, stop-admission, and drain operations. Queue capacity is fixed at construction (1–65,536 tasks) and rejection returns the original task without running it inline.

- [x] Write tests `submit_runs_once_on_owner`, `full_queue_returns_unrun_task`, `stop_rejects_new_tasks_and_drains`, and `drain_from_owner_is_rejected`.
- [x] Run `cargo test -p rptadv-control-standalone-adapter` and verify the new tests fail on missing crate/API.
- [x] Implement the adapter with `crossbeam_queue::ArrayQueue` and one serialized owner; keep enqueue nonblocking and lock-free, wake the idle owner without making queue submission wait, and return full/stopped tasks to the caller.
- [x] Run focused tests and Clippy with warnings denied; inspect submission for locks and blocking work. Lifecycle drain is intentionally blocking and must run off callbacks.
- [x] Verify the standalone library has no Asterisk/ASL3 dependency or symbols; confirm the Asterisk provider and product tests pass after sharing the descriptor header.
- [x] Exercise the standalone descriptor through the product's real `ControlClient` as well as provider-local tests.
- [ ] Commit `feat: add standalone control executor`.

### Task 2: Standalone native host-services adapter

**Files:**
- Create: `rust/standalone-host-adapter/Cargo.toml`
- Create: `rust/standalone-host-adapter/src/lib.rs`
- Create: `rust/standalone-host-adapter/include/rptadv_standalone_host_adapter.h`
- Modify: `Cargo.toml`
- Modify: `Makefile`
- Test: `rust/standalone-host-adapter/src/tests.rs`
- Test: `tests/test_standalone_host_boundary.py`

**Interfaces:**
- Consumes: `rptadv_product.h` host-services v4 and the selected versioned radio, audio, file, speech, and standalone-control adapter descriptors.
- Produces: `rptadv_standalone_host_services_v1()` plus startup validation that resolves exactly one compatible provider per required service before activating the product.

- [ ] Write tests `required_provider_missing_aborts_before_radio_open`, `duplicate_provider_is_rejected`, `abi_mismatch_aborts_without_activation`, and `complete_manifest_starts_and_stops_once`.
- [ ] Run the focused Rust and boundary tests and verify failures before implementation.
- [ ] Implement manifest validation and host-service forwarding using existing descriptors; do not create a second controller/runtime.
- [ ] Run focused tests and inspect generated ELF dependencies for absent Asterisk/ASL3 requirements.
- [ ] Commit `feat: add standalone host adapter`.

### Task 3: Standalone IAX2 protocol, network adapter, and HTTP registration

**Files:**
- Create: `rust/iax-standalone-adapter/Cargo.toml`
- Create: `rust/iax-standalone-adapter/src/lib.rs`
- Create: `rust/iax-standalone-adapter/src/protocol.rs`
- Create: `rust/iax-standalone-adapter/src/network.rs`
- Create: `rust/iax-standalone-adapter/src/codec.rs`
- Create: `rust/iax-standalone-adapter/include/rptadv_iax_adapter.h`
- Modify: `Cargo.toml`
- Modify: `Makefile`
- Test: `rust/iax-standalone-adapter/src/tests.rs`
- Test: `tests/fixtures/iax2/`
- Test: `tests/test_iax2_standalone.py`

**Interfaces:**
- Consumes: the current product peer-dial/read/write host callbacks, node configuration, bounded peer-ingress contract in ADR 0037, and the ASL3-compatible IAX2 behavior established by documentation and source review.
- Produces: a versioned IAX2 adapter descriptor. Its protocol module constructs and serializes outgoing IAX2 packets and deserializes incoming packets without owning sockets; its network module owns network I/O; codec adapters use released codec libraries where available. The adapter handles peer/media/control events and ASL HTTP registration without calling product policy on network/audio threads.

- [ ] Review the ASL3 manual first. Where it omits or leaves behavior ambiguous, inspect the relevant Asterisk IAX2 and ASL3 `app_rpt` source; record observed wire behavior and its source location in an ASL3 compatibility matrix before implementation.
- [ ] Establish golden packet/session fixtures from that matrix for the currently supported inbound/outbound link behavior, authentication/call-token flow, codec negotiation, keying, DTMF/text signaling, keepalive/timeout, and disconnect. Include ASL3-specific extensions and quirks found in source, not just base IAX2 packets described in the manual.
- [ ] Write protocol tests for outgoing packet construction/serialization and incoming packet deserialization, then implement those operations in `protocol.rs` without network I/O. Keep socket handling in `network.rs` and codec handling in `codec.rs` behind adapters to released codec libraries where available.
- [ ] Verify behavioral interoperability against the compatibility matrix: the target is complete interoperability with ASL3 for the supported link behavior, not merely basic connection and audio. Record any behavior that cannot be matched as an explicit gap rather than silently omitting it.
- [ ] Implement bounded per-peer SPSC ingress; prove ordering, queue-full rejection, and stalled-peer fairness with tests `peer_fifo_preserves_order`, `full_peer_queue_rejects_without_blocking`, and `stalled_peer_does_not_starve_ready_peer`.
- [ ] Implement the HTTPS registration client using configured endpoint/identity, with tests for success, timeout, malformed response, and shutdown cancellation.
- [ ] Run focused tests and a local Asterisk integration test using the project’s existing test image; verify no network or disk operation reaches audio callbacks.
- [ ] Commit `feat: add standalone IAX2 transport`.

### Task 4: Standalone executable and service lifecycle

**Files:**
- Create: `rust/standalone/Cargo.toml`
- Create: `rust/standalone/src/main.rs`
- Create: `rust/standalone/src/lib.rs`
- Create: `debian/rpt-advanced.service`
- Create: `debian/rpt-advanced.default`
- Modify: `Cargo.toml`
- Modify: `Makefile`
- Test: `rust/standalone/src/tests.rs`
- Test: `tests/test_standalone_service.py`

**Interfaces:**
- Consumes: the standalone host manifest, control executor, IAX2 adapter, existing configuration loader, and selected native radio/audio adapters.
- Produces: `rpt-advanced --check-config`, `rpt-advanced --foreground`, and systemd lifecycle; startup resolves providers, starts product only after validation, reload replaces a runtime generation, and shutdown drains control before releasing radio and libraries.

- [ ] Write tests `check_config_never_opens_radio`, `startup_failure_releases_resolved_adapters`, `reload_keeps_process_and_replaces_generation`, and `shutdown_drains_before_adapter_unload`.
- [ ] Verify tests fail against the absent executable before implementation.
- [ ] Implement signal handling and lifecycle outside callbacks; require a non-root service user and never silently fall back when a required provider is missing.
- [ ] Run standalone tests plus an integration test on a Linux host with no Asterisk installed; verify start, config reload, link lifecycle, and clean shutdown.
- [ ] Commit `feat: add standalone controller service`.

### Task 5: Debian package split and install combinations

**Files:**
- Modify: `debian/control`
- Modify: `debian/rules`
- Modify: `Makefile`
- Create: `debian/app-rpt-advanced.install`
- Create: `debian/rpt-advanced.install`
- Test: `tests/test_package_combinations.py`
- Test: `tests/test_rust_product_surface.py`

**Interfaces:**
- Consumes: the built standalone executable, versioned shared libraries, and unchanged `app_rpt_advanced.so` artifact.
- Produces: `rpt-advanced` for standalone and `app-rpt-advanced` for the optional Asterisk adapter; dependencies express actual shared-library/provider requirements and keep USBRadioPlus independent of the controller.

- [ ] Add package-control tests for all four install combinations in the spec and assert standalone has no Asterisk/ASL package dependency.
- [ ] Run the focused package tests and verify they fail on the current single `rpt-advanced` package definition.
- [ ] Move only packaging ownership/install manifests; preserve shared-library versioning and the optional Asterisk adapter binary.
- [ ] Build and inspect Debian packages; verify `dpkg-deb -I`, `dpkg-deb -c`, `readelf -d`, and staged installs for each supported combination.
- [ ] Commit `build: split standalone and Asterisk packages`.

### Task 6: Standalone parity and full verification

**Files:**
- Modify: `tests/test_standalone_service.py`
- Modify: `tests/test_iax2_standalone.py`
- Modify: `tests/test_package_combinations.py`
- Modify: `doc/architecture/README.md`
- Modify: applicable ADRs and wishlist status
- Modify: operator manuals and examples before PR only

**Interfaces:**
- Consumes: all earlier standalone interfaces and the unchanged Asterisk adapter path.
- Produces: evidence-backed acceptance of startup, reload, IAX2 interoperability, bounded lock-free media handoffs, package independence, and unchanged optional Asterisk integration.

- [ ] Run targeted production line/branch coverage as each source component closes; record results in ignored `/.work/quality-progress.md`.
- [ ] Run the full required gate only after targeted component checks pass: formatting/lint/static analysis/Doxygen once, then Debian 13 amd64 and arm64 tests/install checks in parallel, with 100% production line and branch coverage on amd64.
- [ ] Run the adapter integration tests to verify `app_rpt_advanced.so` still works; install and smoke-test all supported package combinations.
- [ ] Update user-facing documentation immediately before PR; document supported ASL3 interoperability and known limitations without adding unrelated wishlist behavior.
- [ ] Commit `test: verify standalone controller integration`.

## Self-review

- **Spec coverage:** Tasks 1–2 cover replaceable control and dynamic host providers; Task 3 covers IAX2/HTTPS and bounded multi-peer transport; Task 4 covers standalone lifecycle, configuration reload, and systemd; Task 5 covers the four approved package combinations; Task 6 covers lock-free guarantees, ABI and dependency boundaries, platform gates, and preserving the existing adapters.
- **Step scan:** Every implementation task starts with named failing tests, then a bounded implementation and focused verification. Package boundaries and provider behavior are explicit; no new wishlist capabilities are included.
- **Type consistency:** Cross-boundary descriptors are versioned `*_v1`; the product host-services boundary stays at v4; Rust tasks consume/produce the existing product callbacks rather than introducing a second product API.
- **Review focus:** The five high-risk input/failure cases each have a targeted test in Tasks 1–5; callback and fairness constraints are exercised in Tasks 1, 3, and 6.
- **Proportion:** The plan maps to the approved system design without specifying packet algorithms or controller policy already owned by existing code.
