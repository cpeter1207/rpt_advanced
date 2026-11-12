# ADR 0044: Private phone peers and bounded RF presentation pacing

Status: Accepted by owner 2026-11-09; media prerequisites released. Macro and
private-call foundations implemented; phone/RF integration pending.

## Context

Autopatch/reverse patch must use the ordinary peer processing path while
half-duplex radios retain receive opportunities. Intentional RF interruptions
create presentation backlog independently of network jitter or clock drift.
Calls also require private authentication, commands and replies before any RF
audio attachment. Existing numeric AllStar identities and public peer media do
not supply those contracts.

## Decision

Implement the [approved phone design](../../../docs/superpowers/specs/2026-11-09-phone-patch-and-half-duplex-design.md)
through the existing product/control/media owners, using native tools and the
repository gates. Provider accounts, mailbox identities, opaque call generations
and radio-node identities remain distinct. SIP/IAX signaling, codecs, packet
jitter and telephone events remain off audio. Phone ingress uses existing inbound
rings and shared DSP; no AllStar handshake/topology is sent to a phone provider.

Incoming calls authenticate mailbox/PIN and start command-only. Explicit `*3`
admits permitted transceive media. Private replies require a call-addressed sink
independent of attachment. `*4` addresses local/remote commands, with phone-only
literal quoting and source-scoped grants. Remote private/protected operations
require verified origin/reply/lifecycle support; ordinary legacy digit forwarding
alone does not establish per-call isolation.

Add one bounded RF programme presentation FIFO on the existing transmit owner,
after fresh peer processing/mixing and before the controller's local RF mix.
Fresh network fanout retains its current source timeline. Do not queue final
PCM, telemetry, regeneration, safety output or access signaling. Preserve source
age through the FIFO and converter; revoke retained backend history with media.

The RF owner may use an explicitly prepared manual-pacing capability in the
shared samplerate provider. This changes intentional presentation consumption,
not the 48 kHz DAC rate or inbound source-clock recovery. Ordinary conversion
retains its nominal ±1000 ppm range. A sized optional descriptor tail negotiates
pacing; missing capability fails explicitly. Prepare ratio limits/storage off
audio and prove processing/reset allocation guarantees across that range.

A single sample-clocked RF arbiter schedules warning, STE, actual off/settling,
usable listening and rekey. VAD and manual admission both obey receive checks;
manual actions never override VAD. Instantaneous pause evidence is distinct from
qualification hangtime. Physical unknown is not evidence of a completed check.
Retain normal safety/identifier priorities and report timing conflicts honestly.

Direct-trigger DTMF macros run on the serialized control owner. Explicit `#`
boundaries and comma deadlines do not reuse the human timeout/auto-completion
parser. Delayed steps retain source/generation/private reply and recheck grants.
No login/unlock credentials occur in expansions; self-revoke clears only the
executing source. Existing typed link macros remain separate.

## Consequences and verification

The first private-source slice admits a typed `PeerAddress::PhoneCall` and keeps
provider account, mailbox, call token, provider/call generations, and actual
product-assigned ingress distinct. Numeric node and operator-alias getters now
return `Option`; current Rust callers explicitly handle the node case. A phone
never receives a public ASL node alias. Ordinary ASL/Echo identities and wire
behavior are retained.

The existing sized `rptadv_peer_info_v1` record has one compatible provider-account
span tail. Its original 80-byte prefix remains valid for node peers. Phone
admission requires all 96 bytes, protocol 3, explicit private-call capability 32,
and nonzero account/call generations. Both current hosts copy only the caller's
declared capacity. Borrowed inspection does not adopt a pending handle.
Phone local bindings use the configured `NodeId` semantics and 63-byte bound,
including named nodes; ASL/Echo retain their existing decimal/wire constraints.
No host table slot, descriptor revision, or SONAME changes: old products reject unknown
phone protocol/capabilities, and new products reject incomplete phone metadata.
This additive record extension does not authorize mixing an incompatible SDK
cohort elsewhere.

The product's bounded private call owner retains its handle without directory
lookup, ASL handshake/topology, radio binding, or a radio-peer slot. Calls start
unauthenticated. The serialized verifier checks the configured salted Argon2id
hash before publishing the mailbox profile. Invalid records remain unavailable;
three failed attempts or the absolute 60-second admission deadline retire the
call. Hashes and PINs are redacted from diagnostics. Login never grants node
administration. Selected-node
collection retains the exact private reply token and actual source, suppresses
idle RF regeneration, and rejects legacy remote forwarding without the required
private-origin/reply/lifecycle profile. Retiring the source ends queued replies
and source-owned grants/collectors; committed reload and stop retire all calls.

Private status preparation reuses the existing status formatter and producer
PCM stream. A bounded per-call queue feeds the existing `Playback` renderer,
independently of `NodeController`, RF demand, and public peer fanout. Its source
fence runs before and after rendering. Other private report families fail
explicitly pending their prepared-stream routes. Phone tokens cannot enter the
ordinary radio telemetry queue, including stopped-macro and route diagnostics;
those paths return an explicit rejection until they have a private sink.
Ordinary phone commands and macros retain the exact call token through lookup,
dial, initial permanent retries and final publication. Ending the call revokes
unfinished work; completed node-link commands keep their existing recovery rules.

The inert telephone grammar emits typed routing, literal-digit and manual-key
requests. It implements `*3`, `*4`, one-symbol `**` quoting and the existing
collection bounds; syntax alone grants no authority or RF attachment. Its
caller must retain the verified source and private reply on every dispatch.
Provider egress, the private digit/media worker,
grammar integration/configuration/allowlists, explicit `*3` media attachment,
per-handle DSP ownership, and negotiated remote lifecycle remain dependent work;
this foundation does not enable a usable phone provider or claim complete Task 3.

