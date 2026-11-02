# EchoLink Interoperability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the approved EchoLink audio, account, Proxy and incoming-text interoperability to both controller hosts without changing existing AllStarLink or local-radio behavior.

**Architecture:** One independently versioned EchoLink DSO owns the protocol, account lifecycle and non-real-time network work. Both hosts use that provider through tagged peer handles; the shared controller retains admission policy, mixing, telemetry and commands. Reuse installed codec, jitter, conversion and PCM-ring providers, and use released Rust packet libraries where fixture tests demonstrate compatibility.

**Tech Stack:** Rust 2024/MSRV 1.85; Tokio 1.48.0 and the existing bounded-owner pattern; provisional `rtp`/`rtcp` 0.17.2 packet crates; existing MD5 implementation; released zlib decoder; protocol-neutral GSM/Speex DSOs; existing SpeexDSP jitter adapter and shared PCM ring; Fluent; Debian 13.

**Spec:** [Approved EchoLink design](../specs/2026-10-28-echolink-interoperability-design.md). Read it and the applicable ADRs before executing this plan.

## Global Constraints

- This document is a plan, not permission to implement, publish repositories, log in an account, install on 524950, restart a service, or make an external call. Obtain execution approval first and live-test approval separately.
- Preserve standalone/Asterisk package separation. No `chan_echolink`, ASL3 runtime, Asterisk channel or dependency in the EchoLink provider. Do not change USBRadioPlus radio behavior.
- GPL-2.0-only compatibility is required. Select the MIT license alternative for dual MIT/Apache packet crates; audit transitive licenses too. Use released codec libraries, not custom codecs.
- Canonical native audio is normalized mono F32 at 48 kHz. Source-rate peer PCM is 8 kHz; the existing inbound ring alone performs ingress conversion and drift correction. No additional elastic/program ring or filter.
- No socket, codec, allocation, mutex, task spawning, file decoding or speech synthesis in a real-time callback. Per-QSO work is serialized off audio; one peer must not block another direct peer.
- EchoLink media is GSM FR, optionally mutually advertised narrowband Speex. No ULAW, Opus, wideband Speex or WebRTC implementation is added to EchoLink.
- Defaults: 20 stations/account; heartbeat 10,000 ms; timeout 100,000 ms; connect 30,000 ms; directory operation 10,000 ms; registration refresh 300,000 ms; directory refresh 240,000 ms; failed refresh retry 20,000 ms; admission freshness 900,000 ms.
- Default ingress/egress gains are -3/+3 dB, applied once at the protocol edges. Do not change radio/mixer/voice-telemetry gains. Jitter starts at 160/180/500 ms minimum/initial/maximum.
- First-talker ownership releases after 800 ms from its last valid packet and accepted audio drain. Receive inactivity guard is 320 ms, not another post-drain delay. Neither replaces the shared RF timeout.
- Human text speaks only locally with attribution and concurrent audio; forwarding is text-only through capable protocols. Protocol-control text is never spoken. Existing command-source, ID, courtesy and telemetry routing remains authoritative.
- Ordinary metadata/policy reload is live. Exclusive account changes may reconnect that account only; rollback restores configuration and attempts reconnection, not guaranteed uninterrupted connectivity.
- Developer docs accompany each implementation commit. Operator docs/examples change at PR preparation. Run affected tests while iterating; keep coverage/test checklist and reports in ignored `.work/quality-progress.md`.
- Pull `:latest` native test images and record their digests. Fast format/lint/static checks precede pushes; GitHub's full PR gate precedes merge. Debian 13 amd64/arm64 tests/packages; 100% production line/branch coverage only on amd64; no automatic Debian 12 or local QEMU work.
- Repository, package and quality-image publication are explicit implementation prerequisites, not implied by this plan. Release only validated merged revisions; coordinate dependency releases before consumer gates need them.

## Review Focus

1. A valid legacy SDES packet may carry version 3, raw `SPEEX` PRIV and surplus zero padding; accept the pinned fixtures without weakening arbitrary malformed-packet rejection (Tasks 1, 4).
2. A large directory, fragmented Proxy frame or unauthenticated flood must remain bounded without evicting healthy QSOs or exhausting tasks (Tasks 5, 7, 8).
3. Reconnect at the same UDP endpoint and reload during pending callbacks must not let old work operate on a new QSO or freed DSO (Tasks 6, 12, 15); document indistinguishable legacy stale packets honestly.
4. A delayed DTMF tail must stay muted after the detector releases, and a suppressed contender must not issue a command or create RF activity (Tasks 6, 10, 12).
5. Identical AllStar/EchoLink numbers, seven-plus-digit EchoLink IDs and unrepresentable relayed text must not collide, truncate or broadcast accidentally (Tasks 3, 11, 13).

---

## File ownership and delivery order

Paths below are repository-relative. `E` is the new `librptadvecholink` production repository; `EW` its companion `librptadvecholink-workflows`; `C` is `rptadv-codec-adapters`; `I` is `librptadviax2`; `D` is `librptadvdtmf`; `R` is this `rpt_advanced` checkout. Use the shared workflow repository and baseline rules, not duplicated inline workflow implementations.

