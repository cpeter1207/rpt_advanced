# Codec-unit recovery provenance for SIP PLC

Status: independently reviewed design; practical unit-authority boundary explicitly adopted by the coordinator on 2026-11-16. Implementation is separately dispatched under the companion plan. No implementation, native validation, publication or node change is claimed.

## Goal and authority boundary

Complete the SIP PLC work by classifying every accepted iLBC coding unit as packet-decoded or natively recovered, including recovery hidden behind a successful native decode. Preserve native programme PCM, retries and codec history. Concealed units may continue already-admitted programme only under existing immutable age and hold limits; they never initiate RF, qualify speech/DTMF, or renew those limits.

**Accepted authority boundary:** the decoder's coding-unit outcome, not mathematical sample purity. A valid subsequent PacketDecoded unit may qualify speech/DTMF despite ordinary prior-PLC decoder/filter history, using normal detector qualification and the existing explicit analysis-history reset. NativeRecovered units and explicit PLC are non-authoritative. No sample-purity claim or invented washout interval is permitted.

This is a narrow supplement to the phone PLC/courtesy design and the separately reviewed standalone analysis-reset design. Do not change codec selection, native enhancement, programme retention/admission, RF policy, detector thresholds, packet duration, jitter defaults, implicit radio resets or courtesy behavior. No PCM heuristic, synthetic detector input, invented silence, decoder replacement pool, second decoder or new worker.

## Verified findings and sources

Audited provider checkout: `/home/codex/rpt-development/shared-components/rptadv-codec-adapters`, HEAD `83725b7`.

The official libilbc **2.0.2** release archive was read in memory and SHA256 verified:
`84d5c99087a35bda3463bcb993245b4f8c28c04fe037215a5ec56570f68ce519`.

Archive URL:
https://github.com/TimothyGu/libilbc/releases/download/v2.0.2/libilbc-2.0.2.tar.gz

Evidence from that archive, not current v3:
- `ilbc/decode.c:81-89`: invalid start index or final bit changes the unit's decode mode to PLC.
- `ilbc/decode.c:119-136`: the native decoder generates PLC and preserves that output.
- `ilbc/decode.c:241-245`: `prev_enh_pl` is set to 1 iff this unit used PLC, otherwise 0.
- `ilbc/ilbc.c:177-183`: multi-unit public Decode repeats DecodeImpl, overwriting the flag for each unit, then sets `speechType=1` unconditionally. Its positive frame count and speech type do not establish absence of recovery.
- `ilbc/interface/ilbc.h:256-402`: the installed public RFC3951 compatibility declarations include `iLBC_Dec_Inst_t` and its `prev_enh_pl` member.
- `CMakeLists.txt:198`: installs that header.
- Provider `rust/ilbc/src/lib.rs:158-188` already invokes native Decode once per 38-byte/160-sample or 50-byte/240-sample unit. Its return path discards the classification, but no second decode is necessary.
- Shared contract `rust/contract/src/lib.rs::Descriptor` exposes decode frame count and status, not output origin.
- Client `rust/client/src/lib.rs:522` implements Malformed-to-conceal recovery without provenance. Its table loader at line 355 reads the entire generated descriptor after a size check.
- Controller `rust/standalone/src/phone/codec.rs` and `call_media.rs::Pending` currently carry packet-wide provenance. Current SIP receive ceiling is 30 ms, so iLBC30 has one unit per accepted packet. General codec consumers still support aggregates.
- `tests/test_native_profiles.py:353` already constructs damaged iLBC units by invalid start index and final bit, and verifies that native concealment remains enabled.
- Packaging already binds the iLBC provider to its separately installed backend at the same package version (`debian/control:60`).

## Decoder-history limitation, explicitly retained

The public compatibility field provides a truthful **unit recovery outcome**. It is not an independent stable provenance query guaranteed across upstream releases. Keep it confined to the pinned provider.

Native enhancement stays enabled. In the audited source:
- `decode.c:158-185` documents enhancer delay of 40 samples for 20 ms mode and 80 samples for 30 ms mode.
- `enhancer_interface.c:196-200` describes backward interpolation mixed with PLC data after loss.
- Enhancement uses retained history; synthesis/high-pass filters also retain state (`decode.c:189-190,233-239`).

Consequently a later `prev_enh_pl=0` does not imply every output sample is independent of PLC history. This proposal deliberately does not derive a washout rule from those delay constants and does not add a speculative third origin state whose clearing condition cannot be justified. Do not skip 80 samples, skip one arbitrary frame, freeze timestamps, or assert that resetting Earshot clears decoder history.

