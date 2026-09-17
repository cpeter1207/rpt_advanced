# rpt_advanced wishlist

This file tracks requested work that has not yet been implemented.

For each new requirement, record the requirement, any material decisions made
for it, and unresolved decisions needed before implementation. Ask for the
unresolved material decisions before starting the work. Remove the entry when
the requirement is implemented.

## Entries

### Activity-scoped CTCSS encode and decode

**Requirements**

- Add independent per-node controls that restrict CTCSS encode and CTCSS decode
  to active received traffic from the local receiver or a connected peer.
- When enabled, hangtime alone must not cause CTCSS encode or decode. IDs and
  courtesy tones must likewise run without CTCSS.
- Telemetry that responds to a command must retain CTCSS from command receipt
  through the response's actual playout, including any waiting interval before
  that playout begins.
- Preserve the existing CTCSS behavior when either control is disabled.

**Decisions recorded**

- The controls are per-node and independently enableable for encode and decode.
- Both controls default disabled, retaining current behavior until enabled.
- A live local-receiver or connected-peer transmission qualifies the policy;
  transmitter hangtime by itself does not.
- A pending command response is a qualifying telemetry interval from accepted
  command receipt until response playout completes. IDs and courtesy tones are
  never qualifying intervals when this policy is enabled.

**Material decisions needed before implementation**

None.

### rpt_advanced parrot with spoken audio-level report

**Requirements**

- Add an enableable parrot mode owned by rpt_advanced. While enabled, record
  received audio from the local receiver or any connected peer.
- When the originating source unkeys (local receiver or linked peer), play a
  spoken message reporting the recording's peak and RMS audio levels, followed
  by the recorded audio. Send both the message and recording through the local
  transmitter and to all connected peers, including the originating peer if
  it remains connected.
- Record PCM at the node's native sample rate (currently 48 kHz). Keep that
  native-rate recording for local playout; convert to each peer's negotiated
  sample rate at send time through its media egress, not by storing a separate
  lower-rate recording for each peer.
- Limit each recording to 30 seconds or less. Reaching the recording limit
  does not replace the source-unkey trigger for the response.
- Preserve the established lock-free audio, bounded storage, serialized
  telemetry, RF-safety, and generation-safe reload/teardown contracts. Speech
  preparation and peer encoding/sending remain outside the real-time workers.

**Decisions recorded**

- Requested feature, not implemented. No code, installed configuration, or
  running node changes are part of adding this wishlist item.
- This is controller-owned rpt_advanced functionality, not reinstatement of
  the USBRadioPlus native software-repeat or native parrot modes retired by
  [ADR 0039](doc/architecture/decisions/0039-retire-usbradioplus-native-mode.md).
- Playback order is spoken peak/RMS statistics first, recording second. The
  30-second maximum applies to the recording.

**Material decisions needed before implementation**

- How simultaneous local/peer transmissions are recorded and sequenced, and
  whether a new signal interrupts, queues behind, or is ignored during replay.
- The recording/measurement tap relative to receive processing and gain;
  spoken units and precision; and whether reported statistics cover only the
  retained recording or the entire transmission when it exceeds the limit.
- The duration setting/default within the 30-second cap, whether over-limit
  audio retains the beginning or end, and whether truncation is announced.
- How the mode is enabled/disabled and scoped, how a disconnected or stuck-keyed
  source is handled, and how replay recapture/peer echo loops are prevented.
- What happens if the spoken statistics cannot be prepared; do not silently
  substitute a different response for the requested spoken report.

### Remove USBRadioPlus native mode and native parrot

**Requirements**

- Implement [ADR 0039](doc/architecture/decisions/0039-retire-usbradioplus-native-mode.md):
  remove driver-native software local repeat and native parrot, including their
  mode-specific state, routes, controls, and obsolete tests. Neither mode is
  required by rpt_advanced or retained as a supported ASL3 option.
- Preserve ordinary app_rpt/legacy echo, hardware local repeat, shared native
  DSP/audio adapters, diagnostics, and the distinct controller transport.
