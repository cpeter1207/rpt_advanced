# DTMF pad test and ASL-compatible regeneration

Date: 2026-10-15
Status: Implemented locally 2026-10-16. Hosted release and live-radio validation pending.

## Outcome

Complete the local RF pad test, reusable DTMF PCM generation, and ASL3 remote
command forwarding. Regenerated tones go only to the local transmitter.
Keep the shared controller behavior identical in standalone and the Asterisk
adapter. Do not change USBRadioPlus, raw-audio muting, filters, link audio,
hardware settings, or unrelated wishlist features. This spec does not
authorize deployment, release, or calls to external nodes.

The owner approved the pad-test variation and fixed-duration regeneration,
including configurable tone duration, gap, and level. The 2026-10-15 decision
replaces peer PCM regeneration and broadcasting `*4` destination-entry digits
with ASL3 behavior. ADR 0023 and the wishlist record that correction.

## Behavior

### Pad test

- Inherit `link_command_pad_test` from `[general]` with a node override, using
  the existing command-map convention. Default `82` means public command
  `*82`; it needs no administrative unlock.
- Only the local RF source starts capture. After recognition, every DTMF
  symbol belongs to that capture, including `*`, `#`, and `A` through `D`.
  Captured symbols cannot execute commands or enter remote-command mode.
- Actual local receiver unkey alone ends capture. A long pause, a command
  timeout, or the `#` symbol does not end it. Retain the first 127 symbols;
  discard subsequent symbols and report overflow before the retained digits.
- After unkey, queue one serialized local-transmitter response through the
  existing station-telemetry producer and PCM ring. Use the node's default
  speech configuration and Morse fallback. Read symbols individually, say
  "star" and "pound", and report an empty capture as "no digits".
- All built-in words have Fluent text, TTS, and Morse forms. Compose bounded
  symbol fragments outside audio rather than exceeding the catalog's current
  per-message length limit with a 127-symbol expansion.
- Reload, source loss, or digit-event loss discards an incomplete capture;
  do not announce a guessed or truncated sequence as an accurate pad test.
  Deliberate capture overflow is different from a lost event.

### Regeneration

Use the current ASL3 `propagate_dtmf` behavior for normal AllStarLink nodes:
eligible received link digits addressed to this node may be regenerated on
its local transmitter. Transit digits are not regenerated. Function-start
and collected command digits are not regenerated. Do not add automatic
replay of ordinary local RF digits or a PCM-regeneration path to peers.

Proposed configuration follows the existing `[general]` / `[node]` convention:

| Setting | Default | Meaning |
| --- | --- | --- |
| `dtmf_regeneration_enabled` | `no` | Enable eligible local-transmitter regeneration |
| `dtmf_regeneration_duration_ms` | `200` | Fixed duration of each regenerated digit |
| `dtmf_regeneration_gap_ms` | `50` | Silence between queued digits |
| `dtmf_regeneration_level_dbfs` | `-12` | Combined peak of the equal-amplitude row/column tones |

The duration is not the received key-hold duration. An idle digit queue waits
500 ms before its first tone, matching ASL3's initial queue delay. Unlike a
status response, regeneration does not wait for the sending source to unkey.
It remains subject to existing half-duplex, transmitter timeout, and RF safety
inhibits. Tone level is specified at the generated-PCM boundary; existing
transmitter processing and hardware gains still apply.
Like ASL's non-interruptible local generator, the regenerated tone and its
interdigit silence replace ordinary local voice-band output for their sample
span. They do not mix with voice, alter peer PCM, or change receive detector
muting. Existing media continues to advance; do not accumulate delayed voice
or restart interrupted announcement samples after the digit sequence.

Duration and gap use positive integer milliseconds, bounded to 1,000 ms each;
level uses integer dBFS from -60 through 0, matching the existing tone
component. Invalid values warn and inherit a valid default under ADR 0019.
Command-map collisions remain errors under ADR 0023.

### Remote commands

- Recognize the remappable `*4` prefix and resolve its destination using the
  existing directory. Keep prefix and destination digits local. Selection
  completes when the entered destination is valid, as in ASL3.
- Subsequent digits use `D <destination> <source> <sequence> <digit>` text.
  Send directly when the destination is adjacent; otherwise relay the
  addressed message through eligible connected links. Do not synthesize
  peer PCM or send an additional IAX DTMF frame for the same command digit.
- On transit, preserve the envelope and never execute or regenerate it.
  Prefer a direct destination; otherwise forward to eligible links excluding
  ingress and the originating source. Only the destination consumes the digit.
- An actual `#` exits selection locally without forwarding it. RF unkey and
  ordinary collection timeout do not exit selected remote-command mode.
  Ordinary commands retain their current unkey termination and muting rules.
- Preserve current inbound authorization, source-isolated command/admin state,
  and originating-connection reply routing. Relayed source text is not proof
  of identity and cannot transfer another source's administrative unlock.
- Retain ASL sequence numbers through relays. Do not promise exactly-once
  delivery or add a new wire protocol, acknowledgement, or replay mechanism.
  Existing topology prevention and bounded transport queues remain in force.

## Implementation boundaries

### Shared DTMF component

Add generation to `librptadvdtmf`, alongside its existing detector. Reuse its
sixteen-symbol frequency mapping and dynamically call `librptadvtone` for the
dual-tone waveform. Do not duplicate oscillators in the controller or add a
new DSP dependency. Render canonical mono F32 at 48 kHz incrementally; sample
counts and phase must be independent of render-block partitioning.

