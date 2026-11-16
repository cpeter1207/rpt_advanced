# Incoming SIP recovery after a proved adaptive source cut

Status: **proposal for independent review and root approval**, 2026-11-16.
No production implementation is authorized by this document. Root chose the
direction of safe fresh-generation recovery on genuine audio; the exact scope,
optional ABI tail and mechanism below are proposed, not yet approved.
Base: `057cd668525a38152a661d20f5d82581e067370c`.

## Intent, evidence and acceptance limit

Restore an already-authorized incoming SIP call's receive programme after a
proved ordinary native adaptation cut, without another caller digit. Retire
the old generation immediately and permanently; only a separately prepared,
current-policy-approved generation can later qualify genuine speech.

The deterministic characterization at the base commit proves:
native phone delay320 ->160 frames produces a160-frame skip; incoming control
revision2 ->cut3 preserves authentication/attachment but fresh8000-frame genuine
PCM cannot rearm; a new actual digit/release settles4 and a distinct input again
renders genuine speech. Evidence:
`.work/sdd-phone-plc-courtesy/adaptation-readmission-report.md`,
`adaptation-readmission-final.log` and `adaptation-readmission-review.diff`.
That proof composes real native/Media/owner seams, not one original failing call.

Strict composed SIP acceptance remains RED. This change cannot manufacture a
valid close for old speech cut before its hold end. If only silence follows,
fresh-generation recovery must NOT produce RF or P. Before/after recovery tests
therefore supply a later independent genuine speech burst and prove its own
held close. Run the existing strict composed test unchanged and report its
actual outcome, including a remaining RED. No retry-until-green or policy
relaxation to satisfy its P assertion.

## Approaches considered

1. **Reuse outgoing pending_release for incoming cuts.** Smallest code, rejected.
   Outgoing has no incoming command collector. Copying its release would turn a
   cause-free reset into incoming command authority and could clear suppression.
2. **One bounded adaptation recovery ticket, ordinary control approval, then
   existing graph refresh. Recommended.** Explicit cause and fresh-source
   evidence request preparation; real collector/policy checks authorize it.
   No old input revival, fabricated digit or fake CollectionEnded.
3. **Recover every cause-free cut after fresh PCM and collector idle.** Avoids
   an ABI addition, but includes unknown receipts/opaque resets and broadens the
   contract beyond the proved blocker. Not recommended without a different
   explicit scope decision.

No new PCM queue, worker, generic recovery framework, timer, warmup/washout,
jitter setting, codec, classifier, RF permission or configuration knob.

## Eligibility: deliberately narrow first increment

All of these are required:
- Configured incoming SIP, existing authenticated and media-authorized exact
  PhoneSource/PeerReply, and persistent attachment under current node policy.
  IAX, outgoing and unauthenticated admission retain existing behavior.
- The origin was an ACTIVE generation at a settled revision, with no pending
  command release and actual SourceIngress::ready_for_media() before the cut.
  An already-pending ticket may carry that same eligible origin across another
  proved adaptation cut, only if its cancellation epoch is still current.
- An ordered **proved native adaptation** interval, not inference from a
  warning, aggregate observation or positive gap alone.
- Same live owner, host clock epoch and attachment/control cancellation epoch.
  Current source/command revision must match the ticket at every handoff.
- After the cut, a positive credited original decoded PCM prefix with unchanged
  nonnull receipt, same host epoch, and born_ns >= the cut's captured owner
  reference_now_ns. Future/unknown/stale/cross-epoch metadata is not evidence.
  Native PLC, ring fallback, gap, zero extension, muted PCM and detector output
  do not satisfy this. Conservative receipt bounds may reject an offer; never
  replace them with poll/dequeue time.
- The real detector has processed that original prefix, no onset/digit changed
  revision, retained raw retry/tone tail has drained, and it is genuinely ready.
- Ordinary Engine control confirms the same current call/attachment, existing
  released command state, outer grammar idle and exact destination collector
  Some(false). Unknown/absent target or either collector pending means no grant.

