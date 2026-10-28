# rpt_advanced wishlist

This file tracks requested work that has not yet been implemented.

For each new requirement, record the requirement, any material decisions made
for it, and unresolved decisions needed before implementation. Ask for the
unresolved material decisions before starting the work. Remove the entry when
the requirement is implemented.

DTMF parrot controls, pad testing, local-transmitter regeneration, and addressed
ASL command forwarding are implemented per ADRs 0023 and 0025. The controller
component extraction, including DTMF generation, is released per ADR 0011.
The standalone controller, shared radio core, direct PortAudio composition,
Rust migration, and `res_usbradio` removal are also implemented. Their ongoing
constraints remain in the ADRs; only remaining work is listed below.

Status audit: 2026-10-17, against merged code and tests. Software-contract
completion is not a claim of physical hardware qualification or deployment.

## Entries

### Complete Asterisk 20 IAX audio-format interoperability

**Status: Sixteen active formats implemented; final controller verification in progress (2026-10-27).** The full
target remains all 19 audio formats mapped by Asterisk 20's IAX compatibility
layer. The user deferred `siren7`, `siren14`, and `g719`; the remaining 16
formats are the active implementation scope. Format recognition alone is not
complete codec support.

The [codec and text-media design](docs/superpowers/specs/2026-10-27-protocol-neutral-codecs-and-text-media-design.md)
was approved on 2026-10-27. Its [implementation plan](docs/superpowers/plans/2026-10-27-protocol-neutral-codecs-and-text-media.md)
is approved. Provider and IAX library gates pass on native Debian 13 amd64 and
arm64, including full production coverage. The ABI-matched candidate is running
on 524950; local repeat, courtesy, and time speech are confirmed. The controller
gate and final incoming-text refinements remain in verification. These candidate
components have not been released.

**Requirements and decisions recorded**

- Support `g723`, `gsm`, `ulaw`, `alaw`, `g726aal2`, `adpcm`, `slin`,
  `lpc10`, `g729`, `speex`, `ilbc`, `g726`, `g722`, `siren7`, `siren14`,
  `slin16`, `g719`, `speex16`, and `opus` for negotiated receive and transmit.
- Follow Asterisk 20's wire-format IDs, extended capability negotiation,
  packet layouts and codec framing. Preserve Asterisk's exact `adpcm`
  payload format, distinct from G.726, and the RFC/AAL2 G.726 packing variants.
- Reuse released codec implementations through independently replaceable
  dynamic adapters. Keep codecs separate from IAX protocol and network I/O.
  Retain canonical 48 kHz internal PCM and the existing shared conversion,
  jitter-buffer and PCM-ring boundaries. Codec-native concealment must always
  remain enabled and take priority; shared-ring PLC is fallback only and must
  not conceal codec-recovered samples again.
- Give codec adapters versioned, implementation-neutral contracts so replacing
  a codec implementation does not require changing protocol or controller
  code. Keep external-library types and calls inside the selected adapter.
- Codec adapters are shared, protocol-neutral components, not part of IAX2.
  Other protocol adapters may reuse them without depending on IAX2. Protocol
  adapters own their wire-format identifiers, negotiation and packet framing;
  codec adapters own codec bitstreams and encode/decode state. This does not
  add another protocol implementation to the current scope.
- Prefer released GPLv2-compatible implementations wherever they exist.
  Write an original implementation only when no suitable compatible released
  implementation exists and the required implementation is trivial. Do not
  replace an available library with custom codec code. Nontrivial missing
  implementations remain explicitly pending rather than being advertised.
- Advertise only codecs with functioning encode/decode providers. A missing
  provider must not be presented as implemented coverage of that format.
- IAX iLBC uses fixed 30 ms mode. Owner deferred 20 ms peer interoperability;
  do not infer a mode from aggregation length or switch it during a call.
- Leave `siren7`, `siren14`, and `g719` unimplemented and unadvertised until
  their provider work is separately resumed. Do not write replacement codec
  implementations or acquire additional licenses for these deferred formats.
- Validate codec payloads against independent Asterisk-compatible fixtures
  and test negotiation, framing, rate conversion and isolated peer routing.

**Material decisions needed before implementation**

None for the deferral decision. The provider design for the active 16 formats
must retain the project's GPL-2.0-only policy; relicensing and commercial codec
purchases are not authorized. The trivial-implementation allowance does not
resume the three explicitly deferred formats.

