# ADR 0044: Private phone peers and bounded RF presentation pacing

Status: Accepted by owner 2026-11-09; prerequisites and macro foundation
implemented, phone/RF integration pending.

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

ADRs 0003/0025/0035 retain exclusive inbound conversion/clock recovery while
allowing this narrowly scoped RF presentation operation. There is no new drift
estimator, audio worker, mixer, or DSP engine. EQ/dynamics/limiting remain shared
FFmpeg graphs. Transport/control and generation reclamation keep their existing
owners under ADRs 0023/0026/0037/0038.

Prepared pacing is released in samplerate adapter `v0.2.0-alpha3`; common
source-rate Earshot analysis is released in Radio `v0.1.0-alpha.11`.
Source-timed ring rendering is implemented in the ABI3 alpha5 candidate; its
native package gates remain pending. Consumers must check each optional
descriptor tail and use the released versioned libraries. These capabilities
do not by themselves implement structured radio-off feedback or the RF
presentation scheduler.

The current-node RF/IAX macro foundation has source-owned cancellation,
asynchronous attachment fencing and explicit command boundaries. Phone-private
authentication, routing, RF deadline/age bounds and their integrated
reload/unload behavior still need implementation and automated evidence.
Actual provider/NAT and physical switching/STE/listening acceptance remain
separate. The [implementation plan](../../../docs/superpowers/plans/2026-11-09-phone-patch-implementation.md)
records task ordering; this ADR does not claim completion or authorize deployment.
