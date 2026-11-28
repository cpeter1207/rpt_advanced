# Controller status stream and REST snapshots

**Status** — Design for implementation planning, 2026-11-25.

The management application needs event-driven station indicators and responsive audio meters. The CLI and scripts need the same observations in a single GET without opening a stream or waiting for another audio sample. Both surfaces use the controller’s published, redacted status objects. They support site and individual radio-port scopes, including a combined view. Configuration, commands, and alarm acknowledgement continue on their existing interfaces. This design adds no application commands to the stream.

The existing control, audio, and management owners stay in place. Reuse the HAL status resources and code-derived OpenAPI under ADR 0016, the authenticated loopback listener under ADR 0045, and the shared loudness and limiters under ADR 0043. Existing JSON/REST status fields such as `transmit_key_requested` keep their meaning. New fields distinguish physical feedback, quality, and measurement windows where the current fields are coarser. [Control API ADR](../../../doc/architecture/decisions/0016-code-derived-openapi-contract.md) · [HAL and loopback ADR](../../../doc/architecture/decisions/0045-modular-json-and-hal-management.md) · [Shared loudness ADR](../../../doc/architecture/decisions/0043-shared-loudness-and-receive-vad.md) · [JSON/REST design](2026-11-18-json-rest-configuration-design.md)

The current HAL observer uses a roughly 250 ms refresh cycle. Product publishes station input/output peak and RMS, but its HAL representation does not yet expose moments at every DSP stage or an event stream. Keep the existing slow status sampler for suitable fields. Feed the shared observation view from the audio owner’s meter publications and from state edges as they occur, independent of that slow poll. Widen provider observations where required. [HAL live types](../../../rust/product/src/management/live.rs) · [Observer](../../../rust/product/src/lifecycle_management.rs) · [Current station meters](../../../rust/product/src/worker.rs)

## What to observe

| Scope | Status and transitions |
| --- | --- |
| Site | Service availability, uptime and process boot ID, active and desired revisions, activation and recovery state, TX inhibition summary, registered ports, shared equipment, sensor and alarm states, CPU, memory, network bandwidth in/out, and management observation health. Group resources by owning site or port. |
| Local receive | COS/COR observation from the selected provider, effective carrier after polarity and debounce, CTCSS and DCS decoder status and detected tone/code when known, local VAD enabled/mode/speech/hold state, qualified receive, RX mute, source availability, and the gate that admitted or suppressed programme. Capture qualified-unkey duration and the suppression reason when a normal controller decision suppresses hang or courtesy. |
| Network link | Link type, public peer identity, direction, mode, permanent/group selection and retry state, connected/disconnected event, ingress and egress key/unkey, mute and qualified activity, and media diagnostics such as jitter, drops, PLC, buffered audio, and underruns. Distinguish a retained retry from a connected transport. Silence detection and link signaling can disagree and remain separate observations. |
| Local transmit | Controller PTT request and actual provider assertion when sampled, TX inhibit and timeout, effective transmit programme, CTCSS/DCS encode activation and tone/code when known, and digital output levels. For an always-on speaker, the physical PTT field is unavailable, with a reason indicating the port has no actuator. |
| Telemetry | Courtesy tone, ID, tail message and other telemetry queued/playing/finished/cancelled/failed. Publish opaque playback instance, known public message ID or typed category, permitted audience, effective polite wait or deadline, and the suppression/interruption reason where the scheduler knows it. Distinguish a queued item and its actual PCM playback. |
| Phone patch | SIP/IAX provider and registration state, each opaque call ID and generation, direction and call phase, private/authenticated/attached state, caller ingress mute and keying, progress, and call-media diagnostics. An unauthenticated call has its own private lane. Expose the same visibility already authorized on REST. |
| Site or port equipment | Sensor value, unit, quality, sampling time, configured ID and owner. An alarm exposes its condition, active/latch state and last transition where the equipment supports them. Include output/actuator observation and source faults where available. |
| Other diagnostics | RX/TX/provider readiness, software and device underruns, clipping counters, queue depths, event-record loss, pending scheduler window/macro/next boundary when available, clock quality, and last ordinary state transition. Use reason codes with bounded metadata and existing typed fault details. |

Represent each live observation with a `quality` of `valid`, `warming`, `stale`, `unavailable`, or `fault` and an actual `observed_at` timestamp when known. Preserve existing sensor quality values when embedding current HAL objects, including `null` values for missing data. A known off, muted, empty, or disconnected state is authoritative only within the validity interval of its source. Electrical COS, PTT, and encoder output are `null` when the provider supplies no feedback. A PTT command is a software observation and has its own field even when the physical state is unknown. Recently keyed signals, frames and meters have freshness limits based on their producer cadence, with a deadline in the item or profile. An expired source is represented as stale rather than emitting a fabricated off edge. Explicitly idle, healthy audio owners advance their meter clocks with silence according to their normal idle behavior.