ADRs 0003/0025/0035 retain exclusive inbound conversion/clock recovery while
allowing this narrowly scoped RF presentation operation. There is no new drift
estimator, audio worker, mixer, or DSP engine. EQ/dynamics/limiting remain shared
FFmpeg graphs. Transport/control and generation reclamation keep their existing
owners under ADRs 0023/0026/0037/0038.

Prepared pacing is released in samplerate adapter `v0.2.0-alpha3`; common
source-rate Earshot analysis is released in Radio `v0.1.0-alpha.11`.
Source-timed ring rendering is released in ring `v3.0.0-alpha5`. All three
providers passed their full native Debian 13 gates; their signed APT indexes
and both architectures' runtime/dev package hashes match the release assets.
Consumers must check each optional descriptor tail and use the released
versioned libraries. The consumer now preserves timed source coordinates and
real/PLC/gap/muted/zero-extension classifications through the existing ring;
opaque DSP output explicitly loses exact provenance. Phone policy uses the
same ring with a 160 ms reserve/target, 300 ms capacity and 20 ms producer
prefix, without inheriting the ordinary peer's 260 ms target. This policy alone
does not qualify complete source age or enable phone audio attachment.
SDK `v0.1.0-alpha33` adds structured output feedback;
its signed packages and native installation checks are verified. Builds using
the alpha33 descriptor headers require matching alpha33 build/runtime floors:
the existing complete-record check safely rejects the smaller alpha32 table.
Controller feedback consumption, phase/age qualification and the RF
presentation scheduler remain separate work.

The shared jitter client's prepared phone constructor selects an 8 kHz, 20 ms
packet profile with 20/40/100 ms delay bounds through the existing jitter ABI 1.
Ordinary peers retain 160/180/500 ms. The dependency-free Rust wrapper retains
an explicit Rust 1.85 minimum independently of RPT's Rust 1.88 toolchain; callers
select its new immutable source revision. It uses the released DSO's existing
descriptor and lifecycle, retaining the current SONAME/package contracts. The
inbound ring continues to own source-clock recovery. Source-age/RF qualification
still depends on the original-time, DSP, retirement and physical-feedback contracts.

The released current-node RF/IAX macro foundation has source-owned cancellation,
asynchronous attachment fencing and explicit command boundaries. IAX
`v0.1.0-alpha10` adds outgoing provider account fields and verified call lifecycle;
controller composition, registration and incoming provider accounts remain open.
Phone routing, RF deadline/age bounds and their integrated reload/unload behavior
still need implementation and automated evidence.
Actual provider/NAT and physical switching/STE/listening acceptance remain
separate. The [implementation plan](../../../docs/superpowers/plans/2026-11-09-phone-patch-implementation.md)
records task ordering; this ADR does not claim completion or authorize deployment.

Host ABI 10 now has an optional `radio_feedback_bind` tail, preserving the
complete 400-byte predecessor prefix. This is needed because `radio_activate`
accepts the established separate RX/TX functions and contexts; its function
signature and the existing metadata callback cannot carry an additional sized
SDK port. Both hosts copy the stopped-only `UrpRadioFeedbackPort` and forward
it through the existing native station or acknowledged `RPFB` channel option
before startup. A partial table is rejected, and an absent tail remains an
explicitly unavailable capability. Binding does not turn requested or serviced
PTT into physical readback, add a hardware owner, or enable RF receive windows.
The callback/context remains owned through checked radio retirement, including
failed activation. DirectCallbacks remains ABI 6; no SONAME change is required.

## Released IAX provider cohort adoption — 2026-11-11

The controller now selects IAX `v0.1.0-alpha11`
(`7dd0f98f21c4982c2d71e7f7ba80fe2e6e9a0994`) and requires its development
and runtime packages. That release provides shared-listener provider account
configuration, registration, incoming authentication, account dialing and timed
media contracts. These provider capabilities supersede the alpha10 prerequisite
status above; their controller composition remains pending.

Existing numeric-only listener creation explicitly supplies an empty account
list. Consumer fixture tables declare their unused optional provider/timed slots
as absent. The generated complete-record size checks and package floors reject
an older runtime cohort before copying its table. No ABI revision or SONAME is
changed, no alternate protocol implementation is introduced, and this adoption
does not enable phone accounts or switch existing calls to timed polling. Timed
media requires genuine ring/DSP/RF retirement integration before activation.

## Public account inspection — 2026-11-11

The existing synchronous configuration visitor now borrows sized public phone
account records through a pointer/count tail on its node record. Global account
labels are grouped by their explicit local node, including disabled complete
accounts. The records contain provider settings and a secrets-file path, never
secret contents; all spans expire when the visitor returns.

This reuses the existing product table entrypoint without a descriptor revision
or SONAME change. Existing node fields keep their layout. Current consumers and
fixtures rebuild from the shared header; exact-size validation rejects mixed
product/host records, checking the prefix before reading the new tail. Initial-alpha packages must ship a
matched product/host cohort rather than retaining a legacy parsing path.

Standalone copies these records into separate account values, with diagnostic
formatting omitting server, username and secrets path. Radio configuration
identity and lifecycle ownership are unchanged. Inspection does not register an
account, open a transport, load secrets or activate phone media. Provider
composition, call lifetimes and the remaining Task 4 acceptance are still pending.

## Physical feedback consumer prerequisite — 2026-11-11

