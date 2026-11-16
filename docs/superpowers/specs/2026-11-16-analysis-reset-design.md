# Explicit standalone speech-analysis reset

Status: bounded design checkpoint, 2026-11-16. No implementation or real-time qualification is claimed.

## Purpose and scope

The owner requires packet-loss concealment not to become a programme gap. Genuine speech after a mapped loss must nevertheless start a fresh analysis sequence, without stitching old neural/filter frames into new observations. This supplements the 2026-11-16 phone PLC/courtesy design only where its original assumption that existing provider operations suffice was disproved.

Scope is the existing explicit standalone `speech_analysis_reset(handle)` operation and its dependency. Do not change implicit source-jump behavior, local radio-session carrier handling, prepared-update/replacement behavior, programme RNNoise bypass, programme ring, existing speech hold/admission, or courtesy policy. No node, RF, releases, publication, codec expansion, substitute detector, replacement pool, or new worker is authorized here.

## Source-backed deficiency

At radio `2eec2cd2402f002b090a21298190c5827ea9454b`:
- `src/speech_analysis.rs` owns only a `SpeechVad` and frame bound. Its reset forwards to `SpeechVad::reset`.
- `src/vad.rs:327` deliberately rejects full reset whenever a frontend exists, closes internal classification and leaves it retired. Repeated genuine input cannot recover it.
- `Active::rearm` resets the analysis SRC and detector, then predicts three synthetic zero frames. This routine is not an acceptable genuine-only loss reset.
- `src/analysis_frontend.rs` owns the upconverter, FIR history, RNNoise handle, 480-sample frame buffers, source-origin/counts and tagged map.
- `src/samplerate_client.rs` already exposes allocation-free reset through samplerate ABI2.

RNNoise adapter `5968c123598a00169c8ccf09141bd36743ce267c` has ABI1, SONAME1 and package version `0.1.0-alpha.2`. Its `src/lib.rs::AdapterDescriptor` and public header expose only create/process/destroy. Its `src/ffi.rs` owns the only upstream bindings. Full in-place reset cannot be implemented by radio through that contract.

The parent's exact-cohort regression observed 80 genuine zero samples at source 1000, reset, then 160 genuine zero samples at source 1240: plain analysis succeeds; configured preprocessing returns nonzero reset and subsequent Translation, with zero callback allocations. That reproduces retirement, not recovered speech recognition.

## Dependency feasibility gate

The adapter quality recipe pins RNNoise 0.2 to SHA256
`90fce4b00b9ff24c08dbfe31b82ffd43bae383d85c5535676d28b0a2b11c0d37`.
Its packaged symbol list already includes `rnnoise_init`.

Candidate implementation is `rnnoise_init(existing_state, NULL)`. The upstream v0.2 `src/denoise.c:266-289` clears state, restores the built-in model and selects the architecture. The non-NULL custom-model branch parses weights and allocates; it is excluded because this adapter only creates the default model.

Before implementation is accepted, inspect the checksum-verified release archive's generated `init_rnnoise`, its layer initialization helpers, and architecture selection with the actual Debian amd64/arm64 build flags. Confirm they do not allocate, free, lock, perform I/O, log or lazily prepare resources. Verify the real shipped DSO under allocation/free instrumentation, including the first process after reset. Generic upstream API documentation is not a real-time guarantee. Stop and report if this gate fails; do not copy opaque state, patch upstream, add a prepared pool or recreate handles on audio.

Primary reference: https://raw.githubusercontent.com/xiph/rnnoise/v0.2/src/denoise.c

## Contract and compatibility

Append `reset(denoiser) -> result` after destroy in the RNNoise ABI1 descriptor. Preserve prefix layout, capability name, result values, SONAME1 and current create/process/destroy semantics. Define `RPTADV_RNNOISE_ADAPTER_DESCRIPTOR_RESET_MIN_SIZE` through the appended reset field; retain the existing prefix-size macro for actively used non-reset consumers. This is an extension, not a compatibility-only alternate implementation.

Reset is serialized with processing, bounded, allocation/free/lock/I/O/log-free, and preserves handle/model/format ownership. It clears all history and scratch; it processes no PCM and returns no VAD observation. Null returns INVALID_ARGUMENT. Upstream initialization failure returns RNNOISE_ERROR; callers must not process until a successful reset or off-audio replacement. Ensure this failure contract is enforceable and tested, not just a comment.

Radio validates descriptor size before reading reset and requires a nonnull reset operation when RNNoise is selected. Old/short RNNoise providers are rejected during preparation of that selected chain; no fallback to plain analysis or direct upstream binding.

Use radio's existing capability word for
`RPTADV_RADIO_SPEECH_ANALYSIS_RESET_CAPABILITY = 4`.
The bit promises the explicit standalone reset described here for every accepted configuration, including plain analysis. No radio descriptor slot or SONAME changes. Old radio providers lack this bit; the new phone composition rejects them on control before processing starts. Existing unrelated consumers do not need to call reset or reinterpret a carrier edge.

