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

- [x] Map the current source and projection for each required observation. Identify retained HAL fields, source quality/freshness support, and the stable identity and generation for ports, peers, calls, graphs, signals, and equipment. Identify opaque provider tap gaps with the adapter that owns each gap.
- [x] Define typed public observation records and an immutable capture containing selection, `boot_id`, `active_revision`, `observed_at`, `monotonic_ms`, and `state_cursor`. Share the capture between REST and stream. Reuse public projections for redaction. Include validity and sample timestamps where available. Keep `snapshot_id`, `page_index` and `complete` on the page representation.
- [x] Put snapshot acquisition and subscription registration at one serialized publication boundary. Retain independent typed state edges from the existing owner or a bounded callback handoff. Give graph and topology creation/removal ordering priority over dependent measurements. Mark edge loss explicitly.
- [ ] Test that an edge inserted during snapshot registration is delivered once after the captured cursor. Exercise a TX pulse and a peer connect/disconnect faster than the present HAL polling interval. Verify consistent boot and revision, and old measurement retirement through reload and disabled-port changes.

### Observation inventory

| Observation | Authoritative owner and identity | Projected boundary |
| --- | --- | --- |
| Lifecycle | Product configuration publication, controller boot UUID, saved/active revision, and station command generation | HAL and stream share the captured lifecycle and visibility. A retired station invalidates its fast edges and audio epoch. |
| Local RX/TX | USBRadioPlus receive metadata, Product worker admission and TX callback, optional shared hardware feedback | RX detail and qualified key transitions use independent bounded callback records. Legacy logical PTT remains a boolean or null. Physical PTT is timestamped independently. Typed standalone RX adds provider-selected carrier and decoder state. Asterisk's provider still needs a typed RX accessor. |
| Peers and calls | Product peer/media owner and private call lifecycle, protocol-qualified public identity, connection generation and exact opaque call identity | Current peers, retained retry and separate phone attempt/call rows. Decoded/egress meters follow exact ownership. Legacy PCM-only key/egress feedback still needs its deciding mixer/provider tap. |
| Local audio | Device/provider boundary, receive callback, local gate and station presentation path | Measure actual exposed signals and their sample positions. USBRadioPlus and its DSP chain own device channels, detector inputs, intermediate RX/TX processing and signaling injection. Provider interiors without a PCM/measurement contract are descriptors marked unavailable. |
| Peer audio | Accepted provider PCM, station mix-minus and codec-input sample positions | Exact controller boundaries retain source/graph epochs. Protocol decoder internals, jitter and codec/PLC trace points, opaque peer processing, and the admitted station-mix input require their owners' additional observation contracts. |
| Telemetry and equipment | Core playback queue and callback, site controller/acquisition and serialized output owner | Public playback lifecycle and scoped sensor/alarm/output status. Private playback has no public event or meter. Equipment observations retain their existing quality and timing where supplied. |
| Resources | 1 Hz Linux sampler on the observer owner | Host/service/process CPU and memory, plus per-interface non-loopback ingress and egress rates with monotonic windows. |

## 2. Site resource sampling

**Owners** — the observer in `rust/product/src/lifecycle_management.rs`, its site resource projection in `management/live.rs`, and a focused Linux resource sampler in `rust/product/src/management/`.

- [x] Sample host/process CPU, memory, and network usage at 1 Hz outside the real-time or serialized runtime paths. Use Linux counters and the effective cgroup, when available, for process and service limits. Publish aggregate host CPU normalized to online capacity, process CPU relative to one logical CPU, the averaging window, CPU count, host available/total bytes, resident bytes, and effective service memory use/limit.
- [x] Derive per-interface `rx_bps` and `tx_bps` for host-visible non-loopback interfaces from byte deltas and actual monotonic elapsed time. Use interface identity to reset baselines on replacement. Publish `window_ms`, time and quality with each reading. Represent unavailable counters and unmeasurable process accounting explicitly. Retire removed interfaces. Use the same site `resources` data for snapshot and state events.
- [x] Test with a controlled monotonic clock and recorded counters. A 1500-byte RX delta and a 750-byte TX delta over 500 ms produce 24,000 and 12,000 bits per second. Cover initial warm-up, reset, disappearance/reappearance, unavailable counters, and a sampling interval different from 1 s. Confirm site-only resource ownership in both projections.

## 3. Controller status producers

