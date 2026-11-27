# Shared Connection Adapters and WebRTC Implementation Plan

> Use `superpowers:subagent-driven-development` or `superpowers:executing-plans` when implementing this checklist.

**Goal** — Bring AllStarLink, EchoLink and SIP/IAX phone connections under a common Product contract and use that contract for WebRTC transceive connections to site ports.

**Architecture** — Providers retain protocol signaling, codec, media timing and transport ownership. Product owns admission, exact connection identity, station policy, source routing and public observation. WebRTC uses the existing authenticated HTTPS/HAL surface for session signaling and its own media and data channels for audio, keying and timing.

**Design** — [Approved shared connection adapters and WebRTC design](../specs/2026-11-26-connection-adapters-and-webrtc-design.md). Its dependencies include the [JSON/REST design](../specs/2026-11-18-json-rest-configuration-design.md) and the [status stream and snapshots design](../specs/2026-11-25-controller-status-stream-design.md).

## Before every checkbox

Before implementing **each** checklist item, review the approved design and relevant sections of the dependent designs, the applicable ADRs and [architecture overview](../../../doc/architecture/README.md), [WISHLIST.md](../../../WISHLIST.md), the current code and tests at the named boundaries, and all applicable `AGENTS.md` files. Recheck the governing rules in any shared provider repository being changed. Use the wishlist to understand existing decisions and dependencies. The approved design and its recorded amendments define the feature scope.

Choose the simplest solution that satisfies the approved item. Trace the existing entry point and reuse its types, functions, tests and checked provider interfaces wherever practical. Use standard or existing dependencies where they meet the contract. Add the smallest missing behavior at its current owner. Introduce a dependency or public interface only for a demonstrated gap. Do not add requirements from the wishlist or invent new configuration, protocols or limits. Verify the affected normal entry points and record the tests and results before checking an item. Follow [QUALITY.md](../../../QUALITY.md) for the required gates.

Scope includes the shared adapter, WebRTC controller transport and management contract, and the correlation data and browser test needed to prove the contract. Voting, simulcast, a new link network, a management app, and production TURN or relay deployment remain future work. Use isolated qualification fixtures. Live-node changes require explicit approval.

## 1. Establish the existing contract and baseline

Start with `rust/product/src/link/{mod,session,outbound,ring,egress,dispatcher,source_clock}.rs`, `rust/product/src/services.rs`, `rust/core/src/link/`, the managed ASL and EchoLink frontends in `rust/peer-support/frontend/`, and `rust/private-phone-host/src/{calls,progress,iax/media,sip/call_media}.rs`. Review ADRs 0018, 0021, 0023, 0025, 0026, 0035, 0037, 0043, 0044 and 0045.

- [x] Trace incoming and outgoing AllStarLink, EchoLink, SIP phone and IAX phone through standalone and applicable Asterisk entry points. Map existing begin, progress, ready and attachment acknowledgment, cancellation, quiescence and ownership handoff. Record which provider generations, source proofs, clock positions, key edges and capabilities are actually available.
- [ ] Establish a conformance baseline with the existing Product peer, private-call, ring, egress, host and same-process reload fixtures. Preserve traces for accepted-prefix retry, private reply ownership, mute and mix-minus, connect versus attach, fast key edges where supplied, cancellation before and after answer, same-device redirect and terminal cleanup. Record unknown measurements separately from known off or silent observations.

### Existing ownership

| Provider | Admission and exact identity | Readiness and media ownership |
| --- | --- | --- |
| AllStarLink | The standalone managed IAX listener and the Asterisk answered-channel hook reach `lifecycle_asl` with an account generation and validated provider source. The ordinary incoming route cannot issue this capability. | The answered negotiated handle joins `PeerSession` only after Product validation and radio binding. Product checks the exact redirected ingress observer before acknowledging attachment. Standalone can expose negotiated radio-key events. Legacy PCM paths lack some source timing and key evidence. |
| EchoLink | Both hosts use the managed frontend. Its claimed QSO and frontend/account generation survive into Product admission. | The acknowledged provider registration is followed by the same Product redirect. Source selection, finite burst drain and input provenance remain provider-owned. The current managed runtime prepares direct profiles and rejects proxy profiles, so proxy qualification remains open. |
| SIP and IAX phone | Both hosts use the same private frontend and checked Product transfer. An opaque call keeps the provider account, call generation and ingress token. Reverse calls require a current mailbox binding before joining the radio. | `PrivateCall` supplies timed PCM, source loss, and provider callback registration. The private session shares `PeerIoWorker` with ordinary peers. Product confirms the exact radio route through the private redirect. Outgoing status currently reports attachment before that acknowledgment, and polling may miss a short phone key pulse. |