The package floors must identify the actual first qualified adapter/radio packages implementing these contracts. Do not invent a released version or claim alpha2/alpha11 suffice. Local candidates may have isolated candidate versions/manifests for tests, never published-version impersonation. Synchronize Cargo/Makefile/Debian/header/package metadata when the implementation receives its version, without releasing it.

## In-place state transition

Add a dedicated `SpeechVad::reset_analysis_history() -> Result<(), i32>` used only by standalone `speech_analysis::reset`. Do not repurpose `SpeechVad::reset` or `rearm_admission`, which serve radio sessions and implicit jumps.

Before fallible work, invalidate the standalone classifier decision and expected source position; retain a failure/needs-rearm state until every stage succeeds. Internal classifier score, positive streak and last voice must clear; this is not the external programme admission owner.

A dedicated Active reset:
1. Reset optional frontend upconverter with its existing ABI2 operation.
2. Zero FIR history, retaining prepared coefficients and storage.
3. Reset RNNoise through the new adapter operation.
4. Clear frontend frame/filtered/output arrays, used/produced/accepted counts and source_origin; clear positions and invalidate all map tags. Retain the known processing delay.
5. Reset downstream analysis converter, detector, 256-sample frame/used, origin, analysis_origin and reusable scratch contents.
6. Clear failure state only after all operations succeed.

No reset-time calls to RNNoise process or Earshot predict; initial setup warmup remains setup-only. The next genuine span establishes its actual source coordinate. Coordinates do not freeze or compress during missing source time. Source-map startup latency stays unqualified. No old mapped tail may enter Earshot.

Samplerate ABI2 currently scrubs its own prepared buffers with discarded zero conversions and retains fractional phase. That private history clearing is permitted: none of those zeros or counters may reach detector input or become caller source evidence. Do not demand bit-identical output to a newly allocated SRC with a different phase.

## Integration and owner boundaries

The actual phone analyzer loader is controller `rust/product/src/link/phone_speech.rs`, not the SDK. It already owns a serial reset callback. Require the new radio capability there and preserve its existing nonzero-to-error handling.

SDK `rust/radio/src/lib.rs` supplies radio configuration, and SDK `rust/product/src/tests/provider_support.rs` contains radio-descriptor fixtures. Qualify its current local-radio behavior against the new DSO, but do not add a second phone analyzer wrapper or teach programme RNNoise bypass to reset. If no SDK production change is needed, record that negative result; a rebuild/test cohort can still be required.

Controller loss handling calls explicit reset once for the relevant fresh analysis sequence, before successor genuine PCM. PLC never reaches speech/DTMF. A successful analysis reset must not invoke the programme gap/reset path or clear ring retention, hold end, immutable birth, source epoch or current admission. Reset failure remains a genuine provider failure and uses existing fail-closed handling, never claims continuity success.

## Verification

Required staged evidence:
- Adapter fake bindings prove exact init/default-model selection, preserved handle, zero process/create/destroy calls during reset, scratch clearing, null and init-failure handling, repeated reset and retry.
- Real RNNoise dirty-history reset yields the same subsequent samples/probabilities as a same-baseline reset reference, with genuine nonzero input; no reset-time silent processing.
- C ABI smoke covers both the active prefix contract and extended reset contract, installed header/DSO/pkg-config checks, dynamic RNNoise linkage and no static adapter.
- Radio exact 1000/80/reset/1240/160 regression; preprocessing disabled, bandpass-only, RNNoise-only and combined; 8/16/48 kHz plus a fractional-phase source rate.
- Partial 480/256 frames, nonzero origin, repeated loss, old speech followed by real silence, then real speech that requalifies with successor-only timestamps.
- Test instrumentation proves no reset-time detector predictions; reset plus first process passes real Rust/C allocation and free guards.
- Failure injection covers each reset stage, no output after failure, successful explicit retry, and no callback destruction.
- Existing implicit-jump retirement, local carrier-reopen, prepared-replacement and programme RNNoise bypass tests remain unchanged in behavior.
- Exact controller/SDK/radio/adapter cohort verifies programme ring/hold/age/admission preserved, synthetic-only input cannot key or renew hold, and later genuine speech can reopen.

Before merge: native Debian 13 amd64/arm64 build/test/package/staged-install gates and 100% production line/branch coverage on amd64, plus platform-independent checks once. Hardware listening/RF acceptance is separate and unperformed. This document does not authorize those deployments.

## Evidence limits and handoff

Source audit supports the design, not its native real-time proof. No generated-model archive audit or native reset instrumentation has yet passed. Current all-zero repro cannot prove positive speech recovery, and Rust-only allocation counts do not prove C-side real-time safety.

Proceed serially: dependency feasibility and RNNoise extension, independent review; standalone radio extension, independent review; SDK compatibility/cohort check and controller activation, independent review; whole-cohort verification. Make local checkpoint commits only. No calendar delivery promise is attached.