### Incoming text-message speech codec

**Status: Implemented in the candidate; final verification in progress (2026-10-27).** Treat human text
messages from text-capable protocols as a protocol-neutral codec input that
produces local-only spoken incoming media, not controller telemetry. This is
separate from the 19 Asterisk IAX audio-format identifiers above.

The approved [design](docs/superpowers/specs/2026-10-27-protocol-neutral-codecs-and-text-media-design.md)
describes local speech and independent text forwarding. Its
[implementation plan](docs/superpowers/plans/2026-10-27-protocol-neutral-codecs-and-text-media.md)
is approved. A real directed IAX loopback message was heard locally on 524950.
The initial-attachment race, sender attribution, and interruption of ordinary
voice telemetry are covered by focused regressions; their live retest and the
controller's final gate remain pending. EchoLink's protocol adapter remains planned.

**Requirements and decisions recorded**

- Include incoming EchoLink human text and messages from every other
  text-capable protocol supported now or added later. This supersedes the
  prior EchoLink incoming-text exclusion, not the planned status of EchoLink
  or any other unimplemented protocol adapter.
- Convert incoming human messages to speech through a replaceable codec
  adapter, reusing the existing speech-synthesis capability outside audio
  callbacks. Keep protocol parsing and message identification in the protocol
  adapter.
- Mix the resulting speech with active incoming audio rather than waiting
  for other transmissions to end. This includes concurrent audio from the
  same peer that sent the text, not just audio from other peers.
- Prefix speech with the stated sender: localized "node [digits] says:" or
  "[callsign] says:". Spell sender characters, but retain the literal body and
  original forwarding bytes. Playable text speech interrupts ordinary voice
  telemetry using its Morse fallback; it does not assert RF receive activity.
- Send text-derived speech only to the local transmitter, mixed with incoming
  audio there. Never include that speech in peer audio egress. Retain the
  originating peer identity. Do not put it through the polite controller-
  telemetry queue or switch it to Morse because incoming audio is active.
- Forward eligible messages to other connected peers only as text, when the
  protocols support that forwarding. Each receiving peer decides whether and
  how to render the text. Preserve normal no-echo rules; do not reflect a
  message to its sender. Text forwarding does not depend on local speech
  synthesis succeeding or completing.
- Do not speak protocol-control text: topology, keying, authentication,
  disconnect, command-forwarding and other signaling remain protocol data.
- Preserve lock-free audio and single-writer PCM-ring ownership. Speech
  synthesis must not block audio or another peer's packet handling.
- If speech synthesis fails, skip its local spoken rendition and log an error.
  Do not fall back to Morse or interrupt other incoming audio; supported text
  forwarding remains independent.

**Material decisions needed before implementation**

None.

### Local receiver S-meter and spoken query

**Status: Planned.** Written design approved on 2026-10-20; the
[written specification](docs/superpowers/specs/2026-10-20-local-receiver-s-meter-design.md)
and [implementation plan](docs/superpowers/plans/2026-10-20-local-receiver-s-meter.md)
record the work. Plan review is pending; no implementation or node changes have started.

- Measure suitable flat FM discriminator noise independently of COR selection,
  or reuse an external RSSI sensor acquired through the existing site adapters.
- Support shared defaults, per-node selection, calibrated dBm/S-unit estimates,
  relative quieting, measurable-range bounds and explicit observation validity.
- Reuse public, remappable `*76<dtmf_id>#` for a localized spoken report with
  the existing Morse fallback and source-only routing after unkey.
- Retain a local query's actual receive-burst observation so delayed command
  handling does not report post-unkey noise or a later station instead.
- Keep raw measurement, sensor acquisition and telemetry with their current
  owners; do not change audio processing, gains or squelch behavior.

### Verified shared-clock native audio

**Status: Partially implemented.** Independent input/output callbacks, local
and per-peer inbound rings, telemetry streaming, snapshot intervals, and
generation-safe reload are implemented. Only the optional verified shared-clock
adapter path remains pending under ADRs 0003, 0025, and 0027.

**Requirements**

- Integrate and qualify the paired receive-then-transmit path only when an
  adapter can prove that ADC/DAC clocks are shared and supply aligned frames.
- Use the existing local inbound ring in unity pass-through, with no adaptive
  correction or redundant converter. Its target reserve is the configured
  squelch delay; zero adds no local-ring delay.
