# Configuration model

Configuration reading, storage, whole-file validation, node/media-set discovery, and
settings resolution are implemented. The AllStarLink link controller is also
implemented locally, but it has not been enabled on a live node and classic
`app_rpt` interoperability remains to be verified. These are the supported
settings.

The installed example is `share/doc/rpt_advanced/examples/rpt_advanced.conf`
under the installation prefix. Its example node is disabled and its ID text is
empty. Installation does not replace an active configuration or activate a node.

Semicolons introduce comments. Blank lines are ignored. Options require a
preceding section. Whitespace around names and values is stripped; a final
newline is optional. Embedded null bytes and malformed section/option lines
are errors. The reader reports the physical line number and imposes no fixed
line-length limit. File read errors reject loading rather than accepting a
partial file.

Flat `[general]` settings provide node defaults. A named node section such as
`[524950]` overrides them. Identifier resolution starts with `[identifier]`,
then `[identifier 524950]`; announcement resolution likewise starts with
`[announcement]`, then `[announcement 524950]`; courtesy resolution starts
with `[courtesy]`, then `[courtesy 524950]`. The `speech_*` and `morse_*`
names are valid in either kind of media-default section and in its named set,
except `speech_level_db` and `morse_level_db`: courtesy uses its single
`level_db` control for every media type.
After those media-default sections, `[speech]` then `[speech 524950]` apply
their matching speech values, and `[morse]` then `[morse 524950]` apply their
matching Morse values. Finally, `[identifier 524950 welcome]` or
`[announcement 524950 release]` overrides every matching setting for that one
set. File order does not change this scope precedence; later occurrences of
the same option in one section win. Empty media paths or text clear inherited
values.

Named templates and macros use `[template label]` and `[macro label]` globally;
`[template node label]` and `[macro node label]` override same-label global
settings for that node. Zero-time events are node-scoped: `[event node label]`.
Template, macro, and event labels are case-sensitive single tokens and cannot
contain whitespace or square brackets.

Node names are case-sensitive, limited to 63 bytes, and cannot contain whitespace or square
brackets. Media-set names are case-sensitive and cannot contain whitespace or square brackets.
The same 63-byte limit applies to decimal remote-node identities, so configured names always fit
the direct-peer and scheduler transports without truncation. Scoped headers use
one space between components. `general`,
`identifier`, `announcement`, `courtesy`, `speech`, `morse`, `time`, `template`,
`macro`, and `event` are reserved section names. Scoped identifier, announcement,
courtesy, speech, Morse, time, template, macro, and event headers must name an existing
node where their syntax includes a node name, which may be declared later in the file.
Repeated ordinary section headers merge options without creating duplicate nodes or media
sets. A repeated named template, macro, or event header is instead rejected as a duplicate
definition. Unknown options and invalid values are rejected even if a later entry would
override them. There is no fixed limit on the number of nodes, identifiers, announcements,
courtesy tones, templates, macros, or events.

## Node settings