| Owner/files | Responsibility |
| --- | --- |
| E `src/wire/{mod,voice,control,text,proxy}.rs`, `tests/fixtures/` | Packet-library wrappers, EchoLink-only wire differences and attributed reference bytes; no sockets/codecs |
| E `src/{account,directory,session,media,playout,diagnostics}.rs` | Account/QSO state, immutable directory, media and ordered playout |
| E `src/transport/{mod,direct,proxy}.rs` | Bounded Tokio UDP/TCP I/O; no controller policy |
| E `src/{lib,ffi}.rs`, `include/rptadv_echolink.h` | Sized C descriptor, handles, ownership, errors and quiescence |
| C `rust/client/`, I `src/codec/providers.rs` | Move existing protocol-neutral codec-loader/client glue; keep codec algorithms in their released DSOs |
| C `rust/speex/src/{lib,framing}.rs`, codec contract/header | Legal four-frame Speex operation, only if current operation cannot express it |
| D `src/{lib,dtmf}.rs`, public header; shared-ring `src/ring.rs`, `include/rate_adjusting_pcm_ring3/rate_adjusting_pcm_ring3.h` | Same DTMF algorithm with explicit 8/48 kHz setup; retained source-span muting without another audio queue |
| R `rust/core/src/{command.rs,config/,link/,runtime/links.rs}` | Protocol-qualified identity, configuration, aliases and existing policy |
| R `rust/product/include/rptadv_product.h`, `rust/product/src/{services,host,lifecycle,configuration}.rs`, `link/` | Typed admission, per-peer capabilities, protocol dispatch and source-scoped behavior |
| R `rust/standalone/src/{providers,host_services,foreground,secrets,runtime}.rs` | Standalone composition, tagged handles and account ownership |
| R `rust/asterisk/src/{services,product,application,radio}.rs`, `link/peer_io.rs` | Same provider, no synthetic EchoLink channel; preserve IAX channel branch |
| R new `rust/peer-support/` | Shared host-side tagged-handle/descriptor client and strict secret loader; no protocol/media implementation |
| R tests, messages, Debian manifests and thin workflow callers | Regression tests, localization, package/ABI probes and final operator instructions |

Read `AGENTS.md`, `WISHLIST.md`, `doc/architecture/README.md` and ADRs 0007, 0011, 0018, 0019, 0021–0023, 0025–0027, 0029, 0035–0038, 0040–0041. Record the new contract decisions in ADR 0007 before coding. Preserve unrelated work. Reuse suitable existing checkouts; do not create extra worktrees just for a plan.

### Contract decisions used by later tasks

- Internal `ProtocolId` has `AllStarLink=1`, `EchoLink=2`; `PeerIdentity { protocol: ProtocolId, node: String }` contains a canonical complete decimal ID, never an alias. `PeerCapabilities` is a bitset for async steps, prepared egress, text and remote commands. Metadata is immutable for one connection generation.
- Host-services ABI 9 adds mandatory `int peer_info(void *context, const void *peer, struct rptadv_peer_info_v1 *out)`. The sized record carries protocol, capability bits, account/QSO generations and immutable borrowed UTF-8 spans for real remote ID, validated callsign and local account; product copies them before returning to other work. Spans remain valid until peer destruction. Validate all lengths and capabilities against actual callback groups.
- Product ABI 7 replaces ambiguous incoming arguments with `authorize_incoming(const struct rptadv_incoming_peer_v1 *request, void *peer)` and `incoming(const struct rptadv_incoming_peer_v1 *request, void *peer)`. The sized/versioned request carries protocol plus local, full remote ID and observed source spans. EchoLink must supply a pending provider-owned QSO; no `verified` flag or caller-invented proof. Product rechecks `peer_info` and bound provider state at attach. Existing IAX pre-answer may still use its null-peer authorization path.
- Preserve the current incoming ownership result: `0`/`-1` consume the peer, positive means caller retains it. Teardown is quiescent before destruction. ABI changes require matched hosts/product, capability probes, package dependencies and image manifests; product becomes SONAME `librptadv_product.so.2`/runtime `librptadv-product2`. Do not silently accept ABI 8/6 artifacts.
- E exports `rptadv_echolink_descriptor_v1()`: fixed capability `rptadv.echolink.v1`, size/version, context create/destroy, account prepare/activate/reconfigure/stop/destroy, dial, pending-admission validation, peer info, serial step/attach/detach, PCM read/write/prepared egress, text send and copied diagnostics. All records are fixed C scalars or pointer/length spans, not Rust/library objects. Create/destroy/control execute off audio; calls into a host may be reentrant. Task 2 freezes the complete header and ABI tests before hosts consume it.
- Keep E's runtime/account owners inside host `State`/`ProviderSet`, retained through product stop and callback detachment. One E context owns a bounded Tokio worker pool shared by all its accounts/QSOs, using IAX's established owner pattern and the same Tokio dependency. It does not depend on IAX or share Rust runtime objects across DSOs. Do not add another product-start descriptor bundle or an independent runtime per account/peer. `PeerInfo` selects each operation per handle, not merely because a global host-table function pointer is non-null.
- New E runtime package is `librptadvecholink1`, development package `librptadvecholink-dev`, SONAME `librptadv_echolink.so.1`; release starts at `0.1.0-alpha1`. Both controller hosts depend on the same released DSO. No WebRTC packet utility DSO, plugin framework or public future-protocol API is created.

## Task 1: Prove packet-crate fit before choosing dependencies

**Files:** E create `Cargo.toml`, `Cargo.lock`, `tests/packet_library_fit.rs`, `tests/fixtures/{README.md,echolib-control.json,echolib-voice.json}`. Fixture data records source commit, construction inputs, expected bytes and license attribution.

**Interfaces:** Consumes the pinned ASL/EchoLib sources in spec §2. Produces accepted packet-crate pins and a fixture corpus; no production wire API yet.

- [ ] Write offline RED fixtures for RTP version 3 byte-preserving round-trip, sequence `65535 -> 0`, zero timestamp/SSRC; eight-byte zero-report RR + SDES/BYE; every NAME-length residue modulo four; raw PRIV `SPEEX`; malformed nested counts, lengths and padding. Assert complete parser iteration catches inner errors. Expected valid bytes come from reviewed reference output, not this implementation's serializer.
- [ ] Start with exact released `rtp = 0.17.2`, `rtcp = 0.17.2`; use only required features and matching `webrtc-util` marshal support. Run `cargo +1.85.0 test --locked --test packet_library_fit`; record actual failures before implementing an adapter. No build compatibility is presumed from crate publication.
- [ ] Make only narrow EchoLink adaptations: normalize supported control-header version bits for the generic parser and restore them when emitting; handle the one reference SDES surplus-zero-word case if the fixture proves necessary. Keep raw PRIV opaque. Do not write a second RTP/RTCP implementation or silently fork the crate.
- [ ] Run the same tests GREEN; inspect `cargo tree -e features` and transitive licenses, save the report under ignored `.work/`. Choose MIT for dual-licensed crates. Compare `rtp-types`/`rtcp-types` only if the first pair fails the fit/MSRV/license gate; their strict version/PRIV behavior also needs fixtures. If neither fits without broad custom parsing, stop for a revised dependency decision.
- [ ] Commit the passing fit evidence/corpus and concise dependency decision. They are **not** proof of real-client interoperability. Do not pull in the `webrtc` stack, ICE, DTLS, SRTP, SDP or SCTP for future use.

