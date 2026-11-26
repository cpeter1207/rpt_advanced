# Controller Status Stream and REST Snapshots Implementation Plan

> **For agentic workers:** Use `superpowers:subagent-driven-development` or `superpowers:executing-plans` to implement this plan task by task. Track progress with the checkboxes.

**Goal** — Publish current controller state and audio measurements through a read-only WebSocket and an equivalent HAL snapshot API. Add one-shot snapshot retrieval to `rpt-advanced-cli`.

**Architecture** — The existing controller owners publish public typed observations into a shared view. REST reads that view and WebSocket delivers a consistent initial capture followed by ordered state transitions and coalesced meter samples. The CLI follows HAL links over its existing authenticated HTTP client.

**Spec** — [Controller status stream and REST snapshots](../specs/2026-11-25-controller-status-stream-design.md). Follow the [JSON/REST configuration design](../specs/2026-11-18-json-rest-configuration-design.md) for management boundaries. Scope covers the controller, shared status providers it needs, and `rpt-advanced-cli`. The React app is outside scope.

## Required review before each checkbox

Before starting **each checklist item**, review `WISHLIST.md`, `doc/architecture/README.md`, applicable ADRs, both design documents linked above, and all applicable `AGENTS.md` files. Recheck any revisions or instructions that changed since the previous item. Start with ADRs [0002](../../../doc/architecture/decisions/0002-lock-free-audio-paths.md), [0016](../../../doc/architecture/decisions/0016-code-derived-openapi-contract.md), [0023](../../../doc/architecture/decisions/0023-unified-control-and-dtmf-policy.md), [0026](../../../doc/architecture/decisions/0026-generational-real-time-runtime-lifecycle.md), [0043](../../../doc/architecture/decisions/0043-shared-loudness-and-receive-vad.md), and [0045](../../../doc/architecture/decisions/0045-modular-json-and-hal-management.md). See `QUALITY.md` for the required gates. After verifying a checkbox, record the command and result before marking it complete.

## Contract and review focus

- `GET /api/v1/status/snapshot/` and `GET /api/v1/status/stream/` share `site=true`, repeated `port=<resource-id>` and `topics`. No scope means site. State topics are enabled by default. Meter publication is opt-in with `meters`; `meter_hz=1|2|5|10|20` is stream-only and defaults to 20 Hz. The subprotocol is `rptadv.status.v1`.
- Preserve existing HAL routes and field types, including the numeric call/registration generations and existing meter timestamps/counters. Encode new 64-bit positions, times and counters as decimal strings. Preserve public/private boundaries in all representations and log only sanitized diagnostics.
- Cover source loss, timeout, kerchunk, and brief key/unkey edges independently of the slow HAL poll. Measure actual provider PTT when available and preserve the separate software request. A change on one graph must never expose stale PCM or measurements as a new epoch on another.
- Cover missing/reset counters, replaced interfaces, monotonic gaps, and insufficient window history. Distinguish known silence and known off from unavailable observations. Test the effects of a live gain adjustment before and after a graph's new measurements warm.
- Cover one oversized port, concurrent topology changes during a paged snapshot, policy expiration during an active stream, and readers slow enough to exhaust their state queue. A healthy fast client and radio worker must continue while those cases are handled.

## 1. Shared read model and capture boundary

**Owners** — `rust/product/src/management/live.rs`, `host_status.rs`, `lifecycle_management.rs`, `management/schema.rs`, and the controller's existing generation and boot identity owners.

- [ ] Map the current source and projection for each required observation. Identify retained HAL fields, source quality/freshness support, and the stable identity and generation for ports, peers, calls, graphs, signals, and equipment. Identify opaque provider tap gaps with the adapter that owns each gap.
- [ ] Define typed public observation records and an immutable capture containing selection, `boot_id`, `active_revision`, `observed_at`, `monotonic_ms`, and `state_cursor`. Share the capture between REST and stream. Reuse public projections for redaction. Include validity and sample timestamps where available. Keep `snapshot_id`, `page_index` and `complete` on the page representation.
- [ ] Put snapshot acquisition and subscription registration at one serialized publication boundary. Retain independent typed state edges from the existing owner or a bounded callback handoff. Give graph and topology creation/removal ordering priority over dependent measurements. Mark edge loss explicitly.
- [ ] Test that an edge inserted during snapshot registration is delivered once after the captured cursor. Exercise a TX pulse and a peer connect/disconnect faster than the present HAL polling interval. Verify consistent boot and revision, and old measurement retirement through reload and disabled-port changes.

## 2. Site resource sampling

