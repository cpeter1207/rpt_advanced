# ADR Divergence Alignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve the six current ADR divergences without implementing incomplete or future wishlist requirements.

**Architecture:** Amend stale reload and aggregate-root records to match the later generational design. Make three bounded behavioral corrections in the scheduler and PortAudio adapter, remove the unused ring ABI-1 facade, and finish USBRadioPlus's existing Rust ownership migration by replacing substantive C with a minimal Asterisk metadata loader.

**Tech Stack:** Rust 2024/MSRV 1.85, Asterisk public C ABI, bindgen 0.72.1, PortAudio/ALSA, Debian packaging, Doxygen/Rustdoc, Cargo tests, existing project quality launchers.

**Spec:** `doc/superpowers/specs/2026-09-15-adr-divergence-alignment-design.md`

## Global Constraints

- Do not implement schedule warnings, fallback links, new schedule actions, EchoLink, Hamlib, voting, REST, WebSocket, standalone operation, or any other wishlist item.
- Preserve current observable radio, link, configuration, Asterisk channel, CLI, and PCM behavior except where an accepted ADR explicitly requires the correction.
- Write and run each focused regression test before its production change.
- Keep internal PCM normalized `f32`; retain 48 kHz native processing.
- Keep consumers dynamically linked to released versioned shared objects.
- Do not deploy, alter a node, connect a link, publish a release, push, or merge as part of this plan.
- Run targeted checks first. Run only formatting, lint, and static analysis before a later push; the full platform gate remains a pull-request gate.

---

### Task 1: Amend reload and aggregate-root ADRs

**Files:**
- Modify: `doc/architecture/decisions/0001-reload-without-asterisk-restart.md`
- Modify: `doc/architecture/decisions/0014-object-oriented-rust-design.md`
- Modify: `doc/architecture/README.md`

**Interfaces:**
- Consumes: ADR 0026's `RuntimeGeneration`, two-owner adoption, retirement, and hardware-handoff contract.
- Produces: unambiguous ownership terminology used by later implementation tasks.

- [ ] **Step 1: Amend ADR 0001**

Replace stop-before-start wording with complete candidate preparation, atomic publication, independent receive/transmit adoption, hazard-protected retirement, and controlled RF-safe device handoff. State that ADR 0026 owns the detailed lifecycle.

- [ ] **Step 2: Amend ADR 0014**

Document `RuntimeNode` as the per-station policy/lifecycle aggregate and `NodeController` as the bounded audio/transmit-policy aggregate. Retain the existing real-time object constraints.

- [ ] **Step 3: Synchronize the architecture map**

Use those same names in `doc/architecture/README.md`; do not change any executable behavior.

- [ ] **Step 4: Verify and commit**

Run:

```sh
git diff --check
rg -n "stop.*prior worker|NodeController.*LinkManager" doc/architecture
```

Expected: whitespace check passes and no obsolete ownership claim remains.

Commit:

```sh
git add doc/architecture
git commit -m "Align reload and aggregate ownership ADRs"
```

---

### Task 2: Accept `24:00` only as a schedule end

**Files:**
- Modify: `rust/core/src/schedule.rs`
- Modify: `rust/core/src/schedule/tests.rs`
- Modify: `rust/core/src/config/schema_tests.rs`
- Modify: `doc/configuration.md`

**Interfaces:**
- Consumes: strict existing `parse_clock()` and `clock_minute()` behavior for events and starts.
- Produces: an end-only parser returning minute `1440` for exactly `24:00`.

- [ ] **Step 1: Write failing schedule tests**

Add assertions equivalent to:

```rust
assert!(start_time_valid("23:59"));
assert!(!start_time_valid("24:00"));
assert!(end_time_valid("24:00"));
assert!(!end_time_valid("24:01"));
```

Also prove a window ending at `24:00` includes 23:59 and excludes the following
00:00 by using a selected weekday or date rather than an unrestricted daily
window. Prove post-window elapsed time across weekday, month, year, and leap-day
boundaries.

- [ ] **Step 2: Verify RED**

Run:

```sh
cargo test -p rpt-advanced-core schedule::tests
cargo test -p rpt-advanced-core config::schema_tests
```

