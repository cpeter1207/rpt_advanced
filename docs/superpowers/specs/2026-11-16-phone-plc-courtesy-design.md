# SIP PLC continuity and telephone speech-close courtesy

Status: planning checkpoint, 2026-11-16. Product implementation and hardware acceptance have not begun. This design follows the owner's correction: **"Packet loss concealment should not be treated as a gap."** It does not diagnose the reported minor breakups as packet loss.

## Intent and scope

Allow currently negotiated SIP audio to use codec-native PLC when available and the already-installed shared-ring fallback otherwise. Concealed audio may continue an already-open telephone speech interval; it cannot start RF, qualify DTMF, extend speech hold, or renew source age. Add a configurable local-transmitter courtesy assignment for a genuine held telephone speech close, after retained source and processing tails drain.

The current SIP catalog is PCMU, PCMA, G722, GSM, G726, AAL2G726, narrow/wide L16, and iLBC30. iLBC is the applicable native-PLC profile; other advertised profiles use existing fallback where native conceal returns zero. Do not negotiate Opus, Speex, G729, FEC, new packet durations or new jitter defaults. Music/tone classifiers are future research only. Preserve IAX and Asterisk behavior; expanding their phone support is not this work.

## Chosen approach and alternatives

Reuse the serial SIP decoder/jitter owner, timed phone ring, immutable age ledger and existing serialized Morse/telemetry producer. This is the only approach consistent with the existing owner boundaries and the requested bounded change.

Rejected alternatives: treating every loss as retirement contradicts the correction; feeding PLC as ordinary speech would grant synthetic authority; a second concealment engine, telemetry queue, audio worker or codec framework duplicates installed capabilities. No new provider ABI is proposed. Controller-private Rust types may change with their consumers.

## Existing interfaces and required changes

* `rust/standalone/src/phone/codec.rs`: `AudioCodec::decode(&mut self, &[u8], &mut [f32]) -> Result<(), MediaError>` currently hides the result of `Decoder::decode_recovering`. Existing codec-client `Decoder::decode`, `inspect`, and `conceal(requested: u32, pcm: &mut [f32]) -> Result<usize, Error>` suffice to distinguish explicit recovery without a provider change.
* `call_media.rs`: `Pending::Audio { start, count, born, prefix }`, `Pending::Gap`, `Media::read` and its exact source-credit lease are the transport seam. `audio_timed` already has a concealed flag, currently always zero. Jitter `Kind::Insertion` changes playout time only and must remain so. Missing spans need native conceal once, then ring fallback, not decoder retries on each refused prefix.
* `rust/jitter-client/src/lib.rs`: `Jitter::observe() -> Result<Observation, Error>` already supplies duplicate/late/full, missing/insertion/skipped, limit-reset and overload counters. These are playout observations, not a proven network packet-loss rate. RTP sequence is not presently a loss-accounting source and there is no RTCP path.
* `rust/product/src/link/private_phone.rs`: `SourceInput::Audio { concealed, birth, .. }` already keeps DTMF provenance; currently both concealed audio and `SourceInput::Gap` call `Ingress::gap`. Preserve the detector's synthetic-input exclusion while routing concealed PCM to a distinct continuity operation.
* `phone_media.rs`: `Ingress::audio`, `gap`, `voice_decision`, `Shared::record/mark_voice/locate/voice_at`, `TimedInput::render_timed_sample`, and `PhoneMediaInput::render_timed_block` own admission, original time, and processed tail qualification. `voice_until = last qualifying original source sample + 1 + hold_frames`; `hold_frames = source_rate * hold_ms / 1000`. Never freeze or refresh this on PLC.
* `rust/product/src/link/ring.rs`: `InboundProducer::write_tagged` has no synthetic flag; `gap(samples)` requests existing fallback. The ring assigns coordinates to gaps following a tagged prefix. `SampleProvenance` separately exposes position, real, plc, gap, muted, and zero_extension. Codec-recovered PCM therefore needs bounded controller-side provenance metadata, not a false real classification. A mapped fallback slot can have both gap and PLC provider flags; this describes the loss slot, not a policy discontinuity.
* `rust/core/src/config/{schema,settings}.rs`, `runtime/prepare.rs`, `controller/courtesy.rs` and `controller/mod.rs` already prepare and select courtesy media. `link/audio.rs` intentionally routes private unkey through a nameless release with no courtesy. Do not manufacture a public peer identity.

## PLC safety contract

