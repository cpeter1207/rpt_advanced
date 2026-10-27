# Codec Adapters and IAX Media Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the embedded mu-law path with swappable protocol-neutral providers and prove the 16 approved IAX audio profiles.

**Architecture:** A new `rptadv-codec-adapters` workspace publishes a common C contract and independent backend DSOs. The IAX library maps its wire IDs to neutral profiles and owns one encoder/decoder per peer; product egress and both hosts exchange explicit packet metadata. Existing jitter and PCM-ring adapters retain their separate responsibilities.

**Tech Stack:** Rust 2024, C-compatible ABI 1 codec descriptors, released codec libraries, existing Tokio/jitter/ring/conversion adapters, native Debian 13.

**Spec:** [Approved design](../specs/2026-10-27-protocol-neutral-codecs-and-text-media-design.md); [integration and execution order](2026-10-27-protocol-neutral-codecs-and-text-media.md).

## Global Constraints

- Inherit every global constraint and artifact revision from the integration plan. No deployment or release is authorized.
- Use GPL-2.0-only-compatible released implementations. No complex original codec, embedded Asterisk runtime, new resampler or custom codec PLC. Preserve all backend-native concealment; prefer it over shared-ring fallback.
- Native PCM is 48 kHz mono normalized `f32`; codec PCM uses 8, 16 or 48 kHz as specified. Keep mu-law first and retain existing host codec policy.
- G.723 encode 6300/decode 5300 and 6300 if the reviewed FFmpeg backend is selected; do not claim 5300 encoding. LPC10 uses 180 samples/22.5 ms, not 20 ms.
- Packet-jitter policy stays 160/180/500 ms. Egress expiry remains 200 ms, with one pending encoded packet and no retry re-encoding.
- Compiler/Rustdoc/header warnings are errors. Focused TDD precedes implementation; only production code requires 100% line/branch coverage.

## Review Focus

1. Codec labels can hide incompatible packing/byte order: independent fixtures in C3/C4.
2. Fractional durations and converter partitions can drift timestamps: C7's 200-frame LPC10 test.
3. Compressed bytes cannot be trimmed by a PCM overlap count: C6's overlap test.
4. A legal large or zero-duration datagram must not become an unexplained hangup: C6's boundary matrix.
5. A missing/wrong provider or incompatible receive mode must fail safely without lying in offers: C2/C5/C8 tests.

---

All paths beginning `codecs/` below mean the planned sibling
`rptadv-codec-adapters/`; `iax/` means `librptadviax2/`; `product/` means
`rpt_advanced/`. No remote repository is created while writing this plan.

### Task C1: Freeze usable sources and establish the provider test/package baseline

**Files:** Create `codecs/AGENTS.md`, `Cargo.toml`, `Makefile`, `LICENSE`, `dependencies/codecs.json`, `tests/test_sources.py`, `tests/fixtures/manifest.json`, `debian/control`, thin `.github/workflows/quality.yml`; modify shared-library workflow companion `.github/workflows/quality.yml`, `.github/workflows/lint.yml`, `.github/workflows/release.yml`; add `containers/codecs.Dockerfile` and `.github/workflows/images.yml` there. Use existing make/packaging/container-launch patterns, not a new runner.

**Interfaces:** Produces `dependencies/codecs.json` entries `{backend, release, sha256, license, build_flags, profiles, encode_modes, decode_modes, fixture_sources}` and a fixture manifest `{profile, direction, source_revision, path, sha256, samples, encoded_bytes}`. `make sources-check` runs `python3 -m unittest discover -s tests -p test_sources.py`. `make test PROFILE=<neutral-name>` selects one profile's shared conformance harness; later tasks fill its provider/fixtures.

- [ ] **Red:** Add `test_sources_are_released_compatible_and_complete`:

  ```python
  assert active_format_count() == 16
  assert deferred_formats() == {"siren7", "siren14", "g719"}
  assert all(source.has_release_hash_and_license for source in selected_sources())
  assert profile("g723").encode_modes == [6300]
  assert profile("g723").decode_modes == [5300, 6300]
  assert all(fixture.independent_of_provider for fixture in fixtures())
  ```

  Helpers are test-local readers of the two manifests. Initially fail on missing/unfinished evidence.
