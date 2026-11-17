# Codec provenance policy: review draft

Status: unresolved policy proposal; independent review required BEFORE codec or
consumer implementation. This document does not approve reduced codec support
or claim compatibility/full transport parity. RTP qualification retains priority.
No code, builds, publication or live-node changes accompany this draft.

Related: [IAX parity design](2026-11-17-iax-media-provenance-parity-design.md)
(frozen a9d570a378762cea93ca75db6b318ab9163c7b20b3275ccc84d93416425180dd),
[emitted timeline design](2026-11-17-emitted-phone-timeline-design.md), and
shared courtesy checkpoint 3d493d3138f8812677b3b072fdc1a157494be01c.

## Separate audible programme from fresh evidence

Preserve native decoded PCM, decoder evolution, correction, enhancement and
concealment. PCM equality alone is not authority equivalence: fresh evidence
controls speech onset/hold, DTMF and commands. Synthetic audio must not renew that
authority, although authorized held programme can remain audible within its
existing age/hold bounds and prevent a pending courtesy tone during the delay.
Do not confuse normal predictor/filter history in an original coding unit with
arbitrarily old media. No PCM-energy heuristic establishes provenance.

Current codec candidate 8596809c3ff8493d705bc2f65371e9f0b6e4fef2 exposes temporal
DecodeSpan origins, not separate programme/evidence PCM or frequency-band origin.
Therefore a mixed-band solution needs an explicit reviewed API extension; it
cannot be encoded truthfully by silently calling the complete mixed PCM original.

## Proposed policy table

| Input/output case | Programme behavior | Fresh evidence and receipt policy | Status |
| --- | --- | --- | --- |
| Original coding unit with ordinary predictor/filter history | Preserve exact native PCM | Eligible using the original unit receipt; normal history is not itself PLC | Retain established unit-provenance meaning |
| Explicit loss or whole-unit native recovery | Preserve native PLC, ring fallback only for absent PCM | No fresh voice/DTMF; inherit qualified predecessor bound, never current poll time | Required invariant |
| SID / DTX / untransmitted comfort noise | Preserve codec-generated programme; no replacement or second concealment | No speech onset/hold refresh or DTMF from generated noise. A new SID arrival alone must not renew synthetic speech authority | Proposed common policy; audit each codec's actual content |
| Successfully corrected packet parameters | Preserve native correction | Do not automatically classify all correction as loss. Exact recovery of coded information versus guessed/history replacement must be distinguished | Requires definition/tests below |
| Uncorrectable parameter substitution | Preserve native output | Substituted parameters cannot themselves establish fresh evidence; mixed output needs an explicit policy, not a false whole-unit claim | Unresolved |
| Independently proven mixed fresh/synthetic unit (not established by the retracted Speex example) | Preserve complete WB PCM unchanged | Either separate proven fresh-component analysis evidence, or mark whole unit nonfresh with an explicit speech-admission limitation | Material decision required |
| LPC10 delayed original output unit | Preserve existing delay and PCM | Map output to the corresponding retained original-unit receipt; include bounded lookahead contributors conservatively when proven necessary | Mapping must be established |
| LPC10 first output without established unit mapping | Preserve native PCM only if authorized bounded programme policy permits | Unknown/nonfresh; never invent original receipt or use newly received packet to relabel startup history | Conservative startup proposal |
| Missing/contradictory provenance, overflow, stale epoch or unsupported capability | No new unproved authority | Existing bounded fail-closed retirement/cut or admission rejection, with diagnostic; no automatic legacy downgrade | Required default |

SID/DTX is a classification issue, not an instruction to disable those codec
modes. Low-rate speech modes must not be mistaken for comfort noise merely
because they use a noise-like excitation or set future DTX state.

## Speex branch proof and correction

The earlier motivating claim of fresh low-band speech coexisting with explicit
high-band PLC is RETRACTED for the audited normal mode-encoded decoder path.
Do not introduce a separate evidence API based on that unproved example.

Pinned Speex 1.2.1 branch table (nb_celp.c and sb_celp.c):

