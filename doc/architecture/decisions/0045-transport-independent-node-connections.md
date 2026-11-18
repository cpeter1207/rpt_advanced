# ADR 0045: Transport-independent node connections and conferencing

Status: Accepted by owner 2026-11-18; implementation alignment and validation pending.

## Context

SIP calls are conceptually node connections, like IAX2 and EchoLink connections.
The product already shares its AudioPeer, node mixer and mix-minus path. The
owner requests reuse of this working architecture rather than a separate SIP
conferencing system or invented, stricter SIP requirements.

This clarifies [ADR 0044](0044-phone-peers-and-rf-presentation-pacing.md) and
builds on [ADR 0025](0025-native-media-routing-and-pcm-ring-ownership.md) and
[ADR 0037](0037-standalone-lock-free-peer-ingress.md); it does not replace their
ownership, authorization or lifecycle boundaries.

## Decision

- Use the same network-to-node audio pattern for SIP, IAX2 and EchoLink.
  Reuse existing jitter buffering, codec and media components wherever possible,
  taking the working IAX path as the behavioral baseline. Keep actual signaling,
  packet framing, clock and codec differences at their transport boundaries.
- Multiple admitted calls use the existing node mixer, mix-minus and conferencing
  behavior. Do not build a parallel SIP conferencing subsystem. Admission and
  routing remain node policy, not accidental consequences of packet format.
- Apply common jitter, packet-loss and courtesy-tone policy to equivalent audio
  conditions. Introduce a difference only for a demonstrated protocol or required
  behavioral distinction, not a hypothetical edge case or an invented stricter
  requirement for SIP.
- Preserve authentication, authorization, private replies, source identity,
  stale-audio retirement and safe detach/reload. Existing peer isolation and
  outgoing-call graph cut/restore require review against the owner's intended
  privacy policy; shared mixing alone does not authorize removing them.

The earlier [wishlist single-call checkpoint](../../../WISHLIST.md) and its
single-call/external-conferencing restriction are superseded by this shared-node
architecture. Multi-call implementation and validation remain pending.

The approved P13/P14 privacy requirements remain: outgoing calls suppress ordinary
peer audio as specified, and incoming calls require explicit authorized `*3`
attachment. Reusing the node mixer does not repeal these routing policies or
per-call lifecycle isolation; any change requires explicit owner direction.

## Consequences and verification

Review existing SIP and IAX paths before adding abstractions. Share policy and
components where behavior is equivalent; retain demonstrated transport-specific
mechanics and required security/lifecycle distinctions. Resolve any proposed
privacy change with the owner before implementing it.

Verify equivalent node routing, mix-minus, jitter/loss behavior and courtesy
cancellation through the existing owners, including multiple admitted connections
when that increment is implemented. Reuse meaningful tests and preserve required
safety checks; do not invent unreachable scenarios. This ADR records direction,
not completed transport parity, hardware validation or deployment approval.