- [ ] **Run red:** `make sources-check`; expected failure for missing audited sources/fixtures, not a fabricated all-compatible list.
- [ ] **Implement the evidence and build inputs:** Audit exact releases, transitive licenses, enabled binary features and the pinned Asterisk/ASL sources in spec section 12. Select existing `audio-codec-algorithms 0.7` for G.711; system libgsm/libspeex/libopus; reviewed FFmpeg avcodec+avutil for G.723/G.726/G.722; bcg729 1.0.4; reviewed libilbc release; released LPC10 and Asterisk Dialogic primitive sources. Keep any required standalone external-library packaging recipes in `packaging/external/<backend>/`, downloading hash-verified release source at package build rather than vendoring it into IAX/product. Isolate primitives from host wrappers; never copy `app_rpt`. If a candidate cannot satisfy licensing or its wire profile, stop that profile as pending and document the exact gap—do not implement a complex replacement or mark this plan complete.
- [ ] **Green:** `make sources-check`. Add required native development/runtime packages to a prebuilt `ghcr.io/cpeter1207/rptadv-codecs-quality-debian13:latest` image through the companion. Build/verify amd64/arm64 manifests and make the provider gate consume freshly pulled images; do not inherit the shared workflow's current source-local Docker build as a purported prebuilt gate. Test packaging from a clean source archive. Inherit Rust 1.85 minimum from product; if a released dependency cannot meet it, report before raising the floor.
- [ ] **Commit:** Only the listed baseline/evidence/package/image files, in their owning repositories: `build: establish codec provider sources and native validation`. Source approval is a prerequisite, not an invitation to implement deferred formats.

### Task C2: Deliver the neutral descriptor and G.711 provider without changing mu-law behavior

**Files:** Create `codecs/include/rptadv_codec.h`, `rust/contract/{Cargo.toml,src/lib.rs}`, `rust/g711/{Cargo.toml,src/lib.rs,src/tests.rs}`, `tests/abi_header_smoke.c`, `tests/conformance.rs`, `tests/provider_fixture.rs`; extend provider Makefile/package inputs. Modify IAX `src/codec.rs`, `Cargo.toml` only when the adapter loader is ready; retain the active mu-law path until C8's coordinated cutover.

**Interfaces:** `rptadv_codec_descriptor_v1()` returns a sized ABI-1 table, capability `rptadv.codec.v1`. The header owns fixed-width profile records and opaque encoder/decoder handles; no IAX IDs or external types. Neutral profile names are `g711-mu`, `g711-a`, `gsm-fr`, `g723`, `g726-aal2-32`, `dialogic-adpcm`, `pcm16-be-8k`, `lpc10`, `g729a`, `speex-nb`, `ilbc`, `g726-rfc-32`, `g722-64`, `pcm16-le-16k`, `speex-wb`, `opus-mono`. Assign stable header IDs 1–16 in that order, distinct from IAX bits.

The table supplies `profile_count`, `profile_at`, `open_encoder`, `open_decoder`, `inspect`, `encode`, `decode`, `reset`, `destroy`. All records start with `struct_size: u32`; the descriptor also has `abi_version: u32` and a 16-byte capability. Use these callback shapes in `include/rptadv_codec.h` (all pointers are borrowed for the call; `void *` contexts are provider-owned):

```c
size_t profile_count(void);
int32_t profile_at(size_t index, rptadv_codec_profile_v1 *out);
int32_t open_encoder(const rptadv_codec_config_v1 *config, void **out);
int32_t open_decoder(const rptadv_codec_config_v1 *config, void **out);
int32_t inspect(void *decoder, const uint8_t *packet, size_t bytes,
                rptadv_codec_packet_info_v1 *out);
int32_t encode(void *encoder, const float *pcm, size_t frames,
               uint8_t *out, size_t capacity, uint32_t flush,
               rptadv_codec_encode_result_v1 *result);
int32_t decode(void *decoder, const uint8_t *packet, size_t bytes,
               float *out, size_t capacity, size_t *frames);
int32_t reset(void *context);
void destroy(void *context);
```

