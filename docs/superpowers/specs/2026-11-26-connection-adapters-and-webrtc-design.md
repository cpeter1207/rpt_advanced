# Shared connection adapters and WebRTC

**Status** — Approved, 2026-11-26.

This design defines a common connection, audio and control boundary for AllStarLink, EchoLink, SIP/IAX phone, and a future WebRTC endpoint. It covers provider parity, browser NAT traversal and timing, and how an authorized status viewer aligns meters with received audio. Existing station, configuration and status designs continue to own their respective policies.

## Shared connection adapter

Define one Product-facing contract for a bidirectional media connection on an exact configured port. Adapt the existing ASL, EchoLink, and phone paths to this contract in place, then bring up WebRTC as another provider of it. Keep the radio, station control, and status paths independent of transport. The existing Product boundaries already support the important pieces. `PeerIo` moves an exclusive provider handle to a registered owner, `PeerSession` and the private phone session handle accepted-prefix PCM, and the ordinary link mixer owns station routing. Use those contracts to guide consolidation. Start with an internal Rust boundary and the checked existing host capabilities. Extend a published descriptor only when the migration demonstrates a missing capability. [ADR 0021](../../../doc/architecture/decisions/0021-versioned-rust-c-adapter-boundaries.md) · [ADR 0040](../../../doc/architecture/decisions/0040-initial-alpha-compatibility-policy.md)

### Lifecycle, authority, and identity

A provider offers a pending connection with its authenticated or validated provider identity, bound account and account generation, direction, immutable port ID, and negotiated capabilities. Serialized Product control applies access policy, reserves resources, and issues exact admission. The provider owns signaling, timing and media until quiescent detach. The adapter supports begin, progress, ready, attach acknowledgment, cancellation and terminal cleanup. A provider answer can precede a usable media endpoint. Publish the connected or attached transition only after the owning generation acknowledges the usable endpoint. Pending progress references an opaque attempt distinct from the admitted connection. Early media, if allowed, has its own bounded route and deadline. Neither receiving a packet nor seeing a media description grants attachment.

Keep separate identities for the boot, port resource, provider account and registration generation, connection incarnation, phone call and ingress token, station command generation, and media graph or source clock epoch. A protocol-qualified public node identity can recur across incarnations. Use the exact incarnation for queued work, reply ownership, guards and exported status. Reconnect invalidates the previous incarnation before publication of a successor. Media renegotiation or a continuity-breaking redirect retires the applicable epoch. A compatible redirect can preserve the transport and logical call without preserving retired PCM or measurements. Cancellation or stop closes admission first. Drain or explicitly abandon accepted work, redirect or stop the provider, wait for acknowledged quiescence off audio, then release callback contexts and libraries. All callback work uses preallocated bounded storage and fixed ownership. [ADR 0026](../../../doc/architecture/decisions/0026-generational-real-time-runtime-lifecycle.md)

Keep provider identity and media grants distinct. ASL and EchoLink use their existing registered peer, source, access and topology checks. A reverse phone connection begins with only its private input and reply endpoints. Apply the current mailbox allow/deny policy and PIN check before publishing a station attachment. Preserve the caller-owned initial mute and separate administrative grant. An outgoing phone leg uses the initiating station's media authority. The telephone recipient gains no interactive command authority by answering. For WebRTC, Product derives listen, transmit, and command authority from the authenticated browser session and selected port. A provider transport can attest receipt of a requested action. Product decides whether that principal and connection can act. Preserve the same exact reply and grant fences for each transport. [ADR 0023](../../../doc/architecture/decisions/0023-unified-control-and-dtmf-policy.md) · [ADR 0044](../../../doc/architecture/decisions/0044-phone-peers-and-rf-presentation-pacing.md)

### Audio, timing, and policy

The adapter receives normalized codec-rate PCM, counted gaps, and discontinuities on a source-position timeline. Distinguish original PCM from codec concealment. Retain the exact declined suffix with its original position, clock epoch, provenance, and birth or conservative arrival bound. Acknowledge finite work only at the existing drain or explicit cancellation boundary. Ingress stages with no supported provenance report that limitation, and media requiring a proven freshness bound drops until the bound is available. Source restarts and clock jumps retire queued samples, converter state, pending key evidence and measurement windows together.

