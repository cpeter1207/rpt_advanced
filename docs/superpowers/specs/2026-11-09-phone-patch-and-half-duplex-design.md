# Phone patch, DTMF macros, and half-duplex receive opportunities

Date: 2026-11-09. Status: design approved by owner 2026-11-09; unimplemented.

This document records the user's phone-patch requirements and subsequent
decisions. Engineering proposals are identified separately. It does not change
runtime configuration, authorize a node installation, or claim provider or RF
acceptance. Paging remains a separate wishlist item.

## Intent and scope

Provide outgoing autopatch and incoming reverse patch through ordinary SIP and
IAX providers without requiring Asterisk. Reuse the controller's peer routing,
audio owners, Earshot VAD, shared processing graphs, command authorization, and
radio hardware paths. Keep a half-duplex receiver available for break-in while
preserving speech as far as a bounded delay permits. Support general DTMF command
macros, including operator-configured telephone-number shortcuts.

The first implementation supports SIP/IAX. Google Voice is optional when a
supported integration exists. Free consumer Google Voice is not a programmable
SIP-trunk commitment; Google's documented SIP Link has paid-plan and certified
SBC requirements. Do not implement an unofficial account/browser bridge.

## Accepted requirements

The following are owner decisions, not discretionary engineering defaults.

| ID | Required behavior |
| --- | --- |
| P01 | Common SIP/IAX providers; native Rust crates where practical; Google Voice when supported. |
| P02 | A phone connection is a private peer/call entity, accessible to local nodes or explicitly allowed local/remote nodes. It is not automatically a public AllStar node. |
| P03 | Every incoming caller supplies mailbox number and PIN, even on private SIP. Configuration associates the mailbox with first name and callsign. Mailbox identity can support future user features. |
| P04 | Incoming calls start with private control access. Explicit `*3` is required to attach transceive audio to an eligible node. Control replies remain private. |
| P05 | All authenticated callers may issue commands that do not require node unlock. Existing protected commands retain their unlock requirement; mailbox login does not unlock them. |
| P06 | Ordinary connected-call digits go to the telephone. `*4` selects a specific local or remote node for commands. Preserve `*3` connection and `*4` addressed-control concepts. |
| P07 | Manual keying is a configured alternative to VAD admission. It never temporarily disables or overrides VAD. Mandatory receive checks apply in either mode. |
| P08 | Earshot qualifies received telephone voice; VAD can make phone-to-peer transmission discontinuous. Its pause detector also guides RF receive windows. |
| P09 | A configurable five-second deadline limits postponement of a usable receive opportunity. Prefer a natural pause before the deadline. |
| P10 | Warn the RF user before a check; preserve enabled STE as far as hardware permits. Minimum listening time and radio switching delays are configurable. Extend listening according to the actual pause, rather than a fixed maximum unkey timer. |
| P11 | Total programme interruption accounts for STE, unkey/usable listening, and rekey lead-in. A 200 + 100 + 200 ms configuration costs 500 ms. Avoid audible speech interruptions longer than 100 ms where possible. |
| P12 | Target normal added local playout latency below 250 ms; permit up to 500 ms during adjustment. Use slight adaptive playout-rate changes and necessary frame discard to avoid accumulating delay. |
| P13 | Configure autopatch playout to local RF or all eligible peers. Default autopatch suppresses peer audio in both directions at the initiating node for the complete call. |
| P14 | A per-call peer-isolation bypass is available through DTMF only after node unlock. Anyone knowing the unlock code may enable it. The bypass expires with that call. |
| P15 | Suppress peer-bound audio from the first detected DTMF digit through command completion. Keep addressed `*4` commands out of band. |
| P16 | Configurable conference limits default to one node attachment per call and one reverse-patch call per node. The provider enforces the account's paid incoming-call allowance. |
| P17 | The provider owns permitted destinations, cost controls, and emergency-service/E911 setup. Do not duplicate those number-class policies in the controller. |
| P18 | Configure DTMF macros directly by trigger command, unlock protection, and command string. Strings contain multiple commands, termination characters, and comma pauses; no separate numbered-macro mapping is required. |

