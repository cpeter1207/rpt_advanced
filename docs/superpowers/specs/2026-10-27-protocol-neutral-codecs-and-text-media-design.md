# Protocol-neutral codecs and incoming text media

Date: 2026-10-27

Status: Approved by the owner on 2026-10-27, including the implementation
choices and bounded-resource defaults below. Implementation is pending. This
approval does not authorize a release, installation or live-node change.

## 1. Goal and scope

Provide replaceable codec adapters that any supported link protocol can use.
Expand standalone AllStarLink media beyond its current 8 kHz mu-law path while
preserving radio behavior, peer isolation and the existing audio architecture.

The overall inventory is all 19 Asterisk 20 IAX audio formats. The owner deferred
`siren7`, `siren14` and `g719`; they remain pending and unadvertised. The active
16 are `g723`, `gsm`, `ulaw`, `alaw`, `g726aal2`, `adpcm`, `slin`, `lpc10`,
`g729`, `speex`, `ilbc`, `g726`, `g722`, `slin16`, `speex16` and `opus`.
Recognition of a format ID is not functioning codec support.

Incoming human text is another codec input. Render it as speech only at the
local transmitter, mixed with live audio, including simultaneous audio from
the text sender. Forward eligible messages to other peers as text only when
the protocols support forwarding. Each peer decides whether/how to render it.
This applies to EchoLink and every other present or future text-capable
protocol. It does not authorize implementing a complete new link protocol now.

Use released GPLv2-compatible implementations wherever suitable. Original
implementations are allowed only for trivial gaps without a suitable compatible
released implementation. Do not write complex codecs, acquire commercial
licenses, relicense the product, or revive the three deferred formats.

Non-goals: SIP/autopatch, a new rpt_advanced wire protocol, full EchoLink
implementation, outgoing text composition, speech-to-text, chat UI, recording
text messages, new hardware support, new DSP filters or gain changes, and other
wishlist work. Ordinary audio from peers must continue even if text fails.

## 2. Architectural constraints and current gaps

Apply ADRs 0002/0003, 0005, 0007, 0018/0020/0021/0022, 0025/0026/0027,
0029, 0035/0036/0037 and 0040. In particular:

- Controller/radio PCM stays normalized mono `f32`; native radio processing
  stays 48 kHz. Decoders return their actual source rate, not resampled PCM.
- The released inbound ring owns conversion, clock recovery and selected PLC.
  Egress uses the existing shared conversion adapter before encoding. Do not
  add codec-owned resamplers, a new PLC, or a transmit-mix ring.
- Audio callbacks remain bounded, lock-free and allocation-free. No packet
  parsing, codec processing, synthesis, loading, logging or subprocess work
  enters them. Existing per-peer Tokio owners retain serialization.
- Protocol, network, codec, packet-jitter and controller policy stay distinct.
  No Asterisk or ASL3 runtime dependency enters standalone codec providers.
- Replace DSOs only at controlled reload/restart after quiescence. Ordinary
  configuration reload must not reload executable code or free live contexts.

The inspected implementation is not codec-neutral yet:

| Existing source | Gap to remove |
| --- | --- |
| `librptadviax2/src/codec.rs`, `session.rs`, `client.rs` | Embedded mu-law implementation, `u32` format masks, hardcoded offer/selection/rate and full-voice subclass |
| `librptadviax2/src/playout.rs`, `jitter.rs` | Encoded byte length treated as sample duration; encoded bytes trimmed by sample offset; 8 kHz timing constants |
| `librptadviax2/include/rptadv_iax2_client.h` | `prepare_ulaw` is stateless, lacks a peer handle and does not return encoded length |
| `rust/standalone/src/iax.rs`, `host_services.rs`; `rust/product/src/link/outbound.rs` | Prepared-media contract assumes encoded bytes equal PCM samples |
| `rust/product/src/media/worker.rs` | One serialized telemetry scheduler with politeness/Morse behavior; unsuitable for concurrent incoming text |
| `rust/core/src/link/protocol.rs`, product peer session | Recognizes ASL control text, not a general human-message classification |

Preserve unrelated working-tree changes, including spoken-timeout recovery.
Do not mix those changes into codec implementation commits.