The product binds the existing optional feedback port before activating its sole
radio TX owner. The stable callback context survives binding/activation failures
until checked radio retirement; neither host adds a second hardware owner.
Only the complete SDK33 physical prefix is copied. An absent, malformed,
wrong-frame, repeated-position or changed-lifecycle record is unavailable for
that callback; a previous OFF observation cannot be reused.

The existing bounded TX meter snapshot carries these separate facts to station
control: physical duplex, controller request, rendered signaling demand,
serviced output, independent readback, acquisition age and the radio's existing
coarse transmitter state. Service and readback require their own validity bits,
the current nonzero request and lifecycle, consistent acquisition positions,
and acquisition age no greater than that callback's native frame count.
This conservative diagnostic freshness rule is not a measured switching profile.
Default false, completed GPIO writes, and logical controller duplex cannot
substitute for independent physical readback or physical duplex declaration.

This prerequisite creates no receive window and does not claim settling,
STE/rekey completion, DAC presentation time, or the five-second deadline.
The future arbiter must consume qualified current evidence with its explicit
freshness/settling policy. Source admission mapping, conservative DSP delay and
history retirement, RF FIFO/pacing, and both-host composed validation remain
required before enabling that behavior. No queue, thread, hardware query,
allocation or clock read is added to either audio callback.


## Ordered source-phone digits — 2026-11-11

The optional sized source-sink tail now carries one private telephone digit with
explicit acceptance/refusal. The provider retains a refused symbol ahead of
later PCM and retries that same symbol. Product validation accepts only telephone
symbols and one unit of dispatch credit. Only a typed private phone receives the
callback; ordinary ASL/Echo source owners cannot route it into public commands.

A source-capable phone retains the existing async/prepared media requirements and
private-call identity. Its serial owner must consume one source stream for PCM
and digits, not poll legacy and source APIs for the same events. Provider code
checks the sink size through the new nullable tail before calling it. The old
sink prefix, source result, host table, ABI revision and SONAME are unchanged.
This capability does not authorize RF attachment or qualify absent source timing.

## Prepared private receive generations — 2026-11-11

An authenticated exact-call control owner can now prepare one receive generation
using the existing phone-policy inbound ring and optional shared processing
graph. A bounded command on the same private owner queue transfers its unique
producer to the existing serial provider reader. Preparation is inactive until
that transfer is adopted; it is not attachment, target authorization, RF readiness
or successful transmission. No extra provider poller, per-call thread or PCM
history is added. A live generation is busy rather than implicitly replaced.

Original provider receipt time is kept separately from the ring's exact delayed
source coordinate. A fixed 64-entry sidecar coalesces original times into 20 ms
source regions, retaining the earliest time; fractional coordinates also consult
the following source position. These are conservative age bounds, not invented
wall-clock conversions or exact DSP provenance. Live metadata collisions refuse
input before PCM acceptance, and dispatch credit is only the prefix the existing
ring accepted. Missing birth, an earlier admission, foreign epoch, unmapped
startup, gaps and PLC do not acquire fresh age evidence. An opaque processing
graph continues to report unknown provenance.

Digits, source loss/discontinuity, explicit media revocation, consumer retirement
and exact call retirement fence retained ring and graph output. A digit retires
the receive generation without disconnecting the authenticated logical call.
Re-admission is explicit through the control owner, uses a fresh generation and
cannot reclaim provider-held PCM from the earlier admission. Both block and
sample output recheck the fence after processing.

Production attachment/manual commands remain explicitly unavailable. Remaining
requirements include typed phone mixer ownership, in-band DTMF onset/muting,
two-sided target policy, reverse-call limits, admission/VAD/hangover behavior,
bidirectional isolation and actual RF age/deadline scheduling with qualified
processor and physical feedback. Prepared-media tests are not evidence of those
capabilities or of live provider/RF acceptance.

## Native SIP media boundary, 2026-11-11

The standalone `phone::media` boundary is an independently testable transport
component, not an activated provider. It negotiates one unencrypted RTP/AVP
G.711 stream at 8 kHz mono and 20 ms, preserving packet sequence, timestamp
wrapping and arrival order for the existing jitter owner. SDP is parsed by
`sdp 0.8.0`; RTP headers by `rtp 0.13.0` with `webrtc-util 0.11.0`.
Those leaf crates use MIT OR Apache-2.0 licenses and the selected native graph
builds with the repository's Rust 1.88 toolchain. The component opens only the
selected released dynamic G.711 encoder and decoder, not the full codec catalog.

Each media owner retains one socket, bounded datagram storage, 16 telephone-event
identities and the two selected codec contexts. It does not add an OS thread,
Tokio runtime, PCM ring, clock estimator, jitter owner or DSP path. Callers must
place it on the existing shared executor, preserve source positions through
jitter/decode and retire it with the call generation. A connected UDP socket
accepts only the negotiated media endpoint. RFC 4733 onset and first end remain
independent of PCM admission; repeated ends do not repeat command digits.

SRTP, SDES/DTLS, ICE, RTCP multiplexing, additional codecs and non-20-ms profiles
are not advertised. Unsupported security attributes are rejected rather than
downgraded. RTCP/session reports, outbound telephone-event scheduling, incoming
SIP admission, signaling, jitter/PCM orchestration, in-band detection and complete
call lifecycle are separate pending integration work. The existing product DTMF
provider remains the in-band detection owner. This boundary does not imply a
secure media session, a usable account or a completed Task 4.

The selected signaling candidate `rsipstack 0.8.2` and latest published `0.8.4`
retain unbounded transport, transaction, incoming-call and state queues. A capped
outer socket does not bound the per-transaction queues. Production SIP activation
therefore requires a qualified bounded admission/control contract; timer,
cancellation and teardown must progress even under packet saturation. No
rsipstack dependency is enabled by this media-only checkpoint.

