# Appliance signaling and output-interlock software

Status: Software-only scope approved; revised 2026-10-17 after user review;
written design awaiting approval.

## Scope and acceptance

Implement the software portions of ADRs [0033](../../../doc/architecture/decisions/0033-appliance-radio-signaling-capability-profile.md)
and [0034](../../../doc/architecture/decisions/0034-appliance-hardware-output-interlock.md).
Use a deterministic simulated hardware backend to prove the signaling,
arming, fault, and recovery contracts. Preserve current CM119 operation.

This completes only the corresponding software portion of the site-I/O and
appliance wishlist entries. It does not complete physical interlock qualification,
board bring-up, or the broader site-monitoring feature.

Out of scope: real GPIO/ALSA pin mapping, device tree, electrical design,
physical TX mutes, sensors, fan control, analog calibration, stored meter history,
general alarm/action automation, REST/UI work, releases, and node deployment.
The existing hardware-repository working changes remain untouched.

The hardware source defines normalized semantics but leaves arm/status pad
assignment, status encoding, supervisor timing, and module discovery unfinished.
No software result may be presented as proof that physical outputs fail safely.

## Approach

Extend the existing radio and GPIO shared objects. Do not introduce another
library, another DSP implementation, or a software replacement for the physical
interlock. A new board-specific adapter would require invented wiring; a
software-only PTT gate would not satisfy ADR 0034.

| Owner | Responsibility |
| --- | --- |
| `librptadvradio` | Independent CTCSS encode/decode selection; existing COR/CTCSS/DCS DSP; prepared PTT/external-CTCSS intent |
| `rptadv-gpio-adapter` | Hardware-neutral GPIO capabilities, port leases, normalized snapshots, prepared outputs, and control-only transport calls; appliance safety state when that hardware is selected |
| Existing controller | Existing PTT and activity-scoped CTCSS policy; no duplicate hang timer or telemetry policy |
| Test backend | Simulated module presence, power/reset/watchdog health, arm feedback, and observable actuator/mute state |

GPIO is not appliance-specific. Sources include USB radio interfaces such as
URIXB connected to a Raspberry Pi, existing CM119 interfaces, parallel ports,
and appliance GPIO. Each backend reports the signals, directions, and safety
facilities it actually provides. No device is assumed to provide every kind of
GPIO, external CTCSS, analog monitoring, or a hardware interlock.

Reuse existing device backends; the new capability contract must not change
CM119 discovery, HID, EEPROM, or parallel-port behavior. Backend-specific pin
mapping and additional device implementations remain out of this software-only
phase. A test backend must not be selectable by an installed service as a
substitute for missing real hardware.

## Signaling profile

Add `appliance_direct_codec` as a hardware capability profile beside the current
native-audio profile. It does not force either CTCSS direction to use external
hardware. Selected capabilities and CTCSS source assignments are immutable for
a session; changing them requires a quiesced session replacement, not a live
reinterpretation of hardware state.

- COR selects normalized hardware COR or the existing DSP carrier detector.
- CTCSS encode and decode are separately configured as disabled, DSP, or
  external. Support all capability-valid combinations, including external
  encode with DSP decode and DSP encode with external decode. Selecting an
  encoder never implies that a decoder exists or is selected.
- DSP decode uses the existing native CTCSS detector. External decode uses the
  normalized decoder-status input. Disabled decode adds no CTCSS qualification.
- DSP encode uses the existing oscillator, level, routing, and turn-off
  behavior, mixing the tone into the selected TX audio leg. It requires a
  suitable audio path, not a dedicated analog CTCSS pin. External encode
  produces only an encoder-enable action and never mixes a native CTCSS tone
  into the audio. Never enable both encoding sources for one port.
- DCS continues to use the existing native decoder and encoder. Do not add a
  DCS pin or a second DCS implementation. Unsupported simultaneous receive
  qualification combinations are rejected rather than assigned new semantics.
- Reuse the existing transmit-signaling state machine and controller-supplied
  CTCSS inhibit for either encoder source. External enable is never asserted
  without effective PTT and remains subject to the selected hardware's output
  gate; the appliance requires its physical interlock.
- External enable supports normal release and tone removal before PTT release.
  Native phase-shift/tail-tone requests are unsupported for external CTCSS;
  DCS turn-off retains its existing behavior.