## 3. Approaches

The owner selected versioned, protocol-neutral codec adapters. That is the
approach in this specification. It makes implementations replaceable without
changing protocol or controller code and reuses the project's DSO lifecycle.

Rejected alternatives are embedding more codecs in IAX2, routing text through
the telemetry queue, and sending locally synthesized text as peer audio. Each
violates an explicit requirement. Rewriting available codec algorithms adds
maintenance without meeting an unmet need and is also rejected.

Approved implementation: use one small common codec contract and separate
provider DSOs for independently replaceable implementations. One provider may
support related profiles; do not duplicate a library just because two protocols
or two packing variants use it. Reuse the existing speech adapter as the
initial text-decoding backend rather than create another Piper integration.
Do not expand the FFmpeg *graph* ABI into a catch-all codec API.

## 4. Codec profiles and evidence

Wire mappings belong to protocol adapters, not codec providers. The following
IAX profiles guide initial provider selection. Outgoing 20 ms packetization is
the approved default for stream codecs, not a requirement to force every codec
into a 20 ms frame. Receive must handle valid peer aggregation and durations.

| IAX name | PCM rate | Required framing/profile | Candidate released implementation |
| --- | ---: | --- | --- |
| `ulaw` | 8 kHz | Raw G.711 mu-law octets | Move the existing released G.711 backend behind a provider |
| `alaw` | 8 kHz | Raw G.711 A-law octets | Compatible released G.711 backend |
| `gsm` | 8 kHz | GSM Full Rate, 160 samples/33 bytes; not WAV49 | libgsm |
| `g723` | 8 kHz | G.723.1, 240 samples/30 ms; inspect speech/SID frame lengths | GPLv2-compatible FFmpeg build; mode limitation below |
| `g726aal2` | 8 kHz | 32 kb/s, AAL2 packing; verify high-nibble-first Asterisk payload | Compatible released G.726 implementation |
| `adpcm` | 8 kHz | Asterisk's Dialogic ADPCM payload, high nibble first; not arbitrary IMA/WAV | Released Asterisk codec implementation, isolated from its host wrapper |
| `slin` | 8 kHz | PCM16; pinned Asterisk IAX uses network byte order | Trivial PCM serialization at the protocol boundary |
| `lpc10` | 8 kHz | 180 samples/7 bytes, 22.5 ms; retain rational timing | Compatible released LPC10 library |
| `g729` | 8 kHz | G.729A, 80 samples/10 bytes; accept validated SID framing | Reviewed GPLv2-compatible bcg729 release |
| `speex` | 8 kHz | Raw narrowband Speex frames; no Ogg wrapper | libspeex |
| `ilbc` | 8 kHz | Both 20 ms/38 bytes and 30 ms/50 bytes; explicit mode | Reviewed GPLv2-compatible libilbc release |
| `g726` | 8 kHz | 32 kb/s, RFC packing; verify low-nibble-first Asterisk payload | Same released G.726 backend, distinct packing profile |
| `g722` | 16 kHz | G.722 at 64 kb/s; PCM rate is not RTP's historical 8 kHz clock | Compatible released G.722 implementation |
| `slin16` | 16 kHz | PCM16; pinned Asterisk IAX copies host-endian data | Trivial serialization using the verified IAX profile |
| `speex16` | 16 kHz | Raw wideband Speex frames | libspeex |
| `opus` | 48 kHz output | Raw Opus packets, mono output; parse actual packet duration | libopus |

These are candidates, not a claim that the node's current packages provide all
of them. Audit exact source releases, dependencies and binary build flags
before selecting a provider. Do not vendor third-party code into the product
or IAX library. If a needed released implementation lacks a usable system DSO,
package that implementation separately as a shared library. Reusing released
Asterisk codec source does not permit copying `app_rpt` or introducing an
Asterisk dependency into the provider.

Specific qualifications:

- bcg729 1.0.4 is a GPLv2-compatible candidate; the current GPLv3 package cannot
  be substituted silently. Likewise review libilbc's transitive dependencies.
  Reuse a compatible released version rather than port a complex algorithm.