- Silently ignore `duplexmode` and the retired `duplex_local_repeat_mode`
  selection. Keep `duplex3` hardware-only at its configured level, including
  load, reload, and tuning persistence; do not retain a software-mode selector.
  Update documentation and tests with the implementation. Under ADR 0040,
  remove unused shared operations rather than retaining alpha compatibility
  shims; version/package checks must reject mismatched artifacts safely.

**Decisions recorded**

- Candidate source removal is under verification. Keep this item until the
  matching shared-library/driver integration is verified. Alpha18 and all
  running nodes are unchanged.
- This does not remove rpt_advanced's native receive/transmit workers or the
  verified shared-clock fast path. Appliance native PCM/hardware is unaffected.

**Material decisions needed before implementation**

None.

### Split native receive and transmit workers

**Requirements**

- Implement the 2026-09-13 amendments to ADRs 0025--0028: call the local receive
  worker on audio input for DSP squelch, CTCSS/DCS decode, deemphasis, and local
  receive processing, then write processed audio to its local receive inbound
  PCM ring.
- Run transmit on DAC/adapter output demand. Mix native-rate output from local
  receive, per-link inbound, and telemetry playout rings; add profile-selected
  DCS or CTCSS; fill the adapter-owned output buffer directly (PortAudio's
  callback output buffer for that adapter).
- Assign all inbound source-rate conversion and drift correction to those
  rings. Do not keep a second raw-capture converter, resample the transmit mix,
  or add a PortAudio output ring/timer. Preserve routing, receive qualification,
  PTT safety, and pre-access-tone link delivery.
- Validate unequal RX/TX frame counts, independent clock drift, stalled input
  or output, per-owner DSP/event queues, and coherent generation adoption and
  safe reload/unload across both workers.

**Decisions recorded**

- Accepted design, pending implementation. Released USBRadioPlus alpha18 still
  uses its playback-driven combined native tick; no deployed behavior changes
  with this documentation amendment.
- The existing supported native rate remains 48 kHz. The adapter supplies its
  clock/cadence; the transmit worker has no separate pacing source.
- Outbound codec conversion and detector-private decimation remain separate
  from the inbound-ring conversion contract. Appliance external CTCSS and
  physical interlock requirements remain unchanged.
- Workers may execute directly in input/output callbacks; no extra OS threads
  are required by the split. Versioned compatible ABI evolution is required.
- An adapter that knows ADC/DAC are drift-free and can deliver aligned frames
  may call receive then transmit back-to-back. Use the same local inbound ring
  in unity pass-through with no adaptive correction or redundant converter.
  In shared-clock mode, target reserve equals configured squelch delay, so zero
  delay adds no local-ring delay. Keep one coherent generation across the pair.
  This is not a promise of zero device/DSP latency. Unknown clock relationships
  remain asynchronous; independent link and telemetry rings keep their recovery.

**Material decisions needed before implementation**

None.

### External-load xrun diagnostics

**Requirements**

- Investigate output underruns caused or aggravated by high activity outside
  Asterisk. Compare quiescent and CPU/I/O-loaded operation and correlate xrun
  timestamps with host scheduling and system load before changing audio code.

**Decisions recorded**

- Use the adapter's per-callback scheduling and xrun statistics when comparing
  quiescent and loaded operation; do not infer a cause from an xrun alone.

**Material decisions needed before implementation**

None.

### EchoLink audio interoperability

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
- Do not implement EchoLink chat, text messages, text-command control, or text
  recording/playback features.

**Decisions recorded**

- Scope is audio interoperability only. EchoLink text-chat and all other text
  features are intentionally out of scope.
- ASL3's current EchoLink behavior is the compatibility reference, except for
  the explicit exclusion of text features.
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

### Fallback links and remaining scheduled-link policy

**Requirements**

- Allow a primary permanent link to name an ordered set of fallback links. A
  fallback is attempted when the primary link cannot be reconnected.
