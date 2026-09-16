# Architecture

This directory is the maintained architecture source for rpt_advanced. Read it
before designing a feature or structural change. Update it, the applicable ADR,
and the affected configuration and test documentation in the same change.

## System boundary

These are initial-version alpha releases. [ADR 0040](decisions/0040-initial-alpha-compatibility-policy.md)
supersedes backward-compatibility requirements for project-owned interfaces.
Do not retain compatibility-only code; preserve required current behavior and
external interoperability, and reject incompatible artifact combinations safely.

`app_rpt_advanced.so` is the metadata loader for an Asterisk-hosted Rust product.
The product owns controller configuration, node workers, local radio policy,
telemetry, and AllStarLink peer control; the Asterisk adapter owns channel/frame
exchange. USBRadioPlus remains the radio channel driver and owns hardware
access. The separately released `rate_adjusting_pcm_ring2` and samplerate
adapter DSOs provide playout buffering, clock-rate recovery, and edge conversion.

Stable reusable functions are progressively extracted as narrow, independently
versioned shared libraries. The approved component boundaries and the rule that
policy remains in the controller are defined by [ADR 0011](decisions/0011-versioned-functional-components.md).
Those planned libraries are not current runtime dependencies until an
extraction is complete.

The USBRadioPlus hardware cutover uses one channel implementation backed by
the dynamic PortAudio/ALSA and GPIO adapters, with no `res_usbradio` imports or
module requirement. Released alpha18's combined native tick runs in the
PortAudio playback callback. The current Rust migration instead implements
independently paced PortAudio input and output callback entry points; Asterisk
frame delivery and device-control I/O remain outside those callbacks. The
`app_rpt` 8 kHz and `RadioPlusAdvanced` 48 kHz protocols remain separate.
The audio, radio-control, and GPIO boundaries are defined by
[ADR 0028](decisions/0028-remove-res-usbradio-through-hardware-adapters.md).
The accepted 2026-09-13 amendment splits that tick into an input-driven local
receive worker and a DAC/adapter-output-clocked transmit worker. The callback
split is implemented in USBRadioPlus's Rust migration. The rpt_advanced Rust
product now implements generational `NodeHost` ownership, coherent paired
receive/transmit calls, per-peer inbound rings, and the program-audio loopback
dispatcher. This does not claim completion of USBRadioPlus's separate
direct-hardware local/telemetry-ring topology or shared-clock adapter cutover;
none of those changes was part of released alpha18.
Separately, [ADR 0039](decisions/0039-retire-usbradioplus-native-mode.md)
retires USBRadioPlus's driver-native software-repeat and native parrot modes.
They are not rpt_advanced prerequisites and will no longer be supported for
ASL3. The candidate removal under verification does not remove the native DSP engine, the new
receive/transmit worker design, or the separate `RadioPlusAdvanced` transport.
Appliance emissions/immunity acceptance targets and its signed-update trust
roles are defined by [ADR 0030](decisions/0030-appliance-update-trust-and-compliance.md).
The fully populated appliance interface, power, and thermal ceilings are
defined by [ADR 0032](decisions/0032-four-port-appliance-interface-and-power-ceiling.md).

The product implementation is Rust-owned. The only retained production C is the
minimal Asterisk metadata shim; it has no controller, media, radio, or control
policy. Each C-facing integration, whether C calls Rust or Rust calls C, is a
separate versioned Rust shared object with only its required
external ABI surface. Each adapter uses a stable C-compatible
descriptor/function-table contract and belongs to a complete selected-product
adapter manifest; Asterisk is the primary inbound case. ADRs
[0013](decisions/0013-rust-owned-implementation-and-asterisk-abi-shims.md) and
[0021](decisions/0021-versioned-rust-c-adapter-boundaries.md) define that
boundary; [ADR 0022](decisions/0022-versioned-external-c-dependency-adapters.md)
defines outbound external-dependency adapters.

### Current product artifacts

The loader selects these C-compatible descriptors. Cargo builds the C-facing
shared objects as `cdylib`; their stable contract is the versioned C table, not
Rust's private ABI.

| Artifact | Ownership |
| --- | --- |
| `librptadv_product.so.1` | Product lifecycle, configuration, controller policy, workers, and descriptor clients; embeds `rust/core` once |
| `librptadv_asterisk_adapter.so.1` | Public Asterisk application/CLI, channel, codec, directory, and frame services |
| `librptadv_control_asterisk_adapter.so.1` | Replaceable serialized control executor backed by Asterisk's taskprocessor |
| `librptadv_file_adapter.so.1` | Offline local-file decoding through FFmpeg |
| `librptadv_speech_adapter.so.1` | Offline Piper synthesis, direct WAV reading, and speech-only level adjustment |