- FFmpeg's inspected G.723.1 encoder supports 6.3 kb/s, not 5.3 kb/s. Record
  `encode={6300}`, `decode={5300,6300}` if that backend is selected. Leave
  missing 5.3 kb/s encoding pending; do not call that mode implemented. Validate
  any use of format-level negotiation when the peer cannot express the mode.
- Do not infer iLBC's IAX mode from an arbitrary packet size or inconsistent
  default metadata. Verify actual peer behavior: Asterisk's inspected
  translator defaults to 30 ms, while HamVoIP documents 20 ms. Preserve both
  profiles without inventing an unsupported mode-negotiation extension.
- SLIN byte order, LPC10 timing and G.726 packing require independent fixtures
  from the pinned implementation. The RFC's old codec table alone is not an
  exact Asterisk 20 behavior contract. Do not silently make SLIN16 match SLIN8.
- Approved Opus transmit policy is mono, 48 kHz PCM and 20 ms packets. Derive
  received duration from a valid packet, not a constant. Other supported Opus
  packet durations must not be mistaken for loss or malformed voice.
- Disable optional encoder DTX initially where supported to preserve existing
  activity semantics; receive valid silence/SID frames according to the codec
  profile, without creating a second PLC or treating them as a hangup.

## 5. Replaceable provider contract

Each provider exposes a sized, versioned C-compatible descriptor with a
capability identity, profile discovery, opaque context lifecycle and typed
operations. Internal callers use a neutral Rust port over that table.

Audio provider operations cover:

1. Profile inspection: neutral codec/profile ID, PCM rate/channels, valid frame
   units, encode/decode directions, sample/byte bounds and codec lookahead.
2. Independent encoder/decoder construction, reset and destruction per peer
   and direction. No shared mutable codec state or global media lock.
3. Payload validation/sample-span inspection without consuming decode state.
4. Decode a whole encoded frame/packet into caller-owned normalized `f32`.
5. Encode with explicit consumed PCM frames, produced bytes and represented
   sample span. `need more input`, `output full`, `malformed`, `unsupported`
   and `complete` are distinct; success with no output is not a voice packet.
6. Explicit finite flush/reset behavior, including delayed codec samples.

Keep codec frame accumulation with its encoder owner. Do not leak native
library types, IAX format bits, socket handles or controller policy through
the contract. Validate sizes and all buffer/count arithmetic at the boundary.

The text codec is a stream-producing decode capability over validated human
text. It may reuse the speech provider's existing open/read/cancel contract;
do not force blocking synthesis into an audio packet's decode operation or add
speech-to-text merely to make the API symmetric. It is not an extra IAX audio
format bit. Its consumer is the product's local text-media source, not IAX.

The selected composition maps each neutral profile to exactly one validated
provider. Missing required providers fail preparation; never silently reduce
a selected composition. Deliberately unselected/deferred profiles are absent
from offers. Existing mandatory-manifest and controlled-replacement rules apply.

## 6. IAX integration

Widen offered/selected format state to 64 bits end to end. Reuse the existing
full-frame subclass compressor, which already handles high-bit single formats.
Read/write legacy and extended `FORMAT`/`CAPABILITY` information elements using
Asterisk's precedence/version rules. Speex16 and Opus cannot fit in `u32`.
Keep all IAX identifiers in the protocol implementation.

Build offers from verified bidirectional profiles and retain mu-law. The approved
initial preference preserves current behavior by keeping mu-law first; other
formats remain available when selected by the peer or the existing host codec
policy. Do not change codec preference based only on sample rate.

Replace the stateless prepared-mu-law path with peer-scoped, codec-neutral
preparation and explicit byte/sample metadata. Update current hosts and
consumers together. This is required by framed/stateful codecs, not a reason
to retain a parallel mu-law implementation. Reuse the codec contract in both
hosts where practical; the deprecated Asterisk adapter may still use public
Asterisk codec services behind its existing boundary.

On ingress, obtain each packet's decoded sample span separately from its byte
length, then admit the encoded packet to the existing jitter adapter. Decode
complete packets before trimming overlapped PCM. Never slice a compressed
payload with a sample offset. Jitter timestamp units and gap counts must use
the selected codec's source sample clock; keep 160/180/500 ms delay policy
unchanged and convert it to samples with checked arithmetic. The shared PCM
ring receives ordered real PCM and explicit missing spans, and alone owns PLC.

