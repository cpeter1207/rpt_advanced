# Local receiver S-meter

Status: Proposed written specification; conversational design approved on
2026-10-20. Includes the requested spoken DTMF query. Implementation and node
deployment have not started.

## Purpose and scope

Provide one signal-strength meter for each configured local receiver. Use
discriminator-noise DSP for suitable flat FM audio or a named external RSSI
sensor. Share the implementation between standalone and the Asterisk product.
Keep RF strength separate from audio peak/RMS, COR and CTCSS qualification.

Only calibrated observations may produce dBm and S-unit readings. Without RF
calibration, DSP may report relative quieting when its no-carrier noise
reference is configured; otherwise report an uncalibrated noise observation.
Do not manufacture an absolute reading from voice amplitude or squelch state.

This work does not implement voting, remote-base control, new sensor drivers,
REST/WebSocket transports, or other wishlist features. It does not change audio,
gain, squelch thresholds, receive qualification, routing, or transmitter policy.
Installing or changing a live node requires separate approval.

## Reuse and ownership

- The shared radio core already filters discriminator noise and accumulates
  noise power. Reuse that computation; do not create a second squelch detector.
  Measurement enablement must be independent of the selected COR source.
- The radio receive owner publishes an immutable measurement observation through
  its existing snapshot/lifetime mechanisms. Audio callbacks do not allocate,
  acquire locks, read sensors, format reports, or synthesize speech.
- Hardware RSSI acquisition uses the existing named IIO or hwmon sensor and
  its acquisition owner. No second device owner is created. Ordinary digital
  GPIO is not an analog input; an external ADC must expose the RSSI voltage.
- Controller policy applies calibration, validity, S-unit formatting and query
  capture. Speech preparation and playback reuse the message bundle, telemetry
  producer/ring, serialization and source-scoped reply path.
- Follow ADRs 0011, 0012, 0019, 0022, 0023, 0026, 0027, 0028 and 0035. Reuse
  published extension points first. If a measurement handoff cannot fit an
  existing contract, document and test the smallest versioned extension and
  update consumers together; no generic adapter escape hatch or duplicate core.

## Sources

### Flat FM discriminator noise

Observe the selected raw native receive channel before de-emphasis, program
filters, software gain, squelch/DTMF muting and dynamics. Hardware ADC gain is
necessarily upstream and belongs to the calibration identity.

The noise measurement uses the existing receiver noise filter. Its useful
passband must be present in the radio output and must not be dominated by wanted
modulation. A source labelled flat is not proof that it meets these conditions.
Speaker/de-emphasized audio is not an admissible DSP source.

Preserve the existing 20 ms noise integration window in elapsed native samples,
independent of callback partitioning. Retain normalized noise RMS before legacy
integer scaling/narrowing. Do not rename the existing compatibility `rssi_peak`
value as RF strength or as a new normalized RMS value.

The current frontend assumes the left ADC channel and receives blanked audio.
The meter must use the configured receive channel and must never interpret
blanked zeros as a fully quieted signal. Reuse existing filter state only when
its channel, coefficients and raw tap are appropriate; do not alter the carrier
detector's current behavior to make the meter work.

Mark windows affected by receiver blanking, unavailable capture, invalid samples
or clipping invalid. Require a complete clean window before restoring validity.
Zero noise alone cannot distinguish a quiet carrier from a disconnected/silent
audio feed: retain that ambiguity in the status, do not extrapolate an unlimited
RF level, and use available device/carrier diagnostics without treating CTCSS
qualification as RF strength.

Full quieting limits this method's useful range. Above or below calibrated
range, report a bound, not an extrapolated precise value. This is an FM
quieting-based RF estimate, not a direct antenna-power or SINAD measurement.

### External receiver RSSI

The selected sensor supplies receiver RSSI, normally through an ADC exposed by
Linux IIO. Respect input voltage limits and use protected/buffered wiring as
required by that hardware. CM119, parallel-port and ordinary GPIO logic inputs
cannot directly measure an RSSI voltage.

Reuse the sensor's actual acquisition timestamp, source identity, error and
staleness state. A last-good cached reading is not a fresh acquisition. Reuse
existing two-point calibration when the receiver output is linear in dB; a
nonlinear output requires a bounded monotonic calibration table. Apply each
calibration once, with its input units explicit.

