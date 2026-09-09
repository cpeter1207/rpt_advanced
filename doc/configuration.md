# Configuration model

Configuration reading, storage, whole-file validation, node/ID discovery, and
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
then `[identifier 524950]`. The `speech_*` and `morse_*` names are valid in
both of those identifier-default sections as well as in an identifier set.
After the identifier-default sections, `[speech]` then `[speech 524950]` apply
their matching speech values, and `[morse]` then `[morse 524950]` apply their
matching Morse values. Finally, `[identifier 524950 welcome]` overrides every
matching identifier, speech, and Morse value for that one set. File order does
not change this scope precedence; later occurrences of the same option in one
section win. Empty media paths or text clear inherited values.

Node and ID-set names are case-sensitive and cannot contain whitespace or square
brackets. Scoped headers use one space between components. `general` and
`identifier`, `speech`, `morse`, and `time` are reserved flat-section names. Scoped
identifier, speech, Morse, and time headers must name an existing node, which may be
declared later in the file. Repeated section headers merge options without
creating duplicate nodes or ID sets. Unknown options and invalid values are
rejected even if a later entry would override them. There is no fixed limit on
the number of nodes or ID sets.

## Node settings

| Option | Default | Meaning |
| --- | --- | --- |
| `node_enabled` | yes | Start the configured node. |
| `full_duplex` | yes | Allow simultaneous reception and transmission. |
| `dtmf_muting` | yes | Silence a local received PCM frame when an in-band DTMF digit completes decoding, before it reaches the local controller or link router. DTMF command decoding remains active when disabled. |
| `transmit_hang_ms` | 0 | Hold PTT this many milliseconds after audio ends. |
| `telemetry_duck_db` | -20 | Smooth receive-active attenuation for sound-file, speech, and Morse identifiers and RF telemetry, from -60 through 0 dB. Local or linked receive selects the ducked level; release is smooth after it ends. |
| `courtesy_delay_ms` | 250 | Delay after local-receiver or linked-audio unkey before a courtesy announcement starts. Resumed local or linked receive before the delay ends cancels the pending tone. PTT remains asserted until a started announcement completes. |
| `receiver_courtesy_sound_file` | empty | Local-receiver courtesy sound-file path. |
| `receiver_courtesy_speech_text` | empty | Local-receiver courtesy speech text, used when its file is absent or unusable. |
| `receiver_courtesy_morse_text` | empty | Local-receiver terminal Morse courtesy text. Set `R` for an R courtesy tone. |
| `receiver_courtesy_morse_frequency_hz` | inherited | Local-receiver courtesy Morse frequency. When omitted, it uses the resolved `[morse]` frequency for this node. |
| `receiver_courtesy_level_db` | -20 | Local-receiver courtesy sound, speech, and Morse level, from -60 through 0 dB. |
| `link_courtesy_sound_file` | empty | Linked-receiver courtesy sound-file path. |
| `link_courtesy_speech_text` | empty | Linked-receiver courtesy speech text, used when its file is absent or unusable. |
| `link_courtesy_morse_text` | empty | Linked-receiver terminal Morse courtesy text. Set `L` for an L courtesy tone. |
| `link_courtesy_morse_frequency_hz` | inherited | Linked-receiver courtesy Morse frequency. When omitted, it uses the resolved `[morse]` frequency for this node. |
| `link_courtesy_level_db` | -20 | Linked-receiver courtesy sound, speech, and Morse level, from -60 through 0 dB. |
| `sample_rate_hz` | 0 | Zero selects the highest usable local signed-linear rate no greater than the hardware-native rate. An explicit rate selects the local channel rate and requires a supported bidirectional Asterisk conversion path. |
| `radio_channel` | node section name | USBRadioPlus channel identifier without `RadioPlus/`. |
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

Each direct IAX peer receives a recipient-excluded `L ` topology advertisement
after topology changes and on a periodic refresh with a 30-second cadence. If a
route list cannot fit the bounded advertisement, its terminal `R000000` entry
means that the list was truncated. These advertisements and cached inbound `L `
messages are control-plane data and never run in the hardware-paced audio path.
An inbound topology that already reaches the local node is a loop: the direct
peer is disconnected without retry. Direct self-links are rejected.

## Identifier settings

| Option | Default | Meaning |
| --- | --- | --- |
| `interval_ms` | 600000 | Positive ID interval in milliseconds. |
| `priority` | 0 | Nonnegative priority through 2147483647; higher wins. |
| `first_key_only` | no | Identify only on first key after at least one interval of inactivity. |
| `regardless_of_activity` | no | Continue periodic IDs during inactivity; not used by first-key-only sets. |
| `sound_file` | empty | File path; missing or unusable files fall back to speech. |
| `speech_text` | empty | Speech text; empty disables speech. |
| `morse_text` | empty | Morse text; empty disables the terminal fallback. |

## Speech defaults

`[speech]` supplies defaults for every node. `[speech node]` overrides it for
that node. The `speech_model`, `speech_speed_percent`, and `speech_level_db`
names are valid in `[identifier]`, `[identifier node]`, and an identifier-set
section. A matching `[speech]` or `[speech node]` value overrides an
identifier-default value; an identifier-set value overrides every default.

| Option | Default | Meaning |
| --- | --- | --- |
| `voice` | `en_US-lessac-medium.onnx` | Local Piper model path. A missing model falls back to Morse. |
| `speed_percent` | 100 | Speaking rate relative to the model default, from 1 through 1000 percent. |
| `level_db` | 0 | Synthesized-speech gain from -60 through 0 dBFS. It does not change sound-file or Morse level. |

Identifier-set overrides are `speech_model`, `speech_speed_percent`, and
`speech_level_db`.

## Morse defaults

`[morse]` supplies defaults for every node. `[morse node]` overrides it for
that node. The `morse_frequency_hz`, `morse_speed_wpm`, and `morse_level_db`
names are valid in `[identifier]`, `[identifier node]`, and an identifier-set
section. A matching `[morse]` or `[morse node]` value overrides an
identifier-default value;
an identifier-set value overrides every default.

| Option | Default | Meaning |
| --- | --- | --- |
| `frequency_hz` | 800 | Positive tone frequency, also required to be below the playback rate's Nyquist limit. |
| `speed_wpm` | 20 | PARIS words per minute, from 1 through 100. |
| `level_db` | -6 | Morse-tone level from -60 through 0 dBFS. -6 dB is approximately the historical half-scale tone. |

Identifier-set overrides are `morse_frequency_hz`, `morse_speed_wpm`, and
`morse_level_db`.

Switches accept `yes` or `no`, ignoring case. Unsigned numeric values use
decimal digits without signs or suffixes. Audio levels accept signed decimal
integers in their documented range. Sample rate and Morse frequency fit unsigned
32-bit values; interval and hang time fit unsigned 64-bit values. Invalid typed
values do not partially update a resolved settings object. Runtime capability
checks are separate from numeric parsing.

No media is configured by default. Zero ID sets is valid. For testing on 524950,
set `voice` in `[speech 524950]` to
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
