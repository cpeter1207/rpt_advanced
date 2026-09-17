# ADR 0028: Remove res_usbradio through explicit hardware adapters

Status: Accepted

## Context

At the time of this decision, `RadioPlusAdvanced` was already a thin Asterisk
channel-technology adapter. It did not call a `res_usbradio.so` helper: it
delegated Asterisk channel callbacks to the USBRadioPlus backend. The legacy
USBRadioPlus backend imported `res_usbradio` helpers for device PCM, mixer and raw-audio
statistics, HID/EEPROM access, timing, and parallel-port/GPIO support. Its
module metadata therefore required `res_usbradio.so`.

That dependency prevents the radio implementation from becoming the
Asterisk-free, standalone component defined by ADR 0005. It also combines
audio transport, radio control, GPIO, and diagnostics behind an Asterisk
resource module rather than explicit replaceable boundaries.

## Decision

The dependency on `res_usbradio.so` will be removed by moving every remaining
hardware interaction behind three selected adapters:

1. The **audio adapter** owns PCM device lifetime, channel normalization,
   capture/playback queue handling, hardware mixer control, and raw-device
   audio statistics. It uses PortAudio for PCM transport and ALSA only where
   mixer access is required. It opens PortAudio callbacks as `paFloat32` and
   publishes canonical normalized `f32` PCM plus explicit
   peak/RMS/clipping, queue, and device-error snapshots through the radio-core
   contract. Every supported CM119 path, including ASL legacy compatibility,
   uses this PortAudio/ALSA adapter; OSS has no fallback role.
2. The **radio-control adapter** owns rig control and radio-signaling actions
   supplied by the selected radio-control implementation. Its initial
   implementation is backed by Hamlib. It receives abstract control requests;
   neither the radio core nor the Asterisk adapter imports Hamlib types.
3. The **GPIO adapter** owns CM119 HID GPIO, parallel-port GPIO, and other
   configured site I/O. It publishes input snapshots and applies prepared
   output actions through a platform-neutral pin contract. It has no Asterisk
   dependency.

`librptadvradio` remains portable: it must not link `res_usbradio`, Asterisk,
OSS, PortAudio, ALSA, Hamlib, or device-specific HID libraries. Under the
2026-09-13 amendment in ADR 0027, its local receive worker accepts canonical
`f32` input and hardware snapshots, while its transmit worker fills the
adapter-supplied canonical `f32` output buffer and produces abstract
radio-control/GPIO actions. DSP COR, CTCSS, and DCS remain radio-core
signal processing; adapters supply only their selected hardware equivalents.
PortAudio/ALSA converts to and from physical device formats below the callback;
Asterisk adapters convert at their own boundary, as defined by ADR 0029.

Hamlib is optional for an ordinary CM119 node with no CAT-capable rig. In that
case, the GPIO adapter directly applies PTT and reports configured COR inputs.
Each configured PTT or receive-signaling function selects exactly one source;
conflicting explicit assignments are invalid. When configuration leaves a
function unassigned and both applicable adapters are available, Hamlib is the
preferred source. CM119 EEPROM access belongs to the GPIO adapter because it
uses the CM119 HID transport; the audio adapter owns ALSA mixer controls.

USBRadioPlus maintains one ASL3 compatibility adapter once these hardware
boundaries replace `res_usbradio`. The old legacy/modern split follows the
resource module's two hardware APIs, not two different radio behaviors. Do not
port or maintain duplicate channel implementations for those retired APIs.
Keep only small Asterisk-version shims if a supported Asterisk ABI actually
requires them, and compile the same adapter against the supported headers.

The single ASL3 adapter retains Asterisk frame assembly and boundary conversion
and uses the selected audio, radio-control, and GPIO adapters. Its app_rpt
interface remains 8 kHz. The separate thin `RadioPlusAdvanced` interface
remains native 48 kHz under ADR 0035; consolidating ASL3 hardware backends does
not merge those controller-facing protocols. The direct standalone path uses
the same selected hardware adapters. A device may be owned by only one
selected composition at a time.

### Native operating-mode retirement (2026-09-13)