For a responsive RSSI meter, configure the named sensor's existing
`poll_interval_ms` to 50 ms when supported by its hardware. Do not silently
rewrite that sensor or poll it from audio. A short receive burst without a real
hardware observation cannot receive an invented or pre-burst strength report.
Qualify the receiver RSSI settling response during hardware calibration. Only
valid acquisitions within the requested burst, after that hardware has settled,
are eligible; a still-fresh reading acquired before key-up is not sufficient.

## Configuration and calibration

Use `[signal_meter]` for shared defaults and `[signal_meter <node>]` for a node.
There is one meter per node because each node has one local receiver.

| Setting | Default | Contract |
| --- | --- | --- |
| `enabled` | `no` | Enable measurement and reports. |
| `source` | `disabled` | `disabled`, `dsp_noise`, or `sensor`; no automatic source substitution. |
| `sensor` | empty | Node-local named sensor for `source = sensor`. |
| `s9_reference_dbm` | `-93` | Configurable S9 reference; VHF/UHF default. |
| `calibration_points` | empty | Finite source-value/dBm pairs for a monotonic piecewise-linear mapping. |
| `calibration_fingerprint` | empty | Configuration identity recorded when RF calibration was made. |
| `quieting_noise_reference` | absent | Positive normalized RMS of unsquelched no-carrier noise; DSP only. |
| `attack_ms` | `20` | Display rise smoothing; raw observations remain available. |
| `decay_ms` | `500` | Display fall smoothing. |
| `stale_after_ms` | `1000` | Maximum observation age; must allow the configured source cadence. |
| `dtmf_id` | absent | Explicit per-node positive decimal selector in the existing `*76` family. |

Except for `dtmf_id`, values inherit from the shared section. The selector does
not inherit and must not collide with a sensor or input selector on the node.
The query prefix remains the existing remappable `link_command_site_status`.
Resolve invalid configuration through the established warning/default policy;
never silently claim a working measurement from an unusable source.

Calibration points use normalized residual-noise RMS for DSP, or the selected
sensor's declared engineering units after any existing sensor calibration.
Require at least two distinct input points, strictly monotonic inputs and dBm
values, and finite numbers. DSP's dBm mapping decreases as noise increases.
Store tables at setup/reload; interpolate only inside their measured range.

Calibrate with known RF-generator levels at the receiver input. Status exposes
a deterministic fingerprint of the known source/device/channel identity,
configured receiver frequency/bandwidth where available, hardware input gain,
noise-filter selection and measurement tap. Store it with the calibration
points. A missing or mismatched fingerprint leaves those points unqualified for
RF reporting, including after restart; downstream voice processing does not
alter the fingerprint. Unknown physical changes such as replacing the radio
require the operator to recalibrate: software cannot prove that unchanged
configuration describes unchanged hardware. Do not adjust hardware gains or
squelch to obtain a preferred S reading.

For relative quieting, use `100 * (1 - noise_rms / quieting_noise_reference)`,
bounded to 0 through 100. This is explicitly a relative indicator, not dB,
SINAD, or percent RF power. The no-carrier reference is an explicit calibration
value, not an automatically learned value during arbitrary on-air activity.

## Status and display

Publish at the existing configured status interval (50 ms by default). Preserve
measurement generation, source, completed-window sequence and acquisition time;
repeated reads must not refresh age or reapply smoothing. External sensors
publish their real cadence rather than pretending that a 20 Hz query creates
20 Hz hardware measurements.

Status contains the raw observation, source units, source quality/age,
calibration state, calibrated/estimated dBm when available, display S value or
relative quieting, and an in-range/below-range/above-range indication. Keep raw
COR and CTCSS status separate. Invalid, stale and unavailable are not zero dBm,
S0, or full strength. Receiver-disabled/transmit-blanked periods cannot refresh
a signal report.

Below S9, one S-unit is 6 dB. With the default reference, S9 is -93 dBm and S8
is -99 dBm. Above S9, show S9 plus dB. Do not imply resolution or range that the
source's calibration does not support. Display smoothing has no effect on
radio squelch or receive timing.

## Spoken DTMF query

Reuse `*76<dtmf_id>#`, followed by unkey. For example, selector `9` gives
`*769#`. Existing command termination by unkey also remains supported. This is
a public read-only status operation; it requires no administrative unlock and
cannot change meter settings or calibration.

