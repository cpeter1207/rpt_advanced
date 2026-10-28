# EchoLink interoperability

Date: 2026-10-28

Status: Proposed for owner approval. Design only; no implementation, repository
publication, release, installation, account login, or radio test is authorized
by this document.

## 1. Scope

Add EchoLink to standalone `rpt-advanced` and the deprecated
`app_rpt_advanced.so` adapter through the same dynamically linked implementation.
Provide complete audio interoperability with current EchoLink stations and
conference servers, using current ASL3 behavior as the compatibility baseline.
Also support the already-approved incoming human-text speech and forwarding.
"Full" means the complete connection, identity, directory, transport, media,
and lifecycle path within this scope, not a copy of the EchoLink desktop UI.

Retain the owner's decisions:

- One separate validated EchoLink account per configured controller node.
- All station/directory metadata is configurable.
- Accept valid stations by default; callsign wildcard deny rules take
  precedence over allow rules. An allow list, when present, restricts admission.
- Dial by EchoLink node number through the existing remappable link commands;
  do not add callsign-search dialing or a directory browser.
- Support a user-operated EchoLink Proxy. Do not build or operate a proxy.
- Reuse existing permanent links, fallback groups, schedules, duplex policy,
  courtesy tones, telemetry, DTMF authorization, and timeout protection.
- Speak incoming human text locally with sender attribution, mixed with live
  receive audio. Never send its synthesized speech to linked peers.

Not included: SIP/autopatch, an enhanced rpt_advanced protocol, a chat UI,
outgoing text composition, text-command administration, message recording,
new REST/WebSocket interfaces, automatic router configuration, mobile UDP Relay
service, new hardware support, or unrelated wishlist work.

## 2. References and corrections to older records

The implementation guide must review the ASL3 and EchoLib source, not just
manuals. Pin packet fixtures and expected behavior to these reviewed versions:

- ASL3 `22.10.1+asl3-3.10.8-1`, app_rpt commit
  `d3929751f102c37b3c8b4e1b0040ec19d907cfea`.
- SvxLink/EchoLib release `26.05.2`, commit
  `595cf9bb3325be753d05b219bdd39feab6a22b33`.
- EchoLink's current official networking, account, and Proxy documentation.

Three corrections are proposed for ADR 0007 and its wishlist entry when this
design is approved:

1. **Proxy:** standard EchoLink Proxy tunnels directory and peer traffic through
   TCP, normally port 8100. It is not a configurable UDP relay pair. [R1, R2]
2. **Liveness:** ASL's sample `rtcptimeout=10` means ten missed ten-second
   heartbeat cycles, approximately 100 seconds, not ten seconds. Use an explicit
   millisecond duration and a 100,000 ms default. [R3]
3. **Addressing:** real EchoLink IDs now exceed six digits. Preserve the complete
   ID; the old seven-character ASL alias cannot be the internal identity or an
   eager command-completion limit. [R4, R5]

Separate accounts and local UDP ports also do not guarantee simultaneous public
reachability. Official EchoLink requires distinct effective public addresses for
simultaneous nodes; each standard Proxy serves one client. These are network
constraints, not software node-count limits. [R1, R6]

The older records remain accepted historical decisions until the owner approves
these corrections. Do not silently mark EchoLink implemented.

## 3. Approach

**Recommended:** a small Rust EchoLink protocol adapter, separate network-I/O
and wire/state modules, and the existing released codec/jitter/media providers.
Publish the protocol component as a separately versioned shared object and
Debian runtime/development packages. Proposed names are `librptadvecholink`,
`librptadv_echolink.so.1`, and a companion workflow repository following the
IAX component's pattern. Names are proposals, not authorization to create them.

Considered alternatives:

- **Wrap EchoLib:** GPLv2-compatible and useful as a reference, but its public
  QSO/dispatcher/proxy APIs own Async sockets, a separate event loop, singleton
  dispatchers, and codec state. Keeping these would duplicate the selected
  Tokio and codec owners; removing them requires a maintained upstream fork.
- **Use Asterisk `chan_echolink`:** prevents Asterisk-free standalone operation
  and introduces an ASL dependency into the Asterisk adapter.

