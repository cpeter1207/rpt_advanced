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
Incoming notifications transfer an owned, generation-bound QSO through the
selected bounded control executor without waiting on the provider callback.
Submission rejection leaves provider ownership unchanged. Accepted control
work revalidates the pending QSO before admission and retires it on failure.
The task may finish before submit returns; only the original acknowledgement
is accessed after acceptance. Host teardown drains accepted handoffs.
The prepared account retains its context and dynamically loaded provider; a
failed stop retains foreign state rather than unload code beneath notifications.
The provider's sized PeerInfo includes the observed incoming SDES endpoint,
including port zero for Proxy framing. This supplies the typed incoming request
without substituting a directory address or assumed port for an observation.
Outgoing calls have no observed incoming endpoint. Shared owners retain that
absence explicitly; incoming handoffs require the real observation. Dial adopts
only the requested real station ID and owning local account, retiring mismatched
returned QSOs before reporting failure.
The network/media operation group remains unpublished while this record and
the shared host glue are integrated; lifecycle-only users do not consume it.
The product's existing configuration visitor carries a borrowed provider account
record and its configured secrets-file path. Product code resolves every account
before any host visit; hosts prepare or copy the public record synchronously and
load credentials relative to their actual configuration file. No host reparses
the controller schema or retains parser spans. These fields belong to the same
coordinated, pre-release host9/product7 and SONAME2 transition.
Nonexclusive account policy and directory metadata updates use the existing
bounded account command port. They retain routing/talker generations, completed
directory entries and in-flight operation tokens. Changed registration metadata
is renewed after the current transaction; its old reply cannot erase that request.
Freshness changes preserve the original snapshot publication time. Only identity,
credentials and the selected transport's effective fields require an exclusive
account handoff; inactive transport fields do not interrupt calls.
Live gain/timing revisions wake each existing serial owner. Only that owner
updates codecs and QSO timers; previously gained PCM/prepared RTP and elapsed
timer anchors remain intact. An exclusive account replacement closes admissions,
joins its notification/network/peer owners off-executor, then starts a new
generation. Retained old handles remain readable for identity but reject media.
Direct sockets may be reused only after that quiescence barrier; weak socket and
driver records do not keep disabled accounts or peer owners active. Failed open
restores the prior configuration and attempts reconnect, never claiming online
status without a successful directory operation. Disabled activation retains its
host callback for later enable; permanent stop cannot be reversed.
Both host adapters use one shared account set for private-credential preparation
and stable notification contexts. Disabled records do not read credentials or
create foreign accounts. Incoming notifications transfer through the selected
host-admission control owner without waiting on provider Tokio work. A full
queue leaves the provider's QSO untouched. Stop gates notifications, joins account
callbacks, then drains accepted host work before the product can be released.
This is one host-lifetime admission executor using the existing backend, never
an executor per account/peer or a second packet/audio queue.
Reserved operator aliases route to Echo before any ASL directory lookup. An
incoming Echo QSO keeps its real protocol-qualified identity while its canonical
operator alias supplies the existing controller link-policy key. It enters
common limits/deny/topology policy only after provider admission; a source IP
alone is never Echo admission proof. IAX retains its directory/source check.
Standalone retains accounts in its stable host-services context; Asterisk owns
an equivalent table/context instead of using the null-context template at
runtime. Preparation precedes product startup, activation follows it, and
account stop plus admission drainage precedes product shutdown. An accepted
callback captures the validated product directly rather than reacquiring the
host lifecycle lock. Outbound Echo destinations use that local account and
check cancellation before and after dial. No Asterisk channel is constructed.
These startup/stop and transactional host-reload paths are implemented on the
feature branch; complete provider capability publication remains pending.
The sized descriptor's prepared-apply tail uses an inactive account handle as
the provider-owned validated configuration/credential snapshot. Applying it
exchanges that snapshot with the target's old settings, retaining both handles
for rollback without copying the protocol schema into either host. Preparation
is side-effect-free; apply executes off executors under ordered lifecycle
serialization. The host requires the complete network group including this
operation. Lifecycle-only sized prefixes remain usable; older incomplete
network groups fail closed. Descriptor version/SONAME stay1 because the
operation is an appended, sized capability requirement, not a prefix change.
The shared host account manager stages opaque snapshots without opening another
admission executor. Commit gates outbound lookup and incoming handoffs, applies
product settings, then account snapshots. Account failures restore snapshots in
reverse before restoring the product. A product error also triggers restoration:
it can occur after publication. Gates reopen only after successful restoration;
failed rollback remains visible and fail-closed. Stale staged revisions cannot
commit over a newer configuration. Queued admissions recheck the gate before
claiming their QSO. Both hosts retain the last successful configuration for
restoration. Standalone discards staged radio settings before reopening the old
configuration. Account addition creates stable gated callback storage before
activation; rollback stops only that new account, not the shared executor.
Disabled records do not require credentials. Removed accounts are disabled
reversibly before commit and permanently retired only after success. Failed
retirement retains callback storage and reports a separate error. Map locks are
control-only and never span foreign calls or audio callbacks.
Remote-command selection rejects EchoLink aliases before creating ASL digit
envelopes or changing the source collector's destination. A completed DTMF
request receives a localized unsupported-route explanation through the normal
source-addressed, polite telemetry path. No EchoLink chat command is invented.
The sized peer-source read result carries a fresh, protocol-scoped numeric
identity for its single human-text callback. Zero means unresolved. This tail
does not change the sender/body callback or authenticated QSO identity; hosts
copy it only after validating the complete result extent. EchoLink resolves a
represented callsign from the current directory snapshot, without using that
claim for authorization. Both host bridges require the new result extent in
this coordinated pre-release host9/product7 transition; descriptor versions and
SONAMEs remain unchanged. An incomplete record fails closed before any write.
The product copies that turn's one bounded text callback, validates the returned
record, then publishes literal content and represented identity together. Multiple
callbacks or an identity without text are contract errors. Text forwarding uses
the current connection's actual protocol/capability, not a process-wide callback.
Resolved Echo senders use their full reversible alias in ASL envelopes; unresolved
claims remain eligible for local speech and Echo chat, but not invented ASL IDs.
Echo chat preserves literal sender/body and has broadcast audience only. Directed
ASL text is never widened to Echo broadcast. Local and ingress/represented source
identities are excluded from forwarding. The appended host human-text operation
passes structured spans to the existing provider serializer on its serial owner;
it is not another ASL envelope, codec or speech path. Chat rejection drops only
that message, not the audio connection. Both hosts update together under the
coordinated pre-release host9/product7 descriptor-extent check, with SONAME2 unchanged.
EchoLink connection notices use the configured off/node/callsign/both identity
style, a validated QSO callsign and full real node number. Both says the node
before the callsign. Catalog fragments retain separate text, speech and Morse;
ordinary local/third-party wording and reconnect cancellation remain shared.
The account's telemetry mode is an additional per-QSO eligibility filter, never
an audience expansion. Off suppresses routed telemetry; on permits it; follow
follows local telemetry eligibility; demand opens ASL's two-minute window only
at typed command-response boundaries. Voice, chat, refreshes and heartbeats do
not extend it. An authorized runtime demand selection opens its initial window.
Windows live with exact reply tokens, not reusable node numbers. Control publishes
an atomic permission bit; audio reads no timer, account map or lock. IDs,
courtesy tones, hang time and synthesized human chat remain excluded from peers.
Runtime selection uses the existing typed command/admin path and optional
`link_command_echo_telemetry` mapping with no default prefix. Modes are 0 off,
1 on, 2 follow and 3 demand; a fixed account policy rejects runtime changes.
Echo operator spellings are canonicalized before command admission, configured
group/schedule duplicate checks and storage, direct typed scheduler construction,
access-list comparisons and courtesy assignment. The shared identity parser is
the only alias parser at these boundaries. Retries and status inherit that same
key; leading-zero AllStar identities are not rewritten. Alternate Echo spellings
cannot establish a second connection or bypass a configured deny rule.