The file and speech providers are independently replaceable and expose only
their own operation. Speech does not invoke FFmpeg; file decoding does not
invoke Piper. Their private process/WAV source is shared at build time, without
a support DSO or a duplicate controller core. The product converts their
source-rate PCM through the released ring2 before prepared playback. Asterisk
child-reaper coordination arrives through host callbacks, not product imports.
Adapter replacement requires quiescence and controlled reload/restart. The
retired combined media descriptor is not retained for initial-alpha compatibility.

## Layering

Runtime dependencies point inward: adapters call inward, node policy calls
reusable services, and real-time code only calls deterministic signal and radio
components. [ADR 0012](decisions/0012-layered-runtime-boundaries.md) defines
the layer responsibilities, allowed boundaries, and components intentionally
kept together as node policy.

Rust components are object-oriented: stateful objects encapsulate behavior and
collaborate through explicit interfaces and composition. [ADR 0014](decisions/0014-object-oriented-rust-design.md)
defines that rule and its real-time constraints.

Rust implementations use ownership, typed domain values, enums, `Result`, and
safe resource management rather than a mechanical C-style translation. Unsafe
code is confined to audited ABI and real-time-buffer boundaries as defined by
[ADR 0015](decisions/0015-idiomatic-high-level-rust.md).

The versioned REST API publishes its code-derived OpenAPI contract and
interactive documentation alongside the API as defined by
[ADR 0016](decisions/0016-code-derived-openapi-contract.md). Asterisk CLI,
REST, and configurable DTMF submit the shared controller operation catalog;
WebSocket is intentionally status-streaming only as defined by
[ADR 0023](decisions/0023-unified-control-and-dtmf-policy.md).

External and separately released project components are dynamic dependencies.
Deliberately separated private Rust components use `dylib`. Every C-facing
boundary is a separate versioned Rust `dylib` adapter shared object with only
its required stable C-compatible descriptor/function-table interface. External
C dependencies are reachable only through their corresponding adapter; leaf
implementation crates that are not component boundaries may remain internal to
their owning artifact. An adapter changes only at controlled restart or reload,
never from a real-time tick. See
[ADR 0018](decisions/0018-dynamic-linking-for-external-components.md),
[ADR 0020](decisions/0020-rust-dylibs-and-external-c-shims.md), and
[ADR 0021](decisions/0021-versioned-rust-c-adapter-boundaries.md), and
[ADR 0022](decisions/0022-versioned-external-c-dependency-adapters.md).

## Runtime structure

```text
Asterisk frames / link peers                 configuration reload
           |                                           |
           v                                           v
link media services ───────────────────────> station-control plane
           |                                           |
           |                                           +--> identifier, announcement, courtesy,
           |                                                speech, file, and Morse preparation
           v
radio-port audio engine <─ RadioPlusAdvanced exchange ─> USBRadioPlus / hardware
```

### Native media routing

```text
audio input → local receive worker
              squelch / CTCSS-DCS decode / deemphasis / receive DSP
                └─ local receive inbound PCM ring ────────────────┐
link packet → per-peer jitter / decode → peer inbound PCM ring ───┤
station telemetry producer → telemetry playout ring ──────────────┤
                                                                ▼
                      transmit worker ← DAC / adapter output demand
                      ├─ native-rate mix / transmit processing / Morse / tones
                      ├─ pre-access-tone program-audio loopback ring
                      │  └─ link distributor → per-link egress queues
                      │     └─ serial codec encode / send outside audio workers
                      └─ selected CTCSS or DCS → adapter-owned output buffer
                                                 (PortAudio callback buffer)
```

Link media ingress serializes concurrent asynchronous callbacks for one peer
before it touches that peer's jitter buffer, decoder, or receive-program ring.
The Asterisk-hosted adapter may retain a narrow ingress mutex, never acquired
by the radio-port audio engine. The planned standalone implementation instead
uses bounded producer-to-owner SPSC packet queues and one assigned media owner
per peer, with a fixed worker pool rather than a thread per peer. See
[ADR 0037](decisions/0037-standalone-lock-free-peer-ingress.md). The
local receive, linked-peer inbound, and telemetry playout rings alone own
source-to-transmit conversion and drift recovery. The transmit worker runs
entirely at the native output rate, with no mix/output resampler. The
complete media ownership and ordering contract is
[ADR 0025](decisions/0025-native-media-routing-and-pcm-ring-ownership.md).