The provider owns wire-frame ordering, loss interpretation, negotiated codec operation and the one packet-level playout delay for its direction. Reuse the shared jitter adapter where the provider admits tagged packets and can use its clock and loss contract. A complete provider that already owns packet-to-PCM playout reports its timestamps, delay, gaps and observable diagnostics at the adapter boundary. Avoid stacking independent adaptive packet delays. The existing inbound PCM ring remains the sole asynchronous conversion and drift owner into the fixed 48 kHz station graph. Keep protocol jitter bounds, station delay, and source-to-output age as distinct settings and measurements. Phone retains its own latency profile and VAD/lookahead policy. Duration and receive decisions use monotonic or sample clocks. Wall time is for presentation and correlation. [ADR 0025](../../../doc/architecture/decisions/0025-native-media-routing-and-pcm-ring-ownership.md) · [ADR 0035](../../../doc/architecture/decisions/0035-fixed-48khz-native-audio.md) · [ADR 0037](../../../doc/architecture/decisions/0037-standalone-lock-free-peer-ingress.md)

Reuse per-source DSP, station qualification, mix-minus and the prepared off-audio distributor. An attached connection has a distinct source and destination. Its normal outbound programme excludes its own source upstream of egress encoding. Product owns audience, monitor/transceive/local-monitor, priority-group selection, local RF-only versus network routes, RX mute, mute epoch, and outgoing privacy-isolation claims. Apply the current routing and mute fences to retained PCM and converter state as well as fresh samples. Keep private replies on their originating connection's queue even when the connection is detached. RF-only signalling, regeneration and courtesy/audio presentation do not enter network programme. A provider may refine its own channel capabilities and negotiated framing. It cannot broaden an audience, force RF PTT, or bypass transceiver inhibit and hardware safety.

Egress takes source-tagged native programme with its immutable freshness deadline. The per-connection owner converts to the negotiated codec rate, encodes once, and handles send acceptance or bounded retry. Retry the exact uncommitted packet within the original deadline. Provider acceptance transfers only the stated ownership. Distinguish the Product's accepted packet, socket submission and any confirmed external feedback. An overflowing or stalled destination drops stale work on its own path. It cannot delay another stream, station control or radio audio.

Represent transport key/unkey, available decoded PCM, effective station qualification, software PTT request, and measured physical PTT separately. ASL uses negotiated control edges where present and its existing PCM fallback otherwise. EchoLink keeps its selected-source and drain semantics. Phone uses permitted original-media/DTMF handling plus its configured VAD and hold rules, including valid-media continuity when VAD is disabled. WebRTC may submit a permitted talk intent when supplied by the approved session design. Transport signalling alone never keys RF. Product applies the common source gate before routing and the station TX arbiter owns actual key timing, lead-in, hang, receive windows and release. A negotiated unkey, timeout, provider loss, stale span, silence, or mute retains its own meaning. Where timing cannot be observed, publish unknown and keep the existing safe gating rules. [ADR 0043](../../../doc/architecture/decisions/0043-shared-loudness-and-receive-vad.md) · [ADR 0044](../../../doc/architecture/decisions/0044-phone-peers-and-rf-presentation-pacing.md)

### Provider responsibilities

| Provider | Keeps ownership of | Reuses |
| --- | --- | --- |
| ASL | IAX registration and account/directory source proof, codec and packet timing, negotiated radio-key and DTMF compatibility, topology/command/text framing and reliable window behavior | Port admission, validated source identity, peer session/PCM ownership, link routing and mix-minus, source-addressed responses and observation |
| EchoLink | Directory and validated station identity, direct/proxy transport, codec, source selection and burst drain, provider protocol and policy gates | Same connection lifetime, ring and dispatcher, protocol text classification at its boundary, common receive and egress routing |
| SIP and IAX phone | Provider registration and dialog or call transaction, answer/early media, codec and negotiated telephone-event transport | Private call owner, mailbox/PIN policy, exact private control, independent call mute and isolation, normal mix-minus only after attachment |
| WebRTC | Browser session binding and the transport's own secure connectivity, negotiation, codec and timing duties | Existing station source/destination contracts, explicit grants, prepared peer graph, private notices where authorized, the same sanitized status observations |

Provider-specific controls remain capability-checked at the adapter boundary. Keep ASL topology and addressed DTMF behind their existing authority. A participant without an authorized control lane has only its negotiated media path. Isolate provider-facing opaque handles and feature negotiation in their existing frontend/dependency owners. Select and operate a single Rust/ABI representation per applicable boundary as each slice migrates. Preserve network interoperability and compatible provider operation during the cutover. [ADR 0018](../../../doc/architecture/decisions/0018-dynamic-linking-for-external-components.md) · [ADR 0045](../../../doc/architecture/decisions/0045-modular-json-and-hal-management.md)

