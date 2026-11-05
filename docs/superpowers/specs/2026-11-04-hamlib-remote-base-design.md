# Shared Hamlib remote-base control

Date: 2026-11-04

Status: Design approved by the user on 2026-11-04; implementation planning.
Node deployment is not authorized by this approval.

## Scope and existing decisions

Provide mode- and frequency-agile remote-base operation through the same
dynamically linked implementation in standalone RPT Advanced and USBRadioPlus.
Include named memories, scan lists, and DTMF access to every supported
remote-base operating function. Preserve existing non-Asterisk-specific radio
features, including parallel-port channel selection and RTX programming.

One configured node owns one rig. Additional rigs use additional nodes. There
is no software-wide port or rig-count limit. Existing hardware ownership,
interlocks, audio processing, tuner, calibration, EEPROM, and GPIO services
remain shared. Do not restore retired USBRadioPlus native parrot/software
repeat modes. Asterisk channel framing and Asterisk jitter-buffer configuration
remain host-specific.

This design develops ADR 0004 and follows ADRs 0011, 0012, 0022, 0023, 0026,
0028, 0029, 0033, 0034, 0035, 0036, and 0038. Its proposals are distinguished
from accepted decisions below. No new REST service, protocol, codec, or radio
audio-streaming implementation is required by this work.

## Architecture and alternatives

Use one shared remote-base service, reached through typed operations from both
hosts. The service owns rig state, limits, tune transactions, memories, and
scan sequencing. The Hamlib dependency is isolated behind its own versioned
adapter. Existing parallel operations use the existing GPIO device owner.

Two alternatives were rejected against the recorded requirements: separate
standalone/ASL implementations would duplicate behavior; invoking rigctl as a
subprocess would add process/protocol handling where Hamlib already supplies
a library API. Using hardware memories/scanners exclusively would exclude
radios whose Hamlib backend does not implement those optional operations.

Proposed component boundaries:

| Component | Responsibility |
| --- | --- |
| Shared remote-base library | Typed operating commands, capabilities, limits, transactions, named memories, scan state, and status. No Asterisk or Hamlib types. |
| Hamlib radio-control adapter | Dynamic libhamlib calls, model catalog, transport setup, capability translation, errors, and per-rig I/O ownership. |
| Existing GPIO adapter and physical owner | Parallel channel/RTX sequences, CM119/other GPIO signaling, pin reservations, interlocks, and safe outputs. |
| Existing shared radio product | Composes the service with its existing audio/hardware lease and publishes prepared control intent and snapshots. |
| Standalone/Asterisk adapters | Configuration ingress and command/status transport. They do not implement memories, scanning, or rig protocols. |

The shared remote-base service is a meaningful independently reusable
functional component under ADR 0011. Package it as one versioned shared object,
with a separate Hamlib dependency adapter under ADR 0022. Final artifact names
and the smallest compatible descriptor extensions are implementation details.
Do not statically embed another copy in each product or add a library per
operation. USBRadioPlus must remain installable without the controller.

## Ownership and execution

Each rig has one serialized non-audio I/O owner. Submit owned, bounded requests
through the existing control boundary and a lock-free handoff to that owner.
One stalled rig must not block another rig or a controller executor.

Hamlib can perform blocking I/O and use internal mutexes. Keep that work inside
the dependency adapter's non-audio worker. Do not promise that Hamlib itself
is lock-free. Audio callbacks only publish epoch-tagged PTT/tone intent and
read coherent prepared snapshots. They do not call Hamlib, operate a parallel
port, allocate, wait for a response, or take a rig-control lock.

PTT-off, fault inhibition, shutdown, and invalid-generation rejection take
priority over ordinary tune requests and status polls. Bound transport timeout
and retry settings. A synchronous CAT call cannot safely be interrupted by
another thread; an unkey is serviced after that bounded call returns. Report
unconfirmed unkey as a hardware fault, rather than claiming the transmitter
is off. Use existing independent hardware interlocks when configured.

Use backend events where reliable; otherwise poll only supported values on
the rig owner with a configurable interval. Publish requested, applied,
observed, and unknown/write-only state separately. Retire generations and
hardware leases using the existing lifecycle; stale completions cannot key
or reconfigure a replacement rig.