### Radio-port audio ownership and lifecycle

```text
                                     station-control plane
           scheduler / configurator / station-control event dispatcher / reclaimer
                                                  │
                         build and validate G+1  │  publish active generation
                                                  ▼
                               ┌────────────────────────────────────┐
                               │ station host (`NodeHost`)          │
                               │ active RuntimeGeneration           │
                               └─────────────────┬──────────────────┘
                                                  │ separate hazard slots
                                     ┌────────────┴────────────┐
                                     ▼                         ▼
                              local receive                transmit
                              input-paced                  output-paced
                              RX DSP / decoder             mix / PTT / signaling
                              RX edge publisher            TX edge publisher
                                     │                         │
                                     └──── separate SPSC ──────┘
                                           notifications
                                                  ▼
                                station-control event dispatcher

retired G ──> wait for hazard slots, callback detachment, and queued work ──> reclaim G
```

The **radio-port audio engine** contains two independently paced serial owners.
The **local receive worker** runs on audio input and owns DSP squelch,
CTCSS/DCS decode, deemphasis, receive processing, and the processed local
receive ring's producer. The **transmit worker** runs at the DAC clock or
adapter-supplied output cadence, owns all inbound-ring consumers, native
mixing/transmit DSP, oscillator phase, PTT, and the pre-access-tone loopback
producer, and fills the supplied output buffer directly. Input and output
frame counts are independently setup-bounded, not paired. For PortAudio the
two callbacks call their respective workers; an extra OS worker thread is not
required. Neither worker has an Asterisk or direct device-I/O dependency.

If the adapter knows ADC and DAC share a clock and supplies aligned input and
output frames, it may call receive then transmit **back-to-back**. The local
ring then uses same-cycle unity-rate pass-through: no drift correction,
redundant resampler, prefill, or added handoff latency. Required device/DSP
latency remains. Unknown clock relationships use asynchronous ring recovery;
network and telemetry sources retain their own independent timing. The paired
call holds one coherent generation across both workers under ADRs 0026/0027.

Each worker has an **RF-signaling edge publisher** for its private state and
its own SPSC notification queue. Receive qualification crosses to transmit
through a generation-tagged lock-free handoff, not shared mutable DSP. The
publishers apply only prepared immediate actions and publish snapshots; they
do not evaluate macros, schedule work, route links, or format telemetry.

For an appliance direct-codec port, the selected hardware adapter applies a
prepared PTT or CTCSS action through the physical output interlock and reports
its armed/fault state. The interlock, not the tick, decides whether actuator
power reaches the radio or DB-25 output. [ADR 0034](decisions/0034-appliance-hardware-output-interlock.md)
defines that boundary.

The **station-control event dispatcher** consumes those notifications and owns
their high-level consequences: telemetry policy, courtesy and identifier
sequencing, macros, link control, status publication, and scheduling results.
Scheduler, configurator, and reclaimer are logical responsibilities of the
same serialized control owner; none runs in the radio-port audio engine. A fixed
worker pool may run link media ingress and egress work, but one peer never has
two ingress jobs or two egress jobs executing at once.

Those logical owners use adapter-neutral execution and lifecycle contracts.
The **control-path adapter** supplies serialized task execution, explicit
submission/ownership results, and safe stop/drain. Its current backend is the
Asterisk taskprocessor; standalone can select an owned or third-party backend
with the same contract. Scheduling and controller policy stay outside the
adapter. See [ADR 0038](decisions/0038-replaceable-control-path-adapter.md).
The Rust product submits through the neutral execution contract and its
validated control-descriptor client. Only the control provider owns Asterisk
taskprocessor handles. Product radio/peer workers and the periodic control
trigger use Rust-owned threads; ADR 0036 confines required Asterisk handling
to the integration adapters.

The station host persists across ordinary reloads. It atomically
publishes a prepared immutable `RuntimeGeneration`; the radio-port audio engine
workers each latch one generation for a complete call using separate hazard
slots. A reload is applied only when both have adopted it; generation-owned
rings prevent old receive output entering a new transmit mix. Hazard slots and
generation-tagged work prevent reclamation while any owner may still use a
retired generation. The control owner waits for safe quiescence and frees the
retired generation only after protected users, queued work, and old callbacks
are gone. [ADR 0026](decisions/0026-generational-real-time-runtime-lifecycle.md)
defines the full reload, hardware-handoff, and failure policy.