### Capacity and observability

Derive admission from available CPU, memory, bandwidth, safe provider limits, and fallible reservation. Add no arbitrary total peer or call ceiling. Bound each connection's packet, PCM, work and reliable windows. Bound pending action times, consumer-owned replies, and observation lanes independently. Use fair progress across ready connections. Allocation, codec negotiation, provider call-number exhaustion, or an overloaded connection rejects the affected attempt and preserves current connections and local repeat. Release admissions and every reserved provider resource on all terminal paths. The controller's control and audio workers never wait on a client or subscriber.

Publish immutable typed state from the existing authoritative owner for connection, direction, registration, pending progress, attachment, mute, and key transitions. Use the same public DTOs for HAL and stream captures. Retain ordered state edges in bounded handoffs and mark overflow explicitly. Current provider assertions can produce exact rapid remote key events. Legacy PCM-only qualification and network egress need instrumentation at their actual mixer or provider decision before claiming equivalent edge coverage. Preserve separate source/graph generations, confirmed media age, and reported measurement quality. Treat counters, jitter and ring drops as diagnostics, distinct from authority and state transitions. Never include audio, authentication operands, private prompts, telephone numbers, keys or transport credentials in status. [Controller status stream design](2026-11-25-controller-status-stream-design.md)

## WebRTC transport and NAT traversal

Expose an authenticated runtime session collection through a relation on the selected port's HAL status or operations view. Use the existing HTTPS management ingress and backend trust, including the browser Origin and authorization policy. A browser never receives a backend service identity or key. Bind each session to its authenticated principal, exact port, runtime generation, and requested media direction. A stable port ID comes from HAL discovery. Admission can allow receive-only use without microphone or transmit permission. Revalidate grants at offer acceptance, on restart, and whenever authorization changes.

| Session action | Runtime behavior |
| --- | --- |
| Create | Submit a bounded SDP offer and requested direction to the discovered collection. Return an opaque owner-bound session, the answer when ready, and links for candidates, state and closure. |
| Exchange candidates | Trickle candidates and end-of-candidates in order per ICE generation over the HTTPS session resources. Bound candidate count, size, rate and lifetime. Retry a submitted candidate without duplicating admission. Reject stale generations and candidates for another session. |
| Observe or renew | Retrieve current signaling state and new server candidates through session-scoped authenticated reads. A restart replaces the ICE generation on that session after a fresh authorization check and gives candidate updates a new fence. |
| Close | End the session explicitly or expire it after cancellation, credential revocation, consent failure, setup timeout or loss of its owner. Revoke key and source admission before retiring provider resources. Closure is safe to retry. |

An ICE-only restart keeps the authenticated DTLS association, Opus track and
DCEP channels. Both ICE credentials change. The controller revokes the old
station and sample grants before accepting new media, then waits for a new
nominated pair and a usable station attachment. The next Ready message carries
the new ICE generation. A talk request names that generation and requires a
fresh sample grant and anchor.

Signaling resources are runtime-owned. Keep SDP, candidate addresses, ICE passwords and TURN credentials out of the public status and logs. The operational status WebSocket remains a read-only observer. The signaling ingress must be reachable through the approved reverse proxy or
deployment path. A site behind CGNAT can reach a publicly available authorized
management relay or existing overlay through an outbound authenticated
connection. This gives the app a signaling path without opening a home
listener. TURN independently supplies media and data connectivity. The
signaling relay cannot grant port access or force transmit.

In the service provisioning document, `webrtc_bind_address` enables WebRTC on
an assigned local IP. Each media session obtains its own ephemeral UDP socket.
The HTTPS signaling service uses the existing loopback listener and trusted
proxy identity. The optional `webrtc_turn_credential` selects a protected
systemd credential named `rpt-advanced.turn.<id>`. Its JSON contains
`server`, `transport`, `expires_unix`, `username`, and `password`. For
`tls`, it also contains `tls_name` and `tls_issuer`. The issuer names a
DER credential in the existing TLS namespace. An unconfigured or unavailable
relay leaves the direct candidate available. Signaling renews the port grant
through the authenticated proxy before its receiver-local deadline.