Conservative receipt bounds are carried on the timed output sample separately
from exact source provenance. An explicitly qualified processor-history contract
may retain an age bound without claiming an exact source coordinate or real/PLC
classification. The current opaque processor clears both independently. The
input trait can also expose a borrowed exact private-call reply token for typed
mixer admission; ordinary inputs expose no such token.


### Native peer programme-bound capability

The controller processor record keeps its original ABI1 ownership prefix and
adds an optional sized programme-buffering query. The native adapter forwards it
to the released SDK query on the same uniquely owned graph and serial audio
owner. Prefix-only callers remain valid, partial callback tails are rejected,
and unknown extension bytes are untouched. There is no second graph, observer
thread, hardware operation or timestamp derived from SDK frame counters.

The callback exposes actual prepared and last-live-block history bounds and
validity flags. Setup warmup is included in SDK counters; consumers must compare
actual deltas and retain original source receipt bounds independently. An absent
or rejected query remains unknown. The existing borrowed private-call identity
is forwarded through processing unchanged.

This capability alone does not implement RF scheduling or history retirement.
Asterisk's existing upstream channel audiohook still owns its graph: its
LINK_ATTACH envelope does not expose that owner's timed receipt/history query
or graph-retirement transaction. A second product graph would duplicate DSP and
is not a workaround. Both-host qualification requires a genuine channel-owner
SDK extension and release; current Asterisk processing stays unqualified.


### Native conservative original-age history

A timed private-phone processor prepares compact metadata beside its existing
graph, before audio publication. Ordinary peers allocate no history. At native
48 kHz, twenty-millisecond buckets retain the earliest actual original receipt
bound and source generation, never PCM or a time derived from output counters.
The array covers at most 24,000 frames with at most 26 buckets. This is a local
metadata horizon, not a claim that the complete ring, DSP, RF FIFO, converter,
backend and physical path meets the end-to-end age cap. The real SDK prepared
maximum remains unchanged even when it exceeds this horizon.

Only a fresh, coherent, single-sample SDK observation with all required validity
flags can qualify output age. The current delay must fit both the real prepared
maximum and retained metadata. Exact post-DSP source provenance remains unknown.
Unknown input, PLC, missing intervals, mixed source generations or clock epochs,
adapter padding, failed/stale observations and pre-admission history cannot
become fresh programme. Buckets can retain a slightly older bound or withhold
qualification rather than underestimate age.

Block rendering, idle processing and failures invalidate the metadata window.
Resumption requires an actual SDK counter baseline and newly qualified source
history. These counters establish continuity only and never reset source birth.
The single serial processor owner performs queries and bounded metadata updates
without new threads, heap allocation, clocks, locks or hardware operations on
audio. Revocation still fences the exact phone media generation; graph owners
are reclaimed off audio after quiescence, never reset speculatively in a callback.

## Released bounded ring cohort — 2026-11-11

The controller selects released ring `v3.0.0-alpha6` at
`360e7312a4a367418ddac04bd81b8996d8a0ae80` and requires Debian
`3.0.0~alpha6` or newer. This retains ring ABI 3 and its SONAME while reducing
prepared converter workspaces and tagged-sample storage. Official amd64 runtime
and development package hashes were verified before isolated consumer tests.
The unchanged 2000-peer/48 kHz admission workload remains required; no fixture
rate, peer count or queue capacity is reduced. The constrained runner allocator
settings remain test-only and do not change production process defaults.
## Native IAX account boundary, 2026-11-11

Standalone now wraps the released alpha11 listener account operations separately
from numeric ASL dialing. A provider-only listener has no invented numeric node
alias. Account labels, configured local binding, transport login, literal secret,
presentation caller fields and destination remain distinct. Successful
registration copies the actual granted interval and endpoints; successful calls
retain their exact account/call generations and one owned provider handle.
Malformed successful receipts retire that unique handle rather than leaking it.

These wrappers add no registration scheduler, listener poller, worker, mailbox
grant or RF attachment. The mixed admission operation consumes one stream and
preserves the existing numeric policy callbacks. Product activation, same-process
reload and timed private-media composition remain pending. Alpha11's immediate
single-listener account replacement cannot implement a multi-listener/product
reload transaction: failures after an earlier replacement cannot restore retired
call generations. Activation must use a prepared, reserved provider transaction
and the product's genuine commit boundary, not sequential mutation with rollback.

## Typed mixer publication and queued-block retirement — 2026-11-11

The existing mixer accepts a typed public identity, or a private phone slot
checked against the full account/call generation, actual ingress and exact
authenticated reply token carried by its phone-policy input. A private slot
never acquires a decimal alias, public last-keyed name or ordinary telemetry
destination. Ordinary AllStar/EchoLink aliases and routing remain unchanged.

The existing node-lifetime media worker owns one small retirement domain reused
by successive mixer preparations. Exact ring-generation and call fences register
with that domain on control, deduplicated by node ownership; repeated refreshes
do not add history. Configured node ownership bounds registrations, and committed
reload retires calls before replacing their node ownership. Registration uses an
atomic release sequence with retirement so neither ordering can miss notification.
Hardware processing and dispatch read only prepared fences and atomic revisions.

Each queued composite block records which prepared sources actually contributed
to its peer mix. The dispatcher validates those source fences once, then checks
one node revision per destination. A later revision change cancels the remainder
of that block and returns from the bounded pass instead of repeatedly rescanning
all contributors. Revision saturation fails closed and never wraps. Unrelated
nodes have independent domains. Revoking a prepared phone receive generation ends
its ring fence while leaving the logical private call available for control.

