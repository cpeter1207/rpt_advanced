# Site monitoring, message macros, and tail messages

Date: 2026-10-17
Status: Design approved 2026-10-17; native implementation plans approved
2026-10-18. M1 receive interruption was implemented and operator-confirmed on
524950. Composed messages, tails, and site monitoring remain in progress/planned.

Implementation plans:

- [Message composition and tail playback](../plans/2026-10-17-message-composition-and-tail-playback.md)
- [Site monitoring and actions](../plans/2026-10-17-site-monitoring-and-actions.md)

## Outcome and scope

Provide hardware-neutral site sensors, calibrated meters, digital inputs and
outputs, fan control, alarms, and actions comparable to common repeater
controllers. Users need no particular interface board. Support the capabilities
their hardware actually exposes, without a fixed software port or sensor count.

The owner approved the feature scope and failure policy, then required
file-configured sensors/meters and internationalized message macros. DTMF may
query a sensor or meter and play a message macro; it cannot configure them.
Message macros may also supply identifier and tail-message content.
Owner revision: identifiers and tails may combine ordinary text or a sound
file with multiple message macros in one ordered sequence. Message macros may
include dynamic station/site values and attention tones, not just one spoken
message selected in place of all other media.
Incoming local-receiver or linked-peer activity requires Morse-only message
playback. An interrupted voiced message stops and switches to its complete
Morse form; this rule applies to message macros and tails as well as IDs.

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
cannot execute a shell or recursively invoke action macros. Message composition
is read-only and follows the bounded, cycle-free rules below. Execute
trip/clear actions once per transition in configured order; repeating an alarm
notification does not repeat its equipment-control actions.

Acknowledgment silences repeats without declaring recovery. A latched alarm
requires the condition to clear and an explicit reset. Reset cannot remove a
physical interlock fault or override an unsafe current condition. Protective
actions do not wait for telemetry queue admission, speech, or clear RF.
Unsolicited alarm messages are local-only and follow ordinary non-ID telemetry
admission rules. Once admitted, they follow the receive-interruption policy
below; command replies retain source-scoped routing.

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

A message macro is a named, read-only content sequence. It has `content`, an
optional Fluent `message_id`, `arg_<name>` bindings, optional media/tone fields,
and an optional `dtmf_id`. A simple catalog-based macro defaults to
`content = {message}`. No action or hardware operation belongs in a message
macro.

Identifiers, tails, and message macros share one ordered `content` grammar:

| Content | Meaning |
| --- | --- |
| Ordinary text | Speak that text, with Morse fallback |
| `{message_macro_name}` | Insert the configured message macro named `name` at this exact position |
| `{message}` | Insert this section's Fluent `message_id`, using its bound values and independent text/TTS/Morse forms |
| `{media}` | Insert this section's existing sound-file, speech-text, and Morse fallback choice as one segment |
| `{tone_sequence}` | Play this section's configured tone sequence here; repeat the token wherever that sequence should play |

Placeholders are structural tokens in the configuration's `content` field,
not Fluent syntax or an expression language. Literal braces use `{{` and `}}`.
Macro names use the existing section-label rules; the `message_macro_` prefix
distinguishes references from other tokens. Expand references at their written
positions, not as an implicit prefix, suffix, or replacement for the whole ID.
All fields of a selected node/global macro resolve under the invoking node.
One macro may reuse another, but references must form an acyclic graph with a
checked finite expansion. No recursion, loops, executable expressions, or
runtime-selected macro names. Values returned by a sensor or translation are
data and are never reparsed as content tokens.

Bindings resolve at reload to an allowlisted read-only value source:

| Binding source | Value |
| --- | --- |
| `callsign`, `node` | Configured station callsign and node number |
| `date`, `time`, `day_of_week`, `greeting` | Local civil date/time and localized forms, honoring the node's 12/24-hour choice |
| `link_status` | The controller's localized current link-status summary |
| `sensor:name` | Configured sensor/meter reading with unit, precision, quality, and observation time |
| `input:name` | Configured digital input state, quality, and observation time |

