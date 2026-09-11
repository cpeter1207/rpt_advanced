# ADR 0012: Runtime dependencies point inward through explicit layers

Status: Accepted

## Context

rpt_advanced must support an Asterisk module today and a standalone controller
later without coupling reusable radio and controller behavior to a particular
audio, telephony, or user-interface framework. Independently versioned
components need a consistent placement and dependency direction to avoid
recreating policy or hardware coupling in each library.

## Decision

Dependencies point inward: adapters call inward, node policy calls reusable
services, and real-time code calls only deterministic signal and radio
components. An inner layer never depends on an outer layer.

```text
CLI / REST / WebSocket / DTMF command mapping
Asterisk / IAX2 / EchoLink / Hamlib / PortAudio adapters
                         ↓
                  Node controller
             ┌───────────┼───────────┐
             ↓           ↓           ↓
      Control services  Media     Radio exchange
                         ↓           ↓
                  Signal components  librptadvradio
                         ↓           ↓
                    Hardware adapters / CM119
```

The layers are:

1. **Foundations:** `rate_adjusting_pcm_ring`, `librptadvconfig`, message
   templating, and generic scheduling. They are deterministic primitives with
   no node policy or hardware ownership.
2. **Signal DSP:** tone, Morse, DTMF, squelch, CTCSS, and DCS. These components
   are real-time safe: no I/O, locks, allocation, or Asterisk dependency.
3. **Radio core:** `librptadvradio` plus GPIO/parallel signaling components.
   It owns the native radio tick, duplex behavior, signal integration, and
   platform-neutral hardware callbacks.
4. **Media services:** `librptadvspeech` and audio-file preparation or
   transcoding. They prepare media outside real-time audio and own neither ID
   nor announcement policy.
5. **Connectivity services:** `librptadvdirectory`, `librptadvaccess`, and
   `librptadviax2` when implemented. They provide directory lookup,
   identity/access evaluation, and protocol mechanics without node routing
   policy.
6. **Node controller:** the shared controller core, presently reached through
   `app_rpt_advanced`. It owns node configuration semantics, identifiers,
   announcements, topology, links, timeouts, schedules, macros, and telemetry
   policy.
7. **Adapters:** the Asterisk module, USBRadioPlus legacy/modern adapters, the
   PortAudio/ALSA CM119 adapter, EchoLink, and Hamlib. They translate external
   APIs and device I/O into core contracts and own API-specific dependencies.
8. **User interfaces:** Asterisk CLI, REST, WebSocket, and DTMF command
   mapping. They validate requests, submit controller operations, and expose
   state; they never mutate real-time state directly.

DTMF detection and generation belong in Signal DSP, while DTMF command
interpretation and authorization belong in the node-controller/user-interface
boundary. IAX2 framing, negotiation, control messages, and media transport
belong in connectivity; AllStarLink link, topology, and RF routing policy
belong in the node controller.

Identifiers, announcements, duplex policy, runtime lifecycle, link-topology
policy, and Asterisk media glue are intentionally not separate shared
libraries. They evolve together as node behavior.

## Consequences

PortAudio, OSS, Asterisk, Hamlib, and network protocol dependencies cannot
leak into deterministic signal or portable radio components. Adapters require
explicit contracts for commands, events, buffers, and hardware callbacks.
Standalone replacement can reuse the inner layers while replacing outer
adapters. New components and interfaces must be placed in one layer and may
only depend on lower layers.