| Option | Default | Meaning |
| --- | --- | --- |
| `node_enabled` | yes | Start the configured node. |
| `full_duplex` | yes | Allow simultaneous reception and transmission. |
| `dtmf_muting` | yes | Silence a local received PCM frame when an in-band DTMF digit completes decoding, before it reaches the local controller or link router. DTMF command decoding remains active when disabled. |
| `transmit_hang_ms` | 0 | Hold PTT this many milliseconds after ordinary program audio or telemetry ends. Identifiers and announcements use a fixed 50 ms natural release tail instead. |
| `transmit_timeout_ms` | 180000 | Maximum continuous PTT duration in milliseconds. Zero disables the watchdog. On expiry, PTT releases immediately and remains blocked until the active receiver/link source clears and `timeout_lockout_ms` has elapsed. |
| `timeout_lockout_ms` | 30000 | Post-watchdog lockout in milliseconds. Zero permits recovery as soon as the timed-out source unkeys. |
| `kerchunk_max_ms` | 500 | Maximum local-receiver or individual-link transmission duration treated as a kerchunk. A kerchunk does not queue its courtesy tone or every-release announcement. Zero disables kerchunk control. |
| `telemetry_duck_db` | -20 | Smooth receive-active attenuation for sound-file, speech, Morse, and generated-tone identifiers, announcements, courtesy tones, and RF telemetry, from -60 through 0 dB. Local or linked receive selects the ducked level; release is smooth after it ends. |
| `courtesy_delay_ms` | 250 | Delay after a receiver or link source unkeys before its assigned courtesy tone starts. Each source retains its own delay when several tones are queued. A rekey by that same source before the delay ends cancels only that pending tone. PTT remains asserted from unkey through the queued tone's completion. |
| `sample_rate_hz` | 0 | Zero selects the highest usable local signed-linear rate no greater than the hardware-native rate. An explicit rate selects the local channel rate and requires a supported bidirectional Asterisk conversion path. |
| `radio_channel` | node section name | USBRadioPlus channel identifier without `RadioPlus/`. |
| `callsign` | empty | Optional local station callsign, up to 63 bytes. `${callsign}` in a scheduled message renders this exact value; an empty value renders nothing. |
| `codec` | empty | Empty selects signed linear for the local radio channel; otherwise select an available local Asterisk codec subject to `sample_rate_hz`. It does not otherwise restrict IAX link candidates. |
| `link_allow_nodes` | empty | Incoming node allowlist; comma-separated decimal node numbers. Empty places no allowlist restriction on verified nodes or their IAX DTMF control events. |
| `link_deny_nodes` | empty | Incoming node denylist. Explicit denial overrides allowlist membership and blocks that peer's IAX DTMF control events. |
| `link_static_directory_file` | empty | Optional local-priority Asterisk-format node directory. `[extnodes]` records use `number=radio@host:port/number,numeric-address`; both `number` fields must be the requested node. A present static record is authoritative. |
| `link_directory_file` | empty | Optional ASL external `[extnodes]` directory used by `link_lookup_method`. Its records use the same syntax and identity check as the static directory. |
| `link_lookup_method` | `both` | Selects sources after the static directory: `dns`, `file`, or `both`. `both` checks ASL DNS, then the external directory. |

Link access and directory settings are validated and inherit from `[general]` to each node. They
apply to incoming calls and to selecting a direct peer for remote-command mode; explicit denial
always wins. There are no access exemptions. These are rpt_advanced configuration lists, not ASL
AstDB lists, so a successful module reload applies an edit. An explicit empty node value clears
its inherited list. Spaces around entries are allowed; empty entries and wildcard patterns are
not. Entries match complete
node identities, not prefixes. Identity verification is separate: listing a
node never authenticates it. A local static record is checked first. If it is
absent, `link_lookup_method` selects DNS, the external file, or DNS followed by
the external file. A present malformed record or a resolved source-address
mismatch rejects the request without trying a later source. Incoming links enter through the
`RptAdvanced(node)` dialplan application; the IAX registration and dialplan
remain Asterisk configuration.

`sample_rate_hz` and `codec` configure only the local RadioPlusAdvanced
connection. For an outbound IAX call, rpt_advanced discovers every concrete
registered format at or below the local radio rate that Asterisk can translate
bidirectionally to matching-rate signed-linear PCM. It attempts those candidates
one at a time from the highest rate downward within one 20-second dialing budget.
This is necessary because Asterisk's public IAX request API reduces a multi-audio
capability to one format before IAX sees it. Each connected peer uses its
negotiated PCM rate, and the link adapter resamples between it and the local
radio rate. A peer can therefore negotiate a rate at or below the local radio
rate without requiring `codec_resample` for the peer-to-radio conversion.

## Courtesy tones

Courtesy tones are named media sets. `[courtesy]` supplies global media
defaults, `[courtesy node]` overrides those defaults for one node, and
`[courtesy node label]` defines one assigned tone. The label is an arbitrary
set name. Default sections provide inherited media settings only; every named
tone must set `input` to assign itself. For a named tone, `[speech]` and
`[speech node]`, then `[morse]` and `[morse node]`, supply their matching
media defaults after the courtesy defaults; the named tone overrides all of
them.

```ini
[courtesy]
level_db = -20

[courtesy 524950 receiver]
input = receiver
morse_text = R
tone_sequence = 500Hz / 80ms

[courtesy 524950 link]
input = link
morse_text = L
tone_sequence = 1000Hz / 80ms

[courtesy 524950 north]
input = link
remote_node = 12345
morse_text = N
tone_sequence = 800Hz / 80ms
```