## Approach and existing boundaries

The selected direction follows P01/P02: native transport adapters over the
existing product and peer path. An external PBX may be the configured SIP/IAX
service, but it is not a controller dependency. A separate telephone mixer/DSP
engine would conflict with the requested common path and is excluded.

Relevant current decisions are ADRs 0003, 0005, 0023, 0025, 0026, 0035, 0037,
0038, and 0043. Current sources establish these integration seams:

| Responsibility | Owner and planned integration |
| --- | --- |
| Accounts, mailboxes, call admission, attachment, commands and unlock | Existing serialized control owner. Distinguish provider account, mailbox ID, call generation, and node ID. |
| SIP/IAX dialogs, network packets, codecs, network jitter and telephone DTMF | Serial per-call transport/media owner, outside radio callbacks. Reuse released IAX/codec providers where applicable. |
| Canonical PCM, source conversion, independent-clock recovery, delayed DTMF/VAD admission | Existing per-peer inbound PCM ring. Prepare a phone-specific low-latency profile within this owner. |
| Per-source DSP and fresh network fanout | Existing link-audio and media dispatcher. Phone admission must not send ASL `!NEWKEY1!`, topology, or radio-only messages to a provider. |
| RF-only presentation pacing and receive windows | Existing TX owner, with a prepared bounded programme FIFO immediately before `NodeController::process_audio_with_parrot` in `rust/core/src/link/audio.rs`. |
| Final audio, Peak Limiter/meters, access signaling and hardware PTT | Existing downstream SDK/radio path, once. Preserve independent requested/serviced/observed output state. |

Do not encode phone legs as numeric AllStar nodes merely to pass current
`ProtocolId` checks. Admit a typed phone leg with explicit capabilities. Both
standalone and the optional Asterisk host use the same product behavior; neither
adds a second radio-device owner. Asterisk must not process link audio twice.

The 2026-11-12 signaling selection rejects the unqualified third-party SIP
stacks rather than maintaining vendor fixes. The owned bounded parsing
prerequisite lives in the existing shared `rptadv-peer-support` leaf so both
hosts can reuse one implementation. It parses admitted bytes only; it is not a
transaction engine, provider, resolver, media stack or authorization boundary.
The [parser plan](../plans/2026-11-12-bounded-sip-parser.md) records its limits,
RFC applicability and verification sequence. Signaling interoperability and
bounded transaction/admission lifecycles remain prerequisites to activation.
Existing released codec and selected SDP/RTP media dependencies are unchanged.

## Call admission and routing

Use the lifecycle invited/dialing, authenticating, command-only, attached,
and closing. A call generation owns its collectors, grants, queued commands,
audio admission and replies. Hangup or replacement revokes them together.
Provider/trunk authentication never substitutes for mailbox authentication.
Store PINs as salted password digests; use bounded login attempts and private
prompts. Never log or regenerate PIN/unlock digits.

A command-only incoming call consumes a transport resource but no RF attachment
slot. `*3` without an explicit destination attaches the configured local target;
`*3<node>` selects an eligible explicit target using normal collection completion.
Attachment requires both phone-side destination permission and destination-side
permission. Empty remote permissions permit no remote attachments. Local targets
must be explicitly configured or derived from the called account's local binding.

At a configured attachment limit, reject the additional attachment privately and
preserve existing ones. No implicit replacement. Local/all-peer playout follows
normal mix-minus; never reflect a source to itself. Several calls attached to one
node remain independent peers when the configured limit permits them.

For outgoing calls, acquire initiating-node peer isolation before dialing and
release it on failure or hangup. Multiple isolated calls retain independent
claims: ending one cannot clear another's suppression. Suppression includes
buffered media revocation, not just fresh enqueue. It does not disable DTMF
detection, addressed control, safety signaling, or call teardown.