The bounded claim is: every coding unit that actually invokes native PLC is marked recovered, excluded from detector authority and never concealed a second time. Subsequent genuinely packet-decoded units can requalify under normal configured detection and source timing. Real post-loss speech/silence tests must characterize this practical behavior; they do not prove mathematical independence from all prior codec state.

A stronger requirement for sample-level history attribution would be a separate design decision requiring additional backend support. It is not silently included or silently claimed here.

## Minimal provider observation

Add a tiny provider-private C accessor compiled against the exact installed `ilbc.h` to read `((const iLBC_Dec_Inst_t *)state)->prev_enh_pl` immediately after each successful native unit decode. Use the existing live state and require the expected 0/1 outcome; an unexpected value fails provider validation.

Do not reproduce the C layout in handwritten Rust, hard-code offsets, use `speechType` as a recovery flag, call DecodeImpl separately, patch upstream, or parse encoded validity bits into a second classifier. Native Decode remains the only operation deciding recovery.

Verify the checksum-pinned upstream source, build header hash and exact backend/package/DSO identities during build and package qualification. An off-audio runtime version check is only a sanity check, not binary attestation; runtime installations do not require development headers. The provider owns the version coupling; no upstream struct crosses the neutral ABI. Existing native format/mode/enhancer behavior remains unchanged.

## Smallest append-only neutral contract

Append one `decode_with_provenance` callback after the active `conceal` field in codec ABI1. Preserve the existing prefix layout, capability identity, descriptor entrypoint, result values and SONAME1. Preserve old decode/conceal behavior for actively used non-provenance consumers.

Proposed inputs are the existing decoder/packet/PCM arguments plus caller-owned span storage and a sized result record. A span is three u32 values:
- `offset_frames`: first PCM frame relative to this decoded packet;
- `frame_count`: nonzero covered count;
- `origin`: 0 PacketDecoded, 1 NativeRecovered; other values invalid.

The result carries produced frame count, written span count, required PCM capacity and required span capacity. Complete results cover all produced PCM exactly, in order, with no holes, overlap or unknown origin. Adjacent equal-origin units may coalesce. This records exact unit boundaries when origins differ, not per-sample purity.

Before calling any native decoder, validate all pointers/sizes, packet framing, PCM capacity and conservative metadata capacity. Either capacity shortage returns OutputFull with both required counts and **no decoder advancement**. For iLBC, required metadata capacity is at most the number of coding units, computed from the packet length and fixed configured mode. Audited all-packet-decoded paths need one span. No per-packet allocation or growing vector.

An empty packet is a non-consuming capability probe: implemented profile/mode returns Complete with zero frames/spans; unsupported provenance returns Unsupported without mutation. This permits startup verification without decoding fabricated audio. Document this explicitly rather than relying on a null function pointer that cannot distinguish profiles within a DSO.

Use a named prefix minimum and a named provenance-tail minimum. The new client reads/copies only the validated old prefix, then conditionally reads the tail; simply enlarging its current RawDescriptor size check would reject old providers for unrelated existing uses.

## Errors, retry and ownership

A successful internally recovered unit returns valid PCM plus NativeRecovered, not Malformed. Never call conceal for it again.

The new provenance-aware path replaces BOTH the neutral client's `decode_recovering` use and the controller `AudioCodec::decode` Malformed-to-conceal fallback. A post-advancement failure cannot trigger a second decode/conceal of the whole packet. Preflight errors and OutputFull consume nothing. On unexpected native/postdecode validation failure, publish no successful result and mark the context unusable until reset; propagate the existing hard provider-failure handling. This proposal does not add general decoder rollback or partial-success transactions. Ordinary internally recovered iLBC units are successful, so mixed valid/recovered packets retain all their PCM.

Explicit missing spans still request native conceal once; its returned PCM is wholly NativeRecovered. Unsupported native conceal returns zero for existing ring fallback, not successful silence.

Retain decoded PCM and its bounded spans together with Pending source coordinates, accepted-prefix progress and immutable source-age evidence. Backpressure retries slice that retained result; they never decode again. Overlap trimming adjusts span offsets. Credit may stop inside a span or at a boundary without changing provenance.

Current SIP may keep the minimal one-unit metadata footprint for its actual receive bounds, but the neutral client/result representation must preserve mixed aggregates for existing general consumers. Do not expand SIP packet durations merely to exercise aggregate tests.

## Controller practical authority

Route PacketDecoded slices through the existing genuine-source path. Route NativeRecovered slices through the existing concealed continuity path. A recovered slice cannot create fresh receipt authority: use the conservative prior admitted-history bound already specified for PLC, never its corrupt packet's receipt as a new speech hold/age grant.

