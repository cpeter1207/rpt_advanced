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
- [ ] Affected full suites, strict checks, independent review and verified bundle. No live/config/provider changes or public publication.

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
