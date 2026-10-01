# Streaming Telemetry PCM Implementation Plan

**Goal:** Produce every telemetry source outside the transmit worker, stream its PCM through the shared ring from a dedicated per-node media worker, and make `Playback::render` consume that ring without blocking or allocating.

**Architecture:** The station-telemetry worker owns file/speech subprocesses, Morse/tone generation, and ring producers. It incrementally writes normalized source-rate `f32` samples to the released rate-adjusting PCM ring, which converts to the fixed 48-kHz native rate. Playback receives a bounded, generation-tagged ring consumer handle and polls it from the transmit callback. Ring consumers and their backing ring must be retired off the callback after cancellation/quiescence. The transmit worker only reads and mixes the resulting telemetry PCM.

**Tech stack:** Rust core/product/media-support crates; C ABI v2 for the private file and speech adapters; existing dynamically linked `rate_adjusting_pcm_ring` ABI unchanged; FFmpeg pipe output and Piper raw stdout.

**Spec:** `docs/superpowers/specs/2026-09-29-streaming-telemetry-pcm-design.md`

**Global constraints:** No callback allocation, blocking, logging, subprocess, file I/O, or ring creation/destruction. Preserve routing, ordering, gain, ducking, interruption, and existing fallback policy. Use finite-media ring settings with PLC disabled and zero reserve/target. Preserve cancellation safety and runtime-generation ownership. No user-facing documentation changes until immediately before a pull request. Keep edits scoped to streaming station telemetry.

**Review focus:**

1. File failure before the first accepted PCM sample falls back to speech; speech failure before its first sample falls back to Morse.
2. Failure after any PCM has been accepted drains only that accepted audio, reports the error, and does not replay or restart the item.
3. Ring backpressure and short writes never block or lose sample order; `Playback::render` consumes only available samples and produces no synthetic silence as media.
4. Interrupt, reload, cancellation, and node retirement cannot destroy a ring or adapter context while the callback can still access it; final destruction is off the callback.
5. Source-rate discovery, resampling, end-of-stream, and producer failure preserve correct 48-kHz output duration and ordering.

## Planned changes

### Task 1: Add the core nonblocking PCM playback contract

**Files:**

- `rust/core/src/audio/playback.rs`
- `rust/core/src/audio/playback_tests.rs`
- `rust/core/src/audio/mod.rs`
- `rust/core/src/controller/telemetry.rs`
- `rust/core/src/controller/tests.rs`

Define a small core-owned PCM consumer interface that returns the number of samples currently available plus terminal/error state, without waiting. Store this already-created consumer in `Playback`; make rendering drain available PCM and finish only after producer EOF and ring drain. Remove the transmit-worker Morse/tone branches; the producer selects interruption fallback and streams its PCM. Provide a reader-extraction method so interrupted or completed handles can be transferred for off-callback retirement rather than destroyed by `render`.

**Produces:** the nonblocking `PcmStreamReader`/`PcmRead` contract and a `Playback` API that accepts and returns the reader without destroying it in the audio callback. Bounded job identifiers, ready queues, and retirement queues are owned by Task 5.

**TDD:** Add tests for partial reads, empty-but-open streams, EOF after drain, interruption, restart, and reader extraction. Run the focused core tests and verify the tests fail against the current whole-vector behavior; then implement and rerun them.

### Task 2: Advance the private file/speech adapter ABI to streaming ABI v2

**Files:**

- `rust/media-support/src/abi.rs`
- `rust/media-support/src/provider_tests.rs`
- `rust/media-support/include/rptadv_media_types.h`
- `rust/file-adapter/include/rptadv_file_adapter.h`
- `rust/speech-adapter/include/rptadv_speech_adapter.h`
- `rust/product/src/media.rs`
- `rust/product/src/media/tests.rs`

Replace whole-result prepare calls with open/read/close stream operations. Opening returns a validated source sample rate; reads run only on the station-media worker and provide bounded chunks until EOF/error/cancellation. Update descriptor validation to require v2 and reject missing or malformed stream operations. Keep the PCM-ring ABI and SONAME unchanged.

**Consumes:** Task 1's worker-side immutable media-job descriptions. **Produces:** separate file/speech stream descriptors with source-rate, bounded-read, terminal-status, and close operations.

**TDD:** Add ABI tests for v2 acceptance, v1/undersized/missing-callback rejection, rate validation, read errors, close-on-all-paths, and cancellation. Run focused media-support and product media tests.

### Task 3: Stream FFmpeg and Piper output from their producer pipes

**Files:**

