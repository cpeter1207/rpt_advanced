# Radio telephone acknowledgment and ringing repair

**Goal:** Render the requested LOCAL command acknowledgment before outgoing dialing starts, then provide ringing feedback only from actual provider progress. Answer must not repeat the acknowledgment.

**Architecture:** Keep the exact existing outgoing claim and lifecycle job. Observe completion of the finite status through its existing playback/reclamation owner; do not sleep for an estimated speech duration. Preserve cancellation/reload and source-only routing. Relay actual provisional state from the existing SIP slot; no second media queue or reactor.

**Source:** Owner report on 2026-11-15, after live 5bc: acknowledgment occurs after answer and ringing is silent. The separately assigned answer-media repair must connect callee media without local RX; this branch does not own that correction.

## Evidence and scope

Current lifecycle.rs calls dial_phone before radio_phone_result and reports Completed only after adoption. Native SIP records180/183, but the existing synchronous established-only host factory cannot expose provisional state to product. Existing current/peer_observe contracts cannot be repurposed to report SIP progress for a handle which does not yet exist.

## Sequence

- [x] Behavioral RED: actual RF request must emit its finite acknowledgment before the factory is entered; no duplicate Completed on answer. Retain administrative/macro silence and current refusal/cancellation/revision checks.
- [x] Reuse prepared status and its existing completed-media return path for a scalar completion receipt. Failed/canceled/reloaded acknowledgment cannot launch a stale call. No PCM copy or guessed timing.
- [x] Focused GREEN and independent review; checkpoint acknowledgment separately.
- [x] Prove actual 180 progress reaches a bounded LOCAL tone source and stops on answer/failure/cancel/reload. Check complete existing interfaces before selecting a nullable sized progress extension; preserve old complete-prefix rejection rules and both hosts.
- [x] Actual wire sequence: acknowledgment finishes before INVITE,180 produces ringback before200, answer ends ringback and does not repeat acknowledgment. Wrong/stale progress cannot affect a successor.
- [x] Affected full suites, strict checks and independent review. Qualified source is preserved in a verified local bundle. No live/config/provider changes or public publication.

## Review focus

A queued notification is not audible completion. Completion here describes finite controller playback/reclamation, not a physical DAC/RF delivery guarantee. A canceled acknowledgment must not leave a worker waiting indefinitely. Ringback cannot precede actual ringing evidence, overlap accepted call media after answer, or enter network/private control output. Incoming explicit attachment and callee no-login authority remain unchanged.

## Acknowledgment recovery checkpoint

Exact5bc baseline reproduced missing pre-dial acknowledgment in `radio-feedback-red.log`.
`radio-feedback-green-progress-red2.log` then passed all three finite-completion tests and the nine-mode real Engine matrix (39.46s): answer, refusal, spawn failure, canceled successor, independent revision change, cancellation/reload before playback, administrative and macro silence. The existing five-second factory deadline remains asserted. Only the obsolete second pre-answer RF drain was removed; the actual finite drain, exact catalog-count and no-duplicate-after-answer checks remain.

Independent source review cleared the completion ownership and lifecycle delta. This checkpoint is not the complete ringing fix or full regression qualification; affected strict/full and actual wire sequencing remain pending. The later test in that same log is the intentional actual180 progress RED (0 versus180), separate from this checkpoint.

## Ringing recovery checkpoint

`radio-ringback-red2.log` proves the actual RF callback/decoder path completed
its acknowledgment before INVITE, then failed at the missing LOCAL audio after
real180. `radio-ringback-green.log` passes that same composed test in47.99s,
including stopped ringback after200, no duplicate acknowledgment, unchanged
two-way audio/private-digit/hangup checks, and the C ABI probe. It also passes the
real reactor/host provisional-state and early-refusal-zero test, complete-prefix
and all partial-tail checks, delayed progress against canceled/reloaded claims,
and four finite receipt/cancellation cases. Runner exit0.

The wire fixture moves its existing10s finite-result bound from after answer to
the pre-INVITE wait; ordinary SIP receive bounds remain4s. Its acknowledgment
counter baseline is captured after RF digits, so those digits cannot satisfy the
finite acknowledgment assertion. No production timeout or speech duration changed.
Independent final source review is clear. This is a recovery checkpoint; full
strict/affected suite and the separately assigned continuous-RTP union remain
pending. No native or live activation is implied.


## Aggregate fixture reconciliation

The first full run passed 807 core unit tests and 94 integration tests. Product
finished with 863 passing tests, one existing ignored case, and two primary test
failures followed by three poisoned-lock failures. The old RF admission fixture
waited for factory entry without driving the newly required finite acknowledgment;
it now asserts the factory is still unentered and renders through the existing
shared receive/transmit probe before its unchanged admission and provider deadlines.
The exhaustive host-prefix test omitted the prior complete 432-byte table; only
that exact boundary was added, preserving rejection of every partial tail.
Independent source review cleared both test-only corrections. All five affected
checks then passed in `radio-feedback-strict.log`, followed by the Asterisk, peer
support and SIP suites. Clippy stopped on the new completion envelope reaching
128 bytes; an allocation-free layout repair and remaining strict checks are pending.
This does not claim an aggregate pass yet.


