# Shared radio parity for USBRadioPlus and standalone

Status: Approved by the user on 2026-11-03, including the local Unix-domain
tuner transport. This document does not authorize deployment.

## Goal and scope

Close the seven gaps found in the 2026-11-03 code audit. USBRadioPlus and
standalone use the same radio configuration, hardware operations, calibration
workflows, processing profiles, and diagnostic measurements. Move existing
implementations to their shared owner and delete replaced implementations.

Do not implement REST, WebSocket, remote-base/Hamlib, additional link protocols,
or other wishlist requirements. Do not restore hardware repeat or native echo.
The controller retains its receive delay line, DTMF muting, mixing, and parrot.

## Approach

The user selected shared implementation rather than two implementations with
matching behavior. Keep the existing dynamically loaded
`libusbradioplus_product.so.1` as the radio composition owner. It already owns
the station, radio core, PortAudio/ALSA and GPIO adapters, hardware service,
processing resolver, and prepared updates. No new helper shared object or
repository is justified for this work.

The ASL3 adapter and standalone adapter translate their host contracts and
forward operations. Neither duplicates hardware selection, gain conversion,
EEPROM coding, radio configuration construction, calibration loops, or DSP.
All existing project shared-library boundaries remain dynamic.

## Capabilities and ownership

| Gap | Shared implementation and adapter work |
| --- | --- |
| Live tuner and calibration | Keep one `TunerApp` and its existing calibration algorithms in USBRadioPlus. Separate composition-specific configuration destinations and runtime transport from the menus and algorithms. Add a standalone backend to that same utility. |
| EEPROM tuning | Reuse `EepromTuning`, `HardwarePreflight`, and the bounded hardware service. Resolve enabled startup tuning before preparing immutable radio/DSP state. Reuse existing read, write, validation, save, and failure behavior. |
| Parallel hardware | Use the existing station hardware-plan builder and GPIO adapter for configured parallel inputs, outputs, and existing controls. Remove native's forced `parallel: None`. No transport is opened when unconfigured. |
| CTCSS translation | Use the shared station's receive-indexed receive/transmit tone mapping and default-tone rules. Remove the controller's identity-only mapping construction. |
| CPU saver | Expose the existing independent receive/transmit CPU-saver settings and pass them through the shared resolver and radio core. |
| Named profiles | Preserve explicit named-profile selectors and their referenced sections. Use USBRadioPlus's existing resolver, inheritance, defaults, and validation; do not add a second processing parser. |
| Native diagnostics | Expose the existing physical measurements and processing observations through the shared product. Standalone forwards them alongside its own controller PCM/ring statistics. Do not create another meter or counter implementation. |

## Shared configuration and defaults

Move radio-setting interpretation and native radio configuration construction
out of the controller's duplicated configuration code and
`rust/product/src/radio_configuration.rs`. Reuse the USBRadioPlus station's
configuration model and builders. The controller retains its node policy and
passes applicable radio/processing text and node identity to the selected
shared provider. Composition-specific key names are aliases/destinations in a
thin configuration adapter, not another default or validation implementation.

Provide a side-effect-free shared resolution operation. It accepts configuration
text and node identity, validates without opening hardware, and visits a
resolved request whose borrowed spans the consumer copies before return.
Reuse existing configuration-span and visitor contracts where possible.

Preserve existing composition defaults explicitly in the shared owner. In
particular, standalone keeps exact device selection, mono playback, Voice/Tone
output assignments, pre-emphasis off, and EEPROM disabled unless configured.
USBRadioPlus keeps its existing defaults. This refactor does not silently
replace one composition's defaults with the other's.

Enabling EEPROM is an explicit operator action. As in USBRadioPlus, a valid
enabled tuning image overrides capture gain, output A/B gains, transmit CTCSS
level and squelch threshold before immutable preparation. Other file settings
remain unchanged. Disabled EEPROM or an invalid/unreadable image leaves the
file's tuning in effect. Startup never writes EEPROM. Tests must prove this
precedence and preserve existing invalid/unreadable-image handling.

## Maintenance and diagnostics boundary

Factor the physical part of existing `channel_get_status` and `channel_command`
into a shared station maintenance owner. Add native forwarding operations to
the existing USBRadioPlus descriptor, retaining its complete published prefix.
Prefer sized descriptor-tail additions; change a public ABI/SONAME only when
the existing extension points cannot safely represent the operation.

Reuse existing typed mixer get/set, test tone, EEPROM read/write/save, CTCSS
inhibit, subaudible override, keying and measurement semantics. Exclude Asterisk
echo, DTMF transport and Asterisk frame handoff from the native physical record.
Keep the shared link processor's observation operation so standalone can expose
processed, bypassed and failed-block counts.

Control operations run through existing serialized control/hardware owners.
Callbacks read snapshots and adopt prepared state at their existing boundaries.
They do not perform device I/O, allocation, locking, config parsing, socket I/O,
or request handling. Factor and reuse the existing prepared forward/reverse
update transaction and RX/TX acknowledgments; do not copy it into native mode.
Ordinary supported tuner edits must preserve the stream and PTT state.

## Local tuner transport

The existing tuner is a separate process. An in-process shared-library ABI
cannot reach a radio handle owned by the running standalone service, and there
is currently no standalone operator IPC endpoint.

Add a narrow local Unix-domain control adapter, defaulting to
`/run/rpt-advanced/radio-control.sock`. The service owns the socket with mode
0600, so the service account and root can operate it; existing sudo-based tuner
operation continues to work. Do not open a TCP port or implement remote access.

The adapter transports only the typed radio-maintenance, snapshot, selected
configuration/edit-validation and apply operations needed by the existing
tuner. Use bounded, versioned requests and the existing control executor;
reject malformed, oversized, stale, unknown and saturated requests explicitly.
The owning service controls the configuration path; clients cannot ask it to
execute shell commands or load arbitrary files.

One tuner frontend supports its existing Asterisk runtime backend and the new
standalone backend. Keep the configuration editor's atomic writes, validation,
restore point, rollback, calibration bounds and reference targets. Standalone
edits must target `rpt_advanced.conf` and its correct node/radio/profile scope.
Make the utility installable without requiring the Asterisk channel adapter.

Temporary maintenance keying and test tones belong to the connected session.
Client disconnect, cancellation, failure, reload or service shutdown clears
that session's temporary intent without clearing ordinary controller intent.
Use and test the shared native PTT/CTCSS merge rather than writing unrelated
hardware bits. This prevents an abandoned tuning session from holding PTT.

## Implementation sequence and evidence

1. Add failing shared configuration and parity tests. Move radio/session and
   hardware-plan resolution, support named profiles and CPU savers, and remove
   the duplicated controller builders.
2. Expose shared native physical snapshots and maintenance operations. Reuse
   existing EEPROM/preflight and prepared-update code, with single-device
   ownership and rollback tests.
3. Adapt the existing tuner configuration port and add the local transport and
   native backend. Run the same calibration scenarios through both backends.
4. Complete targeted source-component coverage and developer documentation.
   Update the relevant ADRs and package/SDK admission checks together.
5. Run fast preflight before pushing. Use the full hosted Debian 13 amd64/arm64
   gate at the end, with production-only 100% line/branch coverage on amd64.

Keep focused test, coverage and documentation closure records in ignored
`.work/quality-progress.md`. Do not repeat closed component reports. Reuse the
current prebuilt `latest` quality image through the maintained labeled
container launcher and clean up task containers.

Changes to released dependency packages, PR merges, releases and node
activation are separate steps after a concrete validated candidate exists.
This task does not restart, reconfigure, connect, transmit on, or install to
node 524950 without authorization for that step.