On egress, retain the existing native-to-codec converter and bounded per-peer
owner. Accumulate actual converter output until complete codec units exist.
Preserve source-span identity, original media age, codec delay and fractional
sample timing when creating IAX millisecond timestamps. LPC10 must not acquire
a growing half-millisecond error per frame. Keep bytes, PCM frames and duration
as separate quantities throughout the product and host-services ABI.

Retain at most one prepared media packet across socket backpressure. Never
re-encode a retry or refresh its age. Preserve current expiry, reliability,
unkey/tail-drain and sequence-recovery behavior. Codec processing is bounded
per owner turn; synthesis never runs on that owner. No global codec lock or
new thread per connection is introduced.

Derive packet/scratch/assembly bounds from the verified profiles and permitted
aggregation, not the current 4096-sample or 1500-byte assumptions. Read complete
datagrams and detect truncation explicitly. Valid codec frame sizes, including
empty/non-audio cases, must not cause a generic connection teardown. Unsupported
offers fail negotiation cleanly; malformed media follows tested error policy.

## 7. Incoming text: two independent outputs

Protocol adapters classify payloads into control, human message, or unknown
before forwarding or synthesis. ASL topology/key/DTMF/disconnect/authentication
text is never spoken. Unknown IAX text is not automatically chat. Add only
human-message forms supported by pinned protocol source/fixtures; do not invent
new wire messages or new text capabilities for old peers.

A validated human envelope retains protocol/source identity, authenticated
ingress connection, intended audience and generation. It has two independent
admission paths:

| Path | Result | Failure behavior |
| --- | --- | --- |
| Text forwarding | Adapt the human message to each eligible destination's supported text representation | Reject/count only that destination; never substitute synthesized audio |
| Local rendition | Text codec streams speech into a local-only media source | Skip/log local failure; do not suppress successful text forwarding |

Do not echo a message to its sender or bypass existing authorization, destination
and topology rules. Use protocol-supported forwarding/source information; do
not introduce a new cross-protocol message-ID extension. Local forwarding
handles one admitted message once per eligible egress and respects the
protocol's existing duplicate/loop rules. No claim of global exactly-once
delivery is made for protocols that lack such a mechanism.

Text-derived PCM is **never** included in the program loopback, peer codec
input, command-response/broadcast audio, parrot capture or parrot replay.
No local synthesis event may manufacture a peer radio-key transmission. Peers
receive text only, and independently decide whether/how to render it.

## 8. Local speech production and mixing

Use the existing speech adapter and source-rate PCM stream primitives.
Preserve the node's configured speech voice/model, speed and level; do not
hardcode levels or apply a second gain. Treat received text as literal data,
not configuration, shell arguments, markup, templates or controller commands.

Use a dedicated finite-media ring for each admitted text origin, separate from
that origin's live-audio ring and the serialized telemetry ring. Exactly one
text producer owns it; transmit owns its only consumer. This is necessary for
same-peer live audio and speech to coexist without two SPSC writers. Open the
released ring with finite-media policy: no PLC or added clock target, with
source-rate conversion once in that ring. Use accepted sample budgets, explicit
EOF, filter-delay drain and off-audio endpoint retirement from existing media
code. Empty occupancy is neither EOF nor permission to replay old samples.

Synthesis runs on a bounded non-audio blocking executor, not a Tokio peer task,
station control, or the telemetry worker. Reuse the existing preparer and
factor finite-stream lifecycle helpers instead of copying the telemetry
scheduler. One origin's jobs retain order; different origins may make progress
independently. Bounded resource admission may reject new local renditions but
must not stall peer audio, text forwarding or real-time callbacks.

Transmit consumes ready text PCM only into the local voice-band mix. Mix it
with local receive and all otherwise eligible live peer audio, including audio
from the text sender. New receive activity neither postpones it nor changes it
to Morse. Normal local duplex, timeout, hardware-inhibit and RF-safety rules
still apply. Awaiting synthesis alone must not key or hold PTT. Text media
contributes local activity while played, without rewriting remote wire key
state. Continue existing transmit processing; no new filtering or normalization.

