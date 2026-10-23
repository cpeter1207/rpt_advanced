# Clock-scheduled messages and optional Morse fallback

Status: Implemented and validated on 2026-10-23, including the approved playback
and cancel controls. The [full hosted gate](https://github.com/cpeter1207/rpt_advanced/actions/runs/40021040672)
passed at `eb7fead05370a4a2e0b639d95515987add071a04` on native Debian 13 amd64
and arm64, with 100% production line and branch coverage on amd64. Live node
acceptance is a separate deployment step; this feature is not installed.

## Intent

Play configured messages at a local date/time, daily, on selected weekdays,
or at recurring clock boundaries. Reuse the existing scheduler, message
compositions, tail-message preparation, telemetry producer, and PCM ring.
Do not add a second scheduler, audio engine, or network-media source.

## Requirements and agreed decisions

- Scheduled occurrences become pending at their configured clock time, not
  relative to startup, the preceding playback, or successful completion.
- Support one local date/time, daily time, selected weekdays, and stepped
  minute/hour schedules. Calendar precision remains one minute.
- Tail and scheduled messages support sound files, TTS, Morse, and ordered
  combinations using existing content and message-macro composition.
- Preserve existing substitutions, catalog localization, references, media
  settings, and per-node/global inheritance. File/TTS/Morse fields in one
  media segment remain fallback alternatives; multiple ordered segments
  express sequential playback.
- Scheduled messages wait for all local-receiver and connected-peer activity
  to end before primary playback. They remain serialized with other telemetry.
- Each scheduled message has a maximum polite wait and an expiry action:
  `skip` or `morse`. The user explicitly selected configurable expiry behavior.
- If Morse expiry meets an occupied telemetry slot, each message selects
  `skip` or `wait`, defaulting to `skip`. Never play two messages simultaneously.
- New receive activity during either a tail or scheduled message immediately
  cancels its primary stream and selects its complete configured Morse form.
  If that form is unavailable, stop the message. Never continue a sound file
  or TTS through receive, or play only a partial Morse substitute.
- The traffic transmit-timeout timer must not suppress or truncate a tail or
  scheduled message. Hardware interlocks, site inhibition, half-duplex rules,
  and device faults remain effective.

## Added playback controls — 2026-10-23

- Every configured message can optionally expose a per-message DTMF selector,
  including IDs, tails, and scheduled messages. Reuse the current `*77` message
  command family and its remappable per-node prefix rather than a second parser.
  Selectors share one per-node namespace and conflicting assignments are rejected.
- Manual invocation requests that configured content independently of its
  automatic calendar/traffic trigger. Admission: use the same bounded
  message queue and per-message polite/expiry policy, not immediate preemption;
  it never permits overlapping playback or bypasses safety. A rejected full
  queue does not mark the source played. Selecting an event plays its media only,
  never its action macro or link operations.
- Completion policy: successful manual ID playback satisfies that
  configured ID through the existing ID-priority rules; successful manual tail
  playback advances that tail's normal cooldown. Manually playing scheduled
  content does not consume a future calendar occurrence. Cancellation/failure
  is not success and may not immediately restart the same cancelled attempt.
- A per-message repeat setting can suppress local-receiver and linked-peer
  program audio during playback. Continue capturing, draining inbound rings,
  detecting DTMF, and handling commands; suppress repeat only after these steps.
  Do not disable the receiver, disconnect peers, or accumulate delayed traffic.
- A per-message destination selects local RF only or local RF plus connected
  links. Default remains local only. This explicitly extends the earlier
  local-only ID/tail rule, without changing ordinary source-scoped status replies.
- Names and defaults for the common message fields: optional
  `dtmf_id` (the existing decimal selector), `repeat_during_playback = yes`,
  and `playback_destination = local | local_and_links` (default `local`).
  Expose these on configured message, ID, tail, and scheduled-media entries.
- The user confirmed repeat suppression does NOT bypass the receive-interruption
  rule: new local or linked receive still selects configured complete Morse,
  or stops the message when none exists. DTMF remains decoded before the repeat
  gate, even when repeat audio itself is suppressed.
- Add a remappable DTMF operation to cancel the currently playing message and
  restore normal repeat operation. This is an explicit abuse-recovery control:
  a very long uploaded bulletin cannot require waiting for its natural end.
  Default: `*78`, available to local RF without an administrative unlock.
- Cancellation ends the entire active message attempt: all segments, pending
  Morse replacement and unread producer/ring PCM. It must not resume or restart
  that cancelled attempt. It restores this message's repeat gate at the next
  output callback; device-submitted PCM and ordinary hardware latency remain.
  Producer cancellation/reclamation stays off audio. Existing receive/DTMF
  qualification, site inhibition, interlocks, and traffic timeout stay effective.
- Cancellation scope: the current attempt only, not unrelated future
  schedules or configuration. Do not advance successful ID/tail completion for
  a cancelled message. Other pending messages retain their normal serialized
  admission policy; none may overlap the cancelled stream.
- Routing detail: local-plus-links uses the normal eligible outbound
  peers, never overriding receive-only or inactive fallback link modes.

## Approaches

Use the existing event scheduler to admit the same compiled message recipes
used by tails. This follows the user's explicit reuse requirement. Add only
the missing clock selectors and per-occurrence delivery policy.

Adding calendar rules to release-triggered tails would conflate traffic-release
timing with civil time. A separate scheduled-playback worker would duplicate
ordering, generation, substitution, and media ownership. Neither is needed.

## Configuration

Extend existing `[event node name]` entries with the shared composed-message
settings already accepted by tails: `content`, `message_id`, `arg_*`, sound,
speech, Morse, and tone settings. A `content` reference can select an existing
`[message name]` or per-node message macro. Keep current event `message`,
`template`, and typed action-macro behavior unchanged when these new options
are not selected. Conflicting message forms warn and resolve deterministically
using the normal configuration rules.

Retain current `at = once YYYY-MM-DD HH:MM`, `daily HH:MM`, and
`weekly weekday HH:MM`. Add `every N minutes` and `every N hours`, and allow
an optional `days` selector using the existing weekday names, lists, and ranges.
Minute steps select 0, N, 2N, ... within each hour; hour steps select
0, N, 2N, ... within each day, at minute zero. This is cron-like field stepping,
not elapsed-time repetition. Accept singular and plural units, with N from
1 through 60 minutes or 1 through 24 hours. Thus `every 60 minutes` and
`every 1 hour` both select each hour at minute zero.

Per-entry settings, inheriting event defaults where applicable:

- `polite_maximum_wait_ms`: nonnegative start-wait budget from the occurrence's
  scheduled time. Default: 60000 ms. Zero permits immediate playback
  only if eligible; otherwise apply the expiry action immediately.
- `polite_expiry = skip | morse`: default `skip`.
- `polite_expiry_busy = skip | wait`: default `skip`, selected by the user.
  This applies when `polite_expiry = morse` has expired but another telemetry
  item owns playback. `wait` retains the selected Morse form until the slot is
  free; it does not interrupt or overlap the current message.

For example, using an existing configured message macro:

```ini
[event 524950 hourly_time]
at = every 1 hour
content = {message_macro_time}
polite_maximum_wait_ms = 60000
polite_expiry = skip
polite_expiry_busy = skip

[event 524950 weekday_weather]
at = daily 08:00
days = Monday-Friday
sound_file = /var/lib/rpt-advanced/messages/weather.wav
speech_text = The weather report is unavailable.
polite_maximum_wait_ms = 120000
polite_expiry = skip
```

The referenced macro/file must exist. The weather example deliberately has no
Morse component: receive interruption stops it. Media gains continue to inherit
configured speech/Morse and message settings; no hardcoded level is added.

## Occurrence and delivery policy

- Use existing host-local time, civil-minute identity, weekday parsing, DST
  deduplication, wall-clock correction, reload identity, and no-catch-up rules
  from ADR 0017. Do not implement catch-up after downtime or durable delivery.
- A delayed control tick must not grant a fresh polite-wait budget. Translate
  the scheduled occurrence's remaining wait to a monotonic deadline once.
- A deadline is a start deadline, not a playback-duration cap. Once playback
  begins, do not cut it off because its polite-wait budget expires.
- The explicit `polite_expiry_busy = wait` exception permits an expired,
  Morse-selected occurrence to wait for serialized playback. Its original
  deadline and Morse selection remain latched; waiting never restarts its
  polite budget or permits primary speech/file playback after expiry.
- On expiry, skip the occurrence or select its Morse form. If no complete
  Morse form exists, skip. The Morse choice never overrides half-duplex or
  safety restrictions; if these prevent starting at expiry, skip this occurrence.
- Keep at most one unstarted occurrence per configured scheduled message. If a
  newer occurrence becomes due, replace the older pending occurrence rather
  than accumulate stale time/weather reports. Do not overlap an already playing
  occurrence with its successor; retain at most the latest pending successor.
- Value policy: substitutions use actual playback-attempt time, not
  the old scheduled minute. Resolve one coherent snapshot for the whole
  attempt; an interruption retains that snapshot. A pre-output deferral refreshes
  values on readmission, as the current message pipeline does.
- Scheduled, tail and manually selected message media follow their configured
  local-only/local-plus-links destination. IDs default to local only but may
  explicitly select local-plus-links under the added user requirement above.
  Ordinary command-status response routing remains source-scoped and unchanged.
- A skipped or interrupted scheduled occurrence is consumed, not retried every
  control tick. A later recurrence is independent. Tail completion/cooldown and
  release opportunities retain their existing rules.
- Keep current action macros at-most-once and independent of RF completion.
  Message expiry cancels message delivery, not an already-dispatched action.

## Media and timeout ownership

Relax the current mandatory-Morse check for tail and scheduled media. Preserve
ID and other existing required-Morse contracts. Explicit/inherited media Morse,
catalog `.morse` forms, and existing literal message-macro text provide Morse
segments; never invent a transcript for file or TTS-only media lacking one.
A composition lacking any required spoken-content substitute has no complete
Morse alternative; decorative tones may still be omitted under the existing rule.

Reuse `MessageRecipes`, the generation-safe preparation handoff, `MediaProgram`,
and producer-side Morse switching. Cancel unread primary PCM and converter
history at interruption; no callback decoding, synthesis, allocation, or logging.

Make timeout exemption explicit for tail and scheduled playback in both PCM
admission and PTT demand. Continue to suppress timed-out traffic: playing an
exempt message must not allow a continuously keyed receiver or peer to bypass
its traffic timeout. Leave other timeout behavior unchanged. Do not recursively
arm tails from message-generated transmitter releases.

Reuse the released schedule provider's current primitives where possible.
Prefer compiling selected minutes/hours/weekdays into its existing records over
changing its ABI. Scheduling and message policy remain on station control;
rendering remains on the existing serialized telemetry producer.

## Validation and scope

Use focused failing tests before implementation: clock boundaries and selected
days, per-node inheritance, date/DST/clock-step/reload behavior, queue pressure,
deadline expiry/zero wait, replacement of stale pending occurrences, fresh
substitutions, all configured media combinations, interruption without Morse,
complete versus partial fallback, timeout crossing/lockout with traffic still
muted, ID priority, and safe PTT release. Preserve existing tail timing and
ordinary event/action tests.

Update ADRs 0008, 0009, 0017, 0023, and 0025 plus the affected architecture and wishlist
status. The new decision supersedes the earlier tail spec's mandatory Morse and
timeout precedence only for tail/scheduled messages. Developer documentation
changes with implementation; operating docs are updated before a PR.

No node installation, restart, link/configuration change, release, REST API,
over-air recording, or network audio-stream support is included by this design.
The independent authorized passive disconnect monitor completed at 12:30.