There may be one named `input = receiver` tone and one generic
`input = link` tone per local node. The generic link tone has no
`remote_node` and is the fallback for every direct linked peer that has no
playable matching override. A named `input = link` tone may set `remote_node` to the
exact decimal identity of a **permanent direct peer**. It then overrides the
generic link tone only for that peer. A temporary peer never uses a
permanent-peer override. An override without playable media therefore falls
back to the generic link tone. `remote_node` is invalid for receiver input. If
no assignment matches a receiver or link source, the result is silence.

When a source rekeys before its courtesy delay expires, only that source's
pending courtesy tone is cancelled: a local-receiver rekey does not cancel a
link tone, and a rekey from one peer does not cancel a pending tone from
another peer. A tone that has already begun uses the normal receive-active
telemetry ducking behavior.

| Option | Applies to | Default | Meaning |
| --- | --- | --- | --- |
| `input` | Named tone | required | Source assignment: `receiver` or `link`. |
| `remote_node` | Named link tone | empty | Exact permanent direct-peer node identity for an override. Omit it for the generic link fallback. |
| `sound_file` | Defaults and named tone | empty | Courtesy sound-file path. An absent or unusable file falls through to speech. |
| `speech_text` | Defaults and named tone | empty | Courtesy speech text. Empty or unavailable speech falls through to the tone sequence. |
| `tone_sequence` | Defaults and named tone | empty | Generated-tone sequence used after file and speech fail or are absent. An absent sequence falls through to Morse. |
| `morse_text` | Defaults and named tone | empty | Terminal Morse fallback. Empty leaves that assignment silent when no earlier source is playable. |
| `level_db` | Defaults and named tone | -20 | Courtesy output control from -60 through 0 dB. It applies that relative gain to file and speech PCM, sets the Morse peak, and supplies the generated-tone peak when a segment omits its own level. |
| `speech_model`, `speech_speed_percent` | Defaults and named tone | inherited | Piper model and speed used by courtesy speech. |
| `morse_frequency_hz`, `morse_speed_wpm` | Defaults and named tone | inherited | Frequency and speed used by the terminal Morse fallback. |

Courtesy media is tried in this order: sound file, speech, generated
`tone_sequence`, then Morse. Each unavailable or unusable source falls through
to the next one. A configured tone sequence is syntax-checked while its enabled
node reloads even if a file or speech source is usable. Tone sequences are
configured only in this file: they require no controller-specific DTMF
programming. The module pre-renders them during reload, so parsing and allocation
never occur in the real-time audio path.

`tone_sequence` is a comma-separated list of segments:

```text
frequency1Hz[+frequency2Hz] / duration_ms [ / level_dB ], ...
```

Each frequency may be a decimal value; one or two frequencies generate one or
two simultaneous sine waves. Use `silence / duration_ms` or `0 / duration_ms`
for a silent segment. The `Hz`, `ms`, and `dB` or `dBFS` suffixes are optional,
whitespace is ignored, and each duration is from 1 through 60,000 ms. A segment's
optional tone peak is from -60 through 0 dBFS and overrides the inherited `level_db`;
otherwise the inherited level is used. The compact equivalent
`frequency@level / duration` is also accepted, but the three-field form is
clearer in configuration files. For example:

```ini
tone_sequence = 500Hz / 80ms, silence / 40ms, 1000Hz+1500Hz / 120ms / -20dB
```

Every non-silent frequency must be below the selected playback rate's Nyquist
limit. A sequence may contain up to 256 segments and 5,760,000 rendered PCM
samples in total (120 seconds at 48 kHz). Two-frequency segments divide their
configured level between the two components to avoid intentional clipping.
Oscillator phase is continuous between adjacent non-silent segments.

This covers the familiar controller patterns without their controller-specific
programming syntax: write a single beep as one segment, a two-tone as one
dual-frequency segment, a delay as `silence`, and longer source-identifying
patterns as additional comma-separated segments. A leading `silence` segment
adds a per-tone delay after the shared `courtesy_delay_ms`; later silent segments
are inter-segment delays. There is no fixed four-segment controller limit.

Queued source events play in unkey order, with each retaining its own delay. Up
to 16 near-simultaneous source unkeys are retained; an event beyond that fixed
real-time queue is ignored rather than allocating in the radio worker.