**Owners** — `rust/product/src/host_status.rs`, `worker.rs`, `host_site.rs`, phone/call and peer owners, and `rust/core/src/controller/`, `runtime/`, and `link/`.

- [ ] Add RX and TX observations at their actual decision points. Include raw and qualified COS, CTCSS/DCS and detected values where available, VAD, RX mute, qualified-unkey duration and suppression reason, requested and observed PTT, protection/inhibit, and encoder activation. Preserve known versus unknown hardware feedback for radio and speaker profiles.
- [ ] Publish network link direction/type/retry and real connect/disconnect or key/unkey transitions, independent per-call private/authenticated/attached/mute status, and registration/progress. Publish shared and port-owned sensor and alarm changes with observed quality, threshold/latch state and output feedback where available. Publish scheduler activity and bounded media/provider diagnostics through their existing owners.
- [x] Carry distinct telemetry queue, start, finish, cancel and failure events for courtesy, ID, tail and other public messages. Include a public playback identity or category, audience and polite deadline/suppression diagnostic. Filter call-private prompt and authorization details using the same visibility as current HAL status.
- [ ] Verify tests for a qualified short unkey, provider disconnect, physical PTT differing from software request, silent but confirmed audio, a telemetry wait with no PCM, rapid link transitions, reverse-call authentication/attach/mute, stale sensor data, alarm latch and owner recovery. Assert that credentials, private prompt content, caller numbers and collected digits cannot surface.

## 4. Audio graph taps and measurement windows

**Owners** — `rust/product/src/worker.rs`, the link and private-phone media owners, existing Core audio/controller outputs, and provider audio/ABI owners discovered in item 1. Use the released adapter contracts and ADRs 0018, 0021, 0025, 0035, and 0043.

- [ ] Register the actual local RX, peer/call ingress and egress, and TX voice/telemetry graph descriptors. Cover each real processing stage, branch and channel, including decoder and resampler boundaries, mix-minus, signaling injection and digital device output. Share taps where PCM is identical. Keep unavailable hardware/provider internals explicitly described. Update and qualify the corresponding shared provider interface for any tap the existing ABI cannot expose.
- [x] Publish peak and unweighted RMS over 100 ms in dBFS, BS.1770 ungated K-weighted momentary loudness over 400 ms in LUFS, and 2,000 ms peak hold. Reuse existing provider taps, integrated LUFS/true peak and clipping counters where available. Preserve channel identity, valid sample counts, silence, warming, faults and graph-generation fences. Calculate shared, bounded observations independently of the subscriber count.
- [ ] Verify real tone/gain fixtures at multiple stages and channels, under bypass, clipping, silence, sample/true-peak distinction, new gain/route epoch, and stale sources. Confirm ready sample/RMS at 100 ms, ready momentary loudness at 400 ms, and hold clearing at 2,000 ms. Compare against BS.1770/EBU test stimuli. Measure memory and callback timing with and without observers and under multiple viewers.

### Provider work needed for full stage coverage

The released SDKs expose mono station callbacks, cached physical feedback,
and selected post-graph aggregate levels. Stereo capture, final device
channels, separately generated signaling and composite DSP internals need
additional measurement interfaces. Keep each unavailable stage visible in
the descriptor and retire its epochs when the graph changes.

- USBRadioPlus owns the borrowed device input and final stereo render in its
  station callbacks. An optional stopped-only observer and acknowledged
  Asterisk binding can carry these measurements without changing its INI.
- Independently prepared filter ports have stage boundaries. The
  `librptadvradio` session owns receive-detector input, channel routing and
  individual CTCSS/DCS waveforms. A composite FFmpeg graph needs observations
  from its owner for multiband splitting, recombination and DSP internals.
- The current link observer has independent integrated loudness and true
  peak validity. Accept these as extra provider aggregates only where the
  prepared rate equals the actual PCM rate. Keep rate mismatches explicit.
- Qualify matched provider artifacts with independent channel stimuli,
  graph updates and bypass, callback allocation and timing, and shutdown
  with readers present before marking the full coverage item complete.

## 5. HAL snapshot route, selection and pagination

**Owners** — `rust/core/src/management/routes.rs`, `rust/product/src/management.rs`, `management/representation.rs`, `management/schema.rs`, `management/openapi.rs`, `configuration_transaction/tests/http.rs`, and existing CLI/HAL fixture utilities.

