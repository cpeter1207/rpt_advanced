# ADR 0026: Generational real-time runtime lifecycle

Status: Accepted

Amended 2026-09-13 for independently paced local receive and transmit workers
under ADR 0027. The current USBRadioPlus migration implements the split callback
entry points; this record's two-owner generational lifecycle remains pending.

## Context

A radio node reloads configuration while radio audio, RF signaling, link media,
announcements, and asynchronous transport callbacks remain active. Each part
needs a clear owner, and a reload must not free state that a callback, encoder,
or native audio block still uses. The native radio path cannot wait for a
mutex, allocate, take a reference count, submit work, or depend on a control
operation completing.

This record refines the worker-replacement requirement in
[ADR 0001](0001-reload-without-asterisk-restart.md). It assigns runtime
lifetime and reclamation without changing the lock-free audio rule in
[ADR 0002](0002-lock-free-audio-paths.md), the PCM-ring ownership in
[ADR 0003](0003-shared-rate-adjusting-pcm-ring.md) and
[ADR 0025](0025-native-media-routing-and-pcm-ring-ownership.md), or the
inward dependency rule in [ADR 0012](0012-layered-runtime-boundaries.md).

## Decision

### Station lifetime and terminology

Each configured node has one long-lived `NodeHost`, called the **station
host** in operational documentation. The station host remains allocated across
ordinary configuration reloads. It owns the active-generation pointer,
pre-registered hazard slots, radio-port audio-engine lifecycle, physical PTT safe
state, and the radio-device lease.

A `RuntimeGeneration`, called a **station operating generation**, contains all
configuration-derived state for one coherent operating instance: immutable
configuration and routing snapshots, prepared processing graphs, fixed media
rings and queues, prepared adapter contexts, and owner-private mutable DSP or
codec state. Owner-private state may change only through its named owner; it
is never shared mutable state.

The following terms are used consistently in architecture, status, and code
documentation:

| Component | Purpose and exclusive responsibility |
| --- | --- |
| **Radio-port audio engine** | The composition of separate local receive and transmit workers, not one combined native-tick owner. |
| **Local receive worker** | Input-driven sole owner of DSP squelch, CTCSS/DCS decode, deemphasis, receive processing/qualification, and the local receive inbound PCM-ring producer. |
| **Radio-port transmit worker** | DAC/adapter-output-clocked sole owner of inbound-ring consumers, native mixer/transmit DSP, oscillator phase, physical PTT, and direct adapter-buffer rendering. It writes one pre-access-tone program-audio loopback block per output count. |
| **RF-signaling edge publishers** | One bounded publisher per audio worker, each owning its state fields and SPSC event producer. Receive publishes decoder/qualification state; transmit publishes PTT and transmit state. Neither executes controller policy. |
| **Link media ingress** | The serial owner for one received network link's jitter buffer, decoder, and producer end of that link's inbound PCM ring. |
| **Station-telemetry audio producer** | The sole telemetry-program-ring producer. It prepares speech, files, and other non-native station telemetry outside the radio-port audio engine. Native Morse and tone generation remains in the engine. |
| **Link audio distributor** | The sole consumer of the program-audio loopback. It fans native blocks into bounded per-link egress queues. |
| **Link media egress** | The serial owner for one link's encoder, packet order, and network send sequence. |
| **Station-control event dispatcher** | The serialized owner for configuration, schedules, topology, link lifecycle, telemetry policy, status, generation construction, retirement, and reclamation. Its scheduler and configurator are logical responsibilities of this owner, not separate real-time threads. |

A receive or transmit worker may execute directly in its adapter callback;
the split does not require an additional operating-system thread. Mutable DSP
state is never shared between the two workers. A per-link ingress or egress
owner is a serial logical executor, not a requirement for one operating-system
thread per peer. A fixed worker pool may
run those executors, but it must never run two jobs for the same link
concurrently.

These logical owners and their lifetimes belong to rpt_advanced, not to
Asterisk's thread model. The station-control owner submits through ADR 0038's
control-path adapter, currently backed by the Asterisk taskprocessor; backend
thread and queue details stay inside that adapter. Standalone can select an
independent backend with the same functional contract. Scheduling and
reclamation policy remain in the controller. Other Asterisk-owned callbacks
or API-mandated thread handling remain in their thin integration adapter.
ADRs 0036/0038 define those dependency boundaries without requiring a new
worker pool or changing the ownership rules described here.

The radio-port audio engine is not a general event dispatcher. The RF-signaling
edge publisher may publish near-real-time state, but it must not run macros, format
telemetry, evaluate schedules, log, perform filesystem or network I/O, or
execute controller policy. The station-control event dispatcher consumes those events
and owns the resulting actions.