## Task 2: Establish the independently testable provider contract and gate

**Files:** E create `src/{lib,ffi}.rs`, `include/rptadv_echolink.h`, `tests/test_abi.c`, `Makefile`, `debian/`, `AGENTS.md`, `QUALITY.md`, `.github/workflows/{quality,release}.yml`; EW reusable workflows. Reuse I's scaffold mechanically, removing IAX-specific code/requirements.

**Interfaces:** Produces the complete descriptor named above, opaque `Account`/`Qso`, `Status { Ok, Pending, Full, Unsupported, Invalid, Closed, Failed }`, sized account/peer/event/diagnostic records and supplied-monotonic-time wire tests. `Full` consumes nothing; `Pending` is not a terminal error. `peer_step_attach` may invoke the supplied callback before returning; detach is a barrier.

The C API follows the existing IAX descriptor's operation shapes where their semantics match. New signatures are `context_create(const RuntimeConfig*, Context**)`, `account_prepare(Context*, const AccountConfig*, const AccountSecrets*, Account**)`, `account_activate(Account*, EventFn, void*)`, `account_reconfigure(Account*, const AccountConfig*, const AccountSecrets*)`, `dial(Account*, uint32_t real_id, Qso**)`, `validate_pending(const Qso*, PeerInfo*)`, `peer_info(const Qso*, PeerInfo*)`, `account_stop(Account*)`, `account_destroy(Account*)`, and `context_destroy(Context*)`. All return `Status` except destroys; `EventFn(void*, const Event*)` borrows immutable event data during the call. Prepare validates/allocates without network side effects; activate starts login/I/O. Reconfigure prepares and commits via Task 14. `RuntimeConfig` fixes worker count before activation, defaulting to available CPUs with minimum one and maximum four; no RF-priority workers. Rust implementation names may be private; the header fixes their public C-prefixed names/layouts.

- [ ] Write RED C/Rust ABI tests for size/version/capability, null/invalid records, consumed-versus-retained handles, no unwind across C, partial operation progress, and stop/detach/destroy ordering. Account activation emits incoming/status work to an off-audio host callback with explicit ownership; pending admission cannot claim directory verification through input flags.
- [ ] Run `make test` and confirm missing-contract failures. Freeze exact layouts in the public header and generated bindings; expose no packet-crate, Tokio or codec-library types. Derive the event/operation list from spec §§4–13, not a future plugin system.
- [ ] Implement minimal handle lifecycle and dynamic loading probe sufficient for these tests; add only later operations' required public slots. A disabled account opens no socket and sends no login.
- [ ] Run `make test quality install-check package-check`. Bootstrap deterministic Debian 13 amd64/arm64 image jobs with copied shared baseline, thin callers, rustdoc and C header docs; coverage applies only to production. Add a focused test workflow for iteration, not a second full-gate policy.
- [ ] Commit. Ask for repository/image publication when needed; publish no project or account under plan-only authority. Once approved, establish the gate before production implementation proceeds.

## Task 3: Move codec-client glue and qualify peer identity

**Files:** C create `rust/client/{Cargo.toml,build.rs,src/lib.rs}` by moving I `src/codec/providers.rs` and its generic tests/bindings; update both Cargo workspaces/locks. R modify `rust/core/src/link/{peer,hub,text}.rs`, `runtime/links.rs`, `command.rs`; add `rust/core/src/link/identity.rs` and identity tests.

**Interfaces:** C produces `rptadv-codec-client` with the existing `CodecCatalog`, `ProfileId`, `Encoder`, `Decoder` and result APIs unchanged. R produces `PeerIdentity::parse(protocol: ProtocolId, node: &str) -> Result<Self, IdentityError>`, `EchoAlias::parse(digits: &str) -> Result<u32, IdentityError>` and `EchoAlias::format(id: u32) -> String`.

- [ ] Write RED `codec_client_move_preserves_profiles_and_ownership`, `protocol_identity_does_not_collide`, `echo_alias_round_trips_full_u32`, `echo_dial_waits_for_terminator_or_unkey`: IDs `123456` and `1002775` map to `3123456` and `31002775`; `*331002775#` is one destination. Reject zero, overflow and malformed aliases without falling through to an ASL lookup.
- [ ] Run the new tests with `cargo test --locked -p rptadv-codec-client` and `cargo test --locked -p rpt-advanced-core echo_`; confirm RED before moving/editing implementation.
- [ ] Move, do not duplicate, neutral loader/client logic. Consume the source-only Rust client crate from C's released repository at an exact Git commit recorded in Cargo.lock; publishing to crates.io is not required. Codec implementations still load through versioned DSOs. This introduces no new runtime catalog DSO, static codec implementation or EchoLink-to-IAX dependency. Replace I's implementation with imports and preserve IAX profile mapping/behavior.
- [ ] Propagate canonical identity through hub/retry/group keys, connect tokens, source collectors, status and text routing. Keep the operator alias separate. Resolve aliases before ASL lookup in every existing link operation; no second schedule/fallback engine.
- [ ] Run the moved codec tests plus affected existing IAX negotiation, command and link tests GREEN; record exact filters in `.work/quality-progress.md`. Commit each repository's self-contained move/identity change.

## Task 4: Implement wire wrappers, not another standards stack

**Files:** E create `src/wire/{mod,voice,control,text,proxy}.rs`, `tests/wire_packets.rs`; reuse Task 1 corpus.

