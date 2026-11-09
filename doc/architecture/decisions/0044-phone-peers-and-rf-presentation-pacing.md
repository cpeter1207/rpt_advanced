# ADR 0044: Private phone peers and bounded RF presentation pacing

Status: Accepted by owner 2026-11-09; media prerequisites released. Macro and
private-call foundations implemented; phone/RF integration pending.

## Context

Autopatch/reverse patch must use the ordinary peer processing path while
half-duplex radios retain receive opportunities. Intentional RF interruptions
create presentation backlog independently of network jitter or clock drift.
Calls also require private authentication, commands and replies before any RF
audio attachment. Existing numeric AllStar identities and public peer media do
not supply those contracts.

## Decision

Implement the [approved phone design](../../../docs/superpowers/specs/2026-11-09-phone-patch-and-half-duplex-design.md)
through the existing product/control/media owners, using native tools and the
repository gates. Provider accounts, mailbox identities, opaque call generations
and radio-node identities remain distinct. SIP/IAX signaling, codecs, packet
jitter and telephone events remain off audio. Phone ingress uses existing inbound
rings and shared DSP; no AllStar handshake/topology is sent to a phone provider.

Incoming calls authenticate mailbox/PIN and start command-only. Explicit `*3`
admits permitted transceive media. Private replies require a call-addressed sink
independent of attachment. `*4` addresses local/remote commands, with phone-only
literal quoting and source-scoped grants. Remote private/protected operations
require verified origin/reply/lifecycle support; ordinary legacy digit forwarding
alone does not establish per-call isolation.

Add one bounded RF programme presentation FIFO on the existing transmit owner,
after fresh peer processing/mixing and before the controller's local RF mix.
Fresh network fanout retains its current source timeline. Do not queue final
PCM, telemetry, regeneration, safety output or access signaling. Preserve source
age through the FIFO and converter; revoke retained backend history with media.

The RF owner may use an explicitly prepared manual-pacing capability in the
shared samplerate provider. This changes intentional presentation consumption,
not the 48 kHz DAC rate or inbound source-clock recovery. Ordinary conversion
retains its nominal ±1000 ppm range. A sized optional descriptor tail negotiates
pacing; missing capability fails explicitly. Prepare ratio limits/storage off
audio and prove processing/reset allocation guarantees across that range.

A single sample-clocked RF arbiter schedules warning, STE, actual off/settling,
usable listening and rekey. VAD and manual admission both obey receive checks;
manual actions never override VAD. Instantaneous pause evidence is distinct from
qualification hangtime. Physical unknown is not evidence of a completed check.
Retain normal safety/identifier priorities and report timing conflicts honestly.

Direct-trigger DTMF macros run on the serialized control owner. Explicit `#`
boundaries and comma deadlines do not reuse the human timeout/auto-completion
parser. Delayed steps retain source/generation/private reply and recheck grants.
No login/unlock credentials occur in expansions; self-revoke clears only the
executing source. Existing typed link macros remain separate.

## Consequences and verification

The first private-source slice admits a typed `PeerAddress::PhoneCall` and keeps
provider account, mailbox, call token, provider/call generations, and actual
product-assigned ingress distinct. Numeric node and operator-alias getters now
return `Option`; current Rust callers explicitly handle the node case. A phone
never receives a public ASL node alias. Ordinary ASL/Echo identities and wire
behavior are retained.

The existing sized `rptadv_peer_info_v1` record has one compatible provider-account
span tail. Its original 80-byte prefix remains valid for node peers. Phone
admission requires all 96 bytes, protocol 3, explicit private-call capability 32,
and nonzero account/call generations. Both current hosts copy only the caller's
declared capacity. Borrowed inspection does not adopt a pending handle.
Phone local bindings use the configured `NodeId` semantics and 63-byte bound,
including named nodes; ASL/Echo retain their existing decimal/wire constraints.
No host table slot, descriptor revision, or SONAME changes: old products reject unknown
phone protocol/capabilities, and new products reject incomplete phone metadata.
This additive record extension does not authorize mixing an incompatible SDK
cohort elsewhere.

The product's bounded private call owner retains its handle without directory
lookup, ASL handshake/topology, radio binding, or a radio-peer slot. Calls start
unauthenticated. A trusted serialized PIN verifier must separately publish the
mailbox result; that result never grants node administration. Selected-node
collection retains the exact private reply token and actual source, suppresses
idle RF regeneration, and rejects legacy remote forwarding without the required
private-origin/reply/lifecycle profile. Retiring the source ends queued replies
and source-owned grants/collectors; committed reload and stop retire all calls.

Private status preparation reuses the existing status formatter and producer
PCM stream. A bounded per-call queue feeds the existing `Playback` renderer,
independently of `NodeController`, RF demand, and public peer fanout. Its source
fence runs before and after rendering. Other private report families fail
explicitly pending their prepared-stream routes. Phone tokens cannot enter the
ordinary radio telemetry queue, including stopped-macro and route diagnostics;
those paths return an explicit rejection until they have a private sink.
Ordinary phone commands and macros retain the exact call token through lookup,
dial, initial permanent retries and final publication. Ending the call revokes
unfinished work; completed node-link commands keep their existing recovery rules.

The inert telephone grammar emits typed routing, literal-digit and manual-key
requests. It implements `*3`, `*4`, one-symbol `**` quoting and the existing
collection bounds; syntax alone grants no authority or RF attachment. Its
caller must retain the verified source and private reply on every dispatch.
Provider egress, PIN verification,
grammar integration/configuration/allowlists, explicit `*3` media attachment,
per-handle DSP ownership, and negotiated remote lifecycle remain dependent work;
this foundation does not enable a usable phone provider or claim complete Task 3.

ADRs 0003/0025/0035 retain exclusive inbound conversion/clock recovery while
allowing this narrowly scoped RF presentation operation. There is no new drift
estimator, audio worker, mixer, or DSP engine. EQ/dynamics/limiting remain shared
FFmpeg graphs. Transport/control and generation reclamation keep their existing
owners under ADRs 0023/0026/0037/0038.

Prepared pacing is released in samplerate adapter `v0.2.0-alpha3`; common
source-rate Earshot analysis is released in Radio `v0.1.0-alpha.11`.
Source-timed ring rendering is released in ring `v3.0.0-alpha5`. All three
providers passed their full native Debian 13 gates; their signed APT indexes
and both architectures' runtime/dev package hashes match the release assets.
Consumers must check each optional descriptor tail and use the released
versioned libraries. SDK `v0.1.0-alpha33` adds structured output feedback;
its signed packages and native installation checks are verified. Builds using
the alpha33 descriptor headers require matching alpha33 build/runtime floors:
the existing complete-record check safely rejects the smaller alpha32 table.
Controller feedback consumption, phase/age qualification and the RF
presentation scheduler remain separate work.

The current-node RF/IAX macro foundation has source-owned cancellation,
asynchronous attachment fencing and explicit command boundaries. Phone-private
authentication, routing, RF deadline/age bounds and their integrated
reload/unload behavior still need implementation and automated evidence.
Actual provider/NAT and physical switching/STE/listening acceptance remain
separate. The [implementation plan](../../../docs/superpowers/plans/2026-11-09-phone-patch-implementation.md)
records task ordering; this ADR does not claim completion or authorize deployment.