`rptadv_codec_config_v1` contains neutral `profile_id`, selected `mode_id`,
`packet_frames` (all `u32`) and caller-derived maximum packet bytes (`size_t`).
`rptadv_codec_profile_v1` reports ID, source rate, channels, direction flags,
lookahead and maximum legal frame samples/bytes, plus a counted borrowed mode
table; each mode has ID, bitrate, frame samples and frame bytes (`u32`; variable
encoded lengths use zero frame bytes and the reported maximum). The profile
table is immutable for the DSO's lifetime. Packet inspection and encode-result
records carry the fields defined below, plus `required_capacity: size_t` on
capacity errors. Rust owners expose:

```rust
fn inspect(&self, packet: &[u8]) -> Result<PacketInfo, CodecError>;
fn encode(&mut self, pcm: &[f32], out: &mut [u8], flush: bool)
    -> Result<EncodeResult, CodecError>;
fn decode(&mut self, packet: &[u8], out: &mut [f32])
    -> Result<usize, CodecError>;
```

`PacketInfo { frames: u32, mode: u32 }` counts source PCM frames.
`Profile` records PCM rate/channels, legal mode/frame units, maximum encoded/
decoded counts and lookahead. `EncodeResult` records `status`, `consumed_frames:
usize`, `encoded_bytes: usize`, `source_start_frames: u64`, `source_frames:
u32`, `packet_frames: u32`. Source positions are relative to encoder reset;
`packet_frames` includes required final frame padding, while `source_frames`
excludes it. `CodecStatus` is `Complete=0`, `NeedInput=1`, `OutputFull=2`,
`Malformed=3`, `Unsupported=4` or `Failed=5` (`repr(u32)`); `CodecError` contains
`status: CodecStatus` and `required_capacity: usize`. At most one
packet is produced per call. Flush with empty real input does not invent voice.

- [ ] **Red:** Add `contract_g711_counts_and_isolation` and `contract_rejects_bad_tables`: `assert_eq!(encode_mu(&[0.0;160]).encoded_bytes,160); assert_eq!(inspect_mu(&[0xff;160]).frames,160); assert_eq!(encoder_a_after_encoder_b(), encoder_a_alone()); assert!(short_table().is_err()); assert!(wrong_capability().is_err()); assert!(undersized_output_preserves_context());`. Compare independent mu-law/A-law vectors, not just round trips. Include null/size/overflow errors in the shared C ABI fixture, not duplicated backend tests.
- [ ] **Run red:** `cargo test --locked --workspace contract_`; `make abi-check`. Expected missing contract/provider failures.
- [ ] **Implement:** Own codec state and buffer assembly in each context, validate all arithmetic and capacities before state mutation, and wrap the existing released G.711 backend. Build `librptadv_codec_g711.so.1` with runtime package `librptadv-codec-g7111`; `librptadv-codec-dev` installs the sole common header/pkg-config contract. Contract crate contains layout/types only, not a hidden codec implementation or second DSO. Add a neutral, non-audio IAX loader `CodecCatalog::load(providers: &[ProviderSelection]) -> Result<Self, CodecError>` in `iax/src/codec.rs`, where `ProviderSelection { path: PathBuf, profiles: Vec<ProfileId> }`. It retains validated libraries until all contexts stop; do not switch calls yet.
- [ ] **Green:** Repeat focused tests plus `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked`. Verify staged DSO loading and mu-law output equality to the current released backend; no missing-provider silent fallback.
- [ ] **Commit:** Listed contract/G.711/loader files only: `feat: add versioned protocol-neutral G711 providers`.

### Task C3: Add PCM, GSM, Speex and Opus providers through one conformance harness