## Compact completion envelope

The focused layout RED measured a 128-byte MessageCompletion after adding the
finite receipt. The existing optional recipe identity now stores checked index+1
as NonZeroUsize, reducing that identity to one word without a box or new allocation.
Only equality uses the encoded identity; the producer's MessageRequest retains
the original index. The impossible Vec index usize::MAX fails before port/ring
allocation. The regression checks compact size, unchanged requests for zero and
usize::MAX-1, and refusal without adding a port.

`radio-feedback-layout-green.log` passes all 808 core tests, all-target Clippy for
core/product/standalone/Asterisk/peer-support/SIP, warning-fatal Rustdoc and fmt.
Runner exit 0, with independent exact-source review clear. Aggregate product and
standalone verification of the final continuous-media union remain pending.


## Combined continuous-media recovery

The selected union applies the clean net continuous-media leaf c4733fad (parent
5bc2526) on feedback checkpoint 766b5bf. It excludes every startup-only transport
change. The only merge conflicts were this boundary's ADR and the shared outgoing
wire fixture: both pre-INVITE completion/actual180 ringback and immediate no-RX
first greetings remain, while obsolete post-answer completion waits are removed.
Independent source overlap review is clear.

`radio-feedback-continuous-outgoing.log` rebuilt the exact private product and SIP
libraries, passed the C ABI probe and all six actual-product outgoing cases in
311.62 seconds, launcher exit0. Coverage includes administrative and real RF DTMF
origination, both immediate-answer greetings, both negotiated codecs through
30-second strict idle cadence, accepted source-cut recovery, no callee control or
self-echo, actual RF return and exact local/remote hangup. The separate full
core/product aggregate and standalone-base/host strict groups remain pending at
this recovery checkpoint. No native or live qualification is implied.


## Final combined development qualification

The exact combined production tree from ab85356 passed all six outgoing
actual-product compositions in 311.62 seconds. The same tree then passed 809
core unit tests and 94 core integration tests, and 871 product unit tests plus
five product integrations. The existing `egress_full_path_load` experiment
remains intentionally ignored. The complete aggregate log reaches both final
successful doc-test summaries; its launcher exit receipt was lost when the
execution session expired across context recovery, so no numeric launcher
result is inferred for that group.

The remaining combined group records launcher exit 0: 318 standalone library
tests (the six already-qualified outgoing cases excluded), two binary tests,
30 integrations and one doctest; Asterisk 66 tests; peer-support 13 unit and
82 integration tests; SIP adapter 13 tests. It includes the unchanged incoming
phone-origin and existing-ordinary-link compositions. The freshly rebuilt C ABI
probe, all-target Clippy with warnings denied, warning-fatal Rustdoc, and
formatting pass for all six affected packages. The recorded product library
SHA256 is 8136fb8c932aaf931aaa9abfe7e9c432ce5bd782c012b614faabd49c1db6a715;
the unchanged SIP library is
b3d352592388e358d5d95f7a727c395f0fd9d1592f36fb219f221fa2e1e989f3.

Evidence: `.work/radio-feedback-continuous-outgoing.log`,
`.work/radio-feedback-continuous-full.log`, and
`.work/radio-feedback-continuous-host-strict.log`. Every run used the freshly
pulled constrained quality image digest
`sha256:4e62696d8e6809b7b716c3c81d70b59ed9ab5b109a7539dc97c855dc7d41e844`,
private targets/providers, and a 595-second outer runner cap. The native
handoff preserves the separately qualified 90-second per-process watchdog for
each isolated composed case; no fixture deadline or speech clip was extended.

Independent overlap review confirms that the six continuous-media production
and support files match c4733fad exactly. All startup-only transport state is
excluded. The optional HostServices10 progress tail is 440 bytes; the prior
complete 432-byte prefix remains accepted and partial tails rejected. Product
and host must be rebuilt together. No SIP/provider ABI or dependency change is
introduced. Asterisk still exposes no outgoing-phone factory.

This is an affected-development qualification, not the full pull-request gate
or physical radio/provider validation. Native arm64 build and composed checks,
parent-selected deployment, and manual acknowledgment/ringing/first-hello
acceptance remain. Full production coverage, packaging/staged-install and
platform-independent PR checks remain separate. Ringback reflects actual SIP
180; 183 early media is not implemented. Finite completion certifies controller
playback/reclamation, not physical RF delivery. No live node/config change or
public push was performed by this work.
