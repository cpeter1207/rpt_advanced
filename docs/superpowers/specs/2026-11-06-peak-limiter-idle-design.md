# Peak Limiter, mandatory output metering, and legacy idle integration

## Requirements

- P1: Rename final limiter to Peak Limiter.
- P2: Remove true peak meter from the stages menu. It should not be disableable and should follow the peak limiter.
- P3: Add the peak limiter to the local receive and link audio chains as well as the voice/telemetry chain.
- P4: Add idle integration to the inactive legacy Asterisk link hooks.

## Recorded implementation decisions

Both integrated loudness and true peak are mandatory after every chain, sharing the existing analyzer. Peak Limiter itself remains configurable.

Keep limiter defaults false/-3dBFS/4x/5ms/1ms/100ms; installed local/link/voice settings explicitly enable it with -2dBFS.

The shared FFmpeg Peak Limiter remains fixed after optional dynamics, software gain, and role-specific filtering/preemphasis. Its technical lookahead_limiter_* keys remain. The existing reorderable multiband limiter is a separate component. Remove loudness_meter_enabled rather than retain an alias.

Native RX, each link, and TX receive separate always-on integrated/true-peak analyzers. They observe final chain output and audible handoffs, exclude setup warmup, and retain epochs only when their complete path remains unchanged. Preserve identity processing for unconfigured links. Reuse OutputLoudnessMeter.new(rate,max_samples), which already supports 8..384 kHz.

Extend the existing sized UrpAstLinkObservation record and use the current observer operations. Require a matched SDK/package cohort; reject short output storage and verify returned sizes in consumers. No new FFmpeg/radio/audio provider ABI or copied DSP is needed. The root will add direction SILENCE=3 to the existing link_process signatures; it advances prepared graph state with zeros, discards graph output, meters silence, and clears caller scratch.

Legacy LinkHooks retain graph ownership under their existing audiohook mutex. A per-hook Asterisk timer/framehook clocks bounded silence when READ PCM supplies insufficient credit; WRITE frames supply none. Use at least 50 Hz and raise the rate when needed to keep each quantum within 960 samples. Fractional sample counts retain their remainder. All original frames and controls are returned unchanged. No new processing thread/hardware owner or PTT audio queue is introduced.

A stalled Asterisk channel reader coalesces expirations. Resume processes at most one current quantum; no exact elapsed aging claim across a stalled reader. Normal periodic idle gaps advance continuously.

A separate framehook data lease retains the existing LinkHook/module references until destroy_cb. Track current channel/id/FD slot. Masquerade ATTACHED(new), fixup, DETACHED(old) must preserve timer state and ignore superseded detach. Terminal detach removes FD registration before releasing the timer; asynchronous detach remains serviceable. Setup failures unwind all owned resources.

## Approaches considered

- Reuse the shared final limiter and analyzer in every role: selected; one implementation and fixed ordering.
- Duplicate limiter/meter logic in each controller: rejected; violates the shared graph boundary.
- Use a hardware clock or RPT-specific worker for legacy hooks: rejected; neither covers all hooks and ownership would change.
- Add per-hook worker threads or replay wall-clock gaps: rejected; adds scheduling/ownership or catch-up bursts.

## Acceptance and preservation

- Keep -23 LUFS, the 3-second rolling AGC window, existing gain bounds, 2/6 dB/s rates, and 500 ms hold.
- Native receive CPU saving must not prevent mandatory output metering or required AGC silence advancement; reuse existing bypass/processor boundaries without another hardware owner.
- Retire the application-level RX/TX CPU-saver controls and always clock the shared graphs. Retired meter and CPU-saver keys produce ordinary unknown-option diagnostics; remove them from deployed settings. The underlying shared radio API remains unchanged.
- Preserve reload/unload, all frame/control/PTT behavior, radio calibration, and artifact matching.
- Check all three actual processing graphs and GUI visibility, not only labels.
- Verify real Asterisk timer/framehook lifecycle and both 20.15.2 and target 22.10.1 API contracts.
- Node deployment continues under existing authorization with rollback and actual running-payload/config checks. No push, PR, or release is authorized.

## Interfaces, ownership and checks

### common_peak (inspect_usbradio)

ProcessingChain.peak_limiter: PeakLimiter; GraphDescriptionFactory.dynamics_at_rate(chain,rate)

- all three roles accept and emit peak controls
- actual negotiated-rate SRC return
- all gain/filtering precedes peak limiter
- independent final-output peak oracle

### tuner (inspect_hybrid)

Peak Limiter page and toggle for every role; no loudness_meter_enabled control

- all role page/toggle visibility
- multiband limiter remains distinct/reorderable
- no meter disable option
- shared-backend edit/reload

### mandatory_meter (root)

independent mandatory RX/link/TX OutputLoudnessMeter observers; extended UrpAstLinkObservation

- all roles including bypass and silence advance meter
- independent epochs/complete RX reload identity
- actual RX/TX adopted pointers
- old observation size/new provider fails safely
- new consumer rejects short old record

### legacy_idle (agc_detector)

per-hook Asterisk timer/framehook; existing link_process direction SILENCE=3; PreparedLink.advance_silence(count)

- 3s silence/rekey matches continuously zero-fed reference
- READ credit prevents duplicate ticks; WRITE supplies none
- fractional rates and <=960 samples/wake
- zero allocation/scratch on failure
- attach failure/unload/reload/async detach
- masquerade ATTACHED(new),fixup,DETACHED(old) retains ownership until destroy

### integration_install (root)

Matched integration and native installation

- real shared graph and descriptor integration
- fresh Debian13 amd64 focused/full applicable checks
- native Debian13 arm64 build/check/package
- matched14-package install and both configurations
- active role settings, diagnostics, mapped payload hashes, reload and runtime health

