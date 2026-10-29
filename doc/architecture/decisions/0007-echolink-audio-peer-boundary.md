# ADR 0007: EchoLink audio-peer boundary

Status: Planned; decision accepted

## Context

rpt_advanced must interoperate with EchoLink at audio level with parity to the
current ASL3 EchoLink behavior. Incoming human text messages also become spoken
incoming media under the approved 2026-10-26 codec requirement. EchoLink supplies
an external audio/control transport; it
does not supply the AllStarLink topology advertisements used by native peers.

## Decision

Model an EchoLink station as an external audio peer at the controller boundary.
It participates in the normal audio mix, receive activity, telemetry, courtesy
tone, identifier, and link lifecycle paths, but not in AllStar topology
advertisements. Keep the transport, directory registration, station metadata,
UDP audio/control, heartbeat, and callsign admission behind a dedicated
EchoLink adapter. The adapter is outside real-time callbacks and hands decoded
canonical normalized `f32` audio at its source rate to the peer's inbound PCM
ring. Under the 2026-09-13 amendment in ADR 0025, that ring alone converts and
corrects drift into the native transmit mix; outbound codec conversion remains
at egress. EchoLink's transport representation converts only at its external
boundary. The two-worker media path is implemented; the EchoLink adapter
described here remains planned.

The adapter identifies incoming human text and submits it to the shared,
protocol-neutral text-message codec under ADR 0022. Speech synthesis stays
outside audio callbacks. Controller policy mixes its PCM with concurrent
receive audio, including audio from the text sender, only at the local
transmitter, without waiting for silence or entering the polite telemetry
queue. Never send synthesized text as peer audio. Forward eligible messages
only as text when the protocols support forwarding; each peer determines its
own rendering. Preserve source identity and normal no-echo rules.
Protocol-control text is never a speech input.
This incoming-text requirement supersedes the prior text exclusion; outgoing
text composition, text commands, chat interfaces and text recording remain
outside scope. A synthesis failure skips only the local spoken rendition and
logs an error; it does not select Morse, interrupt other incoming audio, or
prevent supported text forwarding.

Each configured rpt_advanced node owns one EchoLink identity. Its default UDP
audio/control pair is 5198/5199, but the pair and optional bind address are
configurable so local nodes can use unique adjacent pairs. Direct firewall/NAT
forwarding remains an administrator responsibility. Distinct local ports alone
do not make multiple accounts publicly reachable: simultaneous nodes require
distinct effective public endpoints. A node may instead use the published
EchoLink Proxy protocol, normally over TCP port 8100, including its standard
challenge authentication and directory/audio/control tunnels. The user supplies
and operates the proxy; each proxy serves one client. EchoLink calls use the
existing link reconnect policy. The proxy preserves the real remote IPv4
address for directory-backed admission. Configuration exposes the complete
ASL3 station and directory identity set: callsign, EchoLink node number,
password or secret source, name, location, email, latitude/longitude, RF
frequency, CTCSS tone, power, antenna height, gain, direction, and status
message.

Incoming calls allow all valid EchoLink stations by default. Per-node wildcard
callsign allow and deny lists refine that policy, with deny taking precedence.
The default simultaneous-station limit is 20. Heartbeats run every 10 seconds
and the control-heartbeat timeout is 100 seconds, corresponding to ASL's ten
missed heartbeat cycles; both are per-node configurable. Explicitly disconnected
or ordinary on-demand calls do not reconnect automatically. Permanent and
scheduled calls use the established permanent-link reconnect policy.

Outbound dialing uses only the normal configurable link-connect operation. The
reserved EchoLink form pads the complete positive 32-bit EchoLink node number
to at least six digits and prefixes it with `3`. Collect until the normal
terminator or unkey, not a fixed seven-digit alias limit. Canonical internal
identity is protocol-qualified and never the alias. Callsign-search dialing
and a directory browser are not provided; the adapter performs directory
lookups for normal dialing and admission. It supplies ASL3-equivalent
receive/transmit gain adjustment, a connection-announcement mode of off, node number, callsign, or
both, and configurable EchoLink telemetry output behavior.