- `module/app_rpt_advanced_loader.c` supplies only Asterisk module metadata and
  passes selected descriptors to the versioned Rust Asterisk entry adapter.
- `rust/product/` owns product lifecycle, node/radio/peer workers, runtime
  composition, and clients of the host, control, file, and speech descriptors.
  It embeds the controller core once and imports no Asterisk API.
- `rust/asterisk/` owns public-Asterisk registration and host services:
  applications, CLI, channels, frames, codec selection, directory lookup, and
  radio reservation. It does not embed the controller core.
- `rust/control-asterisk-adapter/` owns only control-executor backend resources.
  `rust/file-adapter/` and `rust/speech-adapter/` compile the private
  `rust/media-support/` source into separate capability providers.
- `rust/core/src/controller/`, `rust/core/src/policy/`, and
  `rust/core/src/audio/` implement node transmit ownership, duplex behavior,
  hang time, identification, and telemetry sequencing and rendering.
- One node owns one receiver and one transmitter. Multiple local radios use
  additional configured nodes joined through loopback or the local network;
  voter and simulcast timing is defined by
  [ADR 0024](decisions/0024-voting-receivers-and-simulcast-timing.md).
- `rust/core/src/schedule/`, `rust/core/src/schedule.rs`, and
  `rust/core/src/template.rs` parse civil-time triggers and replacement
  windows, constrain macro operations, and render scheduled messages. The
  control plane runs them; they never execute in a radio worker.
- `rust/core/src/runtime/` materializes configuration-owned permanent-link
  intent and local-time replacement windows. It snapshots lock-free local/link
  receive activity and returns ordinary attach or detach work to the serialized
  control plane; it never changes a peer from a radio worker.
- `rust/core/src/link/`, `rust/product/src/link/`, and `rust/asterisk/src/link/` implement
  AllStarLink admission, peer media, topology, advisory keyed-source queries,
  and permitted DTMF control. A direct receive edge starts the canonical
  legacy-compatible `K?` exchange and repeats it once per active second. Peer
  readers strictly parse canonical `K?`/`K` text, while the hub queues replies
  and non-ingress relays for reader-owned IAX delivery. The hardware-paced path
  only advances lock-free query state and reads the lock-free advisory source
  snapshot. The first valid keyed downstream response to each sent query wins;
  the latest accepted response is used at unkey and the direct peer remains the
  fallback. The unkey edge cancels unsent locally originated queries and
  rejects late replies. This work never tags PCM frames or runs an IAX write in
  an audio callback.
  A dedicated external-peer adapter owns EchoLink registration, UDP transport,
  and callsign access control while using the same controller audio boundary.
- `rust/core/src/config/` parses, validates, and inherits configuration.

## Core invariants

- Audio operations are lock-free. Control-plane work, configuration parsing,
  media preparation, and process execution never occur in an audio callback.
- Audio scheduling is best-effort: prefer FIFO priority 99, otherwise obtain
  the highest permitted priority, and continue with inherited scheduling if no
  increase is possible. Elevation denial alone is not an audio-start failure.
  See the pending implementation amendment in
  [ADR 0028](decisions/0028-remove-res-usbradio-through-hardware-adapters.md).
- Standalone code uses thread-safe ownership, messages, and atomics instead
  of locks wherever possible. Concurrent packet producers hand off through
  bounded lock-free queues; only the assigned peer owner writes its inbound
  PCM ring. ADR 0037 does not change the current Asterisk module.
- Each radio audio worker holds one immutable runtime generation for a complete
  call. Reload publishes a fully prepared replacement atomically and does
  not free a retired generation until its safe-quiescence grace period ends.
  See [ADR 0026](decisions/0026-generational-real-time-runtime-lifecycle.md).
- Internal controller processing uses canonical normalized `f32` PCM at
  48,000 Hz. RNNoise and voice-processing graphs run directly at that rate;
  higher native rates are unsupported. Inbound rings alone own conversion to
  native transmit rate; outbound codecs and private detector decimation remain
  distinct boundaries. See
  [ADR 0029](decisions/0029-canonical-f32-internal-pcm.md) and
  [ADR 0035](decisions/0035-fixed-48khz-native-audio.md).
- Local receive, per-peer inbound, and telemetry playout PCM rings own source-rate
  conversion and elastic clock recovery for native playout; their statistics
  remain observable for diagnosis. See
  [ADR 0025](decisions/0025-native-media-routing-and-pcm-ring-ownership.md).
