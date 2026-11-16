# SIP Fixed-Frame Codec Batch A Implementation Plan

> For agentic workers: use the executing-plans workflow, with independent source review and explicitly granted serial test slots.

**Goal:** Add the five existing IAX profiles GSM FR, G726-32, AAL2-G726-32, PCM8 and PCM16 to SIP without duplicating codec or media owners.

**Architecture:** Extend the existing negotiated profile and released codec catalog. Keep the existing RTP socket, egress owner, jitter, source-credit ledger, converter and finite-packet policy. Resolve event-clock negotiation before adding wideband advertisement.

**Tech stack:** Rust; existing rptadv-codec-client and released gsm, spandsp and pcm DSOs; current SDP/RTP crates and product ABI.

**Spec:** `2026-11-15-sip-codec-profiles.md`, ADR0044 and the owner's all-IAX-codec parity request. This isolated branch starts at qualified `ab54cf0`; it does not mutate that checkpoint.

## Global constraints

- No new external dependency, vendor patch, duplicate codec/SRC, queue or runtime.
- No production edit before an observed intended RED; no advertisement before the corresponding end-to-end checks pass.
- One explicitly granted heavy job, constrained launcher, 1 CPU/1536 MiB/no swap. No live or system-provider action.
- Keep PCM frames, RTP ticks, bytes, source birth, retirement and directional PT mapping distinct.
- Existing G711/G722 behavior and assertions remain controls. All sixteen IAX profiles stay in the larger parity inventory.

## Review focus

- Standard G726 versus AAL2 nibble order and PCM16 little-endian provider versus RTP L16 network order.
- Dynamic answer payload IDs versus the offer's receive mapping; duplicate/remapped PTs must not become another codec.
- Valid mixed-clock answers must not be rejected as malformed merely because the offered audio and event clocks differ.
- Larger packets must preserve immutable WouldBlock retries, finite true-source spans, source age and cancellation without maximum-UDP-sized queues.
- Private event and in-band control must not duplicate, escape the exact call or leak to RF/ordinary peers.

## Task 1: Prove definite codec wire contracts

Files: `rust/standalone/tests/phone_batch_a.rs` (new, tests only).

- [x] Write seven tests through existing `MediaDescription::parse`, `AudioProfile::parameters` and `AudioCodec::{load,encode,decode}`.
- [x] Ran the bounded RED command: compilation13.15s; all7 tests failed at parsing with UnsupportedDescription; no unrelated compiler/provider failure. Receipt `.work/batch-a-red.log`, terminal RUN_EXIT=101. This is intended RED, not passing qualification.
- [x] Retained the RED receipt before production implementation.

Exact profiles: GSM ID3, 8 kHz/160 PCM/33 bytes; G726 ID12 and AAL2 ID5, 8 kHz/160 PCM/80 bytes; PCM8 ID7, 8 kHz/160 PCM/320 bytes; PCM16 ID14, 16 kHz/320 PCM/640 bytes. All mode0 and 20 ms for this batch. Test both L16 rates against signed network-endian words; test G726 variants as nibble swaps with identical decoded PCM.

## Task 2: Resolve event-clock and offer/answer design

Files: `phone/media.rs`, `phone/media/tests.rs`, `phone/call_media.rs`, `phone/registration_calls.rs`, `host_services/product_sip_outgoing_tests.rs`, `tests/phone_media.rs` under `rust/standalone/src` or `rust/standalone/tests` as appropriate.

- [x] Independent source/standards review selected separate receive mapping and matching-clock transmit eligibility. Multi-rate SDP lists do not constrain audio/event pairings.
- [x] Add RED cases for matched 8/16 kHz events, valid L16/16k plus event8k, event omission, directionally remapped PTs, malformed mappings and multiple event mappings.
- [x] Prove the same elapsed-time origin, fixed event onset across all updates/final repeats, shared sequence/SSRC, source timestamp gaps/wrap and immutable WouldBlock retries.
- [x] Reuse the current 200 ms digit/50 ms gap sender; do not add an event queue or sender.

Current source: audio timestamp milliseconds are anchored to peer elapsed time minus original source age (`product/link/outbound.rs`), while event onset uses that endpoint elapsed origin. Current profiles all use RTP8k, so this existing math is coherent. There is no RTCP report implementation or explicit RS/RR=0 SDP signaling.

Selected policy: retain negotiated receive event mappings independently of transmit event capability. Prefer same-clock event egress, comparing event rate to RTP clock, not PCM rate (G722 remains8k). For a valid outgoing mixed-clock answer, retain audio and valid receive events while selecting existing synthesized in-band transmit behavior. Incoming answers may select a matching event mapping or genuinely omit events. Never erase a retained receive mapping or reinterpret its declared clock. Actual L16+event8 wire proof must show received native events retain exact accepted source credit without granting outgoing-callee controller authority, no event egress, exact synthesized digits and no duplicate/privacy leak. Event-only IVRs may not recognize in-band digits; no universal IVR compatibility is claimed.

