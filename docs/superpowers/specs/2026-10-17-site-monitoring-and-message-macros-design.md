# Site monitoring, message macros, and tail messages

Date: 2026-10-17
Status: Written design for owner review; implementation has not started.

## Outcome and scope

Provide hardware-neutral site sensors, calibrated meters, digital inputs and
outputs, fan control, alarms, and actions comparable to common repeater
controllers. Users need no particular interface board. Support the capabilities
their hardware actually exposes, without a fixed software port or sensor count.

The owner approved the feature scope and failure policy, then required
file-configured sensors/meters and internationalized message macros. DTMF may
query a sensor or meter and play a message macro; it cannot configure them.
Message macros may also supply identifier and tail-message content.

This design does not authorize implementation, deployment, hardware changes,
link connections, a release, or the unrelated unfinished wishlist features.
Complete REST/WebSocket interfaces, historical measurement databases, remote
sensor protocols, and new per-chip userspace drivers are outside this slice.

## Research baseline

The design adopts useful capabilities rather than controller-specific pin
counts, electrical ratings, or numeric programming commands.

| Reference | Relevant capabilities |
| --- | --- |
| [ACC RC-850 hardware manual](https://www.repeater-builder.com/acc/pdfs/RC-850-Hardware-Ref-Man-V3.pdf) | Optional analog telemetry, spoken readings, and extrema |
| [S-COM 7K manual](https://manualzilla.com/doc/5823491/s-com-7k-owner-s-manual---the-henry-savage-ham-radio-club) | Input-edge macros, latched/momentary outputs, retriggerable timers, and PTT fan run-on |
| [CAT-250 manual](https://www.repeater-builder.com/cat-auto/pdfs/cat-250-manual.pdf) | Digital-input actions, on/off/pulsed outputs, macros, and fan run-on |
| [RLC-3 manual](https://mharc.ca/Documents/rlc3%20control%20manual.pdf) | Calibration, smoothing, threshold alarms, hysteresis, and recovery actions |
| [ARCOM RC210 manual](https://www.arcomcontrollers.com/documents/rc210/rc2108_09manual.pdf) | Two-point calibration, current/minimum/maximum values, analog alarms, digital edges, and fan timers |
| [NHRC-7 manual](https://www.repeater-builder.com/nhrc/manuals/current/nhrc-7-manual-20120108.pdf) | Latched/repeated alarms, steady/pulsed outputs, and fan run-on |

Explicit stale-data handling, output ownership, and failure behavior below are
our requirements, not claims that every reference controller provides them.

## Ownership and hardware support

- Shared controller policy owns calibration, alarm state, fan demand, action
  dispatch, message expansion, and tail/ID scheduling. Standalone and the
  optional Asterisk adapter share that implementation.
- Reuse the existing GPIO adapter for supported CM119/URI-family and parallel
  digital I/O. A device name does not imply a verified driver or pin mapping.
- Use Linux GPIO through a libgpiod adapter, hwmon through a libsensors adapter,
  and IIO for kernel-supported ADC measurements. External C dependencies remain
  behind separately versioned dynamic adapters under ADR 0022. Ordinary IIO
  operating-system access stays in its hardware owner, not a generic OS facade.
- Use stable device identity plus channel/line identity, not enumeration order.
  Bind only configured endpoints and require only their declared capabilities.
- Hardware I/O runs outside audio callbacks. Bounded backend work publishes
  immutable/atomic observations to station control; one slow backend cannot
  stall audio or unrelated nodes. Do not create a thread per sensor.
- The existing control owner serializes policy and actions. Audio sees only
  prepared media and existing bounded control intent. Do not add polling,
  logging, message expansion, device writes, locks, or allocation to audio.
- Each physical output has one owner. Reject ambiguous assignments; do not let
  site commands claim PTT, CTCSS, or interlock endpoints already owned by radio
  control. Shared read-only measurement access does not duplicate device owners.
- Preserve ADRs 0033/0034: appliance interlocks remain authoritative. Other
  interfaces need not emulate safety facilities they do not possess.

Linux reference interfaces: [hwmon](https://docs.kernel.org/hwmon/sysfs-interface.html),
[IIO](https://docs.kernel.org/driver-api/iio/core.html), and
[libgpiod](https://libgpiod.readthedocs.io/en/master/).

## Configuration model

Use `rpt_advanced.conf`. For `sensor`, `input`, `output`, `alarm`, `fan`, and
`tail`, `[kind]` supplies shared defaults, `[kind node]` supplies node defaults,
and `[kind node name]` defines a named instance. Defaults alone create no
hardware or playback. Inheritance is field-wise: named instance, node defaults,
shared defaults, then built-in safe defaults. Endpoint identities and DTMF IDs
belong to named instances, never wildcard defaults.

Messages use `[message name]` for a global definition and
`[message node name]` for field-wise node overrides. Keep existing action
`[macro ...]` and literal `[template ...]` meanings unchanged.

Sensor and input names share a node-local namespace. Output and fan ownership
is validated after inheritance. Resolve references and capabilities before
publishing a configuration generation; do not discover or allocate from audio.

### Sensors and meters

A meter is the presented value of a sensor, not a second acquisition pipeline.

| Fields | Meaning / defaults |
| --- | --- |
| `enabled` | `no`; enable only a configured named endpoint |
| `backend`, `device`, `channel` | Required source identity when enabled; supported values come from the selected adapter |
| `label_message_id`, `unit` | Localized label and engineering unit; unset label uses the stable name |
| `precision` | Decimal places for display/speech, default `1`; does not change alarm precision |
| `poll_interval_ms`, `stale_after_ms` | Defaults `1000` and `5000`; positive, with stale time greater than polling interval |
| `smoothing_ms` | Default `0` (none); optional elapsed-time exponential smoothing |
| `calibration` | `none` or `two_point`, default `none` |
| `input_low`, `value_low`, `input_high`, `value_high` | Two distinct finite source values and their engineering-unit values |
| `dtmf_id` | Optional explicit decimal query selector |

The adapter normalizes driver raw/scale/offset readings into documented source
units first. Calibration then maps those units into the configured engineering
unit; it does not apply a driver scale twice. No arbitrary formula evaluator.

Expose calibrated current and smoothed values, current quality, observation
time, and minimum/maximum values with times. Extrema use valid unsmoothed
calibrated readings. Keep extrema in memory until reset or restart; retain them
on an unchanged-source reload, reset them when source or calibration changes.
Log alarm/state transitions; do not add a time-series database.

Quality is `valid`, `stale`, `unavailable`, or `fault`. A failed read is never
zero and never refreshes the observation time. A retained last-good value may
appear in diagnostics with its age, but not as a current spoken reading.

### Digital inputs and outputs

Inputs specify `enabled`, `backend`, `device`, `line`, `active_level`
(`high`/`low`, default `high`), `debounce_ms` (default `50`), an optional
`dtmf_id`, and named `on_active` / `on_inactive` action lists. Input status
includes quality and time as well as state. These are site inputs, not a new
debounce policy for radio COR/CTCSS.

Outputs specify `enabled`, `backend`, `device`, `line`, `active_level`,
`startup_state`, `safe_state`, and `pulse_ms`. The built-in states are `off`;
default pulse duration is `500` ms. Supported operations are on, off, and a
timed pulse returning to the prior logical state. Pulse timers run on control,
never by sleeping an audio or station-control worker. Safe-state/fault handling
cancels pulses and overrides ordinary commands.

Manual output actions use existing administrative authorization. Requested,
successfully submitted, and physically observed state remain distinct; an
interface without readback cannot report a write as verified physical state.

### Alarms and actions

An alarm names a sensor/input and selects `above`, `below`, `active`, `inactive`,
or `unavailable`. Separate above/below alarms provide an outside-range alarm
without an expression language. Analog alarms specify `trip_value` and
`clear_value`; their ordering supplies explicit hysteresis. `reading` selects
`current` or `smoothed`, default `current`.

Other fields are `enabled` (default `no`), `trip_delay_ms` and `clear_delay_ms`
(default `0`), `latched` (default `no`), `repeat_interval_ms` (default `0`, no
repeat), `on_trip`, `on_clear`, and optional trip/clear/repeat message-macro
references. Conditions and delays use monotonic time. Unknown input does not
clear an active threshold alarm; it separately reports sensor health.

Action lists contain validated existing controller macro references and the
necessary typed site-output/fan/inhibit operations. They are not scripts and
cannot execute a shell or recursively invoke message/action macros. Execute
trip/clear actions once per transition in configured order; repeating an alarm
notification does not repeat its equipment-control actions.

Acknowledgment silences repeats without declaring recovery. A latched alarm
requires the condition to clear and an explicit reset. Reset cannot remove a
physical interlock fault or override an unsafe current condition. Protective
actions do not wait for telemetry queue admission, speech, or clear RF.
Unsolicited alarm speech is local-only and follows ordinary non-ID telemetry
activity rules; command replies retain source-scoped routing.

### Fans and failure behavior

A named fan claims one supported output and selects `ptt`, `temperature`, or
`combined`. PTT demand follows its configured local transmitter(s), with
`run_on_ms` (default `120000`). Temperature switching uses `on_temperature` and
`off_temperature`; a PWM-capable backend may instead use a configured list of
temperature/duty points. Combined demand uses the greater cooling request.
There is no userspace GPIO bit-banging for PWM.

Optional `rpm_sensor`, `minimum_rpm`, and `spinup_ms` provide fan-fault monitoring
only where tachometer feedback exists. An explicitly configured temperature
policy requires its thresholds/curve; there is no universal thermal threshold.
A temperature-sensor failure requests full cooling while output control is
available. Missing output control is a visible fault, not a claim of cooling.

Ordinary outputs attempt their configured safe state on failure or shutdown.
Hardware owns physical power-loss/disconnect safety. Transmitter inhibition
requires an explicitly configured protective action through the existing
controller/interlock mechanism. A fan, macro, or manual command cannot bypass it.

## Message macros and DTMF

Each message macro has a `message_id` from the Fluent catalog, optional
`arg_<name>` bindings, and optional `dtmf_id`. Bindings resolve at reload to
known read-only sources, such as `sensor:battery`, `input:door`, or an existing
node/time/callsign substitution. They cannot evaluate code or select arbitrary
runtime fields. Resolve one coherent snapshot for each playback request.
Provide explicit quality/age alongside measurement arguments; if a referenced
reading is not valid, play the localized unavailable form instead of its
numeric-reading form.

```ini
[general]
link_command_site_status = 76
link_command_message = 77

[sensor 524950 battery]
enabled = no
unit = volts
precision = 1
poll_interval_ms = 1000
stale_after_ms = 5000
dtmf_id = 1
; Enabling requires this installation's backend, device, and channel.

[message battery_report]
message_id = site-battery-report
arg_voltage = sensor:battery

[message 524950 battery_report]
dtmf_id = 1
```

The disabled example makes no hardware assignment. The catalog provides
separate text, TTS, and Morse forms:

```ftl
site-battery-report =
    .text = Battery voltage: { $voltage } volts
    .tts = Battery voltage is { $voltage } volts.
    .morse = BATTERY { $voltage } V
```

Ship English messages and allow additive administrator language bundles. Reuse
Fluent and its existing locale/per-attribute fallback, rather than hardcoding
wording in Rust or requiring a complete copy of the English catalog. Validate
configured IDs and bindings as well as built-in IDs. Preserve the existing
bounded per-form message contract; do not silently truncate translations.

DTMF `*761` queries sensor/input ID `1`; `*771` plays message macro ID `1`.
Use the existing prefix map, collection, unkey, terminator, and timeout rules.
Both are public read-only operations. No DTMF command configures sensors,
meters, message macros, or tails. IDs are explicit canonical positive decimal
strings within the existing command-length bound; absence means not DTMF
addressable. Sensor/input selectors share one namespace; message selectors use
another. Validate duplicate selectors and effective per-node prefix collisions.
There is no arbitrary sensor-count limit imposed by selector storage.

Selectors are not link destinations: query `1` must not change remembered link
state or inherit the link command's special `0` recall behavior. DTMF supplies
only a validated configured selector, never a Fluent ID or expression.

ADR 0023 routing remains authoritative: a local request answers locally after
unkey, a peer request answers only its exact originating connection after its
unkey, and CLI replies stay on the CLI. Identifier/tail invocation is local-only
regardless of a macro's DTMF exposure. No automatic alarm action is smuggled
into a read-only message macro.

## Identifiers and tail messages

Identifiers and named tails may reference `message_macro`. A referenced macro
supplies the existing media text/TTS/Morse forms; it does not replace identity,
priority, timing, speech/Morse fallback, or destination policy. Keep ordinary
configured sound-file support. A message reference replaces inline speech/Morse
content for that entry, avoiding two competing content sources; an explicitly
configured sound file remains the first choice under existing media precedence.

```ini
[identifier 524950 routine]
interval_ms = 600000
polite = yes
polite_maximum_wait_ms = 60000
message_macro = station_identification

[tail]
delay_ms = 0
interval_ms = 0

[tail 524950 battery]
message_macro = battery_report
interval_ms = 1800000
delay_ms = 250
```

`station_identification` must resolve to a configured message macro containing
the station's identification; this fragment illustrates the reference, not a
complete deployable station configuration. Sensor readings alone are not IDs.
All delay/interval fields use integer milliseconds. Named tails can also use
the same direct media fields and inherited levels as identifiers.

`interval_ms = 0` makes a tail eligible after every qualifying traffic release.
A positive interval is minimum elapsed time since successful playback; it
becomes eligible at a later traffic release. It does not key periodically from
prolonged idle. Existing periodic announcements retain that separate behavior.
Tail playback, IDs, and other controller-generated telemetry cannot recursively
trigger another traffic-release tail. A release occurs after all qualifying
local/linked traffic ends, not while another input remains active.

### Approved timing and precedence

1. Normal polite IDs and tails wait for the existing transmitter hang timer to
   expire. Ordinary courtesy and command-response rules remain unchanged.
2. With zero tail delay, keep PTT asserted at hang expiry and start the tail.
   A pending polite ID follows it unless its maximum wait has expired.
3. With positive tail delay, unkey at hang expiry and count the delay from that
   controller unkey edge. Rekey when it expires, then play the tail and a
   pending polite ID. Ordinary transmitter settle/release safeguards still
   apply; do not clip initial or final media samples.
4. A polite-ID maximum-wait deadline overrides both hangtime and tail delay.
   Play that due ID first; the tail follows when otherwise eligible. Never
   extend or reset an ID deadline to accommodate a tail.
5. An ID played during hangtime does not make the tail eligible early or
   restart the original hang deadline. At an expired-ID/hang-expiry tie, play
   the ID first. For a positive tail delay, then unkey and start that delay.
6. An ID during an already-running tail delay does not restart that delay. If
   the tail becomes ready during the ID, play it immediately after the ID;
   otherwise release PTT and wait for the original tail deadline.
7. An expiring ID deadline also wins during tail playback: interrupt the tail,
   send the ID, then restart the eligible tail from its beginning when quiet.
   Never overlap telemetry or resume a spoken message midway through a word.
8. New receive activity cancels an outstanding release/delay opportunity and
   interrupts a tail. Reconsider unfinished eligible tails after the next
   ordinary hang expiry. Only completed playback advances a tail interval.
9. Process multiple eligible tails in configuration order, rechecking ID
   deadlines and receive state between them. Apply each named tail's delay
   without sleeping a worker; transmitter-generated releases create no new
   tail opportunity. A later tail cannot pass an earlier eligible one.

These rules do not bypass half-duplex constraints, transmitter timeout,
interlocks, or the existing requirement to use Morse when receive activity
interrupts a spoken/file identifier. All telemetry is generated off audio and
streamed through the existing telemetry ring. Reuse announcement/identifier
scheduling and rendering, not a second tail-specific audio engine.

## Reload, safety, and diagnostics

Use ADR 0019 warning/default resolution for unknown fields or values. A missing
required endpoint, invalid calibration, unknown reference, or unsupported
capability resolves to an explicitly disabled affected feature where safe;
never choose a different pin, bypass a configured protection, or invent a
measurement. Retain the running generation and reject reload when no safe,
deterministic candidate exists, including conflicting physical output ownership.
Expose effective resolved configuration and all warnings.

Publish validated configuration generations under ADR 0026. Preserve unchanged
endpoint ownership and output state without gratuitous pulses during reload.
For an unchanged endpoint, transfer its active pulse's prior state and original
deadline to the new control owner; likewise retain an unchanged fan run-on
deadline. Do not extend a pulse or leave it asserted by canceling its timer.
Changed or removed endpoints complete the configured safe handoff before their
timers are canceled. Cancel stale-generation writes and messages; retire device
state only after its owner is quiescent. Required protection whose source is
lost becomes unavailable/faulted, not silently normal or disabled.

Expose reading age/quality, alarm condition/latch/acknowledgment, requested and
observed output state, fan demand/RPM/fault, next eligible tail time, and pending
ID deadline through the shared status model. Reuse available CLI/DTMF paths;
future REST/WebSocket adapters consume the same model without implementing
their unfinished transport/security features in this slice.

## Validation and architecture updates

Use deterministic fake clocks and backend fixtures to test conversion,
calibration, smoothing, extrema, debounce, hysteresis, persistence delays,
sensor loss, acknowledgment/reset, pulse cancellation, fan run-on and failure,
ownership conflicts, reload, and stale-generation rejection. Test actual
dynamic-library contracts without claiming simulated tests qualify wiring.

Test DTMF selectors and remapping, unchanged remembered link destinations,
source-scoped replies, additive locale catalogs, missing/stale readings,
translation bounds, and reuse inside identifiers/tails. Exhaustively test zero
and positive tail delay, intervals, no recursive tails, receive interruption,
all ID-deadline crossings above, and multiple named tails. Assert PTT sequencing,
media order, no overlap, and no new audio-callback I/O/allocation/locks.

Follow the existing preflight and hosted Debian 13 native PR gates; production
coverage is required on amd64. Hardware qualification requires a representative
physical backend when available and is reported separately from software tests.
Do not change node 524950 without a separate installation/testing approval.

Before implementation, update ADR 0009's allowed triggers/substitutions and
ADR 0023's read-only site/message operations, and record the tail/ID timing
decision. Update the architecture map and site-I/O wishlist to approved/planned,
not implemented. Preserve ADRs 0012, 0019, 0022, 0025-0028, and 0033/0034.
Update developer docs with code; update operator manuals/examples before a PR.
