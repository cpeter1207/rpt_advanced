# ADR 0023: Unified control operations and DTMF policy

Status: Accepted

## Context

rpt_advanced must expose one supported control and status catalog through the
Asterisk CLI, REST, and configurable DTMF without turning RF input into an
unbounded administrative interface. It also needs predictable RF responses and
pad-test behavior while preserving decoded-tone muting.

## Decision

Asterisk CLI, REST, and DTMF map to the same controller operation and status
catalog wherever their transport can represent the operation. The CLI is
Asterisk CLI only. WebSocket is the deliberate exception: it streams status
only and accepts no operations. REST and CLI configuration-management
operations are not exposed through DTMF or WebSocket.

DTMF starts with familiar AllStarLink defaults and permits inherited per-node
remapping. Duplicate command strings make configuration invalid. Every DTMF
source may request status. A status reply is prepared as speech with Morse
fallback, is routed to the local transmitter and all linked peers, and waits
for its originating local receiver or link peer to unkey when that source has
an unkey state.

Administrative DTMF commands require both an unlock and a lock code. Codes
have global defaults with per-node overrides; omitting either disables DTMF
administration. Store codes only as salted digests using the strongest available
algorithm in an upgradable format. The default administrative idle timeout is
five minutes, and only a successful administrative command renews it. The lock
code immediately clears the administrative unlock.

Pad test is a public inherited per-node command whose default is `*82`. After
the command, a local RF user may enter any of the sixteen DTMF symbols. Local
receiver unkey alone ends capture. The first 127 symbols are retained; later
ones are discarded and reported as overflow. The reply follows normal local and
linked telemetry routing, uses speech with Morse fallback, calls `*` “star”,
`#` “pound”, reads `A` through `D` individually, and says “no digits” for an
empty capture. Captured digits are not interpreted as later commands, and the
normal decoded-tone muting remains in effect.

Peer DTMF regeneration is a configurable capability. Its routing, muting,
authorization, and timing remain separate implementation decisions. Remote-base
DTMF operations use this same command-map and authorization policy as described
by ADR 0004.

## Consequences

All operational interfaces submit typed controller requests rather than
reimplementing node policy. RF replies remain serialized telemetry and cannot
interrupt their origin while it is keyed. DTMF changes require command-map,
authorization, and RF-response tests in addition to DSP detection tests.
REST/WebSocket route and schema publication remains governed by ADR 0016.