- Adopt one coherent generation across the paired calls. Retain bounded frame
  handling, elapsed-sample timing, receive qualification, and PTT safety.

**Decisions recorded**

- Unknown clock relationships remain independently paced. The current
  PortAudio adapter uses separate capture and playback streams at 48 kHz.
- The independent-clock composition is complete; this item does not require
  duplicate controller rings or a second mixer in USBRadioPlus.
- Device and DSP latency remain even when local-ring delay is zero. Peer and
  telemetry rings retain their independent recovery.
- The implemented `status_snapshot_interval_ms` has per-node overrides, a
  global fallback, and a 50 ms default. RF edge events publish immediately;
  periodic snapshots publish at the first callback ending at or after their deadline.

**Material decisions needed before implementation**

None.

### EchoLink audio interoperability

**Status: Planned.** No EchoLink adapter is implemented.

**Requirements**

- Interoperate with the EchoLink network at audio level with feature parity to
  ASL3's EchoLink support.
- Support EchoLink registration, directory/station metadata, inbound and
  outbound audio connections, EchoLink keepalive/liveness handling, per-station
  access control, configured station limits, and bridging an EchoLink call to
  its configured rpt_advanced node.
- Permit each EchoLink account to use a user-provided proxy for its EchoLink
  audio/control transport. The proxy may be used for NAT traversal or to make
  multiple local nodes reachable from one host. Supplying, operating,
  authenticating, and securing the proxy are outside the project scope.
- Treat EchoLink peers as normal audio-link participants for transmitter,
  telemetry, identifier, courtesy-tone, and receive-activity behavior. Do not
  claim that EchoLink provides AllStarLink topology advertisements.
- Provide ASL3-equivalent EchoLink audio receive/transmit gain adjustment,
  configurable connection-announcement content (off, node number, callsign,
  or both), and configurable EchoLink telemetry output behavior.
- Support ASL3's reserved EchoLink dialling form: pad the four- through
  six-digit EchoLink node number to six digits and prefix it with `3`, so it is
  usable through the normal configurable link-connect command.
- Convert incoming EchoLink human text messages to speech using the shared
  text-message codec. Mix that speech with live audio, including audio from
  the text sender, only at the local transmitter, not in peer audio egress.
  Forward messages to eligible peers as text only where the protocols support
  forwarding; the receiving peers choose their own rendering.
- Outgoing text composition, text-command control, chat interfaces and text
  recording remain out of scope.

**Decisions recorded**

- Scope includes audio interoperability, incoming human text converted to local
  speech, and supported text forwarding without synthesized peer audio. The
  incoming-text addition was explicitly approved on 2026-10-26.
- ASL3's current EchoLink behavior remains the audio compatibility reference;
  the shared text-message codec defines incoming-text speech behavior.
- Each rpt_advanced node has a separate EchoLink station account and identity.
- Configuration exposes every ASL3 EchoLink station and directory field:
  callsign, EchoLink node number, password/secret source, name, location,
  email, latitude/longitude, RF frequency, CTCSS tone, power, antenna height,
  gain, direction, and status message.
- Incoming calls allow all valid EchoLink stations by default. Per-node
  callsign allow and deny lists support wildcards; deny takes precedence.
- Outbound calls use only the normal configurable link-connect command and
  `3xxxxxxx` EchoLink-node mapping. Callsign and directory lookup are not
  required.
- The default simultaneous-station limit is 20, matching ASL3's documented
  configuration example. It is configurable per node.
- The default control-heartbeat timeout is 10 seconds, matching ASL3's
  documented default. It is configurable per node.
- EchoLink calls use the existing link reconnect policy: explicitly
  disconnected or ordinary on-demand calls do not reconnect automatically;
  permanent and scheduled links use their established reconnect policy.
- EchoLink uses a per-node UDP audio/control port pair, defaulting to
  `5198`/`5199`. It binds all local interfaces unless an explicit bind address
  is configured. Nodes sharing a host use unique adjacent pairs. The
  administrator provides direct router port forwarding and firewall rules when
  the account does not use its configured user-provided proxy.
- The supported proxy contract is a user-operated EchoLink-aware UDP relay,
  configured with a host, audio/control port pair, and optional credential
  reference. It carries both UDP audio and control traffic and preserves enough
  source identity for inbound-call access control. No proxy is shipped or
  operated by rpt_advanced.

**Material decisions needed before implementation**

