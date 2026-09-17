# Direct Callback Production Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Promote the proven USBRadioPlus/rpt_advanced direct PortAudio callback proof of concept into a fail-safe, generation-correct, observable, package-deterministic implementation and commit both repositories.

**Architecture:** USBRadioPlus remains the 48 kHz hardware/DSP owner and invokes separate bounded receive and transmit callbacks directly from PortAudio. rpt_advanced owns controller policy and generation-scoped local media: each operating generation receives a newly prepared PCM ring plus a synchronized qualification ring, so receive and transmit can adopt independently without crossing generations. The existing released rate-adjusting F32 ring supplies drift correction; no new resampler, output ring, timer, or wishlist feature is introduced.

**Tech Stack:** Rust, C-compatible versioned descriptor ABIs, Asterisk public channel APIs, PortAudio/ALSA, `rate_adjusting_pcm_ring2`, Cargo, GNU Make, Debian 13 native quality containers.

**Spec:** `doc/architecture/decisions/0001-reload-without-asterisk-restart.md`, `0002-lock-free-audio-paths.md`, `0025-native-media-routing-and-pcm-ring-ownership.md`, `0026-generational-real-time-runtime-lifecycle.md`, `0027-variable-frame-native-tick-and-adapter-io.md`, `0028-remove-res-usbradio-through-hardware-adapters.md`, `0029-canonical-f32-internal-pcm.md`, `0035-fixed-48khz-native-audio.md`, `0036-asterisk-without-asl3-dependency.md`, `0038-replaceable-control-path-adapter.md`, `0040-initial-alpha-compatibility-policy.md`, and `.work/direct-callback-production-cross-audit.md`.

## Global Constraints

- Do not implement any unimplemented wishlist item.
- Preserve current RF, telemetry, link, and processing behavior except for correcting the audited production defects.
- Native PCM is interleaved or mono canonical normalized `f32` as already established, at exactly 48,000 Hz.
- Audio callbacks allocate no memory, take no lock, perform no logging or I/O, and never wait for control work.
- Each PCM/metadata queue has one named producer and one named consumer.
- Startup, reload, stop, rollback, and failure leave physical PTT deasserted unless a current healthy transmit callback explicitly requests keying.
- A failed candidate reload retains the prior complete operating generation.
- External and separately released components remain dynamically linked; consumers must select the required released SONAME deterministically.
- Initial-alpha compatibility is not required. Update both current consumers and fail incompatible combinations explicitly.
- Write a failing behavior test before every production change and record the red and green commands in the task report.
- Run formatting, lint, and static analysis before commits; run complete repository quality gates only after targeted issues are closed.
- Do not deploy or alter a node.

---

### Task 1: Explicit direct-callback admission and deterministic providers

**Files:**
- Modify USBRadioPlus: `rust/asterisk/include/usbradioplus_asterisk.h`, `rust/asterisk/src/lib.rs`, `rust/asterisk/src/host/channel.rs`, `rust/asterisk/src/tests.rs`, `Makefile`, `debian/control`, `tests_py/test_debian_packaging.py`, `tests_py/test_quality_infrastructure.py`, `tests_py/test_release.py`
- Modify rpt_advanced: `rust/product/include/rptadv_product.h`, `rust/asterisk/src/radio.rs`, `rust/asterisk/src/services.rs`, `rust/asterisk/src/services_tests.rs`, `rust/asterisk/build.rs`, `debian/control`, affected packaging tests

**Interfaces:**
- Produces direct attachment ABI revision 2. The callback descriptor ends with a writable `uint32_t accepted_abi_version` field.
- Caller initializes `accepted_abi_version` to zero. USBRadioPlus writes `URP_AST_DIRECT_CALLBACKS_ABI_VERSION` only after the descriptor has been validated and retained successfully.
- `rpt_advanced` accepts attachment only when `ast_channel_setoption` returns zero and the field equals the requested ABI version. A provider that merely returns zero is rejected before `ast_call`.
- USBRadioPlus links exact `librptadvradio.so.3` and `librptadv_portaudio_alsa_adapter.so.2` SONAMEs from their pkg-config-selected libdirs.