- At a scheduled link-window start, allow configuration to disconnect all links,
  temporary links only, permanent links only, or no existing links before it
  connects the scheduled peer.

**Decisions recorded**

- Fallback links are used only when the primary link cannot be reconnected.
- When a primary becomes available, disconnect its active fallback before
  reconnecting the primary to avoid network topology loops.
- Future permanent-only semantics: `*806` will disconnect only permanent links
  so any permitted peer can be linked manually, and `*816` will disconnect and
  then reconnect only permanent links. Current all-link disconnect/reconnect
  behavior remains documented in the configuration manual.

**Material decisions needed before implementation**

None.

### Scheduled-link warnings

**Requirements**

- Allow each scheduled event to configure one or more warnings before its start
  and before its end or disconnect.
- Warning lead times are individually configurable, allowing patterns such as
  60, 30, 15, 10, 5, and 1 minute before start and 10, 5, and 1 minute before
  disconnect.
- Each event provides its own configurable warning message.
- For inactivity-based schedules, calculate warnings from the expected
  inactivity-timer expiration. Reset the pending warning schedule whenever
  qualifying activity resets that inactivity timer.

**Decisions recorded**

- Warnings are required before scheduled event starts and before scheduled
  event ends.
- Inactivity-based warnings follow the expected inactivity deadline rather
  than a fixed calendar end.
- Each event uses one configurable warning-message template. `${time_remaining}`
  is replaced with the natural-language time remaining for the warning.
- A warning that becomes due during local-receiver or linked-peer activity is
  skipped rather than delayed or transmitted over the activity.
- A qualifying activity reset begins a new inactivity interval; warnings that
  played during the preceding quiet interval are eligible again.
- A warning due at or after its related start, end, or inactivity deadline is
  skipped.

**Material decisions needed before implementation**

None.

### Complete control and status interfaces

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
- A DTMF status response is transmitted locally and to all linked peers. It
  waits for the originating local receiver or originating link peer to unkey.
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

**Requirements**

- Support configurable GPIO inputs and outputs, voltage, current, temperature,
  and fan monitoring.
- Support thresholds, hysteresis, alarms, actions, analog meters, outputs, and
  fan control.
- Use USBRadioPlus CM119 GPIO where available; support parallel-port GPIO for
  additional site I/O.

**Decisions recorded**

- CM119 GPIO and parallel-port GPIO are supported site-I/O sources.
- The appliance provides the same site-I/O capability through its isolated
  DB-25 analog/digital front end; CM119 and parallel-port sources remain
  supported for external adapters.

**Material decisions needed before implementation**

- Hardware abstraction and configuration model for GPIO, analog inputs, fan
  outputs, and external sensors.
- Alarm/action catalog, default-safe output state, acknowledgment behavior, and
  authorization for manual output control.
- Sensor conversion/calibration model and retention/reporting policy for meter
  history.

### Remote-base memories and scan lists

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

### DTMF regeneration and telephony integration

**Requirements**

- Provide an inheritable, per-node DTMF pad-test command. A local RF user keys
  the receiver, enters the command followed by DTMF digits, then unkeys.
- After that source unkeys, serialize a telemetry reply that reads back the
  captured DTMF digits exactly. Use the node's default speech configuration and
  fall back to Morse when speech cannot be prepared.
- The pad-test capture owns the digits after its command so they are not
  interpreted as other DTMF commands. It must preserve the normal receiver
  DTMF-muting behavior.
- Permit configurable DTMF regeneration to connected peers.
- Support autopatch, reverse patch, and paging through an external IAX or SIP
  connection, with a more user-friendly interface than ASL3.
- Preserve the long-term option to replace Asterisk with a lock-free native
  IAX2 implementation. If that direction is chosen, support IAX2 or SIP
  connection to a self-hosted Asterisk instance or third-party VoIP provider,
  including Google Voice.

**Decisions recorded**

- Pad-test results use default speech with Morse fallback and are deferred
  until the originating local receiver unkeys.
- The command is per-node configurable under the established DTMF command-map
  model. Its inherited default is public `*82`; it does not require a DTMF
  administrative unlock.