Keep bit64 semantics unchanged: it selects in-band RX and TX. Add semantic capability512 for TX-only synthesis with native RX. Require PHONE kind and complete51 prerequisites. Preserve old valid51/115/307/371; add563/819, reject missing prerequisites, other kinds and unknown1024. Redundant64+512 is harmless and may be accepted: do not invent an exclusion requirement. Keep Session.digit_mode from existing RX accessor; give only PrivatePhoneControl the new64|512 TX accessor. Existing detector, source credit and authority checks stay unchanged. Endpoint send_digit must independently refuse mismatched-clock native TX. Update header, both bindgen allowlists, validators, accessor/constructor and publication tests; old products explicitly reject new unknown512, no silent fallback. No ABI layout change.

RFC4733 sections1.4 and2.5.2.2 permit voice-encoded tones with event payloads; section2.5.1.2 recommends event transmission with SHOULD, not unconditional MUST. This is a permissible scoped implementation choice, not an RFC-mandated fallback. RFC9429 section5.10 is only JSEP-specific precedent. RFC7160 section4.2 mixed-clock same-SSRC additionally requires explicit RTCP-disabled signaling and clock-switch state; elapsed*rate alone is not that algorithm. A replacement-only event train also needs honest steady audio source consumption/gap accounting and pending-audio ordering, which the present Sent/WouldBlock/Expired contract cannot fake. Therefore no new suppression path or mixed-clock event transmission is added.

Primary references: https://www.rfc-editor.org/rfc/rfc3551.html , https://www.rfc-editor.org/rfc/rfc3264.html , https://www.rfc-editor.org/rfc/rfc4733.html , https://www.rfc-editor.org/rfc/rfc7160.html , https://www.rfc-editor.org/rfc/rfc9429.html#section-5.10 .

## Task 3: Implement fixed profile packets after RED/design review

Files: `phone/media.rs`, `phone/codec.rs`, `phone/call_media.rs`, `host_services.rs` and existing adjacent tests.

- [x] Extend `AudioProfile`/`AudioParameters` and the immutable owned-provider catalog with the five exact profiles.
- [x] Parse dynamic format identity separately from directional PT numbers; answer using the selected mapping, with no packet-driven mode change.
- [x] Replace the remaining fixed 160-byte receive/send signatures and 172-byte pending datagram with negotiated bounded storage allocated during setup. The batch maximum is 640 encoded bytes, not 65507 per queue slot.
- [x] Reuse `AudioCodec` finite PCM padding, inspection and native decode. Convert only PCM16's two-byte wire order at the SIP boundary, not PCM values or provider behavior.
- [x] Keep IAX's separate 160-byte private path unchanged when relaxing SIP host validation.
- [x] Extend finite-tail/source-credit, OutputFull, reset/cancel and malformed-size tests to the new profiles. Run the initial profile tests GREEN.
- [ ] Complete provider-missing/fault qualification for the expanded catalog as part of the final native/coverage gate; no per-provider absence injection is claimed by the local loopback tests.
- [ ] Run actual two-way, continuous silence/first utterance, private authentication and IVR fixtures with rate-derived physical durations; retain G711/G722 and P14 controls. Run strict and independent review before the local recovery commit.

Estimate: 3-5 active hours for fixed-profile plumbing/tests, plus approximately 1-2 hours for the event-policy review and qualified same-clock/mixed-answer fallback if accepted. A new suppression protocol is a separate estimate, not hidden in this range. Serial waiting, full native gates and hardware validation are additional.

## Local checkpoint: profile/provider boundary only

Initial7 profile tests are GREEN, plus one exact PT128-ignore/static-PT reinterpretation control; finite packet3, media unit16 and unchanged phone_media15 also pass. The first GREEN attempt found the new parser rejecting an unsupported extra PT128; its existing acceptance oracle was preserved, and the parser now ignores that extra format without allowing selected static PT reinterpretation. Receipt `.work/batch-a-checkpoint-retry.log` records the successful targeted checks. No full, strict, larger-packet transport or actual-call qualification is claimed. Outbound offers still contain only PCMU/PCMA/G722.

Directional bit512 RED is separately observed before its implementation: capability563 is rejected, and both the native-RX/in-band-TX session fixture and outgoing synthesized-command fixture fail admission. All three command exit codes are101. The existing bit64 cases remain controls. Product implementation, larger RTP packets and two bounded8/16k event mappings are next; no product GREEN claim is made here.