Expected: new `24:00` end cases fail because `parse_clock()` rejects hour 24.

- [ ] **Step 3: Add the end-only parser**

Implement the narrow shape:

```rust
fn end_clock_minute(text: &str) -> Result<u16, ScheduleError> {
    if text == "24:00" {
        Ok(24 * 60)
    } else {
        clock_minute(text)
    }
}
```

Use it only for exclusive end boundaries and update `elapsed_after_end()` to examine the immediately preceding civil date when end minute is 1440.

Update `doc/configuration.md` to describe `24:00` as valid only for the
exclusive end of a scheduled window.

- [ ] **Step 4: Verify GREEN and commit**

Run the two targeted commands from Step 2, then:

```sh
cargo fmt --all -- --check
git diff --check
git add rust/core/src/schedule.rs rust/core/src/schedule/tests.rs rust/core/src/config/schema_tests.rs doc/configuration.md
git commit -m "Support midnight schedule end boundaries"
```

---

### Task 3: Preserve monotonic activity at timestamp zero

**Files:**
- Modify: `rust/core/src/runtime/link_schedule.rs`
- Modify: `rust/core/src/runtime/aggregate.rs`
- Modify: `rust/core/src/runtime/links.rs`
- Modify: `rust/core/src/runtime/link_schedule_tests.rs`
- Modify: `rust/core/src/runtime/links_tests.rs`
- Modify: `rust/core/src/runtime/aggregate_tests.rs`

**Interfaces:**
- Consumes: `ActivitySnapshot::last_sample() -> Option<u64>`.
- Produces: scheduler activity callbacks and stored state using `Option<u64>` end to end.

- [ ] **Step 1: Write failing zero-timestamp tests**

Add a cold-start case distinguishing:

```rust
let never_observed = None;
let observed_at_boot = Some(0_u64);
```

Prove `Some(0)` starts/resets the quiet interval and survives reload, while `None` retains the conservative cold-start behavior.

- [ ] **Step 2: Verify RED**

Run:

```sh
cargo test -p rpt-advanced-core runtime::link_schedule::tests
cargo test -p rpt-advanced-core runtime::links::tests
cargo test -p rpt-advanced-core runtime::aggregate_tests
```

Expected: the current `u64` sentinel cannot distinguish the two cases.

- [ ] **Step 3: Carry `Option<u64>` without sentinels**

Change `Window.last_activity`, `ControlState.prior_activity_ms`, `activity_ms()`, `LinkScheduler::tick`, and the corresponding `links.rs` callbacks to `Option<u64>`. Compare timestamps only when both are `Some`; never convert `Some(0)` to `None`.

- [ ] **Step 4: Verify GREEN and commit**

Run the three targeted commands from Step 2, then core formatting and diff checks. Commit only the listed files:

```sh
git add rust/core/src/runtime/link_schedule.rs rust/core/src/runtime/aggregate.rs rust/core/src/runtime/links.rs rust/core/src/runtime/link_schedule_tests.rs rust/core/src/runtime/links_tests.rs rust/core/src/runtime/aggregate_tests.rs
git commit -m "Preserve zero-valued receive activity timestamps"
```

---

### Task 4: Add explicit topology-blocked retry state

**Files:**
- Modify: `rust/core/src/link/hub.rs`
- Modify: `rust/core/src/runtime/links.rs`
- Modify: `rust/core/src/runtime/links_tests.rs`
- Modify: `rust/core/src/runtime/render_tests.rs`

**Interfaces:**
- Consumes: peer-advertised topology updates and existing schedule generation/reservation validation.
- Produces: an automatic-route retry state that is blocked on one topology generation and has no ordinary due time.

- [ ] **Step 1: Write failing blocked-route tests**

Prove all of these independently:

```text
final permanent/scheduled dial rejected for Loop -> retained blocked intent
blocked status exposes topology_blocked=true and due_ms=None
ordinary timer/local lifecycle changes -> no retry
changed peer-advertised topology -> one retry becomes eligible
unrelated peer topology change -> remains blocked
ReconnectAll/operator retry -> eligible
configuration generation change -> eligible
ordinary timeout/network failure -> existing exponential retry unchanged
```

- [ ] **Step 2: Verify RED**

Run:

```sh
cargo test -p rpt-advanced-core runtime::links::tests
```

Expected: loop rejection currently returns to normal timed retry or loses explicit blocked state.

- [ ] **Step 3: Implement the smallest explicit state**

Represent blocking directly on `Retry`, for example:

```rust
blocked_topology_evidence: Option<TopologyEvidence>,
```

`TopologyEvidence` is a canonical fingerprint of only the peer advertisements
that contributed to the rejected route. `take_retry()` skips blocked entries
and exposes `topology_blocked = true` with `due_ms = None`. Retry becomes
eligible only when that relevant evidence changes, `ReconnectAll` explicitly
requests another attempt, or configuration revalidation changes the route.
Unrelated peer revisions do not clear the marker. Nonpermanent operator dials
remain unchanged.

- [ ] **Step 4: Verify GREEN and commit**

Run the targeted test, all `rpt-advanced-core` tests, Rustfmt, Clippy with warnings denied, and `git diff --check`. Commit:

```sh
git add rust/core/src/link/hub.rs rust/core/src/runtime/links.rs rust/core/src/runtime/links_tests.rs rust/core/src/runtime/render_tests.rs
git commit -m "Block automatic retries on topology loops"
```

---

### Task 5: Make PortAudio priority elevation best effort

**Repository:** `rptadv-portaudio-alsa-adapter`

**Files:**
- Modify: `src/lib.rs`
- Modify: `src/tests.rs`
- Modify: `include/rptadv_portaudio_alsa_adapter/rptadv_portaudio_alsa_adapter.h`
- Modify: `README.md`
- Modify: `debian/changelog`
- Modify in `rpt_advanced`: `doc/architecture/decisions/0028-remove-res-usbradio-through-hardware-adapters.md`
- Modify in `rpt_advanced`: `doc/architecture/README.md`
- Modify in `rpt_advanced`: `wishlist.md`

**Interfaces:**
- Consumes: injected scheduling operations and existing ABI-2 stats prefix.
- Produces: append-only stats for actual policy/priority and a nonfatal scheduling-limitation indicator.

- [ ] **Step 1: Write failing table-driven scheduler tests**

Cover priority 99 success, lower permitted success, no permitted elevation, unavailable metadata, already-adequate inherited FIFO, both callback starts, successful restoration, genuine PortAudio start failure, and failed restoration after an actual change.

- [ ] **Step 2: Verify RED**

Run:

```sh
cargo test --locked stream_start_
cargo test --locked scheduling_restore_failure
```

Expected: no-permission and unavailable-metadata cases currently return `AUDIO_PORTAUDIO_ERROR` instead of starting.

- [ ] **Step 3: Implement bounded startup selection**

Factor startup-only selection into a result such as:

```rust
struct SchedulingSelection {
    changed: bool,
    policy: i32,
    priority: i32,
    limited: bool,
}
```

Try descending permitted FIFO priorities without callback-time work. If none succeeds or metadata is unavailable, retain inherited scheduling and continue. Restore only after a successful change; restoration failure remains fatal.

- [ ] **Step 4: Append observable stats compatibly**

Append policy, priority, and limitation fields to `StreamStats`, shared stats,
and the C header. Each capture and playback stream reports the actual values
for its own callback worker. Accept the old ABI-2 prefix size and copy only the
caller-advertised prefix so the SONAME remains 2.

Mark ADR 0028 and the architecture map implemented, and remove only the
completed best-effort scheduling item from `wishlist.md`; retain its separate
external-load/xrun investigation item.

- [ ] **Step 5: Verify GREEN and commit**

Run the targeted tests, `make test`, then `make lint static-analysis docs`. Commit:

```sh
git add src/lib.rs src/tests.rs include/rptadv_portaudio_alsa_adapter/rptadv_portaudio_alsa_adapter.h README.md debian/changelog
git commit -m "Make audio priority elevation best effort"
```

Commit the factual ADR/wishlist update separately in `rpt_advanced` after the
adapter tests pass:

```sh
git add doc/architecture/decisions/0028-remove-res-usbradio-through-hardware-adapters.md doc/architecture/README.md wishlist.md
git commit -m "Record best-effort audio scheduling implementation"
```

---

### Task 6: Remove the obsolete PCM-ring ABI-1 facade

