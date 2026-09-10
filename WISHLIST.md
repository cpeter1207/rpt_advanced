# rpt_advanced wishlist

This file tracks requested work that has not yet been implemented.

For each new requirement, record the requirement, any material decisions made
for it, and unresolved decisions needed before implementation. Ask for the
unresolved material decisions before starting the work. Remove the entry when
the requirement is implemented.

## Entries

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

### Persistent, fallback, and scheduled links

**Requirements**

- Allow a node to declare permanent direct links that are connected at module
  startup and reconnected according to the normal reconnect policy.
- Allow a primary permanent link to name an ordered set of fallback links. A
  fallback is attempted when the primary link cannot be reconnected.
- Allow a node to schedule connections to specific links at specified times for
  scheduled nets. Scheduled connections persist across Asterisk restarts.

**Decisions recorded**

- Permanent links are expected to connect automatically at startup.
- Scheduled links must survive Asterisk restarts.
- Fallback links are used only when the primary link cannot be reconnected.
- Schedules use a human-readable local-time format with start and end times,
  specific calendar dates, and/or days of the week; they do not use cron.
- A scheduled connection supports either an automatic end-of-window disconnect
  or a configurable inactivity-based disconnect.
- When a primary becomes available, disconnect its active fallback before
  reconnecting the primary to avoid network topology loops.
- `*806` disconnects all permanent links so any permitted peer can be linked
  manually. `*816` disconnects and then reconnects all permanent links.
- At a scheduled event start, configuration may choose to disconnect all
  links, temporary links only, permanent links only, or no existing links
  before connecting the scheduled peer. Existing topology-loop prevention
  handles all other overlap cases.
- Only local-receiver activity or received activity from any linked peer resets
  an inactivity disconnect timer. Telemetry, identifiers, announcements, and
  other transmitter-only activity do not.
- Any number of scheduled events may overlap. Existing topology-loop
  prevention resolves connection conflicts.
- For an inactivity-based scheduled connection, the end time ends the schedule
  but is not a hard disconnect. The connection remains until its configured
  inactivity period elapses, preventing an active net or post-net conversation
  from being interrupted.

**Material decisions needed before implementation**

None.

### Scheduled-event warnings

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
- Provide a WebSocket streaming API for status changes, including audio-level
  data at a cadence suitable for real-time meter displays.
- Use the CLI, DTMF, REST, and WebSocket interfaces as the stable control-plane
  foundation for user-interface development. No supported function or status
  may be exclusive to one interface.
- When DTMF requests status, send the resulting status over the air as speech
  with Morse fallback after the originating source unkeys, when that source has
  an unkey state.

**Decisions recorded**

- All four interfaces must expose equivalent supported operations and status.
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

**Material decisions needed before implementation**

None.

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

- Provide a DTMF pad test that reads decoded digits through default speech with
  Morse fallback.
- Permit configurable DTMF regeneration to connected peers.
- Support autopatch, reverse patch, and paging through an external IAX or SIP
  connection, with a more user-friendly interface than ASL3.
- Preserve the long-term option to replace Asterisk with a lock-free native
  IAX2 implementation. If that direction is chosen, support IAX2 or SIP
  connection to a self-hosted Asterisk instance or third-party VoIP provider,
  including Google Voice.

**Decisions recorded**

- Pad-test results use default speech with Morse fallback.
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

### Standalone lock-free controller

**Requirements**

- Run rpt_advanced as a standalone application with no dependency on Asterisk.
- Keep all I/O and inter-thread communication lock-free.
- Implement enough IAX2 to interoperate with current AllStarLink nodes.
- Keep the standalone application small, fast, and suitable for inexpensive
  hardware.

**Decisions recorded**

- Standalone operation has no Asterisk dependency.
- I/O and inter-thread communication are lock-free.
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
- Standalone audio I/O uses `libportaudio2`.
- IAX2 interoperability is limited to current ASL3 parity; no additional NAT
  traversal is required. Support ASL's current HTTPS registration mechanism,
  not retired IAX registration.
- The eventual appliance target is a 1U rack controller with an inexpensive ARM
  processor, 1--2 GB RAM, 64 GB SSD, Wi-Fi, Ethernet, USB-C, a status LCD,
  D-sub GPIO, and one to three CM119 radio ports. Detailed hardware design is
  out of scope and may change.
- Standalone rpt_advanced runs as a non-root Linux service managed by
  `systemctl`. It is upgraded as a Debian package through `apt`.
- Logging uses `/var/log` with log rotation when local resources permit, or an
  external logging service otherwise.
- REST/WebSocket administration is separate from SSH/Linux accounts. SSH is
  used for terminal administration; REST/WebSocket access permits delegated
  administration without Linux login access.
- ASL3 interoperability is defined by the current official ASL3 registration,
  IAX2, and application behavior and is verified through compatibility tests.
- Standalone CM119 devices are selected by stable Linux device identity (such
  as udev serial-based identity) and mapped one-to-one to configured nodes.
  Ambiguous or unavailable audio devices fail closed.
- USBRadioPlus and rpt_advanced Debian packages use exact matching versions to
  preserve their lockstep shared-object ABI.
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