**Interfaces:** `parse_datagram(channel: Channel, bytes: &[u8]) -> Result<WirePacket<'_>, WireError>`; `write_voice(packet: &VoicePacket<'_>, out: &mut [u8]) -> Result<usize, WireError>`; `write_control(packet: &ControlPacket<'_>, out: &mut [u8]) -> Result<usize, WireError>`; `ProxyDecoder::push(&mut self, bytes: &[u8], emit: &mut dyn FnMut(ProxyRecord<'_>)) -> Result<usize, WireError>`. `WirePacket` distinguishes voice, SDES, BYE, human chat and station/control information; unknown well-formed optional items are not human text.

- [ ] Write RED `wire_reference_roundtrip`, `gsm_four_units_is_144_bytes`, `control_unknown_optional_is_skipped`, `human_chat_excludes_station_info_and_commands`, `proxy_split_and_coalesced_records`, `malformed_is_not_close`. Include invalid GSM/WAV49, unsupported payloads, zero SDES/BYE sources versus legal zero-report RR, and source-port variants from SvxLink issue 826 as explicit fixtures, not invented universal acceptance.
- [ ] Run `cargo test --locked --test wire_packets`; confirm RED. Use Task 1's selected RTP/RTCP parser/builders for standard fields. Limit hand-written code to documented Echo headers/legacy adaptation, chat grammar and the published nine-byte Proxy framing.
- [ ] Enforce bounds before allocation: IPv4 UDP payload <=65,507 bytes; Proxy UDP records same bound, TCP_DATA <=65,536 bytes, SYSTEM exactly one byte, TCP_STATUS four, TCP_OPEN/CLOSE zero. Unknown types or impossible lengths reset that transport parser with a typed error, not a media-induced QSO hangup. Cap chat at the existing shared human-text bound, using a smaller wire limit if the reference requires one.
- [ ] Run fixture, truncation-at-every-byte and malformed-size table tests GREEN. Verify extra trailing non-padding bytes and malformed nested RTCP items remain rejected. Commit packet wrappers and rustdoc with source attribution.

## Task 5: Resolve account settings and load secrets in both hosts

**Files:** R modify `rust/core/src/config/{mod,scope,schema,settings}.rs`, `rust/product/src/configuration.rs`; create `rust/core/src/config/echolink.rs`, `rust/peer-support/{Cargo.toml,src/lib.rs,src/secrets.rs,src/echolink.rs}`; move shared checks from `rust/standalone/src/secrets.rs`, update both hosts' bindings/build metadata. E account configuration records. New Rust crate package name: `rptadv-peer-support`.

**Interfaces:** `EchoLinkSettings::resolve(document: &ConfigDocument, node: &NodeId) -> Result<Resolution<Self>, ConfigError>` uses existing parsed types/warning handling; `load_account_secrets(path: &Path, account: &str) -> Result<AccountSecrets, SecretError>`. `AccountSecrets` is private, not printable; same strict loader serves both hosts. Resolved settings produce the Task 2 account record off audio.

- [ ] Write RED tests for global/node fallback, identity never inherited, disabled defaults/no provider required, all spec §5 metadata and named modes, malformed value warning/default, enabled invalid account rejection, duplicated callsign/endpoint rejection, and regular-file/owner/0600/no-symlink secret checks. Do not include real passwords or token-shaped fixtures.
- [ ] Run `cargo test --locked -p rpt-advanced-core echolink` and `cargo test --locked -p rptadv-peer-support secrets`; confirm RED. Keep parameter names/defaults exactly as the spec, including `audio_port+1`, Proxy TCP 8100 and host-relative default secret paths.
- [ ] Implement resolution and move the strict loader; reject a candidate only when defaults cannot produce a safe deterministic enabled account. Validate unique local bind endpoints and checked station/handshake budgets. Preserve all existing radio/gain settings.
- [ ] Run affected config/secrets tests GREEN, including per-host service-user ownership and no secret in warning/Debug/status records. Commit configuration and shared host glue; defer operator examples until Task 16.

## Task 6: Directory-backed admission and QSO state

**Files:** E create `src/{directory,account,session,diagnostics}.rs`, `tests/{directory,qso_state}.rs`.

**Interfaces:** `DirectorySnapshot::lookup(&self, callsign: &str, address: Ipv4Addr, now: Instant) -> Result<DirectoryEntry, AdmissionError>`; `Account::handle(&mut self, event: AccountEvent, now: Instant, actions: &mut Vec<AccountAction>)`; `Qso::handle(&mut self, event: QsoEvent, now: Instant, actions: &mut Vec<QsoAction>)`. Actions are bounded by the configured mailbox budget. Pending QSO verification is provider-owned and can be rechecked without trusting host input.

- [ ] Write RED tests `directory_plain_compressed_and_delta_atomic`, `stale_directory_refresh_fails_closed`, `outage_retains_good_snapshot_and_live_calls`, `callsign_and_address_must_match`, `deny_wins`, `simultaneous_admission_respects_limit`, `heartbeat_silence_is_not_hangup`, `single_bad_packet_does_not_close`, `first_terminal_cause_is_retained`.
- [ ] Run `cargo test --locked --test directory` and `cargo test --locked --test qso_state`; confirm RED. Add decompression-bomb, half-applied delta, changed address, duplicate/conflicting ID, remote BYE and same-endpoint reconnect fixtures.
- [ ] Implement plain/zlib full/differential directory parsing using a released decoder. Start with hard internal limits: 2 MiB compressed/16 MiB expanded snapshot, 100,000 records, 1,024 bytes/line; reject before publishing and retain last good state on overflow. These guard resources, not advertised node-count limits; expose failure counts. Use all spec account/refresh deadlines and ordered server failover.
- [ ] Implement account and QSO states, fresh SDES establishment, monotonic liveness and one first-cause terminal event. Valid selected or suppressed session-bound voice/control refreshes network liveness; malformed traffic/chat does not. Ordinary explicit disconnect does not retry; existing controller permanent/scheduled policy does. Do not fabricate stale-BYE discrimination where wire bytes are identical.
- [ ] Implement first-talker selection and separate raw/selected receive state. Tests assert the 800 ms/drain rule, 320 ms guard without a second delay, contender cannot issue commands/RF activity, and accepted tail never leaks into the next talkspurt. Run GREEN and commit.