**Repository:** `rate_adjusting_pcm_ring-rust`

**Files:**
- Delete: `src/legacy_bridge.rs`
- Delete: `src/legacy_bridge/tests.rs`
- Delete: `src/rate_adjusting_pcm_ring.c`
- Delete: `include/rate_adjusting_pcm_ring.h`
- Delete: `tests/test_ring.c`
- Delete: `tests/test_consumer.c`
- Delete: `rate_adjusting_pcm_ring.pc.in`
- Delete: ABI-1 Debian install/doc manifests
- Modify: `src/lib.rs`
- Modify: `src/ring.rs`
- Modify: `Makefile`
- Modify: `README.md`
- Modify: `QUALITY.md`
- Modify: `Doxyfile`
- Modify: `debian/control`
- Modify: `debian/tests/control`
- Modify: `debian/tests/install-check`
- Modify: `debian/changelog`

**Interfaces:**
- Consumes: proof that USBRadioPlus and rpt_advanced reference only ring ABI 2.
- Produces: one F32 ABI-2 DSO/package/header/pkg-config surface, with unchanged SONAME 2.

- [ ] **Step 1: Verify current consumers before deletion**

Run this production/build/package-only search separately in each consumer,
excluding documentation, plans, tests, and generated output:

```sh
rg -n 'rate_adjusting_pcm_ring\.h|\brpcr_[a-z_]+|librate_adjusting_pcm_ring\.so\.1|librate-adjusting-pcm-ring1|librate-adjusting-pcm-ring-dev|rate_adjusting_pcm_ring\.pc' \
  -- src rust include Makefile debian
```

Expected: no matches. Do not search this implementation plan when asserting
the result.

- [ ] **Step 2: Make distribution checks fail on legacy output**

Add negative staged/archive assertions to the existing install/archive checks for the old DSO, header, pkg-config file, packages, and facade source. Run `make install-check distcheck`; expected failure while ABI 1 is still emitted.

- [ ] **Step 3: Remove ABI-1 build and package surfaces**

Delete the listed files and all `V1_*`, compatibility-test, compatibility-coverage, ABI-1 install, smoke, packaging, and autopkgtest targets. Preserve every ABI-2 target and SONAME.

- [ ] **Step 4: Remove legacy-only Rust branches**

Remove `mod legacy_bridge` and legacy-only ring methods/state. Keep `Ring::create` as the sole constructor and retain ABI-2 behavior unchanged.

- [ ] **Step 5: Verify GREEN and commit**

Run:

```sh
cargo test --all-targets --locked
make test install-check distcheck
make lint static-analysis docs
```

Then confirm the legacy-name audit returns no matches and commit:

```sh
git add -A
git commit -m "Remove obsolete PCM ring ABI 1"
```

---

### Task 7: Move USBRadioPlus channel hosting into Rust

**Repository:** `usbradioplus`