Neither alternative fits the existing architecture as simply as the recommended
approach. Do not copy app_rpt implementation code. Use independently written
packet/state logic, source-reviewed fixtures and released codec/cryptographic
libraries. Do not implement GSM, Speex, MD5, a new executor, queue algorithm,
resampler, jitter buffer, or PLC algorithm. Retain required attribution and
licenses for any incorporated upstream material. [R7]

## 4. Boundaries and reuse

Apply ADRs 0007, 0011, 0018, 0019, 0021–0023, 0025–0027, 0029, 0035–0038,
0040 and 0041. The controller remains the policy owner; the new adapter owns
EchoLink wire behavior, not radio policy.

```text
direct UDP or Proxy TCP -> account transport -> per-QSO serial owner
  -> EchoLink packet handling -> jitter adapter -> codec provider
  -> source-rate DTMF detection -> existing inbound PCM ring
  -> existing 48 kHz transmit mix

existing peer egress -> shared conversion -> codec provider
  -> EchoLink packetizer -> nonblocking account transport

validated human text -> shared text router and speech producer
  -> existing local-only text PCM input
```

The EchoLink DSO exposes a sized/versioned C descriptor, explicit lifecycle,
opaque account/QSO handles, bounded packet/media operations, capabilities and
value-free errors. No C++/Rust types or external-library types cross the ABI.
Wire/state logic accepts supplied bytes and monotonic time and produces bounded
actions; it does not call sockets. Its network owner performs Tokio I/O.
Codecs remain protocol-neutral providers and never own transport framing.

Reuse the existing host peer operations and tagged opaque handles in both
hosts. Asterisk must not create an `EchoLink/` channel or require
`chan_echolink`, app_rpt, or ASL3. Its radio path stays unchanged. Standalone
must not load Asterisk. Both hosts select the same EchoLink provider.

Current gaps requiring narrow changes are real, not just packaging:

- Product session setup unconditionally emits ASL `!NEWKEY1!` and parses ASL
  topology/keying text. Select protocol-specific session behavior instead.
- Incoming admission currently uses the AllStar directory and numeric identity.
  EchoLink needs its own verified admission and protocol-qualified identity.
- Text destinations currently assume ASL serialization. Use each connection's
  actual protocol and forwarding capabilities.
- Async/prepared host operations are currently table-wide. Do not enable them
  for EchoLink and accidentally send Asterisk IAX peers through unsupported
  operations. Prefer a complete tagged dispatcher; if per-peer discovery is
  needed, make one explicit narrow contract extension and update its consumers.

Do not claim a new ABI is necessary before checking the existing extension
points. If an incompatible contract is necessary, change descriptor versions,
SONAME/packages and all consumers together. Reuse/move codec-client glue from
its current IAX ownership; EchoLink must not depend on the IAX library merely
to load GSM or Speex. No duplicate codec implementation is permitted.

## 5. Configuration and secrets

Register an `echolink` section family. Defaults in `[echolink]` fall back to
built-ins; `[echolink <controller-node>]` overrides them. Account identity and
credentials are node-specific and never inherited from another account.
Disabled is the default. Proposed example, not currently accepted syntax:

```ini
[echolink]
enabled = no
directory_servers = servers.echolink.org,backup.echolink.org
max_stations = 20
heartbeat_interval_ms = 10000
heartbeat_timeout_ms = 100000
connect_timeout_ms = 30000
receive_gain_db = -3
transmit_gain_db = 3
announcement = node
telemetry = demand
telemetry_dynamic = yes
transport = direct
bind_address = 0.0.0.0
audio_port = 5198
secrets_file = /etc/rpt_advanced/echolink-secrets.conf

[echolink 524950]
enabled = yes
callsign = KG0BP-R
node_number = <validated EchoLink node number>
name = KG0BP Repeater
location = <station location>
email = <station contact>
status_message = <station information>
```

Also expose latitude/longitude in decimal degrees, `frequency_mhz`, `tone_hz`,
`power_w`, `antenna_height_m`, `antenna_gain_db` and `antenna_direction_deg`.
These are advertised metadata, not hardware-control commands. The adapter owns
their conversion to EchoLink's supported representations; publish/test any
rounding. Do not confuse antenna gain with receive/transmit audio gain.