This proves typed core publication and revocation of queued composite blocks,
including actual phone-policy ring consumers. It does not prove cancellation of
already accepted provider packets, phone-tainted parrot history, final-TX DSP or
RF presentation. Production attachment remains unavailable until those retained
owners and the remaining admission requirements are joined safely.

## Final-TX programme retirement bridge

The existing serialized radio-control operation gains opcode 8 with a sized
request wrapping its unchanged prefix and the released SDK retirement envelope.
Both hosts validate the complete extension before borrowing it. The native host
retains the optional callback only within its advertised extent and uses the
existing radio handle. Asterisk dispatches through the existing owned channel
and requires an explicit SDK acknowledgment with unchanged generation and
request identity. Unsupported capabilities fail closed. No host-table layout,
SONAME, second graph, hardware owner or audio-thread control call is introduced.

Dispatch acknowledgment preserves the SDK's success, pending or error result;
it is not completed audio retirement. The existing TX feedback consumer now
copies a complete optional programme tail and reports its completed-render
retirement ID and separately qualified prepared/current bounds. A partial or
malformed tail remains unavailable without discarding valid physical-prefix
facts. Existing frame-count, physical-generation and monotonic-publication
checks apply to the whole observation. Missing bound flags never become a known
delay, and an impossible current bound is withheld. These fields do not supply
source birth, DAC presentation time or physical PTT-off acknowledgment.

This checkpoint exposes and tests actual dispatch and feedback paths, not an
activated RF policy. A retired source may already be retained in final-TX DSP
before the next control pump. Zero controller input or requested unkey cannot
prove that old programme is suppressed. Activation additionally requires an
immediate generation-aware permit at the existing post-DSP programme boundary,
before signaling, plus the matching completed-render retirement acknowledgment.
That capability and the RF FIFO/arbiter consumer remain prerequisites.
## Revision-qualified outbound spans — 2026-11-11

A dispatcher-side revision check cannot make a later queue write atomic with
retirement. Each inseparable phone-tainted composite and each private-phone
destination therefore carries the captured node revision in the existing
queue's private descriptor. The shared queue allocation binds its node domain
once on control. The descriptor adds one u64, with no new PCM storage, public
AudioSpan change or per-publication reference-count operation. Ordinary-only
spans remain unmarked and retain their existing FIFO behavior.

The sole consumer discards stale marked descriptors and their corresponding
PCM without copying, bounded by its entry snapshot of queued descriptors.
Partial spans retain the same revision in a scalar inside the existing shared
allocation. Only the sole consumer reads or changes this pending scalar; it is
cleared when a span completes or is discarded. The consumer retains its original
112-byte representation on 64-bit targets; private descriptors grow from 56 to
64 bytes. No observer or producer changes the pending state. A read rechecks after
copying and clears a cancelled prefix; an unclocked read containing an obsolete marked
prefix is conservatively silenced as a whole. A preceding peek is advisory,
so serialized egress treats a cancelled read as retirement rather than panic.

Successful consumer admission is a distinct boundary from queued storage.
This closes publication after completed retirement, but does not revoke PCM
already accepted into downstream conversion, codec, provider or RF history.
Those owners still require their own fences before production phone attachment
can become available.


A read of an already peeked or partly consumed span never silently substitutes
its successor when retirement cancels that pending span. It returns cancellation
and leaves the successor queued. Serialized egress then reevaluates source due
time before admitting the successor, so an ordinary future span behind retired
private media is neither sent early nor lost.

## Prepared phone processor identity join — 2026-11-11

The inner timed ring consumer forwards its exact borrowed private-call token
before the existing ProcessedPeer graph prepares conservative receipt history.
Outer mixer identity alone cannot qualify that preparation. Real 8/48 kHz phone
input therefore uses the same bounded history owner and original receipt bounds
as the qualified graph contract; exact post-DSP provenance remains unknown.
Retiring the media generation still revokes its output without ending private
call control. This joins a prepared path, not production attachment readiness.

## Mixed product mixer input ownership — 2026-11-12

The existing product transmit worker now owns a direct enum of its ordinary
processed peer input and the exact prepared phone media input. Every PeerInput
operation forwards to that original owner, including finite drainage, timing,
provenance, private reply and retirement. The ordinary host and regeneration
constructors select the ordinary variant without changing their ring policy.
No second ring, processor, buffer, thread or per-sample reference-count work is
introduced. The measured amd64 representation is 88 bytes versus the previous
72-byte ordinary input; the 16-byte ordinary-slot cost avoids another allocation
or virtual call. The mixed worker remains a preparation prerequisite, not
permission to attach a phone or publish unqualified RF programme.

## Shared phone account preparation, 2026-11-11

The protocol-neutral standalone account owner consumes copied
`ResolvedPhoneAccount` records from the existing product visitor. It does not
parse ordinary configuration or link another controller implementation.
An immutable prepared account owns its settings, one resolved numeric endpoint,
literal credential, and nonzero generation. A sorted vector shares unchanged
accounts through `Arc`; complete settings, endpoint and credential equality
preserves the exact generation. Changed or newly enabled accounts receive a
fresh generation from one serialized owner. Failed or abandoned preparation
leaves active snapshots unchanged; generation gaps are allowed, reuse is not.

Each `secrets_file` is one UTF-8 literal secret, not an INI document. Strip only
one terminal LF or CRLF; preserve spaces, semicolons and hash characters. Reject
empty values and remaining control characters. Resolve relative paths against
the host configuration directory and reuse the shared opened-file ownership,
regular-file, mode 0600 and no-final-symlink checks. Accept at most 4096 file bytes, with a capped read to reject concurrent
growth; IAX additionally requires at most 255 credential bytes. SIP
uses the bounded literal string without a smaller digest-specific limit.
Diagnostics and Debug output must never contain credentials, credential paths,
provider usernames or endpoints.