## Local checkpoint: negotiated transport and directional events

Direction split `a6ee1d0` passed its five affected product tests. Separate local
RED commit `9aecbaf` records the four fixed-array API compiler failures and three
behavioral event failures before implementation. Replayed archive/test/released
DTMF pin corrections end at `fb2a8ca`; the qualified `ab54cf0` and released G722
recovery branch remain unchanged.

The saved transport implementation passes profile8, variable UDP transport1,
original phone_media15, media/event unit20, call-media11 and finite-source3.
Independent read-only source review found no production issue. The first
call-media runner omitted its private provider directory, so its eleven Load
failures reached no behavior; the corrected retry verifies the original product
and SIP DSO hashes before all eleven tests pass. These are focused checks, not
full qualification. New offer formats remain local/unpublished pending actual
call validation and strict checks.

The first L16 wire preflight incorrectly compared the image-rebuilt alpha4 DSO
with the official release binary hash. It stopped before build/test; subsequent
wire runners use the already verified privately extracted official release DSO
and its exact hash, with no system installation. The first actual mixed test
then failed after18.03s at the quiet fixture's lifetime-zero RX-key assertion.
Its new combination had deliberately keyed RF for ordinary-link proof and RF
origination before answer. That is an invalid fixture combination, not evidence
of a failed RTP latch (the socket source assertion already passed). The three
RF/private IVR scenarios now omit only the quiet/latch branch, preserving the
post-answer no-rekey assertion. A separate administrative L16 quiet scenario
retains the original lifetime-zero assertions and30-second cadence test. These
four actual cases and strict qualification are still pending at this checkpoint.
The synthetic mixed-event input proves RX identity/credit and TX fallback, not
an independently generated RFC7160 mixed-clock sender timeline.

### L16 real-call checkpoint and isolated measurement correction

Mixed8k receive/private in-band transmit and matched16k events each passed71.51s
in `l16-wire-fixture-retry.log`. No-event then failed the isolated RF-output
assertion by729 aggregate output counts before the separate quiet case ran.
Read-only review found that immediate=true snapshots output immediately after
sending200, before ACK/try_complete/progress0 cancels authorized local ringing.
The linked fixture also sends1s silence before the greeting, so this combination
does not measure immediate first-utterance behavior.

A separate unchanged-assertion diagnostic passed quiet70.45s and no-event71.45s
(`l16-quiet-diagnostic.log`). The latter captured ACKs=0/PTT=true at the privacy
baseline and ACKs=1/PTT=false afterward, with no added PCM or linked turns. This
confirms the invalid pre-answer measurement boundary, but cannot retrospectively
attribute the earlier729 samples. The three isolated L16 scenarios now reuse the
established immediate=false setup, including its0.5s media transition followed
by the strict0.2s post-answer ringback-stability assertion, before the unchanged
peer-privacy measurement. Temporary diagnostics are removed. No production code,
privacy threshold, source deadline or per-case timeout changed. Separate quiet
zero-key/30-second RTP and existing immediate first-utterance controls remain.
The corrected no-event fixture subsequently passed73.14s in `l16-settled.log`,
with diagnostics removed and all privacy/ringback assertions unchanged. The
separate quiet control previously passed70.45s. Four additional outgoing profile
selectors and two incoming L16 authentication selectors compile but still await
execution; their presence is not passing wire evidence.

### Strict source checkpoint

The first strict run stopped on two stale check metadata files from the separate
manifest-only cohort checkout. After deleting only their verified16-byte Cargo
completion markers, the bounded retry regenerated both metadata files from this
Batch A source and checked the embedded origin and hashes. No production change
was used to repair that tooling failure. `batch-a-strict-retry.log` records exit0:
formatting, four-package all-target Clippy, Rustdoc, five directional product
checks, twenty outbound source/age/cancellation checks and the catalog-lifetime
check passed. This receipt precedes the newly prepared malformed-length host
regression; the settled no-event rerun is recorded above, and remaining codec wire cases
are pending.

### Compatibility regressions observed before repair

`compatibility-red.log` records two separate Cargo101 results: two SDP tests
reject valid case-insensitive encoding names or silently lose their event
mapping, and one actual host callback test retires a live call after malformed
encoded length. The wrapper exit0 verifies only those expected RED outcomes.
RFC8866 section5.14 (referencing RFC4855 section3) requires case-insensitive
encoding names; numeric clock/channel and payload identity remain exact.
https://www.rfc-editor.org/rfc/rfc8866.html#section-5.14