Other per-node/default options are callsign `allow`/`deny` patterns, named
announcement modes `off|node|callsign|both`, and named telemetry modes
`off|on|follow|demand`. Follow ASL's released semantic behavior, not its stale
numeric mode comments. The shared telemetry eligibility/routing rules remain
the upper bound: IDs, courtesy tones, hang time and synthesized human text must
not leak to peers merely because EchoLink telemetry is enabled. [R8]

Dynamic selection is a typed controller operation, not a second COP parser or
an EchoLink text command. Its optional DTMF mapping is administrator-configured,
has no new default prefix, uses normal collision checks and requires the
existing administrative unlock. `telemetry_dynamic=no` prevents that runtime
change; configuration reload can still set the policy. This does not authorize
building the pending REST/WebSocket interfaces.

An account-specific secrets section contains its directory password and, when
selected, its Proxy password. Reuse the strict regular-file, no-symlink,
owner/mode-0600 secret-file checks; provide that same loader to both hosts.
Do not put passwords in the main configuration, process arguments, status,
logs or fixtures. Account and Proxy passwords are distinct. No secret defaults
are shared among nodes.

The same loader code serves both hosts, but the selected file must belong to
the account running that host. Use each host's configuration directory for its
default secret-file path; the example above is standalone. Do not weaken mode
0600 or grant either service the other's Linux privileges merely to share a
file. Administrators may configure and provision each path explicitly.

For Proxy mode use `proxy_host`, `proxy_port=8100` and the account's Proxy
secret. Do not expose a misleading Proxy UDP-port pair. `audio_port+1` selects
the direct control port. Changed direct ports prevent local bind collisions;
they are not advertised as a way to make ordinary clients support several
accounts through one public address.

Configuration resolution follows ADR 0019. Unknown values warn and inherit a
safe default. An enabled account without a valid unique identity, accessible
credentials, necessary provider, or unambiguous endpoint cannot safely start;
reject that candidate rather than borrowing another account or opening access.
Invalid live candidates retain the previous complete generation.

The proposed audio gain defaults match ASL3. Apply each exactly once at its
protocol media edge; do not alter saved radio, mixer, local receiver or
voice/telemetry gains. Migration maps ASL `erxgain`/`etxgain` as dB, but converts
`rtcptimeout` from heartbeat counts to milliseconds. Do not import obsolete
driver paths, recording commands, dialplan contexts or numeric telemetry
comments as new controller features. [R3, R8]

## 6. Account, directory and admission

Each enabled account has one serialized directory/control owner on the shared
bounded Tokio pool, not a runtime or OS thread per station. States are disabled,
connecting, registering, online, retrying and stopping. A QSO becoming connected
does not prove the account's directory registration or inbound reachability.

Implement login, positive/negative login replies, refresh, logout, ordered
server failover and bounded full/differential directory retrieval, including
the plain and compressed variants used by the reference implementations.
Reuse released decompression behind the relevant adapter boundary and bound
compressed bytes, expanded bytes, records and field lengths before allocation.
Publish a completed immutable directory snapshot atomically; never expose a
half-applied differential list. Failed refresh preserves the last complete
snapshot, its age and degraded status, not a fabricated empty directory. [R9]

Proposed bounded defaults: directory connect/read operation deadline 10 seconds;
registration refresh 300 seconds; normal directory refresh 240 seconds; failure
retry after 20 seconds with the next configured server. Successful sessions
survive a directory-only outage. Authenticate new calls from a directory entry
validated within 15 minutes; on an older/missing entry request bounded refresh
and fail closed if verification cannot complete. Expiration is configurable;
it is not a second QSO-liveness timer. These are design defaults, not claims
that every reference client uses the same intervals.

Admission binds callsign, real node ID, observed remote address, local account
and a fresh connection generation. Match callsign **and** registered address;
do not authenticate only the text label in SDES. Validate the real remote
address carried by the selected Proxy, not the Proxy's address. A claimed text
sender is not an authenticated transport identity.

Normalize callsign case while retaining `-L`, `-R` and valid conference names.
Evaluate deny, then allow, after directory validation. Do not add an unauthenticated
IP-bypass mode. Enforce `max_stations` before publishing a QSO; a simultaneous
arrival cannot exceed it. Duplicate connections to the same EchoLink station
on one local node are rejected even if different numeric aliases were used.

Legacy directory and Proxy authentication do not provide modern end-to-end
encryption or cryptographically signed UDP peers. Report those limits honestly;
do not invent an incompatible authentication handshake. [R1, R6, R9]

