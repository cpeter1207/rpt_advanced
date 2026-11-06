# Earshot local receive VAD

Source: the user's VAD request and subsequent selection of Kerchunk filter for node524950. Exact requirements and source checkpoints are retained in `HybridCoder/state/vad-20261106/requirements.json`.

## Behavior

- `off`: preserve the current receiver behavior, delay and cost; no SRC or neural prediction.
- `kerchunk`: retain every configured COS/CTCSS/DCS requirement. Block audio and receive admission until the first fresh speech decision in the RF-qualified burst, then latch admission until that qualification closes. A subsequent burst must detect its own speech.
- `on`: require speech activity or its hang, together with every enabled RF qualification. When RF requirements are absent, speech is the receive qualification; disabled audio, stale required snapshots, duplex, blanking and post-transmit guards still apply.
- Configurable `vad_hang_ms`, default 1000, measured on the native sample clock. RF loss closes admission immediately. With no RF requirement capable of ending a burst, Kerchunk uses On semantics and emits a configuration diagnostic.
- Existing configurations default to Off. Install node524950 with Kerchunk filter after validation.

## Detector and ownership

Earshot 1.2.2 belongs to the portable, released `librptadvradio` receive owner immediately after squelch. It consumes exact 256-sample 16 kHz analysis frames at threshold 0.5. Keep programme PCM at 48 kHz. Reuse the released samplerate ABI2 through the same neutral descriptor-client pattern as the existing squelch/CTCSS/DCS clients. No duplicate resampler, detector, hardware owner, PCM delay queue, or background inference thread.

Prepare all state outside callbacks. At stream or RF-burst rearm, reset SRC/history, clear partial analysis, score, latch and hang, reset Earshot and predict three zero frames. Synthetic priming advances no audio clock. Actual converter input-used/output-generated counts and live delay determine analysis positions. Clamp the analysis copy to Earshot's normalized input range; do not modify programme levels.

Earshot's released API has no supported state clone. Experiments across all 256 analysis-frame alignments found reset plus three silent frames reduced two cold speech fixtures' worst measured onset delays from 288/109 ms to 29/51 ms. Continuous state falsely classified new noise after a rapid speech/unkey/rekey sequence. These are fixture results, not a universal RF guarantee. Verify real SRC delay, callback partitions, native ARM64 execution and allocation behavior.

## One delay line and burst admission

Use a minimum 160 ms reserve only when VAD is active; preserve any larger configured squelch delay. The existing local PCM ring/delay remains the only audible delay line. Keep capture/render flowing while speech admission is pending; mute delivery until admission, retaining the bounded lookback. Never infer admission from PCM energy or the ring's real-sample count.

Separate raw RF eligibility from speech admission. Carry the radio generation, first native sample/count, RF-burst identity, last fresh voiced-decision position, effective mode and minimum reserve through one sized native RX metadata record. Match speech admission to the unchanged raw RF token after rendering. Raw loss invalidates the admission token and mutes the old accepted tail, including SRC caches, through the released ring's existing source-position metadata. VAD keeps one continuous tagged timeline with raw-false PCM explicitly zeroed; a normal carrier closing does not reset the ring or discard a new carrier's first block. A previous burst cannot qualify or replay into a later one. Actual faults use a separate reset-required epoch and require the consumer acknowledgment before publication resumes. The TX endpoint also checks the identity of the currently adopted RX handoff, preventing an old-ring callback from surviving a concurrent replacement/cancellation.

Publish speech admission only after the decisive PCM was fully accepted. A short write, overflow, source discontinuity or failed reset invalidates admission, resets/re-primes the existing ring, and requires a fresh post-discontinuity voice decision. No temporary mute interval requiring a later unmute is published. This policy requires no ring provider ABI change.

Legacy AppRpt8k analyzes undelayed squelch-qualified speech and uses the radio's existing delay storage. When VAD is enabled, run the existing de-emphasis once before analysis/storage; invalidate delay contents on RF loss and mode/storage transitions. Preserve the exact original Off path. Both standalone and app_rpt_advanced use the controller-owned PCM ring when VAD is active. Standalone always disables the radio's internal delay. AppRpt8k uses internal storage; the legacy Advanced Off path preserves its original internal squelch delay with external-delay disabled.

## Interfaces and configuration

- Radio descriptor ABI/SONAME 5, alpha10: add mode/hang/external-delay policy and raw-versus-admitted RX metadata. Preserve failed-setup, update/rollback and deferred reclamation rules.
- USBRadioPlus alpha31: translate shared receive settings, bind radio ABI5, expose Receive-page mode/hang controls, and project typed RX metadata through direct-callback ABI6. Native request ABI4 acknowledges the nested radio contract change. Preserve the existing TX callback/metadata behavior.
- rpt-advanced alpha22: consume direct RX metadata through the canonical SDK type, keep raw-token handoff and speech admission separate, and prepare the owner-correct ring reserve. Extend the existing control-plane radio query for the resolved receive policy; host-services ABI10 rejects older callback semantics. No runtime parsing or allocation in callbacks.
- Only the radio library requires Rust >=1.87. Its scoped QA uses stable1.87 and the validated June2025 coverage nightly. USB/RPT retain their Rust1.85 minimum. Install real newer Debian compiler packages only inside the private native builder, never through fake package metadata or an MSRV override.

Update WISHLIST/ADR0004 to replace the planned Silero/ONNX choice with the user's Earshot choice. Preserve ADR0025/0026 ring ownership and source/burst association. All new ABI consumers, package bounds, tests and developer documentation move together.