- [ ] **Step 1: Add failing admission tests**

Add a host fixture whose setoption returns zero without touching the acknowledgment, and assert `radio_activate` fails without calling `ast_call`. Add USB tests that malformed descriptors never receive acknowledgment and a valid retained descriptor does.

- [ ] **Step 2: Run only those tests and confirm the expected failures**

Run the smallest existing Cargo test filters for `services_tests` and USB Asterisk direct attachment. Record the failing assertions, not compile/setup errors.

- [ ] **Step 3: Implement the ABI acknowledgment minimally**

Use this exact C layout in both current build boundaries until the USB public header becomes the installed source of truth:

```c
struct urp_ast_direct_callbacks {
    uint32_t struct_size;
    uint32_t abi_version;
    void *receive_context;
    int (*receive)(void *, uint32_t, float *, uint32_t);
    void *transmit_context;
    int (*transmit)(void *, float *, uint32_t, uint32_t *);
    uint32_t accepted_abi_version;
};
```

Write the acknowledgment back with an unaligned-safe store only after `ControlOperation::Direct` succeeds. Check it before starting the reserved Asterisk channel. Remove stale revision-1 literals and use one named constant in each generated boundary.

- [ ] **Step 4: Add failing mixed-prefix linker/package tests**

Create test pkg-config outputs with stale unversioned libraries earlier than the selected current libdir. Assert build commands select the exact required SONAMEs and package metadata requires the direct-capable USBRadioPlus version.

- [ ] **Step 5: Implement exact provider selection**

Pass exact linker filenames (`-Wl,-l:librptadvradio.so.3` and `-Wl,-l:librptadv_portaudio_alsa_adapter.so.2`) with each provider's pkg-config libdir. Apply the same pkg-config-selected-path policy to rpt_advanced ring and samplerate provider linkage. Keep diagnostic source overrides explicit and outside release composition.

- [ ] **Step 6: Run targeted ABI, admission, packaging, and link tests**

Verify old-provider success-without-ack rejects, current attachment starts, malformed/duplicate/late attachment rejects, and dry-run link lines contain only the exact SONAME requirements.

- [ ] **Step 7: Record a reviewed Task 1 checkpoint**

Record exact files, focused test evidence, and task-only diff paths in the ignored SDD report. Existing uncommitted POC prerequisites overlap these files, so defer coherent repository commits to Task 4.

---

### Task 2: Generation-scoped local media, qualification, and health in rpt_advanced

**Files:**
- Modify: `rust/core/src/runtime/aggregate.rs`, `rust/core/src/runtime/generation.rs`, `rust/core/src/runtime/node_host.rs`, their focused tests
- Modify: `rust/product/src/link/ring.rs`, `rust/product/src/worker.rs`, `rust/product/src/worker_tests.rs`, `rust/product/src/host.rs`, `rust/product/src/host_tests.rs`, `rust/product/src/services.rs`, `rust/product/src/services_tests.rs`
- Modify/remove: `rust/asterisk/src/radio.rs` and obsolete-only tests/calls for `ready`, `exchange`, `Frame`, receive state, and scratch PCM

**Interfaces:**
- One `LocalMediaProducer` belongs exclusively to a generation's receive state; one `LocalMediaConsumer` belongs exclusively to the same generation's transmit state.
- `LocalMedia` contains two identically configured released rings: canonical PCM and qualification samples (`0.0` unqualified, `1.0` qualified). Producer writes identical accepted spans to both; unequal acceptance is a visible terminal source fault.
- Consumer renders equal frame counts and targets from both rings. Qualification is `false` when no real qualification sample was rendered; otherwise it is the final real qualification sample at or above `0.5`.
- Each generation gets fresh rings during candidate preparation. Independent adoption therefore cannot consume prior-generation PCM or qualification.
- `RadioHealthSnapshot` uses atomics to expose active direct ABI, RX/TX callback failures, PCM/qualification rejected samples, concealed samples, last RX/TX progress sample counters, and source-stalled state. Formatting remains outside callbacks.
- A stopped, inactive, panicked, malformed, or failed receive callback publishes unqualified status; a transmit failure returns silence and unkeyed.

