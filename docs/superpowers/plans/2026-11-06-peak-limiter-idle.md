# Peak Limiter and Idle Integration Implementation Plan

> **For agentic workers:** Use subagent-driven development with the scoped ownership below.

**Goal:** Implement requirements P1-P4, verify them, and install the matched cohort on node524950.local.

**Architecture:** Reuse the shared FFmpeg final limiter and existing output analyzer in every role. Retain legacy graph ownership and add an Asterisk timer/framehook using the existing serialized boundary.

**Tech Stack:** Rust, FFmpeg, Asterisk public APIs, existing Debian13 QA/native build tooling.

**Spec:** ../specs/2026-11-06-peak-limiter-idle-design.md

## Global Constraints

- Preserve existing 3-second LUFS AGC controls and radio calibration.
- Meter always follows Peak Limiter and has no enable control; Peak Limiter remains configurable.
- No duplicate DSP/provider implementation, new hardware owner, callback allocation/locks/logging, or backlog replay.
- Existing explicit node installation authorization applies; no push or publication.

## Tasks

### common_peak

Owner: inspect_usbradio; requirements: P1, P3.

- rust/core/src/processing_profile.rs
- rust/core/src/profile_resolution.rs
- rust/core/src/ffmpeg_graph.rs

- [ ] Add and run meaningful regression RED.
- [ ] Apply the smallest shared implementation after matching routing admission.
- [ ] Run targeted GREEN, formatting, lint, documentation and appropriate coverage.
- [ ] Review source/API/config/package integration; record exact evidence.
- [ ] Checkpoint only owned changes; parent coordinates source pins and installation.

### tuner

Owner: inspect_hybrid; requirements: P1, P2, P3.

- rust/tune/src/catalog.rs
- rust/tune/src/menus.rs

- [ ] Add and run meaningful regression RED.
- [ ] Apply the smallest shared implementation after matching routing admission.
- [ ] Run targeted GREEN, formatting, lint, documentation and appropriate coverage.
- [ ] Review source/API/config/package integration; record exact evidence.
- [ ] Checkpoint only owned changes; parent coordinates source pins and installation.

### mandatory_meter

Owner: root; requirements: P2, P3.

- rust/runtime/src/processing.rs
- rust/station/src/runtime.rs
- rust/asl3/src/link.rs
- rust/product/src/lib.rs
- rust/product/src/native_processing.rs
- rust/product/include/usbradioplus_product.h

- [ ] Add and run meaningful regression RED.
- [ ] Apply the smallest shared implementation after matching routing admission.
- [ ] Run targeted GREEN, formatting, lint, documentation and appropriate coverage.
- [ ] Review source/API/config/package integration; record exact evidence.
- [ ] Checkpoint only owned changes; parent coordinates source pins and installation.

### legacy_idle

Owner: agc_detector; requirements: P4.

- rust/asterisk/src/host/link.rs
- rust/asterisk/wrapper.h
- rust/asterisk/build.rs

- [ ] Add and run meaningful regression RED.
- [ ] Apply the smallest shared implementation after matching routing admission.
- [ ] Run targeted GREEN, formatting, lint, documentation and appropriate coverage.
- [ ] Review source/API/config/package integration; record exact evidence.
- [ ] Checkpoint only owned changes; parent coordinates source pins and installation.

### integration_install

Owner: root; requirements: P1, P2, P3, P4.


- [ ] Add and run meaningful regression RED.
- [ ] Apply the smallest shared implementation after matching routing admission.
- [ ] Run targeted GREEN, formatting, lint, documentation and appropriate coverage.
- [ ] Review source/API/config/package integration; record exact evidence.
- [ ] Checkpoint only owned changes; parent coordinates source pins and installation.