None.

### Complete control and status interfaces

**Status: Partially implemented.** Current CLI/DTMF controls, remapping,
hashed unlock/lock, source-scoped replies, parrot controls, and pad testing are
implemented. Complete operation/status parity, REST, WebSocket, OIDC, and
code-derived OpenAPI documentation remain pending under ADRs 0016 and 0023.

**Requirements**

- Provide a complete CLI interface for every supported rpt_advanced operation
  and status query.
- Make every DTMF command configurable.
- Provide a REST API that can perform every supported operation and query every
  supported status.
- Expose code-derived REST API documentation alongside the REST API. The
  published contract must describe every versioned endpoint, request, response,
  status code, authentication requirement, and streaming handoff where
  applicable.
- Provide a WebSocket streaming API for status changes, including audio-level
  data at a cadence suitable for real-time meter displays.
- Use the CLI, DTMF, REST, and WebSocket interfaces as the stable control-plane
  foundation for user-interface development. A supported operation or status
  may not be exclusive to just one applicable control interface; WebSocket is
  intentionally status-streaming only.
- When DTMF requests status, send the resulting status over the air as speech
  with Morse fallback after the originating source unkeys, when that source has
  an unkey state.

**Decisions recorded**

- Asterisk CLI, REST, and DTMF expose the shared supported operation and status
  catalog as their transport permits; WebSocket is deliberately status-only.
- DTMF status is deferred until the applicable originating source unkeys.
- The WebSocket stream must be responsive enough for real-time audio meters.
- The CLI is implemented as Asterisk CLI commands only.
- REST and WebSocket remote access is permitted through nginx or Apache. The
  reverse proxy provides TLS termination.
- Unauthenticated REST and WebSocket clients may view all status. Every
  authenticated OIDC user has full administrative access.
- Use a free, open-source OIDC provider that supports a local user registry and
  external OIDC identity providers.
- REST URLs and WebSocket streams are versioned from the first release.
- DTMF retains familiar AllStarLink default commands and permits per-node
  remapping.
- A DTMF status response is sent only to its command source: local-receiver
  replies use the local transmitter and linked-peer replies use only that peer.
  It waits for its originating source to unkey. CLI/REST responses remain on
  the requesting control interface (ADR 0023).
- The WebSocket status stream includes peak, RMS, clipping, FIFO/ring
  statistics, COR, CTCSS, PTT, and link state at 20 updates per second. Slow
  clients receive the newest sample and stale meter samples are dropped.
- Use Keycloak as the initial supported OIDC provider.
- The REST and WebSocket listeners bind only to loopback; nginx or Apache is
  responsible for externally reachable proxying and TLS termination.
- DTMF mappings that conflict are invalid configuration.
- All DTMF sources may request status. Administrative DTMF commands require an
  unlock code; a lock code clears that administrative unlock.
- Administrative DTMF unlock expires after a configurable no-command period,
  defaulting to five minutes.
- WebSocket is status-streaming only. Administrative operations use REST.
- Unlock and lock codes support global defaults and per-node overrides. If
  either required code is omitted, DTMF administration is unavailable.
- All DTMF codes are stored as salted digests using the strongest available
  algorithm. The stored format supports migration to stronger algorithms.
- Only a successful administrative command renews the administrative unlock
  timeout.
- Generate an OpenAPI 3.1 contract at `/api/v1/openapi.json` and provide an
  interactive Swagger UI at `/api/v1/docs`.
- The OpenAPI document and documentation UI are available without OIDC
  authentication, matching read-only status access.
- Use a maintained Rust code-first OpenAPI generator and schema annotations.
  Tests fail if any routed REST endpoint lacks a documented operation.

### Mode- and frequency-agile remote base

**Status: Planned.** Hamlib control, speech squelch, and remote-base operations
remain unimplemented under ADR 0004.

**Requirements**

- Support a node as a mode- and frequency-agile remote base using Hamlib's
  rig-control library for radio control.
- Audit the legacy radio-control features currently exposed by USBRadioPlus and
  replace each with Hamlib where Hamlib provides an equivalent. Retain only
  features Hamlib cannot provide.
- Source COS, CTCSS, and DCS from USBRadioPlus DSP or its existing hardware
  signaling. Replace USBRadioPlus hardware signaling with Hamlib only where
  practical.
