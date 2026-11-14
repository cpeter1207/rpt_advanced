# Outgoing SIP controller implementation plan

> For agentic workers: use superpowers:executing-plans; follow the existing approved design task by task.

**Goal:** Make and complete an outgoing telephone call through the actual controller.
**Architecture:** Reuse the existing synchronous Sofia account owner, fixed call slots and reactor; carry a typed outgoing role into the existing phone media owners without mailbox authority.
**Tech stack:** Rust, owned C shim, unmodified Sofia 1.13.19, released RTP/G.711/ring/DSP interfaces.
**Spec:** docs/superpowers/specs/2026-11-09-phone-patch-and-half-duplex-design.md; Task 4 and Task 6 of the existing implementation plan.

Scope: execute the approved phone-patch design P01/P13/P15/P17 through the
existing controller and unmodified Sofia 1.13.19. Incoming mailbox/private
control and explicit attachment remain unchanged. Base: diagnostics 07b9fd8.
Live/staged d72 and the independent half-duplex branch remain untouched.

## Review focus

- Outgoing Digest challenges cannot change the account registration state.
- A final SDP answer cannot activate media before host validation and explicit ACK.
- Cancellation, late/repeated success and slot exhaustion cannot duplicate ownership.
- An outgoing recipient cannot acquire incoming mailbox/control authority.
- Node isolation must precede INVITE and survive until the exact call releases it.

## Task 1: First adapter checkpoint

**Files:** rust/sip-adapter/native.c, native.h, include/rptadv_sip_adapter.h and src/lib.rs; rust/peer-support/src/sip/registration.rs; tests/test_sip_outgoing.py and existing incoming fixture.
**Interfaces:** append originate_call(context, generation, destination, offer, token_out) and acknowledge_call(context, generation, token); expose direction on the sized call record. No Sofia types cross the adapter.


- [x] Write an independent literal UDP fixture for account registration, outgoing
   INVITE with the existing G.711/telephone-event offer, bounded Digest
   authentication, provisional/final answer, explicit host ACK, and BYE.
   Verify no ACK/media activation before the host accepts the answer.
- [x] Add only explicit originate/acknowledge operations to the sized owned
   descriptor. Reuse its eight call slots and one 4096-byte remote-SDP extent;
   add a direction scalar so an outgoing callee is never incoming admission.
   Incoming stores its initial offer; outgoing stores its final answer.
- [x] Reuse the account secret, native challenge parser and synchronous owner.
   Keep call authentication separate from registration observations; repeated
   challenges terminate within the existing bounded lifecycle. Keep wire text
   out of diagnostics. Caller URI validation reuses the existing narrow grammar.
- [x] Cover cancel/rejection, invalid/missing answer, duplicate final response,
   stale generation and shared capacity, then existing incoming/registration
   regressions. Commit after targeted strict checks and read-only review.

## Task 2: Actual-controller checkpoint

Extend the existing account reactor with typed outgoing ownership, prepared
RTP offer/answer binding, and existing PCM/ring/VOX/egress paths. Add node-origin
dial/hangup lifecycle without granting mailbox or protected-command authority
to the recipient. The first controlled node has no ordinary peers; reserve and
hold that condition before INVITE and for the call. This is an explicit first
checkpoint, not full linked-peer isolation. Failure/hangup releases only the
call's claim. Exercise bidirectional media and teardown in the actual Product
fixture before any parent-announced hardware test. Provider destination/cost
policy is not duplicated. Exact live target and cost approval remain required.

## Task 3: Follow-on isolation requirement

Implement acknowledged bidirectional linked-peer custody revocation before
dialing, independent per-call claims/unlock bypass, outgoing telephone DTMF,
and remaining composed cases using the existing design. Do not claim legacy
provider accepted PCM is revoked merely because application queues are cleared.

## Verification and resource controls

Behavioral RED before implementation; serial affected checks in the existing
constrained launcher (one CPU, 1536 MiB, no swap, 256 PIDs, 20-minute bound).
No heavy job while native staging owns the slot. Keep logs in .work, commit
verified increments and bundle locally. No private source/binary publication.

