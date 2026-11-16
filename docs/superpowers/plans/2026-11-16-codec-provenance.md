# Codec-unit Provenance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve native PCM and truthful per-unit recovery classification through the existing SIP receive/retry path.

**Accepted authority boundary:** A valid subsequent PacketDecoded unit may qualify speech/DTMF despite ordinary prior-PLC decoder/filter history. Directly recovered units and explicit PLC are non-authoritative. No mathematical sample-purity claim and no invented washout interval.

**Architecture:** Read the pinned iLBC unit outcome once after its existing native decode, append a neutral span-producing codec operation, then consume that result in the existing controller owner. Preserve active old operations/prefix only; no historical compatibility framework or new decoder/worker.

**Tech Stack:** Codec ABI1/SONAME1, Rust provider/client/controller, provider-private C accessor, pinned libilbc 2.0.2, native Debian 13 amd64/arm64.

**Spec:** `docs/superpowers/specs/2026-11-16-codec-provenance-design.md`

## Global Constraints

- Implementation is separately dispatched; this plan does not authorize publication, releases, node access or RF.
- Keep current SIP codecs and 30 ms receive bound, native PLC/enhancement, programme ring/hold/age/admission and source coordinates.
- No synthetic qualifying analysis, double decoder advancement, PCM heuristics, fabricated silence, sample-purity claim or washout.
- Scope append-only codec capability with unchanged active prefix, old operations, entrypoint and SONAME1; no retired-alpha compatibility shims.
- The exact iLBC source SHA256 is `84d5c99087a35bda3463bcb993245b4f8c28c04fe037215a5ec56570f68ce519`. Verify source/header/build identities during build/package qualification. Runtime version is only a sanity check, not binary attestation; runtime installations do not need development headers.
- Follow current AGENTS.md/QUALITY.md, protect concurrent work, use freshly pulled labeled quality images and bounded focused checks.
- No codec operation is newly promised to be audio-thread safe; preserve the existing serial non-audio decoder owner. Added observation/metadata/retry work must not allocate/free.
- Commit small local increments with command-local `codex <codex@localhost>` attribution; do not stage unrelated controller work.
- Actual candidate hashes/versions and exact client revision must be recorded; old alpha floors cannot be asserted to provide the new contract.

## Review Focus

- Native failure after an earlier aggregate unit advanced must not trigger conceal/redecode: Task 1 poison/reset tests and Task 2 both-fallback regression.
- Metadata OutputFull must not advance native state: Task 1 capacity/call-count tests.
- Larger generated descriptors must not cause prefix overreads or unrelated legacy rejection: Task 1 prefix/tail matrix.
- Overlap/partial credit may split provenance spans: Task 2 retained metadata/cursor tests.
- PacketDecoded after loss is eligible without claiming clean decoder history: Task 2 explicit practical-authority transition tests.

---

### Task 1: Provider, native audit and neutral client contract

**Checkout:** `/home/codex/rpt-development/shared-components/rptadv-codec-adapters`, audited base `83725b7`; recheck current source and use an isolated coordinated branch/worktree.

**Modify:** `include/rptadv_codec.h`, `rust/contract/src/lib.rs`, `rust/provider/src/{lib,tests,boundary_tests}.rs`, `rust/client/src/{lib,tests}.rs`, `rust/client/build.rs` only if necessary for new declarations, `rust/ilbc/src/{lib,tests}.rs`, `rust/ilbc/Cargo.toml`, `rust/{g711,pcm,gsm,spandsp}/src/lib.rs`, affected fixtures in `tests/test_native_profiles.py`, `tests/test_reference.py`, installed C descriptor tests and package checks.
**Create:** `rust/ilbc/src/provenance.c` and an iLBC-specific build script which preserves the existing backend-link search behavior. Compile a tiny accessor only, never upstream codec source into the provider.
**Metadata:** existing dependency/build/package manifests and `debian/control` only as required for actual candidate identity. Keep no release claim.

**Interfaces produced:**
- C span `rptadv_codec_decode_span_v1`: three u32 fields `offset_frames`, `frame_count`, `origin`; 12-byte record, alignment 4. Origin 0 PacketDecoded, 1 NativeRecovered; other values rejected.
- Sized result `rptadv_codec_decode_result_v1`: u32 `struct_size`, u32 `reserved` (zero), then size_t `frames`, `span_count`, `required_pcm_capacity`, `required_span_capacity`.
- Appended callback after conceal: `decode_with_provenance(void *, const uint8_t *, size_t, float *, size_t, rptadv_codec_decode_span_v1 *, size_t, rptadv_codec_decode_result_v1 *) -> int32_t`.
- Named descriptor minimum through conceal for active old operations, and provenance minimum through the new callback. Existing ABI/version/capability/entrypoint unchanged.
- Client caller-owned `DecodeSpan` storage and `Decoder::decode_with_provenance(packet, pcm, spans)` returning validated frames/span count or a typed error carrying status plus both required capacities. No allocations in this method.
- Client `Decoder::require_provenance()` uses the documented empty-packet, zero-capacity call; Complete/zero confirms the selected profile/mode, Unsupported rejects it. No native processing.
- Metadata capacity bound: iLBC `packet_bytes / configured_unit_bytes` (38 or 50); other audited all-packet-decoded SIP paths one for nonempty packet; empty packet zero. Conservative capacity is checked before decode even if final output coalesces.