If synthesis fails before PCM, skip the local rendition and log an error. If
it fails after audible PCM, stop the remaining rendition and log; already-played
audio cannot be undone. Never use Morse, restart another speech attempt
automatically, stop other audio, or cancel otherwise valid text forwarding.
Successful EOF drains the finite stream, then releases its local activity.

### Approved bounded-resource defaults

The owner approved these design choices with this specification:

- Admit at most 4096 UTF-8 bytes per local rendition. Reject/count an oversized
  local rendition rather than truncate it; otherwise valid text may still be
  forwarded within the destination protocol's own limits. Protocol adapters
  validate source encoding/syntax before classification. Do not add
  fragmentation to a protocol that lacks it.
- Keep at most four local rendition jobs per origin, including an active one,
  and 64 admitted local jobs per node. Reject/count the newest local job when
  full. Text forwarding has separate existing bounded egress admission.
- Use two synthesis workers shared by the product, with one job per origin at
  a time and fair admission across origins. Do not create a worker per peer.
  This limits concurrent synthesis, not the number of connected peers.
- Reuse the configured speech subprocess budget. Make stdin submission,
  stream reads, ring-full waiting and cancellation respect that budget;
  today's blocking stdin `write_all` cannot establish bounded cancellation.

Do not add operator configuration knobs solely for these initial bounds.
Document/count admission failures. Implementation may choose a simpler
equivalent bounded executor, but may not replace these ownership guarantees
with an unbounded queue or a global lock across external work.

## 9. Lifetime, errors and observation

Create providers, codec state, fixed buffers and text-ring endpoints off audio.
Tag every message, job, prepared packet and completion with its owning station
and connection generation. Disconnect/replacement cancels pending local speech
from that origin; a late result cannot revive it. Ordinary reload uses ADR
0026's prepared-generation handoff, cancellation and retirement. Do not replay
forwarded text after reload. Retain DSOs until every context/job/callback stops.

Report malformed/unsupported codec media, encoded bytes versus sample spans,
decode/encode failures, codec-buffer occupancy, text admission/rejection,
forwarding successes/failures, synthesis failures, cancellation and text-ring
occupancy through existing diagnostics. Keep error logs bounded/rate-limited
and metadata-only: no credentials, authentication payloads or full human text.
Do not add REST/WebSocket interfaces in this work.

Provider failure must not silently select an incompatible implementation,
corrupt another peer, or become an unexplained generic hangup. Preserve the
first protocol/I/O cause and established retry/termination semantics.

## 10. Packaging and compatibility

Keep codec adapters in protocol-neutral production component(s), not inside
the IAX protocol repository. Distribute providers as independently replaceable,
versioned shared objects with runtime/development packages as needed. Reuse
the existing companion/shared-library workflows, signed package repository and
image-building process. Do not create one duplicate provider per consumer.

Only providers link their external codec libraries. Rust leaf dependencies
may live inside their owning provider under ADR 0018; external C libraries and
separately released project components remain dynamic dependencies. Verify
actual linked dependencies and license/build configuration, not just headers.

Reuse published descriptor extension points if adequate. The current
stateless prepared-mu-law table cannot express stateful encoding and frame
metadata, so a coordinated codec-neutral ABI revision is expected. Specify its
descriptor versions, SONAME/package handling and exact consumer dependency
checks in the implementation plan. Update standalone, product, IAX provider,
headers, test fixtures and quality-image ABI checks together. Old incompatible
combinations must fail safely; do not retain obsolete alpha code under ADR 0040.

No release/deployment follows from this spec. Approved node validation later
must use an ABI-matched set and preserve configuration and existing links unless
the owner explicitly authorizes changing them.

## 11. Verification and completion

Use TDD and focused tests first; record progress in ignored `.work/` reports.
Before claiming each active format, require independent known payload fixtures,
full-duplex interoperability and declared profile evidence, not only a
self-round-trip test. Test:

1. Every active format's encode/decode, rates, state isolation, boundaries,
   malformed input, capacity failures, reset and flush behavior.
2. G.726 packing, Dialogic ADPCM, both SLIN byte orders, rational LPC10 timing,
   both iLBC modes, supported G.723 modes and real Opus packet duration.
