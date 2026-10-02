# Priority link groups and translated telemetry

## Status

Design proposal for review. No implementation or node configuration change is
authorized by this document alone.

## Goals

- Configure a named, ordered group of permanent peers. Keep every reachable
  member connected, while selecting only the highest-priority reachable member
  for local transmit audio.
- Implement the scheduled-link warnings already specified by WISHLIST.md and
  ADR 0009.
- Move built-in controller-generated RF messages out of Rust source and into
  shipped, translatable message bundles with separate text, TTS, and Morse
  forms.
- Keep message preparation, link operations, configuration, and localization
  outside real-time audio workers.

## Non-goals

- Implement other wishlist items or change radio/audio processing.
- Translate arbitrary CLI, log, REST, or configuration diagnostics in this
  change. The catalog covers controller-generated telemetry presented over RF;
  the existing CLI/API contracts and internal logs remain English.
- Change user-authored identifiers, courtesy tones, announcements, or other
  custom media. Those remain configured as they are today.
- Add a Python production runtime. Python in this repository is test/tooling
  only and does not produce controller telemetry.
- Install or configure the feature on node 524950 as part of implementation.

## Architecture

### Message catalog

Use Project Fluent's Rust `fluent-bundle` with FTL resources. Ship an `en-US`
catalog and use stable message IDs in controller code. Each RF-capable catalog
entry has independent `.text`, `.tts`, and `.morse` attributes. The same
message arguments are passed to each variant, while each locale controls its
wording and ordering. Text is the canonical diagnostic/status rendering; TTS
and Morse use their respective attributes. If speech cannot be prepared, the
existing telemetry policy selects the localized Morse variant.

The catalog covers built-in connect/disconnect messages (including direct and
third-party wording), rejected-loop explanations, `*722` time replies, status
and command replies, and scheduled-link warnings. It also includes named-link-
group selection/unavailability messages. Stable IDs and argument names are
code-owned; message wording is bundle-owned. No built-in RF message wording
is embedded in Rust source. Unsolicited event messages and warnings go only to
the local transmitter; command replies retain ADR 0023's source-specific
routing.

For example, a catalog entry may express all three variants:

```ftl
peer-connected-local =
    .text = { $remote_node } connected
    .tts = Node { $remote_node } connected
    .morse = { $remote_node } CONNECTED
```

Localized values such as time remaining are formatted for the selected locale
before formatting the FTL message. This keeps locale-specific word order and
plural forms in the bundle without inventing a second template language.

### Locale selection and loading

- Default locale is `en-US`; `[general] language` selects the global locale,
  and a node section may override it with `language`.
- A node with no override inherits the global locale. Unsupported locale names
  resolve to `en-US` with a configuration warning.
- Package-owned catalogs live under
  `/usr/share/asterisk/rpt_advanced/messages/<locale>.ftl`.
- Administrator catalogs/overrides live under
  `/etc/asterisk/rpt_advanced/messages/<locale>.ftl` and take precedence over
  the packaged file for the same locale.
- The packaged English catalog is always required. A missing, malformed, or
  incomplete optional translation falls back per message attribute to the
  corresponding English value. Invalid English catalog data rejects runtime
  generation construction, preserving the currently active generation.
- Load and validate catalogs during configuration generation preparation, not
  from an audio callback. A successful reload swaps the complete catalog with
  the rest of that generation; failed reload leaves active messages unchanged.
- The existing configured speech voice remains authoritative. Selecting a
  locale does not download or choose a speech model; an operator must configure
  a compatible local voice separately. If that voice is unavailable, use the
  selected locale's Morse attribute.

### Formatting and validation

- Add a typed internal `MessageId`/argument context boundary so callers cannot
  pass arbitrary FTL identifiers or mismatched arguments silently.
- Validate required IDs and all three attributes in the shipped English
  catalog. Treat translation catalogs as optional overlays; invalid entries
  are skipped individually with a useful reload diagnostic and English
  fallback, rather than disabling unrelated valid translations.