## Task 7: Fair direct transport and directory transactions

**Files:** E create `src/transport/{mod,direct}.rs`, `tests/{direct_transport,fairness}.rs`; wire account/directory actions from Task 6.

**Interfaces:** `Transport::try_send(&self, destination: Endpoint, packet: OwnedPacket) -> Result<(), SendError>`; `Transport::stop(&mut self) -> StopFuture<'_>`; account events carry authenticated observed endpoints, immutable owned bytes, monotonic arrival and generation. `DirectoryTransport` exposes one bounded connect/read/write transaction used by direct and Proxy transports.

- [ ] Write RED loopback tests for login positive/negative/logout/refresh, directory TCP split reads/EOF/timeouts, UDP audio/control routing, endpoint spoofing and two independent accounts. No test logs into the public directory.
- [ ] Write `slow_peer_does_not_delay_other_peers`, `control_survives_media_backpressure`, `unauthenticated_flood_has_bounded_state`, `shutdown_detaches_before_free`. Simulate socket readiness rather than a live network; assert bounded queued bytes and no task per packet.
- [ ] Run `cargo test --locked --test direct_transport` and `cargo test --locked --test fairness`; confirm RED. Implement on the existing shared Tokio pool, one serial owner/QSO and account socket owners. Use bounded mailboxes of 64 events and 256 KiB/accounting per QSO, 16 events/turn before yielding. Reserve eight event slots for control; shed only complete unsent stale media under the existing media-age policy.
- [ ] Limit pending unauthenticated handshakes with `max_pending_handshakes`, default checked `2 * max_stations`, inherited per account and separately counted. Retain no task per unknown datagram. Read/write failures produce typed evidence; one malformed packet cannot close a healthy call.
- [ ] Run GREEN including >=1,000 simulated peers without encoding work, account starvation and close while attach reenters. Record scheduling/queue maxima as measurements, not guaranteed unlimited scale. Commit.

## Task 8: Implement the standard TCP Proxy client

**Files:** E create `src/transport/proxy.rs`, `tests/proxy_transport.rs`; use Task 4's record decoder and Task 7 directory interface.

**Interfaces:** `ProxyTransport::connect(settings: &ProxySettings, secrets: &AccountSecrets, runtime: &RuntimeHandle) -> ConnectFuture<'_>` produces the same transport/event interface as direct mode. `ProxySettings` is the resolved account host/port; there is no fictitious UDP relay pair.

- [ ] Write RED local fake-Proxy tests for eight-byte nonce, uppercase-password MD5/raw digest, newline callsign, BAD_PASSWORD/ACCESS_DENIED, TCP_OPEN/STATUS/DATA/CLOSE directory tunnel, UDP data/control origin address and incremental record writes. Include zero-length permitted records and every header split boundary.
- [ ] Run `cargo test --locked --test proxy_transport`; confirm RED. Reuse the released MD5 implementation already selected by IAX; do not use it for administrative-code storage.
- [ ] Implement one authenticated TCP client per account, bounded partial-record staging and serial directory tunnel transactions. Never drop part of a submitted record; keep control reservations and drop only whole unsent stale media. Reject oversized records before allocation and report Proxy head-of-line stalls separately from loss.
- [ ] Test proxy busy/disconnect/reconnect and unaffected second-account/direct QSO. Assert no account receives another's directory/voice traffic or secrets. Run GREEN and commit.

## Task 9: GSM media, jitter and ordered draining

**Files:** E create `src/{media,playout}.rs`, `tests/{media,jitter_playout}.rs`; use C's moved codec client. R `rust/product/src/link/{ring,source_clock,egress}.rs` only where protocol-independent existing behavior must be exposed, not duplicated.

**Interfaces:** `MediaSession::receive(&mut self, packet: VoicePacket<'_>, arrival: Instant) -> Result<(), MediaError>`; `MediaSession::poll(&mut self, now: Instant, sink: &mut dyn PcmSink) -> Result<MediaProgress, MediaError>`; `MediaSession::encode(&mut self, pcm: &[f32], end: bool, out: &mut PacketBatch) -> Result<EncodeProgress, MediaError>`. `PcmSink` has `audio(source_start: u64, pcm: &[f32])` and `gap(source_start: u64, samples: u32)` with explicit accepted counts; retry suffixes exactly once.

- [ ] Write RED `gsm_80ms_roundtrip_uses_four_raw_units`, `sequence_wrap_zero_timestamp_works`, `reorder_and_loss_keep_source_positions`, `codec_plc_precedes_ring_fallback`, `backpressure_preserves_partial_audio_and_gap`, `finite_egress_tail_is_not_lost`, `retry_reuses_encoded_bytes`, `unkey_drains_tail_before_final_event`.
- [ ] Run `cargo test --locked --test media` and `cargo test --locked --test jitter_playout`; confirm RED. Use profile 3 GSM FR (33 bytes/160 samples, four per packet), dynamic codec provider, existing jitter and ring interfaces. The parser packet wrapper supplies the twelve-byte Echo header.
- [ ] Derive the 8 kHz source timeline from unwrapped 16-bit sequence and 640-sample packet span when timestamps are unusable. Use monotonic arrival, 160/180/500 ms jitter defaults, real segment resets and ordered missing spans. No codec-history advance for jitter insertions; no double concealment. GSM gaps reach ring fallback because its provider has no native PLC.
- [ ] Preserve selected-talker drain and one final unkey through jitter, decoded pending PCM and ring. Egress reuses shared conversion with finite flush, bounded prepared packets, scheduled sends and no stale catch-up burst. Apply -3/+3 dB only once at their respective edges.
- [ ] Run GREEN with delayed/reordered/duplicate/empty packets, 8 -> 48 -> 8 rates and partial sink acceptance. Measure packetization/jitter/ring latency separately. Commit without changing IAX codec or media behavior.