1. The decode/PLC operation executes once per retained jitter decision. Partial/refused sink credit retains identical PCM, coordinates, provenance and original bound. Positive missing prefixes and overlap trimming do not conceal the same source interval twice.
2. Native success is delivered as concealed PCM. A native unsupported span (zero frames) requests the shared-ring fallback once. Nonrecoverable provider errors remain errors, not successful silence. Missing receipt metadata and genuine clock reset are not recoverable packet loss.
3. An accepted loss interval advances source time. Delay insertion does not. Ring starvation without a valid source coordinate remains unqualified, not an excuse to invent a coordinate or receipt.
4. Continuity inherits a conservative immutable bound from already-admitted original history in the same epoch/generation, at least as old as the history it depends on. Do not stamp PLC with the current poll or a later packet's receipt. Unknown, expired, cross-epoch or retired provenance fails closed.
5. Keep exact synthetic classification through conversion and processing. Use bounded metadata allocated at preparation, not a growing PCM/history queue or allocation per packet. Mixed original/synthetic fragments inside one 20 ms receipt bucket must remain distinguishable; a bucket-wide boolean is insufficient.
6. Synthetic samples never reach a speech or DTMF qualifying decision. Already-open output can include qualified continuity only before its original hold end and existing source-age deadline. A newly admitted generation, pre-onset loss, rekey after close, muted interval, or synthetic-only history cannot initiate RF. Original speech can reopen normally.
7. Existing ring G.711 Appendix I fades after 10 ms and reaches silence at 60 ms. These are installed algorithm behavior, not a new global PLC-duration setting. Native PLC and fallback both remain bounded by existing hold, age, authorization and generation contracts.
8. Detector input after a missing interval must not stitch pre-loss and post-loss frames into a fictitious contiguous qualifying analysis frame. Verify the released source-coordinate jump/reset behavior while keeping programme admission alive. Do not feed synthetic waveforms into the classifier merely to advance its cursor.
9. Retirement, hangup, command muting, clock reset, stale/unknown age, reload and authorization revocation still cut output immediately. A loss is not one of those events merely because it needs concealment.

### Evidence boundary: invisible native decoder recovery

The existing codec-client test `decoder_validates_native_counts_and_recovers_only_malformed_packets_once` proves explicit Malformed-to-conceal handling but its return type hides recovery provenance. Real Opus malformed fixtures prove that mechanism independently, without adding Opus to SIP. Existing corrupted iLBC30 fixtures instead return native decode success; they do not prove an explicit Malformed return. The iLBC adapter currently discards native speech output and the provider ABI returns a frame count, not per-frame recovery classification.

This work must label all explicitly requested PLC and reproduced recovery paths. It must not claim that the current ABI reveals every internal codec recovery. If the safety review requires distinguishing an implicit iLBC-success path that can manufacture detector-qualified output, stop that affected portion and present the concrete provider-contract deficiency; do not disable native recovery, add heuristic classifiers, silently narrow the promise, or improvise an ABI extension. Normal compressed decoding remains original input at the existing provider boundary unless the provider reports otherwise.

## Courtesy contract

Configuration adds `input=phone` as one generic telephone-speech assignment per node, separate from receiver/generic-link/exact-peer assignments. `remote_node` remains valid only for link. Reuse existing validation, inherited media settings and prepared Morse:

```ini
[courtesy 524950 telephone]
input=phone
morse_text=P
```

This is a proposed future node configuration, not a change made by planning. Existing defaults are 20 WPM, 800 Hz, -20 dB and 250 ms courtesy delay; preserve configured overrides and do not create a new default tone. Absence of the assignment leaves phone courtesy off.

The event is the **held speech interval closing**, after retained and processed output drains. A low raw VAD score is not itself a close. Loss, short callback, temporary DSP pending, source starvation, mute, retirement, graph replacement and hangup are not speech-close events. Genuine closure needs source-coordinate evidence that the existing hold expired, plus drained qualified output. Do not play a false courtesy for a hard safety cut.

Carry a typed prepared private-call/media-generation identity to the existing planner. Do not use an account name, mailbox, caller identity or fake ASL link. Cancel a pending tone on same-source speech rekey or invalid generation. For active playback, this design chooses cancellation on same-source rekey or retirement, using existing `Playback::cancel`; do not truncate unrelated receiver/link courtesy. This is a phone-specific policy and must be tested before wiring it to the mixer. Pre-prepare retained identity/fences so audio does no allocation, blocking or reclamation.

The tone goes only to local RF through existing serialized station telemetry. It never enters telephone egress, peer fanout, in-band command detection or DTMF regeneration. Existing duplex, half-duplex receive opportunities, timeout priority, site inhibit, RF interlocks, kerchunk and CTCSS policies remain authoritative.

## Verification and delivery boundaries

Follow AGENTS.md, QUALITY.md and ADRs 0002/0003/0008/0044. Update recorded invariants when implemented, not by pretending the old concealment-retirement statement is still true. No SONAME or dependency revision changes are expected. Any necessity for one requires explicit evidence and a coordinated decision.

Use serial focused test batches in freshly pulled maintained Debian 13 quality images, one CPU/1536 MiB and 20-minute bounds. Preserve tests and evidence. About 2.9 GiB was free at planning; no broad cleanup, cache removal, permission changes or builds are authorized by this plan-writing task.

Manual milestone 1: known loss during already-open speech continues without renewed hold, and synthetic-only input does not key. Manual milestone 2: a real held close yields one P after its tail, with rekey cancellation and no phone/peer echo. Each requires a qualified build, explicit deployment/RF authorization and advance interruption notice from the coordinating root. The planner does not access node 524950. Source publication, PR creation, merge and release remain separately authorized.

## Planning review

Self-reviewed against the source interfaces above. The two nontrivial proof obligations are exact mixed-source provenance without ABI changes, and distinguishing natural tail-drained speech close from all hard cuts. Tasks below place tests before those changes. The invisible native iLBC recovery issue is an explicit evidence limit, not a confirmed source defect. No implementation, test run or hardware success is claimed.
