# SIP iLBC30 and Bounded Receive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Negotiate the released IAX-compatible iLBC30 profile and honestly bound multi-duration SIP receive at30ms.

**Architecture:** Keep fixed profile TX pacing and reuse native packet inspection before the current RTP source latch. Configure only decoder/receive workspaces to the complete-unit ceiling and pass actual packet span/length through existing jitter and source credit.

**Tech Stack:** Rust, existing rptadv-codec-client and released providers, RTP/SDP, current jitter and product ABI.

**Spec:** `docs/superpowers/specs/2026-11-15-sip-ilbc30-and-bounded-receive.md`.

## Global Constraints

- Preserve immutable Batch A source and receipts in its separate worktree.
- Local receive ceiling30ms; existing TX20ms and iLBC TX30ms; no mode20 encoder.
- No new ABI, provider release, dependency, queue, converter or live changes.
- Use the serial1CPU/1536MiB maintained launcher only after an explicit slot grant.
- Keep source age, finite real-source span, exact retirement, privacy and authorization unchanged.
- Local targeted commits are recovery points, not full hosted/native/coverage qualification.

## Review Focus

- Malformed first packets must neither latch SSRC nor overwrite output; provider faults must remain terminal.
- Peer maxptime must not be mistaken for our independent receive ceiling.
- GSM partial units and iLBC60ms aggregates must not fit a claimed30ms contract.
- Variable packet spans must preserve exact original birth and refused source prefixes across loss/overlap.
- Fixed TX cadence, event8k pacing and immutable blocked datagrams must survive30ms audio.

### Task1: Prove and implement bounded native receive

**Files:** Modify `rust/standalone/src/phone/codec.rs`, `phone/media.rs`, `phone/call_media.rs`; test `rust/standalone/tests/phone_variable_receive.rs`, existing `phone_media.rs`, `phone/call_media_tests.rs` and `phone/media/tests.rs`.

**Interfaces:** Add `AudioCodec::inspect(&self, packet: &[u8]) -> Result<u32, MediaError>` and crate-local RX capacity accessors. Require a call-local `Fn(&[u8]) -> Result<u32, MediaError>` on `RtpEndpoint::receive`, `try_receive`, and `receive_wire`. Extend `RtpEvent::Audio` with actual `length: usize` and `span: u32`; no stored callback or default validation bypass.

- [ ] Add `bounded_receive_accepts_complete_native_units_through_thirty_ms`: real existing profiles decode10/20/30ms where native units permit; GSM20ms succeeds and partial/40ms fails; every over-ceiling/empty/partial input fails. Existing TX output lengths remain unchanged.
- [ ] Add real RTP receive tests for accepted10/30ms spans, unchanged output/SSRC on invalid payload and provider-error classification. Observe behavioral RED before implementation.
- [ ] Prepare decoder maxima from floor(rate*30/1000/native_unit)*native_unit and corresponding bytes, retaining fixed encoder configuration. Inspect before latch/copy. Decode actual prefix through existing native recovery; swap only L16's actual prefix.
- [ ] Allocate selected RX maxima in Media, retain existing jitter loss unit, pass actual inspected span/payload to put and actual returned length/span to decode. Reject impossible shapes without altering source-credit semantics.
- [ ] Extend existing source-credit fixtures for mixed10/30ms packets, refusal, overlap, loss, original birth and bounded retirement. Execute targeted GREEN plus unchanged fixed-TX/finite/transport controls.
- [ ] Commit the independently verified receive change; update ADR0044 at the existing packet boundary.

### Task2: Negotiate and carry iLBC30

**Files:** Modify `phone/media.rs`, `phone/codec.rs`, existing `phone/media/tests.rs`, `rust/standalone/tests/phone_ilbc.rs` and `phone_codec.rs`; no ABI files.

**Interfaces:** Add `AudioProfile::Ilbc30` with PT100, profile11/mode30, PCM/RTP8k,240frames/50bytes. Startup catalog opens the existing released iLBC provider both ways before advertisement. Keep `MediaDescription` directional fields unchanged.

- [ ] Add table-driven SDP RED: incoming mode20/30/absent resolves30; outgoing answer20/30/absent keeps30; mode30+ptime20 accepted; fractional valid ptime/maxptime; maxptime20 excludes iLBC but keeps G711; unsupported mode/format/PT reinterpretation rejects.
- [ ] Add SDP emission RED: explicit mode30, maxptime30, no common ptime in mixed-duration offer, selected answer ptime30. Peer maximum never shrinks our advertised receive capability.
- [ ] Add profile/real-provider RED for240/50/8k/mode30 and bounded decode. Existing finite-source tests automatically cover one-frame, half-tail, exact-full, two-packet, OutputFull/reset when the profile enters ALL.
- [ ] Implement the single profile/catalog selection and minimal validated decimal packet-time eligibility. Preserve format-name case handling, directional PT mapping and native event8k behavior.
- [ ] Execute new SDP/profile/finite/provider-error GREEN plus the existing eight-profile controls; commit.

### Task3: Qualify actual paced30ms calls

**Files:** Modify only existing `host_services/product_sip_outgoing_tests.rs`, `product_sip_tests.rs` caller fixture rate/packet duration seams and new profile selectors; record receipts in this plan and ignored quality-progress.

**Interfaces:** Test caller pacing derives1000*packet_frames/pcm_rate_hz; RTP increments derive packet_frames*rtp_clock_hz/pcm_rate_hz. Existing20ms values and exact privacy/first-utterance assertions stay unchanged.

- [ ] Add incoming native-event/in-band authentication and outgoing quiet/two-way/finite/private-IVR30ms selectors, observing intended RED at the unimplemented path before repairs.
- [ ] Run bounded actual SIP/RTP/controller cases with exact private product/SIP and official alpha4 hashes; preserve original per-case bounds and release the slot after each granted partition.
- [ ] Run focused age/finite/event/lifetime regressions, C ABI and strict checks, then coherent full suites under the existing queue. Obtain independent exact-diff review.
- [ ] Save clean recovery commits/bundle and explicit limits: simulated endpoint shares owned codec, external/hardware/native/coverage remain separate. Do not claim full sixteen-profile parity.

## Execution ledger

The parent approved the bounded30ms design and source seam before production work.
Baseline is immutable0d0e126 (runtime ed73d27), with coherent core/product and
standalone/C ABI/strict GREEN. Eighteen excluded outgoing cases remain pending
exact-runtime replay in the frozen Batch A tree; that backlog is not hidden by
this new branch. No production change or new RED result exists at plan creation.