“Genuine audio” here means original timed decoded PCM, not synthetic recovery.
It requests a new graph; it is NOT a positive speech or RF decision. Silence can
make preparation eligible, but only the new analyzer's normal genuine-speech
decision can open output. No new threshold or source-age exemption.

Explicitly excluded: startup, generic/source-clock reset, unknown receipt,
overflow/limit reset, malformed/provider errors, hangup, auth revocation,
detach, explicit media retirement, new DTMF/onset/command, or a merely PREPARED
generation cut while its command release is outstanding. The last case is an
explicit scope limit, not assumed fixed; widening it requires design review.

## Cause boundary: existing interface is insufficient

`rptadv_peer_source_discontinuity_v1(void *context)` has no reason or interval.
`Media::discontinuity: bool` collapses startup, unknown receipt and inferred
adaptation. The peer observation header explicitly calls its counters
non-transactional. Sampling totals around a product read cannot distinguish a
cut combined with an unknown receipt or other reset. Neither gap, activity,
finish_pending nor SourceBirth may be overloaded with hidden reason bits.

Proposed smallest published extension, subject to explicit approval:
- Append one optional `source_adaptation` callback to the existing sized
  `rptadv_peer_source_sink_v1`, after digit.
- Signature:
  `int32_t (*rptadv_peer_source_adaptation_v1)(void *context, uint64_t source_start, uint32_t count)`.
  A nonzero, nonoverflowing source-rate interval is required.
- The callback still performs the SAME hard retirement/reset before later PCM.
  Return0 completes,1 retains the identical cut for retry, negative terminates.
  It adds a cause to retirement, never permission to transmit or skip retirement.
- Check size through the exact appended field and nonnull callback. Old/short
  sinks use the existing generic discontinuity callback and receive NO recovery.
  Existing generic producers remain nonrecoverable. Unknown/malformed metadata
  fails closed. No jitter ABI change, SONAME change or required peer capability.
- Preserve ABI1 prefix offsets and introduce named current-prefix/tail min-size
  constants. Do not use new sizeof(sink) as the legacy digit-tail requirement.
  Update header consumers, C ABI probes and literal Rust sink initializers
  coherently. This optional active extension is not a compatibility algorithm.

### Native witness lifetime

In `call_media.rs`, one optional witness records the exact interval skipped by
one exclusive-owner `Jitter::advance` call. Arm only after a Packet decision
with zero prefix and zero trim has been completely credited, with its checked
packet end equal to `source_end`. Snapshot immediately before/after advance;
require a strictly positive checked `skipped_span` delta, fitting nonzero u32,
and checked interval `[source_end, source_end + delta)`. Other cumulative
counters must not change at this advance. Never derive start from a later cursor.

Compare named counters, NOT the entire Observation. `struct_size` is validated
metadata. `queued_packets`, `buffered_span` and `delay` are occupancy/delay gauges
and may change normally across put/get/advance; they provide no cause authority.
The cumulative counters are `duplicate_drops`, `late_drops`, `full_drops`,
`missing_span`, `insertion_span`, `skipped_span`, `limit_resets`, `overload_drops`
and `overload_span`. Any required observation failure, decrease, or any of these
counters equal to u64::MAX invalidates proof (including an unchanged saturated
value). All additions/subtractions are checked, not saturating or wrapping.

After the arming advance, allow only successful Accepted puts and NotDue gets
before the one successor Packet get. Compare snapshots around each operation.
All cumulative counters must remain unchanged, except `late_drops` may increase
by a checked delta during an Accepted put: native cleanup retires queued packets
at/before the already-established cursor, including the just-skipped interval.
This cleanup is NOT proof of adaptation; the original advance and exact successor
interval remain the proof. Duplicate/full/rejected puts invalidate the witness;
so do errors, explicit resets, Missing/Insertion/ambiguous or overlapping gets,
a second advance, unknown receipt or interval mismatch. Consume the witness on
the next Packet only if zero trim, known original receipt and its entire missing
prefix exactly equal the stored interval. Never suppress/delay/reorder receive
or jitter operations to preserve a witness. Startup/epoch changes stay generic.

