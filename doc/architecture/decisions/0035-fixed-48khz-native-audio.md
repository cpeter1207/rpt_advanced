# ADR 0035: Fixed 48 kHz native audio

Status: Accepted

## Context

CM119 audio, RNNoise, USBRadioPlus native processing, and rpt_advanced use
48 kHz. Generalizing the native pipeline to additional rates adds conversion,
state, latency, and test cases without a current requirement. The adapter and
shared-ring boundaries already provide conversion for sources using other
rates.

## Decision

The radio/controller native PCM rate is **48,000 Hz**. USBRadioPlus,
`librptadvradio`, rpt_advanced, native telemetry, and voice-processing graphs
use that rate. Higher native rates are unsupported; do not implement or retain
an alternative native-rate processing path in anticipation of future hardware.
Internal PCM remains canonical normalized `f32` under ADR 0029.

The selected audio adapter opens a 48 kHz stream. Its rate cannot change while
open. A device that cannot supply the selected 48 kHz stream cannot start that
composition. This narrows the native-rate choices in ADR 0027 without changing
its bounded variable-frame worker calls or sample-clocked timing contract.

RNNoise receives 48 kHz samples directly. Its required 480-sample framing is
handled by preallocated assembly/output storage, not by a resampler. Remove
its input and output conversion stages, including unity-ratio converters.
Other voice-processing stages operate directly at 48 kHz unless an actual
external implementation requires a different input representation; do not
add conversions merely to preserve a redundant stage.

Sample-rate conversion belongs at a real source/sink boundary:

- Under the 2026-09-13 amendment in ADRs 0025/0027, inbound PCM rings alone
  convert local receive, decoded peer, and telemetry sources to the native
  transmit rate and correct independent-clock drift. The current USBRadioPlus
  migration implements the separate callback entry points, but this complete
  ring topology remains pending. Local receive DSP precedes its ring; transmit
  performs no sample-rate conversion and fills the adapter output buffer directly.
- Peer ingress decodes at the negotiated codec rate and leaves inbound rate
  conversion to that peer's ring. Egress converts native PCM to the codec rate
  before encoding; this outbound boundary is not an inbound correction stage.
- The app_rpt compatibility adapter retains its 8 kHz interface: inbound
  conversion belongs to its program ring, outbound conversion to its egress.
- The rpt_advanced radio interface exchanges native 48 kHz PCM directly.
- Speech and sound-file sources produced at another rate enter the telemetry
  playout ring at that source rate and are converted once there.

The transmit worker's rate and cadence are supplied by the audio adapter's
output clock, normally the DAC clock. This does not broaden this composition's
48 kHz support: native rate is still fixed for the open stream. Independent
input/output cadence is handled by the rings, never a second transmit timer.

Use the released shared rate-adjusting PCM ring and samplerate adapter for
those conversions. The shared ring also retains near-unity asynchronous
clock recovery when both nominal rates are 48 kHz: equal nominal rates do not
make independent clocks identical. This is not a reason to add a second
converter or buffering stage.

If the adapter knows ADC and DAC have no relative drift and can deliver
aligned frames, ADR 0027 permits receive followed immediately by transmit.
The local inbound ring then provides synchronous unity-rate pass-through,
without adaptive correction, a unity converter, or prefill delay. This is a
clock-topology capability, not a new sample rate, and leaves independent
peer/telemetry conversion and intrinsic device/DSP latency unchanged.

Private detector decimation is an algorithm detail, not another native PCM
interface. Existing lower-rate CTCSS/noise analysis does not change the 48 kHz
voice path. This decision does not add a speech-detector requirement or change
its recorded implementation choice.

Reusable shared libraries retain their published general-purpose APIs and
ABIs. Restrict the radio composition, not unrelated consumers of a released
converter or ring. Do not advertise higher-rate product support merely
because a dependency can convert it.

## Consequences

RNNoise no longer adds two unnecessary sinc-filter delays or their converter
state. Its framing, model, level scale, and enable/disable policy remain
unchanged. Tests compare direct RNNoise output and framing across callback
partitions rather than preserving the removed converters' filtering.

Stream setup tests reject unsupported native rates. Native-to-native handoffs
have no rate-conversion stage except where independent-clock recovery is
required. Existing codec, Asterisk, and media-boundary conversion tests remain.
No shared-ring or samplerate-adapter ABI/SONAME change is needed. The
unreleased radio-core bootstrap is narrowed before its initial release;
already deployed nodes are not changed by recording this decision.

The appliance PRD, system clocking architecture, radio-port endpoint contract,
and traceability matrix carry this same 48 kHz constraint. A wider codec
data-sheet capability does not require a wider software-rate implementation
or change pending hardware qualification.