- A capture accepts all 16 DTMF symbols, including `*`, `#`, and `A`--`D`.
  Only local receiver unkey ends it. It retains the first 127 symbols,
  discards later symbols, and reports the over-limit condition before reading
  back the retained symbols.
- Pad-test telemetry follows the normal local-and-linked routing policy.
- Readback announces symbols individually: “star”, “pound”, and letters
  `A`--`D`; an empty capture says “no digits”.
- DTMF regeneration is configurable for connected peers.
- Initial telephony integration uses external IAX or SIP rather than requiring
  a native IAX2 implementation.

**Material decisions needed before implementation**

- DTMF regeneration routing, muting, authorization, and timing policy.
- Telephony provider scope, outbound/inbound/reverse-patch access rules,
  dialing restrictions, emergency-call policy, recording policy, and caller-ID
  behavior.
- Whether and when to begin a native lock-free IAX2 replacement; it is an
  architectural direction, not an active implementation requirement.

### Shared radio core and direct PortAudio adapter

**Requirements**

- Move code common to the USBRadioPlus legacy and modern implementations into
  a dynamically linked shared object named `librptadvradio`.
- `librptadvradio` must not depend on OSS or PortAudio. The ASL legacy and
  modern channel adapters use the versioned audio-I/O adapter instead.
- Link `app_rpt_advanced` with `librptadvradio` and the versioned PortAudio
  adapter rather than `libportaudio2` directly.
- Add one focused source part that connects a CM119 through PortAudio's ALSA
  backend using PortAudio's minimum recommended latency.
- PortAudio input calls only the `librptadvradio` local receive worker; output
  calls its transmit worker to fill the callback buffer directly. Neither
  performs controller, configuration, or blocking work.
- Keep this boundary suitable for the eventual standalone application without
  introducing standalone-only features in this work.

**Decisions recorded**

- The shared object owns portable USBRadioPlus radio behavior. After the
  hardware-adapter cutover, maintain one ASL3 compatibility implementation,
  not separate legacy/modern resource-module backends (ADR 0028).
- `app_rpt_advanced` may depend on public Asterisk APIs but not ASL3. Minimize
  ASL3 dependencies and constrain them to the optional ASL3 adapter; retain
  AllStarLink protocol interoperability independently (ADR 0036).
  Minimize new Asterisk dependencies and confine them to the thin module
  adapter so standalone transition does not redesign the core. The hardware
  appliance has no Asterisk or ASL3 dependency, including build and packages.
  Controller execution and lifecycle use neutral contracts. Isolate the
  current Asterisk taskprocessor behind a control-path adapter (ADR 0038),
  preserving ordering, admission/failure behavior, and reload/unload safety.
  Other Asterisk-specific thread handling stays in its integration adapter.
- `librptadvradio` is a separately versioned repository and Debian package,
  with a published ABI consumed by USBRadioPlus and `app_rpt_advanced`.
- PortAudio/ALSA remains an audio-adapter dependency, not a `librptadvradio`
  dependency. OSS and `res_usbradio` are removed under ADR 0028.
- Adapters own direct CM119 mixer, GPIO/PTT/COR, EEPROM, and device-selection
  I/O. `librptadvradio` receives those services through a platform-neutral
  callback contract.
- `app_rpt_advanced` links `librptadvradio` and the PortAudio adapter. The
  adapter alone links `libportaudio2` and ALSA.
- The direct PortAudio/ALSA CM119 adapter coexists as a selectable per-node
  path with the Asterisk adapters. Configuration rejects concurrent ownership
  of one device.
- Each configured radio owns one full-duplex PortAudio stream at 48 kHz.
  Higher native rates are unsupported under ADR 0035. RNNoise and native
  processing use that rate directly; inbound rings own source conversion and
  independent-clock recovery, while outbound codec conversion stays at egress.
- Its CM119 PortAudio source part uses ALSA with the device's
  `defaultLowInputLatency` and `defaultLowOutputLatency` values.