## Link command mappings

Each `link_command_*` value is the DTMF prefix after the initiating `*`. Empty
values disable that operation. Prefixes cannot overlap. The defaults use the
standard app_rpt link-function assignments, including the `806` and `816`
forms used by its reference configuration; an operator may instead map
disconnect-all and reconnect-all to `71` and `74`. A node-free command executes
when its complete prefix is received unless it prefixes a longer node-free
command; in that case the longer command has priority, while `#`, receiver
unkey, or the three-second timeout selects the shorter command. A
destination-taking command ends with `#`, receiver unkey, or after a
three-second interdigit timeout. Prefixes contain up to 63 DTMF digits from
`0` through `9` and uppercase `A` through `D`.

| Option | Default | Operation |
| --- | --- | --- |
| `link_command_disconnect` | `1` | Disconnect the selected nonpermanent link. |
| `link_command_monitor` | `2` | Connect in monitor mode: receive from the peer without sending program audio. |
| `link_command_transceive` | `3` | Connect in transceive mode. |
| `link_command_remote` | `4` | Enter direct-peer remote-command mode. `#` exits this mode locally. |
| `link_command_status` | `70` | Report direct-link status. |
| `link_command_disconnect_all` | `806` | Disconnect all current links and retain them for reconnect-all. |
| `link_command_last_keyed` | `72` | Report the most recently active direct linked node. |
| `link_command_local_monitor` | `75` | Connect in local-monitor mode: receive locally without forwarding the peer to other links. |
| `link_command_disconnect_permanent` | `811` | Disconnect a permanent link and cancel its recovery. |
| `link_command_permanent_monitor` | `812` | Make a permanent monitor link. |
| `link_command_permanent_transceive` | `813` | Make a permanent transceive link. |
| `link_command_full_status` | `73` | Queue direct-link RF status and, when that reply is queued, log the best-effort topology cache. |
| `link_command_reconnect_all` | `816` | Restore links saved by disconnect-all. |
| `link_command_permanent_local_monitor` | `818` | Make a permanent local-monitor link. |

Destination-taking operations accept node number `0` as the last node used by a
previous linking operation. Remote-command mode may select only an attached peer
whose node identity independently resolves and passes the same per-node
allow/deny policy as an incoming peer. The policy is rechecked before each
forwarded digit. It forwards subsequent DTMF digits only to that peer;
it does not invoke the local command decoder while active. `#` and local
receiver unkey always exit remote-command mode locally. An attached peer's IAX
DTMF end events enter the same control queue only while its identity passes the
current per-node allow/deny policy. An explicitly outbound audio link may
remain connected after a policy change, but its rejected peer DTMF is ignored.
DTMF starts and malformed end events are ignored.

Local in-band DTMF is decoded in the radio worker. With `dtmf_muting = yes`, a
frame containing a completed digit is silenced before it reaches the local
controller or link router. Each node inherits this setting from `[general]` and
can override it in its own section. This does not apply to IAX DTMF control
events, which contain no program-audio frame. Local receiver unkey terminates
an active DTMF command exactly as `#` does.

`*722` announces the local system time. It uses speech with the selected node's
`[speech]` defaults and falls back to Morse with its `[morse]` defaults if speech
cannot be prepared. Speech says the appropriate greeting followed by the time;
Morse sends only the time.

`*10` disconnects every currently connected nonpermanent direct peer. It does
not disconnect permanent peers or create reconnect-all state.

## Time settings

`[time]` supplies clock-announcement defaults and `[time node-name]` overrides them
for one node.

| Option | Default | Meaning |
| --- | --- | --- |
| `format` | `12` | Clock format for `*722`: `12` sends an AM/PM time; `24` sends a 24-hour time. The local system timezone is used. |

## Scheduled messages and macros

Named templates avoid repeating message text. A global `[template label]`
applies to every node. `[template node label]` overrides the global template of
the same label for that node. A resolved template requires one nonempty `text`
value; a node-specific template without `text` retains the global value.

```ini
[template net_start]
text = ${greeting}. The ${callsign} net starts at ${time}.

[template 524950 net_start]
text = ${greeting}. The KG0BP net starts at ${time}.
```