**Files:**
- Create: `rust/asterisk/build.rs`
- Create: `rust/asterisk/wrapper.h`
- Create: `rust/asterisk/src/host/mod.rs`
- Create: `rust/asterisk/src/host/channel.rs`
- Create: `rust/asterisk/src/host/delivery.rs`
- Create: corresponding focused test modules
- Modify: `rust/asterisk/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `rust/asterisk/src/lib.rs`

**Interfaces:**
- Consumes: existing Rust `driver_*`, `channel_*`, `channel_service`, and delivery APIs.
- Produces: Rust-owned Asterisk channel technology, pinned per-channel host state, frame conversion, and delivery worker.

- [ ] **Step 1: Port channel-host tests before host code**

Move the observable assertions from the C shim tests covering delivery callbacks, control/jitter, request/call/answer/hangup/read/write/text/indicate/fixup/setoption/digits, and reload-exclusion lock ordering into Rust fixture tests.

- [ ] **Step 2: Verify RED**

Run the new Rust test targets. Expected: direct Asterisk host types and callbacks do not exist.

- [ ] **Step 3: Add strict public-Asterisk bindings**

Reuse rpt_advanced's bindgen pattern and allowlist only symbols required by the channel host. Do not expose Asterisk types to other USBRadioPlus crates.

- [ ] **Step 4: Implement Rust channel and delivery owners**

Move C channel state/callback translation into pinned Rust objects while calling existing product APIs. Preserve frame sizes, formats, jitter behavior, DTMF analysis, queue ordering, owner locking, worker shutdown, and error returns byte-for-byte.

- [ ] **Step 5: Verify and commit**

Run focused Rust tests, existing adapter tests, Rustfmt, Clippy, Rustdoc, and `git diff --check`. Commit:

```sh
git add rust/asterisk/Cargo.toml Cargo.lock rust/asterisk/build.rs rust/asterisk/wrapper.h rust/asterisk/src/lib.rs rust/asterisk/src/host
git commit -m "Move USBRadioPlus channel hosting into Rust"
```

---

### Task 8: Move USBRadioPlus link audiohooks into Rust

**Repository:** `usbradioplus`

**Files:**
- Create: `rust/asterisk/src/host/link.rs`
- Create: `rust/asterisk/src/host/link_tests.rs`
- Modify: `rust/asterisk/src/host/mod.rs`
- Reduce corresponding code in: `src/chan_usbradioplus_shim.c`

**Interfaces:**
- Consumes: existing Rust `link_prepare`, `link_prepare_reload`, `link_process`, `link_observe`, and `link_destroy` APIs.
- Produces: Rust-owned audiohook/datastore lifetime and staged graph reload.

- [ ] **Step 1: Port failing audiohook lifecycle tests**

Cover graph-before-publication reload, masquerade survival, callback quiescence, datastore destruction, reference lifetime, and statistics.

- [ ] **Step 2: Verify RED, implement minimally, verify GREEN**

Retain Asterisk's required first-member/pinned audiohook layout and use existing Rust link processing unchanged.

- [ ] **Step 3: Run focused checks and commit**

```sh
git add rust/asterisk/src/lib.rs rust/asterisk/src/host/link.rs rust/asterisk/src/host/link_tests.rs
git commit -m "Move USBRadioPlus link hosting into Rust"
```

---

### Task 9: Move USBRadioPlus reload and CLI into Rust

**Repository:** `usbradioplus`

**Files:**
- Create: `rust/asterisk/src/host/reload.rs`
- Create: `rust/asterisk/src/host/reload_tests.rs`
- Create: `rust/asterisk/src/host/cli.rs`
- Create: `rust/asterisk/src/host/cli_tests.rs`
- Modify: `rust/asterisk/src/host/mod.rs`
- Reduce corresponding code in: `src/chan_usbradioplus_shim.c`

**Interfaces:**
- Consumes: existing Rust driver/channel reload transactions, selection/status/command APIs, and hardware commands.
- Produces: Rust-owned serialized reload and unchanged CLI contract.

- [ ] **Step 1: Port failing reload and CLI tests**

Cover configuration selection, multi-channel transaction ordering, control gating, transmitter-state replay, every CLI spelling/usage/output key, EEPROM parsing, flash timing, follow mode, and error output.

- [ ] **Step 2: Verify RED, implement minimally, verify GREEN**

Keep CLI text byte-for-byte compatible so tune utilities need no change. Preserve candidate preparation, commit/rollback, jitter reconfiguration, and live transmitter replay ordering.

- [ ] **Step 3: Run focused checks and commit**

```sh
git add rust/asterisk/src/lib.rs rust/asterisk/src/host/reload.rs rust/asterisk/src/host/reload_tests.rs rust/asterisk/src/host/cli.rs rust/asterisk/src/host/cli_tests.rs
git commit -m "Move USBRadioPlus reload and CLI into Rust"
```

---

### Task 10: Cut over USBRadioPlus module lifecycle and delete substantive C

**Repository:** `usbradioplus`

**Files:**
- Create: `rust/asterisk/src/host/lifecycle.rs`
- Create: `rust/asterisk/src/host/lifecycle_tests.rs`
- Replace: `rust/asterisk/include/usbradioplus_asterisk.h`
- Rewrite: `src/chan_usbradioplus_shim.c`
- Replace: `tests/test_chan_usbradioplus_shim.c`
- Remove obsolete shim fixture internals/stubs
- Modify: `tests/run_c_tests.sh`
- Modify: `Makefile`
- Modify: `Doxyfile`
- Modify: `debian/control`
- Modify: `debian/not-installed`
- Modify: `debian/rules`
- Modify: `tests_py/test_debian_packaging.py`
- Modify: `tests_py/test_release.py`
- Modify: `tests_py/test_validate_release.py`
- Modify: `tools/validate_release.py`
- Modify affected Rustdoc, Doxygen, and packaging documentation

**Interfaces:**
- Consumes: Rust-owned channel, link, reload, CLI, and existing provider descriptors.
- Produces: a minimal loader descriptor with load/reload/unload and a metadata-only C module; its descriptor ABI and DSO SONAME are versioned independently.

- [ ] **Step 1: Write loader forwarding tests**

Test descriptor size/version/capability, provider composition, load/reload/unload forwarding, failure translation, and no substantive C symbols.

- [ ] **Step 2: Verify RED**

Current ABI 3 exposes product operations rather than a lifecycle descriptor and current C contains substantive symbols.

- [ ] **Step 3: Prove the minimum incompatible boundary**

Before changing a public number, demonstrate in a focused test/fixture that the
current descriptor ABI cannot host Rust-owned channel registration, audiohooks,
CLI entries, and lifecycle without retaining substantive C operations. Record
which descriptor fields are incompatible. If the proof does not require a
SONAME change, retain the current SONAME.

- [ ] **Step 4: Introduce the loader descriptor**

Use a narrow table shaped as:

```c
struct urp_asterisk_loader_descriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    const char *capability;
    int (*load)(const struct urp_provider_manifest *providers, void *module_self);
    int (*reload)(void);
    int (*unload)(void);
};
```

Keep the header private. Increment the descriptor ABI version because its
table changes. Change the DSO SONAME only if the Step 3 binary-compatibility
proof requires it. Update the co-packaged module and adapter dependencies
together; retain no obsolete compatibility descriptor.

- [ ] **Step 5: Reduce C to metadata and forwarding**

The C file may resolve provider descriptors, call the three Rust lifecycle entries, and declare `AST_MODULE_INFO`; it must contain no channel, audio, radio, configuration, CLI, DTMF, link, worker, or policy logic.

- [ ] **Step 6: Delete obsolete C fixtures and update artifacts**

Update `Makefile`, `debian/control`, `debian/not-installed`, `debian/rules`, the
three listed packaging/release tests, `tools/validate_release.py`, docs,
Rustdoc/Doxygen inputs, and install/archive checks for the proven SONAME and
minimal loader. Package dependencies must reject a module/adapter descriptor
version mismatch before Asterisk loads it.

- [ ] **Step 7: Verify and commit**

Run targeted Rust and loader tests, `make test`, formatting, Clippy, Rustdoc, Cppcheck/Clang-Tidy/Doxygen for the remaining loader/header, and `git diff --check`. Commit:

```sh
git add -A
git commit -m "Complete USBRadioPlus Rust ownership cutover"
```

---

### Task 11: Cross-repository verification and architecture reconciliation

**Files:**
- Modify only if factual status changed: `rpt_advanced/doc/architecture/README.md`
- Modify only if factual status changed: applicable implementation-status documents

**Interfaces:**
- Consumes: all preceding committed repository changes.
- Produces: evidence that only the six audited divergences were resolved.

- [ ] **Step 1: Re-run the six-item audit**

Confirm:

```text
ADR 0001 wording matches ADR 0026
ADR 0013 has no substantive project C
ADR 0014 names actual aggregate ownership
ADR 0017 three edge rules pass
ADR 0028 elevation denial is nonfatal and observable
ADR 0040 emits no ABI-1 ring artifact
```

- [ ] **Step 2: Run repository preflight checks**

Run each repository's formatting, lint, and static-analysis commands without rewriting sources. Run focused tests already named above. Do not run or claim the full PR quality gates locally.

- [ ] **Step 3: Review scope**

Use `git diff --stat` and `git diff --check` in all four repositories. Search for new wishlist configuration keys, routes, services, or features; expected result is none.

- [ ] **Step 4: Report without integration side effects**

Report commits, targeted results, residual risks, and the exact preflight/full-gate distinction. Do not push, open pull requests, deploy, connect links, or release without a new explicit request.