ADR 0039 separately retires USBRadioPlus's native software local-repeat mode
and native parrot from future ASL3 support; rpt_advanced does not require them.
This narrows feature preservation for the subsequent removal, not the already
completed hardware cutover. Preserve ordinary app_rpt, legacy echo, hardware
local repeat, shared native DSP/statistics, and the distinct `RadioPlusAdvanced`
transport. Mode removal is implemented in the current candidate and remains
under verification; it does not alter released alpha18.

### Best-effort audio scheduling (amended 2026-09-13)

The audio adapter first attempts Linux `SCHED_FIFO` priority 99 for both capture
and playback callbacks. If 99 is unavailable, it attempts the highest lower
priority actually permitted by the operating system, service resource limits,
and current privileges. The kernel's theoretical maximum alone is not proof
that a priority is available. Selection is bounded, occurs during stream
startup rather than in a PCM callback, and does not lower an already adequate
real-time priority merely because an attempted increase failed.

If no priority increase can be obtained, the adapter retains the existing
inherited scheduling and continues normal stream startup. Failure to increase
priority is a nonfatal scheduling limitation, not a stream, device, or module
failure. It must not cause startup rejection, restart loops, or repeated
elevation attempts in the audio callback. Record the actual policy and priority
and a useful nonfatal diagnostic outside the callback; never claim priority 99
was established when it was not. The adapter must not grant itself additional
privileges or change system-wide scheduling policy to satisfy this preference.

Offline media subprocesses must not inherit a real-time host-thread policy.
Before executing Piper or FFmpeg, the media adapter resets the child to
`SCHED_OTHER` at priority zero. If that reset fails, media preparation fails and
the configured telemetry fallback applies; a CPU-heavy media child must never
compete with audio callbacks at their real-time priority.

Any temporary scheduling change to the startup caller is restored before
returning. Inability to restore a scheduling change that actually succeeded
remains a distinct lifecycle-safety fault; this amendment does not hide that
fault or genuine device/open/start errors. Buffering, DSP, control-thread
scheduling, bounded callback work, and kernel real-time throttling are
unchanged. This amendment replaces the previous requirement that inability to
establish priority 99 prevent stream startup.

Validation must cover priority 99 success, fallback to a lower permitted
priority, no permission to increase priority, unavailable scheduling metadata,
an already adequate priority, caller restoration, and both callback threads.
No-elevation cases must still start and process audio when the device itself
is usable. Genuine device and lifecycle failures retain their existing tests.
Investigate output underruns under both quiescent and externally loaded
systems: correlate callback and xrun timestamps with activity outside Asterisk,
including CPU load, disk I/O, and scheduling delays, before assigning a cause.
Fallback scheduling does not promise glitch-free audio under arbitrary load.

### Receive/transmit callback split (amended 2026-09-13)

Nominally equal capture and playback rates do not imply a shared hardware
clock. The audio adapter drains capture independently of playback readiness,
using separate PortAudio input and output callbacks under one device lease.
Capture calls the local receive worker directly for squelch, CTCSS/DCS decode,
deemphasis, and receive processing. That worker publishes processed PCM to the
local receive inbound rate-adjusting ring. Playback invokes the transmit
worker to mix native-rate ring outputs from local receive, connected links,
and telemetry, add profile-selected CTCSS/DCS, and render directly into
PortAudio's output buffer. The transmit worker follows the DAC/output
callback's clock and requested frame count, not capture readiness or a timer.
The local receive ring is the single-producer/single-consumer handoff and the
only capture-to-playback converter; correction does not precede receive DSP.
No polling worker, callback mutex, duplicate raw-capture resampler, or
PortAudio output ring is added. All inbound ring conversion follows ADR 0025.
Setup preallocates a small block-sized working margin and converter history;
capacity is not an instruction to accumulate that much latency. Capture
occupancy, correction, shortfall, and discard counters are observable.
Both callbacks must stop before the ring or device reservation is released.

An adapter with a known common ADC/DAC clock and aligned input/output delivery
may instead declare ADR 0027's shared-clock capability. It calls receive then
transmit back-to-back and uses the local ring in unity-rate pass-through mode,
without adaptive drift correction and with a target reserve equal only to the
configured squelch delay. PortAudio may use one full-duplex callback in this
mode. A matching nominal rate alone does not qualify; unknown or independent
clocks retain the separate callbacks and adaptive ring. All modes preserve
private worker state, safe lifecycle, and source-specific recovery on
independent inbound rings.