## 7. Direct and Proxy transports

Direct mode owns the configured IPv4 UDP audio/text and control sockets plus
outbound directory TCP. Standard defaults are UDP 5198/5199 and TCP 5200.
The administrator supplies firewall/NAT forwarding; do not open the router.
No IPv6 peer-wire extension is introduced. [R10]

Proxy mode implements the published EchoLink Proxy challenge/response and
incremental TCP framing, including directory open/data/status/close, audio,
control and rejection messages. Use released MD5 solely because the standard
challenge protocol requires it; do not reuse it for stored administrative
codes. Handle split headers/payloads and multiple records per read. Reject an
oversized frame before allocating its advertised length. The Proxy frame's
remote IPv4 address and channel select the peer endpoint. [R2]

A standard Proxy carries one account and has no arbitrary per-packet UDP-port
field. Distinct accounts need distinct effective public endpoints, normally
separate user-provided proxies or addresses. Detect local conflicts and report
directory duplicate-login or Proxy-busy failures. Do not promise to detect
external routing correctness from local configuration alone. [R1, R6]

One slow peer never blocks other direct peers or another account. Use bounded
per-QSO queues and work quotas, serialize each QSO's mutable state, and yield
to socket/timer readiness. No task-per-packet spawning or busy polling.
One Proxy TCP connection is necessarily ordered: congestion can hold all QSOs
using that Proxy. Bound stale queued media and report that head-of-line limit;
do not claim that Tokio can remove it. Never discard a partially written TCP
record or corrupt framing; shed only complete unsent media records under the
existing stale-media policy. Reserve bounded capacity for control.

## 8. QSO control and identity

States are connecting, connected, closing and closed, with a preserved first
terminal cause. Parse and emit the EchoLink RTCP-like compound SDES/BYE control
forms, supported station-information records, heartbeat and codec capability
fields. Skip well-formed unknown optional SDES items; malformed lengths are
packet errors, not automatic disconnects. Do not send ASL negotiation, topology,
keying or `K?` text to an EchoLink peer. [R11]

Refresh QSO liveness only on valid session-bound voice/control traffic, not
arbitrary UDP or malformed chat. Silence is not a hangup. A single dropped,
late, duplicate, reordered, empty, malformed, or unsupported packet cannot close
a QSO. Close on an authorized local operation, valid remote BYE, exhausted
connection/liveness deadline, selected-transport loss, or unrecoverable owner
failure. Log unexpected termination once with its first cause and bounded
sequence/deadline details; user/scheduler closes remain ordinary events.

Keep the accepted default station limit 20. Proposed heartbeat interval is
10 seconds and timeout 100 seconds, measured with monotonic time. Directory
retry, connect deadline, media inactivity, jitter delay and RF timeout are
separate concepts. Permanent/scheduled links reuse existing reconnect policy;
ordinary or explicitly disconnected calls do not automatically reconnect.

Internal generation fencing discards stale queued work, but cannot identify
every delayed UDP packet after the same endpoint reconnects: legacy peers may
reuse or omit wire identifiers. Require a new validated SDES establishment
before admitting voice, reject inconsistent available identifiers/sequences,
and reset timeline state on establishment. Do not invent a handshake extension
or claim an absolute stale-BYE guarantee when old/new packets are identical.
Cover that residual limitation in tests and diagnostics without imposing an
arbitrary new reconnect delay.

Canonical identity is `(protocol=EchoLink, complete real node ID)`, scoped to
the local node and connection generation. Keep validated callsign metadata
beside it. AllStar and EchoLink stations with equal raw numbers are different
peers. Do not infer EchoLink topology or add EchoLink-only reachability claims
to ASL topology; direct duplicate/self checks still apply. Transitive loops
through networks that publish no topology cannot be ruled out.

Parse the real ID as a positive 32-bit protocol number, not a six-character
string or an ASL alias. Its numeric/wire range is distinct from the configurable
station-count/resource limit.

## 9. Dialing, DTMF and controller behavior

Keep the ASL form: prefix the real EchoLink number, padded to **at least** six
digits, with `3`. Real ID `123456` has alias `3123456`; real ID `1002775` has
alias `31002775`. This is a reversible alias, not a six-digit storage limit.
Examples with default command mappings are `*33123456#` and `*331002775#`.
Do not complete an EchoLink destination eagerly after seven alias digits;
collect until `#` or the source's normal unkey termination. Reuse existing
timeout handling. Resolve the alias before AllStar directory lookup.