| Low-band input | DTX after NB decode | Null high-band path | Implication |
| --- | --- | --- | --- |
| Modes 2-8 | Cleared at nb1537-1548 | sb1289-1299 near-zero excitation through normal filter/QMF history | Original coding-unit semantics; no independently retained high-band PLC proved |
| Mode1 extra !=15 | Cleared | Same normal null-band path | Do not suppress actual low-rate speech solely because mode1 |
| Mode1 extra =15 | Set | sb1283-1286 calls sb_decode_lost with DTX | DTX/CNG indication, not the claimed fresh low-band speech case |
| Mode0 | Prior flag retained | DTX recovery if flag set, otherwise normal null-band path | NB1455-1476 already generates noise, no fresh low-band speech |
| Null packet | Prior flag | sb1249-1252 recovery | No new packet evidence |

NB encoder1039-1046 writes extra15 only when dtx_count is active; speech modes
clear the decoder DTX flag. The high-band null path with DTX false uses near-zero
excitation and existing synthesis/QMF memory, not independently concealed old
media. Normal filter history must not be assigned an ancient PLC receipt.

### Remaining opposite-direction combination

Encoder sb580-585 derives its local dtx boolean from LOW MODE ==0, not from the
NB decoder's DTX-status flag. At sb717-743 low mode0 forces high submode0. Thus
low mode0 plus nonnull high mode is not emitted by this audited encoder path.
The decoder independently accepts valid nonzero high submodes after decoding
low mode0: sb1239-1277 has no coupling rejection, and sb1303 onward decodes the
high-band parameters. Acceptance alone does not prove a supported legitimate
speech combination; do not classify an invented packet as fresh speech.

Low mode1 with extra15 is different: encoder low mode is nonzero, so this
coupling does NOT force high mode0. A nonzero high mode is source-path possible
while the NB decoder DTX flag is set. Decoder then executes the ordinary nonnull
high-band path, not sb_decode_lost. Whether that output is legitimately fresh
high-band speech or packet-directed comfort-noise reconstruction needs an exact
encoder fixture and semantic review. Do not infer speech authority merely from
new high-band bits or nonzero mode. Source inspection does not yet establish
whole-unit homogeneous authority for all received combinations.

Recommendation: finish those reachable-case fixtures and DTX semantics before
choosing any larger API. Existing per-unit metadata plus packet parser/native
DTX query may suffice for ordinary supported behavior; no separate evidence API
is justified yet. Malformed/unsupported combinations remain bounded fail-closed,
without silently removing existing legitimate format support.

### Conditional alternatives for a genuinely proven mixed-origin case

If a native audit and supported fixture prove independently fresh evidence mixed
with synthetic content, whole-unit nonfresh classification preserves native PCM
but can suppress legitimate onset/hold. That limitation requires explicit review
and cannot be called full compatibility. Alternatively, a separate caller-owned
programme/evidence contract must obtain proven evidence from the SAME decode,
not a second context or filtering mixed PCM. Sample-rate/alignment, command mute,
receipt identity and detector equivalence would need separate review.

Fresh evidence from either component or band must NOT redate an independently
retained or concealed contributor in any other component or band. This applies
equally to fresh low/concealed high and concealed low/fresh high cases, only
where supported native behavior proves that distinction. Keep evidence receipt/voice authority
separate from programme contributor age and enforce programme expiry even when
fresh evidence continues. Apply an inherited age bound only where native audit
proves an independently retained/concealed contributor; ordinary reconstruction
from current parameters and normal predictor/filter history are NOT old PLC and
must not acquire an artificial ancient receipt. If an independently stale
component cannot be removed without changing the native output contract, reject
or expire that programme under the reviewed bounded policy rather than relabel
it fresh. Any component-separation alternative requires its own reviewed native
output/alignment contract; fresh evidence alone cannot authorize stale programme.

For any such separate evidence path, do not feed programme and evidence twice
into the detector. This invariant is conditional; it does not establish that the
retracted Speex fresh-low/PLC-high example exists.

## LPC10 delay, correction and receipt mapping

Checksum-verified SpanDSP 0.0.6 lpc10_decode.c:857-868 handles startup;
875-907 can substitute older RMS/RC after failed Hamming correction;
909-966 selects delayed parameters and explicitly maintains a one-frame delay.
The public decode result reports only sample count, not an origin/mapping ledger.

