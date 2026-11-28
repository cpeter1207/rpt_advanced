# ADR 0046: Shared connection admission and WebRTC

Status: Accepted 2026-11-26. Implementation and qualification are in progress.

## Context

AllStarLink, EchoLink and private SIP/IAX calls already use station media and
generation ownership. The controller needs a common admission and observation
contract for those providers and for WebRTC sessions between management clients
and site ports.

## Decision

Product owns station permission, exact connection admission, mix-minus, mute
and privacy scope. Each provider owns signaling, packet timing, codec state and
transport shutdown. An answer can precede media attachment. Publish an
attachment only after the current station graph acknowledges the endpoint.
Retire the old permission and its exact media scope before releasing transport
resources. Reuse the existing prepared PCM handoff and distributor from
[ADR 0025](0025-native-media-routing-and-pcm-ring-ownership.md) and the
generation rules from [ADR 0026](0026-generational-real-time-runtime-lifecycle.md).

WebRTC signaling uses the authenticated HAL surface in
[ADR 0045](0045-modular-json-and-hal-management.md). The existing trusted proxy
supplies the verified browser principal. The media association uses full ICE
with Trickle ICE, optional TURN, DTLS-SRTP, Opus RTP and two DCEP data channels.
ICE restart keeps the negotiated track and authenticated association and retires
the old station grant. Transmit eligibility requires a current dated sample
lease, provenance and all normal station protection. Neither media nor keying
depends on a status observer. Add public station telemetry to the authorized
WebRTC management output after per-session voice subtraction. Preserve
addressed reply ownership and use the configured playback audience on
ordinary connections. Station admission and privacy isolation also apply
to the WebRTC output.

The WebSocket and REST snapshots observe the same optional meter correlation
using an opaque media binding, track, sample span, RTP coordinate and epoch.
Source and clock uncertainty remain explicit. A level is correlated only when
the owner can measure its relationship to the active track.

## Consequences

Existing providers remain independently released. Missing checked provider
capabilities stay unavailable until verified. Transmit-capable WebRTC requires
decode provenance. Voting, simulcast, management UI and production relay
deployment are separate work.

The [approved design](../../../docs/superpowers/specs/2026-11-26-connection-adapters-and-webrtc-design.md)
and [checklist](../../../docs/superpowers/plans/2026-11-26-connection-adapters-and-webrtc-implementation.md)
record the protocol detail and qualification evidence.