- `rust/media-support/src/process.rs`
- `rust/media-support/src/process_tests.rs`
- `rust/media-support/src/lib.rs`
- `rust/media-support/src/provider_tests.rs`
- `rust/media-support/tests/preparation.rs`

Have FFmpeg emit a documented float PCM stream with its actual sample rate supplied by the selected output format, and read it in bounded chunks from stdout. Have Piper use raw PCM stdout (`--output_raw`) and expose its configured native sample rate. Drain or close stderr safely, preserve timeout/cancellation behavior, and ensure child cleanup is deterministic. Remove the now-obsolete whole-file temporary WAV/read-all path where no longer used.

**Consumes:** Task 2's v2 stream operations. **Produces:** incremental normalized `f32` chunks plus validated source-rate and terminal status.

**TDD:** Use controlled helper processes to prove the first chunk arrives before process completion, sample-rate metadata is correct, and EOF/error/timeout/cancellation close the child and stream. Run only process/provider/preparation tests for this task.

### Task 4: Bridge per-node media workers to the shared rate-adjusting ring

**Files:**

- `rust/product/src/media.rs`
- `rust/product/src/host.rs`
- `rust/core/src/runtime/aggregate.rs`
- `rust/core/src/runtime/node_host.rs`
- `rust/product/src/media/tests.rs`

Create one generation-owned station-media worker for each configured node. It opens one file or speech stream at a time, creates the shared PCM ring off the audio path using the discovered source rate and 48-kHz output, pushes chunks as they arrive, and publishes the consumer through a bounded generation-tagged handoff. Handle partial producer acceptance by retrying or yielding on the worker only. Tie worker cancellation, consumer retirement, and ring destruction to the existing generation/quiescence lifecycle. Do not change the shared ring ABI.

**Consumes:** Tasks 1–3's job, stream, and reader contracts. **Produces:** callback-ready stream handles and control-side retirement acknowledgments. One active telemetry stream per node remains consistent with the existing serialized transmitter.

**TDD:** Test incremental production/consumption before source EOF, source-rate conversion, ring backpressure, stale generation rejection, worker cancellation, and off-callback ring destruction. Run focused product media/host/runtime tests.

### Task 5: Request streamed media from runtime and preserve controller behavior

**Files:**

- `rust/core/src/runtime/prepare.rs`
- `rust/core/src/runtime/prepare_tests.rs`
- `rust/core/src/runtime/aggregate.rs`
- `rust/core/src/runtime/tests.rs`
- `rust/core/src/controller/telemetry.rs`
- `rust/core/src/controller/tests.rs`
- `rust/core/src/controller/mod.rs`

Replace synchronous full-audio preparation for IDs, announcements, courtesy media, tone sequences, Morse, and spoken command status with bounded media-worker requests. Keep fallback selection correct when no PCM has yet been published. Preserve priority and routing, including local-only IDs and command-source telemetry. Ensure the active transmit callback sees only nonblocking stream reads and bounded queue handoffs.

**Consumes:** Task 1's reader contract and Task 4's product worker. **Produces:** runtime-triggered playback requests for configured and command telemetry, with existing controller policy unchanged.

This task owns the bounded callback request IDs, ready-stream handoff, and off-callback retirement queues; callbacks carry identifiers/handles only, never owned media descriptions.

**TDD:** Add regression tests for source selection/fallback, no premature keying or end-of-media, ordering, gain, interruption and restart behavior, generation reload, and existing routing. Run focused core runtime/controller tests plus product runtime integration tests.

### Task 6: Remove whole-result media plumbing and close architecture records

**Files:**

- `rust/core/src/media.rs`
- `rust/core/src/runtime/prepare.rs`
- `rust/product/src/media.rs`
- `rust/media-support/src/lib.rs`
- `rust/media-support/src/process.rs`
- `WISHLIST.md`
- relevant ADR(s) under `doc/architecture/`

Delete superseded whole-vector file/speech preparation paths and duplicate conversion code. Update Rustdoc and architecture records to describe the implemented stream ownership and failure semantics; resolve the matching wishlist gap. Defer user-facing docs until the PR is prepared.

**TDD/verification:** Run source-specific tests for each touched crate after its changes. Then run formatting, lint, and static analysis once; run the full release quality gate in the project container after implementation is complete. Verify production line and branch coverage requirements on Debian 13 amd64 and execute the supported native platform matrix as specified in `AGENTS.md`.

## Execution order and commits

Complete tasks 1–6 in order because each layer defines the contract consumed by the next. Commit each coherent task after its focused tests and Rustdoc checks pass. Do not include unrelated working-tree changes. Run the complete quality gate only after focused defects and source checks are clean; user-facing documentation is updated immediately before requesting a PR.