Use full ICE at both endpoints and Trickle ICE. Prefer an admissible direct candidate pair when one works. Supply a public authenticated TURN service with UDP and client-to-relay TCP and TLS on port 443 available for restrictive networks. Use a properly validated TURN TLS identity and short-lived, restricted allocation credentials. Test both ends behind independent CGNAT, including a case where each obtains a relay candidate. TURN over TCP or TLS carries datagrams on that client-to-relay leg. Its ordinary UDP allocation still relays over UDP toward the peer or its relay address. The design requires neither endpoint to accept public unsolicited media connections. Preserve consent freshness per selected candidate pair and close the transmit gate on lost consent. Track relay path, consent and setup failure using sanitized categories. [RFC 8445](https://www.rfc-editor.org/rfc/rfc8445.html) · [RFC 8838](https://www.rfc-editor.org/rfc/rfc8838.html) · [RFC 8656](https://www.rfc-editor.org/rfc/rfc8656.html) · [RFC 7675](https://www.rfc-editor.org/rfc/rfc7675.html)

Use DTLS-SRTP and the WebRTC transport and security profiles. Authenticate the negotiated peer fingerprint through the authorized signaling exchange. Keep SRTP and data-channel protection end to end when using TURN. Use Opus with a 48 kHz RTP timestamp clock. Pass decoded PCM, loss and continuity information to the adapter. The RTP clock rate defines timestamp units even when the codec encodes a different internal audio bandwidth. Browser playback and capture device clocks still require the appropriate source-clock mapping. [RFC 8827](https://www.rfc-editor.org/rfc/rfc8827.html) · [RFC 8834](https://www.rfc-editor.org/rfc/rfc8834.html) · [RFC 8835](https://www.rfc-editor.org/rfc/rfc8835.html) · [RFC 7587](https://www.rfc-editor.org/rfc/rfc7587.html)

### Push-to-talk control and sample timing

Establish versioned DCEP-negotiated data channels on the approved session. A reliable ordered control channel carries the talk-grant decision, media epoch, clock/correlation anchors, explicit key-off and terminal release. A separate bounded unordered, partially reliable timing channel carries short-lived sample-key lease updates. Bound message size, update frequency, outstanding bytes and remaining delivery lifetime. Reject unsupported versions and message kinds. Each lease binds the current session, station grant, media epoch, timing sequence, admitted sample window and receiver-side expiry. Latest valid updates advance that lease. Duplicates, out-of-order updates and late updates cannot extend or revive a retired grant. If no valid renewal arrives, or the receiver cannot prove a sample belongs to the lease, release the source gate. Always enforce expiry locally even when an ordered key-off is lost or delayed. [RFC 8831](https://www.rfc-editor.org/rfc/rfc8831.html) · [RFC 8832](https://www.rfc-editor.org/rfc/rfc8832.html)

A granted lease makes samples eligible for the existing source and station policy checks. It cannot force TX. Product fences the accepted receive span by its effective sample range and timing uncertainty. Stale media or deferred packets cannot resume a completed talk burst. On overlap, ambiguous timestamps, clock discontinuity, loss of consent, permission revocation or transport restart, revoke eligibility and require a current grant and anchor before reopening it. The station retains its existing receive priority, PTT lead-in/hang, TX protection and hardware feedback rules. The sample-key lease and radio release paths operate without a status subscriber.

A native provider may negotiate an RFC 8285 RTP extension for controller-specific per-packet timing or lease evidence when both endpoints support the same version. It remains optional. Standard browser WebRTC exposes capabilities implemented by the user agent and provides no general API to inject an arbitrary RTP header extension. Browser PTT uses the lease gate and control channels. Where browser Encoded Transform exposes the RTP timestamp, use it to bind the browser's offered timing to the received stream. Providers without this capability may associate the current lease with bounded receiver-observed RTP ranges and report the reduced precision. Reject a request that requires sender-sample precision if the mapping cannot be established. A controller-specific header extension never substitutes for admission or the expiry fence. [RFC 8285](https://www.rfc-editor.org/rfc/rfc8285.html) · [W3C WebRTC RTP capabilities](https://www.w3.org/TR/webrtc/#rtcrtpheaderextensioncapability) · [W3C Encoded Transform metadata](https://www.w3.org/TR/webrtc-encoded-transform/#rtcencodedframemetadata-dictionary)

### Clock model

Associate each timestamp with a declared clock kind and epoch. Service monotonic time governs freshness and expiry. A verified TAI mapping can support cross-machine correlation when available. Report its offset or anchor, capture uncertainty and loss of synchronization explicitly. A wall time label, remote clock claim or one RTP observation does not establish TAI or bound one-way delay.

| Clock or coordinate field | Contract |
| --- | --- |
| Sample coordinate | Original position and end-exclusive sample span, nominal sample rate, and exact source/graph epoch. Preserve positions across retransmission and decline. |
| RTP coordinate | Extended RTP timestamp for a known synchronization source and media epoch. Unwrap the 32-bit wire timestamp against observed continuity. Detect wraps and distinguish them from resets, restarts and retired sources. |
| Anchor and period | Actual observed correspondence between a source sample and an RTP timestamp, and, where measured, a monotonic or TAI instant, the sample period and drift. Use a rational period or rate so the nominal 48 kHz period need not round per sample. |
| Uncertainty | Bound mapping error and observation age, include actual period or drift information where proved, and publish unknown when the clock cannot be related. Carry uncertainty through the sample-range gate and downstream correlation. |

Encode nanosecond values and extended 64-bit coordinates as decimal strings. Nanosecond units describe representation granularity. Accuracy depends on the source, timestamping, clock resolution, synchronization and measured uncertainty. The design makes no sub-nanosecond accuracy claim. UTC remains a display and event-correlation clock.

Reuse RTCP Sender Reports to relate RTP and sender reference timestamps. Use the RFC 6051 NTP timestamp extension when negotiated and RFC 7273 clock identity when the clock relationship is actually known. An exchanged timestamp still needs a verified mapping to the station monotonic timebase. Absolute Capture Timestamp is an expired Internet-Draft. An interoperable deployment may use the extension as opportunistic evidence, with a fallback that derives a qualified mapping from the supported observations. Treat RFC 6464 audio level as a codec-stream audio indication and keep calibrated RF RSSI in the radio provider's separate observation vocabulary. [RFC 3550](https://www.rfc-editor.org/rfc/rfc3550.html) · [RFC 6051](https://www.rfc-editor.org/rfc/rfc6051.html) · [RFC 7273](https://www.rfc-editor.org/rfc/rfc7273.html) · [IETF Absolute Capture Timestamp draft](https://datatracker.ietf.org/doc/draft-ietf-avtcore-abs-capture-time/) · [RFC 6464](https://www.rfc-editor.org/rfc/rfc6464.html)

### Receiver signal quality

A capable receiver publishes RF RSSI tied to the exact capture source and
sample span, with its clock epoch, sampling position and quality. Express
calibrated received power in dBm and include calibration identity and
uncertainty in dB. Report unknown where a provider only yields an uncalibrated
indicator, or publish that indicator with its distinct unit. A receiver with
a measured noise floor or SNR may include those quantities with their own
units and observation windows. Authenticate the source and verify the declared
clock relationship before treating samples from separate receivers as
comparable. Carry the quality record on the connection's timing channel or
negotiated native metadata extension. Optional status observers can see the
same sanitized record. This design carries the timing and RF observations a
future voter or timed transmitter can use. Its choice of source and scheduling
of RF launch stay outside this design.

### Correlating meters with what the listener hears

Assign each authorized rendered WebRTC media path a stable opaque media binding for the session and a replaceable media epoch. Announce the binding, epoch, receiving track/direction, RTP source and current anchor through the session's control channel. Publish the same opaque binding and epoch with permitted meter observations in the status capture and stream. The binding reveals no credentials, connection address, or dialable identity. Observers see only the meters allowed for their identity, and a viewer can correlate their own receiver path without opening another control session.

At the measured egress codec input, capture the actual correspondence between a source sample and its extended RTP timestamp when accepting the encoded frame. A meter reading names its binding, media epoch, graph generation, stage/signal, sample rate, end-exclusive measured sample window and relevant anchor. An upstream meter can use this mapping only across quantified or otherwise proven delay and rate relationships. Report estimated or unavailable correlation across changing or opaque DSP stages, unknown resampling delay or a different mix. For an end-exclusive sample range, convert its end using the anchored sample-to-RTP relationship before placing the display marker. Preserve missing and concealed spans in the mapping.

In the browser, read the matching receiving SSRC through `RTCRtpReceiver.getSynchronizationSources()`. The API provides a recent RTP timestamp and a receiver-local timestamp for media delivered to that receiver's track. Extend the RTP timestamp against the current anchor, map each bounded meter window into that timeline, and interpolate its display position only between compatible observations within the maintained bounds. A meter delivered early can wait for the receiver timeline to reach its window. If it is late, stale, or missing, show the freshest valid result or mark the reading stale. Avoid interpolation across source resets, missing observations or unmatched epochs. Track delivery is the browser observation boundary. Hardware output, browser mixing and audio-device buffering can add delay beyond that point. [W3C RTCRtpReceiver synchronization source observation](https://www.w3.org/TR/webrtc/#dom-rtcrtpreceiver-getsynchronizationsources)

Re-anchor after ICE restart, SSRC change, device change or graph-generation discontinuity. Retire queued correlation data for the old epoch. A dropped or inaccessible browser sample or status update may delay or invalidate the display. Playback and PTT remain independent. Receiving and mixing audio, maintaining a transmit lease and releasing the source work with the status socket disabled or disconnected. [Controller status stream design](2026-11-25-controller-status-stream-design.md)

### WebRTC qualification and scope

| Case | Required evidence |
| --- | --- |
| Connectivity | Browser and controller behind independent CGNAT, direct where reachable, TURN-only over UDP, TCP and TLS 443, UDP-blocked fallback, consent expiry and ICE restart. Confirm bidirectional permitted audio and key behavior and preserve other connections under relay failure. |
| Session security | Discovery and candidate flow through the authenticated proxy, rejected cross-user/port session claims, wrong grant or epoch, unauthorized candidates, stale fingerprint or lost credentials, and cancellation during setup and active media. Receive-only sessions never contribute microphone PCM. |
| Timing | Deterministic drift and clock jumps, sample and RTP wraparound, exact timed spans after jitter/PLC, late and reordered lease updates, lost key-off, consent loss and grant revocation. Prove that stale samples cannot reopen the gate and that radio protection remains authoritative. |
| Correlation | Use a known audio marker and bounded meter windows through controller encoding and browser track delivery. Verify codec-input agreement, observed upstream delay where supported, correct graph/epoch retirement, slow or missing meter delivery and receiver fallback. Measure any additional hardware delay separately before making hardware playback claims. |
| Capacity | Multiple simultaneous peers and quiet/active callers without a fixed peer-count cap, saturated destination handling, allocation refusal and cleanup, relay bandwidth pressure, and audio/PTT with no status viewer. Compare to baseline ASL, EchoLink and phone behavior. |

The design covers shared connection ownership, WebRTC media/control contracts and playback correlation. Receiver voting, simulcast, a browser application, network/TURN deployment and implementation of the new adapter are outside this design task. Preserve future extension points through existing typed capabilities.

### Migration and qualification

1. Inventory each current frontend's hooks against the common contract. Establish conformance fixtures for existing ASL, EchoLink and phone behavior before consolidating the owners. Use current integration evidence to distinguish the transport’s known capabilities from deferred ones.
2. Land shared lifetime, accepted-prefix and status handoffs around the existing adapters. Move the smallest complete transport cohort at a time. Compare the provider and station traces before changing the actual transport or routing. Keep same-process Asterisk reload and standalone startup supported.
3. Admit WebRTC only after its own signaling and identity layers supply the same capability, authorization and clock inputs. Add versioned descriptors, loader and package floor changes together where the selected transport exposes a real missing capability. Qualify any media latency or RF behavior change using the repo's approved fixture and node process.

| Qualification | Observe |
| --- | --- |
| All providers | Concurrent admissions, rejected source/account, separate claim and actual media publication, reconnect replacing same public alias, cancellation before and after answer, same-device redirect and reopening, retired callbacks and DSO shutdown |
| Media and keying | Accepted-prefix retry, loss/PLC, SSRC/source restart, unchanged age through retry, drift correction, stale-window discard, talk/key/unkey ordering faster than HAL polling, queue and device failure, and distinction between requested and observed PTT |
| Routing and privacy | Three independent parties prove no self-return, normal eligibility, no bypass of mute, monitor, priority, TX protection or isolation, no leakage from an unauthenticated caller, exact source-only prompts and responses |
| Provider regression | ASL registration, addressed control and retries. EchoLink direct/proxy selection and finite drain. SIP and IAX telephone events, early media, call progress and end-to-end login. WebRTC transport and browser permission evidence from the WebRTC-specific plan |
| Load and observability | Slow peer and subscriber isolation, fallible new admission under pressure, real short-lived transitions, ordered status retirement, no credential/audio payloads, current capture after a deliberate observation gap |
| Release | Supported native ARM64 and AMD64 provider/build/package checks, isolated startup and same-process Asterisk reload/unload. Record actual evidence at each boundary before representing it as supported |