Connect, disconnect, permanent connect, pause/reconnect, scheduled groups and
status all resolve to that same canonical identity. Existing `remote_node`
lists can use these numeric aliases without a second scheduler. Callsign
dialing and a directory-search interface remain out of scope.

EchoLink audio carries in-band DTMF. Reuse the shared DTMF implementation,
with an explicit source-rate setup for the 8 kHz decoded input; retain its
existing 48 kHz local-RF behavior. Detect before publishing PCM to the peer's
existing delay ring, then gate that ring's output with the source's detector
state. This avoids a second resampler or lookback queue and preserves ADR
0023/0025 muting. Insufficient configured delay can still expose a prefix.
Current detector/command suppression immediately overrides delayed output.
Also retain the source-span suppression decision alongside buffered audio,
using the existing ring timeline and bounded metadata, so a previously muted
digit tail does not become audible when the current detector releases.
Command termination ends future suppression; it does not retroactively unmute
already inhibited samples. Do not add another audio-delay queue for these tags.
Any required detector contract change is coordinated and tested in all current
consumers; do not write a second decoder in the EchoLink adapter.

Digit events retain their authenticated ingress connection, independent
collector/unlock state, command termination and source-scoped reply. Reuse
configured local-only regeneration and pad testing. Human chat and station
information cannot issue commands. EchoLink is not an ASL `D`-text route:
do not transmit ASL remote-command envelopes as EchoLink chat or invent an
out-of-band DTMF extension. Unsupported `*4` destinations receive a localized
unsupported-route response rather than a false success.

For shared-account conferencing, default to ASL's first-talker policy among
EchoLink stations, with an 800 ms silence release of talker ownership. Keep
local-receiver duplex/mixing policy unchanged. Derive ordinary EchoLink receive
unkey from the reference 320 ms inactivity guard and drain already accepted
jitter/PCM audio before publishing its final unkey. The guard is not an extra
hold after that drained endpoint. Never leave a tail to play at the next key.
Only selected-talker audio produces controller receive activity or DTMF
commands. Suppressed contenders are counted but cannot prolong RF timers,
trigger courtesy tones or interrupt telemetry as received voice. Their valid
human chat remains independently eligible for local text speech. Measure the
800 ms release from the selected talker's last valid packet; hand ownership
over only once its accepted audio has drained, so queued talkspurts cannot
be spliced together at a winner change.
Expose received-versus-selected-talker state separately. Do not create a second
EchoLink RF timeout: the existing controller owns timeout notices, traffic
inhibition and recovery. [R12]

## 10. Audio, jitter and packetization

GSM Full Rate at 8 kHz mono is mandatory. Reuse the released GSM provider's
33-byte/160-sample coding units, not WAV49. Baseline EchoLink packets contain
four coding units and an 80 ms audio span with the RTP-like EchoLink header.
Sequence fields use protocol byte order. Accept the reviewed legal header
variants, including reference clients with zero timestamps or SSRC; do not
treat those fields as a usable clock or sole authenticated identity. [R13]

Support the established optional EchoLib narrowband Speex extension when both
peers advertise it and the selected provider is present. GSM remains available
and is the fallback when Speex is not mutually supported. Do not advertise
Opus, Speex wideband, ULAW or other codecs merely because this product supports
them on IAX. Verify the actual four-frame Speex bit packing: concatenating
independent encoded byte strings is not assumed legal. If necessary, extend
the shared provider's bounded batch/framing operation rather than manipulate
codec bits inside the protocol or change the IAX profile silently. [R14]

Use the current jitter adapter with a per-QSO sequence-derived source timeline,
monotonic arrival timestamps and tested 16-bit sequence wrap. Do not require
nonzero RTP timestamps. Distinguish reorder/loss from a new talkspurt or session
reset; reset codec/jitter state at an actual new segment, not at every packet.
Use the existing 160/180/500 ms minimum/initial/maximum packet-delay policy as
the validation starting point, separate from PCM reserve/target/capacity.