## Backends and radio parity

Hamlib is the primary CAT backend. Select models by canonical symbolic names,
for example `RIG_MODEL_IC7300`, rather than numeric IDs. Generate and validate
the supported-name table against the installed Hamlib version. Support the
serial, network/rigctld, USB, and other transports offered by the selected
backend. Named `hamlib_option_*` values pass through adapter validation;
operating requests cannot submit arbitrary Hamlib API calls or shell commands.

Expose supported frequency, mode, passband, VFO selection, split, repeater
offset, RIT/XIT, power, tone, squelch, RF/audio levels, preamp/attenuator,
antenna, tuner, and other remote-base operating functions through a typed
capability catalog. Backend-specific operating functions use declared typed
settings with units and constraints. Transport credentials, raw CAT commands,
firmware writes, factory reset, and configuration-file management are not
remote-base operating functions.

Move or expose the existing parallel implementation through the same service;
do not write another bit-banging implementation. Preserve active-low four-bit
channel selection and the existing two-word RTX sequence, including shared
pin ownership and PTT behavior. Use Hamlib equivalents when they actually
support the selected device. Otherwise retain parallel control. Backend
selection is explicit; there is no automatic switch of physical ownership.

The current RTX path parses tone and H/L power fields but does not program
those capabilities. Preserve supported behavior and report the real
capabilities; do not advertise working power/tone control based only on the
accepted text syntax. DSP/external signaling remains independently selectable.

Audit existing USBRadioPlus native and ASL entry points into a parity table.
Every non-Asterisk radio operation must either reach this shared service or
remain in an existing shared audio/GPIO service available to both hosts.
Include USB-device reassignment and hardware local-repeat controls where the
hardware supports them. Honor the shared configured diagnostics interval
instead of a standalone-only fixed interval. No physical backend is allowed
to silently ignore an accepted operating request.

## Audio and signaling

Rig control does not transport PCM. Continue using the shared 48 kHz F32 audio
path, shared processing profiles, and existing receive/transmit owners. Select
CM119 or Linux-supported onboard audio independently from CAT. A Hamlib model
does not imply a usable audio-streaming API. Existing supported streaming
audio adapters may be selected; new streaming backends are separate work.

PTT, COR, CTCSS encode, CTCSS decode, and DCS each have one explicit source.
CTCSS encode and decode are independent: disabled, shared DSP, an external
device, or the rig when its backend supports that function. Prefer Hamlib for
an otherwise unassigned supported hardware function, as ADR 0028 requires.
Conflicting physical assignments are invalid. Rig tone frequency and enable
are separate operations; activity-scoped encode policy remains shared.

Decoder programming and actual receive qualification are distinct. Hamlib's
tone/code getters return configured values, while its generic DCD getter is
not proof of CTCSS/DCS detection. Keep the DSP or external GPIO qualifier
explicit. Where a radio's decoder is programmed through CAT but its qualified
signal comes from GPIO, configure `decode_control_source = hamlib` separately
from the decode qualification source. Programming otherwise follows the
selected decoder. Reject unsupported qualification rather than substitute raw
carrier detect, an enabled squelch function, configured tone, or audio level.
Do not add unverified model-specific tone detection in this implementation.

For non-FM operation, keep controller transmit admission independent of
receive qualification. Linked audio can request PTT even while receive is
open. A physically half-duplex radio may interrupt receive while transmitting;
simultaneous RF receive/transmit is not required. Suppress its receive route
during its own transmission. A genuinely full-duplex radio can retain receive
according to configured policy. Do not let unreliable AM/SSB squelch prevent
a linked peer from transmitting.

Retain ADR 0004's speech-squelch and SSB-selection requirements without adding
a second detector here. Shared speech/noise qualification, when available,
also supplies scan activity. The documented fallback is conventional noise
squelch, then open receive. A mode whose effective receive qualification is
always open cannot use carrier-based automatic scan resume; report that
condition and require a usable configured scan qualification or manual hold.

## Limits and tuning

