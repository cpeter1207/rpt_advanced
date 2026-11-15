# SIP negotiated codec profiles

The owner's codec-parity request reuses the existing released codec providers.
No new external dependency, codec implementation, deployment or public push is
part of this local checkpoint. Baseline: `1285f8f56acfc7e6a84ed95741f2885e23fde69a`.

1. Introduce explicit provider/mode, PCM/RTP clocks, packet bounds and directional
   payload mapping in standalone phone media. Reuse the immutable shared codec
   client rather than maintaining duplicate descriptor/context code. Preserve
   PCMU/PCMA-only negotiation and all current G.711 behavior.
2. Prove the neutral change with the existing timed RTP, call-media exact credit,
   provider fault/cleanup, SDP/socket and native jitter suites; run scoped strict
   checks and commit this independent source checkpoint.
3. Add G.722 tests for 320 PCM frames, 160 RTP ticks, 160 bytes and 20 ms using the
   released SpanDSP provider. Qualify product source-rate detection and finite
   tail/timed-send assumptions first. Retain one verified composition catalog
   before any wider offer. Do not bypass DTMF or add a second resampler.
4. Verify incoming/outgoing loopback, private control, event and in-band DTMF,
   source age, retirement, loss and finite-tail behavior. Stop for authorized
   real-hardware testing before expanding codec breadth.

Later parity retains codec-specific units and modes rather than forcing 20 ms.
Dialogic ADPCM/LPC10 RTP compatibility requires an explicit wire decision.
Siren7, Siren14 and G.719 remain deferred and unadvertised.

Review focus: compressed bytes are not PCM frames; G.722's RTP clock differs;
missing provider directions never become advertisement; retries preserve source
birth and packet identity; DTMF/source-rate and finite-tail guards remain truthful.


Current local evidence:
- Neutral checkpoint f2f0d8ba preserves G.711; focused suites and scoped strict
  checks passed. Its independent bundle requires baseline 1285f8f.
- G.722 compilation RED captured three missing profile/catalog items. After
  source plumbing, finite-tail RED reproduced the one-frame G.711 flush rather
  than a negotiated 160-frame packet.
- Owned DTMF candidate 0fa83df qualified 24 Rust tests plus C smoke and strict
  quality; independent review clear. Alpha4 draft PR #5 is published separately at exact f8d11c3; hosted
  coverage/native package/release qualification remains separate from this
  isolated candidate test. No system or live installation occurred.
- Independent exact-full finite review found missing final-state fencing.
  Regression RED reproduced fresh input acceptance after the final full packet;
  the correction fences only fully consumed finite input, retaining a legal
  unconsumed suffix for multi-packet calls.
- G.722 focused GREEN: codec 3, media 16, call-media 11, external media 15,
  provider-fault 1, capability 1, source 13, and readiness with both the isolated
  candidate and installed old detector. The old detector rejects enabled SIP
  before publication; disabled SIP and IAX-only configurations remain valid.
- Actual incoming G.722 telephone-event authentication passed in 20.41 s. The
  first in-band fixture failed because prompt waits froze its RTP source clock;
  reusing the existing paced silence path passed in 22.83 s. Product jitter,
  expiry and detector logic were not relaxed. Outgoing G.722 two-way continuous
  silence/first utterance/radio return passed in 70.43 s; original G.711 private
  incoming passed in 19.94 s.
- Independent review corrected catalog retention through worker join/TLS cleanup
  and made new wideband test durations/correlation use PCM rate. The original
  G.711 nonzero oracle remains intact. Final strict checkpoint passed formatting,
  four-package Clippy all-targets and Rustdoc with warnings denied, outbound 20,
  call-media 11, and catalog lifetime 1. No hardware or full-aggregate claim.

## Remaining parity batches

The actual IAX catalog contains 16 profiles. G.722 is the first addition, not
completion. The remaining groups below are an execution order, not permission
to advertise before verification or to bypass the hardware milestone above.
Use the pinned owned provider metadata, inspection, PLC and finite-span APIs.
No new external dependency or duplicate SRC/codec implementation is planned.

1. **Fixed, zero-lookahead profiles:** GSM, G726-32, AAL2-G726-32, PCM8 and PCM16.
   GSM is 160 PCM/33 bytes. The G726 variants need distinct negotiated names and
   verified nibble order. PCM8 is already network-endian; PCM16's owned IAX
   representation is little-endian and needs bounded byte swapping for RTP L16.
   Replace remaining 160-byte assumptions with profile-bounded storage and
   dynamic directional payload mapping, without enlarging every queue entry to
   a maximum UDP datagram. Estimate: 3-5 active engineering hours.
2. **Explicit 30 ms mode:** iLBC mode 30, 240 PCM/50 bytes. Preserve explicit SDP
   mode and received packet inspection. Offer decoded PCM through existing
   source-admission fragments of at most 20 ms, preserving original packet birth,
   offset and accepted-prefix credit. Do not change IAX parity to mode 20 merely
   to fit the initial G.711 cadence. Estimate: 2-3 active hours.
3. **Delayed fixed coding units:** G729 and G723. G729 uses 80 PCM/10-byte units
   with 40-sample lookahead; its existing encoder disables VAD and decoder accepts
   a trailing 2-byte SID. Qualify truthful Annex B negotiation and receive policy.
   G723 encodes 6.3 kbps (240 PCM/24 bytes) and decodes 5.3/6.3 kbps, with
   60-sample lookahead. Replace only the initial zero-lookahead finite-packet
   policy with provider-native delayed flush/source-span accounting. Do not
   assume every legal packet or finite tail is 20 ms. Estimate: 4-6 active hours.
4. **Variable packets:** Speex8, Speex16 and Opus. Existing bounds are 63, 107 and
   1275 bytes; lookahead is 40, 143 and 312 samples. Opus encodes 960 PCM/20 ms
   at 48 kHz but can decode 5760 PCM/120 ms. Preserve inspected duration, bounded
   decode storage, native PLC and exact source fragmentation. SDP Opus uses
   48000/2 even for this mono PCM path. Estimate: 4-8 active hours.
5. **Legacy compatibility, still required:** Dialogic ADPCM emits raw 80 bytes
   for 160 PCM with persistent state; RFC DVI4 has a separate predictor/index
   header and wire contract. LPC10 emits 7 bytes/180 PCM (22.5 ms), with
   540-sample lookahead, unlike RTP LPC's 14 bytes/20 ms. Neither may be advertised
   under an incompatible standard alias. Verify a real target peer's exact
   private mapping or prove an appropriate wire adapter using owned provider
   state. Otherwise a distinct owned wire/codec profile needs separate design
   and qualification. Initial compatibility investigation: 1-2 active hours;
   implementation cannot be estimated honestly until that evidence exists.

Each batch needs negative SDP/mode checks, bounded packet validation, true source
age/cut/finite-tail tests, loss/PLC, event and in-band control, and actual two-way
wire coverage before advertisement. The 14 conventional profiles therefore need
roughly 13-22 further active engineering hours after this foundation. Serial test
slots, hardware validation, hosted native gates and any release wait are extra.
These estimates are planning ranges, not commitments. The two legacy profiles
remain explicitly open; reaching 14 is not full parity.

Evidence: `rust/standalone/src/iax.rs` composition manifest and the pinned owned
codec adapters' `rust/{pcm,gsm,spandsp,ilbc,bcg729,ffmpeg,speex,opus,dialogic}`
metadata/implementations. Wire references: RFC 3551, RFC 3952, RFC 5574 and
RFC 7587. Codec names alone are not evidence of wire compatibility.