Sample host and controller resources off the realtime threads at 1 Hz. Expose host aggregate CPU utilization in percent of online CPU capacity. Process CPU in percent of one logical CPU can exceed 100. Report the averaging window, online CPU count, host available and total memory bytes, controller resident bytes, and a separate effective service memory limit/current usage when cgroup data is available. Leave provider/process accounting unavailable where the host cannot measure it reliably. Keep CPU, memory, and bandwidth site-scoped.

Measure network bandwidth in each direction for each host-visible non-loopback interface. Report `rx_bps` and `tx_bps` as bits per second with `window_ms`, observation time, and quality. Compute rates from the byte-counter deltas and actual monotonic elapsed time between samples. A fresh or replaced interface warms before it has two samples. A reset starts a new baseline. Retire removed interfaces and report unavailable when counters cannot be read. The site `resources` topic carries these measurements in both snapshots and periodic state updates.

## Audio paths and measurements

Each active graph publishes descriptors for actual PCM tap points. A chain has a stable ID within an entity generation, a direction, ordered stages and connections between their outputs and inputs, sample rate, channel labels and channel mapping. A stage identifies its purpose, branch and position, and its state as active, bypassed, disabled, or unavailable. Descriptors name the effective graph rather than the configured list of optional filters. Use the same stable IDs across REST and WebSocket. A rebuilt chain publishes its new graph generation before samples from it appear. A reused stage ID in a new generation starts a new measurement epoch.

| Chain | Required coverage where the path exists |
| --- | --- |
| Local RX | Capture/device PCM, each channel mapping or conversion, receive DSP filters and deemphasis, each EQ/AGC/dynamics/limiter stage, qualified/muted RX output, and the actual station programme input. Include separate detector input where present. Include stages before local admission so an operator can see signal while the gate is closed. |
| Each network or authenticated phone leg | Decoded PCM input, jitter/PLC and conversion boundaries where PCM exists, link-EQ/AGC/compression/limiters, ingress mute/media gate, station mix input, and egress mix-minus, egress DSP stages and codec input. Codec packets and predecode jitter carry diagnostic counters. Label a provider or codec-internal PCM tap unavailable until the provider exposes it. Unauthenticated calls publish phase only, with no private-path meters. |
| Local TX voice and telemetry | Voice source mix, each voice DSP stage, presentation pacing, individual public telemetry sources and telemetry bus, combined mix, TX EQ/preemphasis/dynamics and limiting where present, signaling injection and digital device output per actual channel. Describe separate CTCSS/DCS/signaling channels and taps where a provider publishes PCM. Include the input and output of both the voice and telemetry branches before the final output. The `local_tx:link_mix_input:mono` tap measures the actual sum of admitted peer audio before it joins voice processing. |

One meter describes one actual PCM signal, including each meaningful channel. Register the input/output of each active processing stage, with signal continuations, channel and multiband splits, and recombination represented explicitly. Adjacent stages can share one tap where the PCM is identical. A chain may also expose an explicitly labelled aggregate with defined channel membership. An identity or bypassed stage can reference the already measured signal. An opaque provider boundary exposes the measured input/output and identifies internal stages as unavailable until their owner supplies taps. A missing tap in a supported graph is visible as a coverage gap. The detailed implementation plan must inventory those gaps and specify the owner or adapter changes needed to expose all stages. Digital measurements describe PCM headroom, level and clipping. Physical RF deviation requires separate hardware feedback.

| Field | Definition |
| --- | --- |
| `peak_dbfs` | Maximum absolute PCM sample in the trailing 100 ms. Full-scale magnitude 1 is 0 dBFS. Upstream floating-point samples can exceed it. |
| `rms_dbfs` | RMS over the same 100 ms, unweighted, with full-scale constant-magnitude PCM at 0 dBFS. |
| `lufs` | Momentary loudness over the trailing 400 ms, ungated, using BS.1770 K-weighting and signal/channel weighting with known channel layout. A mono tap uses the mono measurement. A graph aggregate uses its declared layout. This public 400 ms window stays fixed when the AGC’s configured control window changes. |
| `peak_hold_dbfs` | Maximum sample peak within the trailing 2,000 ms. Hold advances with the observed audio clock independently of display refresh or client rate. Reset on source/measurement-epoch change. |
| Additional observations | A distinct `true_peak_dbtp` and `integrated_lufs` where already measured by the audio owner, clipped samples/counts and valid sample counts. Maintain the published true-peak and integrated-loudness meter epochs. |