The existing `PeerIo::ready` reports whether an input frame can now be read.
Usable endpoint evidence comes from the prepared or answered provider handle,
successful registration and Product's exact route acknowledgment. The code
currently keeps superseded link transports live until separate detach and
can announce an ordinary connection before that acknowledgment.

### Baseline evidence

The isolated Debian 13 arm64 baseline at `eaef6bf` passed 154 Product tests
across peer sessions, rings, egress, dispatch, private calls, redirect withdrawal,
radio-bind refusal and owner retirement. The Asterisk fixture closed the status
subscription, module and channel in the same process. A later ordinary unload
in the stress loop failed. Full native provider and repeated lifecycle baseline
coverage remains open.

## 2. Share lifetime, media ownership and station policy

Use the current `PeerIo` transfer, `PeerSession`, private-call session, provider callbacks, `PeerIdentity`, inbound ring, `source_clock`, link mixer and prepared egress distributor as the starting points. Keep the boundary internal to Rust while the current checked host capabilities suffice.

- [ ] Consolidate connection admission and retirement around the exact port, provider account/generation, pending attempt, connection incarnation and station generation. Publish connection or attachment only after usable endpoint acknowledgment. On reconnect, cancellation, or stop, revoke admission and queued work first, and retain callback/DSO ownership until the provider quiesces. Test same-public-identity replacement, stale callbacks and cancellation at each ownership boundary.
- [ ] Share the timed PCM handoff. Carry original versus concealed samples, gaps, end-exclusive source spans, clock epoch and provable birth or arrival bound. Retain an unaccepted suffix with its original evidence. Retire queued samples, conversion state, key evidence and meter windows on discontinuity. Preserve one packet-level delay owner and the existing 48 kHz PCM ring as the asynchronous rate and drift owner. Exercise loss, retry, source restart and compatible redirect using the existing ring and source-clock tests.
- [ ] Share the existing mix-minus, per-source DSP, station receive qualification and egress dispatcher. Preserve blocking, per-peer RX mute and its epoch, monitor modes, priority, routing and privacy isolation on fresh and retained media. Encode once and retry the exact uncommitted packet within its original deadline. Keep provider acceptance, socket submission and confirmed feedback distinct. Verify three independent parties, private reply isolation, and a stalled destination beside a healthy one.
- [ ] Reuse the existing station TX arbiter. Carry transport talk state, valid PCM, station qualification, software PTT and measured PTT as distinct observations. Keep the reason for release and current handling of lead-in, hang, receive priority and TX protection. Verify fast key and unkey, timeout, mute, stale audio, provider loss, and a physical PTT reading different from the software request.
- [ ] Reuse the current HAL/status observation types and owner handoffs. Publish real lifecycle and key transitions where the owner supplies them, order graph and connection retirement before dependent meters, and declare observation loss. Retain counter and jitter diagnostics independently of key or attachment authority. Verify rapid transitions, recovery after an observation gap, and absence of protected payloads. Observe legacy PCM-only key or egress behavior at its owner before claiming equal coverage.

### Lifecycle evidence

The replacement regression failed before the old owner was stopped and passed
with the exact reply retired and the provider quiesced. The successor kept its
command route. The controller now reports link success and outgoing call
attachment from the exact media acknowledgment. Both continue to use the
existing station link hub and serialized peer worker. The ARM64 standalone
fixtures passed SIP dial, hangup, RTP handover and three-party mix-minus, plus
AllStarLink originate. The phone receive fixture recorded a qualified key and
unkey between status reads. Managed EchoLink direct and private reverse IAX passed on ARM64 with their
explicit SDK and provider paths. The application mix-minus tests passed with
receive-only and timed keyed sources, exact session registration, and
immediate revocation of previous ingress epochs. Managed Proxy and repeated
same-process lifecycle qualification remain separate.

