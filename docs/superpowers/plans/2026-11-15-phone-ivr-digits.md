# Connected-call telephone digit completion

Approved scope: finish P06/P15 from verified 2155, preserving the live node.
Use the existing call/grammar/event/socket owners. No callee node authority,
new command prefix, parallel transport worker, or duplicated PCM engine.

## Source findings and intended increments

1. SIP event egress and exact serial delivery. The existing host completed-digit
   callback is IAX-only. Add negotiated RFC 4733 events on the existing RTP
   socket/SSRC/sequence owner, using its single retained datagram for backpressure.
   One event's scalar progress belongs to the call; audio-generation cuts must
   not retarget it. Call end cancels it. Use the current serial polling cadence,
   bounded work and existing private command queue, not another event queue.
   A rejected/full operation retains its original exact call and symbol.
2. Route initiating RF ordinary connected-call digits to that exact active call,
   preserving normal mapped local hangup/control and addressed out-of-band control.
   The approved phone-only grammar already defines literal ** quoting, # and
   no-egress no-echo behavior. Verify the RF/phone distinction before changing
   user-visible syntax; do not infer authorization from a media-only callee.
3. Reuse released DTMF generation and private Playback for genuinely in-band-only
   destinations. No hand-written tone generator and no duplicate media path.
4. Actual IVR wire composition, no control/result leakage, stale/reload/end/back-
   pressure tests, both codec profiles, affected suites/strict, reviewed commits.

## Verification order

- Behavioral host RED: established negotiated SIP handle refuses existing digit
  callback while IAX already has the route.
- Pure actual RTP framing/cadence tests: negotiated payload, exact event timestamp,
  increasing duration, marker only at onset, repeated final report, shared sequence,
  original audio timestamps, immutable refused datagram, bounded expiry and teardown.
- Existing-host GREEN plus raw socket event decode, no mailbox authority or self-echo.
- Owner/routing RED then minimal GREEN. Suppress audio at actual DTMF onset through
  completion using existing gates; events never reach another call or node by name.
- Full affected aggregate and static gates remain required before publication.

RFC source: https://www.rfc-editor.org/rfc/rfc4733.html (sections 2.2 and 2.5).
The telephone event duration choice remains an implementation detail of the
existing completed-digit callback, not a new user command or provider cost policy.

Linked-peer isolation is owned separately. Coordinate host/phone/settings edits
and serialize constrained test runs; do not alter the running node.

## Confirmed scope, 2026-11-15

The owner approved `**` as the active-call escape. Core completion is a single-call SIP softphone; simultaneous calls, IAX and Asterisk-host integration remain stretch work. An external SIP system can provide conferencing. Active-call RF quoting must preserve existing mapped command and hangup handling and must not grant the called party local control.

## Transport checkpoint

2026-11-15: the existing answered SIP host callback reproduced a clean behavioral refusal after normal call/account teardown. The narrow sender now passes 14 transport cases and the actual host lifecycle case (1.37 seconds; constrained launcher exit 0). Source review found no additional ownership/backpressure issue. Tests cover all 16 symbols, negotiated event payload, one socket/SSRC/sequence shared with audio, immutable refused datagrams, triple final reports, bounded expiration and source retirement. No callee authority is added.

This is only the transport checkpoint. Private-owner routing, RF grammar, in-band fallback, actual IVR composition and full affected/static gates remain unfinished.


## RF grammar and private-owner checkpoint

The actual local RF dispatcher now captures the answered, attached outgoing call
ID. Ordinary telephone digits and the approved `**` quote reach only that exact
call through the existing bounded private command ring. An automatically completed
mapped command retains its optional `#` terminator; a bare or quoted telephone
`#` stays distinct. Peer, incoming-phone, selected-remote, macro and no-call
control retain their existing authority and parsing paths.

The SIP event callback retains busy input at the existing ring head. If event
payload was not negotiated, the same private finite Playback owner uses the
released DTMF generator. Neither mode creates an RF operation result or a new
worker/PCM queue. Call retirement cancels queued control and finite playback.

Verification recorded in `.work/phone-ivr-radio-green2.log` and
`.work/phone-ivr-product-green.log`: seven radio filters, all 30 DTMF tests,
exact current-call/bounded-owner tests, both finite in-band cases, and strict
all-target Clippy for core, product and standalone passed. The initial owner RED
and the optional-command-terminator failure are retained in the earlier logs.
The actual RF-to-SIP wire fixture is saved separately and is not yet execution
qualified at this checkpoint. Full affected suites, wire/in-band composition and
aggregate union gates remain pending.
