# SIP iLBC30 and bounded receive design

This is the next isolated increment after Batch A runtime ed73d27, not a claim
that all sixteen ordinary IAX codecs or independent endpoints are qualified.
The requested parity includes the already released iLBC30 provider. Mode20
encoding and the unresolved legacy ADPCM/LPC10 mappings remain outside this increment.

## Negotiated behavior

Add iLBC/8000 with local dynamic PT100 and explicit mode=30, using existing
provider profile11, mode30, 240 PCM frames and50 encoded bytes per30ms TX packet.
RFC3952 section5 makes30 the common mode when a20 preference meets30; absent
mode retains30. Do not infer mode from packet length. Preserve exact directional
PT and offered-format validation. Unsupported modes do not become mode30.

A peer's ptime is a send preference, not our receive limit. Validate its numeric
syntax without forcing20. Accept harmless positive decimal receiver spellings
such as20.0 as an explicit extension beyond canonical RFC8866 real syntax; emit
canonical integer20/30. Validate every digit even when the whole part saturates
for comparison, and keep positivity separate so0.5 remains valid. A peer's maxptime limits our fixed TX duration; skip
an ineligible iLBC choice without rejecting an otherwise eligible20ms codec.
Our SDP advertises maxptime:30 as OUR receive ceiling, independently of the
peer's receive ceiling. Omit common ptime from a multi-duration offer; a selected
answer requests its selected packet duration. This30ms bound is an explicit
implementation limit, not a standards mandate or support for all packetizations.

Accept complete native coding units within30ms. GSM therefore remains one20ms
unit; iLBC30 remains one30ms unit and rejects60ms aggregation at this limit.
G711, G722, G726 and L16 accept smaller complete units up to30ms. Native framing
inspection occurs before SSRC latch, output copy or jitter admission. Only native
Malformed becomes InvalidPacket; impossible metadata/results and provider errors
remain terminal Codec failures. Damaged complete units still use decode_recovering.

## Ownership and resource bounds

Reuse the existing AudioCodec encoder/decoder, RTP endpoint, single source owner,
jitter and retained source-credit ledger. A call-local borrowed inspection
callback does not store another owner or queue. Decoder configuration and L16
scratch use complete-unit RX maxima; encoder configuration and timed egress
remain fixed per selected TX profile. RTP Audio carries inspected PCM span and
actual encoded length. Jitter receives only that payload prefix and decoding
uses its actual returned span/length. Original source receipt age, overlap/gap
credit, finite-source accounting, exact cancellation and WouldBlock identity stay.

At L16/16k the RX maximum is480 PCM frames/960 encoded bytes. The existing65
possible jitter payload slots plus PCM, receive and endian workspaces total at
most66,240 bytes, versus44,160 at20ms, a22,080-byte worst-case increment. Jitter
payload boxes retain actual admitted lengths rather than eagerly allocating that
maximum. Unchanged opaque codec contexts, TX storage, rings and metadata are not
included in this bound. No new ABI, provider release, dependency, queue or SRC.

## Sources and qualification

- RFC3952 section5: https://www.rfc-editor.org/rfc/rfc3952.html#section-5
- RFC3264 sections6.1/7: https://www.rfc-editor.org/rfc/rfc3264.html#section-6.1
- RFC8866 sections6.4/6.5/9: https://www.rfc-editor.org/rfc/rfc8866.html#section-6.5
- RFC3551 sections4.2/4.4: https://www.rfc-editor.org/rfc/rfc3551.html#section-4.2
- Released adapter83725b7a, rust/ilbc/src/lib.rs; existing client becbff939.

Independent source review supports this seam and complete-unit sizing. Actual
RED/GREEN, loopback wire, strict and final aggregate receipts are still required.
No node/live change or combined-artifact publication is authorized by this plan.