## 3. Migrate one existing provider at a time

Keep each provider's signaling and codec behavior where it already lives. Compare each migration against the baseline before migrating the next provider.

- [ ] Move AllStarLink admission and media handoff onto the common contract. Reuse its IAX registration and directory/source checks, reliable windows, topology, addressed DTMF, and negotiated radio key or existing PCM fallback. Verify the current registration, addressed control, retries, source validation and reconnection fixtures.
- [ ] Move EchoLink through the same contract. Preserve directory validation, direct and proxy selection, source selection, burst drain and protocol text handling. Verify direct and proxy media, selected-source ownership and finite drain without changing the shared station mixer.
- [ ] Move SIP and IAX phone through the common contract at the existing private call and attachment boundary. Use the current JSON/REST single-port association, mailbox/PIN authorization and block rules. Preserve private prompts and replies, initial reverse-call mute after attachment, call-owned mix-minus, VAD/valid-media continuity, and the outgoing caller's existing authority. Verify both call directions, early media, progress, telephone events and authenticated attach for the applicable transports.
- [ ] Verify mixed and concurrent provider admission, normal standalone operation, and same-process Asterisk reload/unload on the migrated paths. Exercise fallible admission and release, bounded per-connection queues and fair progress under pressure. Derive capacity from measured resources and real provider limits without a fixed total peer or call ceiling. Use current released DSOs. If a provider capability is missing, qualify the smallest compatible extension with its loader and version/package checks.

## 4. Add authenticated WebRTC signaling and media

Start with the existing routes, HAL discovery, OpenAPI, runtime coordinator, HTTPS/TLS trust and proxy fixtures in `rust/core/src/management/`, `rust/product/src/management*` and `rust/peer-support/src/https*`. Review ADRs 0006, 0016, 0018, 0021 and 0045 and the approved WebRTC transport section.

- [ ] Qualify the simplest maintained WebRTC implementation that supplies full ICE, Trickle ICE, consent, TURN UDP/TCP/TLS, DTLS-SRTP, Opus and the two required data channels. Check how it exposes RTP timestamp, source, packet acceptance, loss and clock observations. Reuse existing codec, RTP, jitter, rate conversion and peer-support code where the selected provider can do so without adding another playout owner. Keep the transport outside the station audio thread.
- [ ] Expose runtime session creation, candidates, state/restart and closure on the selected port through authenticated HAL discovery. Reuse the authenticated frontend principal and the existing backend trust and Origin checks. Verify the principal at the backend boundary and distinguish two users sharing one proxy service certificate. Bind each session to its exact principal, port, generation and authorized direction. Revalidate on offer, restart and authorization changes. Verify cross-principal rejection through the same proxy, wrong-port rejection, stale ICE generation and fingerprint, bounded SDP/candidates, idempotent candidate retry/close, and receive-only sessions with no microphone ingress.
- [ ] Connect the approved session to the common adapter after transport and media are ready. Use DTLS-SRTP and protected data channels end to end, and Opus with its 48 kHz RTP clock. Supply packet timing, provenance, loss and discontinuity at the existing handoff. Apply ordinary network and audio blocking and station TX policy. Verify independent receive-only and transceive sessions, usable-endpoint acknowledgment, restarts, revocation and cleanup on refusal or provider error.
- [ ] Qualify signaling from behind CGNAT through the approved proxy/relay or existing overlay path, using an outbound authenticated site connection. Verify independent media traversal with full ICE and Trickle ICE. Test direct where reachable, two independent CGNAT endpoints, both using relay candidates, TURN/UDP, TURN/TCP, TLS on port 443, blocked UDP, consent loss and ICE restart. Use short-lived TURN credentials and validated TLS identity in the fixture. Verify one relay failure cannot disrupt unrelated peers. Keep SDP, candidate addresses and credentials out of public status and logs.

### Transport evidence