Proposal: establish output-unit identity and receipt mapping in independent native
fixtures before adding metadata. Maintain a small bounded per-unit receipt queue
with proven delay, not a stream-wide oldest receipt. Track actual startup and
lookahead dependence separately. On reset/epoch change invalidate that queue; an
unmapped unit is nonfresh, never reassigned a newer receipt. Aggregation and
partial output retain the same unit identity and receipt. Do not claim that a
one-frame source comment alone proves complete output timing: synthesis buffering,
parameter smoothing and lookahead must be traced to returned PCM.

### Failed-correction propagation and clearing proof

Further checksum-verified native audit shows synths initialization with 180
zeros (lpc10_decode.c:1041-1043). Synthesis appends variable pitch epochs, emits
180 samples and retains a tail (:554-572); returned blocks can straddle epochs.
Consequently a fixed one-frame receipt shift does not establish PCM provenance.
The proposed <=180 retained-tail bound is a proof obligation, not an assumption.

Hamming failure copies delayed donor parameters from [2] into [1]; housekeeping
writes the repaired [1] back into [2] (:881-904,956-966). Repeated failures can
therefore retain an old donor indefinitely. A fixed last-N packet receipt window
alone is WRONG for those recovered contributions. Trace actual failed-correction
donor identity through delayed parameter selection, smoothing/median choices,
pitch-epoch synthesis and retained output, separately from ordinary predictor
history. Immutable donor age must survive repeated repair and mixed output.

Require a concrete bounded-state propagation and clearing proof before choosing
metadata/API shape. Bounded storage does not imply bounded donor age: old donors
must expire under programme policy, not be refreshed when the bookkeeping window
moves. Conversely, after genuinely new parameters replace the failed-correction
donor and all dependent retained synthesized output drains, clear that recovery
classification at the proved boundary. Do not permanently taint all future
original units because normal filters retain history. If exact dependency/clearing
cannot be established, fail closed for the ambiguous output and report the
compatibility limitation rather than manufacturing a fresh receipt.

Required native/reference sequences: startup zero block; valid frames across
variable pitch epochs; isolated and arbitrarily repeated Hamming failure;
median/smoothing paths that select a repaired donor; partial output while donor
age expires; then sufficient valid input to prove exact bounded clearing. Assert
PCM and subsequent history unchanged, no receipt rejuvenation, no unbounded
metadata, and no permanent concealment label after proved donor replacement and
drain. Determine whether existing metadata can express the proven mapping before
proposing a native hook or provider API extension.

Ordinary predictive history remains compatible with original-unit classification,
as in the reviewed GSM/iLBC policy. Hamming correction that reconstructs coded
information is not automatically invented speech. A failed correction that copies
old parameters is different, but it may affect only part of a unit. Either prove
that the existing unit definition still permits the relevant correction as normal
codec reconstruction, with no false fresh authority, or add a native per-unit
observation/classification hook. Do not duplicate the stateful decoder, disable
error correction, or label all LPC10 output perpetually concealed.

## G.723 bounded classification and SID/repair semantics

Pinned FFmpeg n7.1.1 g723_1dec.c:182-211 has bounded packet-only forbidden
pitch/gain checks. At960-1068 an unpack failure can still return successful PCM,
using history-dependent recovery/CNG. A read-only equivalent parser may identify
those units before the existing decode, with no native modification. It must
mirror all rejection predicates and be tied to the actual native source/build.
Classify rejected units recovered; do not change their native result. SID and
untransmitted units need the common nonfresh CNG policy, not a false successful-
decode-is-original inference. Good subsequent units retain normal native history.

## Parallel audit implications: Dialogic, G729 and Opus

Read-only provider-evidence audit reports Dialogic's pinned Asterisk primitive
has no SID/DTX/erasure branch; ordinary prediction is not concealment. G7291.0.4
SID generates CNG, while ten-byte pitch-parity failure replaces pitch but retains
other genuine parameters. Whole-unit nonfresh classification there has the same
explicit compatibility limitation as mixed-band Speex. Opus1.5.2 short parsed
frame payloads can invoke PLC/DTX; internal CELT silence and nonfatal entropy
recovery require cause visibility. Successful return, encoder DTX disabled, and
final-range checks do not prove original output.

