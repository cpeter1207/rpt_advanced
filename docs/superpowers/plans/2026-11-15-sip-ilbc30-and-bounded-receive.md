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

- [x] Add `bounded_receive_accepts_complete_native_units_through_thirty_ms`: real existing profiles decode10/20/30ms where native units permit; GSM20ms succeeds and partial/40ms fails; every over-ceiling/empty/partial input fails. Existing TX output lengths remain unchanged.
- [x] Add real RTP receive tests for accepted10/30ms spans, unchanged output/SSRC on invalid payload and provider-error classification. Observe behavioral RED before implementation.
- [x] Prepare decoder maxima from floor(rate*30/1000/native_unit)*native_unit and corresponding bytes, retaining fixed encoder configuration. Inspect before latch/copy. Decode actual prefix through existing native recovery; swap only L16's actual prefix.
- [x] Allocate selected RX maxima in Media, retain existing jitter loss unit, pass actual inspected span/payload to put and actual returned length/span to decode. Reject impossible shapes without altering source-credit semantics.
- [x] Extend existing source-credit fixtures for mixed10/30ms packets, refusal, overlap, loss, original birth and bounded retirement. Execute targeted GREEN plus unchanged fixed-TX/finite/transport controls.
- [x] Commit the independently verified receive change; update ADR0044 at the existing packet boundary.

### Task2: Negotiate and carry iLBC30

**Files:** Modify `phone/media.rs`, `phone/codec.rs`, existing `phone/media/tests.rs`, `rust/standalone/tests/phone_ilbc.rs` and `phone_codec.rs`; no ABI files.

**Interfaces:** Add `AudioProfile::Ilbc30` with PT100, profile11/mode30, PCM/RTP8k,240frames/50bytes. Startup catalog opens the existing released iLBC provider both ways before advertisement. Keep `MediaDescription` directional fields unchanged.

- [x] Add table-driven SDP RED: incoming mode20/30/absent resolves30; outgoing answer20/30/absent keeps30; mode30+ptime20 accepted; fractional valid ptime/maxptime; maxptime20 excludes iLBC but keeps G711; unsupported mode/format/PT reinterpretation rejects.
- [x] Add SDP emission RED: explicit mode30, maxptime30, no common ptime in mixed-duration offer, selected answer ptime30. Peer maximum never shrinks our advertised receive capability.
- [x] Add profile/real-provider RED for240/50/8k/mode30 and bounded decode. Existing finite-source tests automatically cover one-frame, half-tail, exact-full, two-packet, OutputFull/reset when the profile enters ALL.
- [x] Implement the single profile/catalog selection and minimal validated decimal packet-time eligibility. Preserve format-name case handling, directional PT mapping and native event8k behavior.
- [x] Execute new SDP/profile/finite/provider-error GREEN plus the existing eight-profile controls; commit.

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


### Initial behavioral RED receipt

`ilbc-receive-red.log` completed its expected-RED collection with wrapper0 and
four separate Cargo101 results. Receive tests:2 failures (PCMU10ms decode rejected,
80-byte RTP rejected) and the unchanged GSM20ms control passed. Incoming SDP/profile
3, outgoing SDP2 and the real variable-span source-credit1 failed as intended:
missing iLBC mapping/offer, fixed packet-time rejection, absent receive maxptime,
and rejection before the new source birth could be recorded. No compile/setup
failure was substituted for these assertions. Log SHA256:
`48d602fac7ff62807b2d4ba1d5e54f025c8afeb752b1630ec15f384ad518f10b`.
Production remains identical to the frozen baseline at this recovery point.
Native malformed-unit and provider-fault inspector rows, actual returned span/
length, and untouched output suffix still need the mandatory inspector seam.


### Task1 focused receive qualification

The first focused run passed framing3, provider-fault1, profile8 and transport1,
then failed one adapted test row: its PCMA fixture had accidentally changed the
original unnegotiated PT0 negative into negotiated PT8. Restoring PT0 changed no
production behavior or artifact cache. That failed receipt is preserved as
receive-green-first.log, SHA256
`f8f42b653f63b4dd4fb2bbcbbad6966a7f812320574c9f82d45480f25709ef90`.

The corrected receive-green-retry.log completed RUN_EXIT0: framing3,
provider-fault1, profile8, transport1, media15, call-media13, pre-latch inspector1,
finite3 and formatting passed. The source-credit cases include 10/30 ms input,
refused prefixes, both signed offsets, retained birth and exact PCM/gap counts.
Independent source review covered the three production files before execution.
This is a local recovery checkpoint, not full aggregate/native/coverage or wire
qualification of the new receive path. Task2 iLBC/SDP tests remain intentionally
RED and unimplemented. Frozen Batch A replay, including the separately observed
PCMA strict-window boundary failure, remains separate.


### Task2 focused iLBC qualification

Independent exact-diff review found no blocking issue atop2aca00af. The
ilbc-green.log checkpoint completed RUN_EXIT0: iLBC4, variable receive3,
provider-fault1, original profile8, transport1 (including actual iLBC UDP),
media15, media-unit25, source13 and finite3 passed, followed by standalone
all-target Clippy with warnings denied and formatting. Log SHA256:
`308ebe0c0f88765f27a1091d2ba38394c5912a66051a1d0ee936952107668f65`.
The old exact-ptime30 negative was replaced by invalidptime0, since ptime is a
preference. Its new positive/fractional cases were captured in the earlier RED.
No dependency pin, public artifact or live installation changed. Actual30ms
call fixtures, aggregate/native/coverage and external-peer checks remain open.


### Caller cadence RED

The next test-only selectors cover incoming native/in-band authentication and
outgoing quiet/native-IVR/in-band-IVR calls. They are not yet executed. A minimal
real-UDP caller test first captured the fixture's hardcoded20ms source advance:
the existing eight profiles succeeded, then iLBC's second packet carried160 RTP
ticks instead of its required240. ilbc-clock-red.log has Cargo101 and
CLOCK_RED_EXIT101; wrapper0 means expected-RED collection, not a passing test.
Compilation took13.70s, the test0.04s. Mounted source checksums were verified and
post-format compiled-input checksums retained independently of blank launcher
revision metadata. No production source changed in this test-only increment.
