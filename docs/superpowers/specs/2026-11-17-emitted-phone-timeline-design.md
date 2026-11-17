# Emitted phone-media timeline design (review draft)

## Intent and scope

A verified benign jitter delay reduction must remain inside the media layer.
It must not reset the authorized source, cancel a valid courtesy opportunity,
renew speech, or undo its own latency reduction. Original receipt age, command
suppression, source replacement and retirement authority remain unchanged.
The shared courtesy planner separately requires uninterrupted quiet across all
selected authorized sources (commit 3d493d3). This design does not change that
planner or the original positive SIP courtesy assertion.

This is a proposal, not implemented production behavior. No live node, deployment,
release, main-branch or APT change is included. Actual packet-loss parity with IAX
remains an explicit additional qualification/provider-contract scope below.

## Evidence and approaches

The RTP adapter currently uses extended wire-frame coordinates at the host sink.
A native verified 80-frame skip inside a 160-frame packet produces a coordinate
hole and a generic discontinuity. IAX's pinned ce9fe2da timed API instead emits
contiguous playout-frame coordinates and decodes the whole packet before trimming.

Rejected: weaken discontinuity/receipt checks or treat every jump as packet loss.
That would admit unproved, stale or unauthorized media. Also rejected: call PLC
for 80 frames then decode the whole 160-frame packet, advancing codec chronology
twice. The test-only counted-gap spike decodes correctly and preserves authority,
but fails the latency objective: the actual product ring adds 160 timeline frames
for 80 gap + 80 PCM, exactly as for 160 PCM. Across 120 iterations these accept
19,200 frames; contiguous 80 PCM accepts 9,600. This is seam evidence, not a full
native bridge proof. Tests/logs remain available as negative evidence.

Recommended: distinguish native/wire chronology from emitted host coordinates.
This matches the existing IAX trim model without adding transport policy to the
controller or teaching the programme source about a benign adjustment.

## RTP adapter changes

1. Keep extended RTP/wire cursor, packet receipt lookup, native counter witness
   and jitter accounting in wire coordinates. Do not redefine native counters.
2. Keep a separate emitted cursor for source-sink callbacks. Initialize it once
   per Media lifetime from the first admitted decision. Checked arithmetic and the
   existing generic-cut authority govern overflow or unprovable transitions.
3. For the initially supported benign case require the existing exclusive native
   advance witness, unchanged unrelated counters, exact next packet/offset/span,
   known original receipt, prior admitted history, and an in-packet negative
   offset matching the entire skip. No unknown prefix, full-packet skip, epoch
   change, invalid packet or mixed unexplained movement qualifies.
4. Retain the qualified decision with its wire start, emitted start, decoded
   suffix and classification. Advance wire chronology over the proven omitted
   prefix without emitting gap/PCM, calling PLC, or changing the host cursor.
   Decode the full compressed packet once and offer only its retained suffix at
   contiguous emitted coordinates. Do not use a mutable global qualification flag.
5. Zero/partial sink credit retains the same decoded decision. Advance emitted
   cursor and credited wire suffix only by accepted frames; do not repeat a trim,
   decode, packet get or exclusive advance on retry. Commit native advancement
   only once the retained decision is complete.
6. Genuine counted loss still advances both timelines by actual accepted loss
   duration. Existing original/concealed provenance and inherited-age rules remain.
   Native insertion still moves neither source timeline. Unproved movement takes
   the existing hard-cut path, never the benign shortcut.
7. Original suffix PCM keeps its packet's immutable receipt, not current time or
   a receipt manufactured from emitted coordinates. Concealed PCM retains prior
   provenance. Neither trimming nor concealment creates VAD/DTMF input or renews
   original speech hold. Detector input for the retained original suffix remains
   governed by existing source rules; no explicit source reset is injected.

All command lookback/mute coordinates and media provenance must use the same
emitted host axis after the boundary. No wire-coordinate adaptation-cut payload
may accidentally be forwarded as an emitted-coordinate span. Existing full-skip
adaptation recovery remains fail-closed unless its mapping is separately proven.

## Subsequent cuts, replacement and command coordinates