**Owners** — the observer in `rust/product/src/lifecycle_management.rs`, its site resource projection in `management/live.rs`, and a focused Linux resource sampler in `rust/product/src/management/`.

- [ ] Sample host/process CPU, memory, and network usage at 1 Hz outside the real-time or serialized runtime paths. Use Linux counters and the effective cgroup, when available, for process and service limits. Publish aggregate host CPU normalized to online capacity, process CPU relative to one logical CPU, the averaging window, CPU count, host available/total bytes, resident bytes, and effective service memory use/limit.
- [ ] Derive per-interface `rx_bps` and `tx_bps` for host-visible non-loopback interfaces from byte deltas and actual monotonic elapsed time. Use interface identity to reset baselines on replacement. Publish `window_ms`, time and quality with each reading. Represent unavailable counters and unmeasurable process accounting explicitly. Retire removed interfaces. Use the same site `resources` data for snapshot and state events.
- [ ] Test with a controlled monotonic clock and recorded counters. A 1500-byte RX delta and a 750-byte TX delta over 500 ms produce 24,000 and 12,000 bits per second. Cover initial warm-up, reset, disappearance/reappearance, unavailable counters, and a sampling interval different from 1 s. Confirm site-only resource ownership in both projections.

## 3. Controller status producers

**Owners** — `rust/product/src/host_status.rs`, `worker.rs`, `host_site.rs`, phone/call and peer owners, and `rust/core/src/controller/`, `runtime/`, and `link/`.

- [ ] Add RX and TX observations at their actual decision points. Include raw and qualified COS, CTCSS/DCS and detected values where available, VAD, RX mute, qualified-unkey duration and suppression reason, requested and observed PTT, protection/inhibit, and encoder activation. Preserve known versus unknown hardware feedback for radio and speaker profiles.
- [ ] Publish network link direction/type/retry and real connect/disconnect or key/unkey transitions, independent per-call private/authenticated/attached/mute status, and registration/progress. Publish shared and port-owned sensor and alarm changes with observed quality, threshold/latch state and output feedback where available. Publish scheduler activity and bounded media/provider diagnostics through their existing owners.
- [ ] Carry distinct telemetry queue, start, finish, cancel and failure events for courtesy, ID, tail and other public messages. Include a public playback identity or category, audience and polite deadline/suppression diagnostic. Filter call-private prompt and authorization details using the same visibility as current HAL status.
- [ ] Verify tests for a qualified short unkey, provider disconnect, physical PTT differing from software request, silent but confirmed audio, a telemetry wait with no PCM, rapid link transitions, reverse-call authentication/attach/mute, stale sensor data, alarm latch and owner recovery. Assert that credentials, private prompt content, caller numbers and collected digits cannot surface.

## 4. Audio graph taps and measurement windows

**Owners** — `rust/product/src/worker.rs`, the link and private-phone media owners, existing Core audio/controller outputs, and provider audio/ABI owners discovered in item 1. Use the released adapter contracts and ADRs 0018, 0021, 0025, 0035, and 0043.

- [ ] Register the actual local RX, peer/call ingress and egress, and TX voice/telemetry graph descriptors. Cover each real processing stage, branch and channel, including decoder and resampler boundaries, mix-minus, signaling injection and digital device output. Share taps where PCM is identical. Keep unavailable hardware/provider internals explicitly described. Update and qualify the corresponding shared provider interface for any tap the existing ABI cannot expose.
- [ ] Publish peak and unweighted RMS over 100 ms in dBFS, BS.1770 ungated K-weighted momentary loudness over 400 ms in LUFS, and 2,000 ms peak hold. Reuse existing provider taps, integrated LUFS/true peak and clipping counters where available. Preserve channel identity, valid sample counts, silence, warming, faults and graph-generation fences. Calculate shared, bounded observations independently of the subscriber count.
- [ ] Verify real tone/gain fixtures at multiple stages and channels, under bypass, clipping, silence, sample/true-peak distinction, new gain/route epoch, and stale sources. Confirm ready sample/RMS at 100 ms, ready momentary loudness at 400 ms, and hold clearing at 2,000 ms. Compare against BS.1770/EBU test stimuli. Measure memory and callback timing with and without observers and under multiple viewers.

## 5. HAL snapshot route, selection and pagination

**Owners** — `rust/core/src/management/routes.rs`, `rust/product/src/management.rs`, `management/representation.rs`, `management/schema.rs`, `management/openapi.rs`, `configuration_transaction/tests/http.rs`, and existing CLI/HAL fixture utilities.