Each worker latches one immutable generation for its setup-bounded call and
advances its owned timing on its own sample clock. Receive and transmit counts
and call times need not match. RF edges and periodic snapshots publish at the
responsible worker's boundaries under ADR 0027. Neither worker waits for the
other, for partial device I/O, or for a configuration handoff.

In ADR 0027's verified shared-clock mode, the adapter calls receive then
transmit back-to-back with one coherent generation protected across the pair.
The same private owners and hazard protections apply; publication between
those two calls must not switch the second call to a different generation.
The local ring is unity-rate pass-through with a target reserve equal only to
configured squelch delay. Changing between paired and independent callback
modes is a controlled adapter handoff: quiesce both owners before changing
ring mode or ownership.

### Media and event queue ownership

All real-time media queues are fixed-capacity, single-producer/single-consumer
queues with one explicit owner at each end:

| Queue | Producer | Consumer |
| --- | --- | --- |
| Local receive inbound PCM ring | Local receive worker | Radio-port transmit worker |
| Per-link inbound / receive-program ring | Link media ingress | Radio-port transmit worker |
| Telemetry playout / telemetry-program ring | Station-telemetry audio producer | Radio-port transmit worker |
| Program-audio loopback ring | Radio-port transmit worker | Link audio distributor |
| Per-link egress media queue | Link audio distributor | Link media egress |
| Receive RF-signaling event ring | Receive edge publisher | Station-control event dispatcher |
| Transmit RF-signaling event ring | Transmit edge publisher | Station-control event dispatcher |

The station-control event dispatcher publishes a prepared, immutable RF-action
snapshot atomically for the radio-port audio engine. It does not directly key
hardware. The transmit worker owns the physical PTT transition and publishes
the resulting PTT state through its edge publisher. Receive qualification is a
generation-tagged lock-free handoff to transmit, not shared detector state or
a second PTT writer. The two event publishers must not share an SPSC writer.
Receive qualification must retain its association with processed sample
positions through buffering and rate correction. A latest decoder snapshot
alone must not qualify older queued audio or truncate a valid buffered tail.
Use bounded generation-tagged timing metadata; do not add another PCM writer
or require the workers to rendezvous.

Audio payload does not use a multiple-producer or multiple-consumer queue. A
bounded non-real-time control queue may have multiple producers, but a full
queue, slow link, or slow external operation must never delay the radio-port audio
engine. Each bounded queue has an explicit, observable stale-media drop policy
at its non-real-time boundary.

An asynchronous transport may call a received-packet callback concurrently.
In the Asterisk-hosted adapter only, the narrow ingress mutex permitted by
ADR 0002 serializes only
the link-media-ingress entry before it touches that link's jitter buffer,
decoder, or PCM-ring producer. It is never acquired by the radio-port audio engine,
RF-signaling edge publisher, mixer, station-telemetry audio producer, or hardware path.
If an adapter can invoke an unregistered arbitrary thread, it must first hand
off a fixed packet descriptor to a registered link-media-ingress executor;
that arbitrary thread must not retain a generation reference.

The planned standalone transport uses ADR 0037's bounded SPSC fan-in instead
of that ingress mutex. Each registered producer has its own queue to the
assigned media owner; that owner alone writes a peer's receive-program ring.
Queue topology follows bounded worker counts rather than creating a thread
per peer. The same generation tagging and quiescence rules apply.

### Atomic generation publication and reclamation

The station host holds an acquire/release atomic pointer to the active station
operating generation and a monotonically increasing generation ID. Every
long-lived executor has one pre-registered hazard slot. Local receive and
transmit have separate slots even if one adapter sometimes calls them on the
same thread. Each worker and other generation readers protect a generation
as follows:

1. Load the active pointer with acquire semantics.
2. Store the pointer in the executor's fixed hazard slot.
3. Reread the active pointer. If it changed, clear or replace the slot and
   retry before dereferencing generation state.
4. Use that one generation for the entire native PCM block or tagged work item.
5. Clear the slot on completion.

No radio-port-audio-engine operation allocates, dynamically registers a slot,
increments a general reference count, waits for another owner, or invokes a
reclaimer. Pointer storage is not reused until the generation is safe to free,
so the hazard protocol cannot observe an ABA reuse of the same address.

Queued non-real-time work carries its generation ID and has a generation-owned
outstanding-work count. A worker checks that its generation remains admitted
before starting external work, then clears its count when it completes or is
cancelled. Generation tags reject stale ingress, egress, telemetry, and
control work after a reload without requiring a radio-port-audio-engine lock.

The station-control event dispatcher keeps retired generations in a control-owned
retirement list. It may reclaim a retired generation only after all of the
following are true:

- no registered hazard slot protects it;
- no tagged work item remains outstanding;
- new ingress and egress work for it is gated off;
- its ingress callbacks/producers are detached or redirected and quiescent
  (under the ingress mutex only in the Asterisk-hosted compatibility adapter);