Migration is staged without changing radio behavior: extract and verify the
audio/statistics path, then radio-control and GPIO paths, switch the retained
ASL3 compatibility implementation to those contracts, remove its duplicate
backend and obsolete private types/build selection, and finally remove the
`res_usbradio` headers, symbols, module requirement, and package dependency.
The final package and integration checks must prove that neither USBRadioPlus
nor rpt_advanced has a runtime dependency on `res_usbradio.so`.
Tests must retain equivalent device selection, calibration, EEPROM, signaling,
reload, and audio behavior; removing a duplicate file alone is not proof of
that cutover.

## Consequences

PortAudio/ALSA, Hamlib, and GPIO implementations become replaceable adapter
dependencies with their own versioned contracts under ADR 0022. Device-level
statistics are owned at the audio boundary instead of being inherited from an
Asterisk helper. The Asterisk module remains a compatibility adapter rather
than a hardware owner, and standalone operation uses the same radio behavior.

The per-node composition resolves one stable CM119 identity before it opens
adapters. It passes that resolved identity to both the audio and GPIO adapters,
rejects an ambiguous match, and prevents concurrent ownership. This preserves
the device-ownership rule from ADR 0005.

## Implementation status

The direct-callback proof of concept (2026-09-16) removes the product's
`rpt-radio` thread and its Asterisk PCM exchange. Host-services ABI 2
(`rptadv.hst2`) reserves a channel, installs both direct endpoints through
the version-1 `0x52504144` channel option, and starts it. Product ABI 2
(`rptadv.prod2`) rejects old host tables; the existing product descriptor
entry-point symbol and library SONAME remain, with exact admission checks
preventing mixed-alpha use. Current consumers must be rebuilt together.

Preparation allocates stable inactive callback contexts and one local F32
SPSC queue with twice the maximum callback count. Successful runtime
publication installs fixed independent RX/TX generation owners and activates
the contexts with release/acquire atomics. Failed activation synchronously
hangs up before returning, while normal quiesce destroys the channel before
reclaiming contexts or registrations. RX time advances from accumulated
48 kHz samples; TX consumes local PCM directly into its caller's buffer.

This proof of concept does not claim the later complete asynchronous ring
design: its local queue has bounded newest-sample drops and zero-fill
shortfall, with a latest-state receiver snapshot. Adaptive clock correction,
sample-associated qualification, and generation-tagged local PCM remain the
separate ADR 0025/0026 migration requirements.

The current USBRadioPlus migration implements independent PortAudio input and
output callback entry points and runs receive DSP from the input callback.
It does not yet implement the processed local-receive ring, the complete
local/link/telemetry ring mixer, the verified shared-clock fast path, or the
generational station-host lifecycle. Those remain a later implementation
tranche. Released alpha18 still hands raw capture through its clock-recovery
ring to a playback-driven combined native tick.

The current PortAudio/ALSA adapter source implements best-effort scheduling at
stream startup. It tries FIFO priorities from 99 downward, continues with the
inherited scheduler when elevation or metadata is unavailable, and records the
policy, priority, and limitation separately for capture and playback. The new
statistics append the original ABI-2 prefix, so the adapter retains SONAME 2
and accepts ABI-2 callers using the earlier prefix size. Restoration failure
after an actual scheduling change remains fatal. Publishing and deploying that
source remain separate release operations; already released binaries are
unchanged.

Released alpha18's hardware cutover removed the duplicate backend and
resource-module imports before the complete Rust migration. In the current
migration, Rust owns station composition, callback processing, app_rpt DTMF and
8 kHz echo state, native 48 kHz delivery to rpt_advanced, Asterisk channel and
frame delivery, incoming-link control, transactional reload, CLI commands, and
the complete module lifecycle. The remaining C entry point only declares
module metadata, composes the released provider manifest, and forwards load,
reload, and unload through the versioned Rust loader descriptor. Hardware
lifecycle teardown quiesces callbacks and delivery before releasing device
identity. Shared parallel-port access retains one physical owner with
per-channel signaling state. Node installation and full release-gate
verification are separate from this source-level implementation status.