- When external encode is selected, its frequency, deviation, and turn-off
  waveform belong to that radio/encoder, not an on/off GPIO. Do not imply that native
  CTCSS tuning fields configure it. Profile validation reports incompatible
  native-generation settings instead of silently emitting a different tone.
- Validate each selection against the chosen audio/GPIO backend's capabilities.
  Missing required inputs or outputs cannot silently change the selected source
  or bypass receive qualification. Report unsupported assignments and leave
  that port safely inactive. Unused capabilities need not be present.
- Raw external decoder status remains observable when provided. No
  activity-scoped decode policy is added; that wishlist requirement was withdrawn.

Program filtering, levels, duplex behavior, routing, native rate, and CM119
signaling remain unchanged. DCS still joins program audio after its existing
processing stages. The portable core performs no device I/O.

## Port count and safety capabilities

There is no fixed software limit on port count. Size port collections and
preallocate callback-visible state from the configured endpoints during control
setup. Practical capacity depends on hardware, memory, CPU, and I/O resources;
allocation or admission failure must be explicit and safe. Neither the ABI nor
an aggregate port mask may encode a four-port ceiling. The planned appliance's
four slots remain a physical limit of that board only.

Each hardware context owns its actual endpoints and capabilities. An appliance
context covers the ports wired to its common physical interlock; that common
fault does not disarm unrelated USB or parallel-port hardware. Such backends
report hardware arm, watchdog, mutes, and feedback only if actually available.
Do not fabricate those signals or require the appliance arm protocol from a
device that lacks it. Preserve the existing safe-output behavior for those
devices without claiming physical interlock protection. A backend lacking the
required physical safety facilities cannot satisfy the appliance contract.

## Appliance safety contract

One control-owned context covers an appliance's common interlock and its
configured port leases. Each port has a stable logical identity. A duplicate
lease, missing required module, invalid snapshot, or unavailable required
endpoint fails closed. Unconfigured empty slots are not faults.

Transport callbacks operate on normalized values, not guessed pin numbers:
module presence, input/main power good, reset released, watchdog valid,
hardware arm feedback, COR, CTCSS status, and transport health. The transport
also accepts arm/disarm requests, prepared port outputs, and safe-output
requests. GPIO polarity, wire encoding, and physical timing belong to the
eventual backend. Read and command errors are faults, never good snapshots.

The adapter distinguishes hardware health from confirmed output arm. A
successful command submission alone is not arm confirmation. State is:

| State | Behavior |
| --- | --- |
| Disarmed | Safe outputs; may initialize and observe health; no actuator admission |
| Arming | Explicit request accepted after successful initialization and healthy current inputs; outputs remain safe pending fresh arm feedback |
| Armed | Current-epoch prepared outputs admitted only while all required safety conditions hold |
| Faulted | Latched fault and safe outputs; restored health or watchdog pulses do not re-arm |

Creation and shutdown are disarmed. Initialization includes all configured
port leases, safe output submission, current valid safety observations, and
current-epoch radio-ready confirmation for every configured port. Control
confirms radio-ready only after that port's session, processors, and audio
endpoint have successfully initialized and completed required warm-up. A fault
or session replacement invalidates this confirmation; healthy hardware alone
does not establish radio readiness.
An explicit control-plane arm request is required at startup and after a
fault. Recovery first repeats initialization and establishes healthy current
inputs; only a new arm request may enter Arming. Missing arm acknowledgment
at the transport-configured deadline faults. Tests supply the clock/deadlines;
this phase does not invent the physical watchdog or arm timing.

Any required safety fault disarms the whole appliance and all its outputs.
The service owner requests PTT and external CTCSS release, both TX-leg mutes,
and site-output disable. If transport I/O itself fails, report that safe-state
submission failed; do not claim hardware release was confirmed. The independent
physical interlock remains responsible for asynchronous protection during a
stalled process, failed transport, or loss of power.

Invalidate pending actions on fault, disarm, and port retirement using an
arm/lease epoch. Actions and arm requests from an earlier epoch cannot become
active after recovery. Re-arm starts with released outputs and requires fresh
current-epoch intent; it cannot replay an old PTT or site-output request.

## Real-time and lifecycle boundary

Use the GPIO adapter's existing single service-owner pattern per hardware
context. Only that non-audio owner performs transport I/O, initialization, arm acknowledgment,
and recovery. Audio owners publish bounded prepared intent and read atomic
snapshots; they never allocate, wait, log, or call a hardware transport.
Each port retains one transmit intent producer. An interlock fault revokes
admission for its covered ports independently of the next audio callback.