- Native rate is fixed at 48 kHz for an open stream. Receive follows input
  availability; transmit follows DAC/adapter output demand at that native rate.
  A verified shared-clock adapter may call receive then transmit in one turn
  without local ring-added latency; equal nominal rates alone are insufficient.
  The workers support independent bounded frame counts and elapsed-sample
  timing. Source drift is corrected after receive DSP in its inbound ring, not
  by the transmit mixer or a second output queue. Asterisk representation and
  egress conversion plus partial device I/O remain adapter responsibilities.
  Edge events publish after their detecting worker call; meter/FIFO snapshots
  default to a 50 ms per-node interval with global fallback. See
  [ADR 0027](decisions/0027-variable-frame-native-tick-and-adapter-io.md).
- A valid configuration reload replaces workers without restarting Asterisk;
  invalid configuration leaves the running configuration intact.
- Unknown configuration sections and parameters are warnings and are ignored.
  Unknown, malformed, or unsupported values are warnings resolved through
  inheritance and a sensible default; only an irrecoverable inability to build
  a safe deterministic configuration rejects reload. See
  [ADR 0019](decisions/0019-tolerant-configuration-resolution.md).
- USBRadioPlus preserves the 8 kHz `app_rpt` interface through one ASL3
  compatibility adapter after the ADR 0028 hardware cutover. The separate thin
  rpt_advanced interface exchanges native 48 kHz PCM; resource-module API
  differences must not create duplicate channel implementations.
- `app_rpt_advanced` may depend on public Asterisk APIs, not on ASL3. Any
  unavoidable ASL3-specific code is confined to its optional compatibility
  adapter under [ADR 0036](decisions/0036-asterisk-without-asl3-dependency.md).
  Minimize new Asterisk dependencies and keep them in the thin module adapter.
  The hardware appliance requires neither Asterisk nor ASL3 in its build,
  installed packages, runtime services, or hardware-control path.
  Controller execution and lifetime contracts are independent of Asterisk's
  thread model. The control-path adapter may use its taskprocessor under
  ADR 0038; other necessary Asterisk API thread handling stays in its adapter.
  Standalone selects a non-Asterisk control backend.
- Telemetry is serialized. Speech is preferred and falls back to Morse when
  speech cannot be prepared.
- WebSocket streaming is status-only. CLI, REST, and DTMF use the shared
  controller operation catalog under ADR 0023.
- Scheduled work is wall-clock control-plane work. A due event queues its
  optional message before its validated controller operation. It settles the
  dispatch before releasing the runtime lock and preserves same-time
  configuration order. The ticker submits one FIFO control task each second,
  while runtime calendar-minute de-duplicates event occurrences. This lets a
  configured post-window quiet-time deadline expire promptly without entering a
  radio or audio callback. The detailed civil-time, reload, and no-catch-up
  policy is defined by [ADR 0017](decisions/0017-scheduler-route-lifecycle-and-civil-time.md).
- Control-path execution is a replaceable adapter capability, not scheduling
  policy or a media queue. Its Asterisk taskprocessor backend preserves current
  FIFO execution and failure/reload behavior. Standalone media fan-in under
  ADR 0037 remains separate from that control executor.
- A configured replacement window withdraws its named permanent route before it
  attaches the replacement, and withdraws the replacement before restoring the
  permanent route. Post-window quiet-time decisions use only local or linked
  receive activity. Matching replacement windows intentionally form a union;
  topology admission is their only conflict gate. ADR 0017 defines the required
  continuous route-ownership, retry-gating, and activity-presence semantics for
  scheduler and hub changes.

## Change procedure

1. Read this map and the relevant ADRs before choosing a design.
2. Preserve the core invariants or add an ADR explaining the approved change.
3. Update this map when ownership, boundaries, data flow, or invariants change.
4. Add or supersede an ADR for a material architectural decision.
5. Update tests, configuration documentation, examples, and manuals with the
   same interface change.
6. When a change affects the appliance, synchronize the appliance PRD and
   hardware architecture with this map, applicable ADRs, and the wishlist in
   their owner-controlled repositories. If a requested change conflicts with
   an accepted decision or documented requirement, identify that conflict and
   obtain the product owner's explicit confirmation before resolving it.
7. After every architecture change, run a fresh traceability review across the
   software architecture, ADRs, wishlist, appliance PRD, and hardware
   architecture. Record and resolve any inconsistency before considering the
   architecture change complete.

## Decisions

See [the ADR index](decisions/README.md).