- [ ] Re-read codec instructions and relevant current architecture decisions. Verify the checksum release archive and installed public compatibility header; record build-header hash and separate backend package/DSO identity. Inspect actual successful/failure paths of every current SIP backend, not just Rust wrappers. Record why each supported PacketDecoded classification is valid at the unit boundary. Do not advertise unaudited profiles.
- [ ] Add failing shared ABI/client tests for short prefix, absent/short/null provenance tail, old active-prefix consumers, per-profile unsupported probe, empty supported probe, and result layout/offsets. Expected old consumers remain valid through the old operations; provenance-required consumers fail before media.
- [ ] Add failing capacity tests: PCM short and spans short each report both conservative requirements with zero native decode/conceal calls and zero successful spans. Test zero-length, wrong-size result, invalid pointer/alignment, zero/holey/overlapping/overflowing ranges, unknown origin and wrong frame count.
- [ ] Add failing real iLBC20/30 fixture tests for valid/bad, bad/valid, valid/bad/valid, all-bad and repeated recovery. Use existing invalid-start-index/final-bit mutations. Assert exact unit origins, PCM count and equality to an independent pinned native reference. Do not add SIP20 support.
- [ ] Run the affected existing Rust test and native fixture targets in the approved isolated environment; record expected missing-tail/operation failures. Use existing Makefile/QUALITY.md commands discovered in the checkout, not an invented build runner.
- [ ] Add the C accessor using `ilbc.h`'s actual `iLBC_Dec_Inst_t`. Check the native Decode count and then read 0/1 outcome immediately after each existing per-unit call. Unknown flag is provider failure. Do not inspect once after the aggregate, mutate state, decode twice, or use speechType.
- [ ] Implement the append-only contract and shared preflight. Produce valid, ordered, exhaustive spans only on Complete. Coalesce adjacent equal origins while enforcing the conservative preflight capacity. Validate every output before client publication.
- [ ] Implement current SIP profiles explicitly. Unsupported provenance remains a non-consuming operation on unaudited profiles; existing old decode/conceal remain active. Do not default every Backend implementation to PacketDecoded.
- [ ] Implement context failure handling: preflight/OutputFull leave state unchanged; unexpected native/postdecode failure after processing poisons the decoder until successful reset. No partial-success transaction, rollback, automatic reset, second decode or whole-packet conceal. Test successful reset clears poison and failed reset does not.
- [ ] Keep the existing client `decode`, `conceal` and `decode_recovering` behavior for active old consumers. The new path never invokes `decode_recovering`. Test that even deliberately Malformed native failure after unit advancement does not request conceal.
- [ ] Implement size-checked prefix/tail reads instead of copying a larger generated descriptor unconditionally. No dynamic historical-version registry or compatibility adapter.
- [ ] Run native reference/call-count and allocation/free tests, including first operation and repeated aggregates. Compare subsequent outputs with reference states to detect accidental extra advancement. Audit support probes for no state mutation.
- [ ] Update installed header/DSO/pkg-config/package checks and local candidate metadata. The iLBC provider/backend exact package relationship remains; runtime headers are not required. Runtime version sanity checks do not replace build/package attestation.
- [ ] Run focused Rust/native suites, formatting/lint/static checks required for the local checkpoint. Record full native/coverage/package checks still pending. Commit coherent provider/client increments locally.
- [ ] **Independent review gate before controller edits:** assess per-unit source evidence, all current SIP profile audit, capacities, poison/reset semantics, active-prefix compatibility and native PCM equivalence.

### Task 2: Controller integration with bounded retained provenance

**Checkout:** `/home/codex/rpt-development/phone-plc-courtesy`; coordinate with the current controller worker before touching shared files.

**Modify:** `rust/standalone/src/phone/codec.rs`, `rust/standalone/src/phone/call_media.rs`, `rust/standalone/src/phone/call_media_tests.rs`, `rust/standalone/tests/phone_codec.rs`, `rust/standalone/tests/fixtures/phone_codec.c`, `rust/standalone/Cargo.toml`, `Cargo.lock`; affected `rust/product/src/link/{phone_media_tests,private_phone_tests}.rs` only in coordination. Update current architecture/dependency notes and `debian/control` in the same coherent integration, without overwriting the active worker's ADR changes.