- [ ] Register canonical routes, status discovery relations `rpt:status-snapshot` and `rpt:status-stream`, and the links from site, port and snapshot views. Parse selection once for both surfaces. Check unknown/malformed selectors with `400`, unavailable IDs with `404` under existing visibility rules, no view with `503`, and regular GET to the stream URL with `426` plus the snapshot link.
- [ ] Construct `application/hal+json` pages with `Cache-Control: no-store`. Keep each page within the existing 4 MiB limit. When needed, retain the capture under a bounded opaque reader/selection-bound cursor. Return HAL `next`, a deterministic merge key, repeated snapshot identity/state cursor and the page completion flag. Expiry returns `410` with a fresh-capture link.
- [ ] Test site-only, port-only and mixed selectors, repeated port IDs, omitted topics, `meters`, existing HAL compatibility, malformed route queries and captures too large for one page. Change peer membership and graph generation during paging and verify each page still describes the original cut. Test expiry and an authorized but mismatched reader or scope.
- [ ] Generate the REST request/response schemas, link relations, status/media types, `101` upgrade and `426` documentation from the route inventory. Describe the stream frames and authentication alongside the generated OpenAPI. Verify each reachable endpoint/method matches the documented inventory.

## 6. WebSocket transport and stream delivery

**Owners** — existing `rust/peer-support/src/https.rs`, `https/http.rs`, `https/trust.rs`, and Product management observation/connection handling.

- [ ] Reuse the TLS listener, admission and origin policy with a maintained RFC 6455 framing implementation. Validate authorized service identity, protocol and query before the upgrade. Reject untrusted origin, invalid version and unsupported subprotocol. Route no accepted application frame to operations. Respect connection budgets, credential expiry/revocation and service shutdown. Send Ping after 15 s idle and close after 15 s without its Pong.
- [ ] Stream complete initial snapshot pages at a single cursor, followed by ordered `event`, replaceable latest `meters`, and `resync_required` records. Assign contiguous per-stream `seq` at send, and publish observed times and ownership. Support graph generation and peer/call retirement fences. Reuse typed state/capture and avoid reparsing JSON in producer callbacks.
- [ ] Send state ahead of meters using independent bounded lanes. Count superseded meter updates. Use the specified resync/close behavior on lost state or a blocked writer. Prove shutdown closes/accountably retires all upgraded connections and reloading can revoke a connection without stalling radio or another client.
- [ ] Add an authenticated loopback end-to-end test. Compare the stream snapshot and REST pages for the same held publication. Verify the delivery cadences, fast-state-edge ordering, expired identity, rejected application messages, coalescing, overflow and reconnect. Exercise nginx and Apache Upgrade forwarding, origin validation and equivalent visibility through the existing proxy test paths.

## 7. Controller CLI snapshot command

**Owners** — `rust/cli/src/main.rs`, `http.rs`, a snapshot module in `rust/cli/src/`, `rust/cli/tests/operator.rs`, and existing install/help documentation.

- [ ] Add `rpt-advanced-cli status snapshot [--site] [--port ID]... [--topics LIST]`. Discover the relation from `rpt:status`, encode selectors with the existing URL API, and use the one-shot authenticated HTTP client. Retain existing `status`, `discover` and `get` behavior.
- [ ] Follow same-origin HAL `next` links and merge pages by public resource/measurement identity while boot ID, snapshot ID, selection and state cursor agree. Bound output and continuation work. Report expired, divergent, cyclic or malformed captures as a failure. Emit completed JSON to stdout and diagnostics to stderr.
- [ ] Extend the CLI mTLS fixture to assert discovery and correct selectors, request termination on quiet/disabled ports, and valid JSON for single-page and merged output. Assert useful failure/exit status for bad filters, expired capture, inconsistent metadata, or a link outside the approved origin. Verify installed help/man pages at the affected packaging gate.

## 8. Qualification and handoff

- [ ] Run the targeted Rust suites for `rpt-advanced-core`, `rptadv-peer-support`, `rptadv-product` and `rpt-advanced-cli` using the pinned checkout/SDK and supported Debian 13 environment. Run `make lint dependency-boundary product-surface` and the affected static checks under the maintained image. Verify standalone and same-process Asterisk reload/unload with subscribers present.
- [ ] Run the integrated load cases with simultaneous calls, peers, a port large enough to page, and slow or disconnected viewers. Record actual CPU, memory and callback timing, fairness, byte bounds and state/meter loss results. Verify that a subsequent good snapshot establishes a new cursor after every intentional gap.
- [ ] Update applicable ADRs, architecture and checklist records with verified results. Update the OpenAPI and CLI/operator documentation, packaging and install checks as required. Before integration run the required native Debian 13 amd64/arm64 PR gates and the amd64 production coverage gate. Node deployment or live configuration changes require separate explicit approval.