- When a radio supports single sideband and the selected mode is not FM, use
  full-duplex node operation so linked-peer audio can key the transmitter even
  when receiver squelch is unavailable.
- Provide DSP speech squelch for AM and single-sideband receive. It must make a
  best-effort human-speech decision rather than merely thresholding audio level.
- For single sideband, make a best effort to reject off-frequency splatter in
  the speech-squelch decision.
- Extend the familiar AllStarLink default DTMF command set with remote-base
  operations. Every default remains subject to per-node remapping.

**Decisions recorded**

- Hamlib is the primary radio-control implementation.
- COS, CTCSS, and DCS remain USBRadioPlus DSP or hardware-signaling functions
  unless a Hamlib replacement is practical.
- Non-FM single-sideband-capable operation requires full duplex.
- Support one Hamlib rig per node. Additional radios use additional nodes.
- Make a best effort to select the appropriate radio audio path: CM119 for
  conventional radios, Linux-supported onboard USB audio where available, and
  streaming APIs where a supported radio provides them.
- Support all Hamlib control transports.
- Hamlib models are configured by canonical enumerated names, not numeric model
  identifiers.
- Per-node frequency ranges, modes, transmit-power limits, and receive-only
  ranges are configurable. Out-of-range transmit or receive requests are
  rejected.
- Authenticated REST users and authorized Asterisk CLI users may control the
  rig. DTMF rig control is configurable per node as open, unlock-code-protected,
  or disabled.
- Reject a non-FM mode request when the selected radio/audio implementation
  cannot provide full duplex.
- The DSP speech-squelch implementation and its configurable defaults may be
  selected during implementation. If unavailable, fall back to conventional
  noise squelch; if that is unavailable, leave receive open.
- Remote-base configuration uses inheritable `[remote_base]` and
  `[remote_base node]` sections. `hamlib_model` is a canonical model name,
  `audio_source` selects `cm119`, `alsa`, or `hamlib_stream`, and
  `hamlib_option_*` passes named backend options.
- Speech squelch uses stateful 16 kHz Silero VAD through ONNX Runtime. A
  configurable fixed SSB passband in the shared FFmpeg graph is the universal
  fallback; capable I/Q sources may add adaptive pre-demodulation selection.
- Default remote-base DTMF commands cover frequency, mode, receive/transmit
  VFO, split/offset, transmit power, CTCSS/DCS selection, remote-base status,
  and scan control. Other supported rig capabilities remain available through
  the Asterisk CLI and REST API.

**Material decisions needed before implementation**

None.

### Site I/O, telemetry, and alarms

**Status: Partially implemented.** The ADR 0033/0034 software contracts now
provide independent CTCSS source profiles, capability-based multiport GPIO,
explicit arm/readiness, epoch-gated output intent, and fault recovery. Tests
compose the real radio and GPIO shared libraries with simulated hardware.
The radio, GPIO, and controller PRs merged after their native Debian 13
amd64/arm64 gates passed. Device integration and physical interlock qualification
remain pending; those software gates are not a release or device rollout.
Analog monitoring, sensors, fan control, general alarms/actions, configuration,
read-only DTMF reports and typed shared-product controls now have implemented
software and targeted tests. The full hosted gate passed on native Debian 13
amd64 and arm64, including 100% production line and branch coverage on amd64.
Physical site-device qualification remains outstanding. New REST/WebSocket
transports are not part of this implementation.

**Requirements**

- Support configurable GPIO inputs and outputs, voltage, current, temperature,
  and fan monitoring.
- Support thresholds, hysteresis, alarms, actions, analog meters, outputs, and
  fan control.
- Use hardware-neutral GPIO with capabilities reported by each backend: USB
  radio interfaces such as URIXB on a Raspberry Pi, CM119 GPIO, parallel ports,
  and appliance GPIO. Do not require every device to provide every I/O type.

**Decisions recorded**

- CM119 and parallel-port GPIO already have backends. Additional hardware support
  must use its actual capabilities; a device example does not claim a verified
  driver or pin mapping.
- The appliance adds its isolated DB-25 analog/digital front end and physical
  interlock. Other GPIO devices need not provide its analog monitoring, arm
  feedback, watchdog, or physical mutes. Require only the capabilities used by
  configured signals and that hardware's safety contract.
- CTCSS encode and decode independently select disabled, DSP, or external per
  port. External encode may be combined with DSP decode for encoder-only radios.
  Reuse the existing DSP and controller encode policy; no activity-scoped decode.