Configure permitted RX ranges, TX ranges, modes, maximum power, and receive-only
ranges per node. Enforce them in the shared service for DTMF, ASL commands,
memory recall, scan steps, and every other ingress. Check effective transmit
frequency after split, offset, and XIT, and effective receive tuning after RIT.
Use integer Hz at the internal boundary. Do not hardcode audio or RF levels.

Apply a multi-setting request as one logical tune transaction:

1. Validate the complete desired state and backend capabilities.
2. Wait for actual PTT to become inactive. An explicit operator tune/recall
   request may interrupt receive after its DTMF source has finished the
   command; momentarily close that receive route while changing the rig.
   Do not wait forever for open AM/SSB receive to become idle. Automatic scan
   steps instead require no qualified receive or linked activity. Keep
   serving emergency unkey while waiting; stale work is discarded.
3. Inhibit new transmit admission, apply the prepared settings on the rig owner,
   and read back values when supported.
4. Publish one coherent applied state and release inhibition after success.

Hardware writes are not physically atomic. A partial failure attempts a safe
restore; if state cannot be established, retain transmit inhibition and report
the first failure. Write-only backends report applied-but-unverified status.
Do not claim watts-based power enforcement unless the backend can reliably
convert or establish that limit. Unsupported required safety controls prevent
that remote-base configuration from becoming operational.

Observe front-panel changes when the backend can report them. Out-of-policy
receive tuning closes the receive route; invalid or unknown transmit state
inhibits PTT. Startup, disconnect, and shutdown begin with transmit inhibited.
An unsupported request reports a localized error instead of a success tone.

## Memories

Each named memory has a stable numeric DTMF selector and a complete supported
operating state: RX/TX frequency, mode/passband, split/offset, power, and
independent encode/decode settings. A parallel channel-selection memory may
instead name its channel and configured frequency/policy metadata. There is
no fixed 100-memory software limit.

Support list, query, recall, and a readback of the current operating state.
User decision: memories and scan lists are defined only in configuration.
DTMF and other operating interfaces do not create, save, rename, delete, or
persistently edit their definitions. There is no live-override state file.
Ordinary live tuning and transient scan state do not rewrite configuration;
restart selects the configured startup memory. Do not copy memories into
each radio's proprietary memory store; that is unnecessary for this scope.

## Scanning

Scan lists name ordered memory selectors. Named memories provide frequency,
mode, tone, and safety policy, so scans use ordinary validated tune operations
even when the rig has no native scan API. Support list selection, start,
pause/resume, stop, direction, dwell, post-activity hold, member skip, and an
optional priority member. Status identifies the list, member, hold reason,
next action, and most recent tune failure.

Approved defaults: dwell 500 ms after tune completion and configured settle
time, resume 2 seconds after qualified receive activity ends, priority scanning
disabled. Use monotonic deadlines on the non-audio owner. Remain on a busy
member for the whole receive transmission. Do not make a priority excursion
or retune during a local or linked transmission. A failed member is reported
and skipped; all members failing stops the scan and inhibits unsafe transmit.

A linked transmit request pauses the scan before further tuning. Approved
transmit policy: use the currently applied scan member only if its complete
state permits transmit. Do not retune to another memory while admitting an
incoming transmission or add another pre-key audio queue. A receive-only
member denies PTT and reports the reason; an operator may stop the scan and
recall a transmit-capable memory before transmitting. Do not advertise a
partially applied scan step as current. Code inspection found no existing
pre-key retune allowance: transmit settle time begins after PTT and is not
such an allowance. Add zero buffering or retune grace. If transmit demand
begins during an in-flight scan tune, reject that burst until its sources
unkey and count the rejection; do not key a partial state or replay old audio.

Scan stop leaves the currently applied member selected; explicit memory
recall stops the scan and selects the requested memory instead. Shutdown or
restart does not resume a live scan automatically; it restores the configured
safe startup memory.

## DTMF and replies

Reuse the existing source-owned collector, command map, administrative grants,
timeout, muting, and addressed `*4` forwarding. Per-node remote-base DTMF can
be open, require the existing hashed-code unlock, or be disabled. Public status
is readable under existing policy. An unlock does not cross source/connection
identities. Existing default link commands, `*70`, `*722`, `*806`, and `*816`
retain their meaning.