Bypass affects only its call's isolation claim and is checked against the
initiating node's call/source-scoped unlock. It always requires the unlock code,
including when remote-base DTMF access is configured `Open`. Existing mailbox
authentication, command availability, and remote destination selection grant no
bypass authority. No extra mailbox command-role system is introduced.

An outgoing telephone recipient receives call media without automatically
acquiring node-control privileges. A reverse caller with no telephone egress
never has ordinary digits echoed back to itself. This is a routing rule, not an
implicit `*3` connection.

## Human DTMF grammar — proposed details

Keep configured command mappings; the following describes default `*3`/`*4`.
The phone ingress grammar is distinct from the existing RF/IAX grammar.

| Context/input | Effect |
| --- | --- |
| Telephone mode, ordinary digits including `#` | Telephone egress, when one exists. No implicit node command. |
| Telephone mode, `*3` / `*3<node>` | Request permitted transceive attachment. |
| Telephone mode, `*4<node>` | Select local or remote command destination; selection alone creates no audio attachment. |
| Phone node-command mode, bare `#` | Exit to telephone mode locally, as current remote selection does. |
| Either phone mode, `**` followed by one DTMF symbol | Send that one literal symbol to the selected sink, bypassing only the outer phone grammar. |

Thus `***4` sends literal telephone `*4`; `***3` sends literal telephone `*3`;
and `**#` terminates a selected node command without leaving command mode.
Quoting never bypasses the destination's parser or authorization. An unfinished
quote expires privately using the normal collection timeout; it is not replayed.

Extend phone-local selection to hosted nodes without removing the current
RF/IAX self-target rejection. Human RF/IAX unkey, `#`, selection persistence,
sequence numbers, addressed relaying, and no-echo behavior remain unchanged.
Phone manual-key/unkey actions are remappable, available in phone command mode
only, and cannot overlap reserved routing, unlock, or macro triggers. Proposed
defaults are `*91` key and `*90` unkey. Under configured VAD admission these actions
report unavailable and do not change the mode. Existing hangup/disconnect actions
remain available through the selected node's command namespace.

Deduplicate in-band and negotiated telephone-event DTMF. Gate buffered leading
and trailing audio for the complete detected command, including delayed samples
after its terminator. Normal termination/unkey/timeout ends collection; it does
not cause already-suppressed PCM to become audible. Detection and OOB commands
continue while media is suppressed. Authentication and control replies go only
to the originating call, unless an explicitly attached destination receives
ordinary transceive programme.

### Remote authorization compatibility

RPT addressed digit origins admit a private opaque call token and key collectors
by origin plus actual ingress connection. Emit no public topology/dialable node
for that token. Add explicit call-origin, private-reply, and lifecycle capability
negotiation and cleanup; unique tokens without retirement would exhaust the
current bounded origin collectors. A claimed origin remains routing metadata,
not authentication.

Legacy AllStar `app_rpt` can collect forwarded DTMF in shared state regardless
of the `D` message source. An opaque token or exclusive command lease alone
cannot prove independent unlock state. Ordinary interoperable control remains
subject to the destination's policy. Protected operations that require isolated
per-call grants must require a verified destination capability or a separately
verified authenticated/resettable session; otherwise report unsupported.
Private remote command replies also require an addressed return path to the
originating call, not ordinary broadcast peer PCM. A command-only connection may
not open a media attachment to obtain its replies. Verify remote regeneration
and reply audience before accepting a private-control profile; report unsupported
operations rather than leak their digits or responses to RF/other peers.
Do not advertise legacy per-call authorization isolation from successful audio
or public-command interoperability. This boundary needs explicit acceptance
coverage and documentation before claiming complete remote phone control.

## DTMF command macros — proposed execution details

Keep existing `[macro ...]` single typed link actions. Add distinct
`[dtmf_macro <label>]` entries, with optional node scope matching configuration
conventions, containing `trigger`, `protected`, and `commands`. The label is
descriptive, never another number the operator must map to a DTMF function.