Independent source review confirmed that the previous host rejected invalid
encoded length before entering the terminal media-error path. The minimal repair
must preserve that nonterminal rejection and the existing terminal behavior of
correct-length/wrong-PCM-frame input. The saved data-driven test constructs real
Media/Shared for every profile and calls the actual ABI callback, including
null/oversized bounds and the terminal frame control. Production repair and
GREEN are not yet part of this RED checkpoint.

The following minimal repair passed `compatibility-green.log`, terminal0:
new SDP2 and actual host lifecycle1; media/event22; original phone_media15;
profile/provider8; formatting, all-target standalone Clippy and Rustdoc with
warnings denied. Independent review cleared the exact two-file source delta.
Bad encoded length now returns the same nonterminal-1 before frame validation;
valid-length wrong PCM frames still retire the call. ASCII-insensitive name
comparisons retain exact numeric clock, channel, payload and format-shape checks.
No new codec, state, queue, dependency or runtime API was introduced by this repair.

### Eight-profile local wire milestone

On clean runtime source `ed73d2704fc24563cdecbe80def6941c8144e27b`,
`batch-a-six-wire.log` exited0 with the existing per-case bounds and assertions:

- GSM full-rate quiet/two-way/successor:70.55s.
- Standard G726-32 quiet/two-way/successor:70.51s.
- AAL2-G726-32 quiet/two-way/successor:70.42s.
- L16/8k quiet/two-way/successor:70.43s.
- L16/16k private incoming authentication with matched16k events:19.96s.
- L16/16k private incoming authentication with in-band digits:22.71s.

The four outgoing cases retain the original30-second continuous-silence RTP,
zero receiver-key, first utterance, two-way signal and exact successor/hangup
controls. The incoming cases retain private prompt/control and no-radio-output
assertions. The L16 directional mixed/matched, absent-event and quiet receipts
are separately described above. Existing package manifests already include GSM,
PCM and SpanDSP providers; this batch adds no dependency.

These are real loopback SIP/RTP/controller-path tests with a synthetic endpoint
using the same released owned codecs. They are not independent third-party
interoperability or hardware proof. The G726 provider-boundary test checks the
nibble-order relationship and equal decoded PCM, not an external conformance
vector. RFC3551 section4.5.4 specifies low-first G726 packing and its difference
from I.366.2 AnnexE AAL2 packing; external peer/vector validation remains a
separate qualification limit.
https://www.rfc-editor.org/rfc/rfc3551.html#section-4.5.4

Full coherent core/product, standalone/C ABI/strict, and current G711/G722/P14
outgoing union regressions remain queued. Native arm64, coverage, packaging,
hosted checks, external endpoints and hardware have not been qualified by these
receipts. No public combined artifact or live configuration changed. This is
eight of sixteen ordinary IAX profiles, not complete parity. iLBC30 is the next
isolated packet-duration increment; the six remaining conventional profiles
and two legacy wire ambiguities remain open.

### Coherent full core/product aggregate

`batch-a-full-core-product.log` exited0 on frozen `442819d` (runtime source
`ed73d27`). Fresh product/SIP builds matched the exact wire-tested private DSO
bytes before execution. Full core passed823 unit plus94 integration tests. Full
product passed887 unit plus5 integration tests; its existing ignored load
experiment is `host::tests::egress_load::egress_full_path_load`, invoked separately
by `tests/peer_egress_load.py`. Product unit runtime was422.63s. Source was unchanged
during the run; no stale16k test oracle failed. Standalone/C ABI/remaining union
regressions and the native/coverage/package/hardware limits above remain open.


### Frozen standalone, C ABI and strict checkpoint

`batch-a-base-strict.log` exited0 on frozen `f97bbc8` (runtime source
`ed73d27`), using freshly verified quality image
`sha256:c3800ed0094d0d01284b70481d3189a76044042b2900594115db2e2a41f2d0fa`.
The official alpha4 detector hash and the private product/SIP hashes were
checked first; fresh product/SIP builds were byte-identical to the wire-tested
artifacts. The real C ABI smoke passed. Standalone passed342 unit,2 binary,
40 integration and1 documentation tests. Its22 outgoing namespace cases were
explicitly excluded here and require their separate actual-wire receipts.
Six-package all-target Clippy and Rustdoc with warnings denied, plus the
non-mutating workspace format check, passed. Log SHA256:
`2b1457d2a6fcb7761f18a1518c35fac3bbb6c635057b9bf75a2e10b0cf9e972f`.

Current G711/G722/P14 and corrected L16 outgoing union regressions remain a
separate pending checkpoint. These local results do not substitute for native
arm64, production coverage, hosted PR gates, packaging, external interoperability
or hardware validation. No public combined artifact, node or live state changed.
