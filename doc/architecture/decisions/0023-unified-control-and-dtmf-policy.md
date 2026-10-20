# ADR 0023: Unified control operations and DTMF policy

Status: Partially implemented (DTMF slice released and operator-confirmed; broader interfaces pending)

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

Peer command collection and prepared replies retain the originating connection,
not merely its node number. Each receive source has independent partial-command and
administrative-unlock state. A disconnected or replaced connection cannot
receive an old reply. Addressed reply PCM carries its destination through the
existing program-audio dispatcher; it is excluded from local transmitter demand
and from every other peer's output. Prepared-media ownership is reclaimed on
control, never by an audio callback.

Identifiers are separate from command replies. Periodic, first-key, and other
configured identifiers are sent only to the local transmitter; they are never
forwarded to linked peers.

When DTMF muting is enabled, muting remains active for the entire command,
until the command ends because the source unkeys, a command terminator digit is
received, or the command times out. Raw-audio muting is independent of explicit
tone regeneration and of the selected remote-command destination.

Amended 2026-10-15: DTMF regeneration and `*4` forwarding follow ASL3. This
supersedes peer PCM regeneration and forwarding during destination collection.
Regeneration is disabled by default and sends eligible received link digits
only to the local transmitter. Command-start and collected command digits are
not regenerated. No regenerated tone enters peer program audio. Ordinary local
RF audio continues to obey the existing muting policy; this feature does not
add a second local-RF digit replay.

The inheritable per-node regeneration settings select fixed tone duration,
interdigit gap, and combined dual-tone peak level. Approved defaults are
200 ms, 50 ms, and -12 dBFS, with equal row/column amplitudes. An idle digit
queue has ASL3's 500 ms initial delay. Generation belongs to the shared DTMF
component, reusing the shared tone component; synthesis stays outside audio
callbacks and playback is local-only PCM. Like ASL's local generator, that
tone/gap span replaces ordinary local voice-band output rather than mixing
with it. It never replaces peer output or changes input muting. Already
playing media advances normally rather than accumulating a delayed replay.

The `*4` command collects and validates its destination locally. Neither its
prefix nor destination digits are broadcast. Subsequent digits use ASL's
`D <destination> <source> <sequence> <digit>` text envelope. Send directly when
the destination is adjacent; otherwise relay the addressed envelope through
eligible links. A transit node does not execute or regenerate those digits
and does not return the message to its ingress/source. Preserve destination,
source, and sequence through relays. Only the addressed node processes the
digit under its normal command and regeneration policy.

An actual `#` exits remote-command mode locally and is not forwarded. Ordinary
RF unkey and the normal command-collection timeout do not clear the selected
destination. They still end that source's raw-audio muting interval. Source
loss and generation retirement discard stale source-owned work. Model actual
unkey separately from a received `#`, including for pad test.

Administrative DTMF commands require both an unlock and a lock code. Codes
have global defaults with per-node overrides; omitting either disables DTMF
administration. Store codes only as salted digests using the strongest available
algorithm in an upgradable format. The default administrative idle timeout is
five minutes, and only a successful administrative command renews it. The lock
code immediately clears the administrative unlock.

The default DTMF unlock and lock prefixes are `*800` and `*801`; each requires
the corresponding configured code followed by `#`. They are remappable with
the node's other command prefixes. The parrot enable and disable operations use
`*804` and `*805`, are also remappable, and require an active administrative
unlock. Those parrot changes are live only; configuration reload restores the
configured `parrot_enabled` value.

Implementation status — 2026-10-05: the hashed-code unlock/lock and authorized
live parrot commands are implemented in the Rust configuration and runtime.
Buffer allocation and reclamation follow ADR 0025.

Pad test is a public inherited per-node command whose default is `*82`. After
the command, a local RF user may enter any of the sixteen DTMF symbols. Local
receiver unkey alone ends capture. The first 127 symbols are retained; later
ones are discarded and reported as overflow. The reply follows the
source-specific routing above, uses speech with Morse fallback, calls `*` “star”,
`#` “pound”, reads `A` through `D` individually, and says “no digits” for an
empty capture. Captured digits are not interpreted as later commands, and the
normal decoded-tone muting remains in effect.

Remote command transport does not bypass inbound access policy or DTMF
administrative authorization. A claimed source in a relayed text envelope is
not authentication. Preserve independent command/unlock state and the current
originating-connection reply contract. Remote-base DTMF operations use this
same command-map and authorization policy as described by ADR 0004.
Remote destination selection is applied before the next queued digit is parsed.
Each ingress connection may retain at most 128 simultaneous origin collectors;
new starts above that bound are ignored without evicting active commands or
administrative grants. Idle digits need no collector, and expired idle state is
reclaimed on control. This does not limit the number of connected peers.

Implementation status — 2026-10-16: pad capture, localized readback, local-only
PCM regeneration, and addressed direct/transit command routing shipped in
controller v0.1.0-alpha11 with DTMF provider v0.1.0-alpha.2. Regeneration uses
the generator-capable DTMF shared-library descriptor; startup rejects an older
provider. Tests cover both host ingress paths, source isolation, waveform
boundaries, and transmitter safety. The full native PR gate passed with 100%
production coverage, and both components were released. Node 524950 has the
packages installed with its configuration preserved. The operator subsequently
confirmed that DTMF pad testing and regeneration work correctly. Addressed relay
behavior has automated coverage; that confirmation is not a manual test of every
remote-command route. The broader
CLI/REST/WebSocket control and status catalog remains incomplete. ADR 0016
governs REST/WebSocket route and schema publication.

## Consequences

Approved extension (2026-10-17): configured sensor/input queries (`*76` plus
selector) and message-macro requests (`*77` plus selector) are public read-only
operations. Selectors are canonical positive decimal strings, not link
destinations; zero does not recall a prior destination. They cannot configure
equipment or execute action macros. Prefixes inherit and may be remapped;
conflicting selectors/prefixes are rejected. Replies preserve the originating
connection and unkey/routing rules above. IDs and tails using the same composed
content remain local-only. Message-macro requests are implemented with targeted
command, routing, queue, and configuration tests. Local-only tail playback is
implemented with release, deadline, interruption, and allocation tests. Site
queries and typed site controls are implemented with targeted shared-product
and adapter tests; the full hosted gate for this extension has not run.

All operational interfaces submit typed controller requests rather than
reimplementing node policy. RF replies remain serialized telemetry and cannot
interrupt their origin while it is keyed. DTMF changes require command-map,
authorization, muting, local-only regeneration, addressed relay, and RF-response
tests in addition to DSP detection tests.
REST/WebSocket route and schema publication remains governed by ADR 0016.

Site-control boundary approved 2026-10-20: the product exposes a separate typed
administrative operation for logical output on/off/pulse, fan automatic/manual
demand, alarm acknowledgment/reset, extrema reset and manual transmit inhibit.
The Asterisk CLI is already protected by its administrative host access; future
adapters must authorize their caller before invoking this operation. The shared
executor checks the active generation and ownership handoff before policy runs.
Read-only DTMF site queries do not gain access to it. The standalone composition
validates the same product table; this addition does not create a new remote
CLI, REST, or WebSocket transport.