`commands` is a node-control command stream, not raw audio or a shell command.
It preserves DTMF symbols and explicit `#` command terminators. Each comma waits
the configurable pause duration; proposed default 2000 ms. Repeated commas add
their durations. Pauses occur between complete commands; a comma after a partial
command is invalid rather than an implicit `#`.
Whitespace is insignificant. Semicolon is a configuration comment and must not
be advertised as a command separator. Operator-defined emergency-number macros
use the ordinary provider dial action and preserve short numbers such as 911 or
999; no default emergency shortcut is activated automatically.

Execute expansions through normal typed command dispatch with the original
source identity and private reply route. `protected=yes` requires active unlock
at admission and before each delayed command; `protected=no` does not authorize
protected commands in the expansion. Pauses and macro admission do not renew the
unlock timeout. Never submit phone macros through a local-trusted ABI that loses
their source identity.

Proposed credential rule: expansions cannot submit mailbox login or admin-unlock
operations, or contain their credentials. Obtain grants interactively before
invocation; ordinary lock/revoke operations remain available. This prevents a
public macro from disclosing or exercising a configured secret merely because
its trigger is public. Apply the rule after remapping and destination resolution;
an unverifiable credential-bearing remote expansion is unavailable.

Macro control streams use `*4` as an explicit destination selector, including the
hosted local node. Within that stream `#` terminates the selected target's
command, rather than acting as the human phone-mode exit. Target selection is
scoped to the expansion; it does not silently change the human caller's selection
after completion/cancellation. Destination grants remain source/call-scoped.
Require explicit terminators in configured streams; wall-clock interdigit expiry
does not split a macro command. The selector consumes its destination terminator;
later terminators go to the selected parser, using addressed digits remotely.

Compile bounded tokens on control and advance on monotonic deadlines without
sleeping the control owner. Proposed configurable limits: 4096 expanded symbols,
120 seconds total duration, one expansion per source, no nested macro invocation
initially.
Invalid/ambiguous triggers or expansion syntax warn and leave that macro
unavailable, following configuration fallback policy. Do not shadow built-ins.
Source loss, permission/generation retirement, explicit cancellation, or fresh
live command input cancels remaining work. Ordinary RF unkey after a trigger
does not cancel an admitted macro. Completed commands are not rolled back.

## VAD and half-duplex receive windows

Phone `tx_mode=vad|manual` selects admission, with VAD proposed as the default.
Use Earshot analysis before delayed playout, with pre-roll in the existing
inbound ring and configurable voice hangtime (proposed 1000 ms). Manual mode
uses explicit key/unkey state; Earshot still measures pauses for receive checks.
Peer discontinuous transmission is separately configurable. Do not confuse
voice qualification hangtime with instantaneous silence for pause planning.

One node-level RF arbiter covers phone, remote-base and ordinary linked
programme. Actual full-duplex radios omit receive windows; a controller's
logical duplex setting alone cannot establish physical simultaneous reception.
When enabled on half-duplex hardware:

1. Track time since the last usable receive opportunity. Start warning/STE early
   enough that the receiver can actually begin listening by the five-second
   deadline. A speech pause is preferred, not a reason to exceed the deadline.
2. Play the configured warning on local RF, perform enabled STE using its normal
   signaling owner, request PTT off, and respect actual off/settling information.
   Unknown physical PTT state never proves a usable receive opportunity.
3. Listen for at least the configured usable receive time, after settling. Reuse
   normal receiver COS/CTCSS/DCS/VAD qualification and delayed speech pre-roll.
   Detected local RF activity holds reception for the user's complete turn.
4. With no RF activity, extend the off interval while telephone/link speech is
   absent. When queued speech requires resumption, complete rekey lead-in before
   playing it. A minimum window cannot be shortened by new speech.

The maximum quiet interval follows the observed pause; no invented SM-100
numeric maximum is imposed. VAD cannot predict future speech: speech arriving
during STE/listening/rekey enters the bounded programme policy below. During
prolonged RF activity stale caller speech may be discarded, and the caller gets
a private receive/busy indication rather than an indefinitely growing replay.
The warning cadence, minimum listen, receiver settle, rekey lead-in and STE cost
are operator-visible settings. Unusable timing combinations warn and select a
safe deterministic profile; never silently count deaf time as listening.