**Consumes:** Task 1 client spans, per-profile probe and typed capacity errors; reviewed explicit standalone analysis-reset capability.
**Produces:** One decode operation per retained packet; bounded PCM/span cursor survives sink retries; current programme continuity and detector-exclusion paths consume exact unit origins.

- [ ] Re-read active controller source and reviewed PLC ownership changes. Record exact controller/client/radio/RNNoise/codec/backend inputs. Replace the old codec-client pin with the reviewed local candidate through supported isolated dependency preparation; do not pretend a candidate is released.
- [ ] Add failing startup tests for every `AudioProfile::ALL` selection: missing tail, unsupported selected profile, null callback and wrong descriptor reject composition before activation; reload retains prior complete configuration. Probe each actual opened decoder configuration off audio with empty input, never fabricated compressed/PCM media.
- [ ] Allocate metadata only during `AudioCodec` preparation. Under current SIP maxptime30, iLBC30 needs one span; other audited current SIP codecs return one. Keep existing receive PCM allocation and do not enlarge packet durations. General mixed-unit tests belong to the shared provider/client; make the retained span slicing helper testable with a small artificial mixed map without claiming SIP accepts those aggregates.
- [ ] Add a regression that fails if either fallback runs: neutral client `decode_recovering` OR controller `AudioCodec::decode`'s existing Malformed-to-`conceal` branch. A post-advancement failure must produce one hard error, zero extra conceal/decode calls, and no successful PCM authority.
- [ ] Switch `AudioCodec::decode` to the new span operation. **Replace both fallback paths in this new receive path**, not merely the neutral client call. Keep explicit missing-span native conceal as a separate once-only operation; successful hidden iLBC recovery never takes it.
- [ ] Retain the validated spans with existing Pending PCM and accepted-prefix progress. Deliver no more than the current span's remaining frames in each source-credit operation; trim and advance only accepted credit. Keep original source coordinates and age evidence. No decoder call on retry or overlap trim.
- [ ] Add tests for refusal, partial acceptance inside a span, exact boundary acceptance, overlap crossing a boundary and stale pending cancellation. Verify identical retained PCM/metadata and single decoder advancement. Reject invalid provider span results before any source delivery.
- [ ] Route recovered spans to the existing concealed continuity path and PacketDecoded spans to genuine processing. Preserve conservative prior admitted-history birth for recovered audio; a corrupt packet's receipt cannot renew age or hold. Use the reviewed explicit analysis reset once per relevant loss transition, not on every retry.
- [ ] Add practical-authority tests: recovered-only input never qualifies speech/DTMF or keys; established hold end remains unchanged; a later valid PacketDecoded unit may requalify using normal thresholds even though native decoder history is retained. No arbitrary washout, PCM heuristic or sample-purity assertion. Characterize genuine silence after recovery without altering the accepted boundary.
- [ ] Run current real iLBC30/native codec suite, phone media/private product tests and allocator guards with exact candidate DSOs. Include all current SIP codecs and an old active-entrypoint consumer. Preserve valid/recovered PCM; verify no double fallback and no unintended IAX/Asterisk behavior changes.
- [ ] Update SDK/cohort/package checks only where actually affected. Controller runtime/dev floors must identify the first actual qualified codec artifacts; header availability at build is not runtime attestation. Keep active old consumers supported without adding historical compatibility code.
- [ ] Update concise architecture notes to state the accepted unit-authority boundary and residual decoder-history limitation. Commit only coordinated, validated integration files locally.
- [ ] **Independent review gate:** both removed new-path fallbacks, retained span/PCM identity, metadata bounds, source-age/hold preservation, startup refusal and accepted practical authority.

### Task 3: Qualified cohort and handoff

- [ ] Record exact source commits, client pin, archive/header hash at build, DSO hashes, package versions, native architecture and freshly pulled image digests. Differentiate actively supported old entrypoints from candidates that require the new tail.
- [ ] Run platform-independent formatting/lint/static/Doxygen once; native Debian 13 amd64/arm64 tests/build/package/staged-install checks and required 100% production line/branch coverage on amd64. Record stages independently; focused checks are not a full pass.
- [ ] Independently review the complete diff for accidental decoder PCM/history changes, sample-purity claims, invented washout, new packet bounds/codecs, silent provider downgrade or programme admission resets.
- [ ] Return reviewed local commits, exact cohort and remaining manual/hardware limits to the coordinator. No source publication, release, deployment or RF action.

## Planning self-review

The accepted authority boundary appears in both documents and tests. Both old fallback locations are explicitly excluded from the new receive path. Metadata capacities are computed before native mutation and the present SIP footprint remains one span. Source/header attestation belongs to build/package qualification; runtime version is only a sanity check. Compatibility is limited to active prefix/entrypoint use. No implementation or verification success is implied by this plan.
