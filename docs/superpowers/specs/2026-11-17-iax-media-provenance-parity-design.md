# IAX media provenance and parity: review draft

Status: proposal only, not approved implementation or qualified transport parity.
No production edits, builds, publication, deployment, or live-node changes are
part of this draft. IAX parity is required work, separate from the RTP change.

## Requirements and reviewed dependencies

Chris requires consistent jitter/loss/courtesy behavior across IAX and RTP:
proved benign adaptation should remain inside media delivery; genuine resumed
audio during a courtesy delay cancels the waiting tone. Concealment must never
manufacture speech authority or renew its original hold.

Read alongside [emitted phone timeline design](2026-11-17-emitted-phone-timeline-design.md)
and shared-core commit `3d493d3138f8812677b3b072fdc1a157494be01c`.
That commit cancels pending courtesy after release scheduling whenever selected,
authorized receive activity remains audible, before checking its deadline.
Held programme/PLC may keep receive busy without creating speech or renewing
hold. It does not indiscriminately cancel already audible courtesy media.
A shared planner proves neither provider provenance nor ordinary/private parity.
ADR 0040 forbids compatibility-only shims for obsolete alpha artifacts; update
current consumers and package/version checks together, rejecting mixed cohorts.
ADR 0044's bounded adaptation authority remains a constraint, not permission to
classify arbitrary holes or epoch changes as benign.

## Audited baseline

Exact IAX manifest pin: `ce9fe2da0d8480c1503f5eb80c3c95d8ac2e5360`.
References in this section are to that revision, not checkout HEAD.

- `include/rptadv_iax2_client.h:122-150`: timed event v1 is 72 bytes on native
  64-bit targets. Its fields include host clock epoch/reference, acknowledged
  media epoch, event kind/flags/length, media epoch, source start and born time.
  V2 is 80 bytes, appending request flags and reserved word. V2 also permanently
  activates private PCMU/PCMA 8 kHz/20 ms semantics; it is not an ordinary-peer
  provenance capability.
- `src/ffi.rs:938-1000`: existing `poll_timed` reads the physical size first,
  validates version and optional tail before peer mutation, then selects ordinary
  `poll_timed` or private `poll_timed_private`. Unsupported versions return -1.
  There is no independent capability query in this interface.
- `src/playout.rs:354-419`: negative offset skips decoded prefix only after
  decoding the whole packet. `src/client_timed.rs:174-193` advances the exported
  playout cursor only by emitted audio/gap count. It is not wire timestamp space.
  Therefore a benign IAX trim already compresses the host timeline. Do not add
  padding that undoes the trim or call PLC over the same decoded prefix.
- `Cargo.toml:15`: codec client currently pins `becbff939a0817cc91df6dcb0d40e85e381ec87b`.
  Its recovering decode count is insufficient to prove mixed-unit provenance.

## Proposed smallest explicit contract

Extend the existing poll slot with a new explicitly versioned, caller-sized
record. Proposed v3 layout starts with the existing 72-byte timing prefix and
adds fixed-width request-profile, request-flags, output-origin and reserved
words (88 bytes on the supported native 64-bit platforms; verify C/Rust layouts
with compile-time tests before adopting this number). Do not reinterpret v2's
private activation or overload TIMED_BIRTH to mean native concealment.

Request profile explicitly selects a permanent per-peer ordinary or private
profile, not a mode that may change on every poll. Validate and latch the profile
on the first successful activation; no subsequent profile change is allowed.
In particular, private -> ordinary must reject before mutation and must never
restore generic codecs or visible TEXT. Ordinary -> private after ordinary
activation also rejects; a new peer is required for a different profile.
Deliver/service-only is a per-call choice within the latched private profile,
not a profile transition. Service-only is invalid for the ordinary profile.

This strengthens the explicit contract around the existing permanent latch:
`src/client_service.rs:121-157` atomically validates/activates the private profile;
`src/client_timed.rs:65-87` prohibits alternating timed/untimed polling and keeps
private activation persistent; the v2 header explicitly says a v1 request cannot
restore generic/visible-TEXT semantics. New v3 activation must inspect any
already-active legacy profile before mutation. Conflicting legacy calls after
v3 activation reject before consumption or mutation. Do not silently reinterpret
v1 as ordinary after private activation, or use v2 to change an ordinary v3 peer.
If an older entry point is still needed by a current consumer, its interaction
with the profile latch must be explicit and tested; otherwise remove it under
ADR 0040. Unknown versions,
short records, unsupported profiles, unknown flags and nonzero reserved words
must reject before polling, decoder advancement or profile mutation. Read only
validated extents; preserve caller tails. Current ordinary/private requirements
must remain expressible without retaining obsolete compatibility-only paths.