**Files:** Create backend crates `codecs/rust/{pcm,gsm,speex,opus}/Cargo.toml` and each `src/{lib.rs,tests.rs}`; extend `tests/conformance.rs`, audited fixtures and per-provider Debian install files. No IAX mapping in these crates.

**Interfaces:** Implements C2's unchanged table for PCM16 BE 8 kHz, PCM16 LE 16 kHz, GSM FR, Speex NB/WB and Opus mono. Artifacts `librptadv_codec_<backend>.so.1`, runtime packages `librptadv-codec-<backend>1`, common development header. Opus opens 48 kHz mono/20 ms encoding with optional encoder DTX disabled; packet inspection uses libopus's duration API. GSM uses 160 frames/33 bytes, not WAV49. PCM is trivial boundary serialization, not a new DSP stage.

- [ ] **Red:** Extend parameterized `profile_vectors` / `profile_state_and_bounds` using independent fixtures. Assertions include `assert_eq!(pcm_be([0x12,0x34]), sample_0x1234()); assert_eq!(pcm_le([0x34,0x12]), sample_0x1234()); assert_eq!(gsm_160().bytes,33); assert_eq!(opus_60ms().frames,2880); assert!(odd_pcm_bytes().is_err()); assert!(gsm_wav49().is_err());`. For every profile test directions, exact duration, independent contexts, reset, bounded capacity, malformed input and finite flush. Compressed lossy PCM compares against independent decoder output with a recorded tolerance, not bit-exact input PCM.
- [ ] **Run red:** `cargo test --locked --workspace profile_`; expected new profiles absent.
- [ ] **Implement:** Only backend DSOs call their external C libraries. S16 conversion happens at that backend's API boundary, preserving normalized `f32`. Use released duration/validation APIs where available; keep codec-native PLC enabled and expose its bounded lost-span operation. Preserve valid SID/silence semantics without creating wire key activity from empty output. Unsupported loss durations fall back to ring PLC without concealing native output twice.
- [ ] **Green:** Repeat the profile tests, then `make abi-check install-check`. Inspect actual DT_NEEDED entries and dependency license inputs. Confirm replacing one backend fixture does not change another profile/context.
- [ ] **Commit:** Each independently passing backend with its fixtures/docs/package inputs: `feat: provide <backend> codec profiles` (substitute the actual backend in the message).

### Task C4: Add framed/legacy codec providers with exact ASL profiles

**Files:** Create `codecs/rust/{ffmpeg,bcg729,ilbc,lpc10,dialogic}/Cargo.toml` and each `src/{lib.rs,tests.rs}`; extend the same conformance harness, `dependencies/codecs.json`, fixtures, external-library packaging and provider package install files.

**Interfaces:** C2's descriptor unchanged. FFmpeg provider owns avcodec+avutil for G.723/G.726/G.722, separate from the existing graph adapter's ABI. Other artifacts are `librptadv_codec_<backend>.so.1` / `librptadv-codec-<backend>1`. Their selected external primitive libraries are dynamic dependencies, not copies in the product or IAX library.

- [ ] **Red:** Extend `profile_vectors` with `assert_ne!(g726_rfc_payload(),g726_aal2_payload()); assert_eq!(lpc10_frame(),(180,7)); assert_eq!(ilbc20_frame(),(160,38)); assert_eq!(ilbc30_frame(),(240,50)); assert_eq!(g729_frame(),(80,10)); assert_eq!(g722_pcm_rate(),16000); assert!(g723_encode_5300().is_unsupported());`. Compare Dialogic state/framing, G.726 nibble order and codec SID handling to pinned independent peer fixtures. Assert ambiguous/unsupported mode cannot mutate a decoder or become a hangup.
- [ ] **Run red:** `cargo test --locked --workspace profile_`; expected missing profile/mode failures.
- [ ] **Implement:** Use only C1-audited releases. Keep 30 ms G.723, 22.5 ms LPC10 and both iLBC modes explicit in profile records; derive limits from codec units. Inspect Asterisk/HamVoIP mode behavior and record its fixture evidence; do not invent negotiation bits or guess iLBC mode from arbitrary aggregate length. Resolve the configured/known profile first, validate whole units, and reject/log ambiguous packets without destroying the connection. If compatibility cannot be established, leave that mode pending and stop completion, not the working mu-law baseline.
- [ ] **Green:** Run affected profile filters and ABI/package checks. All 16 format profiles must have independently verified decode and encode behavior within declared mode limits before the final completion claim. A self-round-trip or successful library initialization is insufficient.
- [ ] **Commit:** One backend at a time with exact source/license evidence: `feat: provide <backend> ASL-compatible media profiles`.