Preparation belongs off foreground/control and accepts a caller-owned resolver;
numeric endpoints bypass DNS. A blocking resolver's admission permit remains
inside its actual job until completion, including after async cancellation.
Stale results cannot publish. This module creates no executor, thread, queue,
listener, registration refresh or call. The host must know whether product
publication occurred before committing a reserved listener transaction.
Rejected preparation preserves existing calls; a post-publication operational
failure must not be presented as a successful rollback. Account activation,
shared listener lifecycle and both-host composition remain separate work.

## Timed block rendering and bounded parrot capture — 2026-11-12

Private mixer inputs use a shared timed-block result carrying the original
real/PLC activity count separately from the conservative oldest receipt bound.
Ordinary inputs retain their existing block path. Opaque inputs remain unknown.
The qualified private processor fills PCM and records original metadata once,
then executes one native block process and one buffering query. It does not
replace batching with per-sample DSP/FFI calls or scan output PCM for metadata.

For every emitted sample the audited SDK prepared maximum, rather than the
last output's current delay, bounds the whole block's possible lookback. Before
new input reuses the existing fixed metadata buckets, a scalar folds retained
history from block start minus that prepared maximum. The sole fill pass folds
new source metadata. Missing history, source generation or clock changes, PLC,
unknown original receipt, invalid counters, zero padding, failed processing,
or a prepared bound beyond the 500 ms metadata horizon withhold qualification.
No additional PCM buffer, age vector, native graph or worker is introduced.
This is a conservative original-age bound, not exact post-DSP provenance or
end-to-end RF latency qualification.

The existing capture tap includes all active peer inputs, independently of RF
group selection. During that same render it aggregates private taint and the
oldest original receipt, including real zero-valued PCM. Unknown or mixed clock
epochs stay unknown for the whole inseparable clip. Capture and control share
the existing prepared node retirement domain; adapter refresh must preserve
that domain's identity. A revision change discards a tainted recording or
completed clip, while ordinary-only clips survive unrelated retirement. The
two existing preallocated recording buffers retain only scalar metadata and
never assign a fresh receipt time.

Phone-tainted replay is currently rejected on control before PCM copying,
report preparation or playback publication. The existing replay owner has no
qualified matching host-clock and RF age admission, so retaining the original
bound alone cannot safely enable it. Legitimate qualified private replay and
final-RF enforcement remain feature-completion prerequisites. Ordinary replay
is unchanged. This checkpoint does not enable phone attachment or commands.

## Reserved listener wrapper, 2026-11-12

The alpha12 consumer prepares complete numeric and provider-account listener
replacements in one borrowed RAII batch. It verifies every transaction slot and
the same retained provider owner before passing opaque handles. Numeric elements
remain decimal identities; empty whole profiles can disable admission. Copied
configuration spans need not outlive preparation. The token retains safe mutable
listener borrows, not another long-lived transport owner.

All wrapper allocation precedes provider reservation. Commit consumes the token
without a fallible result; dropping an uncommitted token aborts before releasing
listener borrows. The wrapper does not decide product publication. Its outer
coordinator commits after either committed terminal outcome. Confirmed rejection
discards the uncommitted reservation. Unknown or fatal outcomes keep admission
fenced: the coordinator may discard only the uncommitted provider reservation
while retaining all old and candidate listener and DSO owners through orderly
shutdown. That fail-stop cleanup neither compensates product publication nor
resumes operation or claims rollback. Current boolean reload results cannot
activate this path. Committed reload/stop still retire all product phone calls under this ADR.

## Private source digit authority and shared analysis — 2026-11-12

The named PHONE_INBAND_DIGITS capability is an optional bit in the unchanged
sized peer metadata record. It is valid only with the complete asynchronous,
prepared, source-media private Phone profile. Its absence explicitly selects
NativeEvents, not an event-absence fallback. Both modes retain the validated
source digit callback and its accepted/refused retry contract. NativeEvents
dispatches native symbols only. InBand consumes non-authoritative native
symbols without inserting commands or wedging provider order.

Both timed private modes use the existing SourceIngress source-rate detector.
The released DTMF alpha3 contract accepts exactly 8 or 48 kHz. Current G.711
phone media is 8 kHz; other timed private formats return UnsupportedFormat
rather than silently resampling. Legacy private readers and ordinary peers
keep their existing preparation behavior.

The shared analyzer separates one-time analysis from accepted-prefix commit.
It retains only source identity, original birth, progress and bounded mute
intervals. Ordinary source ingress keeps its existing 4096-sample/64-interval
profile. Private ingress retains the existing 20 ms writer limit, requiring
at most three 8 ms partitions and four interval slots. A retry copies the
provider-owned raw suffix back into existing scratch and reapplies cached
muting without advancing detection or emitting another digit. Outgoing private
prompts may overwrite that scratch between partial or refused retries.

Before authoritative in-band analysis, the private owner reserves bounded
command-queue space. Native symbols retain their individual acceptance checks.
Detected onset revokes its prepared media generation before publication;
the logical call remains alive for private control. Original birth and actual
accepted source credit are preserved, including partial writes. Native-event
backpressure remains exact and only the one serial provider reader is used.
These changes prepare source-private control and suppression; no provider,
manual attachment, RF route or remote call-origin capability is activated.