Keep decoder-native PLC enabled and try it first. Publish only unresolved loss
as ordered missing-source spans to the shared PCM ring. Jitter insertions do
not advance codec history; never conceal the same span twice. Preserve a
partially admitted PCM block/gap across backpressure. GSM has no native PLC
in the selected provider, so its missing spans use the existing ring fallback.

The inbound ring alone converts source PCM to native 48 kHz and corrects clock
drift. Egress uses the shared conversion owner and codec provider, preserves
finite tails, and forms legal EchoLink packets outside audio callbacks.
Preserve unsent bytes on a retry; do not encode twice or send stale media in
a burst. No program-mix ring, extra elastic FIFO, new audio filter or unrelated
gain change is introduced. State packetization, jitter and ring latency
separately in diagnostics.

## 11. Human text and telemetry

Classify human chat separately from `oNDATA` station information, protocol
records and legacy loopback administration strings. Only valid human messages
on an admitted QSO enter `HumanMessage`. Bind authenticated account/QSO and
generation independently of any claimed or conference-relayed sender. [R15]

Reuse the bounded origin-ordered speech producer and local PCM input already
merged in PR 54. Callsign attribution says "[callsign] says:"; numeric sources
say "node [digits] says:" using the packaged Fluent catalog and existing
character separation. Body text is literal, never a template or command.
Speech mixes with simultaneous audio, including its sender. Playable text
interrupts ordinary local voice telemetry into its Morse form. Speech failure
skips only that rendition and logs a metadata-only error, not the human body.

Text forwarding is independent of speech success/admission. Use a real
per-protocol serializer, not the current ASL-only formatter for every peer.
Preserve body and represented origin/audience; never forward to the originating
connection or a known original source, and never turn an ASL control frame
into chat. EchoLink chat's sender label does not authorize remote control.

For cross-protocol forwarding, use a faithful destination representation.
An identified EchoLink numeric origin can use its reserved numeric ASL alias;
do not fabricate an ASL source for an unresolved conference-relayed callsign.
A fresh directory match establishes a represented numeric identity for that
claimed callsign, not proof that the person sent the message. It may supply
the destination protocol's sender field and best-effort original-source echo
exclusion; authorization, quotas and cancellation always remain attached to
the authenticated ingress QSO. No matching entry means unresolved origin.
If the destination cannot represent the original source/audience, mark that
forwarding route unsupported and count it rather than invent an envelope,
rewrite the body or accidentally broadcast directed text. Local speech still
works. Document that broadcast networks without end-to-end IDs cannot guarantee
global text-loop suppression; bound repeated local admission without claiming
cryptographic provenance. No new wire loop-marker extension is in scope.

Controller connection/disconnection messages use the existing localized,
polite, serialized telemetry path and reconnection cancellation. Inbound audio
is receive activity for speech-to-Morse interruption, IDs, courtesy selection
and duplex policy. Connection status/heartbeats are not receive activity.
Courtesy tones and hang time apply only to the local transmitter. Explicit
message audiences and source-scoped command replies retain their normal rules.

## 12. Reload, shutdown and failure isolation

Ordinary metadata, policy, gain and timing changes apply through prepared
generations without restarting the host or interrupting unrelated radios/peers.
Prepare changed account endpoints before publication where simultaneous
ownership is legal. Account identity, credentials, transport or socket changes
may require a controlled reconnect of that account; do not log in one identity
twice as a make-before-break shortcut. Failure preserves the prior usable
generation and reports precisely which account could not change.

That preservation is a configuration guarantee, not an uninterrupted network
session guarantee. For an exclusive account replacement, failure reinstates
the old configuration and attempts to reconnect its old account. If its old
credentials or network are no longer usable, report retrying/offline rather
than claiming rollback restored connectivity. Other accounts and radio audio
continue throughout this bounded account-specific interruption.

Disable/replacement gates new admissions, invalidates the exact QSO generations,
cancels their pending speech/commands, stops their owners, drains/quiesces
callbacks and then releases rings, codec/jitter handles, sockets and libraries.
No callback frees prepared media, joins a thread or unloads code. Replacement
of an adapter DSO still requires the controlled restart/module-reload policy;
ordinary configuration reload never hot-loads executable code.

Local speech failure, one peer's full queue, a directory timeout, one malformed
datagram and a missed heartbeat each have distinct diagnostics and bounded
recovery. None may masquerade as an unexplained generic hangup. Bound incomplete
connections, directory work, Proxy frames, per-peer mailboxes and queued media.
Expose/enforce a per-account unauthenticated-handshake budget separately from
the connected-station limit so invalid traffic cannot consume unlimited tasks.