The Debian 13 arm64 proof built on Rust 1.88.0. `str0m` 0.24.1 with dynamic
OpenSSL negotiated direct Trickle ICE, DTLS fingerprint verification, both
DCEP modes and Opus RTP with the actual SSRC, extended timestamp and receiver
arrival. It mapped the codec-input sample window to the observed RTP epoch.
The ARM64 coturn fixture verified bidirectional datagrams and connected
two relay-only RTC endpoints with protected RTP over TURN/UDP, TURN/TCP
and TURN/TLS. It used time-limited credentials and normal certificate
validation. Direct ICE on owned UDP sockets and the simulated DCEP and RTP
fixture passed in the same run. The selected ICE stack still needs a
way to confirm consent responses on the selected pair. The pinned is 0.11.1 can
retain a path while it receives Binding requests without replies to its
outbound consent checks. The published str0m 0.26.0 and is 0.12.1 have the
same observation gap and str0m 0.26.0 requires Rust 1.89.0. Preserve the
qualified Rust 1.88.0 pin until a provider supplies the checked selected-pair
response time. A local provider prototype verifies the exact path and uses
the first accepted response for a transaction. Its 106 ICE tests passed,
including the one-way response-loss case. The str0m integration, transport
expiry behavior, and released dependency still need qualification.
[RFC 7675 §5.1](https://www.rfc-editor.org/rfc/rfc7675.html#section-5.1) ·
[str0m 0.26.0](https://docs.rs/crate/str0m/0.26.0)

The ARM64 release Opus adapter passed negotiation, encoding, variable packet
duration, decoding, and explicit concealment. It reports normal PCM origin as
unavailable because its checked descriptor has no provenance support. A
separate Opus candidate with the existing optional provenance ABI passed
the codec-native reference and failure tests on AMD64 and ARM64. The
provider source is preserved locally at `df75c1a19ca6609a6a23069948ad8c1457917603`.
Controller transmit and browser qualification remain separate. The direct transport fixture also verified DCEP role admission,
backpressure, closure, and protected RTP. The bounded wire tests verified
decimal clock coordinates and sample range containment through RTP wrap.

The ARM64 mTLS handoff fixture verified two browser identities on one proxy
certificate, denial of spoofed assertions, and revocation at request claim.
The existing proxy owns OIDC. WebRTC session creation must bind the typed
frontend identity to the port and runtime owner before admitting media.

The ARM64 foreground controller at ccd6323c passed both isolated station
fixtures. Receive-only delivered the simulated receiver marker over Opus and
rejected talk. Transceive keyed the simulated transmitter and reached another
listener, excluded the source from its own return mix, released on lease
expiry without key-off, and rejected delayed lease renewals. The separate
listener remained connected. The fixture used the qualified unpublished Opus
provider to obtain decode provenance. The running controller was untouched.

The follow-up at 3085b843 used the real standalone controller and the
authenticated HAL session. It renewed ICE on the existing DTLS/DCEP
association, refused a candidate for the retired generation, and resumed
the receiver marker on a fresh in-band anchor. The same-RTC driver tests
verified both channels and RTP after restart. Total peer silence caused
disconnect and closed further protected-channel writes in 21.5 seconds
in the isolated ARM64 test.

The mixed TURN fixture reproduced a passive endpoint that never started
ICE checks while direct traffic was unavailable. The driver now supplies
the initial ICE timer input. At b26aad42, the ARM64 coturn tests passed
for both symmetric relay peers and the mixed UDP/TLS path. At 3857a1ed,
the isolated controller used its systemd-supplied short-lived TURN/TLS
credential and a relay-only UDP peer. The received Opus radio marker and
the RTP, HAL and WebSocket correlation matched in two runs. Both runs used
private loopback addresses. Deployment through independently hosted CGNAT
and port 443 remains part of release qualification.

The optional WebRTC path currently revokes sessions at configuration
activation. Comparable graph redirects during ordinary link attachment
preserve the transport. A configuration save replaces the command owner
for each enabled port. A future compatible handoff needs an acknowledged
successor station and current authorization, old lease retirement, and a
fresh media anchor. Test an edit to another port and a verified rollback,
followed by explicit withdrawal of the selected port.

## 5. Implement timed keying and receiver observations

Reuse the existing admission fences, source timeline, station gating, and negotiated capability checks. Follow the design's keying, clock model and receiver-quality sections.

- [ ] Add the versioned reliable ordered control channel and unordered, partially reliable timing channel. Key leases bind the exact session, station grant, media epoch, sequence, sample window and receiver-local expiry. Bound each channel's messages, outstanding work and timing. Apply leases to sample eligibility through Product and release on expiry, uncertainty, loss of consent, revocation, restart or key-off. Test duplicate, late and reordered updates, lost key-off, overlap, stale samples and receiver-local release with the status socket absent.
- [ ] Implement sample-to-extended-RTP anchors with source/graph epochs, a rational sample period or rate, nanosecond values as decimal strings, and measured uncertainty. Use RTCP Sender Reports and RFC 6051/7273 where supported and qualified. Use verified TAI only where established. Handle RTP wrap, drift, clock jumps, source reset and concealment. Use browser Encoded Transform timestamps where exposed, or the designed reduced-precision receiver mapping. Qualify any native RFC 8285 extension only when both ends negotiate it. Keep unknown or insufficient precision explicit.
- [ ] Carry calibrated RF RSSI with sample span, clock epoch, calibration identity and uncertainty where the provider supplies it. Carry measured noise floor or SNR in their own units and windows where available. Preserve unavailable and uncalibrated states. Verify source ownership and timing comparability and publish only the authorized sanitized observation. Keep the work to timing and RF-quality metadata.

## 6. Correlate status meters with receiver playout

Reuse the status graph, meter `audio_position`, capture/stream visibility and generation fences. Follow the design's **Correlating meters with what the listener hears** section.

- [x] Publish the opaque session media binding, media epoch, selected track/direction, RTP source and anchor over WebRTC control. Project the same binding and epoch for authorized meter viewers. Anchor the actual egress codec-input samples to the encoded RTP timestamp. Carry stage/signal, graph generation, rate, the end-exclusive meter window and relevant anchor. Report estimated or unavailable upstream correlation where measured delay or a common mix cannot be established.
- [ ] Use a known audio marker and a browser fixture with `RTCRtpReceiver.getSynchronizationSources()` to align a meter window at track delivery. Verify early and late frames, missing anchors, concealed spans, different mixes, source/graph changes, ICE or SSRC restart, and the supported receiver fallback. Retire pending old-epoch display data. Measure hardware output delay separately if making hardware-playout claims. Verify audio and key handling with status absent or disconnected.

The isolated ARM64 standalone fixture verified a 1 kHz receiver marker
through Opus RTP. The authorized HAL snapshot and the next authenticated
WebSocket meter batch used the in-band anchor and the actual codec-input
sample span. A separate transceive fixture verified the admitted key state
and local lease expiry alongside a receive-only client. The tests used the
compiled foreground controller with simulated device input and output.

The browser loopback fixture passed three consecutive Edge 158 runs. Its
1 kHz marker crossed a real 32-bit RTP wrap. The receiver held an early meter
until track delivery, discarded a late meter and retired the old SSRC and
epoch. A new track continued with no status traffic. The fixture uses
simulated status records and a browser test flag to place the timestamp near
the wrap. The follow-up at 040a76e1 also renewed ICE credentials with
standard browser restart, kept the same fingerprint and media contract, and
delivered protected data on the original channel. The fixture connected two browser peers. The controller-to-browser and real status-stream display test
remains part of qualification.

## 7. Review and qualify the combined implementation

- [ ] Compare the code and API against each approved design section. Run the baseline provider suites and WebRTC signaling, media, timing, routing, privacy and correlation fixtures. Verify simultaneous calls and peers, slow viewer and destination isolation, admission refusal and cleanup, bounded queues and relay bandwidth pressure. Record the observed capacity and precision and any unavailable provider evidence.
- [ ] Update applicable ADRs and affected architecture, OpenAPI, test and packaging records through the existing generation paths. Run the required native Debian 13 amd64 and arm64 qualification and amd64 production coverage gate. Verify standalone startup and same-process Asterisk reload/unload with matching artifacts. Record exact source revisions, providers and test evidence before claiming support.