Retirement is not automatic re-admission. The source-private completion join
below replaces the earlier local pound release. No event-absence or elapsed-time
heuristic reopens a retired generation; the logical call and control events
remain available. Fresh-media adoption is still required before attachment
activation.
The added capability changes no ABI field layout or SONAME; older consumers
that do not recognize the bit reject that unsupported profile.


A source reset cancels provider-owned PCM retries but does not erase an already
qualified authoritative in-band digit. The serial private owner reserves one
command slot before flushing that completion and acknowledging the reset. A
full queue retains the provider reset for retry without advancing detection or
source position. NativeEvents never dispatches the in-band completion; ordinary
peer reset behavior is unchanged. Media retirement still happens immediately
and does not wait for command capacity. This is not the pending private-grammar
collection-ended/re-admission join described above.

## Selected RF programme metadata, 2026-11-12

The existing LinkAudio owner separately records the peer programme selected for
its controller input. This is neither the broader all-active parrot capture tap
nor proof of final admission or RF presentation. Real zero-valued PCM and active
concealment retain contribution; registration, transport key state and discarded
idle processor output do not. Ordinary keying and source-activity policy remain
unchanged.

Selection is snapshotted before the existing single render so original age can
be folded without a per-peer age vector. Desired groups are still adopted only
at the existing all-inputs-idle boundary. Exact contributor flags are finalized
where the selected peer planes are mixed, after rechecking retirement. The full
composite age is unknown if any selected programme is untimed, has a missing or
different clock, or raced node retirement. No current clock or output counter
is substituted for original receipt.

One scalar block record retains that original node revision. Borrowed iteration
exposes the exact media-generation and private-call fences already owned by each
actual private contributor, without cloning a token or allocating on audio.
Retirement after rendering remains visible through those fences; getters never
renew the block snapshot. Empty or rejected callbacks cannot expose a previous
block as current. Capture metadata, fresh network fanout and mix-minus retain
their independent original paths.

This adds no RF FIFO, policy, clock qualification, final-fence consumer or ID
presentation acknowledgment. Those joins and physical qualification remain
required before production phone RF attachment can become available.


## Authoritative collection inspection prerequisite — 2026-11-12

The outer phone grammar and the exact selected-node source collector expose
read-only pending-collection state. A selected target or retained administrative
grant is not itself pending collection. Empty action output is also not proof
of completion: discard expiry may produce no action, and addressed symbols can
leave the inner node collector active after the outer grammar becomes idle.
Inspection does not cancel, reconstruct or renew those owners or their grants.
The node query requires an authenticated matching source token and open node
admission, but it is not call-registry authorization; control must still validate
the actual PhoneSource and PeerReply against the live private call.

Actual released-detector tests distinguish a qualified pending digit from
current audible suppression at both 8 and 48 kHz. Scalar state inferred during
the existing 8 ms partitions agrees with the provider's explicit end-of-input
completion through quiet tails, replacement tones and short unqualified tones.
The private completion join below uses this scalar proof without automatic
readmission. Fresh-media adoption remains a separate pending prerequisite.
No attachment, RF or provider capability is enabled.


## Revision-qualified private collection completion — 2026-11-12

Private suppression ends only after the actual outer grammar and exact local
node collector are idle. The existing ordinary Engine pump reconciles this
after its normal expiry and scheduled work, inside the same serialized
accepting/revision/reload/fatal admission fence. Pending-reload servicing does
not reconcile. Existing grammar expiry marks silent transitions; the node's
existing collector sweep notifies exact private completions even when no
operation is emitted. One dirty bit per bounded call avoids repeatedly scanning
unchanged collectors. Inspection never cancels a selected target, reconstructs
a collector, or renews a grant.

The existing serial private owner tags its 16-byte digit slots with a checked
32-bit revision. New authoritative symbols, qualified tone onsets and source
resets advance it; exhaustion ends the exact call without reuse. Call/account
generations and actual ingress remain independent typed fences. The same
bounded command queue carries completion for the last consumed revision.
Queue refusal retains one dirty intent; a newer processed event replaces that
intent only after the real grammars run again. The serial owner reads pending
source activity before applying completion and rejects obsolete revisions.
A matching request waits for retained raw PCM acceptance and the released
detector's qualified quiet tail. Only scalar metadata is retained; no extra
PCM pass, copy, detector or worker is introduced.

Private pound no longer clears suppression on its own, including quoted or
addressed pound. Ordinary peer analysis keeps its original automatic pound
behavior and profile footprint. A source reset preserves an existing private
collection latch while retiring its media, and still emits a qualified in-band
completion before acknowledging reset when capacity permits.

NativeEvents still cannot correlate a native symbol with a later detected tone
onset. Such an onset invalidates an earlier completion rather than guessing
by digit value or timing. Similarly, a reset with no new authoritative symbol
cannot reuse an old revision to release a preserved latch. These conservative
orders remain unsupported for automatic readmission under the current source
contract. InBand completion supplies its actual new revision after the tone
ends. No timeout is invented in the serial owner.

Completion clears only the source suppression latch. It neither revives the
retired media generation nor prepares/adopts another ring or graph. A future
fresh-media handoff must retain the expected revision and full lifecycle,
source and RF qualification fences. Production attachment, RF routing,
provider advertisement and remote capability remain unavailable.


Global control-input loss already cancels every node's partial collector and
clears its expiry deadline. That same Host cancellation marks the bounded
private-call set dirty once, so an idle call does not depend on a now-impossible
expiry notification. This does not cancel the outer phone grammar or change
the existing loss scope. Ordinary reconciliation still checks both grammars
and the exact source revision after processing any newer queued private input;
loss notification itself neither clears suppression nor admits media.

## Scoped IAX listener cohort composition — 2026-11-12