Template text is strict. It accepts only `${day_of_week}`, `${date}`, `${time}`,
`${greeting}`, `${link_status}`, `${node}`, and `${callsign}`. `${callsign}`
uses the resolved node `callsign` setting and may render empty. Unknown,
malformed, or unterminated substitutions reject the complete configuration
reload; text is not interpreted as a shell command. A rendered scheduled message
is limited to 127 bytes. Validation proves the worst case using the longest
weekday, date, clock, greeting, direct-peer status, node, and callsign values;
it rejects text that could exceed the limit rather than truncating it at runtime.

| Substitution | Rendered value |
| --- | --- |
| `${day_of_week}` | Full local weekday name. |
| `${date}` | Local date as `YYYY-MM-DD`. |
| `${time}` | Local clock using the event node's `[time]` `format`. |
| `${greeting}` | `Good Morning` before noon, `Good Afternoon` before 5 PM, or `Good Evening` afterward. |
| `${link_status}` | Current bounded direct-peer status, such as `NO LINKS`. |
| `${node}` | Event-owning node name. |
| `${callsign}` | Event node's configured `callsign`, or empty when it is unset. |

If rendered text contains no Morse-representable non-whitespace character, it
is treated as no message. This prevents a silent transmission when speech is
unavailable; an associated macro still runs.

Named macros select exactly one validated controller operation. A global
`[macro label]` may be overridden for one node by `[macro node label]`; omitted
node-specific settings retain global values. The only initial operations are
`connect`, `disconnect`, `disconnect_all`, and `reconnect_all`. `connect` and
`disconnect` require a decimal `target_node` of at most 63 digits.
`disconnect_all` and `reconnect_all` reject `target_node`. `connect` creates a
nonpermanent transceive direct link. `disconnect` detaches the active direct
peer named by `target_node`. `disconnect_all` detaches every current temporary
and permanent direct peer and pauses their retained retry records;
`reconnect_all` resumes every retained retry.
Macros never run shell commands or launch processes.

```ini
[macro connect_news]
action = connect
target_node = 123456

[macro 524950 clear_links]
action = disconnect_all
```

Zero-time scheduled events use `[event node label]`; there is no global event
section. Each event has one required local-time `at` trigger and must define a
direct `message`, a named `template`, a named `macro`, or a message/template
plus a macro. `message` and `template` are mutually exclusive. A message uses
the same strict substitution syntax as a named template. The named template or
macro is resolved global-first, then by a same-label node override.
Events for a disabled node are skipped while that node is disabled. Missed
occurrences are not retained or played later when the node is re-enabled.

```ini
[event 524950 morning_net]
at = weekly Tuesday 19:00
template = net_start
macro = connect_news

[event 524950 evening_id]
at = daily 21:30
message = ${greeting}. This is ${callsign}.

[event 524950 special]
at = once 2026-12-31 23:55
macro = clear_links
```

`at` accepts exactly these local-time forms:

| Form | Meaning |
| --- | --- |
| `daily HH:MM` | Run once each local day at a 24-hour hour and minute. |
| `weekly weekday HH:MM` | Run on the named full weekday (`Sunday` through `Saturday`) at that local time. Weekday spelling is case-insensitive. |
| `once YYYY-MM-DD HH:MM` | Run once on one valid local Gregorian calendar date and time. |

Seconds, time zones, ranges, aliases, and cron expressions are intentionally
invalid. A repeated local minute during daylight-saving fallback runs once, not
twice. Events due in the same minute run in complete configuration-section
order across all nodes. The scheduler retains one FIFO control task for each
wall-clock minute it observes, so slow speech preparation or a link action
cannot discard events due in a later minute. Their messages enter the serialized
telemetry path. When an event has both a message/template and a macro, the
message is queued before the macro executes. The macro does not wait for on-air playback: it runs
after the telemetry queue accepts the message. If that queue is full, both the
message and macro remain pending until a later control-plane tick can queue the
message. A macro-only event has no telemetry-queue dependency and runs when it
is selected. A successful configuration reload reevaluates the current local
minute. Macro dispatch, local-time matching, template rendering, and speech
preparation are control-plane work, never audio-callback work.

## Link lifetime, recovery, and duplex