- [x] Register canonical routes, status discovery relations `rpt:status-snapshot` and `rpt:status-stream`, and the links from site, port and snapshot views. Parse selection once for both surfaces. Check unknown/malformed selectors with `400`, unavailable IDs with `404` under existing visibility rules, no view with `503`, and regular GET to the stream URL with `426` plus the snapshot link.
- [x] Construct `application/hal+json` pages with `Cache-Control: no-store`. Keep each page within the existing 4 MiB limit. When needed, retain the capture under a bounded opaque reader/selection-bound cursor. Return HAL `next`, a deterministic merge key, repeated snapshot identity/state cursor and the page completion flag. Expiry returns `410` with a fresh-capture link.
- [ ] Test site-only, port-only and mixed selectors, repeated port IDs, omitted topics, `meters`, existing HAL compatibility, malformed route queries and captures too large for one page. Change peer membership and graph generation during paging and verify each page still describes the original cut. Test expiry and an authorized but mismatched reader or scope.
- [x] Generate the REST request/response schemas, link relations, status/media types, `101` upgrade and `426` documentation from the route inventory. Describe the stream frames and authentication alongside the generated OpenAPI. Verify each reachable endpoint/method matches the documented inventory.

## 6. WebSocket transport and stream delivery

**Owners** — existing `rust/peer-support/src/https.rs`, `https/http.rs`, `https/trust.rs`, and Product management observation/connection handling.

- [x] Reuse the TLS listener, admission and origin policy with a maintained RFC 6455 framing implementation. Validate authorized service identity, protocol and query before the upgrade. Reject untrusted origin, invalid version and unsupported subprotocol. Route no accepted application frame to operations. Bound handshake, request and per-subscriber work. Remove fixed subscriber counts and release the HTTP admission slot after upgrade. Respect credential expiry/revocation and service shutdown. Send Ping after 15 s idle and close after 15 s without its Pong.
- [x] Stream complete initial snapshot pages at a single cursor, followed by ordered `event`, replaceable latest `meters`, and `resync_required` records. Assign contiguous per-stream `seq` at send, and publish observed times and ownership. Support graph generation and peer/call retirement fences. Reuse typed state/capture and avoid reparsing JSON in producer callbacks.
- [ ] Send state ahead of meters using independent bounded lanes. Count superseded meter updates. Use the specified resync/close behavior on lost state or a blocked writer. Prove shutdown closes/accountably retires all upgraded connections and reloading can revoke a connection without stalling radio or another client.
- [ ] Add an authenticated loopback end-to-end test. Compare the stream snapshot and REST pages for the same held publication. Verify the delivery cadences, fast-state-edge ordering, expired identity, rejected application messages, coalescing, overflow and reconnect. Keep more subscribers connected than the HTTP request budget and verify that regular GET still succeeds. Exercise nginx and Apache Upgrade forwarding, origin validation and equivalent visibility through the existing proxy test paths.


**Transport verification on Debian 13 amd64** — The fresh quality image
`sha256:c3800ed0094d0d01284b70481d3189a76044042b2900594115db2e2a41f2d0fa`
passed `cargo clippy --locked -p rptadv-peer-support --all-targets -- -D warnings`,
`cargo test --locked -p rptadv-peer-support --lib --test https` (71 unit and
16 regular integration tests before the expiry case), and
`cargo test --locked -p rptadv-peer-support --test https status_stream -- --include-ignored`
(6 focused cases, including the 15-second idle and Pong deadlines, active
certificate expiry, revocation, and overflow with concurrent GET). The verified
HTTPS integration binary passed `real_proxies --ignored --nocapture` in the
same image with nginx 1.26.3 and Apache 2.4.71 from Debian 13. This covers
forwarding and admission. Compare the held Product snapshot and stream capture
in the integrated test before marking the full transport items complete.

## 7. Controller CLI snapshot command

**Owners** — `rust/cli/src/main.rs`, `http.rs`, a snapshot module in `rust/cli/src/`, `rust/cli/tests/operator.rs`, and existing install/help documentation.