Optional telemetry must not delay a reserved receive check. Mandatory identifiers
and safety retain their existing priority/deadlines and remain outside the
programme FIFO, but their RF intervals participate in the same reservation plan.
Do not claim a supported timing profile unless both requirements fit. A safety
event may cancel phone/link programme instead of preserving the call's TX turn.
An overrun or hardware failure is reported as a missed check, never a reset of
the deadline clock that makes the metric appear satisfied.

The SM-100 manual supplies the beep, adjustable check interval/window, switching
allowance, and full-turn RF break-in behavior. Its 300 ms window is an example,
not a published minimum, maximum or factory default. Initial switching settings
must therefore be explicit hardware-profile values measured during acceptance.

## Speech pacing and latency

Keep fresh source audio and network timestamps flowing through the existing
network dispatcher while RF pauses. The additional FIFO holds only RF-bound
peer/telephone programme, after fresh source processing and before the local
controller mix. It does not hold complete final PCM, telemetry, regenerated
digits, parrot/safety notices, access signaling or PTT transitions. Controller
demand distinguishes queued RF-playable programme from current source activity.

The RF owner drains this FIFO through one persistent manual-pacing conversion
capability in the shared samplerate provider. Hardware output stays 48 kHz with
the required callback frame count; acceleration consumes more source samples.
Existing inbound rings alone retain their source-clock estimators. This new
capability follows queue-age policy and introduces no second drift estimator.
Proposed ordinary speed bounds are 1.00–1.02, configurable within prepared safe
limits; simple sample-rate variation slightly changes pitch.

This is a prerequisite provider change: current samplerate ABI-2 only permits
nominal ratio ±0.1%. Preserve its ordinary contract and add an explicitly
negotiated manual-pacing capability, with allocation/storage bounds prepared on
control. Current SDK FFmpeg exact-block graphs cannot simply add `atempo`;
variable consumption, hidden buffering and callback allocation would need a new
qualified contract. Continue all EQ/dynamics/limiting through shared FFmpeg.

Account actual consumed/generated samples, converter history, ring reserve,
VAD/DTMF lookahead, packetization, jitter buffering, graph latency and key lead-in
in the local age budget. Preserve original source time through compression;
never reset timestamps to disguise old media. Network/provider delay is reported
separately from controllable local delay. The generic peer jitter/ring targets
already exceed the phone budget; do not stack them unchanged with another buffer.
Proposed phone packet-jitter minimum/initial/maximum is 20/40/100 ms with 20 ms
packets, within existing owners and subject to measured end-to-end validation.

For acceptance, measure local age from first local admission of the corresponding
transport PCM span to scheduled DAC presentation, using its preserved mapped
source position. Include every local queue/filter in that interval; report radio
switching wait and first-syllable latency separately as well. Provider/network
delay preceding local admission is a separate measurement, not a controllable
500 ms guarantee. Fixed hardware output delay must be calibrated or bounded.

Prefer removing silence and bounded acceleration. Discard stale speech in short
bounded spans with transition smoothing when necessary to keep retained audible
media within 500 ms of its source time. Flush converter history as well as queue
contents on revocation/discontinuity. If no separately attributable removal is
possible in the composite RF bus, conservatively flush on contributing-cohort
changes. Ordinary talkspurt end may drain still-valid audio.

At 2% acceleration a 500 ms backlog takes about 25 seconds to recover. Repeated
forced gaps can require substantially more throughput, so lossless continuous
speech is not promised. A 500 ms hold added to an existing 200 ms path also
exceeds the budget immediately; later acceleration cannot undo that deadline.
Source PCM received while RF is off can be buffered or discarded; RF speech
never captured while the hardware is deaf cannot be reconstructed. PLC/FEC
cannot restore those missing words over an analog RF path.