Task 1 verification: cargo test --locked -p rptadv-sip-adapter, literal
tests/test_sip_registration.py, tests/test_sip_calls.py,
tests/test_sip_outgoing.py and tests/test_sip_native_events.py against the built
DSOs, plus affected strict Clippy/C/Ruff checks. Expected initial action RED:
"outgoing call was not admitted" from the deliberately unimplemented operation.
Expected GREEN: literal INVITE/auth/answer/ACK/BYE assertions pass without
registration-state failure; existing incoming ownership/diagnostics still pass.

## Adapter checkpoint evidence — 2026-11-14

The outgoing adapter and optional consumer observation tail are verified; no
product origination command or outgoing reactor role is selected yet. The
literal fixture covers initial Digest, provisional/final response, duplicate
finals before/after host ACK, BYE, CANCEL racing with200, invalid answer,
rejection, shared capacity, nonreused tokens and repeated authentication failure.
The cancellation race was reproduced before adding its one-shot late-answer
BYE obligation. Optional consumer prefix/padding/direction assertions were
recorded RED before their repair. Existing registration/incoming/privacy and
native-event scenarios remain green, as do affected strict checks. Evidence is
in .work/outgoing-final.log; this is not the full pull-request quality gate or
real-provider telephone-call acceptance.


## Task 2a: Existing-reactor media preparation checkpoint

The next test-first increment retains one pre-answer UDP endpoint in a typed
pending originator claim. The prepared account's numeric route selects its
local interface; the offered RTP port is never replaced. A final answer must
select offered G.711/RFC4733 media and the same address family. Reconnection
requires a bounded real kernel receive drain ending in EAGAIN; exhausting the
budget aborts adoption. No queued pre-answer packet establishes the source.

The control caller creates a unique cancellable pending Dial before submission
to the existing account command queue. Full-queue refusal retains the exact
command in that Dial and is retried nonblocking, so cancellation remains usable
during queueing or socket preparation; the reactor checks that same close flag
before emitting INVITE. It never blocks the serial reactor until answer. Its fixed native call
slot retains offer, result sender and exact close flag. The final answer is
validated into the existing Media owner before explicit ACK and Handle
publication. Outgoing observations never produce an incoming Receipt. Drop,
refusal and account retirement terminate the exact slot; no mailbox or node
control privilege is created. One current-thread runtime and existing media
queues remain unchanged.

Verification uses the real reactor and literal loopback registrar/recipient:
INVITE/180/final answer/ACK, selected codec on the same advertised socket,
no incoming receipt, refusal/unsupported answer, pending cancellation, account
retirement and repeated admission beyond the retained native slot count.
Actual product outgoing role, command mapping and the call-time peer-free
reservation remain the next increment, not a claim made by this fixture.


## Task 2b: Typed actual-controller caller scope

Existing requirements are Task4/Task6 and design P02/P13/P15/P17. This is
telephone origination, not an ordinary AllStar node dial. No default DTMF
prefix is specified by those documents, so do not invent one while adding the
typed operation. Retain explicit incoming attachment and all mailbox checks.

Smallest cross-host boundary: append one optional account-aware telephone-dial
callback to the sized HostServices10 table, retaining the existing dial's
current-generation callback, returned handle ownership and ordinary PeerIo
operations. Pass configured account, local node and provider destination as
separate spans. Older full prefixes remain recognizable but outgoing operation
returns unavailable; reject partial tails. Standalone binds its existing SIP
Client/account generation on foreground publication. Asterisk supplies an
explicit absent callback until its actual provider composition is implemented;
that is a documented feature gap, not successful both-host coverage. No provider
ABI change or new transport thread is needed by this consumer slice.

The returned PHONE handle is adopted through a typed OutgoingMedia path. Keep
transport direction and the initiating operation distinct from incoming mailbox
credentials. Reuse PhoneCalls, PrivatePhoneSession, the phone ring and egress;
introduce a media-specific grant checked against the exact live call rather
than setting phone_authenticated. Outgoing called-party digits cannot enter
LoginCollector, PhoneDtmfGrammar or a node command collector. Incoming role
continues its existing authentication and explicit attachment path unchanged.

Before submitting INVITE, serialized product control must atomically reserve
the initiating node only if it has no ordinary peers. The same call claim
prevents a peer attach/retry from publishing until failure or hangup releases
that exact claim. Do not change node-wide configured connectivity or unrelated
nodes. This first milestone does not claim linked-peer queued custody isolation,
multiple isolated calls, outgoing IVR DTMF or unlock bypass is already complete.