Keep classification reasons explicit in the design: packet-decoded, SID/CNG,
partial repair, whole-unit PLC and unknown. Preserve receipt identity separately
from authority eligibility. A side-effect-free parser may prove packet predicates,
not hidden stateful causes. Any native hook needs reviewed source/ABI/package
changes, never opaque-structure scraping, a second decode or behavior change.
The G729 dependency must remain the separately named pinned GPL2 bcg7291.0.4 DSO,
not a current GPL3 substitution. Parallel audit tag readings have not yet closed
archive-byte/runtime attestation; do not overstate those findings as binary proof.

## Required proof and licensing/exact-pin gates

- Audit exact codec candidate8596809 and IAX basece9fe2da, not released API
  assumptions. Sources: FFmpeg7.1.1, Speex1.2.1, SpanDSP0.0.6 as pinned in
  dependencies/codecs.json. Revalidate any native package patches and build flags;
  matching a version string is insufficient. Current tag-file readings do not
  themselves attest installed FFmpeg/Speex binaries.
- Before native hooks or copied parsing logic, review each source license and
  notices, dynamic-library boundary, package/SONAME consequences and exact source
  checksums. FFmpeg target remains LGPL-compatible without version3/nonfree;
  Speex BSD and SpanDSP LGPL obligations remain. No vendored duplicate decoder.
- All fixtures compare exact native PCM AND subsequent valid decoder history;
  preserve enhancement/correction. Mixed aggregates, malformed units, startup,
  DTX transitions, indefinite synthetic continuation and return to speech matter.
- Assert no synthetic onset, hold refresh or DTMF. Also assert real speech is not
  silently lost under the selected mixed-band/correction policy. Verify same
  command lookback/mute timeline and both ordinary/private courtesy behavior.
- If a supported independently concealed-high-band/fresh-low-band case is proved,
  add an indefinite continuation regression in BOTH fresh/concealed component
  directions; otherwise retain this as a conditional generic mixed-origin contract
  test, not a claim about Speex reachability. Fresh evidence from either direction
  must not renew the independently old contributor in the opposite direction. Prove programme contributor age continues to advance and its expiry
  is enforced separately from fresh analysis evidence and legitimate voice
  authority. Include a control using ordinary current-parameter reconstruction
  and filter history, which must not be falsely aged as retained PLC. Native audit
  must establish which case each fixture exercises, not infer it from PCM alone.
- Test preflight capacity without decoder advancement, one decode per unit,
  zero/partial credit, immutable receipts, bounded storage, reset/epoch/auth
  revocation and no output donation to replacement. No retry-only qualification.
- Review policy and any API extension independently before implementation. Exact
  source native amd64/arm64 cohort and full PR gates follow, not substitute for
  semantics. Physical listening/peer interoperability remain separate acceptance.

## Material decisions and estimate impact

1. First establish a supported reachable genuinely mixed-origin unit. Only then
   decide whether whole-unit nonfresh suppression is acceptable or a separately
   reviewed native evidence API is required; no such API is justified by ordinary
   Speex filter history or the retracted fresh-low/PLC-high example.
2. Approve the original-unit definition for normal correction/history, and decide
   how partial uncorrectable parameter substitution differs from whole-unit PLC.
   This must be consistent across LPC10, G729 and other codecs, not protocol names.
3. Approve nonfresh CNG/SID/DTX policy while preserving audible native output and
   bounded receipt/hold semantics; confirm low-rate actual speech remains eligible.
4. Require proven LPC10 unit mapping rather than assuming current-packet receipt;
   decide startup programme treatment when no legitimate predecessor exists.

G.723 parser/reference work appears about one engineering day. Speex/LPC10 source
and semantic proof needs roughly 1-2 days before reliable implementation sizing.
A separate native evidence API would add an estimated 2-4 engineering days across
provider, consumers, alignment tests and review. Therefore retain the prior
5.5-9-day overall IAX estimate only as conditional on no such extension; if required,
allow roughly 7.5-13 days, with uncertainty until the branch/receipt proofs settle.
These are planning ranges, not a promise or reason to delay independent RTP gates.