Use the existing descriptor-size extension convention for opaque generator
creation, rendering, and destruction. Keep the existing detector prefix and
ABI/SONAME 1 intact if the additive tail is sufficient. New consumers require
the extended descriptor before enabling generation; an older provider cannot
be mistaken for one supplying the new functions. Update development/runtime
dependencies and staged package checks with the new provider requirement.

### Controller and media owners

Use a distinct local-unkey event rather than synthesizing `#`. This serves
both the approved pad test and ASL remote-mode persistence while preserving
ordinary command completion. Keep parser, authorization, and routing policy
on station control; the radio detector publishes only bounded copied events.

Regeneration is local radio-control signaling, not a deferred status reply.
Give it a bounded local-only PCM lane so speech preparation or active receive
cannot impose the status queue's wait-for-unkey rule. Reuse the existing
off-audio per-node fanout worker (`product/link/dispatcher.rs`) to service
generation, not the serial speech/file worker. Do not add a thread per digit
or perform synthesis in an audio callback. A bounded generation-owned
digit queue connects station control to that sole producer. It renders through
the shared DTMF/tone components into a shared PCM ring; the transmit worker
only consumes the resulting PCM and selects it for the local RF output.

This lane is excluded from the program-audio loopback, every peer's mix-minus
output, and parrot capture. It does not inherit speech ducking or speech gain.
Use existing transmitter safety policy for PTT demand and existing ordinary
telemetry serialization unchanged. Allocate workspaces, generator state, and
ring storage off audio. Queue overflow discards new pending digits and is
observable; no producer blocks a radio callback or peer fanout. Bound each
media-worker turn to one native-sized generation chunk alongside bounded
fanout, skip writes on ring backpressure, preserve partial progress, and
retire all work with its
generation under ADR 0026. Do not use PLC to extend a signaling tone after its
producer finishes or fails.

Both Asterisk and standalone transports deliver validated text envelopes to
the shared routing policy. They retain socket/channel I/O only. Extend the
existing text-send path for transit; the IAX protocol library needs no new
packet format. A disconnected/replaced source cannot receive stale replies
or leave its command state attached to a new connection.

## Alternatives not selected

- Per-peer PCM regeneration contradicts the approved local-only routing.
- Generating tones in the output callback contradicts off-audio synthesis.
- Putting regeneration in the ordinary status queue delays it until unkey.
- A new oscillator or per-digit worker duplicates existing shared components.

## Verification and completion

Use test-driven development and targeted, table-driven checks first:

1. All sixteen generated frequency pairs, combined level, exact tone/gap
   sample counts, silence, repeated digits, and partition-independent output.
2. Defaults, global/node inheritance, invalid-value warnings, disabled mode,
   and command-prefix collisions.
3. Empty, mixed-symbol, 127/128-symbol, long-pause, actual-unkey, literal-`#`,
   event-loss, reload, and speech-fallback pad tests. No captured command runs.
4. Local-only regeneration during peer activity; command suppression; bounded
   overflow; clean tone termination; no tone in peer PCM or parrot capture;
   no speech ducking; transmitter inhibition and generation retirement.
5. Direct and transit `D` routing, no destination-entry forwarding, preserved
   envelopes, no ingress reflection, addressed-only execution, remote-mode
   persistence across unkey, actual-`#` exit, and normal command termination.
6. Authorization and reply identity across concurrent origins, disconnect,
   reload, and reconnect. Exercise both host transports through shared policy.
7. Dynamic provider loading, descriptor-size checks, exported symbols,
   package dependencies, and install/build behavior.

Keep focused evidence in ignored `.work/`. Run formatting, lint, and static
analysis in freshly pulled native quality images before a push; the full PR
gate supplies Debian 13 amd64/arm64 tests and amd64 production coverage.
Do not claim completion from a design review. Remove the implemented DTMF
wishlist requirements only after verified implementation; leave unrelated
telephony work. Update source developer documentation before implementation
commits and operator documentation before a PR, following repository rules.

## References

- [ADR 0023: control and DTMF policy](../../../doc/architecture/decisions/0023-unified-control-and-dtmf-policy.md)
- [ADR 0011: shared functional components](../../../doc/architecture/decisions/0011-versioned-functional-components.md)
- [ADR 0025: native media ownership](../../../doc/architecture/decisions/0025-native-media-routing-and-pcm-ring-ownership.md)
- [ADR 0026: generation lifecycle](../../../doc/architecture/decisions/0026-generational-real-time-runtime-lifecycle.md)
- [ADR 0041: Fluent catalogs](../../../doc/architecture/decisions/0041-fluent-localized-controller-telemetry.md)
- [ASL3 propagation setting](https://allstarlink.github.io/config/rpt_conf/#propagate_dtmf)
- [ASL3 command handling](https://github.com/AllStarLink/app_rpt/blob/22b60b5de6a7d99e3042f9b413d129dee3b97ad0/apps/app_rpt.c)
- [ASL3 addressed forwarding](https://github.com/AllStarLink/app_rpt/blob/22b60b5de6a7d99e3042f9b413d129dee3b97ad0/apps/app_rpt/rpt_link.c)
- [ASL3 local tone queue](https://github.com/AllStarLink/app_rpt/blob/22b60b5de6a7d99e3042f9b413d129dee3b97ad0/apps/app_rpt/rpt_channel.c)
- [ASL3 tone definitions and pad test](https://github.com/AllStarLink/app_rpt/blob/22b60b5de6a7d99e3042f9b413d129dee3b97ad0/apps/app_rpt/rpt_functions.c)
- [Asterisk generator/output arbitration](https://github.com/asterisk/asterisk/blob/22/main/channel.c)
- [Asterisk non-interruptible tone playback](https://github.com/asterisk/asterisk/blob/22/main/indications.c)