Existing context-specific values such as `time_remaining` remain available only
where that context supplies them. Bindings cannot evaluate code or select
arbitrary runtime fields. Resolve one coherent snapshot at preparation for a
playback attempt, not at configuration load. Multiple references in that
attempt use the same snapshot. A deferral before the first sample or later
rescheduling of a cancelled message creates a new attempt with a fresh snapshot.
Switching an active message to Morse, or continuing it after priority/safety
preemption, retains that attempt's snapshot. Apply source freshness checks before use;
if a referenced reading is not valid, replace only its measurement segment with
the localized unavailable form, not the entire ID or alarm sequence.

Use the existing audience-specific identity formatting: speech identifies node
numbers as nodes and reads their digits separately; callsigns receive character
separation. Numeric measurements and dates/times retain natural-language
formatting rather than being treated as node numbers. Units, labels, and
unavailable states are localized. Resolve each binding for its selected text,
TTS, or Morse form; do not reuse an already Morse-formatted time or status
string as spoken text.

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
content = {message}
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
configured IDs and bindings as well as built-in IDs. Translatable wording lives
in Fluent; literal configuration text is an operator-supplied alternative.
Preserve the existing bounded per-form message contract per rendered segment;
do not force an entire composed ID into one 127-byte message or silently
truncate translations. Validate expansion size and preparation resources on
control before admitting playback: count expanded segments and bytes with
checked arithmetic before materializing them. Adjacent text segments may be prepared as
one utterance within those bounds, without introducing an artificial pause at
every macro boundary. Files and tones preserve their explicit sequence points.

Attention tones reuse the existing courtesy `tone_sequence` grammar and shared
tone generator. They may appear before a message, between messages, or after
all messages, and may occur more than once in a macro. They are ordinary
ordered segments, not a prefix-only option. For example, this alarm plays two
attention bursts, then its localized speech or Morse message:

```ini
[message temperature_alarm]
content = {tone_sequence} {message}
tone_sequence = 1000Hz / 200ms / -20dB, silence / 100ms, 1000Hz / 200ms / -20dB
message_id = site-temperature-alarm
arg_temperature = sensor:pa_temperature
```

Here the tones precede the message because of their position in `content`,
not because they are always a prefix. A reusable tone-only macro can place
attention tones anywhere in a larger sequence:

```ini
[message attention]
content = {tone_sequence}
tone_sequence = 1000Hz / 200ms / -20dB, silence / 100ms, 1000Hz / 200ms / -20dB

[message site_report]
content = {message_macro_attention} {message_macro_time} {message_macro_attention} {message_macro_temperature} {message_macro_attention}
```

This plays attention, time, attention, temperature, then attention. Different
tone-only macros can supply different attention sequences without adding a
second tone-reference syntax. A tone-only macro needs no `message_id`, speech,
or Morse text. Literal text and `{media}` segments can be interleaved as well.
Whitespace separating structural tokens does not generate spoken content.
Tones retain their position when speech is unavailable while the station is
idle. The receive-driven Morse-only rule below suppresses attention tones.
Reusing the current media fallback field alone would not satisfy this rule.
Tone level/duration remain explicit and subject to existing validation and
transmit processing. Message macros may also insert sound files with `{media}`
and existing file/speech/Morse fallback fields; no separate file decoder or
tone renderer is added.

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

Identifiers and named tails use `content` to combine text, files, and any number
of configured message-macro references, subject to validated resources. Their
default `content = {media}` preserves the ordinary file/speech/Morse media
choice. Setting `content` specifies ordering; it does not forbid ordinary media
fields or replace them with an exclusive `message_macro` setting.

For example, a spoken ID can contain the owner's requested composition:

```ini
[identifier 524950 routine]
interval_ms = 600000
polite = yes
polite_maximum_wait_ms = 60000
content = This is the KG0BP repeater. {message_macro_time} {message_macro_temperature} {message_macro_link_status}

[message time]
message_id = site-time
arg_time = time

[message temperature]
message_id = site-temperature
arg_temperature = sensor:pa_temperature

[message link_status]
message_id = site-link-status
arg_links = link_status

; A recorded ID can precede the same multiple message macros instead.
[identifier 524950 recorded]
sound_file = /var/lib/rpt-advanced/sounds/kg0bp-id.wav
speech_text = This is the KG0BP repeater.
morse_text = DE KG0BP
content = {media} {message_macro_time} {message_macro_temperature} {message_macro_link_status}

[tail]
delay_ms = 0
interval_ms = 0

[tail 524950 battery]
content = {message_macro_battery_report}
interval_ms = 1800000
delay_ms = 250
```

The two ID sections illustrate alternative compositions, not a recommendation
to enable both. Referenced catalog messages, sensors, and files must be supplied
for an enabled entry. A translatable opening can replace the literal opening
with another catalog-backed message macro using `arg_callsign = callsign` and,
if wanted, `arg_node = node`. A sensor report alone is not identification.
All delay/interval fields use integer milliseconds. Media levels continue to
inherit the existing node/global speech, Morse, and tone settings.

The entire expanded sequence is one telemetry item and keeps its caller's
identity, priority, deadline, route, cancellation, and completion. Do not mark
an ID satisfied or a tail interval elapsed after only its first macro. Do not
release PTT, add hangtime, or allow unrelated telemetry between its segments.
All file decoding, TTS, tone, and Morse production stays outside the transmit
callback and streams through the existing telemetry ring.

For ordinary speech failure, use each segment's Morse fallback in order; a
file segment uses its configured fallback without losing following macros.
An unavailable meter does not discard the station-identification segment.

### Receive activity and Morse-only playback

Use the same rule for identifiers, tails, announcements, alarms, command
replies, and message-macro compositions. Receive activity means qualified
local-receiver activity OR an incoming transmission from any connected peer,
not just the message's originating peer. Do not infer it from local PTT,
hangtime, telemetry playback, or an audio-level threshold.

- Check current receive activity before admitting the first non-Morse sample
  and throughout playback, including between segments. A message prepared
  while idle must not start speaking if receive became active during preparation.
- If an otherwise eligible message starts while receive is active, select its
  complete Morse form immediately. Existing scheduling and source-unkey rules
  still determine when a queued message becomes eligible; this is not permission
  to start every pending message during a transmission.
- If receive becomes active during speech, a sound file, or an attention-tone
  segment, stop that primary sequence and begin the complete Morse form from
  the beginning. Preserve its destination and coherent value snapshot. Never
  restart at a partial word or lose the station identity from an interrupted ID.
- Latch Morse for the remainder of that message. An unkey, another peer keying,
  or a segment boundary must not restore speech or repeatedly restart Morse.
  Preserve this choice across an ID-priority preemption or safety deferral.
- Literal text is Morse-rendered, catalog macros use their `.morse` forms, and
  each file/media segment uses its configured Morse fallback. Omit attention
  tones and recorded file audio in this receive-driven mode. Embedded tone-only
  macros contribute no Morse; a wholly tone-only macro has nothing to play in
  this mode and must not keep PTT asserted or satisfy an ID.
- Require a usable complete Morse fallback for voiced compositions. A missing
  fallback follows warning/default resolution and safely disables the affected
  content if no valid fallback exists. Never continue voice during receive or
  report a failed/interrupted primary sequence as successfully completed.

The interruption must suppress already-buffered primary PCM, not merely stop
future synthesis. At the next output boundary that observes receive activity,
gate the old non-Morse stream and invalidate its unplayed samples, including
resampler/PLC history, using the existing generation-safe cancellation handoff.
Do not drain old speech audibly, conceal it into the Morse transition, or admit
late producer writes from the cancelled stream. The telemetry producer prepares
and streams Morse off audio; transmit may wait silently for available Morse
samples but never blocks, allocates, or synthesizes in its callback. Samples
already submitted to hardware cannot be recalled.