Why puts cannot silently resynchronize within this bounded lifetime: a successful
Packet clears both wrapper `unsuccessful_gets` and native `lost_count`. Neither
advance nor puts increments them, and NotDue does not enter native get. The
first non-Packet get invalidates the witness before any later put. Therefore
neither put-side >20 reset is reachable. The wrapper disables native auto-adjust,
so remaining-span bookkeeping cannot introduce a second hidden adjustment.
This is a source-specific proof, not a general property of aggregate counters.
See [SpeexDSP 1.2.1 jitter.c](https://raw.githubusercontent.com/xiph/speexdsp/SpeexDSP-1.2.1/libspeexdsp/jitter.c)
(lines 363-368, 527-532, 581-584, 654-713) and adapter `lib.rs`
(put231-242, Packet432, non-Packet437-449, advance460-499).
The existing cohort records Debian libspeexdsp1 1.2.1-3; implementation must
recheck that identity. A different provider requires revalidation, not inference.

Task1's mandatory acceptance gate extends the actual no-gap steady-cadence trace
through Media's production packet admission and successor: the poll880 cut after
packet840 ends860 advances the next due time to900; normal packet900 put occurs
BEFORE get900 and must retain the exact adaptation cause. Include permitted
occupancy-gauge changes and skipped-packet cleanup. The existing fixture's direct
jitter injection alone is insufficient. If this proof or gate fails, stop before
owner recovery and report the smallest necessary observation-contract change;
no synthetic/buffer-only replacement fixture or relaxed safety is acceptable.

### Sized sink and private event dispatch

Both SIP `call_media.rs` and production IAX `host_services/iax_phone.rs` must
validate the old digit-tail minimum, then read fields through raw checked
prefix access (or a bounded copied value). Never form `&*sink` of the enlarged
struct over an old-sized allocation. Read the optional appended field only
when its entire field fits. Preserve the old digit offset and size constant;
IAX never emits adaptation and its behavior is unchanged. Test genuinely old
allocations, not new full structs with a falsified size.

Public `SourceInput` and `PeerIo::read_source[_at]` signatures stay unchanged.
Add crate-private `SourceDispatch<'a> { Input(SourceInput<'a>),
AdaptationCut { start: u64, count: usize } }` and
`read_source_causal_at(completion, reference, dispatch)` sharing the existing
validated read implementation. Public readers map the latter event to generic
`SourceInput::Discontinuity`; only the private phone owner consumes its cause.
Both dispatches use one shared retirement/reset implementation. No public enum
variant or downstream exhaustive-match migration is introduced.

Replace PeerIo's `source_reset_pending: bool` with one bounded
`Option<SourceReset>` where `SourceReset` is `Generic` or
`Adaptation { start: u64, count: usize }`. Establish the identity before first
dispatch; persist it across reads until completion. A retry must match the exact
cause and range, otherwise latch a source-contract failure. Later PCM/gaps get
no credit until completion; existing permitted control/text handling is unchanged.
Owner-side cut state likewise snapshots eligibility, origin epoch and cut time
on the FIRST attempt, even if retirement returns pending. Retries finish that
same operation, never resample already-retired media or advance revision twice.

## Owner/control mechanism and exact lifecycle

Use the existing serial source owner, command queue and ordinary graph-refresh
path. Add one owner-local pending recovery record and bounded shared scalar
publication, not a queue or history.

Private proposed types:
- `RecoveryTicket { revision: NonZeroU32, cancellation_epoch: NonZeroU32 }`;
  packed into one AtomicU64 for coherent publication, zero means absent.
- Extend the existing allocated source-ready shared state with that request
  word and a nonwrapping cancellation epoch. Keep ready revision separate.
- Owner-local `RecoveryCut`: ticket, original cut SourceTimeReference, exact
  source interval and a fresh-original-evidence flag. At most one.
- Each prepared/adopted media generation carries an immutable origin
  cancellation epoch captured at preparation, alongside its expected revision.
  The queued candidate, active origin and recovery ticket retain that epoch;
  no cut may substitute the latest shared epoch for the origin's epoch.
- Control-local `approved_recovery: Option<RecoveryTicket>`; never assign a
  recovery revision to `consumed_revision`, which remains real digits only.

Flow:
1. On eligible adaptation, capture origin eligibility/time, retire old input
   and egress exactly as today, advance revision/reset detector, and clear old
   ready/request. Reset-emitted real digit or any new onset cancels recovery.
2. Consume/discard pre-cut source and synthetic input through existing credit
   rules, without replay or classifier authority. Only newly credited original
   evidence meeting the above fence can publish this ticket after actual drain.
   The old media object stays retired and retains DSP until normal cleanup.
3. `PhoneCalls::reconcile_collections` notices an eligible ticket even when
   collection_dirty is false. It does NOT invent dirty digits or claim a
   collection ended. After all current collector/attachment/auth checks, call
   `PrivatePhoneControl::approve_recovery(reply, ticket) -> bool`.
   This queues `PrivateCommand::Recover(RecoveryTicket)` in the existing
   bounded command queue. Queue-full leaves one retryable request; successful
   queueing records that exact ticket so repeated reconciliation is idempotent
   rather than adding duplicate Recover commands. A changed ticket invalidates
   the old queued acknowledgement, not the normal command release.
4. Owner reads source activity before settling approval, as it does for command
   release. Require exact ticket/epoch/revision, fresh evidence, actual drain,
   unchanged authorization and ready_for_media. Recovery must NOT call
   SourceIngress::end_command or clear command_muted. It acknowledges a fresh
   media fence only; a genuine command still requires its existing release.
5. Only acknowledged approval permits media_rearm_ready/prepare_media_candidate
   to select the recovery revision rather than a consumed-digit revision.
   Reuse media_refresh_nodes -> Host::refresh_inputs -> prepare/adopt, with
   the exact private replacement boundary described below.
   Current configuration, node/RF/manual/half-duplex policy and graph publication
   checks remain authoritative; recovery cannot bypass them.
6. Preparation allocates a fresh existing unique media generation and a fresh
   analyzer off audio. Owner adoption retains normal current source_end and
   actual post-read admission reference. Eligibility PCM was already consumed;
   it is NOT replayed. Previously queued/pre-admission receipt/coordinates stay
   rejected under existing Ingress::accept rules. Further new genuine speech
   must independently qualify; old hold, VAD state, output-open and courtesy do
   not transfer.

### Host replacement is not external cancellation

`host.rs` currently calls public `PhoneCalls::retire_media` before every
`prepare_pending_media`. Leaving that call unchanged would invalidate the very
recovery approval being used. Replace this host call with crate-private
`PhoneCalls::retire_for_graph_replacement(&mut self, source: &PhoneSource,
reply: &PeerReply) -> Result<(), Error>`. It checks the exact current call,
attachment and acknowledged ticket under the current graph preparation context.
Only for that exact approved recovery does it retire/remove the old generation
without bumping the cancellation epoch or erasing that ticket. Otherwise it
performs normal external cancellation. Share the underlying generation-retire
operation; do not weaken public `retire_media` or add a public recovery API.

Reload, detach, explicit cancellation and policy invalidation must invalidate
recovery before entering graph preparation. A new configuration cannot use this
private route to preserve an old approval. Check the ticket/epoch again after
preparation and at publication/adoption; normal graph policy remains mandatory.
The ticket remains current across Busy publication or dropped unpublished
candidates; those generations are permanently retired, while a retry may
prepare a distinct candidate. Successful adoption consumes the request/approval.
A dropped candidate never restores an old generation. External cancellation
still invalidates all retries. An actual Host::refresh_inputs integration oracle
must exercise this route, including Busy/drop retry, not only direct owner setup.

Other private interfaces in `private_phone.rs`:
`recovery_request(&self) -> Option<RecoveryTicket>`,
`approve_recovery(&mut self, reply: &PeerReply, ticket: RecoveryTicket) -> bool`.
Real `consumed_revision` remains digits-only. No new public Rust recovery API,
command, configuration or user operation.

## Cancellation, races and real-time budget

- Explicit control retirement/detach invalidates the shared cancellation epoch
  BEFORE retiring a candidate; clear approved/request state. A ticket may inherit
  only the immutable epoch of its eligible active origin, and that epoch must
  equal the shared epoch. Control invalidation between an epoch load and media
  retirement therefore cannot renew old origin custody. Candidate preparation,
  enqueue, adoption and render all compare their bound epoch against current.
  Queued approvals cannot survive detach/reattach even
  to the same node. Checked exhaustion fails closed, never wraps.
- Hangup/auth/account/call replacement retain existing typed reply fences.
  Recheck at request, approval, candidate preparation, adoption and render.
  Runtime reload/current policy is revalidated by normal graph preparation/
  publication; no cached old policy grant crosses a publication revision.
- Any digit/onset/new command or generic cut cancels the pending recovery and
  ready request immediately. Late CollectionEnded(old revision) cannot authorize
  it. A real new command/release can still follow the normal route.
- Repeated proved adaptation under the same eligible origin replaces the ONE
  ticket with the new revision/cut time, retires any stale prepared candidate
  and invalidates queued old approvals. A synthetic-only interval never makes
  a successor ticket ready. Once a new generation is active, it is a new origin.
- Candidate cancellation/drop, Busy graph publication, changed attachment and
  preparation errors use existing retention/retirement behavior. Busy retries
  preserve a still-current ticket only; errors never resurrect old input.
- Coherent ticket snapshot plus checked cancellation epoch prevents ABA/mixed
  revision approval. Serial owner is the only source-state writer; control is
  the only approval/cancellation writer.
- Setup-only shared storage; fixed scalar owner record and one command variant.
  No extra PCM, history, audio queues, workers or timers. No allocation/free,
  locks, logging, I/O or lazy preparation in source/render callbacks. Measure
  exact state-size delta and guard callback allocations/frees in focused tests.
  Ordinary graph preparation may allocate where it already does.

## Required tests and review decisions

Preserve existing source-cut/cancel, signed-offset, synthetic-authority,
source-age, authentication, outgoing/IAX and exact-input courtesy tests.
Before/after: actual proved native skip, active authenticated incoming SIP,
post-cut independent original speech, actual control idle reconciliation,
distinct input, genuine output and its own valid held/drained close; old input
and old tone remain retired. On base057cd668 the fresh-audio/no-digit expectation
must fail at readiness/admission, not at a fixture/compile error.

Negative matrix: legacy/unknown cut; unproved gap; startup/prepared cut;
synthetic-only and silence-only; before-cut/future/unknown/cross-epoch receipt;
partial/zero credit; detector onset/pending raw tail; pending/unknown collectors;
stale command release/approval; full command queue; repeat cuts; detach/reattach,
candidate cancellation, auth/call/account/config replacement;
control invalidation paused before shared-media retirement; cancellation after
approval before adoption; actual Host refresh Busy/drop retry; exact repeated
reset identity and generic-to-adaptation substitution; put/ambiguity after a
native witness; actual old-size SIP and IAX sink allocations; revision/epoch
exhaustion; explicit retirement; IAX and outgoing unchanged.

Approval gates:
1. Is active-origin-only adaptation recovery the intended first increment?
2. Approve the optional sized source-adaptation callback as necessary for this
   narrow cause scope, or explicitly choose a broader different contract.
3. Independently review the ticket/collector/admission fences before code.

No claim that this resolves the original clean failing-call chronology.
Full native amd64/ARM64, package/staged-install, production coverage, hosted
gates and hardware acceptance remain separate. No node, RF, account, settings,
push, release or deployment is authorized here.