- its queues are drained, safely abandoned, or unreachable; and
- its adapter callbacks and device contexts have stopped using it.

This is a quiescent-state, hazard-slot grace period. The station-control event
dispatcher may block while it waits or uses a condition variable to be
notified; the radio-port audio engine never waits for quiescence.

### Normal configuration reload

An ordinary reload preserves the station host and radio-device lease:

1. The station-control event dispatcher parses, inherits, validates, and preallocates
   a complete candidate generation off the radio-port audio path. It prepares all
   processing state, fixed queues, rings, and non-transmitting adapter context.
2. If any candidate step fails, the candidate is rejected and the active
   generation remains unmodified.
3. After a complete candidate is ready, the station-control event dispatcher atomically
   publishes it and marks the replaced generation retiring. Future callback
   entries select the published generation; already protected old work may
   finish or be cancelled under its generation tag.
4. Each audio worker latches the new generation at its next call boundary.
   Its DSP, rings, and qualification snapshots all belong to that generation.
   Never write old receive output into new rings or consume old-generation
   rings/qualification in a new transmit block. If transmit adopts first, it
   uses the new ring's bounded safe shortfall behavior until new receive audio
   arrives; neither worker waits for the other's adoption.
5. The station-control event dispatcher gates new old-generation ingress and egress,
   cancels generation-tagged telemetry or scheduled work where established
   policy requires it, and waits for the old generation's grace period.
6. The reclaimer releases the old generation only after the reclamation rules
   above are satisfied.

A reload is **applied** when both local receive and transmit have adopted the
candidate.
It is **complete** only after the replaced generation has quiesced and been
reclaimed. A failed validation is never applied.

If one worker stalls before adoption, report `adoption pending` and its owner
and age at the bounded control-plane timeout. Keep the candidate published and
all still-protected state safe; do not report the reload applied or wait from
an audio callback. The same one-retiring-generation limit applies during
partial adoption as during retirement. Any device recovery follows the normal
RF-safe handoff, not force-freeing state to finish a reload.

If an applied generation does not quiesce before the station-control timeout,
the station remains on the new active generation, retains the old generation
safely, reports `retirement pending` with its age and owner counters, and does
not force-free it. To bound retained state, a replacement reload that would
add another retiring generation is rejected until the pending retirement
quiesces. Operators can diagnose the held owner through status and logs.

Module unload follows the same protocol after the station host publishes an
RF-safe inactive state. It waits for all generation and adapter callbacks to
quiesce before releasing the station host.

### Controlled radio-device or adapter handoff

A change to radio-device identity, native rate, or hardware adapter is not an
ordinary generation swap because two contexts must not own the same radio
device. The station-control event dispatcher performs a controlled handoff:

1. Validate the replacement configuration and adapter compatibility without
   claiming the live device.
2. Request an RF-safe idle transition. At the next transmit worker
   block boundary,
   it stops transmit program output, deasserts PTT, and publishes the resulting
   radio state.
3. Stop and unregister both old input and output callbacks, then wait for both
   worker hazard slots and all related adapter callbacks to quiesce before
   closing the old device or unloading its adapter.
4. Open the prepared replacement device, establish its radio-port audio engine, and
   publish the ready candidate generation.

An adapter is never dynamically unloaded while a callback or generation can
reach it, consistent with ADRs
[0021](0021-versioned-rust-c-adapter-boundaries.md) and
[0022](0022-versioned-external-c-dependency-adapters.md). If preparation fails
before the old device is released, the old station remains active. If opening
the replacement fails after release, the dispatcher attempts to restore the
retained old device configuration; if that is not possible, the station stays
RF-safe with PTT deasserted and exposes the failed handoff through status and
logs.

### Lifecycle observability

Status must expose active and retiring generation IDs, lifecycle state,
retirement age, per-worker adopted generation, protected-owner count,
outstanding tagged work, queue occupancy, media drops, and quiescence-timeout
count. These fields are
available to the CLI, REST API, and status stream through the shared control
catalog.

## Consequences

The implementation has one owner for each mutable radio, link, queue, and
generation object. Radio audio remains bounded and lock-free while ordinary
reloads exchange configuration at a PCM-block boundary without restarting
Asterisk.

Tests must deterministically cover hazard acquisition during publication,
callback entry racing a reload, queued stale work, normal retirement, delayed
quiescence, hardware-handoff failure and restoration, and module unload. They
must include receive-first and transmit-first adoption, stalled capture or
playback, concurrent RF publication without a shared producer, and preservation
of generation ownership and sample-associated qualification across the local
receive ring, including drift and buffered tails. A stale generation or
missing receive input cannot keep PTT asserted indefinitely. They
must prove that no radio-port-audio-engine path can take a compatibility ingress mutex, wait for
the station-control event dispatcher, access a reclaimed generation, or write directly
to a link egress queue.
