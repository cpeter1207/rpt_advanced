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

The fixed 172-byte RTP send follows Rust 1.88's
[atomic UDP send contract](https://github.com/rust-lang/rust/blob/1.88.0/library/std/src/net/udp.rs#L683-L702).
Real socket errors remain redacted; sequence advances only after successful
submission of the entire datagram. The caller supplies the original source
timestamp, including skipped or expired spans. Success does not establish peer
delivery.

SRTP, SDES/DTLS, ICE, RTCP multiplexing, additional codecs and non-20-ms profiles
are not advertised. Unsupported security attributes are rejected rather than
downgraded. RTCP/session reports, outbound telephone-event scheduling, incoming
SIP admission, signaling, jitter/PCM orchestration, in-band detection and complete
call lifecycle are separate pending integration work. The existing product DTMF
provider remains the in-band detection owner. This boundary does not imply a
secure media session, a usable account or a completed Task 4.

The former signaling candidate `rsipstack 0.8.2` and the `0.8.4` version
inspected during that historical probe retain unbounded transport, transaction, incoming-call and state queues. A capped
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

The initial cohort join supplied no provider accounts. Numeric IAX startup and
reload now use that transaction as described below. Registration/dial execution,
typed private IAX admission, timed source reads/egress and complete retirement
remain required before actual phone admission. No scheduler or worker is added.

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

Private executor attachment remains explicitly unavailable. Released alpha12
already selects the 20/40/100 ms phone jitter profile through its existing
configure callback when the peer retains authenticated provider ownership.
Both account call constructors restrict codecs to PCMU/PCMA at 8 kHz with
160-frame packets. Numeric peers retain their ordinary jitter policy.
Alpha12 cannot guarantee protocol ACK/expiry progress while an application
event is refused.
Zero-capacity polling is not a service-only operation. A separate bounded
provider progress prerequisite and timed egress/source authorization composition
must be qualified before production admission. This slice changes no host ABI,
SONAME, scheduler, thread count or live account behavior.

Remote Hangup and terminal source errors latch the private host owner closed.
Later null-sink queries report the same closure without consuming input; later
source reads cannot poll again or publish retained media/control. Destruction
remains the existing unique handle owner's responsibility, exactly once.


The alpha12 profile selection is grounded in released source
4e7026c5234a7c7fb61907bbe56fbaef3a18963b: client::configure_jitter,
server::prepare_dial and server_provider's authenticated incoming route.
Qualification uses genuine loopback incoming/outgoing owned calls and the
unchanged host configure wrapper, forwarding to the real jitter DSO. Failed
preparation, retry, duplicate preparation and destruction are checked without
attaching a private executor. This adds no jitter API, owner or activation.

## Current configured phone account admission

Both borrowed pre-answer authorization and owned incoming admission use one
private Host validator in the ordinary serialized Engine turn. It resolves only
the inspected account label through the canonical account resolver, requires
the current record to be enabled, and requires its configured local_node to
equal the actual provider-bound local node and remain hosted. The existing
Call.local string is retained only after that equality check, so the configured
binding remains the default local target without another stored identity.

Borrowed authorization does not adopt or destroy the pending provider handle
and creates no call or permission token. Owned admission rechecks the current
document before creating a private call, prompt or serial owner. A removal,
disable or rebind between the two calls is therefore a rejection; the existing
owned error path destroys the transferred handle exactly once. No account map,
secrets read, native transport or extra worker is created by this check.

The current peer ABI exposes ProtocolId::Phone and nonzero account/call
generations, but does not expose the native SIP/IAX family or an authoritative
published provider-generation map. This check does not invent either value or
compare provider generations with the unrelated Engine revision. Existing
actual request/metadata identity checks, typed ingress fencing, mailbox/PIN
authentication and reload admission fencing remain separate and unchanged.

An enabled bound account is permission for private mailbox/control admission,
not transceive attachment. Two-sided target permission, attachment limits,
outgoing isolation and source-unlock bypass, queued lifecycle-spanning media
authority, provider readiness and RF qualification remain required. *3 and
other unavailable attachment/manual actions remain explicitly unavailable.

## Command-time local attachment policy — 2026-11-12

The existing private Attach consumer reads two explicit current-document lists:
`attach_local_nodes` in `[phone_account label]` and `phone_allow_accounts` in
the exact target node section. Both use whitespace-separated exact canonical
NodeId/PhoneAccountId values. A comma or star in a valid node identity is literal,
never list punctuation or wildcard expansion. Missing or empty lists deny;
malformed lists warn and fall back to the empty deny list. Target permission
does not inherit from `[general]`. The account's `local_node` supplies the
default target only and satisfies neither attachment permission.

Exact live source/reply ownership and the authenticated mailbox are checked
before any response. The current enabled account must still match the call's
authorized local binding, and the requested target must remain hosted. Ordinary
authenticated policy denial uses the existing private Failed response. Both
permissions currently yield Unavailable, not successful attachment; no media,
reservation, queued grant, public alias or remote route is created.

These lists stay in the product configuration document. Provider account ABI
records, host transport snapshots, per-call storage and worker counts are
unchanged. Reload still retires the old source, and a replacement call checks
the replacement document. Existing ordinary local command selection and
source-scoped unlock remain independent.

The approved one-node-per-call and one-reverse-call-per-node defaults still need
a real candidate/publication owner before reservation storage is introduced.
Outgoing bidirectional isolation and its per-call source-unlock bypass are
separate, unimplemented authority. A command-time decision cannot authorize
later serial adoption: lifecycle/policy revocation, source revision, qualified
provider media and RF readiness must join that actual handoff before activation.

## Bound-node private site replies — 2026-11-12

The existing public read-only SiteStatus command now prepares its same compiled
site recipe for the authenticated caller's private reply queue. It authorizes
only the sensor/input reading selected by the existing configured DTMF selector
on that call's bound node: localized label, value/unit and current reading
quality. It does not enumerate arbitrary site data or expose credentials,
provider settings, another node's readings or administrative controls.

The ordinary source-scoped node grammar and command permission still produce
the SiteStatus effect. Before snapshot or producer preparation, Host requires
the exact live PhoneSource/PeerReply owner, its authenticated mailbox, and the
current enabled account with unchanged configured local binding. This does not
grant or renew node unlock, select another destination or grant attachment.
General Message and RemoteBase private reports retain their existing explicit
unavailable behavior.

Public and private site replies share the same control-side value snapshot and
compiled recipe. Private preparation registers one stream with the existing
station producer and queues it only to the original call; it never enters
controller telemetry or creates RF demand. Stale, faulted, missing or unknown
readings retain existing unavailable wording. Invalid selectors, absent clock,
producer failure and queue refusal propagate without a public fallback.
A full private queue retains its prior FIFO owners; a rejected prepared reader
is dropped through the existing producer cancellation contract.

The queued and borrowed reader retain the original call token. Hangup/reload
cancels them, and an equal provider call ID with a different actual ingress
cannot inherit their output. There is no new producer, PCM buffer, transport,
ABI field, worker, cross-node permission, reservation or media activation.
Provider/RF qualifications and a lifecycle-spanning attachment grant remain
separate prerequisites.

## Completed source commands retain their collector — 2026-11-12

A completed peer or private-phone RemoteBase operation retains custody of its
original source collector while the copied operation or native pending receipt
is owned. One lazy Arc allocation is shared by concurrent completed operations
from that collector; the collector observes it weakly. Local commands and
non-remote operations do not allocate custody. When the final owned copy drops,
the existing idle sweep reclaims a collector that has no other required state.
A collector retained for selection or a grant can keep the small weak control
block until its next pending batch or reclamation; no custody history accumulates.

Custody is not authorization. The exact reply/origin, current policy, grant
expiry, macro cancellation and runtime revision checks remain authoritative.
Explicit retirement, ended sources and configuration replacement still remove
collectors even when copied work exists. Lost input keeps its prior semantics:
partial collection and macro work are canceled, not already completed human
commands. No remembered target or synthetic grant is introduced.

This prerequisite fixes the same pump-before-dispatch pruning gap for ordinary
peers and phone callers. It does not enable private RemoteBase responses or
phone attachment; their existing product authorization and unavailable paths
remain unchanged.

## Bound-node private radio status — 2026-11-12

Only the existing short/long RemoteCommand::Status query can now use a phone
caller's private response path. It retains the ordinary node command permission:
public read-only queries do not require or renew administrative unlock. Other
read-only radio query variants, mutations, cross-node selection and General
Message remain unavailable to this path.

The product checks exact live authenticated PhoneSource/PeerReply ownership,
bound node, mailbox and current enabled account/binding before native submission.
It rechecks runtime revision and command permission with those private owners
after source/result callbacks, before registering a producer and queueing output.
Call retirement, replacement or reload cannot transfer an old result to a new
caller. No account authorization check was removed.

The existing native ticket queue, parser and localized reply forms are shared.
Private results use the existing station producer and bounded exact-call reply
queue; ordinary replies retain their existing audience. Synchronous failures,
busy and malformed/native failure results use the same trusted presenter.
Preparation or queue failure does not fall back to public telemetry or RF.

Private pending work is bounded in the existing Host pending vector: at most
one receipt per exact reply and at most the existing maximum_calls count in
total. Retired phone identities count until the existing pump/reload/stop owner
actually drops their receipts. Busy is returned privately before another native
submission; full reply queues preserve prior queued owners. No extra pending
queue, permission map, provider capability or radio/media activation is added.

## Owned bounded SIP parsing prerequisite, 2026-11-12

One experimental parser belongs to the existing shared `rptadv-peer-support`
leaf, reusable by both hosts without a new C ABI, shared-object boundary or
external dependency. It borrows an admitted immutable wire buffer and keeps a
fixed header-span index. TCP framing leases one caller-owned buffer generation;
message borrows prevent reuse while views survive. UDP trailing bytes and TCP
pipelined bytes have distinct consumption semantics. Retained transactions and
calls still require their own explicit admission/storage reservations.

The selected limits are 32 KiB wire, 16 KiB start-line/headers, 16 KiB body,
128 logical fields and 4 KiB unfolded field. The parser/framing index and caller
unfolding scratch together target at most 8 KiB, excluding admitted wire bytes;
allocation/layout tests prove the explicit workspace target on native amd64:
8-byte entries, 1096-byte parser, 1144-byte TCP decoder, 4096-byte scratch,
48-byte message view, 96-byte URI view, 32-byte header view, 24-byte value
iterator and 48-byte name-address view (5488 bytes together on the TCP path,
excluding wire storage and compiler call-stack frames). Instrumented allocation
checks observe zero allocations across 32 RFC 4475 wsinv datagram/bytewise-TCP
cycles, 8008 single-byte mutations, four additional RFC fixtures, two reviewed
header regressions and seven URI cases. These are
implementation bounds, not universal SIP limits or a process-RSS measurement. RFC 3261 framing/grammar, RFC 5954 IPv6 and RFC 8217
name-address rules apply; RFC 4475/5118 provide independent torture inputs.
Valid SIPS syntax remains distinct from unsupported secure routing, which must
fail closed. URI headers never supply arbitrary request headers or authority.

Owned in-house SIP signaling is the current choice under the owner's no-vendor-
repairs direction, starting with this parser prerequisite. Third-party signaling
candidates remain unselected: rsipstack retains unbounded
queues/state, RVoIP conflicts with runtime/resolver/toolchain ownership, and
unmodified oSIP 5.3.1/5.3.2 loses allocations on parser list-insertion failure.
The owner excluded vendor repairs. No native parser/global allocator hook,
transaction state machine, DNS parser, cryptography, codec or DSP is added here.
The approved provider/account/host lifecycle, resolver adapter and existing
`phone::media` remain unchanged. This prerequisite is not SIP activation or
complete Task 4. Independent parser source review and 100% parser production
line/branch coverage are complete; full native gates remain required before merge.

## Numeric IAX account foreground composition, 2026-11-12

Standalone startup and reload now prepare enabled, locally hosted IAX accounts
through the existing secure private-file reader and generation allocator. The
first production profile requires a numeric endpoint and explicit register=no.
Enabled SIP, hostname and registration profiles fail before listener/product
mutation; disabled accounts or disabled local nodes open no account transport.
No resolver, registrar, thread or runtime is introduced.

One foreground-lifetime allocator preserves identity across failed candidates.
Complete account profiles feed the existing listener bind and reserved cohort
transaction. Only published product outcomes replace the active prepared snapshot,
including published-with-error. Rejection preserves the previous snapshot;
fatal outcomes retain listener owners and enter the existing fenced shutdown.
Provider-owned copied credentials survive preparation borrows. Listener-owned
IAX code and the provider's retained runtime outlive socket-task teardown.

The initial account checkpoint kept numeric-only polling and provider admission
closed. The following typed incoming join builds on those actual account/listener
owners. Neither checkpoint alone provides a connected phone call. Registration,
outgoing dial/isolation, private executor/media and RF composition remain separate
requirements; mailbox and attachment guards remain.

## Released dependency cohort, 2026-11-12

The controller package floors now select these released prerequisites:

- [IAX alpha13](https://github.com/cpeter1207/librptadviax2/tree/ce9fe2da0d8480c1503f5eb80c3c95d8ac2e5360)
  supports bounded protocol-only progress while private source delivery is
  refused. Client package and SONAME 2 are unchanged.
- [PortAudio adapter alpha5](https://github.com/cpeter1207/rptadv-portaudio-alsa-adapter/tree/cc63a779d9619c9b3c1f9096d33f81cad4ea720d)
  adds timed output observations while retaining ABI/SONAME 2. Their uncertainty
  is explicitly unbounded; they do not establish a physical RF deadline.
- [USBRadioPlus SDK alpha35](https://github.com/cpeter1207/USBRadioPlus/tree/b7dc3d35a242c4bdfea7166b6f959539d500a363)
  adds stopped-bound post-DSP programme fences on existing owners. Runtime and
  development packages retain identity 1 and require at least
  `0.1.0~alpha35-1` (`0.1.0~alpha35-1.deb13` in signed APT).

The immutable source manifest preserves codec-before-IAX ordering. The SDK
remains a signed-APT dependency rather than an unsupported source-manifest
entry. Raising these floors does not activate phone attachment or qualify RF
timing; consumers must still negotiate their required capabilities.


## Private IAX protocol progress consumer, 2026-11-12

Standalone private source reads now use the released alpha13 sized ABI2 request
through the existing `poll_timed` callback. When a PCM/gap suffix, telephone digit
or retirement acknowledgment is refused, the same serial owner requests one
bounded `SERVICE_ONLY` turn with no source buffers before retrying it. ACKs,
protocol replies and retransmissions continue without consuming later source
items or replacing the held PCM buffer. No second poller, queue or thread is added.

A service-reported media retirement fences held PCM/gaps before further credit.
An already refused private digit survives that retirement and remains behind its
required downstream reset acknowledgment. Invalid or terminal provider responses
latch the exact call closed; null state queries cannot revive or repoll it.
Legacy source polling and untimed prepared sending remain forbidden for private
handles. The standalone private async attachment guard remains in place pending
qualified private timed egress and its complete product/provider lifecycle.

Focused tests include real released-provider loopback signaling: while digit 5
is refused by the public host callback, a later reliable digit 6 is acknowledged
without being offered out of order. Accepted-prefix PCM/birth, media reset,
control retention and terminal cleanup are checked separately. These tests do
not establish working phone calls, provider admission, RF attachment, a physical
radio timing profile or live hardware acceptance. The existing host ABI and
SONAMEs are unchanged; alpha13 build/runtime floors are required.

## Typed incoming IAX product handoff, 2026-11-12

Foreground's existing mixed poll checks each borrowed provider receipt against
the currently committed account label, generation, username, local binding,
numeric provider endpoint and exact bound listener. Call generations, negotiated
G.711 format and call numbers must be valid. Caller presentation and called/context
fields supply no product identity or permission. Removed/replaced accounts and
fatal reloads cannot reopen admission.

The released provider's pre-answer callback exposes metadata only, not a peer.
It therefore performs transport/account fencing, never synthetic product
authorization. On acceptance the host adopts the unique provider peer, constructs
its existing opaque private handle, then asks product authorization with that
actual borrowed handle and protocol 3 before product admission. The provider may
already have answered; a product rejection destroys the call immediately.

Product authorization never consumes ownership. Incoming success and consumed
failure transfer it; an unconsumed failure is destroyed by the host. The provider
callback returns transfer success after local adoption even when product admission
rejects, preventing double destruction. Request identity bytes remain valid through
consuming callbacks. Existing current-document product checks, mailbox/PIN login,
private executor/media qualification and attachment guards remain authoritative.
No new ABI, runtime, worker, radio binding or RF route is introduced.

## Serial private IAX executor and timed egress, 2026-11-12

The standalone typed private handle can now use the existing bounded serial
executor. Construction requires the exact committed account/listener and call
generations, released timed source polling, native timed send and native egress
retirement. The provider still selects private 8 kHz jitter. No new scheduler,
call owner, transport, RF attachment or mailbox/command authority is introduced.
Legacy polling, untimed prepared sends and AllStar text remain refused.

The prepared packet callback previously carried only a source timestamp: neither
it nor the existing opaque encoder result can carry an immutable maximum age.
One optional host-v10 tail therefore adds `peer_send_prepared_timed` at offset
408 (complete size 416). The complete 408-byte prefix remains valid; partial
409--415-byte tables are rejected before a bounded copy. The tail requires the
complete prepared-media/async callback groups, and source-phone admission
requires the tail. Both host constructors and validators are updated; Asterisk
advertises no timed private send. This compatible sized extension changes no
ABI version, SONAME or package identity; private IAX still requires alpha13.

The existing original source clock stamps new private prompt PCM once. Before
creating a pending packet, product samples provider elapsed time first, then
the same original source clock, and floors the remaining 200 ms age allowance
into a checked, nonwrapping deadline. Missing, changed or future clock ages fail
closed. Congestion retains exact bytes, timestamp and deadline; provider expiry
discards the packet and conversion/encoder history without counting acceptance.

The existing encoder-reset callback is the serial native retirement boundary.
Product drops staged media before reset; host alone stores the returned nonzero
provider TX epoch. Pending or failed retirement closes that call, with no
legacy fallback. Source discontinuity and finish retire immediately even when
no new PCM follows. Cancellation ends the exact private session. Private sends
are exactly 160 G.711 bytes/frames at 8 kHz; radio empty unkey markers are never
sent to a telephone.

Focused tests exercise the public host against the released provider: exact
private jitter, one attached executor, stop/detach/destruction, ACK progress
behind held digits, and timed G.711 reception. Product regressions cover the
physically short prefix, capability groups, immutable retry/expiry and immediate
reset failure. Actual-product mailbox login/status and manual telephone smoke
remain separate integration milestones. RF timing, half-duplex receive,
outgoing authority and live hardware acceptance are not established here.

### Finite private prompt completion

A successful private playback result, not an idle interval or temporary reader
Pending, authorizes finite converter/encoder drain. The existing Outbound owner
holds later prompt input until that drain completes. A released G.711 encoder
may flush a short final packet; after the converter has fully drained, the
private 8 kHz owner completes its checked retained PCM to 160 frames with silence
in existing scratch, then asks the codec to encode it. It never appends encoded
zero bytes, discards spoken tail, or weakens the fixed provider packet contract.
The original oldest birth, timestamp and immutable retry deadline remain intact;
padding is excluded from real-source expiry accounting. Waiting incomplete
history still expires at its original age, while the existing queue consumer
discards stale queued spans rather than rescheduling them indefinitely.

### Retained codec birth and private sample cadence

A persistent sub-packet codec remainder must not pin later real samples to the
first span's birth. Conversion retains one source boundary for its existing
output-end birth mapping. Once a produced packet consumes through that boundary,
only the next uncommitted encoder sample adopts the mapped birth; the produced
packet keeps its captured birth, timestamp and immutable deadline. Projection
is limited to real converted output, so completion silence adds no provenance.

Private prompts advance the existing wake deadline by actual rendered 48 kHz
sample counts, not by scheduling lateness. The exact next-render deadline joins
the existing bounded control wake. An unready reader pauses that cadence; its
next real samples anchor at the actual owner time. No sample birth is advanced
to hide scheduler delay, and the 200 ms maximum age is unchanged.


### Genuine product private-call regression

The standalone loopback fixture now loads the actual product DSO and released
IAX provider, prepares a numeric account with a synthetic private secret, and
uses the real product authorization, mailbox/PIN verification and serial
private executor. It sends telephone digits and checks the exact decoded Morse
envelopes for login, authenticated/unlock guidance and the no-links status
reply. Physical radio activate/ready/quiesce are the only simulated host
callbacks; the fixture checks that private replies never appear in radio TX
samples or PTT. Stop joins the callback pump and releases both UDP bindings.

This is a shared private-audio integration regression, not a supported
headless deployment configuration, native SIP acceptance, RF presentation
qualification or a live hardware milestone. It introduces no production
interface or authority change.


### Prepared timed RTP submission, 2026-11-12

The signaling-neutral media owner now accepts exactly one prepared 160-byte
G.711 payload with its original wrapping 8 kHz timestamp, marker, immutable
nonwrapping call-elapsed deadline and transmit epoch. The existing product
Outbound owner remains responsible for mapping original source age to at most
200 ms remaining; its millisecond source timestamps must be converted to 8 kHz
RTP units by the future SIP host. RTP does not stamp a new source birth,
re-anchor a gap, convert PCM or drain a prompt itself.

Submission uses Tokio try_send on the same connected UDP socket. WouldBlock
retains one fixed 172-byte datagram, its deadline and sequence; changed retries
are rejected without replacing it. Only actual WouldBlock requests writable
readiness, and the existing serial owner must also service the original
deadline and call lifecycle. No timer, scheduler, task, queue or codec is added.
Expiry discards uncommitted bytes without consuming a sequence number.
Retirement discards staged bytes and advances a nonwrapping epoch; old work
cannot send or clear a newer generation's pending datagram. Already submitted
UDP datagrams cannot be revoked.

This replaces the unused untimed send interface and removes the redundant
initial RTP timestamp from bind. It is an internal initial-alpha Rust API
change with no C ABI, SONAME or package change. Focused framing and loopback
tests establish source-gap, immutable-retry, expiry and retirement behavior,
not active SIP signaling, product admission or hardware acceptance.

## Native SIP account checkpoint (2026-11-13)

The optional versioned SIP adapter implements only prepared account activation,
Digest REGISTER (native-parsed quoted realm, bounded to 256 bytes), native
accepted-expiry refresh, bounded failure, unregister and
checked shutdown. Incoming requests are refused statelessly in this slice.
Native media, auto-answer and auto-ACK are disabled; SDP/RTP/codecs and future
explicit call answering remain project-owned. Neither controller host is yet a
working SIP client. No provider account, hardware, RF or node deployment has been
used to qualify this checkpoint. The requested lifetime accepts the existing
configuration's 1..65535 seconds, without a new operator realm setting. The future
composition owner must enforce its configured authentication deadline; this
adapter's native transaction timeout is a distinct 32-second resource bound.

The credential profile matches existing configuration bounds: UTF-8 username
255 bytes and literal UTF-8 password 4096 bytes. Sofia's selected
auc_credentials colon-delimited C-string interface cannot represent colon/NUL;
its Digest emitter inserts username inside quotes without escaping, so username
quote/backslash are also rejected. Existing username whitespace/control rejection
remains. Password spaces, quotes, backslash, semicolon and hash remain literal.
Unsupported representations fail during preparation with a redacted invalid-input
code; credentials are never truncated. URI user accepts validated percent escapes.
Derived registrar/AOR limits are 264/1030 bytes, including the configured server's
maximum 260 bytes and percent-encoded 255-byte UTF-8 username.

The first observed reauthentication 401/407 publishes REGISTERING once, retaining
accepted-expiry and successful-registration count. An initial challenge while
already REGISTERING does not restart the host's absolute authentication deadline;
neither do repeated challenges. Silent native refresh emits no request-start
callback, and even peer 100 did not produce one in the focused native probe.
Its transaction timer remains native-owned. The host independently expires
binding usability at its monotonic success observation plus accepted_expires,
even if a later refresh transaction or reauthentication is still pending.
Explicit unregister remains available after an observed reauthentication challenge
when a prior successful registration exists. Initial never-registered
authentication still cannot unregister; this distinction is exercised against the
independent registrar after the challenge callback is delivered.

The native account retains generated Contact, Via rport/NAT reflection and one
15-second OPTIONS keepalive owner. Registration self-validation is disabled
because this registration-only slice refuses new incoming requests statelessly.
The next call-composition milestone must qualify actual provider/NAT behavior;
loopback registration does not establish Callcentric or other provider acceptance.
No ICE, STUN service, transport proxy or alternate DNS stack is introduced.

Resource bounds are source-reviewed against the selected unmodified release:
NTA's `nta_msg_create_for_transport` sets the 8192-byte `msg_maxsize` limit;
`tport_recv_iovec` refuses/truncates oversized UDP input before normal delivery.
`NTATAG_STATELESS(1)` makes new inbound requests take NTA's stateless refusal
path, rather than accumulating server transactions. One activated handle owns one
registration request, at most two native recoverable retries, and one keepalive.
Automatic refresh is no faster than the native one-second minimum; completed
non-INVITE transactions retire through the fixed five-second Timer K. The native
transport send queue is four entries. These limits, not
`NTATAG_MAX_RECV_REQUESTS_PER_SECOND` (a lagged estimate), bound retained work.
An in-flight refresh cannot overwrite pending removal. It may receive one
credential continuation to let queued unregister proceed; a repeated challenge
fails and cancels the native handle, rather than leaving an unbounded credential
wait. Failure does not claim the remote binding was removed; it can persist until
expiry. The owner still drives shutdown and checked destruction.
The eight-slot adapter event queue drops/counts oldest observations on overflow;
the status snapshot remains authoritative and terminal status remains available.

Independent synthetic fixtures target registration challenge/success/shortened
expiry refresh and unregister, refused setup/generations and timer-only failure/
shutdown. The upstream shutdown may need 30 seconds; only final status 200 allows
destruction. Full native-platform gates, complete coverage, host composition,
incoming SIP calls/media, live provider and physical-radio milestones remain open.


## Actual standalone REGISTER composition — 2026-11-13 local checkpoint

The existing `[phone_account]` records now reach a standalone SIP registration
owner. Configuration and secure literal secret loading stay with their existing
owners. The existing generation allocator preserves unchanged settings,
credentials and resolved endpoints; failed preparation never republishes or
reuses a generation. SIP authority uses the configured server, while transport
uses its separately resolved numeric endpoint. The AOR percent-encodes the UTF-8
username's non-unreserved bytes; Digest receives the unchanged literal username
and secret. There are no new operator configuration keys. IAX registration and
hostname activation remain outside this increment.

All candidate SIP contexts and account maps are prepared before the existing
product/IAX cohort publication. Preparation sends no REGISTER. Both committed
product outcomes adopt the exact candidate and explicitly activate its newly
added registrations. Rejection aborts additions; changed/removed old owners
unregister and retire only after publication. Post-publication activation errors
are reported without claiming rollback. Unchanged generations retain their
existing native owner. SIP-disabled records neither read their secret nor start
a reactor, and enable-on-reload uses the inert startup-retained provider.

The reactor enforces the configured absolute authentication deadline for initial
registration and observed reauthentication. The first refresh challenge starts
that deadline once; subsequent challenges cannot extend it. Silent automatic
refresh uses the native transaction timer, while the host separately fences the
last accepted registration lifetime from its monotonic successful observation
plus `accepted_expires`. Preexisting deadlines are checked before native service
and before adopting a later success. Removal has a bounded deadline and shutdown
has a 35-second host guard around the provider's documented 30-second behavior.
Persistent service failures cannot bypass the guard. Failure retains live native
contexts/code and reports incomplete shutdown.

This is a private, registration-only controller milestone. It does not implement
INVITE, calls, provider media, RF attachment or Asterisk SIP activation. Synthetic
loopback evidence is distinct from real-provider/NAT or physical-radio acceptance.
Public source/artifact publication remains held for the separately recorded
combined-distribution license review.

The full standalone foreground fixture now calls the real `run()` and product
with an enabled nonnumeric local node, synthetic private secret and loopback
registrar. Only physical radio activation, readiness and quiescence callbacks
are substituted under `cfg(test)`, reusing the existing physical-I/O simulator.
It checks startup REGISTER/challenge completion, unchanged reload without a new
REGISTER, disabled-account reload/unregister with Call-ID continuity, and joined
SIGTERM exit. A reserved IAX port proves this nonnumeric SIP-only node opens no
IAX listener. The simulator must execute callback turns, emit no keyed/audio
output and join successfully. Child coverage profiles survive fixture cleanup.
This is real-process integration with simulated hardware, not physical-node or
live-provider acceptance; shipped startup and authorization are unchanged.

## Incoming SIP adapter checkpoint — 2026-11-13

The optional ABI-1 call tail preserves the registration prefix and keeps Sofia
handles entirely in the native shim. Eight slots retain a nonreused call token,
one initial SDP offer of at most 4096 bytes, and authoritative coalesced state.
Terminal slots cannot be overwritten by new calls or the registration event
queue; only explicit terminal release reclaims them. The consumer requires the
complete tail before attempting calls. This additive descriptor extension does
not require a SONAME change; existing registration-prefix consumers still work.
The current controller cohort requires the new tail. No product-host ABI change.

NUA media and autoanswer remain disabled. Registered accounts alone admit new
calls, explicit host decisions supply 200/SDP or refusal, and native dialog
states translate ACK, CANCEL, BYE and termination. Re-INVITE media changes remain
unsupported. Final shutdown also retires owned calls because Sofia can quiesce
an unanswered session without a separate call-state event. Host mailbox login,
private control and RF attachment authority are not supplied by this adapter.

The 8192-byte message bound, eight-entry native proceeding limit, send queue
and transaction expiry remain finite per operation. The proceeding limit does
not cap all completed or in-dialog transactions. This controlled loopback
milestone therefore does not claim a global hostile-traffic memory bound.
Independent wire coverage and host composition are distinct checkpoints; no
provider/NAT, product PCM, hardware or public distribution readiness is implied.

The native ownership review added two literal-wire regressions: out-of-dialog
OPTIONS releases its application-owned handle after the automatic response,
while in-dialog OPTIONS preserves its call; and delayed ACK cannot move a
locally closing call back to established. Call lifecycle observations are
monotonic. A test-only shim observer counts actual native handle destruction
without changing the selected third-party library or the production ABI.

## Private controller incoming-call checkpoint — 2026-11-13

The existing SIP reactor now owns native call decisions and retained product
steps. A bounded foreground receipt reuses authorize_incoming and the product's
consumed-handle contract. The private SIP handle reports protocol 3, account and
call generations, an opaque host identity, and the actual native transport
source address. Caller headers supply no mailbox or radio authority. Tracked
terminal native slots stay leased until their product callback is quiescent.

SIP uses the same private PeerIoWorker source and prepared/timed-send callbacks
as IAX. Only RTP/G.711/jitter translation is new: the released G.711 descriptor
handles partial input and flush, the existing packet-jitter capability handles
receive ordering, and the existing product handles PCM conversion, private
prompts, PIN commands, pacing, retry payloads and transmit age. No new shared
ring, resampler, prompt engine, executor thread or product-host ABI is added.
All native operations and callback attachment/detachment remain serialized on
the existing current-thread reactor; no SIP handle enters the IAX executor.
A refused source prefix or digit is retained without blocking native signaling.

Independent literal SIP/RTP now drives mailbox/PIN login and status through both
the actual product and the unprivileged full foreground process. The latter also
retains unchanged registration across reload, removes the account, and joins
SIGTERM shutdown. Both assert no radio audio/PTT. This controlled private-call
checkpoint does not authorize RF attachment or establish real-provider/NAT
media reachability. Public combined source/artifacts remain held for the
separate distribution review; hardware acceptance remains a separate milestone.

The focused private-call fixture also omits every mailbox record and verifies
that the initial login prompt and acknowledged BYE still work without radio
output. No account or PIN provisioning is required for that limited milestone.
A released-codec/jitter source test verifies exact refused PCM and gap prefixes,
unchanged original birth time, RTP timestamp wrap, and a refused RFC 4733 digit
with redundant event ends delivered exactly once.

A host close latch also schedules the terminal product step before ACK, even
when native hangup fails and emits no terminal call event. Callback stop remains
monotonic, so subsequent service turns cannot invoke the released worker again.

Receive birth currently records first userspace RTP dequeue/jitter admission.
Already-retained PCM keeps that age under refused credit, but datagrams waiting
in the UDP socket during a refusal have not been qualified for original arrival
age. This remains an explicit receive/RF freshness qualification item before
enabling radio attachment; the private prompt/DTMF hardware checkpoint does not
claim that stronger guarantee.

### Incoming RTP offset correction — 2026-11-13

A live-call failure exposed unqualified receive translation. The owned jitter
ABI permits signed packet offsets: a positive value is an exact missing prefix;
a negative value trims already-released samples. The SIP adapter now follows
the existing IAX playout semantics, looking up birth by the actual packet
timestamp, retaining prefix plus decoded suffix under refused source credit,
and committing the jitter decision exactly once after all output is accepted.
Delay insertion advances only playout time and emits no fictitious source gap.
Literal RTP/released-provider tests reproduce both signed offsets, insertion,
and the former premature decision commit. This fixes proven owned-adapter
defects; it does not retrospectively prove the cause of a particular live call.

The private SIP bridge reports a fixed operation (read, prepare, send or reset)
and a payload-free error category on terminal media failure. Jitter
operation/shape, source-credit contract, codec, owner and socket failures remain
distinguishable; active socket failures include only numeric errno (zero if
unavailable). These diagnostics contain no SDP, SIP headers, RTP/audio bytes,
DTMF/PIN symbols, account identifiers or arbitrary dependency error strings.

### Private programme return ownership

A prepared phone media generation supplies one ordinary bounded mix-minus queue
to its typed graph slot. The existing private serial session exchanges that
queue with its private-prompt queue at the same Outbound owner; it does not add
another converter, encoder, retry packet or PCM staging array. Private replies
preempt programme and finish their finite tail before programme resumes.
Original span birth/deadline metadata remains with the queue. The generation's
existing retirement token also guards copied conversion and blocked native sends.
This bridge alone does not authorize attachment, implement VOX or qualify RF.

Programme return drains its retained SRC/codec tail at the existing source-idle
deadline; successful finite private prompts still use explicit completion.
G.711 silence-tail padding and original-age deadlines remain with the same owner.

### Typed hosted phone graph preparation

The control owner can reserve one exact authenticated call per hosted node,
checking both current account and node policy before graph preparation. The
prepared graph uses AudioInput::Phone and AudioPeer::for_phone, preserving the
call token, existing processing factory, source ring and serial return wake.
Graph replacement explicitly retires the old media generation before preparing
a new one; it never transfers or duplicates transport ownership. Ordinary Engine
collection reconciliation may reprepare retired media while attachment intent
persists. Failed preparation clears that intent and emits a credential-free
warning. Committed reload and stop retain existing whole-call retirement.

This Rust preparation seam has no host ABI or operator-command activation.
The *3 command remains unavailable until source-rate VOX, retained onset, source
age and transmission qualification are composed. The targeted typed-slot fixture
is a preparation/lifetime test, not a voice or hardware acceptance claim.

Graph candidates retain their unpublished ingress until accepted publication;
Busy retries cannot fill the private owner's command queue. Delivery retains the
same endpoint under queue backpressure and rejects a superseded generation.
Rearm requires both the authoritative collection release and the serial owner's
matching settled/drained source revision, not a dirty-work flag or elapsed time.
A source reset without that release leaves media unavailable rather than
repeatedly replacing the graph; private control remains available.

### Source-coordinate VOX preparation

Typed hosted phone graph preparation now consumes the existing Radio ABI-5
speech-analysis descriptor directly, checking its size, capability and complete
speech tail. Product declares the released Radio alpha11 runtime floor. This
is an existing DSO dependency, not another classifier, radio session, host ABI
or programme buffer.

The serial source owner analyzes each accepted prefix exactly once. Existing
20-ms receipt buckets also retain bounded voice intervals. A positive source
position admits the still-retained 160-ms onset and the design's 1000-ms default
hang on that source timeline. The existing ring remains the only programme
delay. Silence is muted before shared processing and does not claim voice;
unknown receipt/processing age remains unavailable. DSP and all egress
conversion still have their existing single owners. Voice qualification follows
the same proven DSP history horizon as receipt age, retaining a final delayed
voice sample after the current source interval closes. This adds only scalar
metadata, not another PCM buffer or an input-amplitude test.

Tests compose the actual released classifier, source ring, qualified processing
history, typed product mixer and ordinary peer fanout, including seven-sample
credits, delayed decisions, short onset, hang closure and callback allocation
checks. These tests do not qualify live link/RF transmission: kernel socket
receipt age and original-age propagation into mixed peer egress still need
their existing boundary integration before operator *3 activation. Manual mode,
configurable hang and receive-window scheduling remain separately tracked
requirements; this checkpoint adds none of those claims.

### SIP socket receipt qualification

The existing single RTP owner now retains one monotonic last-proven-empty
socket bound, initialized before bind. Only an actual nonblocking kernel EAGAIN
advances it. A cached Tokio WouldBlock uses a nonconsuming kernel probe rather
than assuming the socket is empty. Queued datagrams keep the previous bound
while private source credit is refused; draining later cannot make them fresh.

The original sink reference maps that lower bound conservatively once into
the existing packet-birth slots. Unknown, underflowed or unavailable mapping
takes the existing source-gap path. This proves a socket-ingress not-after
bound, not NIC, wire or remote capture time. It adds no packet/PCM queue,
wall-clock mapping, receiver task or provider ABI.

### Original phone deadline in prepared network egress

The prepared graph validates the phone source clock against the existing peer
I/O clock identity, which shares the native source clock's Host origin. The
single render records each phone's original receipt bound; two minima compute
all mix-minus destination caps in linear time while excluding each destination's
own source. The span gains one eight-byte immutable deadline: original receipt
plus 500 ms. Unknown timing is already expired; ordinary-only spans remain uncapped.
Partial reads keep that deadline unchanged while advancing the existing native
birth. The native 200-ms egress allowance remains separate and unchanged.

The existing Outbound owner keeps compact deadline-only 20-ms buckets covering
its 200-ms retention allowance and filter/boundary overlap. Actual codec source
counts locate each packet; the released fixed SWR filter-size 16/cutoff 1 profile's conservative
support margin includes hidden FIR history and rounded phase. The interval is
rounded outward. Overwritten/missing bounds fail closed; zero-new-source codec
tails keep their preceding real-source cap. Fresh continuing input releases old
caps instead of pinning the first deadline forever. No PCM buffer, DSP, provider
ABI or thread is added. Pending packet expiry and private provider submission
use the same shorter deadline, retained unchanged under backpressure.

Operator attachment remains guarded. Ordinary IAX provider custody still needs
the existing timed G.711 token/retirement integration, including legacy ACK
transition and the lifetime-sticky mode. Application expiry alone is not proof
of that downstream provider boundary; unsupported negotiated destinations must
not fall back to legacy phone transmission.

### Explicit restricted ordinary IAX dial

The optional final ABI-10 host callback peer_dial_timed_g711 uses the ordinary
dial ownership/current-generation contract, but opens a per-call catalog with
only released neutral profiles 1 and 2. It completes initial local TX retirement
before publishing an ordinary ASL handle with capability 128. Neither a caller
ID nor an 8-kHz rate establishes this guarantee. The full catalog and listener
are unchanged; no provider API or codec dependency changes.

On supported 64-bit targets the host table grows from 416 to 424 bytes. The old
complete prefix remains valid, the new pointer is nullable, and partial tails
are rejected. The product rejects missing/incomplete capability and destroys a
wrong returned handle exactly once. Older/Asterisk hosts do not provide this
optional route. This additive host tail does not change the product SONAME.

The host token is lifetime-sticky: reset retires provider TX before encoder use;
failed reset cannot restore legacy media. Timed G.711 uses the released expiring
Mini path; ordinary reliable control remains serviced by the existing executor.
This increment supports fresh restricted dials only, not transitions of existing
unknown-codec/legacy-PCM calls. Outbound selection, retry transport intent, and
operator attachment activation remain separately gated.
### Timed ordinary owner and retry transport intent

A restricted ordinary handle binds the existing I/O source clock before its
serial owner can encode. All media for that handle uses the existing timed
G.711 callback for the rest of its lifetime, including ordinary-only programme.
Its idle completion pads the final short G.711 packet through the same encoder,
then suppresses the legacy empty Voice marker. Finite private prompts still
require their explicit successful completion. No second encoder or queue is added.

Phone-originated remote dial selection is explicit at lifecycle dispatch.
Permanent link/retry records retain only the restricted-transport bit through
recovery; they retain no PhoneSource, mailbox authority or private reply token.
A retry after the caller retires still selects the restricted catalog, while
its completion and loss notifications obey the existing ordinary link policy.
Generic dials remain unchanged. Graph admission for unknown existing egress
and operator attachment activation remain the following controlled increment.

### Controlled incoming phone VOX attachment

Authenticated phone *3 now invokes the existing typed attachment preparation
after both exact account/node policy checks. It retains one private owner and
the existing source-rate classifier, source ring, graph and egress owners.
Unknown ordinary transmitting or group destinations refuse the new attachment,
without dropping their existing owners. A final per-span legacy guard discards
phone-capped media before conversion, including when an ordinary prefix and
phone suffix share a read. Its bounded consumption and rejection accounting
preserve ordinary traffic after refusal; monitor/parrot policy cannot bypass it.

A fresh admission keeps its immutable time cut while draining known same-epoch
pre-cut jitter prefixes that have not entered its ring. It never changes their
birth or feeds them to classification. Once any PCM is admitted, stale input
still retires the generation; unknown birth/epoch and unresolved gaps retain
that retirement behavior. Proven same-epoch SIP loss now uses the bounded
continuity policy below, not blanket retirement. This avoids repeatedly replacing a graph just to chase
the real provider's retained jitter tail.

The composed loopback fixture uses literal SIP/RTP, actual private login and
DTMF commands, the explicit restricted ordinary IAX dial, actual providers and
shared product graph. Only physical radio I/O/binding is substituted. It checks
silence before authority/attachment, speech-qualified remote and local output,
quiet closure, destination mix-minus, attached private-command isolation, the
known remote return pattern and SIP hangup without ending the established
ordinary node link. This is controlled integration evidence, not physical RF,
NAT media, receive-window scheduling, arbitrary-codec or older-host qualification.

### Private SIP call-close diagnostics

The owned Sofia shim emits one record per admitted call and fixed close category
(remote BYE, remote CANCEL, native terminal, explicit local close/rejection, or
account shutdown). Records contain only a process-local nonreused opaque token,
a fixed category, numeric native status, and monotonic elapsed milliseconds
(`-1` when unavailable). A native status does not prove a wire response was
received. Local categories identify the invoked operation, not its root cause.
No SIP reason/header/URI, account/caller identity, authentication material,
DTMF, or audio is logged. Repeated events are coalesced per category; no
per-packet logging is added. This changes neither signaling nor attachment
policy and cannot retrospectively explain an earlier unrecorded disconnect.

### Ordinary-link phone attachment, 2026-11-14

Explicit authenticated phone attachment reuses the shared phone ingress ring,
DSP, radio-clocked mixer and existing ordinary peer egress. A transmitting
ordinary link is not ineligible merely because it lacks the optional timed
G.711 capability. Its negotiated codec, control owner, ASL text, temporary or
permanent lifecycle and provider remain unchanged. No reconnect, codec forcing,
provider format query or new clock is required for this route.

The shared queue's generation retirement, phone source qualification, private
commands, DTMF suppression and local DAC presentation fences remain in place.
Prepared egress retains its existing application age and stream checks; the
synchronous PCM fallback admits only locally current phone spans without
changing ordinary-only unclocked traffic. This does not promise expiry or recall
of audio already accepted into an ordinary network provider's own storage or
reliable retransmission queue. The earlier restricted-transport eligibility
policy is superseded; the P12 local DAC presentation requirement is unchanged.
Explicitly timed/private handles retain their existing provider contract.

The composed regression first establishes an ordinary link, then authenticates
and explicitly attaches a SIP caller. It retains the original one-second voice
fixture, mix-minus and private-status checks, return audio, and ordinary-link
survival after phone BYE. Its simulated 48-kHz device uses absolute 20-ms
deadlines so callback work is included in the frame budget. Missed slots advance
the source coordinate and are skipped, never replayed as a catch-up burst.

### Non-sensitive node operation outcomes

The owner-requested generic completed/unavailable responses have a distinct
typed message catalog entry with no caller-supplied arguments. Only this
constrained outcome returns exclusively to its requesting source. The owner
superseded the earlier mirroring requirement on 2026-11-14: an authenticated
phone receives only its exact private reply, while an RF-origin operation
returns only to LOCAL RF, never to attached phones. Retired or invalid private
callers do not fall back to RF. Existing status producers, bounded private reply
outboxes and local status delivery are reused; no PCM queue or peer fanout is
added. Existing exact-peer status replies remain source-scoped.

Authentication, mailbox/PIN prompts, policy denials, stopped macros and detailed
status retain their existing private or source-scoped routes. Phone tokens still
cannot enter public telemetry. Caller and generation checks precede outcome
delivery; actual dial/attachment completion, not accepted work, determines the
outcome. Unfinished RF dial attempts retain only existing source-operation
custody through initial retries; CLI, scheduled work, linked-peer commands and
established-link recovery do not gain generic local acknowledgments.
Authorized phone macro-expanded node dials receive the same fixed actual outcome;
macro-stopped and authorization details stay private. RF macro expansions remain
outside the new generic acknowledgment route.

Core admission precedes the host's final peer preparation and graph publication.
The dial owner borrows existing pending-source custody across that boundary.
If publication fails, ordinary rejection runs first; only a matching surviving
permanent retry may regain the same source record, after current generation,
call and macro-permit checks. Retired source authority cancels that unfinished
retry. Successful final publication discards the borrowed record, so later
established-link recovery retains no phone authority or RF result provenance.

The earlier combined synthetic qualification drained mirrored LOCAL results
before measuring phone speech. Source-only regression coverage now requires
phone operation outcomes to remain private and RF operation outcomes to leave
all phone reply queues empty. Authentication and detailed status remain private;
a private completed result cannot satisfy a public speech-output assertion. The synthetic remote
is serviced until its bounded caller completes, rather than stopping at an
independent cutoff before return audio is requested. Qualification commands
retain an outer process watchdog; this changes no production or protocol timer.

## Preserve ordinary input across phone rearm — 2026-11-14

An unresolved phone source discontinuity retires that phone media generation;
known timed SIP loss is governed by the bounded continuity amendment below. A
subsequent phone-only graph refresh preserves unchanged ordinary source owners
instead of replacing their inbound rings and restarting the 260 ms receive target.
Preparation holds only read-only reservations for those existing rings. At the
existing null-publication, hazard-free adapter cut, the complete ordinary source
set is validated before moving its unique consumer, processing graph and receive
history. A rejected or callback-busy candidate cannot change those live owners.

The serial ordinary producer, partially accepted input and source/DTMF state stay
in place. Only its outgoing queue is redirected, retaining the existing discard
of old mixed phone history. Retirement waits for the exact runtime-graph and
transport-connection acknowledgment after that redirect, plus existing dispatcher
quiescence. Old ring identity alone is not an acknowledgment of a new graph.
Reload and ordinary topology changes retain their current replacement semantics.
No new PCM queue, provider API, playout target or source-age rule is introduced.

An ordinary peer already detached from host ownership may leave a terminal slot
in the current audio graph until the next publication. Phone rearm may omit only
those terminal slots while retaining every matching survivor in order. The full
set and immutable source identities are validated before any move; a survivor
ending concurrently does not change the validated mapping. Callback-busy refresh
retains authorized attachment intent for the next ordinary pump, without making
the revoked phone generation usable. Hard preparation errors still detach media.


## Phone-only inter-digit tolerance — 2026-11-14

A phone account resolves `inter_digit_timeout_ms` (default 10000, valid 1–60000).
Malformed values warn and fall back through the ordinary account schema. The
existing private startup snapshots the value for the call, sharing it across
mailbox/PIN collection, phone grammar, and that exact source's destination node
collector. Ordinary radio and peer collectors retain three seconds. Explicit
terminators and unambiguous commands still complete immediately; inactivity
keeps existing finish/discard semantics, including private quote cleanup.

This is an input accessibility tolerance, not authentication or call lifetime.
The absolute 60-second login admission window, attempt count, node unlock expiry,
transport authentication settings and call retirement remain separate. A digit
timeout alone does not detach or hang up an authenticated caller. Existing
operation or private-prompt preparation failures may still retire a call through
the existing worker error path. No credential or digit logging is added.

Quote-control keys consumed by the outer grammar refresh only the matching
already-active, unexpired node collector. They never become node symbols, create
collection, renew authorization, or touch another source. This ensures slow
quoted terminators do not lose their destination while every actual key remains
within the configured gap. Inclusive expiry and rollback cannot revive or
shorten that existing deadline.

### Bounded outgoing SIP adapter

The neutral SIP descriptor appends explicit originate and acknowledge slots.
An outgoing-capable consumer requires both complete slots; incoming and
registration prefixes keep their layout. The call record's direction occupies
previous 64-bit alignment padding, and is never inferred from caller identity.
The synchronized consumer validates direction explicitly when this tail is
present; an incoming-only provider's former padding is normalized to incoming.
The complete incoming prefix remains usable, while partial outgoing tails or
a single callback without its partner are rejected. This additive owned
ABI tail retains SONAME1; outgoing deployment must ship the adapter and its
updated consumer together. No Sofia ABI is exposed or modified.

Outgoing and incoming dialogs share the existing eight owned slots, including
terminal observations awaiting release. The same remote-SDP extent holds an
incoming initial offer or an outgoing final answer. A dialing or failed
outgoing call may have no remote SDP/source; an answered/established call must
have both. The serial owner supplies the existing account credentials for at
most two bounded native call challenges, separately from account registration
state. Contact and transport route are passed explicitly through supported NUA
tags, using the already prepared account socket/Contact.

A successful final answer is retained until explicit host media validation and
ACK. Cancellation, answer rejection and teardown keep the same token until
native termination and explicit release. No outgoing callee receives incoming
mailbox or private command authority. This adapter checkpoint does not expose
operator dialing or establish node isolation; those remain controller work.


### Outgoing media-only source role

The exact existing PeerReply allocation stores a one-byte phone admission state
in place of its prior Boolean: unauthenticated incoming, verified mailbox, or
explicit outgoing media-only. The outgoing constructor is used only by a
controller-originated call owner. It never follows from transport registration
or a claimed endpoint. Mailbox/control authentication remains false for that
role, and the incoming authentication operation cannot convert it. The typed
phone mixer accepts either verified incoming media or explicitly owned outgoing
media, while preserving exact source/reply/ingress matching and retirement.

This scalar prerequisite adds no PCM owner, call thread or unlock grant. The
ordinary phone command collector still rejects media-only outgoing sources.
Actual controller origination, account/target policy and peer-isolation claim
publication must be completed before selecting it in the product.


### Outgoing SIP reactor and optional host factory

The existing SIP reactor retains an outgoing Offer on the exact advertised RTP
socket. A final answer must select only the offered G.711 and optional
telephone-event formats; an advertised event format cannot silently fall back
to in-band detection when its mapping is invalid. Before ACK, reconnecting that
socket requires a bounded real-kernel empty observation and resets its source
and event identity. No pending offer reaches incoming receipt admission.

The unique pending Dial owns its result channel and close flag before command
submission. Refused queue credit retains that same command locally; polling
retries the existing bounded queue without blocking caller cancellation. The
reactor checks the close flag before preparation and again before INVITE. That
flag then belongs to the same native slot and media Handle. No caller callback
or borrowed callback context crosses into the reactor, and no second thread or
PCM queue is introduced.

One nullable HostServices10 tail, peer_dial_phone, identifies the configured
account, initiating local node and provider destination separately. It runs on
ordinary control/lifecycle work, never on the SIP or audio callback thread. The
standalone host retains only a clone of its existing reactor sender and exact
committed account pointers. Reload suspends new admission while publication is
unresolved, restores the old cohort on rejection, and publishes only the
committed SIP cohort on success. Pending calls recheck the caller's current
predicate and the exact account pointer; changed accounts or stop cancel them.
The account's incoming default local_node does not silently override the
explicit initiating local node.

The previous complete ABI10 prefix remains valid with the new callback absent;
partial tails and an offered telephone factory without the existing complete
async/prepared/timed/source media group are rejected. Asterisk explicitly
supplies None. This owned optional tail keeps the existing host ABI version and
requires synchronized current header/consumer builds for outgoing use. It does
not imply outgoing Asterisk provider composition is implemented.

The factory returns the existing typed PHONE transport owner, not incoming
mailbox authority. The product still must prepare the typed outgoing role,
validate account/target policy, and acquire the initiating-node isolation claim
before selecting this factory. This seam alone enables neither an operator
telephone command nor a live outgoing call, and it does not provide linked-peer
custody isolation or outbound IVR DTMF.


### Outgoing source cuts and explicit local invocation

A called party has no mailbox or node-command collector. Its actual provider
source discontinuity still retires the old media generation and advances the
existing private source revision. The same serial owner records the final
revision after reset (including any completed digit) as a pending release; the
existing detector-tail and raw-drain checks must qualify it before publication.
Outgoing media preparation/rearm reads that existing settled source-ready scalar.
A newer onset or reset invalidates a captured candidate, and no original birth
or source age is renewed. Incoming calls retain their collector-owned release.
This reuses the private owner and scalar evidence, without another queue, clock,
synthetic digit or authentication grant.

Standalone adapts explicit `phone-dial` and `phone-hangup` requests through its
existing service-owned0600 local-control socket. Its existing version/frame bounds
remain in force. The product's typed command validates account, initiating node,
current call claim and provider policy. Accepted dialing returns only acceptance
and an opaque call identifier, not answer status or copied destination data.
Hangup requires that exact identifier. Socket disconnect retains a telephone call;
connection cleanup still owns only temporary manual RF requests. Stopped/fatal
runtime or absent optional product support is unavailable. No new listener,
thread, DTMF default or callee control authority is introduced. Neither request
content nor dialed destinations are logged by this bridge.

This first outgoing composition requires an explicitly peer-free initiating node
for the exact pending/active call. It is not the full linked-peer custody-isolation
implementation, outgoing Asterisk composition, or physical/public-provider call
acceptance. Those remain separately verified milestones.


The outgoing/incoming integration follows source-only operation results. RF
results never enumerate attached telephones, including authenticated incoming
calls. Explicit private results require the exact live mailbox authority and
never fall back to RF. An outgoing callee is eligible for call media but not
control results; a local result must not retire it. Media attachment enumeration
remains shared only where graph preparation and cleanup require it.

### Configured radio-origin telephone command boundary

The selected bridge adds optional `link_command_phone_dial` and
`link_command_phone_hangup` actions plus the inherited `phone_outgoing_account`
selector. All three default to unconfigured, like existing optional command
families; current command and macro namespaces remain unchanged. ASL-style6/0
are explicit fixture mappings, not new live defaults. Enabled dial prefixes
must leave every literal decimal destination unambiguous. Destinations retain
leading zeros and never recall or replace the last ordinary node.

Only initiating-node local command custody enters this first bridge. Incoming
mailbox authority and ordinary peer control do not authorize telephone dialing
or hangup. A typed request retains its original generation and any existing
macro permit. Product admission resolves the configured account/server and
captures the exact call claim in the same serialized turn. Telephone targets
use existing bounded SIP URI validation; no country, emergency-number or cost
classification is added.

Asynchronous work reuses the existing outgoing job and media-only role. Radio
hangup binds to the exact admitted pending/active call, never a successor found
later by local node name. Current actual answer/failure results use the same
source-only local result route; job acceptance, successful cleanup and stale
completion are not successful answers. This remains the peer-free milestone,
not full linked-peer isolation or physical provider acceptance.


### Radio-origin acknowledgment and provisional ringing (2026-11-15)

The owner's manual test changed the radio command sequence: the finite LOCAL
`Command completed` acknowledgment must finish before outgoing dialing starts,
not after answer. The existing dial job now waits for the prepared status to
return through its existing completed-media queue. A scalar receipt reports
successful finite controller playback only after control-side reclamation;
empty, failed, canceled, or replaced playback cannot authorize dialing. Exact
claim/source/revision cancellation remains authoritative while waiting. This is
not a physical DAC/RF delivery or reception certification. Administrative and
macro submissions retain their prior silent behavior. Answer no longer repeats
the acknowledgment; current refusal/failure retains the source-only unavailable
result.

Native SIP already supplies provisional call status. A nullable complete
`peer_dial_phone_progress` tail extends host ABI 10 after its prior 432-byte
complete prefix. The reactor shares only one numeric scalar on the existing
pending owner. The established-only factory remains available, and the new
borrowed callback executes only on its existing synchronous dial worker. It
reports terminal zero before return, including admission refusal. No callback or
borrowed context is retained by the reactor, and no provider ABI changes.

Actual 180 selects a locally generated 440+480 Hz indication at -12 dB: two seconds
of tone followed by four seconds of silence. Each finite cycle uses the existing
station tone producer, status queue, and completion receipt; at most one is
pending for the exact call. Before another cycle, serialized control rechecks the
original source generation, Engine revision, and exact claim. Status changes,
answer, failure and cancellation stop the exact tone, before answered media is
adopted. There is no private/network telemetry fanout and no guessed pre-signaling
ringing. 100/183 do not assert remote alerting; provider early-media playback is
not introduced by this increment. An older host without the tail has no
provisional feedback capability, rather than fabricated ringing.

The source-only result routing, explicit incoming attachment, called-party
no-login role and peer-free outgoing milestone remain unchanged. The combined
candidate also includes the separately qualified continuous-media path below;
the startup-only checkpoint is not selected.

### Outgoing continuous G.711 media (2026-11-15)

An exact live outgoing Phone recipient remains eligible for the node's
existing clocked mix-minus output while idle. Mix enablement, source
selection, recipient retirement, and public-telemetry exclusion are unchanged:
an idle route publishes zero PCM, not a newly enabled source. Transport
silence cannot key RF or grant incoming-mailbox authority.

The existing dispatcher, revocable output queue, serial private owner, SRC,
negotiated G.711 encoder and sole RTP socket carry those samples. There is no
startup-only datagram, second silence scheduler, PCM queue, thread, or RTP
stream. The current negotiated 8 kHz, 160-sample packet contract supplies
20 ms packets with the same SSRC, sequence space and source timestamps as
programme audio. Existing source cuts, deadlines and recipient retirement
remain authoritative; a late callback is not a license to restamp or burst
old programme.

Callcentric's [device guidance](https://www.callcentric.com/support/device/other)
disables silence suppression; its [AC-211 example](https://www.callcentric.com/support/device/telco-ac-211)
uses 20 ms packets. A
symmetric-RTP loopback fixture reproduced the preceding dependency on local
RX; a startup-only experiment opened that initial path but then suppressed
idle RTP. The continuous regression therefore exercises both negotiated
G.711 profiles through thirty seconds of quiet output before the unchanged
first greeting, without any receiver key or local RF output. This is bounded
transport evidence, not a claimed NAT timeout or physical-provider acceptance.

Private sender admission may observe a concurrent publication after its first
clock sample. Only when a span appears newer than that observation does it
resample the same immutable clock before applying the unchanged source-age,
future-birth and retirement checks. Original receipt and deadline values are
never renewed; invalid or backwards observations cannot advance known time.

An outgoing continuous route does not declare a finite burst complete merely
because its bounded queue is empty at the old source-idle deadline. The same
role predicate excludes that expired deadline from its wake selection. Explicit
finite playback completion, call/source retirement and the original stale
cutoff remain unchanged; incoming programme retains its existing idle finish.

Ordinary shared programme and continuous outgoing Phone media use the existing
source-clock front-of-queue due-time gate. A mapped callback point slightly ahead of a
poll remains in its original bounded queue until that original due time; it is
not discarded or restamped. Private replies retain their fresh-receipt future
rejection. A retired generation still drains immediately, and the original age
cutoff governs retained conversion while waiting. This reuses the ordinary
sender's existing wake selection rather than adding another scheduler.

Healthy-generation wire checks require consecutive 160-sample RTP timestamps.
An accepted source gap, concealment, reset or control cut may retire the phone
generation; its existing marked restart must retain sequence/SSRC and advance
source time, not replay retired samples. The synthetic test observes exact
accepted callback credit rather than accepting any marker as proof. Its
post-DTMF healthy interval continues real incoming zero RTP, since abruptly
stopping an established source would instead test network loss. These tests
preserve the existing reset-marker convention, not a new general marker policy.


### RF command collection custody (2026-11-15)

P15 suppression follows the existing RF detector/dispatcher pair rather than
the replaceable audio-graph generation. Qualified onset and digit completion
publish exact scalar cuts in the existing bounded event ring. Control retains
the original source receipt and clears only that cut after collection, quoting,
selected-remote input or keypad capture completes. A rejected qualified tone
uses an anonymous settlement, never a fabricated digit. Loss, timeout and unkey
use existing parser cleanup; later onsets cannot be cleared by older receipts.

The receive worker zeros command-interval PCM before the existing delay ring.
This prevents replay after control release without another PCM buffer or parser.
Detection qualification latency is not a new zero-delay guarantee; deployed RF
reserve and existing detector suppression remain part of the audio boundary.
### Admitted outgoing source versus audio topology (2026-11-15)

A telephone command retains its original graph work until serialized admission
has validated the source and reserved an exact outgoing call. It then consumes
that work into the original source permit, retaining the same macro cancellation,
inclusive deadline and source custody. The dial job separately retains the
original Engine configuration revision and exact call claim. Ordinary graph
replacement can reclaim its old storage without canceling that admitted call or
renewing its authority. Reload, stop, source cancellation and deadline still
prevent provider work/adoption. Ringback and failed-submission outcomes use this
same source/configuration/call tuple rather than an obsolete audio generation.

The source test proves old-graph reclamation after actual callback adoption,
original macro expiry/cancellation and refusal to newly admit retired work. The
Engine/factory matrix proves a graph-only refresh while ringing survives, while
revision changes and exact cancellation remain terminal. This is a prerequisite
for P13 linked-peer isolation, not a claim that isolation is implemented.

### Default outgoing isolation on existing owners (2026-11-15)

A reserved outgoing call prepares immutable ordinary-peer media suppression in
the initiating node's existing shared mixer before provider dialing. Ordinary
ingress continues through its sole ring/DSP/source owner, and DTMF, text, link
control and teardown remain available. Its programme and key demand cannot enter
local RF or the phone mix, and ordinary media destinations are ineligible.
Local RF, the exact outgoing phone's mix-minus and its clocked idle PCM keep their
existing paths. New and permanent-retry ordinary links may remain connected; their
published graph inherits the same isolation policy.

Entry and release replace ordinary ring/egress endpoints instead of transferring
their retained history. The existing serial redirect carries an exact connection
only for this cut. It discards application conversion and synchronously invokes
the applicable prepared-provider reset before its existing exact-observer ACK.
No new provider interface or downstream network-expiry guarantee is introduced.
A failed or missing required owner fails readiness; pruning it or refreshing the
same call's topology cannot replace that failure with apparent success. Pending
ACKs and callback retirement defer dialing. An already empty graph needs no cut.

Cancellation, failure or hangup restore the ordinary graph through existing
control pumping. Callback Busy retains the isolated policy until safe replacement,
and both boundaries revoke old input history. The one-call bound remains unchanged.
Unlock-qualified per-call bypass and local/all-peer playout remain a following
P14/configuration increment, not implicit authority from mailbox authentication.


### Negotiated codec boundary (2026-11-15, neutral checkpoint)

SIP's codec selection now records separate PCM and RTP clocks, provider mode,
packet-frame/byte bounds and directional payload IDs. This checkpoint continues
to advertise only PCMU and PCMA with the existing 20 ms, 8 kHz behavior.
The existing protocol-neutral codec client at immutable revision
`becbff939a0817cc91df6dcb0d40e85e381ec87b` replaces SIP's duplicate descriptor,
context-lifetime and encode/decode FFI implementation. Providers remain released
versioned dynamic libraries; no codec algorithm, resampler or external-library
wrapper enters the controller. Metadata and both directions are validated before
selected contexts are exposed. The existing fault-wrapped provider tests retain
partial-construction cleanup and exact packet-shape checks.

Phone jitter retains its 20/40/100 ms policy through the same validated client,
now parameterized by source PCM clock and packet bounds. Unwrapped RTP timestamps
are translated into that PCM clock before jitter and source credit. Transmit
RTP timestamps use the separate RTP clock. Original receipt ages, exact retained
prefix/gap credit, deadline and generation retirement remain unchanged.

Codec parity is incremental. G.722 requires 16 kHz PCM with an 8 kHz RTP clock;
it is not advertised at this checkpoint. Its next end-to-end checkpoint must
also qualify source-rate DTMF and the product's currently G.711-specific finite
tail and timed-send boundaries before advertisement or hardware validation.
Legacy Dialogic ADPCM and LPC10 wire mappings remain unresolved; IAX bitstream
availability alone does not establish standard RTP interoperability.


### SIP G.722 source milestone (qualification pending)

The SIP composition retains one verified released-provider catalog before it
publishes accounts. PCMU/PCMA remain preferred; G.722 uses static payload 9,
16 kHz mono PCM and 320 PCM frames per 20 ms packet, but its RTP clock is 8 kHz
and advances 160 ticks. The encoded bound is 160 bytes, never 320. Source,
jitter and ring coordinates stay in PCM frames; telephone events retain their
independent 8 kHz clock. No other codec or legacy ADPCM/LPC10 wire profile is
advertised.

Enabled SIP configuration preparation requires a 16 kHz source DTMF detector
before initial/reload publication. Disabled SIP and IAX-only configurations do
not acquire this requirement. The separately reviewed owned detector candidate
adds 16 kHz within the existing ABI 1 descriptor, retaining 8/48 kHz behavior.
Its alpha4 release must complete qualification and be pinned before deployment.
Local qualification selects the exact unreleased candidate only through an
isolated test library path. Later configurable codec sets must derive this
preparation requirement from their actual PCM rates.

`RPTADV_PEER_FINITE_PACKET_EGRESS` is an additive semantic host contract, not a
layout change. Only a complete private PHONE source/prepared/async handle may
claim it. It tells Outbound to omit its old G.711-specific prepadding. The SIP
owner consumes real PCM with native flush disabled, pads precisely the retained
packet with PCM silence through the same encoder, then performs native finite
completion only after the packet is complete. It returns real consumption and
source_frames separately from full decoded-PCM packet_frames, with unchanged
source_start and original receipt age/cap. OutputFull consumes nothing; reset
abandons retained PCM/history. Existing IAX and older hosts omit the bit and
retain their old behavior. Older products reject the unknown bit safely.

No new resampler, programme path, codec implementation or third-party dependency
is introduced. This milestone is not full codec parity or hardware qualification.

### Exact-call bypass and outgoing playout (2026-11-15)

The optional, initially unassigned PhoneBypass command reuses the initiating
node's existing exact-source administrative collector. It requires node unlock
even with remote-base access Open. Mailbox login, selected remote mode, CLI
availability and outgoing callee media permission supply no unlock grant.
Serialized admission captures the exact existing outgoing claim before consuming
the original graph work. The latch lasts only that call; unlock expiry does not
undo an applied bypass, and a successor never inherits it.

Prepared graph policy keeps call identity separate from the isolation bit.
Changing that bit performs the same ordinary-input/history cut as isolation
entry and release. The exact claim's unresolved ACKs or retained failure cannot
be erased by a policy change. A pre-publication refusal rolls the desired latch
back; a failure after publication preserves the actually committed graph's
policy and retirement custody. Cancellation observed during publication cannot
be reported as successful bypass.

Outgoing phone playout is a resolved configuration snapshot. Local is the new
outgoing mode's conservative default: transmit to local RF but do not forward
telephone audio to ordinary peers. All uses the existing transceive mode and
normal eligible-peer mix-minus. Default isolation still wins until that exact
call has bypass. Incoming explicit attachment forwarding remains unchanged.
Rearm reuses the same configuration; committed reload retires the call.

Confirmation renews only the original still-live source collector, using its
existing bounded grant. Human RF outcomes are LOCAL only; authenticated phone
outcomes remain private. The full ordinary-ring cut retires an ordinary issuing
reply token, so its later result and renewal are canceled rather than remapped to
a successor by name. Pre-cut failures can still target that exact live reply.
Macro/source cancellation and the original Engine revision remain authoritative.
This increment adds no media queue, provider interface, worker or call-concurrency
policy and is separate from the selected default-isolation hardware checkpoint.


The composed bypass fixture keeps one actual ordinary transport connected before,
during and after the same SIP call. It applies the optional command through real
RF tones with healthy RTP continuing, verifies both Local and All audiences and
source-private results, then proves ordinary media stays restored after the
unlock grant expires. It retains the existing call/hangup, mix-minus and bounded
watchdogs. The separate linked-IVR case preserves default isolation and exact
telephone digit privacy; these are synthetic composition checks, not deployment.

## Per-account analysis preprocessing settings — 2026-11-15 integration candidate

Phone speech settings are an exact-account analysis snapshot, separate from
transport identity and registration settings. Control-side attachment preparation
resolves the exact account and passes that snapshot through the private call
owner into the shared-provider classifier and existing programme ring. This is
an offline integration candidate, not deployed preprocessing.

The explicit preprocessing opt-in defaults off. Plain analysis retains threshold
0.5 and one 16-ms onset frame. Enabling preprocessing selects threshold 0.75 and
two onset frames unless those values were explicitly configured. The shared
analysis branch uses the existing 300–3000 Hz bandpass before RNNoise; either
stage can be disabled independently. Finite threshold is inclusive 0–1 and onset
is any positive u32 frame count. Cutoffs must be finite, ordered and below 24 kHz;
provider preparation must additionally enforce the negotiated decoded source's
Nyquist limit and fail explicitly rather than clamp or bypass selected filtering.

Backward voice marking, future voice hold, and programme reserve are independent:
defaults are 160, 1000, and 160 ms. Marking may be zero and is bounded by the
existing 300-ms programme capacity. A marking request larger than reserve does
not promise that already rendered history is recoverable. Hold is a u32
millisecond source-clock deadline and allocates no history. The reserve range is
86–280 ms: the released ring requires worst-ratio headroom for 4096 output frames
at 48 kHz and room for a 20-ms producer quantum within its 300-ms capacity.
The existing 64 receipt buckets cover 1280 ms, so programme capacity is the tighter
lookback bound. None of these settings adds another PCM buffer.

Unknown/malformed values retain the normal exact-account defaults with warnings.
An invalid cutoff order restores the complete 300–3000 Hz pair with warnings.
Warnings for threshold/onset name the effective selected preset. No node/general
inheritance is introduced and tuning does not change account equality.

Existing controlled reload semantics remain authoritative: a successful reload
retires the current private call generation, and the next call/attachment uses
the new committed configuration. Failed reload retains the prior configuration
and call. Analysis objects must be prepared outside callbacks; no tuning mutates
a live graph or recreates it from a source callback. Original accepted programme
PCM remains the only ring input. Preprocessing is analysis-only.

The consumer requires the complete speech-analysis capability tail and bit 0
before passing the 32-byte configured-constructor record, including plain tuned
analysis. An older provider that ignores unknown config bytes is rejected, not
used as a silent unfiltered fallback. Filter bounds use the decoded PCM source
rate, including 16-kHz G.722, never the RTP timestamp clock.

Source callbacks write only the original accepted PCM prefix to the existing
ring before analyzing that same prefix. Ring reserve is forwarded through the
existing receive-policy path rather than adding a second delay owner. Source
discontinuity immediately revokes media but retains its DSP in the existing
serial-owner slot until non-callback replacement or cleanup. A fresh selected
graph still requires the existing exact source revision, authorization and
collector-release protocol; no reset grants readmission.

The selected private cohort now uses a 255-tap native FIR before RNNoise,
not the earlier FFmpeg bandpass recipe. Radio runtime 48c692d with the 3ae3e5c
header and SDK runtime 6a55584/source 2d2589a passed separate provider checks.
The frozen phone-only source 336a4d3 passed its complete affected controller
suites, all 21 isolated actual-call cases, C composition and strict checks.
The earlier FFmpeg native-allocation failure remains historical evidence for
that different recipe, not a passing result or the selected FIR implementation.

This source-only codec/analysis union has not inherited those execution results:
its merged source and exact runtime must be qualified together. Distributable
artifacts, full quality gates, native platform coverage and live qualification
remain separate.

### Directional telephone digit selection (Batch A)

`RPTADV_PEER_PHONE_INBAND_DIGITS` keeps its existing receive-and-transmit
semantics. The additional `RPTADV_PEER_PHONE_INBAND_EGRESS` bit512 requests only
transmit synthesis through the existing finite private digit owner. Native
received events remain actionable, and their source credit, exact-call authority
and deduplication are unchanged. The complete PHONE/ASYNC/PREPARED/SOURCE_MEDIA/
PRIVATE_CALL group is required. Redundant declaration of both in-band bits is
harmless and retains the original in-band receive policy. Other peer kinds,
incomplete callback groups and unknown bits remain rejected.

This is an additive semantic capability, with no ABI layout or SONAME change.
Old hosts preserve their behavior with the bit absent; old products reject the
unknown capability rather than silently discarding negotiated receive events.
Only the private transmit control copy uses the new accessor. The session's
receive policy still uses bit64, and there is no second detector or queue.

### Fixed-frame SIP packet and event mappings

The SIP transport validates encoded packet bytes separately from PCM frames and
RTP ticks. GSM, G726, AAL2-G726 and both L16 rates reuse the released neutral
codec client. RTP L16 converts the existing little-endian PCM16 provider's wire
words to network byte order at the protocol boundary. No converter is duplicated.
The serial egress retains one setup-sized wire buffer and one pending datagram;
WouldBlock retains the original bytes, sequence, timestamp, marker and deadline.
The source birth, finite true-source span and retirement rules above are unchanged.

For the supported 8/16 kHz RTP clocks, SDP keeps at most one event mapping per
clock, including independent local receive and remote transmit payload numbers.
An incoming answer selects a matching event clock or genuinely omits events.
An outgoing valid mixed-clock answer retains native event reception but publishes
bit512 for synthesized in-band transmission when no matching clock was selected.
The endpoint independently refuses mismatched-clock native event transmission.
G722 matches 8 kHz events despite decoding to 16 kHz PCM. Valid answer payload
renumbering does not authorize reinterpretation of an offered payload format.

Native receive-event history remains bounded by the existing exact remote SSRC;
its identity includes the negotiated event clock, raw onset and symbol. Repeated
reports and finals within that mapping yield one completed digit. Cross-clock
onsets are not scaled or equated, since their offsets need not share an origin.
This is not universal deduplication of a sender encoding one physical key twice
under different clock mappings. Mixed-clock event transmission and RTCP clock-
switch signaling are not implemented. In-band fallback is permitted by RFC4733,
but an event-only IVR need not recognize it. Outgoing callee digits retain their
existing lack of controller-command authority.


### Bounded native SIP receive units

Receive packet shape is independent of the selected fixed transmit packet.
The selected decoder is prepared for complete native coding units through a
30 ms local ceiling. GSM remains one 20 ms coding unit under that ceiling.
Encoder configuration and paced transmit size are unchanged. This receive
checkpoint does not yet add iLBC or change the SDP packet-time policy.

The RTP owner requires a call-local framing inspection before copying payload
or adopting SSRC. Native Malformed is discardable peer input; other provider
failures and impossible successful span/mode/length reports remain terminal
codec failures. Inspection does not advance decoder or concealment history.
The existing jitter owner retains actual encoded length and inspected PCM span;
playout decodes only those prefixes through the existing recovery operation.
L16 swaps only the actual payload prefix in its prepared scratch.

The same retained PCM/gap credit, signed overlap handling, original receipt
birth and retirement rules apply to 10/20/30 ms receive spans. There is no new
queue, callback owner, provider ABI, converter or decoder per packet. Only the
selected codec contexts remain live; startup bidirectional probes are temporary.
The worst supported L16/16k receive payload is 960 bytes and 480 PCM frames.
This is an implementation ceiling, not a claim to accept every RTP packetization.


### iLBC30 negotiation checkpoint

The same catalog now includes the released iLBC profile11 with explicit mode30:
240 PCM frames, 50 encoded bytes and 240 RTP ticks at 8 kHz per 30 ms transmit
packet. The default dynamic payload is100. Both selected contexts are prepared
before advertisement. RFC3952 resolves our mode30 preference against either
peer mode20 or mode30 to mode30 in both directions; absent mode uses30. Reserved,
unsupported and duplicate mode declarations are not inferred from payload length.

The local SDP receive maximum is30 ms for the entire advertised catalog, matching
the complete-unit decoder bounds. A mixed-duration offer omits common ptime; a
selected answer emits the profile's preferred packet duration. Positive decimal
ptime is a preference, while peer maxptime limits which fixed TX profile may be
selected. An incompatible iLBC maximum can still leave an offered20 ms codec
usable. The peer's receive maximum never changes our advertised receive ceiling.
Decimal parsing validates all digits without floating point, with a harmless
receiver extension accepting trailing fractional zeroes such as20.0.

This adds no codec implementation, provider version, ABI or media queue. Focused
native-provider, SDP, UDP, finite-tail and source-credit checks are distinct from
actual full-call, external-peer, hardware and aggregate qualification.

### Explicit SIP decoder recovery provenance

The serial SIP media owner distinguishes successfully decoded packet PCM from
explicit decoder recovery and unsupported native concealment. It retains the
concealed flag with the original source coordinates through sink backpressure;
unsupported concealment uses the counted ring-fallback seam when its original
history is qualified; unknown timing remains a hard discontinuity. A per-call payload-free control-side snapshot exposes jitter playout
observations and measured recovery counts, not a network loss percentage. The
original checkpoint left the provider C ABI unchanged and could not distinguish
hidden native iLBC recovery. The codec-unit amendment below supersedes that
limitation through an append-only operation. The bounded
continuity amendment below remains undeployed and is not hardware acceptance.

### Prepared telephone courtesy ownership

An optional named courtesy assignment with `input=phone` uses the existing
prepared media selection independently of receiver and public-link assignments.
Absence is off; structural uniqueness and inherited warning/fallback rules remain
unchanged. This is local-transmitter telemetry, never private-call or peer egress.

Control assigns each prepared private input a nonzero, never-reused scalar media
generation, paired with the existing actual ingress. Its mixer retains the exact
original call and input-media tokens. The audio controller holds only Copy keys
for pending and active phone selections and borrows the current mixer's retained
fences before each playout sample. A missing key, retired call, or retired input
uses the same exact-key cancellation as rekey: cancel pending and active phone
playback without successful completion credit. Mixer replacement cannot adopt
old private selections, even for the same live call. Token cloning and reclamation
remain on control; audio neither allocates nor drops an owning token.

Public-link rekey still cancels only pending courtesy, and unrelated sources
retain their existing behavior. The typed `phone_speech_closed` boundary reuses
normal courtesy delay and kerchunk policy but does not confer call authority.
This checkpoint deliberately has no production speech-close emitter: generic
private inactivity, attachment loss and retirement are not genuine held/drained
speech closure. That proof remains the source-owner follow-up; no PCM queue,
worker, generator, provider ABI or node configuration changes here.

### Bounded explicit SIP concealment and rendered speech lifecycle — 2026-11-16

The counted gap callback is not itself proof of packet loss. The SIP owner now
distinguishes jitter missing decisions and positive known packet offsets from
unmapped receipts and unproven adaptation skips. Unknown receipt timing and
unqualified skips first traverse the retained source-discontinuity callback.
Insertion changes only playout time; overlap and duplicate credit never publish
the same source interval twice. Incoming configured account protocol is copied
into the existing serial private owner; outgoing admission already requires a
SIP reservation. IAX keeps its previous gap/concealment retirement, without new
reset/revision behavior or protocol inference from capability flags.

One leased missing unit uses the codec's existing native concealment callback
once, retaining PCM and original receipt bounds through zero/partial credit.
Unsupported units use ring fallback once instead. The already prepared PCM
scratch bounds large spans; encoded successor packets stay encoded until their
missing prefix has released that scratch. There is no new PCM queue, worker,
codec negotiation, provider ABI or independent concealment duration.

The existing source receipt ledger has a preparation-allocated two-bit overlay
per source slot: unknown, original, synthetic and eligible synthetic. Metadata
precedes PCM credit and follows the existing fail-closed revision snapshots and
playout reclamation. Mixed one-sample and fractional conversion boundaries are
exact. Each existing bucket keeps separate conservative original and synthetic
birth bounds, so inherited PLC age cannot contaminate a fresh original receipt.
Provider shortfall against an original slot cannot borrow that receipt as
authority for synthetic continuation. A later original VAD decision can mark only original preroll, never
retroactively promote synthetic onset. Synthetic continuation requires genuine
qualified output to have reached the consumer and remains within the original
source hold. It inherits the preceding original-history bound, never current
time, and does not refresh last-voice, the hold endpoint or source age. The
existing mixer/egress age limits still apply independently.

Only genuine accepted source PCM enters speech analysis. A concealed interval
forces an analysis-only reset before the next genuine prefix, never a reset of
programme retention. The old configured-preprocessing provider cannot perform
that reset and therefore fails closed; its exact limitation and the separately
reviewed dependency work are recorded in the analysis-history-reset design.
This checkpoint must not be described as complete preprocessed loss recovery
until the reset-capable provider cohort is independently qualified.

The Copy source lifecycle is Open, Draining, Closed or Unavailable. Closed
requires the original hold's exact (including fractional) boundary and verified
absence of qualified post-DSP tail. Initial inactivity, unknown timing, failed
processing, command mute and retired call/media are not closure. Empty renders
do not advance evidence. Processed receipt history carries explicitly bounded
synthetic continuation without converting it to original audio.

Source state deliberately omits a second courtesy key. It belongs to the unique
input already owned by AudioPeer's nonzero prepared PhoneCourtesyKey; that slot
and LinkAudio's exact call/media fences supply the identity when the controller
consumes the evidence. Replacement inputs cannot adopt predecessor keys. There
is still no production call to phone_speech_closed and no courtesy emission.
The original explicit-recovery checkpoint could not classify internally recovered
iLBC units. The codec-unit provenance amendment below supersedes that limitation
without PCM heuristics or controller access to native decoder state.


The processing history's conservative possible-tail predicate is not onset
proof: its lookback includes newly accepted input while output may still contain
older muted PCM. Synthetic eligibility and lifecycle opening therefore use a
separate bounded scalar proof: a validated delivery observation whose whole
possible source-dependency interval is genuine qualified input. Single-sample
proof uses the current delay upper bound; block proof uses the prepared bound
that covers every output in the block. Possible-tail retention continues to
render original programme and its processed tail independently of that proof.

An utterance shorter than the graph's upper bound can remain unprovable under
the current buffering contract. Its genuine programme and tail are not clipped,
but it does not authorize synthetic continuation or a verified lifecycle close.
No amplitude classifier, inferred exact DSP delay, or prolonged-speech waveform
gate is introduced. This conservative authority limitation is explicit pending
a supported richer delivery contract; it is not a claim of exact DSP provenance.

### Reset-capable standalone analysis activation (2026-11-16)

The separately reviewed radio/RNNoise candidate supplies explicit in-place
analysis reset without synthetic inference. The controller now requires that
capability during media preparation; older configured providers cannot be
admitted and fail only on first loss. The existing accepted-genuine-prefix seam
resets analysis after known loss and then analyzes only that accepted prefix.
Zero credit, insertion and retry do not analyze synthetic PCM or reset the
programme ring. Successful reset leaves admission, epoch, retained PCM, original
hold endpoint and receipt age intact; later genuine decisions use their true
source coordinates. Existing failure propagation remains fail-closed.

This is local candidate composition, not a reset-capable release/package floor,
hardware acceptance or permission to activate a node. Courtesy emission and
the independent codec recovery-provenance integration remain separate work.


### Retained codec-unit authority (2026-11-16 local checkpoint)

SIP now calls the reviewed neutral decode-with-provenance operation exactly once
for each retained packet, replacing both the neutral recovering fallback route
and AudioCodec's former Malformed-to-conceal branch. Any post-advancement failure
is one hard codec error and grants no successful PCM authority. Explicit jitter
missing-span conceal remains separate, once-only and bounded by existing scratch.

The selected current profiles need one preparation-allocated metadata slot under
the existing 30 ms receive ceiling. The client validates the complete span map
before any source delivery. Media retains both original PCM and original span
offsets; it moves only accepted source credit, never shifts/relabels retained
PCM on overlap or retries. Each offer stops at the current origin boundary.
Artificial mixed-map cursor tests do not permit longer SIP packets.

NativeRecovered units traverse the existing concealed continuity callback using
the conservative preceding admitted original birth. A corrupt packet's receipt
cannot refresh the original hold or source age. Without original history they
traverse the hard counted-gap seam, not fabricated current-time PCM authority.
Recovered-only audio cannot initiate speech, DTMF or RF. The existing product
analysis-only reset still occurs on the first accepted genuine prefix after
loss, not synthetic audio or refused/retried credit.

PacketDecoded is authority at the accepted coding-unit boundary, not a promise
of sample purity: native decoder, PLC and enhancer/filter history stays intact.
A later valid coding unit may requalify using the normal speech thresholds.
Genuine encoded silence after recovery remains PacketDecoded without any
exact-zero or amplitude claim. No washout, Opus/FEC negotiation, new classifier,
programme queue, audio worker, source-age policy or IAX/Asterisk path is added.

This integration remains an inactive local candidate with independent review,
native ARM64, complete coverage/package/hosted gates and real hardware acceptance
outstanding. ADR 0022 and debian/control retain explicit publication blocks until
actual provenance-capable codec and reset-capable radio/RNNoise artifacts exist.
Courtesy emission remains separate work.
