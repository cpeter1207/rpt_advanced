# PCMA quiet-call rearm boundary

This test-only increment follows immutable Batch A0d0e126, runtimeed73d27.
Production source and the qualified private product/SIP provider bytes are unchanged.

## Reproduced failure

Frozen partition1 passed five outgoing selectors, then the PCMA quiet selector
failed its continuity guard. A single diagnostic run passed. A subsequent two-
process unchanged-guard run passed once and reproduced the failure on phase2,
packet1: consecutive sequence and unchanged SSRC, marker set, timestamp delta472
instead of160, fresh accepted-cut count21 versus previously observed20, and no
skipped synthetic input slots. This was before that packet's PCM decode.
The later503 was teardown after assertion, not evidence of an earlier SIP failure.
The diagnostic log SHA256 is
`14ffe30e5ed79ecca33a747f692f51b9c79562ae0c3178558c67030b80f8688b`.

## Corrected test boundary

Elapsed500ms is not completion of a native-control rearm. Continue paced input
and validate every transition packet under the original marker, forward-time,
sequence, SSRC and accepted-cut-credit conditions. Within a fixed3s deadline,
observe one packet after exactly six native digit credits and after all current
cut credit has reached the wire. Preserve that RTP history, then require the
following100 packets to be strictly continuous. No fresh marker is demanded if
the previous media turn already consumed the cut; no packet is discarded or
normalized. Initial1500-packet no-input silence and zeroRF/key assertions remain.

The extracted assertion helper has a negative control: a fresh credited cut is
still forbidden during the strict interval. It also rejects uncredited markers,
missing markers and nonadvancing source time. No production change is justified
by the original boundary failure.

## Qualification

Independent exact-diff review is clear. pcma-boundary.log completedRUN_EXIT0:
helper1 and two actual full PCMA processes passed70.70s and70.45s. Existing per-
case90s and outer240s bounds were unchanged. These checks support this fixture
repair; they are not all-outgoing replay, native/hardware or coverage qualification.
Original failed and non-reproducing receipts remain preserved in ignored.work.