- The real-time callbacks delegate to the shared library's separate receive
  and transmit workers under the pending ADR 0027 amendment.
- A verified shared-clock adapter may invoke those workers back-to-back in one
  full-duplex callback with a local-ring target reserve equal only to configured
  squelch delay under ADR 0027.

### Independently versioned radio components

**Requirements**

- Split discrete `librptadvradio` functional components into independently
  versioned shared objects, each maintained in its own repository and released
  like `rate_adjusting_pcm_ring`.
- Candidate component boundaries include squelch, CTCSS detection and
  generation, DCS detection and generation, parallel-port and GPIO control,
  and audio-device control.
- Keep a component that is already working correctly isolated from unrelated
  fixes or features in another radio function.
- Retain `librptadvradio` as the radio-core integration layer; it consumes the
  component shared objects rather than duplicating their implementations.

**Decisions recorded**

- Each extracted functional component has its own source repository, versioned
  shared-library ABI, and release process.
- Every extracted shared library—including `rate_adjusting_pcm_ring` and all
  future separations—ships as a separate Debian runtime package and
  development package with a normal SONAME compatibility policy. Consumers
  declare a minimum compatible ABI rather than pinning an exact build.
- The component split is intended to constrain change scope and regression
  risk; it does not add features or alter radio behavior.
- The established rule remains in force: OSS and PortAudio stay outside
  `librptadvradio` and its portable signal-processing components.
- CTCSS detection and generation form one independently versioned CTCSS
  component; DCS detection and generation form one independently versioned
  DCS component.
- Audio-device control is a platform-neutral control contract. PortAudio and
  OSS remain adapter dependencies.
- GPIO and parallel-port components implement signaling semantics with
  adapter-supplied pin I/O, rather than direct Linux device access.
- Extraction order is squelch, CTCSS, DCS, then GPIO/parallel-port control.
  Audio-device control remains deferred until its boundary is defined.

### Independently versioned controller components

**Requirements**

- Split discrete `app_rpt_advanced` functional components into independently
  versioned shared objects, each maintained in its own repository and released
  under the established shared-library policy.
- Candidate component boundaries include scheduling, Morse generation, general
  tone generation, DTMF decoding and generation, and message templating.
- Keep controller integration and node-specific policy in `app_rpt_advanced`;
  extracted components provide focused, reusable behavior without duplicating
  controller state.
- Preserve behavior while extracting components. The purpose is to isolate
  stable functions from unrelated feature and defect work.
- Existing lock-free audio-operation and reload requirements apply to every
  extracted component used from a real-time audio path.

**Decisions recorded**

- The repository, ABI, SONAME, Debian runtime/development-package, and
  compatible-minimum dependency rules for extracted radio components apply to
  every extracted controller component as well.
- Morse uses the generic tone-generator component for waveform production; the
  Morse component owns timing and character encoding.
- DTMF detection and generation share one DTMF component because they use the
  same digit, timing, and level definitions.
- The scheduler component provides generic recurrence and due-event
  calculation only. `app_rpt_advanced` retains link, announcement, warning,
  and macro policy.
- Message templating parses and expands a supplied key/value context only.
  `app_rpt_advanced` defines available substitutions and access-sensitive
  values.
- Extraction order is tone generation, Morse, DTMF, message templating, then
  scheduling.

### Rust-owned implementation migration

**Requirements**

- Migrate all substantive owned implementation in USBRadioPlus,
  `rate_adjusting_pcm_ring`, `librptadvradio`, rpt_advanced, and future
  extracted components to Rust.
- Preserve stable C ABI entry points and shared-library SONAME compatibility
  where existing adapters or released packages consume them.
- Use Rust FFI for external C APIs, including Asterisk, PortAudio, ALSA,
  FFmpeg, Hamlib, and Piper, only through versioned adapter shared objects.
- Do not retain duplicate C implementations of controller, radio, audio, or
  policy logic after a component is migrated.

**Decisions recorded**