The emitted cursor never rewinds or rebases within an existing Media instance,
including after a later hard cut. The cut callback must be accepted before any
next media callback; refusal retains both coordinates and the same cause. A
hard-cut decision still uses the existing gap/retirement semantics, but every
sink span start is translated to the retained emitted cursor, never copied from
a wire timestamp. Full-packet skips do not acquire the new benign classification;
existing typed adaptation recovery, when provable, keeps its counted interval
and authority, translating its start to the emitted axis. Unproved skips remain
generic cuts. Counter overflow is terminal rather than a cursor rebase.

A host clock-epoch change already fails SourceContract and terminates the media
instance; do not reset its cursor and relabel retained receipts under a new epoch.
A new call/media instance may initialize a new coordinate origin only under its
new generation and original admission rules. Replacement/retirement invalidates
old retained output; no buffered suffix or loss is donated to the replacement.
Partial credit followed by an external retirement is not permission to bypass
sink authorization or reuse the old receipt as fresh history.

RFC4733 input currently exposes only symbol/ended to call_media. Pending::Digit
is symbol-only; the source sink does not receive event timestamp/duration. Thus
no historical telephone-event translation ledger is needed or proposed. Keep
current duplicate/end handling and exactly-once symbol credit. In-band detector
lookback and mute spans originate in host source coordinates and must remain on
the emitted axis through retained PCM, gaps, cuts and command suppression.

Additional mandatory sequence tests: trim -> partial suffix credit -> generic
cut or typed full-skip cut -> next packet; trim -> partial credit -> detach or
replacement -> old retry; host epoch change with retained suffix; delayed first
end, duplicate end and refused symbol before/after one or multiple trims; in-band
digit/lookback spanning retained original PCM across trims. Assert exact emitted
ranges, no wire-coordinate sink payload, no stale receipt relabeling, unchanged
command retirement, and no resurrection. Do not invent event-range behavior that
the present symbol-only API does not provide.

## Transport-independent contract and IAX parity

The common host policy should depend on proven media provenance, not a protocol
name. The existing source sink already separates original/concealed PCM and
counted gaps, but the IAX timed event currently cannot prove all those distinctions.
Do not remove the SIP-only continuation guard before the IAX path can supply the
same evidence. Audit the exact loaded ELF/package/source chain; a matching version
string is not source attestation.

For benign IAX trims, qualify existing emitted-cursor behavior through its native
timed API and actual product path; do not insert a gap that it does not currently
emit. For real loss, a versioned provider event extension or equivalent explicit
capability must identify original versus concealed spans with immutable/inherited
birth and known epoch. The smallest ABI-compatible extension must be reviewed
against the exact pinned provider; no guessed flag on an unproved legacy event.
Unknown legacy input remains fail-closed. Ordinary linked-peer and private-phone
paths both need review; shared courtesy planning alone does not prove media parity.

## Verification before production or publication

- Native deterministic negative-offset witness with stateful compressed codec:
  decode once, exact suffix, stable epoch, no source discontinuity or PLC call.
- Separate native wire progress and emitted ranges; positive/negative offsets,
  wrap/overflow, full skip and missing/mutated counter/receipt/epoch cases.
- Zero and split credit at every retained phase; no duplicate samples, skipped
  authority check, premature native advancement or receipt refresh.
- Connect repeated actual native trims to product ingress and paced ring output:
  accepted duration shrinks by the exact omitted total, bounded queue/age, stable
  generation, unchanged hold, no fabricated speech; include stalled consumer.
- Authorization loss, DTMF mute, detach/replacement, timeout and retirement during
  retained output cannot resurrect old media or downlink authority.
- Original real SIP positive-P test and resumed/unrelated-audio cancellation remain.
  No assertion movement, timing relaxation or retry-only acceptance.
- Equivalent IAX trim tests, and loss tests only after provenance is established.
- Fresh quality image, warnings denied, targeted regressions, complete pre-push
  lint/static analysis; independent review, then exact-source amd64/ARM64 hosted
  qualification. Full PR/coverage and physical listening remain distinct gates.

## Proposed execution order and estimate

First independently review this design and resolve coordinate/cut contract issues.
Then implement the RTP mapping with red/green native and product tests, keeping IAX
provider extension separate until its evidence contract is reviewed. Initial RTP
implementation and local integration estimate: 2–4 hours after design approval;
review plus hosted two-architecture rerun: another 45–90 minutes if no new failures.
IAX real-loss parity is not included in that estimate; scope follows provider audit.
These are engineering estimates, not a deployment commitment.
