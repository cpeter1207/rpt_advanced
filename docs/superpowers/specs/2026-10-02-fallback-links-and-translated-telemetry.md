# Ordered fallback links and translated telemetry

## Status

Design proposal for review. No implementation or node configuration change is
authorized by this document alone.

## Goals

- Configure an ordered fallback list for a permanent peer and recover to the
  primary without ever connecting the primary and fallback simultaneously.
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
and command replies, and scheduled-link warnings. Stable IDs and argument
names are code-owned; message wording is bundle-owned. No built-in RF message
wording is embedded in Rust source. Unsolicited event messages and warnings go
only to the local transmitter; command replies retain ADR 0023's
source-specific routing.

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

### Ordered permanent-link fallback

Use the existing `[permanent node label]` route and `fallback_nodes` field from
ADR 0010. The example for the requested node is:

```ini
[permanent 524950 main]
remote_node = 506315
fallback_nodes = 506312,506310,506311,506313,506314
```

The runtime tries the primary first. Only when its normal connection attempt
cannot establish/re-establish the primary does it try fallback nodes, one at a
time and in listed order. A fallback is considered active only after normal
link admission succeeds. The route owns at most one peer at a time. When the
primary becomes available, disconnect the active fallback, then reconnect the
primary; never overlap them. Existing retry/backoff and explicit disconnect
semantics remain authoritative. Reload invalidates stale route work using the
existing route generation/reservation mechanism. Duplicate/self/invalid peer
entries are configuration errors consistent with ADR 0010. A schedule that
replaces this permanent route suppresses the entire route, including its
fallbacks; fallback policy applies only when the permanent route is desired.

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

[node 524950]
language = en-US

[permanent 524950 main]
remote_node = 506315
fallback_nodes = 506312,506310,506311,506313,506314

[schedule 524950 weekday-net]
warning_before_start_ms = 3600000,1800000,900000,600000,300000,60000
warning_before_end_ms = 600000,300000,60000
warning_message_id = scheduled-link-change
```

The millisecond representation matches the existing schedule's integer
duration fields and makes the list unambiguous.

## Compatibility and dependencies

- This is a new alpha feature; no migration from older alpha config syntax is
  required. Existing permanent routes without `fallback_nodes` continue to
  behave as single-primary routes.
- Existing inline custom event templates keep their current syntax and
  behavior; only built-in controller-generated RF messages and selected
  scheduled warning messages use Fluent.
- Add `fluent-bundle` as a Rust dependency. Do not add Python i18n runtime
  dependencies because no Python production message path exists. If Python
  tooling later validates/edits FTL, use Project Fluent's Python packages
  rather than a parallel message format.
- The feature does not alter the telemetry ring ABI or native audio ABI.

## Verification

- Fallback route tests: primary success; ordered fallback attempts; primary
  failure then fallback success; all fallbacks fail; fallback drops then
  retries; recovered primary disconnects fallback before reconnect; reload
  rejects stale work; no primary/fallback overlap; duplicate/self entries
  rejected.
- Schedule warning tests: lead-time ordering; start/end; inactivity reset and
  warning eligibility; skip while active; skip at/after deadline; message
  argument reflects the expected remaining interval; serialization prevents
  concurrent telemetry.
- Localization tests: English completeness; per-attribute translation
  fallback; malformed translated entry isolation; malformed English candidate
  rejection; locale override/inheritance; exact message arguments; output
  bounds; Morse fallback if TTS cannot be prepared.
- No tests should require attached RF hardware. Add a node procedure for
  checking primary recovery/fallback switching and hearing each warning in
  English, then in one translated bundle.

## Open implementation details

The implementation plan must verify how permanent route retry state currently
represents “primary unavailable,” how configured warning duration values are
parsed, and the complete inventory of controller-generated RF strings and
their arguments. It must not widen the work into translation of diagnostics or
other wishlist capabilities.

## References

- [ADR 0009: Scheduled actions and macros](../../../doc/architecture/decisions/0009-scheduled-actions-and-macros.md)
- [ADR 0010: Permanent links and replacement windows](../../../doc/architecture/decisions/0010-configured-permanent-links-and-replacement-windows.md)
- [ADR 0017: Scheduler route lifecycle and civil time](../../../doc/architecture/decisions/0017-scheduler-route-lifecycle-and-civil-time.md)
- [ADR 0023: Unified control and DTMF policy](../../../doc/architecture/decisions/0023-unified-control-and-dtmf-policy.md)
- [Project Fluent Rust crates](https://docs.rs/fluent/latest/fluent/)
- [Project Fluent source and language implementations](https://github.com/projectfluent/fluent)