## 13. Status and acceptance

Existing status surfaces gain account registration/transport state, directory
age, full EchoLink ID/callsign/alias, codec/rate, actual versus selected talker,
heartbeat age/deadline, first close cause, retries, media age, input/output
backpressure, malformed/duplicate/late/lost packets, jitter adjustments, native
PLC/ring fallback and text admission/forwarding failures. Distinguish accepted
outbound data from actual sends and remote receipt. Report Proxy head-of-line
stalling separately from network packet loss. No new management server is part
of this feature.

Required acceptance includes:

| Area | Evidence required |
| --- | --- |
| Wire/parser | Independent fixtures for minimum/maximum/truncated inputs, compound SDES/BYE, optional items, Proxy split/coalesced framing, bounded directory decompression and malformed packets without disconnect |
| Directory/access | Login denial, full/delta refresh, stale cache, failover, deny-wins wildcard rules, callsign/address mismatch, duplicate/capacity races and two-account isolation |
| Media | Independent GSM/Speex fixtures, 80 ms packetization, sequence wrap/reorder, ordered losses/native PLC, finite tails, silence/unkey drain, gains applied once and no telemetry/DTMF feedback to peers |
| Text/commands | Account/QSO attribution, source generation retirement, no control-text speech, local-only mixed speech, voice-to-Morse interruption, faithful forwarding/no echo, literal body and seven-digit real-ID dialing |
| Concurrency/lifecycle | A stalled peer/account while others progress, Proxy congestion, idle parking, reload/rejected reload/disable, async attach reentrancy, stop/detach quiescence and callback allocation/lock checks |
| Real interoperability | Inbound/outbound audio, long silence, normal disconnect and reconnect with ASL3, released SvxLink, a conference and current official EchoLink clients; direct and a user-provided standard Proxy |

Treat SvxLink issue 826 as an unresolved conformance fixture, not proof of one
specific cause. It reports official-client SDES/source-port differences and
some failed Qtel calls. Test those real variants; do not forge an official
client's identity or assume adding one undocumented field fixes them. [R16]

Use deterministic released images and focused TDD/coverage checklists first.
Before merge require the full native Debian 13 amd64/arm64 gate, warning-free
developer docs, packaging/install checks and 100% production line/branch
coverage on amd64. No automatic Debian 12 work. Publish independent Debian
packages through the existing signed repository only after approved releases.
The standalone package remains installable without either Asterisk adapter.

Hardware-free fixtures cannot prove public-network interoperability. Obtain
permission before any account login, external test call, node installation or
restart. Record tested client versions, transports, codec and observed result;
leave unavailable combinations explicitly unverified. "Full interoperability"
is not a promise of unlimited peers, inaccessible networks or undocumented
future extensions.

## 14. Approval boundary and work left to the implementation plan

The proposed choices needing review with this design are the independent Rust
protocol adapter, standard TCP Proxy support, 100-second heartbeat timeout,
untruncated terminated EchoLink dialing, source-rate reuse of the DTMF provider,
and the explicit directory freshness/resource policy. They are not prior owner
decisions merely because they are written here.

After approval, update ADR 0007 and the EchoLink wishlist entry with the scope,
three factual corrections, chosen boundaries and status **planned**. Record any
necessary host/DTMF ABI decision before code changes. The implementation plan
chooses exact function-table layouts, tested queue/record size bounds, package
names/version bumps and fixture ownership; it may not expand scope or weaken
the invariants above. No product code or implementation plan is included here.

## References