### Task C5: Negotiate verified 64-bit formats for incoming and outgoing IAX calls

**Files:** Modify `iax/src/{codec.rs,information_elements.rs,media.rs,session.rs,client.rs,server.rs,ffi.rs,async_ffi.rs}`, `include/rptadv_iax2_client.h`, `src/{session_unit_tests.rs,session_setup_tests.rs,client_unit_tests.rs,server_tests.rs}`; create `src/formats.rs`, `src/formats/tests.rs`. Keep the existing subclass compressor in `src/protocol.rs`.

**Interfaces:** `IaxFormat(u64)` and `FormatCatalog` map protocol bits to C2 profile IDs. `FormatCatalog::select(local: &[ProfileId], offered: u64) -> Result<ProfileId, NegotiationError>` accepts only validated bidirectional profiles. Client2 adds `open_catalog(const rptadv_iax2_codec_selection_v2 *selections, size_t count, void **catalog)` and `close_catalog(void *catalog)`. Each sized selection contains `provider_path: const char *` and neutral `profile_id: uint32_t`; the open call copies borrowed strings and calls C2's loader. Client2 `dial` and server2 `open` receive the same borrowed opaque catalog in their sized options. Standalone builds it from the selected provider manifest and retains it through all peer/server destruction. Other protocols can directly load the common provider descriptor without using this IAX-specific catalog lifecycle. Async ABI1 remains unchanged.

- [ ] **Red:** Add `formats_extended_precedence` / `formats_both_call_directions`:

  ```rust
  assert_eq!(select(peer_offers_ulaw_and_opus()), ProfileId::G711Mu);
  assert_eq!(extended_opus_offer().selected_bit(), 1u64 << 34);
  assert_eq!(legacy_ulaw_offer().selected_bit(), 4);
  assert_eq!(incoming_catalog(), outgoing_catalog());
  assert_eq!(advertised_mask() & deferred_mask(), 0);
  assert!(selected_manifest_missing_provider().is_err());
  ```

  `ProfileId::G711Mu` is C2's ID 1 Rust wrapper. Test exact Asterisk legacy/extended IE precedence/version bytes, high-bit subclass serialization, incompatible offers and no mid-call format substitution.
- [ ] **Run red:** `cargo test --locked -p rptadviax2 formats_`; expected u32/hardcoded-offer failures.
- [ ] **Implement:** Widen all offered/selected state and media subclasses end-to-end, map only at the IAX boundary and open per-direction contexts once selection is committed. Retain mu-law first; otherwise honor existing host policy. Remove duplicate mapping constants. A selected missing provider fails preparation; a deliberate reduced test composition advertises only its real profiles.
- [ ] **Green:** Repeat format/session setup tests and ABI smoke with client2/server2. Reject ABI1 options before dereference, keep async ABI1 unchanged and verify inbound admission/authentication regressions.
- [ ] **Commit:** Scoped protocol/header/tests/developer docs: `feat: negotiate provider-backed 64-bit IAX media`.

### Task C6: Make ingress/jitter duration codec-aware and preserve ordered gaps

**Files:** Modify `iax/src/{playout.rs,jitter.rs,ingress.rs,media.rs,async_driver.rs}`, their existing `*_tests.rs`/`src/media/tests.rs`, and `src/jitter_fixture.rs`; extend `tests/ulaw_media.rs` into shared multi-profile fixtures without duplicating its assertions.

