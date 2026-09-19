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
   no node policy or hardware ownership. PCM-ring payloads use canonical
   normalized `f32` under ADR 0029.
2. **Signal DSP:** tone, Morse, DTMF, squelch, CTCSS, and DCS. These components
   are real-time safe: no I/O, locks, allocation, or Asterisk dependency.
3. **Radio core:** `librptadvradio` plus GPIO/parallel signaling components.
   It owns the radio-port audio engine (local receive and transmit workers), duplex behavior,
   RF-signaling edge publication, and platform-neutral hardware callbacks.
4. **Media services:** `librptadvspeech`, audio-file preparation or
   transcoding, and recorded-message preparation. They prepare filesystem,
   uploaded, or recorded media outside real-time audio and own neither ID nor
   announcement policy. One reserved station-telemetry audio producer writes
   the selected TTS or sound-file source into the telemetry-program ring; only
   one such source may play at a time.
5. **Connectivity services:** `librptadvdirectory`, `librptadvaccess`, and
   `librptadviax2` when implemented. They provide directory lookup,
   identity/access evaluation, asynchronous network I/O, link media ingress
   and egress, per-peer jitter buffering, codec work, and protocol mechanics
   without node routing policy.
6. **Node controller:** the shared controller core, presently reached through
   `app_rpt_advanced`. It owns node configuration semantics, identifiers,
   announcements, topology, links, timeouts, schedules, macros, and telemetry
   policy. Each node owns exactly one receiver and one transmitter; systems
   with more local radios use additional configured nodes connected through
   loopback or the local network. The radio-port transmit worker writes one native
   PCM block to a program-audio loopback; a link-audio distributor schedules
   link-specific encode/send work outside both radio audio workers.
7. **Adapters:** the Asterisk module, the single ASL3 compatibility adapter,
   control-path execution, PortAudio/ALSA CM119, EchoLink, and Hamlib. They
   translate external APIs and device I/O into core contracts and own
   API-specific dependencies.
   Each Rust--C adapter is a separate versioned Rust `dylib` shared object with
   the smallest required stable C-compatible descriptor/function-table ABI and
   no inner-layer policy, as defined by ADR 0021 for C-to-Rust entry and ADR
   0022 for Rust-to-C dependency calls. The latter includes FFmpeg graph,
   sample-rate conversion, Hamlib, speech synthesis, and PortAudio/ALSA
   adapters; it is not one aggregate external-services facade. Real-time
   adapters split setup/control from their preallocated lock-free tick.
   The audio adapter owns normalized `f32` PCM, device mixer access, and raw audio
   statistics; the Hamlib-backed radio-control adapter owns rig actions; the
   GPIO adapter owns CM119 and parallel-port pin I/O. ADR 0028 removes
   `res_usbradio` from these boundaries.
   ADR 0036 distinguishes Asterisk from ASL3: rpt_advanced may use public
   Asterisk APIs, while ASL3-specific dependencies are minimized and confined
   to the ASL3 adapter. No inner layer or rpt_advanced module requires it.
   Asterisk-specific thread handling stays in its adapters. ADR 0038's
   control-path adapter currently uses Asterisk's taskprocessor behind a
   neutral execution contract; scheduling and lifecycle policy remain in the
   controller, and standalone selects a non-Asterisk backend.
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

The pending 2026-09-13 amendment splits the radio core into input-driven local
receive and DAC/adapter-output-clocked transmit workers, both canonical-`f32`
and independently frame-bounded. Receive DSP publishes to its inbound ring;
that ring, per-link inbound rings, and the telemetry playout ring own all
conversion/drift recovery into the native transmit mix. Transmit fills the
adapter's output buffer directly after profile-selected signaling. Adapters
own device I/O and Asterisk representation/egress conversion. ADR 0027 defines
the two-worker contract, backend-specific partial output, and publication
cadence; no new runtime behavior is claimed by this amendment.

## Consequences

PortAudio, OSS, Asterisk, Hamlib, and network protocol dependencies cannot
leak into deterministic signal or portable radio components. Adapters require
explicit contracts for commands, events, buffers, and hardware callbacks.
Standalone replacement can reuse the inner layers while replacing outer
adapters. New components and interfaces must be placed in one layer and may
only depend on lower layers.

The detailed owner, queue, generation-publication, and safe-reload lifecycle
rules are defined by [ADR 0026](0026-generational-real-time-runtime-lifecycle.md).