At a recovered interval, retire only qualifying analysis history through the approved explicit analysis-reset seam, not the programme ring/admission. Keep source coordinates advancing normally. The next PacketDecoded slice starts a new analysis sequence and may earn qualification through normal configured thresholds/onset. Recovered-only history cannot initiate speech/RF/DTMF or refresh hold; no prior admitted history means no PLC authority.

Failure of analysis reset remains an actual fail-closed provider error. This proposal neither hides it nor restores old pre-loss classifier frames.

## All current codec and consumer effects

For current SIP, explicitly implement/review provenance for the selected profiles in `g711`, `pcm`, `gsm`, `spandsp` (G722/G726/G726AAL2), and `ilbc`. Straight packet decoders return one PacketDecoded span after success. Do not infer hidden-recovery absence for other spandsp profiles without reviewing them.

All provider DSOs using shared descriptor glue need compile/layout/old-entrypoint regression checks. Unaudited non-SIP profiles may expose the tail callback but return Unsupported without mutation; their active old decode entrypoint remains available. Do not blanket-default every backend to PacketDecoded. Opus/Speex/G729 or new SIP codecs are not enabled by this work.

Preserve actively used old-prefix/old-operation combinations only, consistent with ADR 0040; add no historical compatibility registry, shims or retired-alpha support framework. Old client/new provider keeps working through the unchanged active prefix. New client/old provider supports existing non-provenance use. The new SIP composition requires the extension for every selected decode profile before activation; missing support fails startup or preserves the previous reload configuration. Do not silently omit iLBC, downgrade authority checks, or discover the missing contract during a live call.

The controller's recorded codec-client git pin was `becbff939a0817cc91df6dcb0d40e85e381ec87b`, distinct from audited provider HEAD `83725b7`; recheck current work before implementation. Update exact tested client/provider/header and runtime/dev package floors together. Use actual qualified candidate versions; no invented published release or unqualified alpha floor. No SONAME bump is required for the append-only extension.

## Alternatives considered

1. Read public Decode's speechType: rejected, always 1 in the pinned implementation.
2. Observe prev_enh_pl after the whole aggregate: rejected, loses earlier recovered units.
3. Treat every successful decode as original: rejected, reproduces hidden PLC authority.
4. Mark the entire mixed packet concealed: conservative but unnecessarily loses valid unit authority; the existing per-unit loop makes precise unit classification cheap.
5. Retry/decode twice or call conceal after native recovery: rejected, advances codec state twice and can replace valid PCM.
6. PCM heuristics, disabling enhancement/PLC, silently removing iLBC, extra decoder or upstream patch: rejected, alters required behavior or expands scope.
7. Exact sample-level recovery-history tracking: not claimed or required by this practical unit-origin proposal; no arbitrary washout is substituted for it.

## Required review and verification

The practical coding-unit authority boundary is adopted; the decoder-history limitation remains explicit. Execute only after separate assignment using `docs/superpowers/plans/2026-11-16-codec-provenance.md`, with independent review between provider/client and controller stages.

Tests must include:
- Native iLBC20/30 valid, invalid-start-index and final-bit recovery.
- Good/bad, bad/good, good/bad/good aggregates, repeated recovery, then genuine speech and genuine silence.
- PCM and subsequent-state equivalence with independent pinned native reference contexts; accessor observations cannot change signal processing.
- Exact one native decode per coding unit and no extra conceal after success.
- Both capacity shortages before mutation; short records, bad pointers, unknown/invalid span coverage and unexpected native flag/status.
- Partial/refused credit and overlap trimming through span boundaries with identical retained PCM/coordinates/age.
- Old/new prefix combinations and per-profile startup capability rejection.
- Recovered-only input never keys/qualifies; established hold is not renewed; later packet-decoded speech requalifies through explicit analysis reset.
- Allocation/free guards for provider observation, metadata handling and controller retries. Codec operations remain in their existing non-audio serial owner; no blanket hard-real-time guarantee is added.
- Native Debian 13 amd64/arm64, installed header/DSO/SONAME checks, exact source/backend hashes and required production coverage. Old entrypoints and unrelated codec behavior remain tested.

Source inspection and checksum verification are complete; native accessor/ABI tests, positive recovery tests and allocation instrumentation have not run. No hardware listening/RF acceptance is claimed. Package/release/node changes remain separately authorized.

## Review handoff

Approved bounded sequence: provider accessor, current-SIP native audit and neutral contract/client tests; independent review; controller integration and exact-cohort tests; independent review and required gates. Keep each increment local and bounded. No delivery-time promise accompanies this proposal.
