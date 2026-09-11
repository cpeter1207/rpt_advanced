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
| [0013](0013-rust-owned-implementation-and-asterisk-abi-shims.md) | Accepted | Rust owns implementation; C is limited to Asterisk ABI shims. |
| [0014](0014-object-oriented-rust-design.md) | Accepted | Rust implementation uses object-oriented composition. |
| [0015](0015-idiomatic-high-level-rust.md) | Accepted | Rust uses high-level, idiomatic domain modeling behind narrow ABI boundaries. |
| [0016](0016-code-derived-openapi-contract.md) | Accepted | REST uses a code-derived OpenAPI contract and colocated interactive documentation. |
| [0017](0017-scheduler-route-lifecycle-and-civil-time.md) | Accepted | Scheduler recovery has continuous route ownership and explicit civil-time semantics. |
| [0018](0018-dynamic-linking-for-external-components.md) | Accepted | External and separately released project components are dynamically linked. |
| [0019](0019-tolerant-configuration-resolution.md) | Accepted | Unknown configuration resolves through warnings and sensible defaults. |