Version acceptance is a strict required-capability handshake, not an automatic
fallback strategy. A version rejection from the old provider fails closed;
never retry an operational error as a legacy poll after possible consumption.
Update current consumers, headers, artifact checks and package dependencies
as one cohort. Retaining old versions is justified only for current required
consumers, not historical alpha compatibility. Final descriptor/SONAME metadata
choice belongs in implementation review; the old function pointer shape alone
is not adequate capability evidence.

Each successful media event describes one homogeneous provenance segment:
original PCM, concealed PCM, or known counted loss. Unknown cuts remain the
existing discontinuity/epoch handshake, not a fourth usable-media origin.
Event kind/origin combinations are validated. Original receipt remains immutable;
concealed/loss age derives conservatively from its qualified predecessor, never
poll time. Missing/future/underflowed birth or an unproved predecessor cannot
acquire continuity authority. Epoch acknowledgement gates new output exactly as
before. No packet-to-host coordinate assumption is introduced.

### Two distinct delivery boundaries

Provider -> consumer delivery is committed when poll returns its media event.
The current provider increments its cursor by returned Audio/Gap count in
`src/client_timed.rs:174-201`; it receives no downstream sink acceptance count.
Do not introduce a fictitious sink acknowledgement into that API. Provider
retained bounded decode spans split a mixed-origin packet into homogeneous poll
results, further limited by caller output capacity. Advance the provider's
retained decoded position and emitted cursor by the returned segment length,
exactly once. Undelivered provider suffixes keep the same decoded storage,
provenance and original/inherited birth. Output-capacity refusal publishes no
segment or cursor credit. A subsequent provider poll continues the retained
suffix without decoding that packet again.

Consumer -> sink acceptance is a separate boundary. After receiving a complete
provider event, the adapter owns its copied PCM/event metadata until the sink
accepts it or retirement invalidates it. Zero/partial sink credit changes only
the adapter's retained offset, never the already-committed provider cursor.
Offer the same immutable event at source_start + accepted offset and retain its
birth/origin. No new delivery poll may overwrite that retained PCM. The ordinary
consumer needs the same ownership discipline; it must not acquire private
service semantics just to make progress while backpressured.

Private service-only polling remains permitted while downstream PCM is retained:
`iax_phone.rs:292-327` currently calls service_source instead of poll_source in
that state. The provider must not deliver media, advance its media cursor or
consume a retained decode suffix merely to service protocol. Actual expiry,
clock revocation or terminal failure can retire media even during service-only
(`client_timed.rs:133-172`). On a returned retirement target, the adapter discards
old retained PCM/gap, completes the actual sink discontinuity/owner retirement,
and only then acknowledges that exact target. Preserve an unaccepted private
control symbol across retirement as the current adapter does; do not replay old
PCM or acknowledge retirement just because a service call returned. Deliver may
resume only after retained ownership and the retirement fence are resolved.
No receipt is refreshed at either boundary. Terminal errors revoke retained
media; replacement never inherits its authority.

## Codec API path

Reviewed codec candidate `8596809c3ff8493d705bc2f65371e9f0b6e4fef2`,
`rust/client/src/lib.rs:563-634`, provides actual APIs
`Decoder::require_provenance()` and
`Decoder::decode_with_provenance(packet, pcm, &mut [DecodeSpan])`.
They validate exhaustive caller-owned spans without allocation or automatic
recovering decode, reset, conceal, or a second decode. Upgrade IAX's dependency
to the qualified provenance cohort, require support before admitting media, and
retain span boundaries through trimming. Preserve native concealment priority;
only genuinely missing output reaches counted ring fallback, never PCM already
recovered by a codec. Do not pretend the existing count-only API proves origin.

## Both product consumers

Private: `rust/standalone/src/host_services/iax_phone.rs:330-377` currently sends
known-birth audio with concealed=0 and turns ambiguous audio into counted loss.
Replace inference with validated explicit origin, preserving authorization,
exact epoch acknowledgements, command fences and immutable receipts. Unknown
input remains fail-closed. Only after this evidence is available may
`rust/product/src/link/private_phone.rs:700-718` replace SIP-name eligibility
with common provenance-based continuation eligibility.

Ordinary: `rust/product/src/link/session.rs:873-900` already accepts audio with
concealed count and counted gaps through SourceInput. Wire provider origin into
the ordinary IAX consumer too, rather than changing only iax_phone. Verify
ordinary linked/receiving activity and ring/tail behavior independently of
private-phone phases. Native or ring PLC must not become original detector or
DTMF evidence. Existing command-driven discontinuity/mute rules remain real
fences even when an authorized adaptive omission is transparent.