## Task 10: Reuse the DTMF detector at 8 kHz and preserve delayed muting

**Files:** D modify `include/rptadvdtmf/rptadvdtmf.h`, `src/{dtmf,lib}.rs` and detector/ABI tests. Shared ring (`rate_adjusting_pcm_ring` repository) modify `src/ring.rs`, `include/rate_adjusting_pcm_ring3/rate_adjusting_pcm_ring3.h` and tagged-output tests. R modify `rust/core/src/audio/dtmf.rs`, `rust/core/src/link/{signals,audio}.rs`, `rust/product/src/link/{ring,session}.rs` and muting tests.

**Interfaces:** Append D's checked descriptor tail `create_at_rate(uint32_t muting, uint32_t sample_rate_hz, struct rptadv_dtmf **out)` and `process_spans(...)` with the existing process arguments plus a borrowed mute-interval callback/context. Report signed block-relative start/end source offsets, including qualifying lookback. Existing `create(muting,out)` and `process` still serve required 48 kHz RF callers. Accept 8,000 and 48,000 Hz; preserve real time with 102/612-sample analysis intervals. Current client prefix-size checks permit ABI 1/SONAME 1 to remain unchanged.

The shared ring needs intentional-mute metadata: it currently has only private missing-span mapping and no public muting/source-cursor API. Append a sized `push_tagged` operation accepting PCM, generation and absolute source mute intervals with bounded lookback, plus an interval-only update for already accepted buffered PCM. Preserve explicit partial-acceptance results. Reuse its existing gap/FIR-delay timeline to apply tags when samples actually leave the ring, including cached/resampler/PLC output. Intentional mute emits silence and increments neither loss nor PLC counters. Publish tags before making the corresponding PCM visible; audio readers never wait for the producer.

The existing product's serialized `PeerSession` owns this detector and its `SourceClock`/ring producer. After reading selected EchoLink 8 kHz PCM from the host and before publishing it to `InboundProducer`, it runs the DTMF provider, emits source-scoped digit events, and publishes tags. E owns jitter/codec decoding, not another ring or controller DTMF collector. Add generation-scoped atomic suppression to `PeerSignals`; the real-time ring consumer applies that immediate override in addition to retained ring tags. Keep IAX's existing out-of-band digit path unchanged.

- [ ] Write RED partition-equivalence tests for all sixteen digits at 8/48 kHz, rejected noise/twist, release/termination, 48 kHz unchanged, and descriptor-tail size validation. Run `cargo test --locked -p librptadvdtmf`; expect missing new operations.
- [ ] Implement rate setup in the existing detector, not a second decoder or resampler. Update public docs/descriptor tests and the consumers that require the new tail. Existing RF/generator APIs retain their useful semantics; no compatibility-only alternate detector.
- [ ] Write RED `dtmf_prefix_muted_with_delay`, `dtmf_tail_stays_muted_after_release`, `command_termination_does_not_unmute_past_spans`, `suppressed_talker_cannot_emit_digit`. Run focused R ring/session and shared-ring tagged-output tests before implementing source-span tags.
- [ ] Detect selected 8 kHz PCM before ring publication; preserve bounded interval metadata, use current detector/command suppression as an immediate output override and retain past inhibited spans until consumed. Follow the ring's actual source-consumption map; neither occupancy nor output callback count is a source cursor. The descriptor-tail ring extension must preserve current callers, source-clock/PLC ordering and consumer ownership; update its dependency pins and ADR before publication. No additional samplerate-adapter extension is currently justified.
- [ ] Run all affected tests GREEN, including a short digit entirely inside the delay, 8 -> 48 kHz conversion with compensation, partial admissions, cached/FIR/PLC tails, reset/generation change and large variable callback partitions. Document that insufficient delay can expose an already played prefix. Commit D/ring/R changes separately; release dependencies only when authorized.

## Task 11: Prove legal narrowband Speex packetization

**Files:** C `rust/speex/src/{lib,framing,framing_tests}.rs`, public codec header and `rust/contract/src/lib.rs` if needed; E media/control tests.

**Interfaces:** Keep existing one-unit Speex behavior for IAX. First try the current bounded encode interface for a legal 640-sample/four-frame Echo packet. If it cannot express one continuous bitstream, add neutral `encode_batch(handle, pcm, samples, frame_count, out, capacity, result)` with exact accepted/result counts; require the sized tail only for negotiated Echo Speex. It is a codec operation, not an Echo protocol parser.

- [ ] Write RED reference test decoding a single packet containing four legal 20 ms NB Speex frames with the upstream decoder; compare PCM length 640 and reference fixture bit boundaries. Add mutual raw-PRIV `SPEEX` negotiation and no-provider/GSM fallback tests.
- [ ] Run C's focused Speex framing tests and E's negotiation tests; confirm whether the existing operation already passes. Do not append independent padded encoded byte strings or manipulate Speex bits in E.
- [ ] Make the smallest provider-owned batching change only if the test requires it. Preserve decoder-native PLC, quality/profile choices and every existing IAX packetization test. No wideband or Opus Echo advertisement.
- [ ] Run GREEN with truncated/malformed frames, encode backpressure and finite tails. Update codec descriptor pins/ABI documentation if extended; no incompatible change without coordinated rebuild/package consequences. Commit the necessary change, or record that existing operation met the requirement without adding API.

## Task 12: Integrate one provider into both hosts

**Files:** R product public header, `build.rs`, `src/{services,host,lifecycle,configuration}.rs`, `src/link/{session,text,outbound}.rs`; shared `rust/peer-support/src/echolink.rs`; standalone `providers.rs`, `host_services.rs`, `foreground.rs`; Asterisk `services.rs`, `product.rs`, `application.rs`, `radio.rs`, `link/peer_io.rs`; fixture/test headers and package/ABI probes.

