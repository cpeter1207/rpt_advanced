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
- [x] Commit `feat: add standalone control executor` (`b1e2da8`).

### Task 2: Host-services composition (folded into the standalone executable)

The product already exposes `rptadv_host_services_v4`; a separate library that
only validates a provider list and returns the same callbacks would add no
runtime behavior. The standalone executable will own this table and compose the
selected dynamic providers directly. Its validation and startup rollback tests
are part of Task 4. This removes the redundant adapter crate while preserving
the approved dynamic boundaries for reusable production components.

- [x] Confirm the host-services contract already exists and is consumed by the product.
- [x] Fold provider selection, host callbacks, and completeness validation into the executable lifecycle rather than adding a pass-through DSO.

### Task 3: Versioned IAX2 component, protocol, network I/O, and codec adapters

**Files:**
- Create: sibling source repository `librptadviax2` with its own versioned C ABI, Rust implementation, tests, Debian packages, and quality checks.
- Reuse: `rptadv-shared-library-workflows` for this library's thin caller and CI; do not create a duplicate workflow repository.
- Create in `librptadviax2`: protocol serialization/deserialization, UDP/network adapter, and codec-adapter boundary.
- Modify in `rpt_advanced`: workspace/package metadata to consume the released dynamic object; add ASL3 compatibility fixtures and integration tests.
- Test: `librptadviax2/tests/fixtures/iax2/` and `tests/test_iax2_standalone.py`

**Interfaces:**
- Consumes: the current product peer-dial/read/write host callbacks, node configuration, bounded peer-ingress contract in ADR 0037, and the ASL3-compatible IAX2 behavior established by documentation and source review.
- Produces: independently released `librptadviax2` with a versioned C descriptor. Its protocol module constructs and serializes outgoing IAX2 packets and deserializes incoming packets without owning sockets; its network adapter owns network I/O; separate codec adapters bridge released codec libraries where available. It handles peer/media/control events without calling product policy on network/audio threads. ASL HTTPS registration stays in the standalone product boundary as ADR 0005 requires.

- [x] Review the ASL3 manual first. Where it omits or leaves behavior ambiguous, inspect the relevant Asterisk IAX2 and ASL3 `app_rpt` source; record observed wire behavior and its source location in an ASL3 compatibility matrix before implementation.
- [x] Establish golden packet/session fixtures from that matrix for the currently supported inbound/outbound link behavior, authentication/call-token flow, codec negotiation, keying, DTMF/text signaling, keepalive/timeout, and disconnect. Include ASL3-specific extensions and quirks found in source, not just base IAX2 packets described in the manual. Remaining handshake and topology gaps stay explicitly listed in the matrix.
- [x] Add the Asterisk-compatible stateless inbound CALLTOKEN challenge/continue/reject decision, with fixtures for empty, valid, invalid, absent, malformed, and non-initial NEW input. This does not establish an inbound listener or call session.
- [x] Ensure outbound NEW uses the standard extnode identity (`USERNAME=radio`, remote `CALLED_NUMBER`, local `CALLING_NUMBER`); pin the identity mapping in the loopback call-setup regression.
- [x] Retransmit the pending outbound setup frame after response loss using Asterisk's default bounded retry schedule; verify retries for NEW and AUTHREP over loopback UDP.
- [x] Add a pure inbound u-law `radio` NEW acceptance primitive that validates node identity and codec capability and emits the legacy FORMAT plus version-0 FORMAT2 ACCEPT. It runs only after the caller validates CALLTOKEN and product access policy; it does not establish an inbound listener or call session.
- [x] Seed linked protocol state after an inbound ACCEPT, validating both 15-bit call numbers and the post-NEW/ACCEPT sequence positions before the peer is exposed.
- [x] Map IAX radio-key/unkey control subclasses through named client and product event constants. Gate key-up on `!NEWKEY!`, `!NEWKEY1!`, or the two-second compatibility timeout; always honor unkey. Focused pre-negotiation, timeout, and explicit-disable tests pass.
- [x] Write protocol tests for outgoing packet construction/serialization and incoming packet deserialization, then implement those operations in `protocol.rs` without network I/O. Keep socket handling in `network.rs` and codec handling in `codec.rs` behind adapters to released codec libraries where available.
- [ ] Verify behavioral interoperability against the compatibility matrix: the target is complete interoperability with ASL3 for the supported link behavior, not merely basic connection and audio. Record any behavior that cannot be matched as an explicit gap rather than silently omitting it.
- [x] Implement the bounded per-peer lock-free ingress primitive with fixed packet storage; prove FIFO ordering, queue-full rejection, oversize handling, and stalled-peer isolation. The listener's datagram demultiplexing and product wiring remain open.
- [ ] Implement the HTTPS registration client using configured endpoint/identity, with tests for success, timeout, malformed response, and shutdown cancellation.
- [ ] Run focused tests and a local Asterisk integration test using the project’s existing test image; verify no network or disk operation reaches audio callbacks.
- [ ] Verify the new shared library has no Asterisk/ASL3 dependency and its ABI/package metadata matches ADRs 0011, 0018, 0021, and 0022.
- [ ] Commit `feat: add versioned standalone IAX2 transport` in the component repository; integrate only its released dynamic ABI in `rpt_advanced`.

