# AllStarLink implementation status

The AllStarLink implementation is present in the local source tree. It is not
deployment-ready: it has not been installed or enabled on 524950 or any other
live node, and it has not yet been proven interoperable with a classic
`app_rpt` peer. This document describes the implemented behavior and the
remaining validation; it is not authorization to activate a live link.

## Implemented locally

The controller accepts the configured linking-only DTMF operations: monitor,
transceive, local-monitor, permanent variants, disconnect, `*10` temporary-link
disconnect, disconnect-all, reconnect-all, status, last-keyed, full status, direct-peer remote-command
mode, and the fixed `*722` local-time announcement. Forced-ID, macro, autopatch,
and other unrelated commands are not implemented. The normal default mappings are documented in
[configuration](configuration.md); mappings are configurable, use a leading
`*`, and destination-taking commands end with `#`, local receiver unkey, or a
three-second interdigit timeout. Destination `0` selects the last link destination.

For administrative testing, `rpt_advanced command <node> <DTMF>` injects a
complete DTMF sequence through that same collector. For example,
`rpt_advanced command 524950 *722` queues the configured time response. The
command terminates an input not ending in `#` exactly as a local receiver
unkey would; it does not bypass command mappings or authorization.

`*4<node>` selects only a directly attached peer whose identity resolves and
passes the same allow/deny policy required for an incoming peer. That policy is
rechecked for every forwarded digit. Subsequent DTMF is delivered only to that
peer, and `#` always exits remote-command mode locally. No remote-command digit
is interpreted as a local command while that mode is active.

An attached direct peer's IAX `DTMF_END` events use the same serial control
queue as locally decoded DTMF only while its identity passes the current
deny-first policy, so a classic authorized peer can send linking commands to
this node through remote-command mode. An explicitly outbound audio link may
remain connected after a policy change, but its rejected IAX DTMF is ignored.
Starts and malformed end events are ignored. The reader is joined before its
hub and runtime callback are released; a reload discards any queued event from
the retired runtime.
Local in-band DTMF is decoded in the radio worker; when a digit completes, its
current PCM frame is silenced before controller or link routing when
`dtmf_muting` is enabled. Local receiver unkey queues the same command
terminator as `#`, so an unfinished command cannot survive a transmission.

Incoming calls enter through `RptAdvanced(node)`. The implementation checks an
optional local static directory first, then uses the configured DNS/file
selection; `both` uses ASL DNS before the optional ASL external directory. Every
accepted source verifies the numeric IAX address. Valid nodes are accepted by
default. Inherited `link_allow_nodes` and `link_deny_nodes` lists can restrict
that policy; denial wins over allowlist membership and there are no access
exemptions. A list never makes an unresolved or mismatched identity valid. A
present malformed record or address mismatch fails closed rather than falling
through to a later source.

Direct links are transceive, monitor, or local-monitor links. Routing provides
mix-minus and prevents local-monitor links from being forwarded to other peers.
The media boundary is rate-aware: Asterisk negotiates an available channel
format, Asterisk translates the wire codec to peer PCM, and the controller uses
libsamplerate when peer PCM differs from the radio rate. Ordinary 8 kHz ASL
operation remains the primary compatibility case; broader codec/rate
interoperability is still to be demonstrated against real peers.
The selected local radio rate bounds the dynamically discovered IAX candidates.
Asterisk's public IAX request path reduces a multi-format audio capability to
one format before IAX negotiation, so rpt_advanced attempts one exact candidate
at a time from the local native rate downward within one 20-second dial budget.
That permits a compatible wideband peer to use direct PCM while a ULAW/SLIN8
peer remains usable without Asterisk's `codec_resample`. After IAX chooses the
wire format, the adapter resamples its matching-rate PCM as needed.

Permanent links retain their routing mode after an initial dial or attachment
failure as well as after an unexpected transport failure. They retry
immediately, then after one second with exponential backoff to a five-minute
maximum. An explicit permanent disconnect cancels a queued retry.
Disconnect-all retains both temporary and permanent links for reconnect-all;
only permanent links retry automatically. Retained state is in memory only, so
no link survives a local Asterisk restart.

The half-duplex policy prevents local receive audio from being repeated and
holds transmit off while the local receiver is active. Linked audio remains
eligible for transmit according to the configured link mode. Identifier and RF
status playback defer until reception ends when half duplex prohibits a
transmission.

The status, last-keyed, and full-status operations queue short spoken RF replies
outside the audio callback. Speech preparation falls back to Morse when Piper
is unavailable or when reception interrupts it; playback starts no sooner than
250 ms after local receiver unkey. They preempt a scheduled identifier without
satisfying it. `rpt_advanced link status <node>` and the compatible
`rpt link status <node>` show direct peers, routing mode, permanence, and the
current topology cache in the Asterisk CLI. Retained links appear as `retrying`
or `paused`, so an operator can distinguish an active transport from a pending
automatic or reconnect-all recovery. Full status logs that cache when its RF
status reply was successfully queued.

Topology is deliberately best-effort rather than an authoritative network map.
The implementation validates inbound app_rpt-style `L ` text advertisements and
caches the last valid one for each direct peer. Each direct peer is sent a
recipient-excluded outbound `L ` advertisement after topology changes and on a
30-second refresh cadence. A terminal `R000000` route means the bounded
advertisement was truncated. A peer that has not advertised yet, does not
support `L `, or has changed topology since its last update can therefore make
the reported topology incomplete or stale.
An advertised route containing the local node is treated as a topology loop;
the direct peer is disconnected without retry. A route naming another direct
peer is also detached, covering a legacy peer that does not advertise its own
topology. Direct self-links, duplicate direct links (including permanent links
and retained retries), and a requested target already named by an attached peer
are rejected. A local rejection queues the spoken status `LINK REJECTED
TOPOLOGY LOOP`, with normal Morse fallback.

Routing and audio exchange remain lock-free in hardware-paced callbacks. Link
admission, dialing, retry, status, topology construction, and IAX text delivery
run on control or peer-reader threads.

## Verification still required

The current source changes require a fresh full quality run before they may be
merged or released. Focused unit and isolated-Asterisk integration tests cover
the local behavior, but this document intentionally makes no current aggregate
coverage or platform-matrix claim.

Before a live deployment, complete the required Debian 12/13 amd64/arm64
quality matrix and staged-install checks, then perform explicitly approved
testing with the existing 524950 link settings. That testing must cover the
linking commands, allow/deny behavior, connection failure and recovery,
disconnect-all/reconnect-all, reload, half-duplex behavior, status, and both
classic 8 kHz `app_rpt` peers and any higher-rate capable peer. No AllStarLink
integration deployment has occurred.

## Reference checks

The ASL3 [IAX text page](https://allstarlink.github.io/developers/iaxtext/)
warns that it is incomplete and potentially incorrect. The local implementation
uses the documented behavior together with observed app_rpt wire conventions,
including `!NEWKEY!`, `!NEWKEY1!`, and `L ` topology advertisements. No app_rpt
implementation is copied.

Read-only checks on 524950 found an active RadioPlusAdvanced channel, an
incoming dialplan still using `Rpt()` and `RPT_NODE()`, DNS-only lookup, and an
IAX radio profile allowing ulaw, adpcm, and gsm. Those settings are reference
material for future approved testing; they have not been changed for this work.