- [ ] **Step 1: Add failing generation and qualification tests**

Cover RX-first and TX-first replacement, old RX completion racing publication, qualified audio followed by unkey while buffered, unequal callback sizes, positive/negative bounded drift, capture stall with transmit timeout disabled, and rollback. Assert no old marker reaches a new generation and stale capture cannot keep local PTT or activity asserted.

- [ ] **Step 2: Run those tests and confirm behavior failures**

Use focused core/product test filters. Preserve the exact red output in the task report.

- [ ] **Step 3: Add generation-owned local media state**

Generalize the runtime generation's receive/transmit state only enough to carry product-prepared local-media endpoints. Prepare both released rings outside callbacks. Do not add another thread, timer, output ring, resampler implementation, or general abstraction.

- [ ] **Step 4: Make qualification sample-associated and stale-safe**

Write qualification samples alongside accepted PCM. On render, threshold only the real metadata span and force false when no real metadata was produced. Reject a generation if the paired rings ever accept different counts. Clear `RadioStatus` when callbacks gate, stop, fail, or are reclaimed.

- [ ] **Step 5: Add failing observability tests**

Inject overflow, concealment, RX/TX panic/failure, inactive entry, and stopped capture. Assert the existing CLI/status path reports current generation, direct ABI, directional failures, last progress, ring occupancy/drop/concealment, and stale-source state.

- [ ] **Step 6: Implement lock-free health snapshots**

Update atomics in callbacks and copy/format them on control. Do not log or allocate from callbacks. Select status from the current active lease/generation, never the first retained same-name lease.

- [ ] **Step 7: Remove the obsolete Asterisk-paced radio exchange**

Delete unused `ready`, `exchange`, `Frame`, scratch conversion, carrier state, and obsolete-only tests/bindings. Retain reservation, acknowledged attachment, start, and synchronous destroy only.

- [ ] **Step 8: Run focused runtime, reload, concurrency, status, and dead-code tests**

Include concurrent serialized RX/TX callbacks while publishing, stopping, and rolling back a candidate. Run Clippy with warnings denied for affected crates.

- [ ] **Step 9: Record a reviewed Task 2 checkpoint**

Record the generation-scoped ring, qualification, health, and obsolete-path removal in the ignored SDD report; commit it with its POC prerequisites in Task 4.

---

### Task 3: RF-safe USBRadioPlus callback and teardown lifecycle

**Files:**
- Modify: `rust/station/src/runtime.rs`, `rust/station/src/media.rs`, `rust/station/src/program.rs`, their focused tests
- Modify: `rust/driver/src/hardware_host.rs`, its focused tests
- Modify: `rust/asterisk/src/lib.rs`, `rust/asterisk/src/host/channel.rs`, `rust/asterisk/src/host/lifecycle.rs`, corresponding tests

**Interfaces:**
- `SharedHardwareState::publish_transmit_fault()` atomically latches a fault, publishes logical PTT false, clears all generated/keying requests, and remains authoritative until non-real-time controlled recovery starts a healthy stream generation.
- Every terminal TX failure path invokes that operation before returning failure: null/oversized spans, direct callback failure, invalid key value, radio renderer failure, panic boundary, and callback-clock stop.
- Every terminal RX failure publishes receiver keyed false and invalidates direct receive qualification before returning.
- Synchronous stop/hangup first gates new callback entry, stops and joins both PortAudio directions, proves callbacks quiescent, then destroys copied external callback contexts.

- [ ] **Step 1: Add failing keyed-failure tests**