### Task 4: Standalone executable and service lifecycle

**Files:**
- Create: `rust/standalone/Cargo.toml`
- Create: `rust/standalone/src/main.rs`
- Create: `rust/standalone/src/lib.rs`
- Modify: `rust/core/src/config/scope.rs`, `schema.rs`, and `settings.rs`
- Modify: `examples/rpt_advanced.conf`
- Create: `debian/rpt-advanced.service`
- Create: `debian/rpt-advanced.default`
- Modify: `Cargo.toml`
- Modify: `Makefile`
- Test: `rust/standalone/src/tests.rs`
- Test: `rust/core/src/config/tests.rs`
- Test: `tests/test_standalone_service.py`

**Interfaces:**
- Consumes: control executor, IAX2 adapter, existing configuration loader, and selected native radio/audio adapters.
- Produces: `rpt-advanced --check-config`, `rpt-advanced --foreground`, and systemd lifecycle; the process supplies host-services v4, resolves providers and validates composition before starting product, reload replaces a runtime generation, and shutdown drains control before releasing radio and libraries.

- [ ] Write tests `check_config_never_opens_radio`, `startup_failure_releases_resolved_adapters`, `reload_keeps_process_and_replaces_generation`, and `shutdown_drains_before_adapter_unload`.
- [ ] Verify tests fail against the absent executable before implementation.
- [x] Add the initial `rpt-advanced --check-config FILE` slice: parse and validate using the portable core only, report warnings/errors, and exit without resolving or opening any radio provider.
- [x] Add the secrets-file parser and `--check-secrets-file FILE` validator; enforce service-user ownership and mode 0600 and never echo secret values.
- [x] Add `[radio]` defaults and `[radio <node>]` overrides directly to `rpt_advanced.conf`; resolve stable device selection, CM119 profile/PTT/GPIO configuration, receive/COR/squelch/CTCSS/DCS and transmit-signaling settings, channels, PortAudio buffer requests, and receive/transmit FFmpeg graphs into each node's typed settings.
- [x] Prove a successful radio-setting reload reopens the adapter with the candidate settings and a rejected reload restores the prior host snapshot. Standalone runtime tests assert candidate channel availability after commit and old channel retention after a rejected reload.
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

- **Spec coverage:** Task 1 covers replaceable control; Task 3 covers IAX2/HTTPS and bounded multi-peer transport; Task 4 covers host-services composition, standalone lifecycle, configuration reload, and systemd; Task 5 covers the four approved install combinations; Task 6 covers lock-free guarantees, ABI and dependency boundaries, platform gates, and preserving the existing adapters.
- **Step scan:** Every implementation task starts with named failing tests, then a bounded implementation and focused verification. Package boundaries and provider behavior are explicit; no new wishlist capabilities are included.
- **Type consistency:** Cross-boundary descriptors are versioned `*_v1`; the product host-services boundary stays at v4; Rust tasks consume/produce the existing product callbacks rather than introducing a second product API.
- **Review focus:** The five high-risk input/failure cases each have a targeted test in Tasks 1–5; callback and fairness constraints are exercised in Tasks 1, 3, and 6.
- **Proportion:** The plan maps to the approved system design without specifying packet algorithms or controller policy already owned by existing code.