- Software imposes no fixed port-count limit. Allocate state at control setup
  according to configured endpoints and available resources. The appliance's
  four-port limit applies only to its physical board.

**Approved implementation — 2026-10-18**

The [site/message design](docs/superpowers/specs/2026-10-17-site-monitoring-and-message-macros-design.md)
records configuration, calibration/freshness, alarms, safe outputs, fan policy,
read-only DTMF queries, and localized reports. Composed messages, release tails,
bounded site acquisition and equipment actions are implemented with targeted tests.
Clock-aligned composed-message events, optional complete Morse for tails/events,
configured playback audiences/repeat policy, manual selectors and current-message
cancellation are implemented on the shared message path; validation is tracked
in the scheduled-message plan. They do not add a second scheduler or renderer.
The full hosted gate passed at `eb7fead05370a4a2e0b639d95515987add071a04`
([run40021040672](https://github.com/cpeter1207/rpt_advanced/actions/runs/40021040672)).
Physical site-device qualification remains outstanding.

### Remote-base memories and scan lists

**Status: Planned.** Requires the remote-base capability and decisions below.

**Requirements**

- Support named remote-base memories containing frequency, mode, and tone
  settings.
- Support named scan lists.

**Decisions recorded**

- Named memory and scan-list operation is a required remote-base capability.

**Material decisions needed before implementation**

- Scan dwell, resume, hold, priority, and stop conditions.
- Whether memories and scan lists are configuration-only or may be changed and
  persisted through administrative REST/CLI operations.

### Operational configuration management

**Status: Partially implemented.** Tolerant parsing, validation, transactional
reload, and failed-reload/device rollback exist under ADRs 0001 and 0019.
Standalone also has `--check-config`. Operator backup, Asterisk CLI/REST
validation commands, historical rollback, and audit workflows remain planned.

**Requirements**

- Provide configuration backup, validation, rollback, and audit logging.
- Expose these functions through the Asterisk CLI and REST API only.

**Decisions recorded**

- DTMF and WebSocket do not provide configuration-management operations.
- Unknown configuration sections and parameter names are ignored with warnings.
  Unknown, malformed, or unsupported values are warnings that resolve through
  inheritance and a documented sensible default. Warnings do not prevent a
  configuration reload.

**Material decisions needed before implementation**

- Backup storage location, retention, encryption policy, and whether backups
  include uploaded media.
- Rollback selection model, live-reload behavior, audit-record retention, and
  audit fields.

### Local audio sources, voting, and recorded messages

**Status: Partially implemented.** Multiple configured nodes, local-file
telemetry decoding/playback, clock-scheduled file/speech/Morse messages,
announcements, and temporary parrot replay exist.
Network audio sources, voter interoperability/native voting and simulcast,
persistent over-air message storage/playback management, and REST uploads
remain planned. Local-file decoding does not imply network stream support.

**Requirements**

- Support multiple local receivers/transmitters through multiple configured
  nodes connected by local loopback or the local network. Do not support more
  than one receiver/transmitter per node.
- Play local files in common compressed and uncompressed formats and audio
  streams using common codecs and sample rates, including weather and Amateur
  Radio Newsline sources.
- Interoperate with ASL3 `chan_voter` and Simple Voter.
- Provide a voter using GPS-disciplined computer clocks and dynamic rate
  correction for CM119 clock drift. Make a best effort at automatic
  nanosecond-level playout-time correction for simulcast; sample-by-sample
  simultaneity is not required.
- Record over-the-air audio as stored sound-file messages; play filesystem and
  uploaded sound files once or on a schedule. REST supports sound-file upload.

**Decisions recorded**

- Local multi-radio systems use multiple configured nodes and loopback or local
  network connections.
- Each node retains one receiver and one transmitter.
- Voting interoperates with ASL3 `chan_voter` and Simple Voter and also has a
  GPS-disciplined native path with best-effort near-simultaneous simulcast.
- Recording and uploaded sound files are playable message sources.

**Material decisions needed before implementation**

- Allowed local/network audio transports and stream protocols, source
  authentication, and source-failure behavior.
- Supported file/stream codecs and formats, plus safety limits for network
  streams and FFmpeg decoding.
- Voter implementation order, GPS receiver interface, timing-distribution
  model, voter selection policy, and simulcast calibration/measurement method.
- Recording format, maximum duration/storage quota, retention/deletion policy,
  upload size/type limits, and REST upload authentication/authorization.

### Telephony integration

**Status — 2026-10-16:** Planned. DTMF testing, regeneration, and addressed
command forwarding are released in `rpt_advanced` v0.1.0-alpha11 with
`librptadvdtmf` v0.1.0-alpha.2; those are not open telephony requirements.
The standalone AllStarLink IAX2 implementation is also released. Autopatch,
reverse patch, paging, and SIP/VoIP-provider integration remain unimplemented.

**Requirements**

- Support autopatch, reverse patch, and paging through an external IAX or SIP
  connection, with a more user-friendly interface than ASL3.
- Support IAX2 or SIP connection to a self-hosted Asterisk instance or
  third-party VoIP provider, including Google Voice.

**Decisions recorded**

- Telephony connects to an external IAX or SIP service. The released native
  IAX2 implementation serves AllStarLink; telephony session behavior is not
  implied by that protocol implementation.

**Material decisions needed before implementation**

- Telephony provider scope, outbound/inbound/reverse-patch access rules,
  dialing restrictions, emergency-call policy, recording policy, and caller-ID
  behavior.

### Generic audio-device control extraction

**Status: Deferred.** The named squelch, CTCSS, DCS, GPIO/parallel signaling,
and controller-function extractions are implemented and independently released
under ADR 0011. Generic audio-device control is the remaining deferred boundary.

**Requirements**

- Define a platform-neutral audio-device control contract without changing
  radio behavior or duplicating the existing PortAudio/ALSA adapter.
- Keep platform I/O outside `librptadvradio` and its portable DSP components.
- Once its boundary is defined, distribute the extracted component as its own
  versioned shared library, repository, and Debian runtime/development packages.

**Decisions recorded**

- PortAudio/ALSA owns current audio-device I/O. OSS and `res_usbradio` are
  removed, not future backends to restore.
- Existing radio and controller components remain dynamically linked; their
  completed extraction is not open work.

**Material decisions needed before implementation**

- The distinct audio-device control responsibility and its boundary with the
  existing audio and GPIO adapters.

### Standalone appliance integration and ingress security

**Status — 2026-10-17:** Standalone execution is implemented. The remaining
appliance integration and ingress-security work is planned. Standalone execution,
AllStarLink registration and calls, the resolver boundary, and Tokio peer
ingress/egress are released and live-validated under ADRs 0005, 0022 and 0037.
The appliance hardware, ingress security, REST/WebSocket services, and other
future capabilities below are not covered by that completion status.

**Requirements**

- Integrate the standalone service with the appliance's selected radio ports
  and hardware safety mechanisms under ADRs 0032--0034.
- Complete appliance compliance, offline update/recovery trust, rotation, and
  revocation qualification under ADR 0030. Signed APT distribution already exists.
- Provide the appliance-hosted OIDC, proxy, WAF, TLS, and trusted-proxy ingress
  described below and in ADR 0006.
- Share the REST/WebSocket work with the control-interface entry and the
  hardware work with the site-I/O entry; these are not duplicate implementations.
- Preserve the implemented standalone, lock-free audio, and per-peer Tokio
  ownership boundaries, with low resource use on inexpensive hardware.

**Decisions recorded**

- Standalone operation has no Asterisk dependency.
- System DNS/SRV resolution now uses a required versioned resolver DSO (ADR
  0022); directory policy remains shared with the Asterisk composition.
  This boundary is implemented, not a remaining standalone feature.
- Both module and standalone controller execution use neutral worker and
  lifecycle contracts. The replaceable control-path adapter uses
  Asterisk's taskprocessor for that composition; standalone uses its owned executor
  with identical functional semantics (ADRs 0036/0038).
- The control-path adapter owns only taskprocessor submission, serialized FIFO
  execution, backend resources, and safe stop/drain. Scheduling, task meaning,
  generation checks, and node policy remain in the controller. A later owned
  or third-party backend must pass the same execution/lifecycle contract tests;
  no replacement taskprocessor is required now.
- I/O and inter-thread communication are lock-free at native media boundaries.
  Standalone callbacks never share a PCM writer: bounded non-waiting mailbox
  admission feeds the peer's exclusive async owner. A full mailbox rejects new
  packets with observable drops; a stalled peer cannot stop unrelated peers.
  SpeexDSP adaptive packet buffering is called only through a separate versioned
  jitter adapter; codec decoding and shared-ring conversion/PLC stay separate.
  Implementation status — 2026-10-16: the Tokio/jitter cutover and isolated
  peer egress are merged, released, and installed from signed Debian packages.
  Native platform gates and production coverage passed; local radio and linked
  parrot audio were confirmed on 524950 as recorded in ADR 0037.
  The Asterisk path retains its existing ingress exception.
- IAX2 interoperability targets AllStarLink.
- Implement IAX2 in our own independently versioned `librptadviax2` library.
  Keep packet/session logic separate from datagram I/O, and keep codec adapters
  separate from protocol logic. Use released codec libraries through plug-in
  adapters wherever possible; do not write codecs when a suitable released
  implementation exists.
- Small binary size, low CPU use, and low memory use are first-class design
  constraints.
- The Asterisk module and standalone application share as much controller code
  as possible in the short term. Standalone operation eventually replaces both
  the rpt_advanced module and its USBRadioPlus Asterisk dependency.
- USBRadioPlus becomes a dynamically linked shared object with an ABI kept in
  lockstep with rpt_advanced. Thin Asterisk/ASL3 adapters remain optional
  front ends and may later be discontinued without affecting the standalone
  controller.
- Standalone audio I/O uses `libportaudio2` only through the selected
  PortAudio/ALSA adapter.
- IAX2 interoperability is limited to current ASL3 parity; no additional NAT
  traversal is required. Support ASL's current HTTPS registration mechanism,
  not retired IAX registration.
- The appliance target is a 1U rack controller with an industrial ARM SoM,
  1--4 GB RAM, 8--64 GB eMMC, Wi-Fi, Ethernet, USB-C, a status display,
  DB-25 site I/O, and one through four direct-codec radio ports on a common
  carrier clock. It is not a literal CM119 design. Detailed appliance hardware
  is maintained in the private appliance hardware repository.
- The four-port ceiling is a board constraint, not a software limit. Other
  hardware compositions may provide any number of ports that available hardware
  and host resources can support.
- A fully populated appliance has a 54 W normal input budget, a 75 W continuous
  input-path rating, four USB-C CAT ports limited to 5 V / 0.9 A each, and a
  33 W sustained enclosure-heat budget at +55 °C ambient. Selected-component
  and four-port-fixture validation is required before DVT.
- Standalone rpt_advanced runs as a non-root Linux service managed by
  `systemctl`. It is upgraded as a Debian package through `apt`.
- Logging uses `/var/log` with log rotation when local resources permit, or an
  external logging service otherwise.
- REST/WebSocket administration is separate from SSH/Linux accounts. SSH is
  used for terminal administration; REST/WebSocket access permits delegated
  administration without Linux login access.
- ASL3 interoperability is defined by the current official ASL3 registration,
  IAX2, and application behavior and is verified through compatibility tests.
- Standalone hardware endpoints are selected by stable identity and mapped
  one-to-one to configured nodes: a CM119 uses a stable Linux identity such as
  a udev serial, while an appliance radio port uses its carrier-port identity.
  Ambiguous or unavailable audio devices fail closed.
- USBRadioPlus and rpt_advanced Debian packages use exact matching versions to
  preserve their lockstep shared-object ABI.
- Appliance package updates use a product-scoped APT `Signed-By` keyring. An
  offline product root certifies time-limited release keys, and an independent
  offline recovery key signs recovery manifests before any image may write
  eMMC. Rotation and signed revocation preserve this trust model; SoM secure
  boot is deferred and physical boot-storage replacement remains outside the
  initial protection boundary.
- Keycloak and nginx or Apache run on the appliance. They are configured for a
  small number of users and protect the loopback-only REST/WebSocket services.
- The appliance includes a WAF and may be directly Internet-connected through
  Cloudflare or a comparable proxy service.
- Ingress supports IP allow and deny lists so traffic can be limited to trusted
  proxy address ranges such as Cloudflare's.
- Use a lightweight WAF with an OWASP-style ruleset, few-user rate limits, and
  non-blocking suspected-false-positive logging.
- Trusted proxy ranges update automatically from the provider's published list;
  an update failure retains the last known-good range set.
- Without a configured trusted proxy, HTTPS permits only explicitly allowed
  private/LAN addresses and otherwise fails closed.
- HTTPS certificates are provisioned by Let's Encrypt.

**Material decisions needed before implementation**

None.