**Interfaces:** Implements host ABI 9, product ABI 7 and the Task 2 Echo descriptor. Shared `HostPeer` tags IAX versus Echo handles; every operation dispatches by that tag. `PeerIo` copies `peer_info`, then enables only the advertised complete callback groups for that peer.

- [ ] Write RED `mixed_protocol_host_dispatch`, `unverified_echo_peer_is_rejected`, `final_attach_revalidates_generation`, `iax_only_callbacks_remain_iax_only`, `async_attach_reentry_is_safe`, `detach_blocks_late_notification`, `partial_descriptor_fails_before_side_effect`. Exercise radio binding, reads/writes, prepared egress, deadlines, observer, text, destruction and tokens—not only dial.
- [ ] Run focused product service/session tests and host dispatch tests; confirm RED. Add canonical protocol identity to incoming records and validate fresh provider-bound directory/callsign/address/account/QSO state before controller deny/allow/limit/loop gates. Do not use an unchecked `verified=true` field or metrics observer as admission proof.
- [ ] Implement tagged shared host glue. Keep IAX's Asterisk channel attachment path; Echo binds the existing product/radio mix without creating a channel. Skip `!NEWKEY1!`, ASL `D`, topology, `K?` and radio-control negotiations for Echo. Use selected-talker events, not heartbeat/connection/raw contender state, as receive activity.
- [ ] Keep provider Libraries, account owners and contexts alive through product stop; never retain a mutable peer borrow across a reentrant serial step. Stop notification producers, detach/barrier, release QSO, then account/context/DSO. No new runtime or thread per peer.
- [ ] Run GREEN with IAX regression fixtures, zero Echo accounts, mixed prepared/unprepared peers, both hosts, and stop during pending dial/admission. Coordinate product SONAME 2, host/product capability checks, loader fixtures, Debian dependencies and image ABI pins in the same integration commits.

## Task 13: Protocol-correct text, commands and telemetry

**Files:** R `rust/core/src/{command.rs,link/text.rs}`, `rust/product/src/link/{text,session}.rs`, shared text producer/router tests, `messages/en-US.ftl`; E `src/wire/text.rs`, QSO text send/receive; existing typed controller operations/status.

**Interfaces:** `TextRoute::encode(message: &HumanMessage, destination: &PeerIdentity) -> Result<OwnedTextPacket, UnsupportedRoute>` delegates to each actual protocol. Preserve authenticated ingress separately from represented origin. Add typed `set_echo_telemetry(node, mode)` using the existing controller command surface; optional DTMF mapping has no built-in new prefix.

- [ ] Write RED `echo_text_speaks_locally_with_callsign`, `text_and_sender_audio_mix`, `text_interrupts_voice_telemetry_to_morse`, `protocol_control_never_speaks`, `synthesis_failure_does_not_block_forwarding`, `unrepresentable_origin_or_audience_is_not_broadcast`, `forwarded_text_cannot_authorize_commands`.
- [ ] Run focused router/producer tests and E text tests before implementation. Resolve a relayed callsign through the fresh directory only as represented identity, not authentication. Numeric Echo origin uses its full reversible ASL alias when that destination can represent it. Preserve literal body and no-echo rules; unsupported routes count without rewriting bodies or inventing control envelopes.
- [ ] Wire all existing link operations to aliases, retain per-source collectors/unlock/replies, pad test and local-only regeneration. An Echo `*4` route fails with localized unsupported-route telemetry; no ASL `D` text or invented OOB digit command. `telemetry_dynamic=no` rejects runtime selection while reload still works. Apply existing command authorization and mapping collision checks.
- [ ] Use connection announcement `off|node|callsign|both` and telemetry `off|on|follow|demand` according to source-verified ASL semantics under current routing/politeness rules; refresh/retry traffic never creates received-audio activity. Run all tests GREEN including disable/reconnect cancellation and no synthesized speech/courtesy/ID/hang leakage to peers. Commit.

## Task 14: Live reload, diagnostics and bounded failure isolation

**Files:** E account, diagnostics, transport and FFI modules; R lifecycle/configuration, host owners, existing status/CLI/DTMF presenters; tests `reload_lifecycle.rs`, `diagnostics.rs` in E and focused host tests in R.

**Interfaces:** `Account::prepare_update(settings: AccountSettings) -> Result<PreparedUpdate, UpdateError>`; `Account::commit_update(update: PreparedUpdate) -> UpdateOutcome`. Copied snapshots include registration/transport, freshness, full identity, codec, raw/selected talker, heartbeat/deadline, first cause, retries, queue/media ages, malformed/late/lost/reordered, jitter, codec/ring PLC, text failures, actual-send and Proxy-stall counters.

- [ ] Write RED `metadata_reload_keeps_live_call`, `policy_reload_gates_next_admission`, `exclusive_reload_reconnects_only_changed_account`, `failed_replacement_restores_config_not_false_online`, `same_endpoint_old_generation_is_discarded`, `cancel_before_library_drop`, `unexpected_close_logs_first_cause_once`.
- [ ] Run focused reload/diagnostic tests; confirm RED. Use prepared generations; do not double-login one account or require host restart for ordinary settings. Stop exact source text/commands and quiesce callbacks before releasing state. Identity/secret/socket replacements may interrupt that account only.
- [ ] Implement copied metadata-only diagnostics through existing status/logger ports, with rate-limited useful errors and distinct controller-requested versus unexpected closes. Never log credentials, Proxy digest, human body or unbounded malformed packet bytes. Distinguish queue admission, actual send and unproven remote receipt.
- [ ] Run GREEN with directory outage, Proxy HOL, one full peer queue, malformed flood, speech failure and account rollback. Current local radio/IAX activity must continue. Commit with updated developer docs and ADR boundary/status notes; do not mark real interoperability verified yet.

## Task 15: Finish focused coverage and isolated end-to-end validation