- [x] Add `rpt-advanced-cli status snapshot [--site] [--port ID]... [--topics LIST]`. Discover the relation from `rpt:status`, encode selectors with the existing URL API, and use the one-shot authenticated HTTP client. Retain existing `status`, `discover` and `get` behavior.
- [x] Follow same-origin HAL `next` links and merge pages by public resource/measurement identity while boot ID, snapshot ID, selection and state cursor agree. Bound output and continuation work. Report expired, divergent, cyclic or malformed captures as a failure. Emit completed JSON to stdout and diagnostics to stderr.
- [x] Extend the CLI mTLS fixture to assert discovery and correct selectors, request termination on quiet/disabled ports, and valid JSON for single-page and merged output. Assert useful failure/exit status for bad filters, expired capture, inconsistent metadata, or a link outside the approved origin. Verify installed help/man pages at the affected packaging gate.

Verification on the maintained Debian 13 image used the isolated CLI and JSON
workspace with the checked dependency versions. The new mTLS fixture rejected
the pre-change binary. `cargo test --offline -p rpt-advanced-cli` passed all
three integration tests. `cargo fmt --all -- --check`, strict CLI Clippy, and
Ruff checks passed. The staged operator binary printed the new syntax under
`--help`, and all 11 package-combination checks passed. The operator package
has no man page.

## Isolated implementation evidence

The Debian 13 arm64 Rust 1.88.0 run used matched release SDKs and an isolated
test manager. Product `management::status` covered the immutable capture,
cursor ordering, playback termination, peer retirement and large-port paging.
The authenticated Product fixture compared REST and WebSocket views of the
same held cut for the site, a disabled port and mixed scopes. It verified
deduplication of repeated port IDs, recovery of an idle publication after a
lost edge, the scoped upgrade route and the generated method inventory. `management::resources` verified recorded
deltas, replacement/reset and the 1 Hz sampling limit. The site-only
projection test verified that resource accounting is never embedded on a port.

The revised ARM64 Product tests kept 32 subscriber registrations alive and
retired their entries after close. The real mTLS transport test kept 24
simultaneous subscribers while ordinary GET succeeded. It closed and
reopened one stream, then verified normal shutdown and proof retirement for
every subscriber. The HTTP admission slot ends at upgrade. Read, write and
buffer budgets still apply per subscriber.

The ARM64 Product status suite passed all ten targeted tests after sampling
link media counters at one-second intervals in the shared capture. The
regression verified that a fast key edge remained current through both a
suppressed counter-only update and the next published diagnostic sample.

The ARM64 native tap and controller tests verified a separate public TX
telemetry input before ducking. The output retains the existing telemetry
meter identity. The callback tests covered downstream replacement and
private reply exclusion without adding audio-thread allocation.

The isolated Asterisk run completed the 12 same-process reloads with a
subscriber, delivered close code `1001` and EOF at unload, and retired the
radio channel. The subsequent ordinary load/unload stress loop hit a module
unload refusal and retained a fixture channel. Full lifecycle qualification
remains open.

The audio meter, radio feedback, station-input, peer ring and provider PCM
fixtures passed. The pinned Debian 13 amd64 CPU benchmark observed 6.35–7.18
microseconds of thread CPU per 20 ms block for one tap and 44.11–46.88 for seven
concurrently drained taps. The fixture observed zero allocator calls after
preparation. These timings cover the meter accumulation and handoff. Full
provider callbacks, multiple viewers and network load are separate gates.

The maintained Debian 13 amd64 quality image passed
`make lint dependency-boundary product-surface`, the Cppcheck and Clang-Tidy
commands from `make static-analysis`, and Ruff on the additional fixtures.

## 8. Qualification and handoff

- [ ] Run the targeted Rust suites for `rpt-advanced-core`, `rptadv-peer-support`, `rptadv-product` and `rpt-advanced-cli` using the pinned checkout/SDK and supported Debian 13 environment. Run `make lint dependency-boundary product-surface` and the affected static checks under the maintained image. Verify standalone and same-process Asterisk reload/unload with subscribers present.
- [ ] Run the integrated load cases with simultaneous calls, peers, a port large enough to page, and slow or disconnected viewers. Record actual CPU, memory and callback timing, fairness, byte bounds and state/meter loss results. Verify that a subsequent good snapshot establishes a new cursor after every intentional gap.
- [ ] Update applicable ADRs, architecture and checklist records with verified results. Update the OpenAPI and CLI/operator documentation, packaging and install checks as required. Before integration run the required native Debian 13 amd64/arm64 PR gates and the amd64 production coverage gate. Node deployment or live configuration changes require separate explicit approval.