Echo diagnostics use sized metadata-only account and peer observation records in
the shared descriptor. Account queries run on host control and use the existing
bounded account command owner; no status sampler or per-packet snapshot allocation
is added. Peer observations run only on the sole serial peer owner. Both hosts
publish copied observations through their existing product status/logger ports.
The complete network group requires both observation operations; an incomplete
pre-release network table fails closed. Lifecycle-only preparation remains valid.
The descriptor and SONAME remain 1; both current hosts and the unreleased provider
advance together. Jitter/PLC spans use the 8 kHz source clock. Queue admission,
complete local socket sends and unproven remote receipt are separate concepts.
Proxy output counters include active/retired head-of-line stalls; a canceled
partial write retains its cursor, and retirement cannot claim a successful send.
The product's sized peer-observation tail carries copied EchoLink metadata.
Both hosts map their native observation records explicitly; an IAX record is
never cast to the larger product record. The serial owner captures the first
failure before cleanup, then publishes atomic copies for status and one error
log. Bounded category details are escaped when formatted; human text and
credentials are excluded. An unavailable ingress capacity is not evidence of
an empty queue. The current pre-release host/product extent checks advance
together, without changing their descriptor versions or SONAMEs.
Account observations include the complete configured callsign without requiring
a QSO. Both hosts expose one optional control-only status text callback at the
current host table tail. It uses the existing borrowed text sink, not a second
copy of the provider record. Missing accounts emit no text; query failure is
unavailable, not an offline assertion. The paired current host table extent
changes; the descriptor versions and SONAMEs remain unchanged.
Account health notifications enqueue coalesced metadata work on the existing
host control owner. They never query or log while inside the provider callback.
Execution rechecks admission/reload gates and the exact account generation.
Repeated failures log at most once per30seconds; online recovery or a new
generation resets that episode. Shutdown gates producers, joins callbacks and
drains accepted control work before releasing logger/backend storage.
Per-QSO ingress observations use weak route handles: querying reserved/queued
slots and charged bytes cannot keep a producer open or borrow its consumer.
The sized peer snapshot includes actual full-credit drops and retained bytes;
both hosts copy those values instead of fabricating empty ingress counters.
Human-text submission rejection remains separate from audio health and emits
only a rate-limited metadata error, never the sender/body or a hangup.

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