The report measures the **local receiver**, not the remote peer's RF input.
For a local-RF request, bind the request to that command's receive burst. Even
when `#` completes the command early, finalize its value at the burst's unkey
boundary: retain the last eligible observation before burst retirement, then
freeze that typed value for the queued response. Integration windows must lie
entirely within the usable part of that burst. An ordinary post-unkey meter read
would instead measure idle noise and is incorrect for this query.

Preserve the last completed burst observation long enough for the existing
delayed DTMF/control handoff to resolve its generation and burst identity. Do
not accidentally select an earlier transmission, a later transmission, blanked
samples, or a stale last-good value. This is bounded measurement metadata, not
an additional PCM recorder or ring. Derive pending retention from the existing
bounded command/reply ownership. If a delayed request's matching burst metadata
has been retired, report unavailable; never substitute a newer burst.

A peer-originated query captures the local receiver's current valid observation
at request admission; if there is no usable local signal, say so. It never
substitutes the peer's voice amplitude or its network quality for local RF
strength. After a reply's capture is finalized, subsequent receive activity must
not change its frozen value.

Wait for the requesting source to unkey and route only to that source under
ADR 0023: local RF replies go to the local transmitter; peer replies go only to
the originating live connection. Never broadcast the reply. Retired generations
or disconnected/replaced peer connections cannot receive stale work. Other
site meters retain their existing post-unkey snapshot behavior.

All report text lives in the shipped Fluent message bundle, with separate
`.text`, `.tts` and `.morse` forms and existing speech/Morse fallback. Example
English speech forms are:

- Calibrated hardware: "Signal strength S seven."
- Calibrated DSP: "Estimated signal strength S seven."
- Above S9: "Signal strength S nine plus twenty decibels."
- Saturated estimate: "Estimated signal strength at least S seven."
- No RF calibration: "Relative receiver quieting eighty percent."
- No usable reference/calibration: "Signal strength is not calibrated."
- No usable observation: "Signal strength is unavailable."
- Peer query with no local receive activity: "There is no local signal."

Choose the applicable range/calibration qualifier rather than silently dropping
it. Use localized number formatting, not English strings assembled in code.
Speech and Morse are generated outside audio callbacks and streamed through
the existing telemetry ring. No new telemetry thread or playback policy.

## Required verification

1. Raw measurement uses the selected channel, completes exact windows across
   variable callback sizes, and works with hardware COR or DSP COR without
   changing their decisions. Voice gains, filters and dynamics do not affect it.
2. Blanking, clipping, missing input, invalid windows, stale observations and
   repeat snapshots cannot become fresh full-strength reports.
3. Both calibrated slopes, nonlinear interpolation, bounds, calibration
   invalidation, relative quieting and S9 conversion have deterministic fixtures.
4. The same RF test burst produces the same query value whether command
   completion occurs before unkey or through delayed unkey delivery. Idle noise
   and later bursts cannot overwrite the pending report.
5. `*76` selector/remapping/authorization, localized TTS/Morse fallback,
   source-only reply, generation/connection retirement and ordinary site-query
   behavior retain regression coverage.
6. Standalone and Asterisk compositions receive the same measurement through
   released shared components. Setup/reload preserves ownership and realtime
   constraints; no extra hardware owner or direct external-library calls.
7. Validate RF estimates with a generator on real supported hardware before
   claiming receiver-specific accuracy. Run targeted checks during development;
   the full native Debian 13 PR gate remains the merge requirement.

## Research references

- [ASL VOTER discriminator-noise measurements](https://allstarlink.github.io/voter/voter-audioanalysis/)
  explain the inverse noise/quieting relationship and raw-audio bandwidth limit.
- [IARU Region 1 VHF Handbook, signal reporting](https://www.iaru-r1.org/wp-content/uploads/2021/03/VHF_Handbook_V9.01.pdf)
  specifies S-unit calibration and meter ballistics. This design's 20 ms DSP
  integration and status cadence do not claim exact IARU transient response.
- [Linux IIO ADC documentation](https://cdn.kernel.org/doc/html/latest/iio/iio_adc.html)
  describes external analog acquisition and source-unit normalization.
- [Analog Devices AD608 RSSI example](https://www.analog.com/media/en/technical-documentation/data-sheets/ad608.pdf)
  illustrates an RSSI voltage with approximately linear dB scaling; it is not
  a required or selected hardware device.