Tests first: typed operation and account/local policy, rejected busy peer set
without INVITE, concurrent peer publication denied during the claim, stale
runtime/cancellation, wrong returned protocol/account/local rejection, outgoing
media without mailbox/control authority, exact release on refusal/hangup/reload,
and actual ProductRuntime bidirectional telephone audio. Repeat existing incoming
login/control/explicit-attachment cases and normal-target strict checks. Budget
from the verified reactor fixture is roughly3–6 active hours plus serialized
slot waiting for this actual-controller slice; native build/manual acceptance
remains a separate parent-announced milestone with target/cost approval.


## Reactor/host recovery checkpoint evidence — 2026-11-14

The optional standalone host factory and existing-reactor originator are source
reviewed and pass their focused actual-callback cancellation and ownership
fixture. Queue refusal retains one exact cancellable command; cancellation while
its offer is prepared produces no INVITE. Product complete-prefix/media-group
validation and Asterisk's explicit absent callback pass, together with affected
all-target strict Clippy and formatting. This is a local recovery checkpoint,
not the full quality gate. The isolated artifacts also pass four outgoing reactor cases, ten call-media
cases, six registration/owner cases, ten foreground-account cases, the full
foreground SIP reload/shutdown case, nine product service cases, the Asterisk
unavailable case and the C ABI probe. Final strict checks and formatting pass.
The synthetic incoming-only descriptor explicitly initializes the optional tail
to NULL; the foreground expectation matches its existing no-output diagnostic
without weakening the actual radio assertion. That literal correction overlaps
the separate 503 regression checkpoint 10d56a0. Evidence is recorded in
.work/outgoing-host-final.log, outgoing-host-remaining.log and
outgoing-host-tail.log, whose final continuation exits zero.
Product command/policy/isolation selection and real outgoing-call acceptance
remain Task 2b, not behavior enabled by this checkpoint.


## Serialized product claim and typed entry checkpoint — 2026-11-14

The existing PhoneCalls owner now accounts for pending and active outgoing calls
under its shared bound. A checked process-local claim holds the initiating local
node and configured account; actual provider admission must match that exact
claim and current runtime. The callee receives only OutgoingMedia, never mailbox
login or node-command authority. Its digits release the existing media source cut
without entering either collector. Cancellation of an uncertain already-adopted
completion is observed by the existing phone pump and revokes only that call.

For this first peer-free milestone, control checks retained ordinary peer owners
before dispatching the factory. The final ordinary publication boundary rejects
new/late peer owners during the claim. Retry admission pauses before consuming
its existing permanent intent. Failed/refused completion, exact hangup, committed
reload and stop release the claim; ordinary connectivity settings are unchanged.
This is not the full linked-peer queued-media isolation design.

The product ABI8 descriptor has one optional typed phone-command tail (offset128,
full size136 on the tested64-bit ABI), with separate account/destination and exact
call identifier. It accepts or cancels work; success does not claim an answered
telephone call. Both host readers copy a complete old prefix into owned zeroed
storage and reject partial pointer tails. Function code remains retained by the
existing loader. No DTMF mapping or existing node-command syntax is overloaded.
Standalone has the prepared SIP originator; Asterisk still returns unavailable
through its explicitly absent outgoing host callback.

Behavioral REDs cover missing reservation/media authority cleanup, admission
before factory, retry exclusion and both descriptor-prefix readers. The focused
product/reader/provider checks and strict all-target Clippy/formatting pass.
The actual ProductRuntime wire/two-way-media composition and an operator invocation
remain the next checkpoint; these helper/ABI tests are not a completed live call
or the full PR quality gate. Detailed local evidence is in the outgoing-product
RED/GREEN logs under .work; the host diff check is separate because container
mount paths do not resolve the host git-worktree metadata.

Checkpoint verification continuation:93 existing/new phone and private-owner
tests plus36 Host phone tests pass, including incoming authentication, explicit
attachment, source retirement and reload. The remaining lifecycle/runtime filters
were intentionally stopped at the next filter to yield the sole build slot to an
urgent independent repair; they are not reported as passing. The focused15
outgoing tests, both tail readers,31 provider cases, all-target strict checks and
formatting passed earlier. This is a reviewed local recovery checkpoint only.


## Task 2c: Actual wire fixture and existing local-control bridge

