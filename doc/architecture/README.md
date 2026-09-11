# Architecture

This directory is the maintained architecture source for rpt_advanced. Read it
before designing a feature or structural change. Update it, the applicable ADR,
and the affected configuration and test documentation in the same change.

## System boundary

`app_rpt_advanced.so` is an Asterisk application module. It owns controller
configuration, node workers, radio exchange, local radio policy, telemetry,
and AllStarLink peer control. USBRadioPlus remains the radio channel driver and
owns hardware access. The separately released `rate_adjusting_pcm_ring` shared
library provides lock-free playout buffering and clock-rate recovery.

Stable reusable functions are progressively extracted as narrow, independently
versioned shared libraries. The approved component boundaries and the rule that
policy remains in the controller are defined by [ADR 0011](decisions/0011-versioned-functional-components.md).
Those planned libraries are not current runtime dependencies until an
extraction is complete.

The approved implementation migration is Rust-owned. C is retained only as a
minimal Asterisk loader and metadata bridge where the Asterisk module ABI
requires it; [ADR 0013](decisions/0013-rust-owned-implementation-and-asterisk-abi-shims.md)
defines that boundary. This is a planned migration, not a claim that current C
sources have already been replaced.

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
[ADR 0016](decisions/0016-code-derived-openapi-contract.md).

External and separately released project components are dynamic dependencies;
Rust implementation crates may remain internal to their owning artifact. See
[ADR 0018](decisions/0018-dynamic-linking-for-external-components.md).

## Runtime structure

```text
Asterisk frames / IAX peers
        |                         configuration reload
        v                                 |
link peer + media ----> node controller <-+
        |                         |
        |                         +--> identifier, announcement, courtesy,
        |                              speech, file, and Morse preparation
        v
radio worker <---- RadioPlusAdvanced exchange ----> USBRadioPlus / hardware
```

- `module/` is the Asterisk boundary: module lifecycle, channels, frames,
  codec selection, radio reservation, and worker ownership.
- `src/controller.*`, `src/duplex.*`, and `src/playback.*` implement node
  transmit ownership, half/full duplex behavior, hang time, and telemetry
  sequencing.
- `src/identifier.*`, `src/time_announcement.*`, `src/morse.*`,
  `src/speech.*`, and `src/tone_sequence.*` prepare and render telemetry.
- `src/scheduled_event.*`, `src/scheduled_action.*`, `src/scheduled_window.*`,
  and `src/message_template.*` parse civil-time triggers and replacement
  windows, constrain macro operations, and render scheduled messages. The
  module control plane runs them; they never execute in a radio worker.
- `module/runtime.*` materializes configuration-owned permanent-link intent and
  local-time replacement windows. It snapshots lock-free local/link receive
  activity and returns ordinary attach or detach work to the serialized control
  plane; it never changes a peer from a radio worker.
- `src/link_*.*`, `module/connection.*`, and `module/dtmf.*` implement
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
- `src/config_reader.*`, `src/config.*`, `src/schema.*`, and
  `src/settings.*` parse, validate, and inherit configuration.

## Core invariants

- Audio operations are lock-free. Control-plane work, configuration parsing,
  media preparation, and process execution never occur in an audio callback.
- Controller processing is signed-linear at its selected operating rate.
  Asterisk codec translation exists only at the application boundary.
- The shared rate-adjusting PCM ring is the only elastic clock-recovery buffer
  used for playout; its statistics remain observable for diagnosis.
- A valid configuration reload replaces workers without restarting Asterisk;
  invalid configuration leaves the running configuration intact.
- Unknown configuration sections and parameters are warnings and are ignored.
  Unknown, malformed, or unsupported values are warnings resolved through
  inheritance and a sensible default; only an irrecoverable inability to build
  a safe deterministic configuration rejects reload. See
  [ADR 0019](decisions/0019-tolerant-configuration-resolution.md).
- USBRadioPlus remains compatible with `app_rpt` through its legacy adapter;
  the rpt_advanced adapter is rate-aware.
- Telemetry is serialized. Speech is preferred and falls back to Morse when
  speech cannot be prepared.
- Scheduled work is wall-clock control-plane work. A due event queues its
  optional message before its validated controller operation. It settles the
  dispatch before releasing the runtime lock and preserves same-time
  configuration order. The ticker submits one FIFO control task each second,
  while runtime calendar-minute de-duplicates event occurrences. This lets a
  configured post-window quiet-time deadline expire promptly without entering a
  radio or audio callback. The detailed civil-time, reload, and no-catch-up
  policy is defined by [ADR 0017](decisions/0017-scheduler-route-lifecycle-and-civil-time.md).
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

## Decisions

See [the ADR index](decisions/README.md).