The negotiated meter profile publishes these units, windows and sample type. The active meter default is **20 updates per second** (50 ms), continuing ADR 0016. Clients may request `meter_hz=1`, `2`, `5`, `10`, or `20` on the stream. The measurement windows remain fixed, and requested lower rates affect delivery only. EBU Tech 3341 defines 400 ms momentary and 3 s short-term loudness and sets at least 10 Hz for the live short-term meter. The 2-second peak hold is a controller display choice. Yamaha documents a factory 2-second hold on one meter and an adjustable duration on another. The controller uses the ITU true-peak field where that measurement exists. [EBU Tech 3341, §2.2](https://tech.ebu.ch/docs/tech/tech3341.pdf) · [ITU-R BS.1770-5](https://www.itu.int/rec/R-REC-BS.1770-5-202311-I) · [Yamaha input-level meter settings](https://data.yamaha.com/files/download/other_assets/7/317397/AD808E.pdf) · [Yamaha POCKETRAK C24 manual](https://data.yamaha.com/files/download/other_assets/1/314301/pocketrakC24_en_rm_b0.pdf)

JSON `null` represents exact digital silence for logarithmic readings and carries `signal_state: "silence"` when the complete windows were valid and current PCM was silent. `quality` distinguishes warming, unavailable, fault and stale measurements. A valid meter can report exact silence while a recent held peak remains finite. Each window retains its own value when it still contains signal. Emit finite JSON numbers only. A restarted or newly enabled path warms until the 400 ms window fills. Peak and RMS become available after their first 100 ms window, earlier than LUFS. Before 2,000 ms have elapsed, the hold covers samples since measurement began. A continuity-affecting filter or route change starts a fresh epoch and identifies the new active revision, so a gain adjustment cannot present an old window as a new result.

## Read-only resources

| GET | Representation |
| --- | --- |
| `/api/v1/status/snapshot/` | Canonical HAL snapshot for a selected site and/or ports. It includes the meter descriptors and their most recent observations when selected. |
| `/api/v1/status/stream/` | WebSocket upgrade for the same selection. Discover via `rpt:status-stream` on the HAL status root and snapshot. Ordinary requests receive `426 Upgrade Required` with a link to the snapshot. |
| Existing `/api/v1/status/site/` and `/api/v1/status/ports/{port-id}/` | Reuse the current site and port views, add links to the scoped snapshot/stream, and expose expanded resource fields through the existing status families. |

Use `site=true` and repeat `port=<resource-id>` to request scopes. No explicit scope selects the site summary. A site summary links to all visible ports. It includes shared site equipment without embedding all port/link meters. Use `topics=radio,links,phone_patch,telemetry,equipment,resources,diagnostics,meters` to choose a subset. Default to applicable state topics. Opt in to `meters` for detailed level data. `meter_hz` applies only to the stream. Validate unknown filters and malformed selectors with `400` before upgrade. Unknown visible resource IDs use `404` under the existing authorization policy. Use HAL links emitted by the status collection to obtain actual identifiers regardless of the node name or ASL number. Publish selector defaults, capability and schemas as HAL profiles and in generated OpenAPI.

A snapshot contains the normalized selection, `boot_id`, active revision, wall-clock `observed_at` in UTC, service `monotonic_ms`, `state_cursor` and HAL links. Its `status` has `site` and/or `ports` matching the selection. The `ports` map is keyed by resource ID. Port status embeds current entities and graph descriptors and uses the same state and measurement schemas as the stream. Per-source timestamps and quality retain their own observation times. The shared capture samples changing link transport counters once per second while publishing authoritative link and key transitions as they occur. A snapshot takes one published controller view and returns the last completed measurements at that cut, even when freshness has expired. It does not poll hardware or wait for a future transition. Use `application/hal+json` and `Cache-Control: no-store`. A functioning controller without a published view can return `503` with the existing problem representation. Retired entities leave a fresh snapshot.

Equipment entries retain their resource UUID as the map key. Each entry carries the public logical `key` from the active equipment configuration. A sensor also carries `name` when its configured text label resolves using the scope and current observations. A client can display the logical key when the label needs unavailable context. The identity and reading come from the same status cut.

One port can outgrow the existing 4 MiB response ceiling when many peers or calls have meters. Page that same captured view using bounded groups of status and measurement records and stable merge keys. All pages repeat `snapshot_id`, `state_cursor` and the selection, with a zero-based `page_index` and `complete` flag. Within `status` a page can contain a separate set of port, peer, call, graph or meter items. Merge pages by scoped resource and measurement identity. A port ID may appear in several pages with different keyed items. GET returns the first page and a HAL `next` link when more data is present. Its short-lived, opaque continuation is bound to the selected capture and authorized reader, and never advances to a later cut. The cache is bounded. An expired continuation returns `410` with a fresh-snapshot link. [Existing HTTP response budget](../../../rust/peer-support/src/https/http.rs)

## WebSocket contract

Upgrade through the existing authenticated loopback HTTPS listener. Negotiate `rptadv.status.v1` with RFC 6455. WebSocket carries UTF-8 JSON text messages. The server accepts only standard control frames from the client, including Ping, Pong and Close. Clients change scope by reconnecting with a different query. The proxy uses its authorized backend certificate and applies the same user access policy as snapshot GET. It checks browser Origin and preserves the canonical Origin to the backend. The backend checks Origin against the configured allowed origins on browser requests. Authorized native clients can omit Origin. Browser code never receives the CLI or proxy client key. Existing policy revocation and expiry apply to long-lived streams. Send a Ping after 15 seconds without traffic and close if it remains unanswered for 15 seconds. Publish upgrade/subprotocol, selection, authentication and close behavior alongside OpenAPI. [RFC 6455, §§4, 5 and 10](https://www.rfc-editor.org/rfc/rfc6455.html)

Use one ordered stream of these message types:

| `type` | Meaning |
| --- | --- |
| `snapshot` | First application message. `data` has the same HAL page shape as GET for this selection. The stream delivers the complete page sequence under one `snapshot_id` and cursor before the first event or meter message. The client replaces its view when `complete` is true. REST continuation links are optional in stream pages. |
| `event` | An ordered state transition recorded after the snapshot cursor. Includes `state_cursor`, capture time, owning port/site, the affected `subject`, a `kind`, and its complete current status for `upsert` or last public identity/reason for `remove`. State edges such as `radio.rx.unkeyed`, `link.connected`, `link.unkeyed`, `link.disconnected`, `phone_patch.call_attached`, `transmit.ptt_changed`, and `telemetry.started` have distinct kinds. If a short key edge occurs between meter updates, the edge still has its own event. Periodic slow status such as resource use updates the same representation. |
| `meters` | Coalesced newest meter records for the selected scopes at the negotiated cadence. Each record identifies its entity, chain, stage, signal and epoch. Clients replace measurements by identity and ignore retired epochs or signals. No meter update creates a resource. |
| `resync_required` | State delivery can no longer reconstruct the current selected view. The server sends a typed reason when possible and closes. The client opens a replacement stream for a fresh snapshot. A slow or disconnected client can also immediately retrieve the snapshot via GET. |

Every message includes `type`, the controller process `boot_id`, and contiguous `seq` for messages actually written on that connection. New protocol positions, monotonic times, generations and 64-bit counters use decimal strings. Embedded existing HAL fields keep their established numeric types and names, including `timestamp_ms`, registration and call `generation`, and existing meter counters. A global state cursor can advance over events that the selected scopes filter out. Clients use the per-stream `seq` for delivery consistency and opaque runtime identity plus owner/graph generation to fence stale observations. Time-sensitive values use captured monotonic time. UTC is for display and cross-system correlation. Stream reopens from a new snapshot across restart, gaps or reconnection. There is no persistent replay log in this protocol.

The snapshot and subscription registration share a publication boundary. Capture ordered events during snapshot transmission in the same bounded state lane. If that lane overflows before the last page, close for resync. A graph layout change or resource upsert reaches the client before meter records referring to its new graph version. Retirement fences queued measurements before publishing the corresponding removal. This example shows a keyed link release and reuses the existing `type`, `node`, `active`, and `muted` names for link identity and receive state.

```json
{
  "type": "event",
  "seq": "8",
  "boot_id": "4114bf08-85b7-4ed3-b879-6e96bba99624",
  "state_cursor": "271",
  "observed_at": "2026-11-25T18:15:01.023Z",
  "monotonic_ms": "206179",
  "scope": {"port_id": "1e80ee51-9bf4-4db2-baa6-758d5dd47a0f"},
  "subject": {"type": "link", "id": "76", "generation": "9"},
  "kind": "link.unkeyed",
  "operation": "upsert",
  "data": {
    "type": "allstarlink",
    "node": "506315",
    "state": "connected",
    "active": false,
    "muted": false,
    "transmit_keyed": false
  }
}
```

One `meters` item can be embedded in a snapshot or supplied in `meters.data`. Its graph descriptor declares windows and units.

```json
{
  "chain_id": "local_rx",
  "graph_generation": "5",
  "stage_id": "deemphasis",
  "signal": "mono",
  "signal_state": "present",
  "observed_at": "2026-11-25T18:15:01.000Z",
  "quality": "valid",
  "measurement_epoch": "12",
  "peak_dbfs": -10.6,
  "rms_dbfs": -19.2,
  "lufs": -18.7,
  "peak_hold_dbfs": -7.8
}
```

In a meter item `signal` identifies the channel. Use `signal_state: "silence"` to distinguish an exactly silent signal. For a port-wide `local_rx` chain, the owning port in the enclosing record is sufficient. For per-link or per-call chains include the exact entity ID and generation in the record. Forward-compatible clients ignore new members and undisplayed event kinds. An unsupported subprotocol must fail negotiation. Outstanding reads and subscriptions require the same visibility for private call details, message text/identifiers and diagnostics. Exclude audio bytes, DTMF digits, caller telephone numbers, private prompt content, credentials, auth proofs, and telemetry text that would expose protected input. Preserve approved public node/callsign and mailbox policy from existing authorized views.

## Media time and WebRTC playout

A meter can include `audio_position` with its `source_generation`, exclusive
`end_sample` and native `rate_hz`. The generation and sample position are
decimal strings. The sample position addresses the measured owner's actual
signal. It is absent when no source samples are available. Observation wall
time and service monotonic time remain separate from the measured sample
position. A 100 ms or 400 ms level describes the window ending at that
position. It does not describe when a status frame was sent.

A future WebRTC peer publishes its authorized media binding and sample-to-RTP
anchors over the connection's own control channel. The matching status graph
can expose that binding and its clock/graph epochs to an authorized viewer.
An app can correlate the codec-input meter with received RTP playout. Upstream
meter correlation depends on a known mapping through processing, resampling
and presentation delay. Late or expired readings remain stale on the app.
Sending audio and key signals never requires opening the status stream.
[Shared adapters and WebRTC design](2026-11-26-connection-adapters-and-webrtc-design.md)

## Realtime boundaries and qualification

Publish observed edges from the existing serialized owner or a bounded, non-blocking callback handoff. Reuse native meter observations and add shared PCM tap accumulators to the provider that owns each stage. Compute window aggregates with bounded state proportional to enabled graph taps and channel count. Keep formatting, socket writes, resource polling and large calculations off the audio callback. Prepare tap storage with the graph and compare measured CPU, memory and callback timing with metering disabled and enabled. Fast meter publication is shared across consumers. Additional subscribers do not add per-frame audio work. Use the same typed observation store for all projections, including the snapshot CLI path.

Send ordered state through a bounded queue per client, independently of a replaceable latest-value meter slot. Give state priority over meter data. Drop superseded meter updates with an observable count. State overflow, producer-side lost edges, or a prolonged blocked writer requires resync and close. Use `1013` for temporary service/slow-reader failure, `1008` for policy violation or unexpected application frames, and `1001` on normal service shutdown. Allow established subscribers to grow with available CPU, memory and bandwidth. Bound handshake and request work, each subscriber’s state and meter lanes, and reads after upgrade. Release the HTTP admission slot on upgrade so established streams leave capacity for regular requests. Return a retryable response when transient admission work or a requested selection exceeds its resource budget. Close the old owner safely after restart, credential removal or proxy loss. Receive/transmit and configuration work cannot be held waiting for a streaming client. [IANA WebSocket close codes](https://www.iana.org/assignments/websocket/websocket.xhtml#close-code-number)

The later implementation plan should qualify these outcomes:

- GET and stream snapshot for the same held publication have identical scoped status, timestamps, graph and units. CLI GET returns promptly on a quiet, inhibited or inactive port.
- Exercise real COS/tone and VAD transitions, PTT request versus physical feedback, courtesy/ID/tail transitions, ASL and EchoLink key/unkey and disconnect, separate private and attached calls, equipment alarms and fault/recovery. Test the distinction between a qualified short unkey and a provider outage.
- Compare stage readings before and after a known gain or bypass change using calibrated audible test tones and EBU loudness stimuli. Verify sample versus true peak, all channels, filter coverage, silence, clipping, stale/warmup, and expected hold expiry with a deterministic clock. Verify new epochs after effective adjustments.
- Verify snapshot-to-event handoff, captured snapshot paging for a single port above the HTTP response limit, multiple selected ports and site scopes, quick key edges, recovery and authorization expiry through nginx and Apache, slow-reader coalescing and resync, and resource limits with many calls, ports and viewers. Prove that sockets cannot delay radio audio or change its state. Document the OpenAPI routes and schemas from the implementation.