Courtesy uses shared-core 3d493d3's uninterrupted quiet rule in both paths.
New genuine audio during the wait invalidates the waiting opportunity; synthetic
continuation may remain audibly busy only within existing age/hold bounds.
An unavailable/retired source cannot synthesize a legitimate close or tone.
Keep ordinary and private positive-tone and resumed-audio cancellation tests.

## Required qualification

1. C/Rust sizes and offsets, minimum and oversized records, protected tails,
   invalid flags/profile/version, rejection before any peer mutation, and
   unsupported-provider fail-closed admission. Test first activation, repeated
   same-profile requests, ordinary -> private and private -> ordinary rejection,
   conflicting v1/v2/untimed entry calls, and private deliver -> service-only ->
   deliver. Assert no rejected request mutates profile, consumes media or restores
   TEXT; ordinary requests never activate private restrictions.
2. Mixed original/recovered units, native PLC versus ring fallback exactly once,
   full decode then partial trim, bounded spans/capacity, zero/split sink credit,
   no duplicate decode/output or time refresh, terminal decode errors. Separately
   assert provider-return credit/cursor across mixed-origin capacity-split events
   and adapter sink-credit offsets across zero/partial acceptance. While an
   adapter retains PCM, service-only neither overwrites PCM nor releases the
   provider suffix. Exercise service-only retirement with retained PCM and with
   an unaccepted digit, exact downstream retirement acknowledgement, then delivery
   resumption; prove stale PCM cannot reappear.
3. Original/inherited receipt age, expiry, missing/future/underflowed times,
   host epoch change, exact retirement acknowledgement, auth loss, detach,
   replacement and retries cannot resurrect retained media or RF authority.
4. In-band commands spanning trims, synthetic spans cannot produce commands,
   private digits retain existing symbol-only semantics, muted/lookback ranges
   use emitted coordinates. Commands must still retire or suppress as required.
5. Real native repeated IAX trims through timed API and product ring with paced
   and stalled consumers: exact omitted duration, bounded occupancy/age, stable
   epoch and no fabricated gap/PLC/reset. Include stateful compressed ordinary
   profiles as well as private G.711. Existing hand-authored offset tests alone
   do not establish native end-to-end parity.
6. Positive courtesy after genuine uninterrupted quiet and cancellation on
   resumed/unrelated selected audio, both ordinary and private. Preserve RTP
   positive assertions. Do not qualify intermittent failures by retry alone.
7. Warnings denied, targeted regressions and required pre-push static checks;
   independent review, exact-source native Debian 13 amd64/arm64 qualification,
   full PR gates including production coverage and package/staged-install checks.
   Hosted ARM execution is not physical station/listening acceptance.

## Exact-source evidence and remaining attestation gap

The inspected candidate quality image manifest is
`sha256:c3800ed0094d0d01284b70481d3189a76044042b2900594115db2e2a41f2d0fa`.
Its IAX ELF is `/usr/lib/x86_64-linux-gnu/librptadviax2.so.2.0.1.0-alpha13`,
SHA256 `509715c27967bf00a966d58c7c11cd4f48925658ee0072b33eb67e5371d04231`,
owned by `librptadv-iax2-client2:amd64` version `0.1.0~alpha13`.
Package verification was clean; retained process maps showed that same path.
Neither version nor package integrity cryptographically ties the ELF to the
manifest source pin. Packaged docs contained no exact source/build attestation.
Obtain trusted build provenance or qualify an isolated exact-source cohort and
record per-architecture source pins, build inputs, artifact hashes and runtime
loaded hashes. Do not reuse the amd64 hash as ARM evidence. No release/APT
publication or live-node operation is authorized by this draft.

## Estimate and review decisions

Estimate after contract approval: 2-3 focused engineering days for provider,
codec upgrade, both consumers and regressions, then 0.5-1 day for independent
review and matching native architecture qualification if no new failures.
This is additional to RTP work; full PR remediation and physical acceptance
may add time. No transport-parity or deployment completion is claimed.

Material review decisions:
- Approve the explicit ordinary/private v3 profile contract and strict required
  capability admission rather than any automatic legacy downgrade. Confirm
  which current consumers still require v1/v2 before removing obsolete paths.
- Confirm codec provenance support across the currently supported ordinary IAX
  profiles is a required admission prerequisite; unsupported combinations must
  fail closed, not silently lose existing codec support or fake provenance.
- Approve exact-source isolated provider-cohort qualification as the way forward
  if published alpha13 source attestation cannot be recovered. Release and node
  deployment remain separate decisions.