- R1: [Official EchoLink Proxy](https://www.echolink.org/proxy.htm).
- R2: [EchoLink Proxy protocol](https://www.svxlink.org/doc/echolink_proxy_protocol.html), also [pinned source document](https://github.com/sm0svx/svxlink/blob/595cf9bb3325be753d05b219bdd39feab6a22b33/src/doc/echolink_proxy_protocol.txt).
- R3: [ASL released configuration and heartbeat source](https://github.com/AllStarLink/app_rpt/blob/d3929751f102c37b3c8b4e1b0040ec19d907cfea/channels/chan_echolink.c), [shipped configuration](https://github.com/AllStarLink/app_rpt/blob/d3929751f102c37b3c8b4e1b0040ec19d907cfea/configs/rpt/echolink.conf).
- R4: [Official current logins](https://www.echolink.org/logins.jsp), read 2026-10-28; IDs above 999999 are present.
- R5: [ASL released command completion](https://github.com/AllStarLink/app_rpt/blob/d3929751f102c37b3c8b4e1b0040ec19d907cfea/apps/app_rpt/rpt_functions.c#L109-L117).
- R6: [Official account/access policies](https://www.echolink.org/access_policies.htm), [new-node FAQ](https://www.echolink.org/faq_new_node.htm).
- R7: [EchoLib build dependencies](https://github.com/sm0svx/svxlink/blob/595cf9bb3325be753d05b219bdd39feab6a22b33/src/echolib/CMakeLists.txt), [QSO implementation](https://github.com/sm0svx/svxlink/blob/595cf9bb3325be753d05b219bdd39feab6a22b33/src/echolib/EchoLinkQso.cpp), [license](https://github.com/sm0svx/svxlink/blob/595cf9bb3325be753d05b219bdd39feab6a22b33/COPYRIGHT).
- R8: [ASL released gain/telemetry definitions](https://github.com/AllStarLink/app_rpt/blob/d3929751f102c37b3c8b4e1b0040ec19d907cfea/apps/app_rpt/app_rpt.h), [telemetry mode behavior](https://github.com/AllStarLink/app_rpt/blob/d3929751f102c37b3c8b4e1b0040ec19d907cfea/apps/app_rpt/rpt_link.c), [manual](https://allstarlink.github.io/config/rpt_conf/#eannmode).
- R9: [ASL directory implementation](https://github.com/AllStarLink/app_rpt/blob/d3929751f102c37b3c8b4e1b0040ec19d907cfea/channels/chan_echolink.c#L3034-L3624), [EchoLib directory implementation](https://github.com/sm0svx/svxlink/blob/595cf9bb3325be753d05b219bdd39feab6a22b33/src/echolib/EchoLinkDirectory.cpp).
- R10: [Official firewall requirements](https://www.echolink.org/firewall_solutions.htm).
- R11: [EchoLib RTCP parser](https://github.com/sm0svx/svxlink/blob/595cf9bb3325be753d05b219bdd39feab6a22b33/src/echolib/rtpacket.cpp).
- R12: [ASL talker arbitration](https://github.com/AllStarLink/app_rpt/blob/d3929751f102c37b3c8b4e1b0040ec19d907cfea/channels/chan_echolink.c#L4278-L4408), [receive pacing](https://github.com/AllStarLink/app_rpt/blob/d3929751f102c37b3c8b4e1b0040ec19d907cfea/channels/chan_echolink.c#L2451-L2546).
- R13: [EchoLib voice framing](https://github.com/sm0svx/svxlink/blob/595cf9bb3325be753d05b219bdd39feab6a22b33/src/echolib/EchoLinkQso.h), [ASL transmit framing](https://github.com/AllStarLink/app_rpt/blob/d3929751f102c37b3c8b4e1b0040ec19d907cfea/channels/chan_echolink.c#L1861-L1886).
- R14: [EchoLib Speex handling](https://github.com/sm0svx/svxlink/blob/595cf9bb3325be753d05b219bdd39feab6a22b33/src/echolib/EchoLinkQso.cpp#L447), [released module manual](https://github.com/sm0svx/svxlink/blob/595cf9bb3325be753d05b219bdd39feab6a22b33/src/doc/man/ModuleEchoLink.conf.5).
- R15: [EchoLib chat and information handling](https://github.com/sm0svx/svxlink/blob/595cf9bb3325be753d05b219bdd39feab6a22b33/src/echolib/EchoLinkQso.cpp#L342), [ASL text handling](https://github.com/AllStarLink/app_rpt/blob/d3929751f102c37b3c8b4e1b0040ec19d907cfea/channels/chan_echolink.c#L2319-L2394).
- R16: [SvxLink issue 826](https://github.com/sm0svx/svxlink/issues/826), an unresolved report, not a protocol specification.
- [ASL3 EchoLink operating guide](https://allstarlink.github.io/adv-topics/echolink/) and [configuration reference](https://allstarlink.github.io/config/echolink_conf/), with source-verified corrections above.
