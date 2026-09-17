# ADR 0023: Unified control operations and DTMF policy

Status: Accepted

Reaffirmed 2026-09-17: this record's source-scoped telemetry routing is the
required behavior. ADR 0025's earlier all-local restriction was an intermediate
implementation and does not override this decision.

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
fallback and is routed only to its command source: a local-receiver command
reply goes to the local transmitter, while a linked-peer command reply goes
only to that originating peer. It waits for the originating source to unkey
when that source has an unkey state. Replies to CLI and REST commands remain
on the requesting control interface and are not injected into RF.

Identifiers are separate from command replies. Periodic, first-key, and other
configured identifiers are sent only to the local transmitter; they are never
forwarded to linked peers.

When DTMF muting is enabled, muting remains active for the entire command,
until the command ends because the source unkeys, a command terminator digit is
received, or the command times out. That muting behavior is unchanged by peer
regeneration. If DTMF regeneration is enabled, forwarding to linked peers
stops as soon as the command can be determined not to be intended for a given
peer. The local transmitter is never subject to that forwarding cutoff.

The `*4` remote-command prefix is the one deliberate discovery phase: DTMF
continues to be forwarded to all connected peers while the destination node
number is being collected. Once the complete destination node number has been
entered, forwarding stops completely for every other peer and continues only
to the selected destination. Subsequent digits remain subject to the command's
normal termination by unkey, terminator digit, or timeout.

Administrative DTMF commands require both an unlock and a lock code. Codes
have global defaults with per-node overrides; omitting either disables DTMF
administration. Store codes only as salted digests using the strongest available
algorithm in an upgradable format. The default administrative idle timeout is
five minutes, and only a successful administrative command renews it. The lock
code immediately clears the administrative unlock.

Pad test is a public inherited per-node command whose default is `*82`. After
the command, a local RF user may enter any of the sixteen DTMF symbols. Local
receiver unkey alone ends capture. The first 127 symbols are retained; later
ones are discarded and reported as overflow. The reply follows the
source-specific routing above, uses speech with Morse fallback, calls `*` “star”,
`#` “pound”, reads `A` through `D` individually, and says “no digits” for an
empty capture. Captured digits are not interpreted as later commands, and the
normal decoded-tone muting remains in effect.

Peer DTMF regeneration is a configurable capability. Its routing and cutoff
rules above are separate from decoded-tone muting, which remains controlled by
the command-source policy. Remote-base DTMF operations use this same
command-map and authorization policy as described by ADR 0004.

## Consequences

All operational interfaces submit typed controller requests rather than
reimplementing node policy. RF replies remain serialized telemetry and cannot
interrupt their origin while it is keyed. DTMF changes require command-map,
authorization, muting, peer-forwarding-cutoff, and RF-response tests in
addition to DSP detection tests.
REST/WebSocket route and schema publication remains governed by ADR 0016.