**Interfaces:** `PacketInfo.frames` drives packet span; decoded PCM uses the selected profile's rate. `Playout` retains one decoder per peer and emits ordered PCM/missing source-frame spans through the existing poll contract. Packet retrieval buffers derive from profile bounds and the full legal UDP payload, not 1500 bytes; network receive detects truncation rather than parsing a partial datagram.

- [ ] **Red:** Add `playout_compressed_overlap_and_gap` / `ingress_datagram_bounds`:

  ```rust
  assert_eq!(trim_after_decode(gsm_packet(),80).pcm_frames,80);
  assert_eq!(jitter_limits(16000),(2560,2880,8000));
  assert_eq!(jitter_limits(48000),(7680,8640,24000));
  assert_eq!(reordered_audio_and_gap().events, expected_order());
  assert!(!valid_empty_voice().hangs_up);
  assert!(!late_full_frame().hangs_up);
  assert!(late_full_frame().acknowledged);
  ```

  Include full legal datagram boundaries, truncated datagram detection, malformed compressed units, supported aggregation, sample-count overflow, decoder failure and output-capacity retry. Verify empty/SID/unknown media preserve the established cause/error policy rather than generic teardown. Missing spans reach ring PLC once, between the correct real samples.
- [ ] **Run red:** `cargo test --locked -p rptadviax2 playout_`; `cargo test --locked -p rptadviax2 ingress_`; expected fixed-8k/byte-trimming failures.
- [ ] **Implement:** Inspect valid packets before jitter admission, use checked source-clock timestamp/sample arithmetic, decode complete compressed packets, then trim PCM overlaps. Preserve the accepted 160/180/500 ms policy at each source rate. Bound work per peer-owner turn using existing mailboxes; do not let a large packet or decoder affect another peer. Retain ACK/late-drop/empty-voice and buffered-tail fixes. Do not add a second concealment stage.
- [ ] **Green:** Repeat the focused matrix, existing ordered-gap/jitter-ring integration and tail/late-packet regression cases. Verify native PLC supplies supported missing spans and the ring handles only the remainder, never double concealment. Confirm explicit packet/sample diagnostics and first-cause logging without payloads or credentials.
- [ ] **Commit:** `fix: preserve codec sample timing through IAX ingress`.

### Task C7: Assemble stateful egress packets and retain timing under backpressure

**Files:** Modify `iax/src/{codec.rs,client.rs,ffi.rs}`, `src/{client_tests.rs,client_unit_tests.rs,ffi_tests.rs}`, `include/rptadv_iax2_client.h`; product integration follows in C8.

**Interfaces:** Client2 exposes `prepare_audio(peer, pcm, pcm_count, encoded, encoded_capacity, flush, result)` returning C2's sized encode result, and `send_prepared_audio(peer, encoded, encoded_bytes, packet_frames, timestamp_ms)`. `reset_encoder(peer)` explicitly abandons expired/discontinuous media state. C2's encoder owns accumulation; callers never treat consumed frames as byte count. Unsupported/no-output/backpressure statuses are distinct.

- [ ] **Red:** Add `egress_codec_packet_timing`:

  ```rust
  assert_eq!(encode_partitioned(&[37,123,80]),encode_whole(240));
  assert_eq!(lpc10_200_frames().elapsed_ms,4500);
  assert_eq!(blocked_twice_then_sent().encode_calls,1);
  assert_eq!(blocked_twice_then_sent().original_birth,first_birth());
  assert_eq!(no_complete_codec_unit().voice_packets,0);
  assert_eq!(finite_flush().real_source_frames,input_real_frames());
  ```

  Include converter zero-output, partial capacity, SID/no-output, tail padding/lookahead, expiry while buffered and unkey flush. Encoder reset discards old burst state and timestamps do not jump by encoded byte count.
