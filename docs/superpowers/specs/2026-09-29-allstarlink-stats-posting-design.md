# AllStarLink status posting design

## Goal

Allow each `rpt_advanced` node to publish its live status to the configured
AllStarLink statistics service. Match the existing ASL3 `statpost_url` contract
so the service can treat an `rpt_advanced` node like an ASL3 node.

## Configuration

- `statpost_url` is accepted in `[general]` as the shared default and in a
  `[node-id]` section as a node-specific override. No URL disables reporting.
- An empty node-specific URL clears an inherited global URL for that node.
- `statpost_time` follows the same inheritance and sets the periodic reporting
  interval. It defaults to 60 seconds and accepts 30 through 600 seconds.
- Invalid or unsupported URLs disable reporting for the affected node with a
  useful configuration warning; they do not prevent the radio node from
  starting. Accept only HTTP and HTTPS URLs. HTTPS uses normal certificate
  validation. Do not add credentials or log sensitive URL components.

## Status contract and timing

Use the ASL3 GET/query-parameter status format, not a new JSON protocol. Each
request identifies the node, current time, and monotonically increasing
sequence number, and carries only status fields that `rpt_advanced` can report
accurately. Encode query values correctly. Represent link state using the
ASL-compatible `T`, `R`, `L`, and `C` states where applicable. Do not fabricate
ASL counters that have no equivalent in `rpt_advanced`.

Publish link and transmitter-key state changes promptly, with a short
coalescing delay of about 200 ms, then publish periodic link snapshots at
`statpost_time`. While keyed, refresh key status every 30 seconds. A failed
request is non-fatal and is retried on the next state change or scheduled
report; do not create an unbounded retry queue.

## Runtime ownership

The node controller owns status selection, sequence/timing policy, and
generation-aware configuration. A bounded, coalescing connectivity worker
owns URL resolution and HTTP I/O. It consumes immutable status snapshots and
never blocks an audio callback or the serialized control executor. A reload
replaces a node's reporting configuration through the existing generation
lifecycle; results from an older generation cannot affect the new one.

This stays within ADR 0012's connectivity and controller layers. It does not
add an Asterisk or ASL3 dependency to the controller, a new shared library, or a
new API surface. It does not alter DTMF, REST, WebSocket, or CLI status
behavior.

## Verification scope

Tests should cover global and per-node inheritance, clearing an inherited URL,
disabled reporting, accepted/rejected URL schemes, accurate query encoding and
field mapping, sequence progression, change-triggered coalescing, periodic
timing, request failure without blocking control/audio work, and configuration
reload generation replacement. Use a local HTTP test endpoint; tests must not
post to the public service.

## References

- [ASL3 `rpt.conf` reference](https://allstarlink.github.io/config/rpt_conf/)
  defines `statpost_url` and `statpost_time` configuration conventions.
- [ASL-Asterisk `app_rpt.c`](https://github.com/AllStarLink/ASL-Asterisk/blob/develop/asterisk/apps/app_rpt.c)
  is the reference implementation for the status request format and reported
  link/key state.