- Formatting errors for a selected translation fall back to English for that
  attribute. Formatting errors in required English content fail candidate
  generation construction.
- Preserve the current bounded telemetry payload and existing queue behavior.
  Validate worst-case formatted size before scheduling or accepting a
  configured warning. Do not perform FTL parsing, filesystem access, or
  allocation on the audio path.
- Retain the existing `${...}` custom-template parser only for existing
  user-authored macro/event templates that are outside the new built-in RF
  catalog. Scheduled-link warning content is bundle-owned and selected by a
  per-event `warning_message_id`; `${time_remaining}` becomes the typed
  `time_remaining` argument supplied to Fluent.

### Named priority link group

Use the existing `[permanent node label]` declaration with an ordered
`fallback_nodes` list. `remote_node` is priority 1; entries in
`fallback_nodes` follow in priority order. The complete group stays configured
as a permanent route. Add a display `group_name`; for the requested node:

```ini
[permanent 524950 main]
remote_node = 506315
fallback_nodes = 506312,506310,506311,506313,506314
group_name = The Blind Hams Network
```

Connect every group member independently using normal link admission and
automatic permanent-link recovery. An unavailable member is retried in the
background using existing backoff, without per-attempt RF telemetry. A
schedule replacing this permanent route suppresses the entire group, including
its retries, until that scheduled replacement ends.

This uses ASL3's documented monitor-only and transceive link modes: monitor
means receive without sending audio to that peer, while transceive enables
bidirectional audio. Those documents do not define automatic priority groups
or an atomic no-reconnect handoff, and the official open-issue search found no
proposal for those behaviors. Therefore the group-selection policy is owned by
`rpt_advanced`; it reuses the documented modes, not an upstream failover
design.

Every member is receive-only unless selected. Select the first reachable member
in configured order as the sole transceive member; lower-priority members remain
connected receive-only. Audio from unselected group members is never mixed to
the local transmitter. Recompute priority when a peer connects or disconnects.
Continue consuming each connected member's inbound media while it is
unselected, so its receive path stays current and can be promoted without
repriming or replaying buffered backlog. Topology-loop rejection remains
subject to ADR 0017's topology-blocked recovery; it is retried when relevant
topology evidence changes, not in a rapid loop.
Do not switch away from a selected source during an active transmission.
Publish a changed winner only when the current transmission has ended and at
an audio-callback boundary. If the selected member disappears, promote the
next reachable member at the next safe callback boundary. Store the selected
member as one atomic group selection, read once per audio callback; do not
independently toggle peer mode flags where an intermediate state could make
two members transceive or none selected. Thus an ordinary priority handoff
has no dial, disconnect, or buffer reprime. The shared topology admission
rules remain in force for every member.

Group event telemetry uses `group_name`, not a sequence of member-level
connect/disconnect announcements: announce which group member is selected
when the winner changes, using the group name and selected node; announce the
group unavailable only when no member is reachable. Background retry attempts
and standby member connection changes are silent. Local transmitter routing
and localization follow the message-catalog rules above.

### Scheduled-link warnings

Extend existing schedule entries with optional comma-separated positive
millisecond lists `warning_before_start_ms` and `warning_before_end_ms`, plus a
`warning_message_id` selecting the FTL message. No warning is enabled when
these settings are absent. Keep ADR 0009 semantics:

- Multiple configured leads are allowed before start and before end/disconnect.
- For inactivity-ended schedules, leads are measured from the expected
  inactivity deadline and reset when qualifying receiver/link activity resets
  that deadline.
- Skip a due warning while local-receiver or linked-peer activity is present;
  do not defer it. After activity resets an inactivity deadline, warnings from
  the prior quiet period may be sent again if due in the new period.
- Skip any warning due at or after its related start, end, or inactivity
  deadline.