## Verification and implementation readiness

Before behavioral implementation, amend ADRs 0003/0025 for RF presentation
pacing, ADR 0023 for phone grammar/macros/call origins, and transport/lifecycle
decisions as needed. Keep telephony marked unimplemented until acceptance.
Select/pin dependencies and ABI/package consequences in the approved plan.

Required automated evidence includes:

- SIP and IAX simulated-provider setup, authentication, DTMF, failures and
  teardown; no AllStar handshakes or unintended topology on phone legs.
- Mailbox/private control before `*3`, both allowlists, conference busy behavior,
  mix-minus, complete bidirectional isolation and reference-counted bypass.
- Per-source unlock, protected macro steps after pause/expiry, quote/terminator
  cases, literal telephone star/pound menus, local and remote `*4`, and immediate
  buffered DTMF suppression without losing OOB commands.
- Manual/VAD mode independence, natural/forced checks, RF full-turn priority,
  warning/STE/rekey ordering, deadline/age bounds, stale-frame removal, hidden
  converter retention, and uninterrupted network fanout during RF windows.
- Live reload/unload without Asterisk restart; generation retirement, canceled
  macros, revoked queue/history, codec/session cleanup and bounded callback work.
- Maintained formatting/lint/static/documentation checks, native Debian 13 amd64
  and arm64 build/test/package/staged-install gates, and required 100% production
  line/branch coverage on amd64 before any PR merge.

Provider acceptance must verify real signaling, NAT, codecs, DTMF and account
policies. Physical-radio acceptance must measure actual off/settle/rekey times,
minimum usable listening, STE, break-in, initial syllables and audible loss/catchup.
No automated fixture alone proves those outcomes. Deployment and release require
their own authorized, verified handoffs.

Builder decisions remaining include concrete crate/codec profiles, scoped
configuration names, safe prepared pacing bounds and ABI evolution. The owner
approved the proposed defaults and legacy-remote authorization limitation on
2026-11-09;
there are no unresolved mailbox, provider-dial-policy or conference-limit choices.

## Sources and applicability

- [ACC SM-100 original manual](https://www.repeater-builder.com/acc/pdfs/acc-sm100.pdf),
  printed pp. 30–34, 39–40, 51–52 and 65–66 (PDF pp. 36–40, 45–46, 57–58,
  71–72). Timing/behavior precedent, not a VAD or buffering standard.
- [ITU-T G.1020 (07/2006)](https://www.itu.int/rec/T-REC-G.1020/en), delay/loss/
  jitter tradeoffs; [RFC 7294 §3.2](https://www.rfc-editor.org/rfc/rfc7294.html#section-3.2),
  reporting of playout adjustment, sample insertion/deletion and time scaling;
  [RFC 7005 §3.3](https://www.rfc-editor.org/rfc/rfc7005.html#section-3.3), adaptive
  buffer model. They do not mandate the owner's 250/500 ms limits or one algorithm.
- [RFC 6716 §4.4](https://www.rfc-editor.org/rfc/rfc6716.html#section-4.4) and
  [RFC 7587 §3.3](https://www.rfc-editor.org/rfc/rfc7587.html#section-3.3), codec
  concealment and Opus in-band FEC; neither guarantees recovery of a 500 ms gap.
- [AllStarLink macros](https://allstarlink.github.io/adv-topics/macros/), command
  sequencing precedent. Direct triggers and comma pauses are this owner's
  requested design, not ASL configuration compatibility.
- [AllStar `app_rpt` source](https://github.com/AllStarLink/app_rpt/blob/de9a43929516caa08d7fbfdef8a6a706499146d4/apps/app_rpt.c#L2103-L2176),
  legacy shared remote-digit collection; RPT source/ADR contracts govern its
  different source-scoped behavior.
- [Google SIP Link requirements](https://knowledge.workspace.google.com/admin/voice/sip-link-requirements)
  and [rsipstack upstream](https://github.com/restsend/rsipstack), optional provider
  path and an unselected signaling candidate, respectively.
