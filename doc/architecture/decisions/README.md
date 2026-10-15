# Architectural decision records

ADRs record decisions that constrain future implementation. New ADRs use the
next zero-padded number and state the context, decision, consequences, and
status. Superseded records remain and point to their replacement.

`Accepted` records an approved decision, not a claim that every capability is
implemented. Scoped implementation statuses distinguish current compositions
from planned features and optional hardware paths.

| ADR | Status | Decision |
| --- | --- | --- |
| [0001](0001-reload-without-asterisk-restart.md) | Accepted | Valid configuration reload replaces workers without restarting Asterisk. |
| [0002](0002-lock-free-audio-paths.md) | Accepted | Audio paths are lock-free. |
| [0003](0003-shared-rate-adjusting-pcm-ring.md) | Accepted | Clock recovery uses the shared rate-adjusting PCM ring. |
| [0004](0004-hamlib-remote-base-boundary.md) | Accepted | Hamlib owns rig control; audio uses an explicit source adapter. |
| [0005](0005-standalone-core-and-optional-asterisk-adapters.md) | Implemented | A shared lock-free core serves standalone operation and optional Asterisk adapters. |
| [0006](0006-appliance-control-plane-ingress.md) | Accepted | The appliance hosts its OIDC, proxy, and WAF ingress layer. |
| [0007](0007-echolink-audio-peer-boundary.md) | Accepted | EchoLink is an audio-only external-peer adapter. |
| [0008](0008-transmitter-protection.md) | Accepted | Per-node watchdogs and source-specific kerchunk handling protect transmitters. |
| [0009](0009-scheduled-actions-and-macros.md) | Accepted | Scheduled actions and macros are validated control-plane work. |
| [0010](0010-configured-permanent-links-and-replacement-windows.md) | Accepted | Configuration owns permanent direct links and bounded local-time replacement windows. |
| [0011](0011-versioned-functional-components.md) | Implemented (initial existing-function extraction) | Stable radio and controller functions use narrow, independently versioned shared-library boundaries. |
| [0012](0012-layered-runtime-boundaries.md) | Accepted | Runtime dependencies point inward through explicit layers. |
| [0013](0013-rust-owned-implementation-and-asterisk-abi-shims.md) | Implemented; boundary scope partially superseded | Rust owns implementation; C is limited to Asterisk ABI shims. |
| [0014](0014-object-oriented-rust-design.md) | Accepted | Rust implementation uses object-oriented composition. |
| [0015](0015-idiomatic-high-level-rust.md) | Accepted | Rust uses high-level, idiomatic domain modeling behind narrow ABI boundaries. |
| [0016](0016-code-derived-openapi-contract.md) | Accepted | Control APIs are versioned and code-derived. |
| [0017](0017-scheduler-route-lifecycle-and-civil-time.md) | Accepted | Scheduler recovery has continuous route ownership and explicit civil-time semantics. |
| [0018](0018-dynamic-linking-for-external-components.md) | Partially superseded | External and separately released project components are dynamically linked. |
| [0019](0019-tolerant-configuration-resolution.md) | Accepted | Unknown configuration resolves through warnings and sensible defaults. |
| [0020](0020-rust-dylibs-and-external-c-shims.md) | Partially superseded | Rust `dylib`s separate private components. |
| [0021](0021-versioned-rust-c-adapter-boundaries.md) | Implemented (current Asterisk entry boundary) | Versioned adapter shared objects isolate C-to-Rust boundaries. |
| [0022](0022-versioned-external-c-dependency-adapters.md) | Implemented (current selected capabilities) | Stable C contracts isolate external implementations, including standalone system DNS; future capabilities remain planned. |
| [0023](0023-unified-control-and-dtmf-policy.md) | Accepted | CLI, REST, and DTMF share a controlled operation model. |
| [0024](0024-voting-receivers-and-simulcast-timing.md) | Accepted | Voting and simulcast use explicit clock and node boundaries. |
| [0025](0025-native-media-routing-and-pcm-ring-ownership.md) | Implemented (telemetry streaming) | The telemetry producer streams every media type through its PCM ring; transmit only mixes consumed PCM. |
| [0026](0026-generational-real-time-runtime-lifecycle.md) | Implemented (current independently paced composition) | Station generations provide lock-free real-time ownership and safe reload reclamation. |
| [0027](0027-variable-frame-native-tick-and-adapter-io.md) | Partially implemented (verified shared-clock adapter pending) | Independent RX/TX callbacks, telemetry streaming, and snapshot intervals are implemented; paired shared-clock callbacks remain planned. |
| [0028](0028-remove-res-usbradio-through-hardware-adapters.md) | Implemented (dependency removal) | Hardware adapters replace res_usbradio; one ASL3 compatibility implementation replaces its legacy/modern split. |
| [0029](0029-canonical-f32-internal-pcm.md) | Accepted | Internal PCM is normalized `f32`; Asterisk and hardware convert at their boundaries. |
| [0030](0030-appliance-update-trust-and-compliance.md) | Accepted | Appliance compliance targets and signed-update roles are explicit. |
| [0031](0031-five-port-appliance-power-and-thermal-budget.md) | Superseded | Replaced by the four-port appliance decision. |
| [0032](0032-four-port-appliance-interface-and-power-ceiling.md) | Accepted | Four-port interface, USB-power, and thermal ceilings are explicit. |
| [0033](0033-appliance-radio-signaling-capability-profile.md) | Accepted | Appliance ports use external discrete CTCSS and profile-selected DSP signaling. |
| [0034](0034-appliance-hardware-output-interlock.md) | Accepted | Appliance actuator power is physically fault-gated and requires explicit re-arm. |
| [0035](0035-fixed-48khz-native-audio.md) | Implemented (fixed native rate) | Native radio/controller audio remains 48 kHz; inbound rings own conversion/drift into transmit, with outbound codec conversion separate. |
| [0036](0036-asterisk-without-asl3-dependency.md) | Implemented (current Asterisk and standalone compositions) | rpt_advanced uses adapter-neutral execution; Asterisk thread handling stays in adapters, and ASL3-specific code stays in its compatibility adapter. |
| [0037](0037-standalone-lock-free-peer-ingress.md) | Implemented, released, and live-validated | Tokio per-connection owners use a versioned SpeexDSP jitter adapter; PCM consumers remain lock-free. |
| [0038](0038-replaceable-control-path-adapter.md) | Implemented (Asterisk and standalone backends) | Control execution uses a replaceable adapter backed by the Asterisk taskprocessor or a standalone owned worker. |
| [0039](0039-retire-usbradioplus-native-mode.md) | Implemented | Remove USBRadioPlus native software-repeat and native parrot modes; preserve shared native DSP and controller transport. |
| [0040](0040-initial-alpha-compatibility-policy.md) | Accepted | Initial-alpha project interfaces do not require backward compatibility; remove compatibility-only code while rejecting mismatched artifacts safely. |
| [0041](0041-fluent-localized-controller-telemetry.md) | Accepted | Built-in RF telemetry uses validated Fluent catalogs with independent text, speech, and Morse forms. |
| [0042](0042-activity-scoped-ctcss-encode.md) | Implemented | Activity-scoped CTCSS transmit control is returned independently from PTT by the native radio callback. |