**Files:** E tests `end_to_end.rs`, `resource_limits.rs`, C ABI/install probes; R `tests/test_rust_asterisk_lifecycle.py`, `tests/test_product_abi.c`, new `tests/test_echolink_lifecycle.py`; shared workflow callers/manifests.

**Interfaces:** Loopback directory, Proxy and QSO fixtures exercise real provider/header/library paths with fake time where possible. Test harness records fixture source versions, package revisions and image digest; no public credentials or network calls.

- [ ] Build one traceability checklist mapping every spec section and each Review Focus item to its exact tests. Use `make coverage` in E and the existing `make rust-coverage` in R for the first coverage reports on Debian 13 amd64; turn gaps into the ignored checklist, then close one component at a time with focused tests/coverage/rustdoc. Delete dead code rather than suppressing warnings or exempting production branches.
- [ ] Exercise direct GSM and negotiated Speex, Proxy directory+media+chat, protocol crossover/no-echo text, partial I/O, loss/reorder/wrap, handoff drain, DTMF delayed tails, 1,000-peer fake transport fairness and bounded unknown traffic. Assertions cover source identity, count/order/latency, not merely non-crashing self-roundtrip.
- [ ] Test both host compositions with installed DSOs: load/reload/unload in the same Asterisk process, standalone with no Asterisk/ASL dependency, independent USBRadioPlus install, product/host mismatch rejection, current IAX behavior unchanged. Run `readelf -d`/`ldd` package probes and staged-install checks; remove all stale test output via the labeled launcher before rerun.
- [ ] Once focused evidence is complete, mark the candidate ready for the full GitHub PR gate in Task 16, using current verified Debian 13 native amd64/arm64 images and 100% production line/branch coverage only on amd64. Do not add a duplicate full pre-push/local gate. A failed PR report becomes the next component checklist, not a reason to rerun the entire gate after every line fix.
- [ ] Commit verified test/build/package changes. Do not claim official-client interoperability, public inbound reachability or Proxy deployment from these isolated tests.

## Task 16: Operator instructions, authorized field test and PR handoff

**Files:** R `README.md`, `doc/`, `examples/rpt_advanced.conf`, `WISHLIST.md`, `doc/architecture/README.md`, ADR 0007; E README/operator docs/package changelog; C/D/I documentation only where their interfaces changed. Exact manual source paths come from each repository's existing manifest, not a new parallel manual.

**Interfaces:** Published docs describe current operation in plain amateur/land-mobile terminology; implementation details belong in developer docs. A live acceptance record identifies each client/version, transport, codec and result.

- [ ] Update operating instructions immediately before PRs: account provisioning/secret ownership, direct ports, standard user Proxy, public-address constraints, all metadata/modes/gains, full-ID dialing and terminator, timeout meanings, reload/recovery/status and known cross-network loop/provenance limitations. Describe what the product does, not past behavior or design history.
- [ ] Ask permission before installing/login/restart/external tests on 524950. If granted, stage ABI-matched candidates in the user's home workspace, preserve radio settings and known-good configurations, and activate only the authorized set. No silent package/index/release publication. Confirm local repeat, courtesy and *722 before linking if requested.
- [ ] With explicit account/call permission, test official EchoLink, ASL3, SvxLink/EchoLib and a conference station; direct/Proxy; GSM/Speex where supported; both directions, silence heartbeat, text attribution/voice-to-Morse, local-only speech, commands, drain/unkey and reload. Record unavailable combinations as unverified; ask the owner to confirm listening results.
- [ ] Mark the ADR/wishlist implemented only for the verified delivered scope; keep unresolved official-client variants or deployment limitations explicit. Review the whole branch for behavior changes outside scope, duplicate implementations and unnecessary dependencies. Run pre-push format/lint/static checks, then obtain authorization to push/open PRs; full gates must pass before any merge.
- [ ] Release dependency packages and merge consumers in dependency order only with release/merge authority. Update quality-image pins after actual releases. A final controller release/install is a separate approved action, not an automatic consequence of approving this plan.

## Self-review and handoff

- Spec §§1–4: Tasks 1–4 and 12 preserve scope, provider boundaries and reuse.
- Spec §§5–7: Tasks 5–8 and 14 cover account config/secrets, directory and both transports.
- Spec §§8–10: Tasks 3, 6 and 9–12 cover identities, QSO state, DTMF, GSM/Speex, jitter/PLC and shared-ring ownership.
- Spec §§11–12: Tasks 12–14 cover protocol-correct text/telemetry, generation fences, live reload and shutdown.
- Spec §§13–14: Tasks 15–16 cover quality, field evidence, docs and approval boundaries.
- All five Review Focus cases have named regression tests. Tasks use one set of identity, ownership and capability contracts; external crate types never cross the C ABI.
- Dependency selection remains conditional only on the explicit first-task compatibility gate. No other protocol implementation or WebRTC framework is hidden in this plan.

Plan review/confirmation is the next step. No implementation or node change has been performed by writing this plan.

## Packet-library research references

- [rtp 0.17.2](https://crates.io/crates/rtp/0.17.2), [rtcp 0.17.2](https://crates.io/crates/rtcp/0.17.2), [webrtc-rs source](https://github.com/webrtc-rs/webrtc): provisional standard-packet reuse, not a whole-stack dependency.
- [rtp-types 0.1.2](https://crates.io/crates/rtp-types/0.1.2), [rtcp-types 0.3.0](https://crates.io/crates/rtcp-types/0.3.0): smaller comparison candidates; legacy version/PRIV behavior still needs verification.
- [Pinned EchoLib packet source](https://github.com/sm0svx/svxlink/blob/595cf9bb3325be753d05b219bdd39feab6a22b33/src/echolib/rtpacket.cpp): fixture source for version bits, zero-report RR, SDES padding and opaque PRIV.
- [Published Proxy protocol](https://www.svxlink.org/doc/echolink_proxy_protocol.html): nonce/digest, nine-byte framing and directory/UDP records. The approved spec holds the remaining ASL3, official-client and EchoLib references.