The next fixture uses the actual product DSO, its typed command, the committed
standalone SIP reactor and account cohort, and the existing radio/phone media
owners. Only physical radio callbacks and the independent loopback telephone
are simulated. It must cover refusal releasing the claim, no incoming receipt or
login prompt, called-party VOX audio reaching the local mix, known local audio
returning through the same offered RTP endpoint, no self echo or callee command
authority, exact-id hangup and a subsequent call. The account default remains a
configured but disabled different node, while explicit policy permits the
initiating enabled node. This is not hardware or public-provider acceptance.

The approved smallest operator route extends the existing0600 local-control
socket and foreground owner, without another server, thread or DTMF mapping.
`phone-dial` carries node, account and destination; the response contains only
acceptance and an opaque call_id, not answer status or copied destination data.
`phone-hangup` requires node and that exact nonzero call_id. Missing optional
product support is unavailable. Neither raw requests, dialed destinations nor
credentials are logged. Existing frame/version limits and product policy remain
in force. Disconnecting this administrative socket does not cancel a telephone
call: the existing connection-owned cleanup continues to cover only temporary
manual RF requests. Tests must distinguish explicit hangup from socket loss.
No live call is authorized by these implementation steps; target and cost
approval remain separate from source/fixture verification.


### Task 2c verification checkpoint

The real ProductRuntime wire fixture first failed at callee-to-RF audio. A focused
reset-without-digit regression then reproduced the exact missing source-ready
revision. The outgoing-only existing-owner repair passes initial/repeated reset,
newer-digit candidate invalidation and pending-cancellation checks; the incoming
explicit-readmission case remains unchanged and passes. No speech clip, age bound,
quiet-tail condition or assertion was weakened.

The final loopback outgoing fixture passes in38.70s: registration, refusal and
claim reuse, answer/ACK, callee speech through VOX to RF, local1kHz returning through
the same RTP socket, no self echo/login/control grant, exact local hangup, remote
BYE and another call. Both observed-RED local socket/typed bridge tests now pass.
The unchanged incoming explicit-attachment/VOX fixture passes in42.97s and the full
foreground registration/reload/shutdown case in20.20s. Physical radio is simulated;
this is not public-provider or hardware acceptance.

Affected checks pass:94 phone/private-owner cases,36 Host phone cases,4 outgoing
lifecycle cases,9 local-control cases and24 runtime cases. All-target strict Clippy,
warning-free rustdoc, formatting and the C ABI probe pass. The earlier broad
lifecycle run reached its180s cap after the first two completed groups; it is not
counted as a full lifecycle/full-PR pass. Focused continuation and final incoming
runs exit0. Evidence lives in .work/outgoing-source-cut-red.log,
outgoing-source-wire-green.log, outgoing-recovery-strict.log,
outgoing-recovery-strict-tail.log and outgoing-recovery-incoming.log.

This is a reviewed local recovery checkpoint. Integration with the independent
incoming repair/timing work, affected combined verification, native staging,
explicit destination/cost approval and controlled manual acceptance still precede
a real outgoing call. Linked-peer isolation, outgoing Asterisk composition and the
full PR/platform/coverage gate remain separate unfinished requirements.


## Integration with current incoming repair — 2026-11-14

The isolated combined branch starts from73b0ea0 and replays only outgoing
checkpoints027f10c,88bf26c,47a8453,2fe0620,9cceedb and43c856f. The equivalent
SIP source-gap repair is already in this base and is not applied again. The
ordinary receive-owner transfer, terminal omission and callback-Busy retry,
operation results, and captured ten-second phone digit tolerance are retained.
Outgoing calls still have media-only authority and no incoming collector.

Initial combined verification passes four core role tests and232 product phone
tests, compiles the standalone test harness and builds exact product/SIP DSOs in
an isolated provider directory. Audio and timing owners reviewed the union. The cross-branch result-audience regression reproduced and is repaired:
implicit RF operation results must select authenticated incoming recipients,
not every media-authorized attachment. Otherwise the incoming-only delivery
check correctly rejects, but then incorrectly retires, an outgoing callee.
Only the implicit result list now retains authenticated recipients. Shared
attachment enumeration remains broad for media ownership and cleanup. The new
actual-host regression and existing exact incoming/stale-recipient cases pass
in integrated-result-green.log after the recorded behavioral RED.

This checkpoint remains under integration verification. No live call, deployment,
public publication, physical acceptance or full pull-request gate is implied.