Use the approved remappable `*6` remote-base command family. Familiar ASL remote
commands follow that prefix; this avoids installing ASL's conflicting bare
`*2`, `*3`, or `*721` commands. Each request is independent, not a persistent
new command mode. The typed remote-base parser owns argument grammar; radio
parameters are not stored in the current link command's `node` string.

| Command | Operation |
| --- | --- |
| `*60<selector>#` | Recall a named memory. |
| `*61<MHz>*<fraction>*<offset>#` | Familiar ASL frequency entry; offset 1 minus, 2 simplex, 3 plus. |
| `*62<tone>#`, `*63<tone>#` | Set RX decode / TX encode CTCSS respectively, matching actual ASL implementation. |
| `*640#` through `*643#` | RX decode off/on, TX encode off/on. |
| `*644#` through `*646#` | Configured low/medium/high power choices when supported. |
| `*651#`, `*652#` | Detailed/short remote-base status. |
| `*661#` through `*664#` | FM, USB, LSB, AM. |
| `*6711#`, `*6714#`, `*6717#` | Step down 20/100/500 Hz. |
| `*6713#`, `*6716#`, `*6719#` | Step up 20/100/500 Hz. |
| `*6721#`, `*6724#`, `*6727#` | Downward scan, slow/medium/fast presets. |
| `*6723#`, `*6726#`, `*6729#` | Upward scan, slow/medium/fast presets. |
| `*679#` | Bounded tune operation, if supported and admitted by safety policy. |
| `*69...#` | Explicit extended operations for remaining capabilities and configured memories/scan lists. |

Reserve fixed extended subcommands during implementation planning for VFO,
exact-Hz RX/TX, arbitrary supported modes/passbands, split/offset, RIT/XIT,
power units, CTCSS/DCS/source selection, other advertised typed settings,
memory/list query, scan selection/hold/resume/stop, and immediate
transmit inhibit. Numeric capability selectors and enum values are stable
published identifiers, not Hamlib's changing numeric model or flag values.
Every supported remote-base operating action must have a DTMF representation;
none may be marked CLI/REST-only for convenience.

Accept ordinary 12-key pads. Within a recognized remote-base request, `*` is
an argument separator rather than an accidental reset. `#` or source unkey
terminates collection; the existing collection timeout remains a fallback.
Specify and test complete grammars before implementation, including fractional
frequency/tone precision, signed values, maximum argument lengths, and prefix
ambiguities. Per-node remapping remains available and collisions are rejected.

All successful changes and errors use the shipped i18n message bundle, with
separate speech/text/Morse variants and normal substitutions. Spoken status
waits for reception to end and returns only to the command source under
ADR 0023. Long catalogs are queried by selector or requested page, not spoken
as an unbounded list. CLI/control requests return results on their interface.
Existing `SETFREQ`/`SETCHAN` channel messages adapt into the same operations;
USBRadioPlus does not take over app_rpt's DTMF parser or send duplicate replies.

## Configuration sketch

These are proposed schema names, not settings available in the released code.
The same shared resolver consumes inheritable sections from
`rpt_advanced.conf` or `usbradioplus.conf`; standalone never needs Asterisk's
configuration files.

The shared PortAudio adapter already opens general devices and discovers
onboard USB audio. Its consumers currently require CM119 HID and CM119 mixer
paths, however. Supporting an onboard USB codec therefore requires making
CM119 ownership optional in the common hardware plan/service and using its
declared audio/mixer/signaling capabilities. Keep parallel GPIO independent.
Do not fabricate CM119 health, EEPROM, or mixer controls when no such device
exists. Reuse the existing audio stream/DSP implementation.

```ini
[remote_base]
enabled = no
dtmf_access = locked

[remote_base 524950]
enabled = yes
backend = hamlib
hamlib_model = RIG_MODEL_IC9700
hamlib_device = /dev/serial/by-id/EXAMPLE
hamlib_option_serial_speed = 115200
audio_source = cm119
startup_memory = local
allowed_rx_ranges_hz = 144000000-148000000
allowed_tx_ranges_hz = 144000000-148000000
allowed_modes = FM
max_power_w = 10
dtmf_prefix = 6

[remote_memory 524950 local]
selector = 1
rx_frequency_hz = 146520000
tx_frequency_hz = 146520000
mode = FM
power_w = 5
ctcss_encode_source = dsp
ctcss_encode_hz = 114.8
ctcss_decode_source = dsp
ctcss_decode_hz = 114.8

[remote_scan 524950 local_channels]
selector = 1
members = local,alternate
dwell_ms = 500
resume_delay_ms = 2000
```