- [ ] **Run red:** `cargo test --locked -p rptadviax2 egress_`; expected missing metadata/stateful assembly failures.
- [ ] **Implement:** Track total source frames since reset, emit only complete legal codec units and expose real source start/span separately from wire duration. Use accumulated rational sample time, converting to IAX milliseconds at emission; never repeatedly round each LPC10 frame. Preserve original age of buffered samples and retry exactly one prepared packet without re-encoding. Reset state on deliberate expiry/discontinuity; do not roll expired PCM into the next burst.
- [ ] **Green:** Repeat focused egress/FFI tests and mu-law timing/expiry/tail regressions. Assert one blocked peer does not stall another owner's encode/send, using the existing independent-peer harness rather than a new thread model.
- [ ] **Commit:** `feat: prepare stateful codec packets with explicit media spans`.

### Task C8: Cut both product hosts over to the shared prepared-media contract

**Files:** Modify product `rust/product/include/rptadv_product.h`, `rust/product/src/{services.rs,services_tests.rs}`, `rust/product/src/link/{outbound.rs,egress.rs,session.rs}`, existing link session tests, `rust/standalone/src/{iax.rs,host_services.rs}`, `rust/asterisk/src/{services.rs,codec.rs}`, loader/manifest/build files, IAX Makefile/build/package version files and `tests/abi_v1.rs` (replace with `tests/abi_v2.rs`). Add `rust/product/src/link/outbound_codec_tests.rs`; update header-generated bindings, never handwritten tables.

**Interfaces:** Host8 replaces `peer_prepare_audio` with peer-scoped C7 preparation and explicit result, adds encoder reset/flush, and gives `peer_send_prepared` encoded-byte count, source packet-frame count and timestamp separately. `PeerIo::prepare_audio(&mut self, pcm: &[f32], encoded: &mut [u8], flush: bool) -> Result<EncodeResult, Error>` and `PeerIo::send_prepared(&mut self, encoded: &[u8], packet_frames: u32, timestamp_ms: u32) -> Result<bool, Error>` are the product-side ports. Standalone delegates to client2. Asterisk retains its existing PCM-only `peer_write` path and public Asterisk codec services; its optional prepared-media callbacks remain `None`. Update its host8 layout/validation without inventing an extra encoder or changing its pacing. Product ABI6 accepts only host8.

- [ ] **Red:** Add `outbound_codec_metadata` / `host_codec_composition`: `assert_eq!(gsm_160().pending_bytes,33); assert_eq!(gsm_160().pending_frames,160); assert_eq!(need_input().pending_packets,0); assert_eq!(asterisk_pcm_host().represented_duration,expected_duration()); assert!(!asterisk_pcm_host().prepared_media); assert!(old_host_table().is_err()); assert!(incoming_and_outgoing_calls_share_catalog());`. Test source-offset/lookahead/age mapping across arbitrary native partitions and the Asterisk path without requiring its private thread model in standalone.
- [ ] **Run red:** `cargo test --locked -p rptadv-product outbound_codec_`; `cargo test --locked -p rpt-advanced-standalone host_codec_`; `make product-abi-check`. Expected old metadata/host failures.
- [ ] **Implement:** Carry C7 metadata through host8 and `Outbound::Pending`; keep converter-output and first-buffered-source age instead of `audio.len()` bytes. Preallocate scratch from profile/transport limits off audio. Advance source-clock anchors from real sample spans, retain one prepared packet, and preserve tail drain/expiry. Validate retained catalogs on ordinary reload; load providers only at composition start. Switch client/server SONAME/package2 and product6/host8 together. Delete `G711Ulaw`/`prepare_ulaw` and all obsolete hardcoded-ULAW-only production paths after parity tests pass; remove the IAX leaf codec dependency. No compatibility shim for retired alpha tables.
- [ ] **Green:** Repeat targeted host/outbound tests; run `make product-abi-check`, IAX ABI2 smoke and existing package/lifecycle/Asterisk integration filters. Real independent peer fixtures must demonstrate encode and decode for every declared profile, both call directions and fallback to mu-law. Proceed to integration I1/I2; do not call the feature complete before those gates.
- [ ] **Commit:** Exact ABI/consumer/package/test files in their owning repositories: `feat: use codec-neutral prepared media in both controller hosts`.