Start with logical and observed physical PTT asserted. Inject each malformed/direct/render failure, then stop future TX callbacks while continuing the hardware service. Assert PTT clears on the first fault and cannot be restored by the stale output snapshot.

- [ ] **Step 2: Run the focused tests and confirm expected failures**

Run station and driver filters only; record the red assertions.

- [ ] **Step 3: Implement one shared RF-safe fault helper**

Consolidate repeated callback failure handling into small allocation-free helpers. They fill output silence, revoke receiver qualification where applicable, latch transmit failure, and increment directional counters. They do not log or submit control work.

- [ ] **Step 4: Add failing teardown/reload stress tests**

Exercise callbacks racing hangup, reload candidate activation, rollback, stream-start failure, and destruction. Use retained test contexts that detect any callback after destroy. Cover independent RX/TX progress and same-name replacement.

- [ ] **Step 5: Harden callback quiescence and recovery**

Ensure adapter stop/join is the barrier before external contexts are released. A failed replacement restores the complete prior generation or stays RF-safe and fault-visible. No stale direct source or qualification survives replacement.

- [ ] **Step 6: Export active-path statistics accurately**

Report direct-path callback/ring health from the active source. Mark the bypassed legacy program ring not applicable rather than returning default zeros as if measured.

- [ ] **Step 7: Run station, driver, Asterisk-host, reload, teardown, and variable-frame tests**

Also run Rustfmt, Clippy with warnings denied, Rustdoc with warnings denied, and `git diff --check` for USBRadioPlus.

- [ ] **Step 8: Record a reviewed Task 3 checkpoint**

Record the fail-safe lifecycle and accurate diagnostics in the ignored SDD report; commit it with its POC prerequisites in Task 4.

---

### Task 4: Production documentation, complete gates, and final commits

**Files:**
- Update rpt_advanced: `doc/architecture/README.md`, `doc/architecture/decisions/0028-remove-res-usbradio-through-hardware-adapters.md`, `doc/implementation-status.md`, affected developer/install/package documentation
- Update USBRadioPlus: `doc/developer.dox`, `doc/rpt-advanced-interface.md`, `doc/hardware-adapter-facade.md`, `doc/native-radio.md`, man pages, package/release documentation, changelog if present
- Update both repositories' package/install verification tests and any cross-repository ABI fixture

**Interfaces:**
- Documentation describes the implemented direct callback path, generation-scoped local rings, failure policy, diagnostics, required provider versions, and rollback/reload lifecycle without claiming pending wishlist capabilities.
- Installed developer artifacts contain the supported public/private-alpha header needed by current consumers; package tests verify its location and dependency ownership.

- [ ] **Step 1: Update architecture and operator/developer documentation**

Remove descriptions of the deleted `rpt-radio`/Asterisk delivery path. Record exact implemented and still-pending ADR portions. Document failure counters and package mismatch diagnostics.

- [ ] **Step 2: Add or update installed-artifact tests**

Build from a release tree, stage-install each repository, compile the cross-repository ABI fixture against installed headers, and inspect `DT_NEEDED` for exact provider SONAMEs.

- [ ] **Step 3: Run targeted documentation and packaging checks**

Run Rustdoc/Doxygen only for affected sources first, plus package/install tests and release validators.

- [ ] **Step 4: Run each repository's complete quality gate**

Run the established deterministic Debian 13 amd64/arm64 gates. Require all platform-independent checks, native build/tests/staged installs, and 100% production line/branch coverage on Debian 13 amd64. Do not build Debian 12.

- [ ] **Step 5: Review the complete diffs and repository statuses**

Confirm no temporary reports, credentials, node artifacts, or unrelated files are staged. Confirm every pre-existing dirty hunk included in a commit is covered by the gate or leave it uncommitted and report it explicitly.

- [ ] **Step 6: Commit remaining documentation/integration changes**

Create final repository-specific commits. Do not push, merge, release, or deploy.