- Warning telemetry is serialized with other telemetry, routed through the
  established policy, and never interrupts an active source.
- The event's message ID is configurable in the schedule; translated wording
  and TTS/Morse forms are provided by the selected bundle.
- Format the `time_remaining` argument as a localized natural-language
  duration through a catalog duration message, then pass that result to the
  event warning message. Round upward to whole seconds so a warning never
  understates its remaining time.

## Configuration shape

```ini
[general]
language = en-US

[524950]
language = en-US

[permanent 524950 main]
remote_node = 506315
fallback_nodes = 506312,506310,506311,506313,506314
group_name = The Blind Hams Network

[schedule 524950 weekday-net]
warning_before_start_ms = 3600000,1800000,900000,600000,300000,60000
warning_before_end_ms = 600000,300000,60000
warning_message_id = scheduled-link-change
```

The millisecond representation matches the existing schedule's integer
duration fields and makes the list unambiguous.

## Compatibility and dependencies

- A permanent route without `fallback_nodes` remains a one-member group. A
  group with more members connects all of them and applies priority selection.
- Existing inline custom event templates keep their current syntax and
  behavior; only built-in controller-generated RF messages and selected
  scheduled warning messages use Fluent.
- Add `fluent-bundle` as a Rust dependency. Do not add Python i18n runtime
  dependencies because no Python production message path exists. If Python
  tooling later validates/edits FTL, use Project Fluent's Python packages
  rather than a parallel message format.
- The feature does not alter the telemetry ring ABI or native audio ABI.

## Verification

- Group tests: all members connect independently; each unreachable member
  retries while other members continue; priority winner selection; a better
  priority appears during a keyed transmission and takes over only at the next
  safe callback boundary; selected peer disconnect promotes the next reachable
  member; no callback observes two transceive members; standby audio never
  reaches the local transmitter; schedule suppression/re-enable and reload
  reject stale work; duplicate/self members rejected.
- Schedule warning tests: lead-time ordering; start/end; inactivity reset and
  warning eligibility; skip while active; skip at/after deadline; message
  argument reflects the expected remaining interval; serialization prevents
  concurrent telemetry.
- Localization tests: English completeness; per-attribute translation
  fallback; malformed translated entry isolation; malformed English candidate
  rejection; locale override/inheritance; exact message arguments; output
  bounds; Morse fallback if TTS cannot be prepared.
- No tests should require attached RF hardware. Add a node procedure for
  checking priority recovery/handoff and hearing each warning in
  English, then in one translated bundle.

## Open implementation details

The implementation plan must verify how a callback-safe atomic group selection
can drive the current `LinkAudio` path, how `LinkManager` exposes peer mode in
status, how configured warning duration values are parsed, and the complete
inventory of controller-generated RF strings and their arguments. It must not
widen the work into translation of diagnostics or other wishlist capabilities.

## References

- [ADR 0009: Scheduled actions and macros](../../../doc/architecture/decisions/0009-scheduled-actions-and-macros.md)
- [ADR 0010: Permanent links and replacement windows](../../../doc/architecture/decisions/0010-configured-permanent-links-and-replacement-windows.md)
- [ADR 0017: Scheduler route lifecycle and civil time](../../../doc/architecture/decisions/0017-scheduler-route-lifecycle-and-civil-time.md)
- [ADR 0023: Unified control and DTMF policy](../../../doc/architecture/decisions/0023-unified-control-and-dtmf-policy.md)
- [ASL3 standard link commands](https://allstarlink.github.io/basics/standardcommands/)
- [ASL3 `rpt.conf` link modes](https://allstarlink.github.io/config/rpt_conf/)
- [AllStarLink `app_rpt` open issues](https://github.com/AllStarLink/app_rpt/issues)
- [Project Fluent Rust crates](https://docs.rs/fluent/latest/fluent/)
- [Project Fluent source and language implementations](https://github.com/projectfluent/fluent)