- Deliberately separated private rpt_advanced components are Rust `dylib`s.
  Their Rust ABI is private to project-owned Rust callers built with compatible
  pinned Rust inputs.
- Every Rust--C boundary is a separate versioned Rust `dylib` adapter shared
  object with only the smallest required stable C-compatible
  descriptor/function-table interface. A tiny C Asterisk loader is permitted
  when macro-generated module metadata or loader ABI requires it; an
  equivalent adapter may serve a PortAudio callback when necessary. The
  adapter forwards to Rust and contains no substantive application logic.
- Internal Rust components use adapter-neutral ports and do not expose external
  C types or adapter-specific policy. An adapter can be removed without
  modifying internal Rust component code.
- Every outbound call to an external C implementation uses its own removable,
  versioned adapter shared object. FFmpeg graph, sample-rate conversion,
  Hamlib, speech synthesis, PortAudio/ALSA, control-path execution, and similar
  dependencies are not imported by internal Rust components or combined into
  one aggregate adapter.
  The Asterisk entry adapter is independently versioned under the same
  capability-per-adapter rule. The generic speech adapter hides whether Piper
  uses a library or a subprocess.
- Each selected product composition has a complete required-adapter manifest.
  Startup and reload reject a missing or ABI-incompatible listed adapter rather
  than offering a reduced feature set or direct fallback. A standalone
  composition does not list the Asterisk entry adapter or an Asterisk-backed
  control-path adapter.
- Adapter replacement occurs only during a controlled module reload or process
  restart after all related callbacks and contexts stop. Runtime hot
  replacement and code loading or unloading from a real-time tick are not
  supported.
- Every real-time-capable adapter separates setup/control from a preallocated,
  lock-free tick that never allocates, blocks, logs, runs a process, loads code,
  or takes a lock.
- Standalone binaries have no project C implementation or Asterisk shim.
- Rust audio ticks preserve the lock-free real-time restrictions and never
  allow a panic to cross an FFI boundary.
- Rust formatting, Clippy, Rustdoc, coverage, and the existing native Debian
  matrix become part of the component quality gate during migration.
- Every external/system dependency and separately released project component
  is dynamically linked and packaged. Rust leaf implementation crates that are
  not deliberately separated components may compile into their owning artifact.

### Standalone lock-free controller

**Requirements**

- Run rpt_advanced as a standalone application with no dependency on Asterisk.
- Keep all I/O and inter-thread communication lock-free.
- In standalone operation, avoid locks, mutexes, spinlocks, and equivalent
  contention gates wherever thread-safe alternatives exist. Scale ingress to
  hundreds or thousands of peers without a thread per peer; use ADR 0037's
  bounded SPSC packet fan-in to a peer's sole media/PCM-ring producer.
- Implement enough IAX2 to interoperate with current AllStarLink nodes.
- Keep the standalone application small, fast, and suitable for inexpensive
  hardware.

**Decisions recorded**

- Standalone operation has no Asterisk dependency.
- Both module and standalone controller execution use neutral worker and
  lifecycle contracts. The replaceable control-path adapter currently uses
  Asterisk's taskprocessor; standalone selects a non-Asterisk implementation
  with identical functional semantics (ADRs 0036/0038).
- The control-path adapter owns only taskprocessor submission, serialized FIFO
  execution, backend resources, and safe stop/drain. Scheduling, task meaning,
  generation checks, and node policy remain in the controller. A later owned
  or third-party backend must pass the same execution/lifecycle contract tests;
  no replacement taskprocessor is required now.
- I/O and inter-thread communication are lock-free at native media boundaries.
  Standalone packet producers never share a PCM-ring writer: each uses its
  own bounded SPSC queue to the peer's assigned media owner. That owner alone
  advances jitter/decoder state and writes PCM. A full queue rejects new
  packets with observable drops; a stalled producer cannot block ready queues.
  The current Asterisk module is outside this new standalone-only requirement
  and retains its existing narrow ingress-mutex exception (ADR 0037).
- IAX2 interoperability targets AllStarLink.
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
