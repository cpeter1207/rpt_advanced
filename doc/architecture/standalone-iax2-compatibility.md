# Standalone IAX2 compatibility source matrix

This is an implementation reference for the standalone IAX2 adapter. It records
the source of interoperability requirements; it is not an operator guide. The
ASL manual's IAX text page explicitly says that it is copied, incomplete, and
may be incorrect, so source behavior takes precedence when the two disagree.

## Source revisions

- ASL3 manual: current published page, accessed 2026-10-06,
  [app_rpt IAX text protocol](https://allstarlink.github.io/developers/iaxtext/).
- ASL `app_rpt`: current `master` head pinned at
  [`5695d4415ed3627409d513c3ce6258623bfee4f5`](https://github.com/AllStarLink/app_rpt/tree/5695d4415ed3627409d513c3ce6258623bfee4f5).
- Asterisk: current Asterisk 22 branch head pinned at
  [`56257f5efdf482c433a204b90c08de0f881b90b0`](https://github.com/asterisk/asterisk/tree/56257f5efdf482c433a204b90c08de0f881b90b0), the major version used by ASL3's current build configuration. The builder derives its selected release from the checked-out Asterisk source; see [`build-asl3`](https://github.com/AllStarLink/asl3-asterisk/blob/develop/build-asl3).
- Base wire format: [RFC 5456, IAX2](https://www.rfc-editor.org/rfc/rfc5456).

## Observed compatibility behavior

| Surface | Observed behavior | Evidence |
| --- | --- | --- |
| Base IAX2 packet types and frame fields | Asterisk's IAX2 implementation defines the command subclasses (including `NEW`, `PING`, `PONG`, `ACK`, `HANGUP`, `REJECT`, `ACCEPT`, `AUTHREQ`, and `AUTHREP`), full-frame flags, and call-token state. RFC 5456 §8.1.1 specifies the 12-octet full-frame header: `F` is bit 15 of the source-call word, `R` is bit 15 of the destination-call word, and `C` is bit 7 of the subclass octet; `C=1` means the remaining seven bits encode a power-of-two exponent, not a command marker. The Rust protocol layer preserves that wire distinction independently of socket ownership. | [Asterisk `iax2.h`](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/iax2/include/iax2.h), [`IAX_COMMAND_*`](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/iax2/include/iax2.h#L871-L920), [call-token state in `chan_iax2.c`](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c#L3135-L3158); [RFC 5456 §8.1.1](https://www.rfc-editor.org/rfc/rfc5456#section-8.1.1) |
| Information elements | IEs are concatenated in the full-frame data, each encoded as a one-octet identifier, one-octet data length, then that many bytes. Preserve unknown IEs; interpretation belongs to session negotiation and is not part of the generic parser. The `librptadviax2` parser borrows IE data from the packet and rejects incomplete headers or bodies. | [RFC 5456 §8.6](https://www.rfc-editor.org/rfc/rfc5456#section-8.6); [pinned Asterisk IAX2 header](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/iax2/include/iax2.h) |
| Text frames | RFC 5456 defines text frames as full frames with type 7, subclass 0, and UTF-8 payload. The parser validates those wire constraints but returns text unchanged; ASL message interpretation remains in the application layer. The `key-query.hex` fixture pins the exact frame bytes and `K? * <node> 0 0` application text shape referenced by the ASL3 source matrix. | [RFC 5456 §8.2.7](https://www.rfc-editor.org/rfc/rfc5456#section-8.2.7); [ASL key-query manual and source](https://allstarlink.github.io/developers/iaxtext/#k-key), [pinned `rpt_link.c`](https://github.com/AllStarLink/app_rpt/blob/5695d4415ed3627409d513c3ce6258623bfee4f5/apps/app_rpt/rpt_link.c#L388-L410) |
| Mini-frame media | Mini frames have a four-octet header: a nonzero 15-bit source call number and a 16-bit timestamp, then codec bytes; frame type and codec are implicit from the call's negotiated state. A zero first word is reserved for meta frames and must not be parsed as a mini frame. The protocol parser borrows media bytes and leaves decoding to codec adapters. | [RFC 5456 §8.1.2](https://www.rfc-editor.org/rfc/rfc5456#section-8.1.2); [pinned Asterisk mini-frame structure](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/iax2/include/iax2.h#L1210-L1223) |
| Connection setup and authentication | IAX2's call setup/authentication is an Asterisk channel-driver responsibility today. The standalone network adapter will own UDP and retries; the protocol crate will serialize/parse frames and authentication IEs; credentials and policy remain product configuration. Tests must cover the actual ASL3 call-token/auth exchange captured from the pinned Asterisk source, not infer it from a successful socket exchange. | [Asterisk `chan_iax2.c`](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c); [Asterisk IAX2 header](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/iax2/include/iax2.h); [ASL3 build source selection](https://github.com/AllStarLink/asl3-asterisk/blob/develop/build-asl3) |
| Standard AllStarLink dial identity | An extnode destination such as `radio@host/node` is parsed into IAX username `radio` and called number `node`; the caller's node number is carried separately as `CALLING_NUMBER`. The IAX client must not replace the `radio` username with its own node number or omit it. The ULAW client emits these standard IEs and has a loopback regression. | [Pinned outgoing dial-string parsing](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c#L4828-L4851), [pinned NEW IE construction](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c#L4975-L5008) |
| Call-token retry | Asterisk appends an empty `CALLTOKEN` IE (54) as the final IE of an outbound `NEW`. The library builds this initial NEW with destination and sequence state zero, then a returned `CALLTOKEN` command (40) carries a token; it validates the exchange, preserves prior IEs, builds the retry with caller-supplied timestamp, and resets sequence numbers. Network/session scheduling remains outside these helpers. Test tokens use a deterministic fixture string because real token hashes depend on server state. | [Pinned IAX2 IE/command definitions](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/iax2/include/iax2.h#L871-L920), [empty IE and NEW construction](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c#L4615-L4623), [retry replacement and sequence reset](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c#L4624-L4697) |
| Inbound call-token gate | Before allocating a call, Asterisk challenges an empty CALLTOKEN IE with command 40, using local call 1, the caller's source call number as destination, echoed timestamp, outgoing sequence 0, and incoming sequence `request ISeqno + 1`. The token is `timestamp?sha1` over the formatted source socket address, timestamp, and process-private integer. A valid token proceeds; absent, malformed, source-mismatched, or expired tokens are rejected when call tokens are required. The library implements this stateless decision and response construction; it does not yet create inbound call state. | [Pinned `send_apathetic_reply`](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c#L4589-L4613), [call-token request handling](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c#L4721-L4812) |
| Incoming `radio` call acceptance | After the listener accepts a valid token and product access policy, `NEW` must target the configured node, carry `USERNAME=radio`, and identify a numeric caller node. With u-law in `CAPABILITY`, Asterisk answers with `ACCEPT`, legacy `FORMAT=0x00000004`, and version-0 `FORMAT2` containing the same 64-bit format mask. The pure library helper emits this response but does not allocate an inbound session or bind a socket. | [Pinned inbound `NEW` admission and ACCEPT construction](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c#L10471-L10476), [IAX `FORMAT2` IE definition](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/iax2/include/iax2.h#L177-L178), [pinned FORMAT2 parser](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/iax2/parser.c#L716-L724) |
| MD5 authentication response | Asterisk's AUTHREQ supplies a two-byte `AUTHMETHODS` bit field and a challenge string. When MD5 is selected, its AUTHREP contains `MD5_RESULT` IE 16 as 32 lowercase hexadecimal characters encoding MD5 over challenge bytes immediately followed by the secret. The component parses these AUTHREQ IEs, constructs the response packet, and verifies an incoming result against semicolon-separated configured secrets, comparing hexadecimal bytes case-insensitively. This supports outbound clients and generic IAX users. The shipped ASL3 `[radio]` user does not configure `auth=md5` or a secret; inbound AllStar `radio` calls instead use CALLTOKEN and configured node/source validation. | [Pinned AUTHREQ challenge construction](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c#L7754-L7762), [AUTHMETHODS/CHALLENGE IE wire definitions](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/iax2/include/iax2.h#L871-L920), [pinned AUTHMETHODS length and byte-order parser](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/iax2/parser.c#L3827-L3838), [pinned MD5 digest and AUTHREP IE construction](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c#L8060-L8078), [pinned inbound secret alternatives and comparison](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/channels/chan_iax2.c#L7833-L7854), [ASL3 radio-user configuration](https://github.com/AllStarLink/app_rpt/blob/master/configs/rpt/iax.conf) |
| ASL text framing | app_rpt extends IAX2 with text frames. The manual documents `T`, `L`, `K`, `K?`, `M`, and legacy/behavioral signals, while warning that its page is incomplete and may be wrong. Parse message prefixes conservatively and preserve unrecognized text for forward-compatible handling; implement only behavior confirmed in the pinned source. | [Manual caveat and text formats](https://allstarlink.github.io/developers/iaxtext/#t-telemetry); [pinned `app_rpt.c` text handler](https://github.com/AllStarLink/app_rpt/blob/5695d4415ed3627409d513c3ce6258623bfee4f5/apps/app_rpt.c#L1816-L1851) |
| Topology | `L ` advertises link modes and node numbers; app_rpt stores the peer's received topology for link/status decisions. Validate exact ordering, self-node handling, and mode characters against source fixtures rather than relying on the manual's example. | [Manual `L` format](https://allstarlink.github.io/developers/iaxtext/#l-linked); [pinned `app_rpt.c`](https://github.com/AllStarLink/app_rpt/blob/5695d4415ed3627409d513c3ce6258623bfee4f5/apps/app_rpt.c#L1846-L1851); [pinned `rpt_link.c`](https://github.com/AllStarLink/app_rpt/blob/5695d4415ed3627409d513c3ce6258623bfee4f5/apps/app_rpt/rpt_link.c#L425-L547) |
| Key state | `K` carries node key state; `K?` requests key status. app_rpt emits `K? * <node> 0 0` and broadcasts it to connected links. Preserve the exact field order and response semantics in captured fixtures. | [Manual `K` and `K?`](https://allstarlink.github.io/developers/iaxtext/#k-key); [pinned `rpt_link.c`](https://github.com/AllStarLink/app_rpt/blob/5695d4415ed3627409d513c3ce6258623bfee4f5/apps/app_rpt/rpt_link.c#L388-L410) |
| Remote DTMF | Each digit uses the app_rpt text envelope `D <command-node> <source-node> <index> <digit>`, not an IAX DTMF frame. `command-node` selects one directly connected peer; otherwise app_rpt broadcasts the text. The standalone peer sends an addressed text per digit and accepts only messages addressed to its local node from that direct peer. Product command policy remains separate from transport. | [Manual miscellaneous signaling](https://allstarlink.github.io/developers/iaxtext/#m-message); [pinned `rpt_link.c` send and direct-peer/broadcast routing](https://github.com/AllStarLink/app_rpt/blob/5695d4415ed3627409d513c3ce6258623bfee4f5/apps/app_rpt/rpt_link.c#L347-L390) |
| New-key handshake | `!NEWKEY!` enables explicit radio-key control and is echoed once; `!NEWKEY1!` disables that control and unkeys immediately. Before either message arrives, the peer session waits two seconds, then permits radio-key events as the compatibility fallback. The client library maps IAX control subclasses 12/13 to zero-payload radio-key/unkey events; the product session applies this negotiated policy and always accepts unkey. This implements the inbound key-state gate, but does not claim complete loss/reordering equivalence for the separate text handshake. | [Manual signals](https://allstarlink.github.io/developers/iaxtext/#miscellaneous-signaling); [pinned `app_rpt.c` handshake explanation and handler](https://github.com/AllStarLink/app_rpt/blob/5695d4415ed3627409d513c3ce6258623bfee4f5/apps/app_rpt.c#L3306-L3365), [receive handling](https://github.com/AllStarLink/app_rpt/blob/5695d4415ed3627409d513c3ce6258623bfee4f5/apps/app_rpt.c#L2278-L2297) |
| Disconnect | Explicit disconnect sends `!!DISCONNECT!!` as text and app_rpt flushes queued text before hanging up. Treat explicit application disconnect and transport loss as distinct events. | [Manual disconnect token](https://allstarlink.github.io/developers/iaxtext/#disconnect); [pinned `rpt_link.c`](https://github.com/AllStarLink/app_rpt/blob/5695d4415ed3627409d513c3ce6258623bfee4f5/apps/app_rpt/rpt_link.c#L221-L241); [pinned `app_rpt.c`](https://github.com/AllStarLink/app_rpt/blob/5695d4415ed3627409d513c3ce6258623bfee4f5/apps/app_rpt.c#L1816-L1821) |
| Codec boundary | Codecs remain replaceable adapters backed by released libraries. The IAX2 protocol only maps negotiated format IDs and media payloads; it does not encode or decode audio. The first adapter is G.711 μ-law, using RFC 5456 format bit `0x00000004` at 8 kHz. Full format preference and interoperability policy must still be derived from ASL3 configuration/source and fixtures. | [Pinned Asterisk IAX codec preference configuration](https://github.com/asterisk/asterisk/blob/56257f5efdf482c433a204b90c08de0f881b90b0/configs/samples/iax.conf.sample); [RFC 5456 §8.7 format values](https://www.rfc-editor.org/rfc/rfc5456#section-8.7) |

## Fixture rule

Pinned Asterisk's `compress_subclass()` keeps values below `0x80` plain,
exponent-encodes larger single-bit values, and its `uncompress_subclass()`
decodes `0xff` as `-1`. The library follows this canonical encoding, so
`IAX_COMMAND_NEW=1` remains wire byte `0x01` rather than an equivalent but
noncanonical exponent form.

Every claimed wire detail must be tied to a pinned source revision and an
independent expected byte/text fixture. The call-token fixture uses a stable
synthetic hash rather than copying a live server's address-bound token. This
matrix establishes source pointers; the implemented primitives and μ-law media
adapter do not claim complete ASL3 interoperability. Before extending session,
control, or media behavior, extract representative outbound and inbound
fixtures from the pinned Asterisk and `app_rpt` sources and record any mismatch
between the manual and source here.

## Current implementation slice

The sibling `librptadviax2` crate currently parses and serializes RFC full- and
mini-frame packets, parses and serializes generic length-delimited IEs, validates
UTF-8 text frames, builds initial NEW packets with a final empty CALLTOKEN IE,
parses AUTHREQ method/challenge IEs, replaces Asterisk's final empty CALLTOKEN IE
with an opaque peer token and builds the sequence-reset NEW retry, constructs
and verifies MD5 AUTHREP authentication, handles outbound ACCEPT/REJECT and
established-call PING/PONG/ACK control, rejects malformed lengths and invalid
call identities, and expands/encodes subclass C-bit values using Asterisk's
canonical rules. It caches replies to retransmitted setup and PING frames. A
separate nonblocking UDP endpoint sends and receives datagrams without parsing
or modifying them. A distinct `G711Ulaw` codec adapter uses the released
`audio-codec-algorithms` implementation to convert normalized mono `f32` at
8 kHz, with RFC 5456 capability bit `0x00000004`. The media layer encodes
negotiated-codec samples directly into caller-owned mini-frame storage; receive
parsing borrows the opaque payload and leaves decoding to the selected codec
adapter. Tests carry encoded μ-law over loopback UDP and decode it only after
protocol parsing.
The library also has a pure incoming `radio` NEW acceptance helper. It checks
the configured called node, numeric calling node, and u-law capability, then
builds an ACCEPT with legacy FORMAT and version-0 FORMAT2 IEs. Token validation
and product access authorization must run first; the helper does not allocate
call state or open a listener.
The library also has a call-token authority that issues and validates the
Asterisk `timestamp?sha1` token, binds it to the source socket address and a
per-process random integer, challenges an empty-token inbound NEW, continues
valid source-bound retries, and rejects absent or invalid tokens without
allocating call state. Token age defaults to ten seconds. `InboundIaxListener`
composes that gate with initial `radio`/ULAW validation and a product
authorization callback. It sends ACCEPT only after authorization, then routes
full and mini frames to a bounded per-call SPSC queue and returns a peer session
to its caller. Routes are retired on peer drop or HANGUP. Loopback tests cover
challenge/accept, denied admission, media delivery, and route reuse. The
standalone process now binds the listener for each effective local IAX port,
dispatches inbound calls to the corresponding configured node, applies current
product topology/access policy, and transfers accepted peer ownership into the
existing link lifecycle. Product callbacks receive the source IP only (not its
ephemeral UDP port), matching extnodes source validation. The listener remains
a distinct versioned IAX network adapter.
The outbound ULAW client sends the standard extnode username `radio` in
`USERNAME`, the remote node in `CALLED_NUMBER`, and the local node in
`CALLING_NUMBER`; this identity mapping is covered by its loopback session test.
Its network owner retries the current reliable setup frame with Asterisk's
default 100 ms initial delay, tenfold backoff capped at 10 seconds, and four
total transmissions, bounded by the caller's dial timeout. Loopback tests cover
loss of both the initial NEW response and the AUTHREP response.
Unit and integration tests pin exact deterministic wire bytes, subclass
exponent boundaries, ASL key-query, call-token, AUTHREP, and μ-law behavior,
malformed input handling, and real loopback transport.

For outbound peer control, the client maps IAX control subclasses 12 and 13 to
zero-payload radio-key and radio-unkey events. The product session gates key-up
on `!NEWKEY!`, `!NEWKEY1!`, or the two-second legacy fallback and always honors
unkey. Focused session tests cover each transition.

The listener admits token-valid, policy-authorized initial ULAW `radio` calls,
routes them to the matching local node, verifies source IP through the selected
extnodes/DNS directory, and transfers the peer into the product's inbound link
lifecycle. Inbound MD5 is not required by the shipped ASL3 `[radio]` user
configuration. Established-call reliable frames now remain in a bounded
64-frame send window until cumulatively acknowledged. The network owner retries
them after an initial two seconds, multiplies the interval by ten up to ten
seconds, and gives up after four retries, matching Asterisk's default linked
full-frame policy. Cumulative ACK retirement handles the 8-bit sequence wrap.
ASL-specific IE negotiation, codecs other than μ-law, complete text semantics,
and sustained live ASL3 interoperability remain open. This slice does not
establish complete interoperability.
