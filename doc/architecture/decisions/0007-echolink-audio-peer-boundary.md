# ADR 0007: EchoLink audio-peer boundary

Status: Accepted

## Context

rpt_advanced must interoperate with EchoLink at audio level with parity to the
current ASL3 EchoLink behavior, while deliberately excluding text chat and all
other text features. EchoLink supplies an external audio/control transport; it
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
boundary. This aligns the pending two-worker design, not a deployed change.

Each configured rpt_advanced node owns one EchoLink identity. Its default UDP
audio/control pair is 5198/5199, but the pair and optional bind address are
configurable so local nodes can use unique adjacent pairs. Direct firewall/NAT
forwarding remains an administrator responsibility. A node may instead route
an account through a configured user-provided EchoLink-aware UDP proxy. The
project does not ship, operate, authenticate, or secure that proxy. EchoLink
calls use the existing link reconnect policy. The proxy contract is a host,
audio/control port pair, and optional credential reference; it must preserve
enough source identity for inbound calls. Configuration exposes the complete
ASL3 station and directory identity set: callsign, EchoLink node number,
password or secret source, name, location, email, latitude/longitude, RF
frequency, CTCSS tone, power, antenna height, gain, direction, and status
message.

Incoming calls allow all valid EchoLink stations by default. Per-node wildcard
callsign allow and deny lists refine that policy, with deny taking precedence.
The default simultaneous-station limit is 20 and the default control-heartbeat
timeout is 10 seconds; both are per-node configurable. Explicitly disconnected
or ordinary on-demand calls do not reconnect automatically. Permanent and
scheduled calls use the established permanent-link reconnect policy.

Outbound dialing uses only the normal configurable link-connect operation. The
reserved EchoLink form pads a four- through six-digit EchoLink node number to
six digits and prefixes it with `3`; callsign and directory lookup are not
provided. The adapter supplies ASL3-equivalent receive/transmit gain
adjustment, a connection-announcement mode of off, node number, callsign, or
both, and configurable EchoLink telemetry output behavior.

## Consequences

No EchoLink protocol, directory, socket, or control work occurs in an audio
callback. EchoLink text messages, chat, remote text commands, and text
recording are not implemented. Its absence of topology advertisements means
the existing advertised-topology loop prevention cannot infer EchoLink-only
transitive paths. A generic HTTP proxy is not a valid EchoLink transport
proxy; the selected proxy contract must carry audio and control UDP traffic.
