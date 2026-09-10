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
- `src/scheduled_event.*`, `src/scheduled_action.*`, and
  `src/message_template.*` parse civil-time triggers, constrain macro
  operations, and render scheduled messages. The module control plane runs
  them; they never execute in a radio worker.
- `src/link_*.*`, `module/connection.*`, and `module/dtmf.*` implement
  AllStarLink admission, peer media, topology, and permitted DTMF control.
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
- USBRadioPlus remains compatible with `app_rpt` through its legacy adapter;
  the rpt_advanced adapter is rate-aware.
- Telemetry is serialized. Speech is preferred and falls back to Morse when
  speech cannot be prepared.
- Scheduled work is wall-clock control-plane work. A due event queues its
  optional message before its validated controller operation. It settles the
  dispatch before releasing the runtime lock and preserves same-time
  configuration order. One FIFO control task is retained for each wall-clock
  minute the ticker observes, so long control work cannot erase later due
  minutes. The ticker only submits control work; it does not enter a radio or
  audio callback.

## Change procedure

1. Read this map and the relevant ADRs before choosing a design.
2. Preserve the core invariants or add an ADR explaining the approved change.
3. Update this map when ownership, boundaries, data flow, or invariants change.
4. Add or supersede an ADR for a material architectural decision.
5. Update tests, configuration documentation, examples, and manuals with the
   same interface change.

## Decisions

See [the ADR index](decisions/README.md).
