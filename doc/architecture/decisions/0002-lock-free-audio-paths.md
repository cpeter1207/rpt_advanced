# ADR 0002: Lock-free audio paths

Status: Accepted

## Context

Real-time radio-port audio must not block on mutex contention, configuration
work, disk I/O, process execution, or network control operations. Receiver
signaling transitions such as COR, CTCSS, DCS, PTT, and GPIO input also need
prompt handling without making either radio audio worker a general-purpose
event dispatcher.

## Decision

All radio-port audio exchange uses lock-free ownership and atomic state. Work
that can block or allocate unpredictably is prepared outside the radio-port
audio path and published as immutable prepared state.

The **radio-port audio engine** is split into two owners under the accepted,
not-yet-implemented 2026-09-13 amendment in ADR 0027. The input-driven **local
receive worker** owns squelch, CTCSS/DCS decode, deemphasis, and receive DSP.
The DAC/adapter-output-clocked **transmit worker** owns native mixing, transmit
DSP, oscillator phase, and actual PTT state, and fills the adapter's output
buffer directly. Neither is a general event dispatcher or waits for the other.
Each may perform an immediate prepared radio action for its own state. Each
has an **RF-signaling edge publisher** that updates atomic snapshots and posts
fixed-size notifications to its own bounded SPSC event queue. The
**station-control event dispatcher**, owned by the station-control thread,
consumes those notifications and performs telemetry, logging, topology,
scheduling, macro, and link-control work outside the radio audio workers.

Every PCM ring has one named producer and one named consumer. The local receive
worker writes processed audio to the local receive inbound ring; the transmit
worker alone consumes it. A linked-peer
receive worker is the sole producer for that peer's receive-program ring; the
transmit worker is its consumer. The station-telemetry audio worker is the
sole producer for the telemetry playout ring; the transmit worker is its
consumer. The transmit worker is the sole producer for the program-audio
loopback ring; the link-audio dispatcher is its consumer. The dispatcher owns
fan-out into each linked peer's transmit-program queue, and that peer's
transmit worker is its sole consumer. A logical linked-peer worker may run on a
shared worker pool, but jobs for one peer are serialized so its jitter buffer,
decoder, encoder, and packet order have one owner at a time.

In the current Asterisk-hosted adapter only, an asynchronous linked-peer
receive listener may use a narrowly scoped mutex
only to serialize concurrent network callbacks before they feed that peer's
single-producer receive-program PCM ring. The mutex is never acquired by the
radio audio workers, PCM-ring consumer, mixer, station-telemetry producer,
or hardware path. It does not protect shared radio audio state and does not
turn the ring exchange into a blocking audio-path operation.

The planned standalone controller does not use that exception. ADR 0037
assigns one media owner per peer and bounded producer-to-owner SPSC packet
queues, allowing concurrent ingress without locking or multiple PCM writers.
Use lock-free thread-safe alternatives wherever available in standalone
operation; do not disguise a lock as an atomic spin gate.

The workers' bounded frame contracts, adapter-owned PCM assembly, and
elapsed-sample timing rules are defined by ADR 0027. Input availability drives
receive; output demand drives transmit, with independently bounded counts.
They must not reintroduce a fixed 20 ms core assumption or a blocking output admission
gate. PCM payloads use the canonical `f32` representation defined by ADR 0029.

## Consequences

Station-control and radio-program boundaries must be explicit. New audio
features need bounded, allocation-free callback behavior and tests that
exercise their concurrent ownership rules. The RF-signaling edge publisher may
coalesce notifications under load, but its atomic snapshots remain
authoritative for the station-control event dispatcher. Network ingress
serialization needs tests for concurrent receive callbacks and proof that
neither radio audio worker ever waits on the compatibility ingress mutex.
Standalone tests additionally prove bounded lock-free fan-in and progress
with a stalled producer under ADR 0037.