Temporary and permanent links exist only in the running Asterisk process; no
link is written to configuration, so none survives an Asterisk restart. A
permanent link whose initial dial or attachment fails, or whose established
transport fails unexpectedly, retries immediately, then after one second with
exponential backoff to a five-minute maximum.
`link_command_disconnect_permanent` cancels its retained retry.
`link_command_disconnect_all` disconnects and retains both temporary and
permanent links for `link_command_reconnect_all`; only permanent links retry
automatically. Reconnect-all makes retained links eligible to dial again without
changing their original routing mode.

With `full_duplex = yes`, local receive and transmit may operate at the same
time. With `full_duplex = no`, local receive is not repeated and transmission
is held off while the local receiver is active. Received link audio remains
eligible for transmission when half duplex permits it. IDs and RF status replies
that become due during local reception wait until reception ends.

## Link status

The status, last-keyed, and full-status command mappings in the table above
queue concise spoken RF replies. They use the selected node's resolved
`[speech]` settings and fall back to the selected node's `[morse]` settings
only when speech cannot be prepared or reception interrupts it. They do not
key or begin playback until 250 ms after the local receiver unkeys, preempt a
scheduled identifier without satisfying it, and wait for reception to end when
half duplex prevents transmission. Speech preparation and topology work remain
outside the radio callback.

Link lifecycle telemetry names only the remote endpoint when this node is a
link endpoint, such as `123 CONNECTED`. For an event between two other nodes,
it says `node 1 CONNECTED TO node 2` or `node 1 DISCONNECTED FROM node 2`.

When its RF status reply is queued, full status writes a best-effort
app_rpt-style route list to the Asterisk operator log. The administrative
commands `rpt_advanced link status <node>`
and `rpt link status <node>` print every directly attached or retained peer with
its transceive, monitor, or local-monitor mode and permanent state. Retained
records are marked `retrying` or `paused`, followed by the current validated
topology cache advertised by connected peers. This is not an authoritative
network view: a peer may not advertise a list yet, and its cached list can be
stale. An empty topology is reported as `none`; local-monitor peers do not
appear in the route list.

`rpt_advanced command <node> <DTMF>` injects an administrative DTMF sequence
through the selected node's normal command collector. It uses the configured
mapping and authorization rules and treats a missing trailing `#` as receiver
unkey, so `rpt_advanced command 524950 *722` requests the configured time
announcement.