3. Legacy/extended IAX offers, preference, fallback to mu-law, high-bit formats,
   incompatible offers and the absence of deferred formats.
4. Jitter reordering/overlap/gaps at each source rate. PLC occurs in order only
   in the shared ring, with no encoded-byte trimming or duplicated concealment.
5. Encoder assembly across arbitrary native callback partitions; no empty voice
   on no-output conversion, timestamp drift, duplicate re-encoding on retry,
   or loss of buffered tails on unkey.
6. Real dynamic provider replacement at controlled restart, wrong/missing ABI,
   package combinations and Asterisk-independent standalone loading.
7. Text/same-peer audio/other-peer audio overlap. Local RF contains all eligible
   contributions; peer egress and parrot contain zero text-derived PCM.
8. Supported text forwarding with local synthesis disabled/failed/full; no
   reflection, unsupported-destination audio substitution or audible control text.
9. Synthesis timeout/stdin stall, full rings, queue saturation, fair cross-origin
   progress, source disconnect, reload and stale completion. Other audio and
   eligible forwarding continue, and PTT cannot stick after speech ends/fails.
10. Existing identifiers, courtesy tones, telemetry, timeout, DTMF, gains,
    source routing and peer scaling regressions remain unchanged.

Run the established complete GitHub PR gate at the end: warning-free applicable
format/lint/static/developer-doc checks once, native Debian 13 amd64/arm64 tests
and package/install checks in parallel, and 100% production line/branch coverage
on amd64 only. Push checks and release checks retain their existing narrower
scope. Clean labeled test containers deterministically; no new flaky performance
thresholds or non-production coverage requirements.

Complete the active work only when all 16 formats meet their declared profiles
and the shared text-codec path meets its routing/lifecycle tests. Do not claim
complete EchoLink interoperability until its separate protocol adapter exists.
The three deferred formats and any unsupported codec modes remain visibly
pending. Update developer documentation before implementation commits and
operator documentation immediately before a PR, as required by `AGENTS.md`.

## 12. Traceability and source baseline

Owner decisions are recorded in `WISHLIST.md`, ADRs 0007/0022, and sibling
`librptadviax2/requirements.md` IA-018 through IA-023. This specification combines
those decisions with the approved defaults above. It does not mark them
implemented or replace the implementation plan.

Source baseline checked on 2026-10-27:

- [Asterisk 20 format IDs](https://github.com/asterisk/asterisk/blob/961c77eb2c3840e6a3340344e9a15a8fc58194e5/include/asterisk/format_compatibility.h),
  [IAX negotiation](https://github.com/asterisk/asterisk/blob/961c77eb2c3840e6a3340344e9a15a8fc58194e5/channels/chan_iax2.c),
  [wire wrapping](https://github.com/asterisk/asterisk/blob/961c77eb2c3840e6a3340344e9a15a8fc58194e5/channels/iax2/parser.c),
  and codec translators under that same pinned revision. Consult actual
  translators as well as `main/codec_builtin.c`; their metadata can disagree.
- [ASL3 app_rpt source](https://github.com/AllStarLink/app_rpt/tree/b6cc1cf175c8652f06d45e56dd9ff486abd4557b),
  [codec configuration manual](https://allstarlink.github.io/config/iax_conf/),
  and [ASL text protocol](https://allstarlink.github.io/developers/iaxtext/).
  Review source to settle details the manual does not establish.
- [HamVoIP codec notes](https://hamvoip.org/) for its documented iLBC profile;
  [FFmpeg G.723 encoder restriction](https://github.com/FFmpeg/FFmpeg/blob/n7.1.1/libavcodec/g723_1enc.c);
  [bcg729 1.0.4](https://github.com/BelledonneCommunications/bcg729/tree/1.0.4);
  [libilbc 2.0.2](https://github.com/TimothyGu/libilbc/tree/v2.0.2);
  [libopus packet API](https://opus-codec.org/docs/opus_api-1.6/group__opus__decoder.html).

Before implementation, freeze each selected backend's exact released revision,
license notices and independent test vectors in its normal dependency inputs.
Public source availability alone is not a compatible redistribution grant.
