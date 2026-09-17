# Architectural decision records

ADRs record decisions that constrain future implementation. New ADRs use the
next zero-padded number and state the context, decision, consequences, and
status. Superseded records remain and point to their replacement.

| ADR | Status | Decision |
| --- | --- | --- |
| [0001](0001-reload-without-asterisk-restart.md) | Accepted | Valid configuration reload replaces workers without restarting Asterisk. |
| [0002](0002-lock-free-audio-paths.md) | Accepted | Audio paths are lock-free. |
| [0003](0003-shared-rate-adjusting-pcm-ring.md) | Accepted | Clock recovery uses the shared rate-adjusting PCM ring. |
| [0004](0004-hamlib-remote-base-boundary.md) | Accepted | Hamlib owns rig control; audio uses an explicit source adapter. |
| [0005](0005-standalone-core-and-optional-asterisk-adapters.md) | Accepted | A shared lock-free core serves standalone operation and optional Asterisk adapters. |
| [0006](0006-appliance-control-plane-ingress.md) | Accepted | The appliance hosts its OIDC, proxy, and WAF ingress layer. |
| [0007](0007-echolink-audio-peer-boundary.md) | Accepted | EchoLink is an audio-only external-peer adapter. |
| [0008](0008-transmitter-protection.md) | Accepted | Per-node watchdogs and source-specific kerchunk handling protect transmitters. |
| [0009](0009-scheduled-actions-and-macros.md) | Accepted | Scheduled actions and macros are validated control-plane work. |
| [0010](0010-configured-permanent-links-and-replacement-windows.md) | Accepted | Configuration owns permanent direct links and bounded local-time replacement windows. |
| [0011](0011-versioned-functional-components.md) | Accepted | Stable radio and controller functions use narrow, independently versioned shared-library boundaries. |
| [0012](0012-layered-runtime-boundaries.md) | Accepted | Runtime dependencies point inward through explicit layers. |
| [0013](0013-rust-owned-implementation-and-asterisk-abi-shims.md) | Partially superseded | Rust owns implementation; C is limited to Asterisk ABI shims. |
| [0014](0014-object-oriented-rust-design.md) | Accepted | Rust implementation uses object-oriented composition. |
| [0015](0015-idiomatic-high-level-rust.md) | Accepted | Rust uses high-level, idiomatic domain modeling behind narrow ABI boundaries. |
| [0016](0016-code-derived-openapi-contract.md) | Accepted | Control APIs are versioned and code-derived. |
| [0017](0017-scheduler-route-lifecycle-and-civil-time.md) | Accepted | Scheduler recovery has continuous route ownership and explicit civil-time semantics. |
| [0018](0018-dynamic-linking-for-external-components.md) | Partially superseded | External and separately released project components are dynamically linked. |
| [0019](0019-tolerant-configuration-resolution.md) | Accepted | Unknown configuration resolves through warnings and sensible defaults. |
| [0020](0020-rust-dylibs-and-external-c-shims.md) | Partially superseded | Rust `dylib`s separate private components. |
| [0021](0021-versioned-rust-c-adapter-boundaries.md) | Accepted | Versioned adapter shared objects isolate C-to-Rust boundaries. |
| [0022](0022-versioned-external-c-dependency-adapters.md) | Accepted | Stable C contracts isolate replaceable external implementations. |
| [0023](0023-unified-control-and-dtmf-policy.md) | Accepted | CLI, REST, and DTMF share a controlled operation model. |
| [0024](0024-voting-receivers-and-simulcast-timing.md) | Accepted | Voting and simulcast use explicit clock and node boundaries. |
| [0025](0025-native-media-routing-and-pcm-ring-ownership.md) | Accepted | Local receive, peer, and telemetry inbound rings alone own conversion and drift recovery into native transmit mixing. |
| [0026](0026-generational-real-time-runtime-lifecycle.md) | Accepted | Station generations provide lock-free real-time ownership and safe reload reclamation. |
| [0027](0027-variable-frame-native-tick-and-adapter-io.md) | Accepted | Input-driven receive and DAC/adapter-clocked transmit replace the combined tick; verified shared clocks permit back-to-back calls without adaptive local drift recovery, with target reserve equal to configured squelch delay and direct output-buffer rendering (implementation pending). |
| [0028](0028-remove-res-usbradio-through-hardware-adapters.md) | Accepted | Hardware adapters replace res_usbradio; one ASL3 compatibility implementation replaces its legacy/modern split. |
| [0029](0029-canonical-f32-internal-pcm.md) | Accepted | Internal PCM is normalized `f32`; Asterisk and hardware convert at their boundaries. |
| [0030](0030-appliance-update-trust-and-compliance.md) | Accepted | Appliance compliance targets and signed-update roles are explicit. |
| [0031](0031-five-port-appliance-power-and-thermal-budget.md) | Superseded | Replaced by the four-port appliance decision. |
| [0032](0032-four-port-appliance-interface-and-power-ceiling.md) | Accepted | Four-port interface, USB-power, and thermal ceilings are explicit. |
| [0033](0033-appliance-radio-signaling-capability-profile.md) | Accepted | Appliance ports use external discrete CTCSS and profile-selected DSP signaling. |
| [0034](0034-appliance-hardware-output-interlock.md) | Accepted | Appliance actuator power is physically fault-gated and requires explicit re-arm. |
| [0035](0035-fixed-48khz-native-audio.md) | Accepted | Native radio/controller audio remains 48 kHz; inbound rings own conversion/drift into transmit, with outbound codec conversion separate. |
| [0036](0036-asterisk-without-asl3-dependency.md) | Accepted | rpt_advanced uses adapter-neutral execution; Asterisk thread handling stays in adapters, and ASL3-specific code stays in its compatibility adapter. |
| [0037](0037-standalone-lock-free-peer-ingress.md) | Accepted | Standalone ingress uses bounded SPSC fan-in and one media owner per peer, without mutex serialization or per-peer threads. |
| [0038](0038-replaceable-control-path-adapter.md) | Accepted | Control execution uses a replaceable adapter, initially backed by the Asterisk taskprocessor. |
| [0039](0039-retire-usbradioplus-native-mode.md) | Accepted | Remove USBRadioPlus native software-repeat and native parrot modes; preserve shared native DSP and controller transport (candidate under verification). |
| [0040](0040-initial-alpha-compatibility-policy.md) | Accepted | Initial-alpha project interfaces do not require backward compatibility; remove compatibility-only code while rejecting mismatched artifacts safely. |