The example model and band are illustrative; every memory and scan reference
must be defined and validated against the actual rig. Unsupported selections
must not be silently substituted. Unknown sections/options retain the
repository's warning/default policy; when safe defaults cannot establish
frequency, power, signaling, or ownership constraints, leave that radio
inactive and retain the prior working generation on reload.

## Verification and acceptance

Use TDD with the smallest failing shared test for each behavior. Verify both
host ingress paths reach the same dynamic implementation and capability
catalog. Test Hamlib's dummy backend and controlled serial/network fixtures;
parallel tests inspect existing adapter pin operations without driving RF.

Required focused checks include limits after offset/RIT/XIT, unsupported
capabilities, partial apply/readback failures, write-only state, external
tuning, PTT-off priority and blocked transport, no cross-rig blocking, stale
generations, scan activity/hold/stop behavior, configuration-only memories,
DTMF parsing/remapping/auth/source routing, and equivalent host behavior.
Assert no new Hamlib/GPIO calls, allocation, or synchronization enters the
audio callbacks. Package/link checks verify shared implementations, selected
adapter requirements, and independent USBRadioPlus/standalone installation.

Use the existing fast push checks and hosted full PR gates. Physical radio
validation is model-specific and must not be inferred from the dummy backend.
No node installation, restart, retune, scan, link change, or RF test is
authorized by approval of this design alone.

On implementation, update ADR 0004's stale physical-full-duplex clause and
DTMF coverage, the related wishlist entries, architecture boundaries, and
actual capability/status evidence. Keep them planned until code and required
validation are complete. Do not implement other wishlist items.

## Approved operating decisions

1. Memories and scan lists are configuration-only; no live persistent edits.
2. Scan defaults and TX behavior: 500 ms dwell, 2-second resume
   delay, no mid-transmission priority excursion, transmit on the currently
   applied member only when permitted, and stop leaves that member selected.
3. The `*6` command family and familiar-ASL mapping above. Exact extended
   subcommand numbers and complete argument grammar belong in the
   implementation plan before coding.

## Primary research and implementation evidence

- [Hamlib developer guide](https://raw.githubusercontent.com/Hamlib/Hamlib/master/README.developer)
- [Hamlib rig API](https://hamlib.sourceforge.net/manuals/4.7/group__rig.html)
- [Hamlib model names](https://raw.githubusercontent.com/Hamlib/Hamlib/master/include/hamlib/riglist.h)
- [Hamlib transport options](https://raw.githubusercontent.com/Hamlib/Hamlib/master/src/conf.c)
- [Hamlib rig ownership](https://raw.githubusercontent.com/Hamlib/Hamlib/master/src/rig.c)
- [Hamlib optional memory operations](https://raw.githubusercontent.com/Hamlib/Hamlib/master/src/mem.c)
- [ASL remote command implementation](https://raw.githubusercontent.com/AllStarLink/app_rpt/master/apps/app_rpt/rpt_functions.c)
- [ASL default command map](https://raw.githubusercontent.com/AllStarLink/app_rpt/master/configs/rpt/rpt.conf)

The actual ASL implementation sets RX tone in remote function 3 and TX tone
in function 4; current sample comments label them in the opposite direction.
Preserve behavior rather than copying that comment defect.

Local evidence: USBRadioPlus `rust/asl3/src/control.rs`,
`rust/driver/src/hardware_host.rs`, `rust/product/src/native.rs`,
`rust/product/src/maintenance.rs`; GPIO adapter public parallel operations;
controller `rust/core/src/command.rs`, `rust/core/src/runtime/dtmf.rs`, and
the shared native radio-maintenance boundary. Source reviewed against SDK
alpha27 and the controller alpha19 shared-radio-parity tree.
