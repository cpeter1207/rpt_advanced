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