Each direct IAX peer receives a recipient-excluded `L ` topology advertisement
after topology changes and on a periodic refresh with a 30-second cadence. If a
route list cannot fit the bounded advertisement, its terminal `R000000` entry
means that the list was truncated. These advertisements and cached inbound `L `
messages are control-plane data and never run in the hardware-paced audio path.
An inbound topology that already reaches the local node is a loop: the direct
peer is disconnected without retry. A route that names another direct peer also
proves a loop, so it is disconnected even when that other peer does not advertise
topology. Direct self-links, duplicate direct links (including permanent links
and retained retries), and a requested target already named by an attached
peer's topology are rejected. A local rejection queues the spoken status `LINK
REJECTED TOPOLOGY LOOP`, with its normal Morse fallback.

## Identifier settings

| Option | Default | Meaning |
| --- | --- | --- |
| `interval_ms` | 600000 | Positive ID interval in milliseconds. |
| `priority` | 0 | Nonnegative priority through 2147483647; higher wins. |
| `first_key_only` | no | Identify only on first key after at least one interval of inactivity. |
| `regardless_of_activity` | no | Continue periodic IDs during inactivity; not used by first-key-only sets. |
| `polite` | no | Hold a due ID while local or linked receive is active and until queued RF telemetry has played. |
| `polite_maximum_wait_ms` | 60000 | Maximum polite hold after the ID becomes due. After this interval the ID becomes eligible even if reception continues; half-duplex operation still cannot transmit during local receive. A status or courtesy announcement that has already started finishes first. |
| `sound_file` | empty | File path; missing or unusable files fall back to speech. |
| `speech_text` | empty | Speech text; empty disables speech. |
| `morse_text` | empty | Morse text; empty disables the terminal fallback. |

## Announcement settings

`[announcement]` supplies global defaults, `[announcement node]` overrides them
for one node, and `[announcement node set]` defines a named playable
announcement. A flat or node-default section alone does not schedule playback.
Announcements use the same sound-file, speech, and Morse fallback sequence as
identifiers: sound file, then speech, then Morse.

| Option | Default | Meaning |
| --- | --- | --- |
| `interval_ms` | 0 | Zero plays after every completed transmission. A positive value limits the set to one successful playback per interval, measured from the prior successful playback. |
| `sound_file` | empty | File path; missing or unusable files fall back to speech. |
| `speech_text` | empty | Speech text; empty disables speech. |
| `morse_text` | empty | Morse text; empty disables the terminal fallback. |

Due announcements play one at a time in configuration-section order, after the
ordinary transmit hang and any due identifier, but before PTT is released. If a
positive interval elapses while the transmitter is idle, the controller keys
PTT, plays any due identifier first, then the announcement, and releases PTT
with the short natural tail. An announcement that becomes due during a
transmission waits for that transmission to finish. No announcement is
configured by default.

## Speech defaults

`[speech]` supplies defaults for every node. `[speech node]` overrides it for
that node. The `speech_model`, `speech_speed_percent`, and `speech_level_db`
names are valid in identifier and announcement default and set sections. A
matching `[speech]` or `[speech node]` value overrides a media-default value;
a named-set value overrides every default.

| Option | Default | Meaning |
| --- | --- | --- |
| `voice` | `en_US-lessac-medium.onnx` | Local Piper model path. A missing model falls back to Morse. |
| `speed_percent` | 100 | Speaking rate relative to the model default, from 1 through 1000 percent. |
| `level_db` | 0 | Synthesized-speech gain from -60 through 0 dBFS. It does not change sound-file or Morse level. |

Named identifier and announcement sets override these with `speech_model`,
`speech_speed_percent`, and `speech_level_db`.

## Morse defaults

`[morse]` supplies defaults for every node. `[morse node]` overrides it for
that node. The `morse_frequency_hz`, `morse_speed_wpm`, and `morse_level_db`
names are valid in identifier and announcement default and set sections. A
matching `[morse]` or `[morse node]` value overrides a media-default value; a
named-set value overrides every default.

| Option | Default | Meaning |
| --- | --- | --- |
| `frequency_hz` | 800 | Positive tone frequency, also required to be below the playback rate's Nyquist limit. |
| `speed_wpm` | 20 | PARIS words per minute, from 1 through 100. |
| `level_db` | -6 | Morse-tone level from -60 through 0 dBFS. -6 dB is approximately the historical half-scale tone. |

Named identifier and announcement sets override these with `morse_frequency_hz`,
`morse_speed_wpm`, and `morse_level_db`.

Switches accept `yes` or `no`, ignoring case. Unsigned numeric values use
decimal digits without signs or suffixes. Audio levels accept signed decimal
integers in their documented range. Sample rate and Morse frequency fit unsigned
32-bit values; interval and hang time fit unsigned 64-bit values. Invalid typed
values do not partially update a resolved settings object. Runtime capability
checks are separate from numeric parsing.

No media is configured by default. Zero identifier or announcement sets is
valid. For testing on 524950, set `voice` in `[speech 524950]` to
`/usr/lib/piper-tts/voices/en_US-amy-low.onnx` to use the installed voice
without downloading another model.

The Morse renderer accepts ASCII letters (case-insensitive), digits, and
`/ . , ? - = + @ ( ) ' ! " : ; _ $ &`. Spaces, tabs, and line breaks separate
words. Unsupported characters are rejected. Timing uses standard 1-unit dots,
3-unit dashes, 1-unit element gaps, 3-unit character gaps, and 7-unit word gaps.
Generated PCM uses the configured Morse level; transmitter processing follows it.

File and synthesized audio preparation uses the `ffmpeg` executable to produce
mono PCM at the selected playback rate. Preparation runs outside the audio loop;
prepared samples are consumed at the radio hardware's cadence. A failed decode
or conversion is a playback-source failure and must follow the ID fallback order.
Preparation occurs when nodes start or reload. Each external process is limited
to thirty seconds before that source fails. Reload can therefore pause radio
operation while files or speech are prepared. Temporary files are removed after
preparation; playback uses immutable in-memory PCM. Sets with neither prepared
audio nor configured Morse text are omitted from scheduling.