The existing foreground listener map is the sole control owner. A scoped
transaction maps prepared IAX accounts to the explicitly named enabled node's
configured port, preserving numeric ASL bindings separately. New listeners bind
complete candidate profiles and enable the existing shared receiver before
product publication, but remain unpolled. The receiver only queues NEW traffic;
it owns no account credentials or product admission callbacks and performs no
registration, authentication or call setup by itself.

All candidate map insertion and batch preparation precede product application.
One borrowed alpha12 batch covers retained, added and removed ports; removed
listeners receive empty replacement profiles. The same borrowed runtime context
is lent sequentially to candidate enablement and product application. No map
reference enters the latter callback, and no extra ownership object or unsafe
alias is introduced.

Both published terminal outcomes consume commit before obsolete listeners are
removed. Rejection aborts and removes only additions. Fatal or unknown outcomes
abort only uncommitted provider preparation, retain every old and new listener
owner, and return to the existing fenced shutdown path. Original product errors
remain distinct; no late sequential binding mutation or compensating product
reload remains.

Production foreground composition still supplies no provider accounts. Shared
account preparation, registration/dial execution, typed private IAX metadata,
timed source reads/egress and complete retirement remain required before actual
phone admission. This cohort join adds no scheduler, worker or live account use.

## Existing prepared-media source revision fence

The existing private preparation endpoint stamps its own consumed collection
revision into its one boxed receive Ingress; no public numeric revision or new
permit API is introduced. Before any authoritative symbol, the only initial
clean boundary is revision 1. Reset cannot synthesize that boundary again.
The serial owner records settled revisions only after actual matching command
release and checks the exact call token before one-time adoption.

Media dequeue stages the same existing box. Cancelled queued boxes cannot
replace a newer live/prepared generation. The owner first performs its existing
bounded source read and collection release, then compares expected, current and
settled revisions. A matching pending release can retain that one box through
a real qualified tone tail. A mismatch, reset, retirement or unsupported native
ordering rejects it, and an active/retired generation's admission anchor is
never rewritten by a later adoption attempt.

An empty cached 20 ms prefix is insufficient: the provider may still own a larger
unanalysed suffix. Each turn starts without drain evidence, and every actual
source read replaces it from the existing idle/drained result. Prepared media
waits until that real boundary and the analyzer's raw/tone state are clear.
The actual last source-read clock reference and source cursor define adoption.
Earlier source PCM is discarded, never relabeled or replayed; later accepted
PCM keeps its original provider birth. No extra read, PCM pass, buffer, graph,
candidate box or worker is created.

The checked amd64 layout adds one aligned scalar to the existing Ingress
(56 to 64 bytes). The private session is 11,424 bytes, control and command
slots are 96 bytes each, and digit slots remain 16 bytes. The shared private
source profile remains 264 bytes; no per-peer thread or additional retained
PCM allocation is introduced.

This hardens the existing prepared-media owner only. The Engine ordinary turn
fences enqueue, but retained call tokens after fatal reload are not a later
adoption permit. Production attachment still needs the actual two-sided target
grant, limits, isolation/bypass authority, lifecycle-spanning queued-candidate
fence, typed publication and RF qualification. No production caller, automatic
preparation, attachment success or RF route is enabled here.

## Inactive owned private IAX source boundary — 2026-11-12

The standalone host can consume an already uniquely owned ProviderCall and its
copied receipt under the current account/listener admission fence. It checks
the committed account generation, exact listener/provider endpoints, nonzero
provider call generation, call numbers and the current 8 kHz G.711 profile.
Failed identity allocation or validation destroys that owned peer exactly once.
No raw incoming callback adoption, account activation, registration or dial
execution is added.

The standalone host artifact owns one checked call sequence, shared by every
native private-call creator in that host. Replacing HostServicesOwner or a
listener cannot reset it. The sequence never wraps; its canonical nonzero
32-hex representation is an opaque identity, not an authentication secret.
There is no allocator static in peer-support or a provider/product DSO.
Asterisk native private-call construction is not yet implemented and must retain
its host-owned sequence across reload while identities survive.

Actual metadata/source callbacks distinguish Phone from numeric ASL without a
caller-number alias. A private source retains one 160-frame PCM prefix or one
control event, with exact accepted offset, provider media epoch and original
host-clock birth. NativeEvents is the sole digit authority. Refusal retains
that event before subsequent events. A reset acknowledges the provider target
only after actual product retirement completes; a digit accompanying reset is
retained until then. Null-sink queries consume nothing. A changed host clock
fails closed rather than restamping a retained source.

Alpha12 does not distinguish codec concealment from all other absent-birth PCM.
Such PCM is discarded as an exact counted interval of unavailable usable source
audio. The gap contract is unresolved source loss, not a claim about network
packet loss. Ambiguous PCM reaches neither analysis nor RF and is never assigned
a fabricated birth or concealment classification. Private legacy reads, ASL
text and untimed prepared sends fail closed. Ordinary numeric peers are unchanged.

Private executor attachment remains explicitly unavailable. The existing
ordinary jitter setup does not qualify the phone profile, and alpha12 cannot
guarantee protocol ACK/expiry progress while an application event is refused.
Zero-capacity polling is not a service-only operation. A separate bounded
provider progress prerequisite and timed egress/source authorization composition
must be qualified before production admission. This slice changes no host ABI,
SONAME, scheduler, thread count or live account behavior.

Remote Hangup and terminal source errors latch the private host owner closed.
Later null-sink queries report the same closure without consuming input; later
source reads cannot poll again or publish retained media/control. Destruction
remains the existing unique handle owner's responsibility, exactly once.