The owner approved the complete [EchoLink design](../../../docs/superpowers/specs/2026-10-28-echolink-interoperability-design.md)
on 2026-10-28. Use released GPLv2-compatible Rust RTP/RTCP libraries where
fixture tests prove they fit EchoLink's legacy wire variants. Keep any required
compatibility adaptation narrow; do not implement another standards stack or
add a WebRTC stack for anticipated protocols. Reuse across adapters is a goal,
not a requirement to reject a useful dependency. The
[implementation plan](../../../docs/superpowers/plans/2026-10-28-echolink-interoperability.md)
records codec-client moves, packet-library fit tests and coordinated host/DTMF
contracts. Host-services ABI 9 and product ABI 7 introduce typed incoming
requests and copied per-peer protocol/capability metadata. Both IAX hosts
preserve their existing transport/media behavior and populate real local and
remote identities. Product SONAME 2 rejects the older host/product pair.
EchoLink host attachment, account reload and finite source-read integration
remain in progress; the provider does not yet advertise its network/media groups.

Source reads return exact accepted-prefix credit; the provider retains rejected
PCM and missing spans. The product holds one finite-ring token and acknowledges
playout only when that token succeeds. A generation discard is a distinct
completion, never proof that audio played. Completing turns admit no new PCM or
gaps, which remain provider-owned until the next turn. Ring replacement preserves
the protocol policy and retires the old token. EchoLink never enters IAX's
NEWKEY, topology, downstream-source query or remote-command exchange.

The serial source owner detects fresh selected-talker PCM at 8 kHz, using the
node's DTMF muting setting. Codec-concealed PCM and counted gaps are not fresh
detector input. Its immediate atomic suppression gates delayed output while
source intervals retain mute decisions through ring playout. The existing
per-peer scratch block holds only derived processing state during a partial
admission; it is not another audio delay queue or a second raw-input owner.

Both hosts share provider/context/account lifetime owners in peer-support.
Their tagged peer handles keep the existing native IAX payload separate from
the shared EchoLink QSO. IAX-only callbacks reject Echo handles before casting;
Echo metadata retains the real decimal station number, not its dial alias.
Source/prepared media, encoder reset and deadline calls share the QSO owner.
Host callback tables expose complete operation groups, while each peer's copied
capabilities select its actual operations. Serial registration tokens are tagged
by protocol; no Echo token enters IAX's executor. Source callbacks translate
borrowed records without copying raw PCM or retaining callback pointers.
The prepared account retains its context and dynamically loaded provider; a
failed stop retains foreign state rather than unload code beneath notifications.
The provider's sized PeerInfo includes the observed incoming SDES endpoint,
including port zero for Proxy framing. This supplies the typed incoming request
without substituting a directory address or assumed port for an observation.
The network/media operation group remains unpublished while this record and
the shared host glue are integrated; lifecycle-only users do not consume it.

## Consequences

No EchoLink protocol, directory, socket, control work or speech synthesis occurs
in an audio callback. The shared incoming-text path uses two bounded ordinary
workers, finite-media rings and generation-tagged SPSC endpoints. The transmit
callback mixes ready speech only into local RF; independent protocol text
forwarding never depends on speech admission or synthesis success. Disconnect,
reload and unload cancel the exact source generation and join workers before
releasing speech contexts. Metadata-only counters and rate-limited failures use
the shared product's existing status/logging ports in both hosts. The EchoLink
transport itself remains planned. Its absence of topology advertisements means
the existing advertised-topology loop prevention cannot infer EchoLink-only
transitive paths. A generic HTTP proxy is not a valid EchoLink transport
proxy; the selected standard TCP Proxy tunnels directory and peer traffic.