Snapshots expose presence, health, arm state, current epoch, latched fault
reason, requested versus confirmed output state, and transport failure.
If a backend lacks output readback, report confirmation as unavailable; a
successful write is not proof of the physical output state.
Existing control status can consume these typed snapshots without inventing a
new UI or alarm engine. Software PCM muting and suppressed intent complement,
but cannot replace, the physical post-DAC mute and actuator gate.

Retire a port only after its audio owner is quiescent. Release its outputs
before destroying transport state. Whole-appliance shutdown invalidates all
leases and leaves no automatic re-arm request. Ordinary unchanged-session
reload is not an arm operation; changing the profile or hardware composition
requires disarm and controlled replacement.

## ABI and packaging

Keep existing ABI-4 radio and ABI-1 GPIO operations/layouts unchanged. Add
descriptor-tail operations with separately sized profile, capability, action,
safety, and transport records. Port collections use counts/handles sized at
setup, not a fixed four-element array. New clients validate those tails before
use. Existing operations continue to mean current CM119/native behavior;
they are not repurposed as appliance calls. All paths share internal logic.

Do not append fields to existing unsized RX/TX result records. A profiled
transmit operation returns prepared external actions alongside its existing
PCM/result span so no second call races a later callback. Session preparation
and updates validate the same immutable profile and capability rules.

This additive contract needs new compatible provider package versions, not
new SONAMEs. If implementation demonstrates that an additive contract cannot
meet the safety/lifetime requirements, stop for an explicit ABI decision.
Do not modify released consumer dependency minima until the provider releases
exist. The composed test uses the candidate dynamic DSOs, not vendored code.

No installed controller/USBRadioPlus configuration selects an appliance or
simulation in this phase. Configuration here means the profile/setup contract
tested through the shared-library ABI. Future board composition will supply
the actual hardware transport and identity mapping.

## Verification and completion evidence

Use test-driven development and the maintained latest quality images. Run
affected tests/coverage first; record completed cases in ignored `.work/`
evidence. Full PR validation remains native Debian 13 amd64/arm64, with 100%
production line/branch coverage on amd64. Do not repeatedly run full gates
while closing individual gaps.

Required checks:

1. CM119/native profile output and qualification remain unchanged.
2. A data-driven matrix covers independent disabled/DSP/external CTCSS encode
   and decode choices, including encoder-only hardware; external COR/CTCSS
   assertion and loss; DSP COR; DCS; and exact variable frame counts. Native
   CTCSS is generated only for DSP encode, never for external or disabled encode.
3. Existing CTCSS inhibit/hold policy controls either encoder independently
   from PTT. Missing required capabilities and incompatible settings fail safely
   with useful errors; unused missing capabilities do not prevent operation.
4. Cold start, missing/duplicate ports, missing or stale radio-ready confirmation,
   each health failure, transport failures, arm success/refusal/timeout, fault
   during arming, and shutdown while keyed.
5. Healthy inputs and renewed watchdog activity never re-arm by themselves;
   only explicit recovery plus a new arm request succeeds.
6. Stale intents/acknowledgments cannot cross fault, lease, or reload epochs.
   A common interlock fault releases every configured port and disables site
   outputs covered by that interlock; unrelated hardware remains operational.
7. Callback allocation/locking guards and service-stall tests: audio does not
   wait; the simulated physical interlock independently removes outputs.
8. C ABI sizes/tail admission, old CM119 client compatibility, shared-object
   linking, warning-free developer documentation, package/archive installation.
9. A hardware-free composed test dynamically loads the radio and GPIO DSOs,
   drives the real profile/session code with the simulated backend, and proves
   fault/explicit-recovery behavior across multiple configured ports.
10. More than four configured ports work without callback allocation or a global
    fixed-size port mask. Simulated USB/parallel-style capability subsets need
    no appliance arm feedback. An appliance fault affects only its own covered
    endpoints, and missing optional GPIO is not reported as a hardware fault.

Mark ADRs 0033/0034 as software-implemented only after those checks pass.
Keep real appliance integration and physical fault qualification explicitly
pending. User-facing manuals/examples are updated before a PR; developer
documentation and ABI comments accompany each implementation commit.