This changes playback selection, not routing or safety. A half-duplex node or
a reply whose originating source has rekeyed may have to hold its Morse form
until transmission is allowed. Timeout/interlock rules still win. Existing
tone-only courtesy cancellation and DTMF regeneration are unchanged. Only
completion of the selected message form advances its ID/tail completion state.

`interval_ms = 0` makes a tail eligible after every qualifying traffic release.
A positive interval is minimum elapsed time since successful playback; it
becomes eligible at a later traffic release. It does not key periodically from
prolonged idle. Existing periodic announcements retain that separate behavior.
Tail playback, IDs, and other controller-generated telemetry cannot recursively
trigger another traffic-release tail. A release occurs after all qualifying
local/linked traffic ends, not while another input remains active.

### Approved timing and precedence

1. Normal polite IDs and tails wait for the existing transmitter hang timer to
   expire. Ordinary courtesy behavior and command-response admission/routing
   remain unchanged; admitted voiced messages use the Morse-only policy above.
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
   send the ID, then restart that already-admitted tail from its beginning when
   transmission is allowed. Preserve its snapshot and latched Morse choice;
   ongoing receive activity requires Morse, not a new wait for quiet.
   Never overlap telemetry or resume a spoken message midway through a word.
8. New receive activity cancels an outstanding release/delay opportunity for a
   tail that has not started. Reconsider it after the next ordinary hang expiry.
   An already-playing voiced tail instead stops and switches to its complete
   Morse form under the policy above; do not defer it merely because receive
   became active. Only completed playback advances a tail interval.
9. Process multiple eligible tails in configuration order, rechecking ID
   deadlines and receive state between them. Apply each named tail's delay
   without sleeping a worker; transmitter-generated releases create no new
   tail opportunity. A later tail cannot pass an earlier eligible one.

These rules do not bypass half-duplex constraints, transmitter timeout,
interlocks, or the requirement to use Morse when local or linked receive
activity interrupts a voiced message. All telemetry is generated off audio and
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
translation bounds, and reuse inside identifiers/tails. Test literal text plus
multiple macros, recorded media plus multiple macros, nested acyclic references,
cycle/missing-reference handling, brace escaping, coherent dynamic values,
per-segment bounds, natural measurement versus digit-wise node speech,
attention tones before, between, and after messages, repeated/tone-only macros,
tone ordering/levels through idle speech-to-Morse fallback, and complete
receive-driven Morse fallback. Test local-only, peer-only, and overlapping
receive activity before playback, during preparation, within each segment,
and at segment boundaries. Assert that buffered speech and stale producer
writes cannot leak after cancellation, Morse stays selected after unkey or
preemption, and destination/snapshot/identity remain intact. Cover tails that
are pending versus already playing, missing Morse, tone-only compositions,
source rekey, and half-duplex/interlock deferral. Verify that segment boundaries
do not unkey, interleave telemetry, or prematurely complete an ID/tail.
Exhaustively test zero and positive tail delay, intervals, no recursive tails,
receive interruption, all ID-deadline crossings above, and multiple named tails.
Assert PTT sequencing,
media order, no overlap, and no new audio-callback I/O/allocation/locks.

Follow the existing preflight and hosted Debian 13 native PR gates; production
coverage is required on amd64. Hardware qualification requires a representative
physical backend when available and is reported separately from software tests.
Do not change node 524950 without a separate installation/testing approval.

Before implementation, update ADR 0009's allowed triggers/substitutions and
ADR 0023's read-only site/message operations. Record the tail/ID timing and
all-message receive-interruption rules, including ADR 0025's buffered telemetry
cancellation contract. Track the reported voice-during-receive defect as
unverified until reproduced; this design change is not a deployed fix.
Update the architecture map and site-I/O wishlist to approved/planned,
not implemented. Preserve ADRs 0012, 0019, 0022, 0025-0028, and 0033/0034.
Update developer docs with code; update operator manuals/examples before a PR.
