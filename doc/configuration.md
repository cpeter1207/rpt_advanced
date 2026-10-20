# Configure rpt_advanced

rpt_advanced runs a radio node, links it to other nodes, and plays IDs,
announcements, and courtesy tones over the local transmitter. It can use the
standalone radio service or the Asterisk adapter. This guide lists the settings
for both. Use only the sections that apply to your station.

The installed example is `share/doc/rpt-advanced/examples/rpt_advanced.conf`
under the installation prefix. Its example node is disabled and its ID text is
empty. Installation does not replace an active configuration or activate a node.

Put each `name = value` line under a section heading. Start comments with `;`.
Blank lines and spaces around names and values are allowed. Check the file with
`rpt-advanced --check-config /etc/rpt_advanced/rpt_advanced.conf` before
restarting the standalone service. A bad line or unreadable file stops loading.
Unknown settings and invalid values give warnings; invalid values use the
inherited default when a safe configuration is possible.

`[general]` sets defaults for all nodes. `[524950]` changes one node.
`[identifier]`, `[announcement]`, `[courtesy]`, `[speech]`, and `[morse]` set
media defaults for all nodes. Add the node name, such as `[speech 524950]`, to
change one node. Add a set name, such as `[identifier 524950 welcome]`, to set
one ID. A set's values take priority over defaults, regardless of file order.
`[tail]` and `[tail node]` set release-tail defaults; `[tail node name]`
defines a tail. A named `[message name]` defines a reusable message. Use
`[message node name]` to change that message for one node.
An empty media path or text clears an inherited value. Courtesy uses one
`level_db` for all media; other media sections can use `speech_*` and
`morse_*` settings.

Named templates and macros use `[template label]` and `[macro label]` for all
nodes. Add a node name before the label to change one node's copy. Events,
permanent links, and replacement schedules always name a node:
`[event node label]`, `[permanent node label]`, or `[schedule node label]`.
Template, macro, event, permanent-link, and schedule labels are case-sensitive
single tokens and cannot contain whitespace or square brackets.
The same name rules apply to messages, tails, and site items. Repeating one
named definition is an error. A node-specific message may override a shared
message of the same name.

Node names are case-sensitive, limited to 63 bytes, and cannot contain spaces or square
brackets. Media-set names are case-sensitive and cannot contain spaces or square brackets.
Remote node numbers are decimal and limited to 63 digits. Scoped headers use
one space between components. `general`,
`identifier`, `announcement`, `courtesy`, `speech`, `morse`, `time`, `template`,
`macro`, `event`, `permanent`, `schedule`, `message`, `tail`, `sensor`, `input`,
`output`, `alarm`, and `fan` are reserved section names. Scoped identifier,
announcement, courtesy, speech, Morse, time, template, macro, event, permanent, and schedule
headers must name an existing node where their syntax includes a node name, which may be
declared later in the file.
Repeated ordinary section headings combine their settings. Repeating a named
template, macro, event, permanent link, or schedule heading is an error. In the
same section, the last value for a setting wins. An empty media path or text
clears an inherited value.

## Node settings

| Option | Default | Meaning |
| --- | --- | --- |
| `node_enabled` | yes | Start the configured node. |
| `language` | `en-US` | Language for built-in RF messages. Set it here for all nodes or in one node section. |
| `full_duplex` | yes | Allow simultaneous reception and transmission. |
| `dtmf_muting` | yes | Mute local DTMF command tones on repeated audio. The start of a tone may still be heard. Commands still work when this is `no`. |
| `dtmf_regeneration_enabled` | no | Play eligible DTMF digits received from links on the local transmitter. See [DTMF regeneration](#dtmf-regeneration). |
| `dtmf_regeneration_duration_ms` | 200 | Length of each regenerated tone, in whole milliseconds; 1–1000. |
| `dtmf_regeneration_gap_ms` | 50 | Silence after each regenerated tone, in whole milliseconds; 1–1000. |
| `dtmf_regeneration_level_dbfs` | -12 | Combined peak level of the two DTMF tones before transmitter processing; whole dBFS from -60 to 0. |
| `parrot_enabled` | no | Record a received burst and play it back after unkey. Live `*804` and `*805` changes last until reload. |
| `dtmf_admin_unlock_hash` | empty | Argon2id hash of the code used after `*800` to unlock admin DTMF commands. Both admin hashes must be set to enable them. |
| `dtmf_admin_lock_hash` | empty | Argon2id hash of the code used after `*801` to lock admin DTMF commands. Do not put a plain code here. |
| `dtmf_admin_timeout_ms` | 300000 | Lock admin DTMF after this many milliseconds without a successful admin command; must be positive. |
| `squelch_delay_ms` | 0 | Delay local receive audio by this many milliseconds. Unkey drops the buffered tail. Zero adds no delay. |
| `status_snapshot_interval_ms` | 50 | Update RX/TX meters and receive-buffer statistics this often, in milliseconds; must be positive. |
| `transmit_hang_ms` | 0 | Hold PTT this many milliseconds after ordinary program audio or telemetry ends. Identifiers and announcements use a fixed 50 ms natural release tail instead. |
| `ctcss_encode_on_input` | no | Send transmit CTCSS only during local or linked receive activity and command replies, plus `ctcss_hang_ms`. Other IDs and telemetry do not turn it on. |
| `ctcss_hang_ms` | 0 | Keep transmit CTCSS on this many milliseconds after qualifying activity ends. It cannot exceed `transmit_hang_ms`. |
| `transmit_timeout_ms` | 180000 | Maximum time PTT may stay keyed without a local receiver or direct link unkey, in milliseconds. Each unkey restarts the timer. On timeout, PTT releases until that source unkeys and the lockout ends. Zero disables the timeout. |
| `timeout_lockout_ms` | 30000 | Post-watchdog lockout in milliseconds. Zero permits recovery as soon as the timed-out source unkeys. |
| `kerchunk_max_ms` | 500 | Maximum local-receiver or individual-link transmission duration treated as a kerchunk. A kerchunk does not queue its courtesy tone or every-release announcement. Zero disables kerchunk control. |
| `telemetry_duck_db` | -20 | Lower ID, announcement, courtesy, and RF message audio during local or linked receive; -60 to 0 dB. Zero leaves the level unchanged. |
| `courtesy_delay_ms` | 250 | Wait this many milliseconds after unkey before a courtesy tone. A source that rekeys during its wait cancels its tone. PTT stays keyed until the tone ends. |
| `radio_channel` | node section name | USBRadioPlus channel identifier without `RadioPlus/`. |
| `callsign` | empty | Optional local station callsign, up to 63 bytes. `${callsign}` in a scheduled message renders this exact value; an empty value renders nothing. |
| `link_allow_nodes` | empty | Incoming node allowlist; comma-separated decimal node numbers. Empty places no allowlist restriction on verified nodes or their IAX DTMF control events. |
| `link_deny_nodes` | empty | Incoming node denylist. Explicit denial overrides allowlist membership and blocks that peer's IAX DTMF control events. |
| `link_static_directory_file` | empty | Optional local-priority Asterisk-format node directory. `[extnodes]` records use `number=radio@host:port/number,numeric-address`; both `number` fields must be the requested node. A present static record is authoritative. |
| `link_directory_file` | empty | Optional ASL external `[extnodes]` directory used by `link_lookup_method`. Its records use the same syntax and identity check as the static directory. |
| `link_lookup_method` | `both` | Selects sources after the static directory: `dns`, `file`, or `both`. `both` checks ASL DNS, then the external directory. |
| `statpost_url` | empty | HTTP or HTTPS URL for node status posts. Empty turns status posting off. Do not put a username or password in the URL. |
| `statpost_time` | 60 | Seconds between status posts; 30–600. |
| `iax_registration_url` | `https://register.allstarlink.org/` | HTTPS registration URL for ASL3. Empty turns registration off. Do not put a username or password in the URL. |
| `iax_registration_interval_s` | 60 | Seconds between IAX registration requests; 30–600. |
| `iax_local_port` | 4569 | Local IAX UDP port; 1–65535. |

Identifiers, announcements, and courtesy tones play on local RF. A command or
status reply goes to the station that sent the command: local RF for a local
command, or the linked peer for a peer command. Linked peers also receive
eligible program audio. Transmit hang time sends no extra peer audio.

Link access and directory settings are validated and inherit from `[general]` to each node. They
apply to incoming calls and to selecting a destination for remote-command mode; explicit denial
always wins. There are no access exemptions. These are rpt_advanced configuration lists, not ASL
AstDB lists, so a successful module reload applies an edit. An explicit empty node value clears
its inherited list. Spaces around entries are allowed; empty entries and wildcard patterns are
not. Entries match complete
node identities, not prefixes. Identity verification is separate: listing a
node never authenticates it. A local static record is checked first. If it is
absent, `link_lookup_method` selects DNS, the external file, or DNS followed by
the external file. A present malformed record or a resolved source-address
mismatch rejects the request without trying a later source. With the Asterisk
adapter, incoming links use the `RptAdvanced(node)` dialplan application.
Configure its IAX registration and dialplan in Asterisk.

The local radio uses 48 kHz audio. With the Asterisk adapter, an outbound IAX
link tries supported codecs from the highest rate down, within a 20-second
dialing limit; a peer can use a rate at or below 48 kHz. The standalone service
uses 8 kHz ULAW for IAX links.

## Standalone IAX passwords

For IAX registration, put passwords in
`/etc/rpt_advanced/iax-secrets.conf`, separate from the main configuration.
In `[general]`, `iax_secret` sets a shared password; a section such as
`[524950]` sets that node's password. Use the password assigned to the node.
For example, replace the placeholder before saving:

```ini
[524950]
iax_secret = <your node password>
```

Create or edit this file with `sudoedit`, then give ownership to the
`rpt-advanced` service user and set mode `0600`:

```sh
sudoedit /etc/rpt_advanced/iax-secrets.conf
sudo chown rpt-advanced:rpt-advanced /etc/rpt_advanced/iax-secrets.conf
sudo chmod 0600 /etc/rpt_advanced/iax-secrets.conf
stat -c '%U %a %n' /etc/rpt_advanced/iax-secrets.conf
sudo -u rpt-advanced rpt-advanced --check-secrets-file /etc/rpt_advanced/iax-secrets.conf
```

It must be a regular file; links to another file are rejected. The service
reads it at start and again for registration after a configuration reload. The
default file may be absent when no IAX password is needed.

## Standalone radio settings

The standalone service owns local audio-device selection and CM119 signaling.
Flat `[radio]` values are shared defaults; `[radio <node>]` overrides them for
one node. These settings are independent of `radio_channel`, which is used only
by the optional Asterisk adapter. The audio rate is 48 kHz. Processing
graphs are mono FFmpeg audio-filter chains; `anull` leaves audio unchanged.

| Option | Default | Meaning |
| --- | --- | --- |
| `device_selection` | `exact` | Select `exact` for an identified device, or `automatic_lowest_alsa_card` to choose the lowest ALSA card. Automatic selection is intended for a host with one radio. |
| `device_identifier` | empty | PortAudio device name or stable USB/ALSA identifier, up to 255 bytes. Used with `usb_serial` when selecting an exact device. |
| `usb_serial` | empty | Optional exact CM119 USB serial number, up to 255 bytes. |
| `input_device_channels` | `1` | Physical capture channel count; accepted values are 1 or 2. |
| `output_device_channels` | `1` | Physical playback channel count; accepted values are 1 or 2. |
| `input_extra_buffer_ms` | `0` | Additional capture buffering requested beyond PortAudio's low-latency default; 0–500 ms. |
| `output_extra_buffer_ms` | `0` | Additional playback buffering requested beyond PortAudio's low-latency default; 0–500 ms. |
| `receive_input_gain_db` | `0` | Receiver input gain, from -30 to +30 dB. |
| `receive_output_gain_db` | `0` | Receiver output gain, from -30 to +30 dB. |
| `receive_graph` | `anull` | FFmpeg chain applied to mono 48 kHz receiver audio; 1–4096 bytes. |
| `transmit_graph` | `anull` | FFmpeg chain applied to mono 48 kHz transmitter audio; 1–4096 bytes. |
| `cm119_profile` | `dudeusb` | CM119 wiring profile: `dudeusb`, `sphusb`, `nhrc`, or `custom`. |
| `cm119_ptt_inverted` | `no` | Invert the CM119 PTT output polarity. |
| `cm119_gpio_1_mode` … `cm119_gpio_8_mode` | `in` | Set each general GPIO pin to input (`in`), output low (`out0`), or output high (`out1`) at open. |
| `cm119_clip_led_gpio` | `0` | GPIO number for the clipping indicator; 0 disables it, 1–8 selects a pin. |

Receive signaling options:

| Option | Default | Meaning |
| --- | --- | --- |
| `receive_audio` | `flat` | Receiver audio source: `disabled`, speaker audio (`speaker`), or flat discriminator audio with deemphasis (`flat`). |
| `receive_signaling` | `carrier` | Receive qualification: carrier only (`carrier`), CTCSS (`ctcss`), or DCS (`dcs`). |
| `carrier_source` | `dsp` | Carrier indication from `disabled`, DSP noise squelch (`dsp`), VOX (`vox`), CM119 GPIO (`cm119`/`cm119_inverted`), or parallel input (`parallel`/`parallel_inverted`). |
| `ctcss_source` | `dsp` | CTCSS/DCS indication source: `disabled`, `dsp`, CM119 GPIO, or parallel input; GPIO and parallel sources accept normal or inverted polarity. |
| `receive_ctcss_tones_hz` | `100.0` | Comma-separated allowed CTCSS tones from the supported decoder table. |
| `ctcss_decoder_gain_db` | `0` | Gain applied before native CTCSS decoding; −60 to +24 dB. |
| `ctcss_relaxed` | `yes` | Enable relaxed CTCSS decode tolerance. |
| `ctcss_override` | `no` | Permit carrier qualification without a decoded CTCSS tone when override behavior is requested. |
| `dcs_receive_code` | `023N` | Three-digit octal DCS code and polarity (`N` normal or `I` inverted). |
| `squelch_level` | `500` | Native DSP noise-squelch threshold on the 0–999 scale. |
| `squelch_hysteresis` | `3000` | DSP noise-squelch closing hysteresis in PCM codes (0–32767). |
| `noise_filter` | `standard` | Native discriminator-noise detector response: `standard` or `alternate`. |
| `vox_threshold` | `0` | VOX detector threshold in PCM codes (0–32767). |
| `vox_hang_ms` | `2000` | VOX hold time after received audio ends (0–32767 ms). |
| `receive_on_delay_ms` | `0` | Ignore receiver keying for this interval after transmitter release (0–65535 ms). |
| `radio_duplex_mode` | `half` | Physical radio capability: `half` or `full` duplex. |

Transmit signaling options:

| Option | Default | Meaning |
| --- | --- | --- |
| `transmit_output_a_assignment` | `voice` | Audio sent to radio output A: `disabled`, `voice`, `tone`, `composite`, or `auxiliary_voice`. |
| `transmit_output_b_assignment` | `tone` | Audio sent to radio output B; same choices as output A. |
| `transmit_signaling` | `carrier` | Transmit carrier only (`carrier`), CTCSS (`ctcss`), or DCS (`dcs`). |
| `transmit_ctcss_tones_hz` | `100.0` | Comma-separated CTCSS tones permitted for transmission. |
| `transmit_ctcss_default_hz` | `100.0` | CTCSS tone used when no selected mapping overrides it. |
| `transmit_ctcss_level_dbfs` | `-24` | CTCSS peak level (−60 to 0 dBFS). |
| `transmit_ctcss_turnoff_mode` | `phase_shift` | CTCSS turnoff: `none`, `phase_shift`, `tone_remove`, or `tail_tone`. |
| `transmit_ctcss_phase_shift_degrees` | `120` | Phase shift for reverse-burst turnoff (0–360 degrees). |
| `transmit_ctcss_turnoff_duration_ms` | `180` | CTCSS turnoff duration (0–1000 ms). |
| `transmit_ctcss_tail_tone_hz` | `55` | Replacement low-frequency tail tone for `tail_tone` mode (0–300 Hz). |
| `transmit_dcs_code` | `023N` | Transmitted three-digit octal DCS code and polarity. |
| `transmit_dcs_level_dbfs` | `-24` | DCS peak level (−60 to 0 dBFS). |
| `transmit_dcs_turnoff_enabled` | `yes` | Send DCS turnoff signaling when transmission ends. |
| `transmit_dcs_turnoff_duration_ms` | `180` | DCS turnoff interval (150–200 ms). |
| `transmit_settle_ms` | `500` | Delay after PTT assertion before program audio starts (0–32767 ms). |
| `transmit_receive_blanking_ms` | `0` | Blank receive qualification while transmitting (0–32767 ms). |
| `transmit_off_delay_ms` | `0` | Ignore receiver activity for this interval after transmit release (0–32767 ms). |

The complete commented example is installed at
`share/doc/rpt-advanced/examples/rpt_advanced.conf`. Run
`rpt-advanced --check-config /etc/rpt_advanced/rpt_advanced.conf` before
restarting the standalone service. Inherited or invalid radio values use the
documented defaults with a warning; unsafe configurations are rejected.

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

Use one `input = receiver` tone and one generic `input = link` tone per node.
The generic link tone has no `remote_node`. For a peer's own tone, add another
link tone with that peer's decimal number in `remote_node`. A linked network may
also report which downstream node is keyed. The first valid report for each
query selects that node's tone; a later valid result can replace it until the
direct peer unkeys. If no downstream tone matches, the direct peer tone plays;
if that is unavailable, the generic link tone plays. A missing playable tone is
silent. `remote_node` cannot be used with `input = receiver`.

When a source rekeys before its courtesy delay expires, only that source's
pending courtesy tone is cancelled: a local-receiver rekey does not cancel a
link tone, and a rekey from one peer does not cancel a pending tone from
another peer. A tone that has already begun uses the normal receive-active
telemetry ducking behavior.

| Option | Applies to | Default | Meaning |
| --- | --- | --- | --- |
| `input` | Named tone | required | Source assignment: `receiver` or `link`. |
| `remote_node` | Named link tone | empty | Decimal node number for a peer-specific courtesy tone. This can match a direct peer or a reported keyed downstream node. Omit it for the generic link tone. |
| `sound_file` | Defaults and named tone | empty | Courtesy sound-file path. An absent or unusable file falls through to speech. |
| `speech_text` | Defaults and named tone | empty | Courtesy speech text. Empty or unavailable speech falls through to the tone sequence. |
| `tone_sequence` | Defaults and named tone | empty | Generated-tone sequence used after file and speech fail or are absent. An absent sequence falls through to Morse. |
| `morse_text` | Defaults and named tone | empty | Terminal Morse fallback. Empty leaves that assignment silent when no earlier source is playable. |
| `level_db` | Defaults and named tone | -20 | Courtesy output control from -60 through 0 dB. It applies that relative gain to file and speech PCM, sets the Morse peak, and supplies the generated-tone peak when a segment omits its own level. |
| `speech_model`, `speech_speed_percent` | Defaults and named tone | inherited | Piper model and speed used by courtesy speech. |
| `morse_frequency_hz`, `morse_speed_wpm` | Defaults and named tone | inherited | Frequency and speed used by the terminal Morse fallback. |

Courtesy media plays in this order: sound file, speech, `tone_sequence`, then
Morse. If one source is missing or cannot play, the next is tried. The tone
sequence is checked when an enabled node reloads.

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

Each non-silent frequency must be below half the playback rate. A sequence may
have up to 256 segments and 120 seconds of audio at 48 kHz.

Use one segment for a beep, two frequencies for a dual tone, and `silence` for a
pause. A leading silence adds delay after `courtesy_delay_ms`.

Courtesy tones play in unkey order. Up to 16 near-simultaneous unkeys can wait;
any more are skipped.

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
three-second interdigit timeout. Remote-command mode is different: it selects
the destination as soon as the entered node number is valid. Prefixes contain
up to 63 DTMF digits from `0` through `9` and uppercase `A` through `D`.

| Option | Default | Operation |
| --- | --- | --- |
| `link_command_disconnect` | `1` | Disconnect the selected nonpermanent link. |
| `link_command_monitor` | `2` | Connect in monitor mode: receive from the peer without sending program audio. |
| `link_command_transceive` | `3` | Connect in transceive mode. |
| `link_command_remote` | `4` | Select a destination for remote commands. `#` exits this mode locally. |
| `link_command_status` | `70` | Report direct-link status. |
| `link_command_disconnect_all` | `806` | Disconnect and pause permanent links, including configured links. Temporary links stay connected. New manual links remain allowed. |
| `link_command_last_keyed` | `72` | Report the most recently active direct linked node. |
| `link_command_local_monitor` | `75` | Connect in local-monitor mode: receive locally without forwarding the peer to other links. |
| `link_command_site_status` | `76` | Read the meter or input selected by its `dtmf_id`. |
| `link_command_message` | `77` | Play the message selected by its `dtmf_id`. |
| `link_command_disconnect_permanent` | `811` | Disconnect a manually created permanent link and stop its retries. Configured links follow their schedule; use `*806` to pause them. |
| `link_command_permanent_monitor` | `812` | Make a permanent monitor link. |
| `link_command_permanent_transceive` | `813` | Make a permanent transceive link. |
| `link_command_full_status` | `73` | Reply with link status and log the known network routes. The route list may be incomplete. |
| `link_command_reconnect_all` | `816` | Resume paused permanent links under the current schedule. Temporary links are unchanged. |
| `link_command_permanent_local_monitor` | `818` | Make a permanent local-monitor link. |
| `link_command_admin_unlock` | `800` | Enter the configured admin unlock code, then `#`. Requires both admin code hashes. |
| `link_command_admin_lock` | `801` | Enter the configured admin lock code, then `#`. Locks admin commands at once. |
| `link_command_parrot_enable` | `804` | Turn parrot playback on until reload. Requires admin unlock. |
| `link_command_parrot_disable` | `805` | Turn parrot playback off until reload. Requires admin unlock. |
| `link_command_pad_test` | `82` | Start a local RF keypad test. No admin unlock is needed. Unkey to hear the captured digits. |

Destination-taking operations accept node number `0` as the last node used by a
previous linking operation.

With `dtmf_muting = yes`, local DTMF command audio is muted on repeat and link
audio. IAX DTMF control events have no audio to mute. Local receiver unkey ends
ordinary commands like `#`; pad test and selected remote-command mode use the
rules below.

`*722` announces the local system time. It uses speech with the selected node's
`[speech]` defaults and falls back to Morse with its `[morse]` defaults if speech
cannot be prepared. Speech says the appropriate greeting followed by the time;
Morse sends only the time.

`*10` disconnects every currently connected nonpermanent direct peer. It does
not disconnect permanent peers or create reconnect-all state.

### Remote commands

Send `*4` followed by the destination node number. The controller selects it
as soon as the directory recognizes a valid node and a connected link can carry
the command. The destination must pass this node's allow and deny lists. The
`*4` prefix and destination digits stay local. Do not add `#` to select the node:
once selected, `#` exits the mode.

Your next digits go to the selected destination. A direct link is used first;
otherwise connected links relay the addressed digits. Only the destination
handles them as commands or eligible tones. Relay nodes do not run the commands
or play the tones. The destination policy is checked before each digit is sent.

You can unkey and key again or pause between transmissions without leaving the
selected mode. Send an actual `#` to leave it. That `#` stays local and is not
sent to the destination.

Incoming digits must pass the current incoming-link access rules. A linked
peer's claimed source name does not prove its identity. Command collection and
admin unlocks stay separate for each source and connection; one source cannot
use another's unlock. An outbound audio link may remain connected after an
access-list change even when its incoming DTMF is blocked. DTMF start events and
malformed end events are ignored.

### Check a radio keypad

1. Key your radio into the local receiver and send `*82`.
2. Keep the radio keyed and press the keys you want to test. All 16 symbols are
   accepted: `0` through `9`, `*`, `#`, and `A` through `D`.
3. Release PTT to end the test. Listen for the digits on the local transmitter.

The test needs no admin unlock and cannot be started by a linked peer. Change
`link_command_pad_test` in `[general]` or a node section to use a different code;
leave it empty to disable the test.

After `*82`, every key is part of the test. Even `#` is read back as a symbol;
it does not end the test. A pause or command timeout does not end it either.
Captured keys cannot run other commands. Only the local receiver's actual
unkey ends capture.

The reply uses the node's speech settings, with Morse as a fallback. It reads
each symbol in order, saying "star" for `*` and "pound" for `#`. An empty test
says "no digits". The test keeps the first 127 symbols. If you send more, the
reply reports overflow before reading the saved symbols. Reload, loss of the
receive source, or lost digit events cancels an unfinished test without a reply.

### DTMF regeneration

Set `dtmf_regeneration_enabled = yes` to play eligible link digits addressed to
this node on its local transmitter. It defaults to `no`. These settings inherit
from `[general]` and may be changed in each node section. Invalid values give a
warning and use the inherited valid default.

Only digits outside a collected command are eligible. The command-start `*`
and the rest of that command are not played as tones. Digits passing through
to another node and ordinary local RF digits are not regenerated.

An idle tone queue waits 500 ms before its first tone. Each tone then uses
`dtmf_regeneration_duration_ms`, followed by `dtmf_regeneration_gap_ms` of
silence. Tone length does not follow how long the sender held the key. Playback
does not wait for the sending station to unkey, but half-duplex limits,
transmitter timeout, and other transmitter safety limits still apply.

The tone and its gap replace local voice and announcement audio for that time.
The interrupted audio continues to advance and is not played back later. The
generated tones are not sent to linked peers or recorded by parrot.
`dtmf_regeneration_level_dbfs` sets the combined peak of the two equal-level
tones. Transmitter processing and hardware gain still affect the final level;
speech gain and telemetry ducking do not control these tones.

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

Use only the substitutions below. Unknown or broken substitutions stop a reload.
The longest possible rendered message must fit 127 bytes; a longer one is
rejected rather than cut short.

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
Macros run only these link actions; they do not run shell commands.

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

Use the host's local time. Seconds, time zones, ranges, aliases, and cron
expressions are invalid. A repeated minute at a daylight-saving change runs
once; a skipped minute does not run. Missed events are not replayed. A `once`
event can run again if the controller restarts during its scheduled minute.

When an event has both a message and a macro, it queues the message first, then
runs the macro without waiting for playback. Reload may cancel a queued or
playing scheduled message. An event with the same node, label, and `at` time
keeps its completed state across reload; renaming it makes a new event.

## Configuration-owned permanent links and replacement windows

`[permanent node label]` declares one configuration-owned transceive direct
peer. It has no global form and does not inherit options. The configured intent
is rebuilt at module start and after a successful configuration reload, so it
is distinct from a link created by DTMF, CLI, or a macro. The peer continues to
use the normal permanent-link recovery policy while its configuration remains
active.

| Option | Required | Meaning |
| --- | --- | --- |
| `remote_node` | yes | One decimal node identity, or a comma-separated priority list of permanent group members. Each must differ from the local node and every other configured permanent or schedule route for that node. The first reachable member has highest priority. |
| `group_name` | empty | Optional display name for a priority group, used in its selection/unavailable telemetry. |

When `remote_node` has multiple members, rpt_advanced keeps each member
connected independently. Only the highest-priority reachable member is
transceive; all other group members are receive-only and their audio is not
forwarded. A recovered higher-priority member takes over once local and linked
inputs are idle; the former winner then becomes receive-only. Retries continue
silently. The group is announced only when a member is first selected or when
all members become unavailable—not for retries, standby connections, or later
winner changes. A one-member list is valid and behaves as an ordinary permanent
link.

`[schedule node label]` temporarily replaces one same-node configured
permanent peer with another direct peer during a bounded local-time window. It
also has no global form or inherited options. `replace_permanent` is the exact
label from a `[permanent node label]` section for the same local node. The
replacement `remote_node` must differ from both the local node and the
permanent peer being replaced.

Declarations whose node is disabled have no running routing effect. They remain
validated configuration and become active only when that node is enabled on a
later reload.

| Option | Default | Meaning |
| --- | --- | --- |
| `remote_node` | required | Decimal identity of the replacement direct peer. It must differ from the local node, the replaced permanent peer, and every other configured permanent or schedule route for that node. |
| `group_name` | empty | Optional display name for this replacement route, up to 63 bytes. |
| `replace_permanent` | required | Label of the same-node configured permanent link to suspend while this window is requested. |
| `days` | empty | Comma-separated full weekday names or inclusive ranges, such as `Monday-Friday` or `Tuesday,Thursday`. Matching is case-insensitive. Empty selects every calendar day unless `dates` is set. |
| `dates` | empty | Up to 64 comma-separated local Gregorian dates in exact `YYYY-MM-DD` form. It cannot be combined with `days`. |
| `start_time` | required | Exact 24-hour local `HH:MM` inclusive window start. |
| `end_time` | required | Exact 24-hour local `HH:MM` exclusive window end, later than `start_time` on the same date. `24:00` is accepted only here to mean the end of the selected date. Overnight windows are invalid. |
| `end_inactivity_ms` | `0` | Post-window quiet interval in milliseconds. Zero restores the replaced permanent peer when the window ends. A nonzero value keeps the replacement after observed local-receiver or linked-peer activity until that activity has been quiet for this interval. |
| `warning_before_start_ms` | absent | Optional comma-separated positive millisecond lead times for warnings before each start. The configured order is retained. |
| `warning_before_end_ms` | absent | Optional comma-separated positive millisecond lead times before the expected disconnect. For inactivity-based disconnects, each warning is based on the current expected inactivity deadline. |
| `warning_message_id` | absent | Fluent catalog message ID for warnings. Required with any valid warning lead; currently `scheduled-link-change`. Warnings remain disabled when omitted. |

Warnings use the selected `warning_message_id` and include time remaining.
Active local or linked receive skips a due warning. Warnings do not interrupt
active audio.

When a window becomes active, rpt_advanced detaches its named permanent peer
before attaching the replacement. When the window ends, it detaches the
replacement before restoring the named permanent peer. With nonzero
`end_inactivity_ms`, the schedule no longer requires the replacement at
`end_time`, but it remains connected until the quiet interval expires.
Local or linked receive activity can defer that handoff; IDs and other transmit
audio cannot. On startup during a quiet interval after a window, only the
remaining quiet time is kept. A quiet interval that crosses midnight is not
restored after startup.

Startup and reload check the host's local time to choose active windows. Reload
does not redial an unchanged link. `*806` pauses configured link attachment and
retry across reload; `*816` resumes links under the current schedule.

Multiple replacement windows are accepted, including windows that overlap.
They form a union: every matching or deferred replacement is requested and its
named primary is suppressed. Normal direct-link topology admission is the only
conflict gate, so do not rely on an overlap to select one replacement route.

```ini
[permanent 524950 primary]
remote_node = 506315
group_name = The Blind Hams Network

[schedule 524950 weekday_net]
remote_node = 2627
replace_permanent = primary
days = Monday-Friday
start_time = 11:00
end_time = 12:00
end_inactivity_ms = 300000
warning_before_start_ms = 3600000,1800000,900000,600000,300000,60000
warning_before_end_ms = 600000,300000,60000
warning_message_id = scheduled-link-change
```

For a single permanent peer, use a one-entry `remote_node` value; `group_name`
is optional. For example:

```ini
[permanent 524950 primary]
remote_node = 506315
```

## Localized built-in messages

`[general] language` selects the shared message locale and a node's `language`
overrides it. Standalone ships English at
`/usr/share/rpt-advanced/messages/en-US.ftl`. Put local message files in
`/etc/rpt_advanced/messages/<locale>.ftl`. For Asterisk, the paths are
`/usr/share/asterisk/rpt_advanced/messages/en-US.ftl` and
`/etc/asterisk/rpt_advanced/messages/<locale>.ftl`.
An absent translation falls back to English independently for text, speech,
and Morse. An invalid localized entry also falls back for that entry. Invalid
required English content rejects a candidate reload, preserving the active
configuration and catalog. Built-in wording is edited in the selected FTL
bundle, not embedded in source or copied into each node section.

You may also add your own catalog entries. For example, add this entry to your
local language file:

```ftl
site-battery =
    .text = Battery
    .tts = Battery voltage
    .morse = BATTERY
```

Use `label_message_id = site-battery` in a sensor, or select a catalog entry
with a message section's `message_id`. Keep separate `.text`, `.tts`, and
`.morse` forms. Use `{ $argument }` in a form to insert its `arg_argument`
binding. Morse text must use characters the Morse player supports.

## Link lifetime, recovery, and duplex

Direct links created by DTMF, a CLI command, or a macro last only while the
service or Asterisk module runs. A `[permanent node label]` entry restores its
link at startup and after reload. If a permanent link fails, it retries at
once, then after one second, with longer waits up to five minutes.
`link_command_disconnect_permanent` cancels the retained retry of a manually
created permanent link. Configuration-owned routes remain controlled by their
current policy and are restored on the next scheduler reconciliation; use
`link_command_disconnect_all` and `link_command_reconnect_all` to hold and
resume those routes together.
DTMF `*806` disconnects and pauses permanent links only. DTMF `*816` resumes
their retries under the current schedule. Temporary links stay connected.
By contrast, a scheduled macro with `action = disconnect_all` disconnects all
links and saves them for `action = reconnect_all`.

With `full_duplex = yes`, local receive and transmit may operate at the same
time. With `full_duplex = no`, local receive is not repeated and transmission
is held off while the local receiver is active. Received link audio remains
eligible for transmission when half duplex permits it. IDs and RF status replies
that become due during local reception wait until reception ends.

## Link status

Status, last-keyed, and full-status replies use the node's speech settings and
Morse fallback. A local command replies on local RF. A command from a linked
peer replies only to that peer. Local replies wait 250 ms after receiver unkey
and wait longer if half duplex prevents transmission.

Link lifecycle telemetry names only the remote endpoint when this node is a
link endpoint, such as `123 CONNECTED`. For an event between two other nodes,
it says `node 1 CONNECTED TO node 2` or `node 1 DISCONNECTED FROM node 2`.

When its status reply is queued, full status writes a best-effort
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

The node rejects self-links, duplicate direct links, and routes that would make
a known loop. An incoming peer that advertises such a loop is disconnected.
For a local rejected request, RF status says `LINK REJECTED TOPOLOGY LOOP`.

## Identifier settings

With several `first_key_only = yes` sets, each qualifying key-up selects one.
The controller takes turns through the sets and wraps after the last one.
It skips a set whose idle interval has not elapsed. A pending initial ID keeps
its turn while waiting to play, unless a higher-priority ID satisfies it.
Periodic IDs still use their configured priority.
A lower-priority periodic ID waits for a pending polite initial ID. Playing
the initial ID also satisfies the lower-priority ID sets.

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

IDs also accept the message `content`, `message_id`, and `arg_*` settings below.

## Composed messages

Use `[message name]` to define a reusable message and `[message node name]`
to override it for one node. A message can contain words, a recording, tones,
and other messages in any order. IDs and release tails can use the same content.

| Option | Default | Meaning |
| --- | --- | --- |
| `content` | `{message}` | Ordered text and references. IDs and tails default to `{media}`. |
| `message_id` | empty | Message name from the selected Fluent language file. |
| `arg_name` | absent | Source for the Fluent argument called `name`. |
| `dtmf_id` | absent | Positive decimal selector for this named message; no leading zero. |

Content accepts these references:

| Reference | Plays |
| --- | --- |
| `{message}` | The selected `message_id`, with its `arg_*` values. |
| `{media}` | This section's sound file, speech, and Morse fallback. |
| `{tone_sequence}` | This section's `tone_sequence`; use the courtesy-tone segment syntax. |
| `{message_macro_name}` | The named message `name`. |

Write `{{` or `}}` for a literal brace. Messages may refer to other messages,
but cannot refer back to themselves in a loop. Keep each text, speech, or Morse
part within 127 bytes. A composition may contain several such parts.

Each message uses the normal `sound_file`, `speech_*`, and `morse_*` settings.
A recording or spoken part needs a usable Morse fallback. During local or
linked receive, playback changes to the complete Morse version. Attention
tones may appear before, between, or after words. Separate media settings in a
referenced message apply to that message's part.

An `arg_*` value may be `callsign`, `node`, `date`, `time`, `day_of_week`,
`greeting`, `link_status`, `sensor:name`, or `input:name`. Do not use
`time_remaining` in IDs, tails, or ordinary message commands; it needs a
deadline, such as the one supplied to a scheduled warning. Node numbers are spoken as digits;
meter readings are spoken as numbers. Values are read when playback is admitted.
A missing or stale site reading is reported as unavailable.

For example:

```ini
[message clock]
message_id = site-time
arg_time = time
dtmf_id = 1

[message temperature]
message_id = site-temperature
arg_temperature = sensor:outdoor

[identifier 524950 regular]
content = This is the KG0BP repeater. {message_macro_clock} {message_macro_temperature}
```

Send `*771#`, then unkey, to hear message 1 with the default command map.
Message and site selectors do not recall a linked node when `0` is entered.
These read-only commands need no administrative unlock. A local RF request
replies on the local transmitter. A linked request replies only to its sender.

## Release tails

Define each tail with `[tail node name]`. `[tail]` supplies shared defaults;
`[tail node]` changes the defaults for one node. Use the ID media settings or
message content above for what to play.

| Option | Default | Meaning |
| --- | --- | --- |
| `interval_ms` | `0` | Minimum time between successful plays. Zero plays after each qualified transmission. |
| `delay_ms` | `0` | Delay after the transmitter unkeys. Zero continues the current transmission after hang time. |
| `content` | `{media}` | Tail message content. |

With zero delay, the tail starts when hang time ends and keeps the transmitter
keyed. With a positive delay, the transmitter unkeys after hang time, waits,
then keys to play the tail. A tail with an interval waits for a later traffic
release; it does not key the transmitter just because its interval elapsed.

Tails play in configured order. A pending polite ID follows the tails unless
its maximum wait has expired, in which case the ID plays first. New receive
before a tail starts cancels that release opportunity. New receive during
playback changes it to Morse. Failed or cancelled playback does not start the
interval timer. IDs and other telemetry do not trigger another tail.

## Site meters, inputs, outputs, alarms, and fans

Define each item as `[sensor node name]`, `[input node name]`,
`[output node name]`, `[alarm node name]`, or `[fan node name]`.
Use the family alone, such as `[sensor]`, for shared defaults. Add a node,
such as `[sensor 524950]`, for that node's defaults. Only a named section
creates an item. Each family defaults to `enabled = no`.

Set `backend`, `device`, and `channel` or `line` in each hardware item's named
section. These fields and `dtmf_id` do not inherit. Sensor and input names and
their DTMF selectors must be unique within a node. Each selector is a positive
decimal number with no leading zero. Choose the exact device;
the controller will not substitute another device if it is absent.

| Backend | Use | Device and channel |
| --- | --- | --- |
| `hwmon` | Meters and supported PWM outputs | Exact kernel hwmon device directory. A sensor uses a channel such as `temp1_input`; an output uses a line such as `pwm1`. |
| `iio` | Analog meters | Exact IIO device directory and channel stem, such as `in_voltage0`. |
| `gpiod` | Digital inputs and outputs | GPIO chip path and numeric line offset. |
| `cm119` | Spare radio GPIO | `device` is the node that owns the radio; `line` is GPIO 1–8, subject to availability. |
| `parallel` | Spare radio parallel-port pins | `device` is the owning node; output pins are 2–9 and input pins are 10, 12, 13, 15. |

Radio PTT, tone-control, interlock, and clipping-indicator pins cannot also be
site outputs. A GPIO output is on/off only. PWM needs a device that provides
PWM and is already in manual mode.

### Meters

| Option | Default | Meaning |
| --- | --- | --- |
| `label_message_id` | empty | Localized meter label; empty uses its name. |
| `unit` | empty | Reported unit, such as `volts`, `amps`, `celsius`, `watts`, `joules`, `rpm`, or `percent`. |
| `precision` | `1` | Decimal places in reports, from 0 through 15. |
| `poll_interval_ms` | `1000` | Time between reads; greater than zero. |
| `stale_after_ms` | `5000` | Reading age limit; greater than the polling interval. |
| `smoothing_ms` | `0` | Smoothing time; zero disables smoothing. |
| `calibration` | `none` | Use `none` or `two_point`. |
| `input_low`, `input_high` | absent | Two distinct readings in the device's source units. |
| `value_low`, `value_high` | absent | Engineering values for those two readings. |
| `dtmf_id` | absent | Positive decimal selector for `*76`. |

Calibration is applied once. For hwmon, use the units reported by Linux, such
as volts or degrees Celsius. Report precision does not round alarm thresholds
or stored readings. Status includes current, smoothed, minimum, and maximum
values. A failed read does not become a zero reading.

### Digital inputs and outputs

| Option | Applies to | Default | Meaning |
| --- | --- | --- | --- |
| `active_level` | Both | `high` | Physical active level: `high` or `low`. |
| `debounce_ms` | Input | `50` | Time a new state must remain stable. |
| `dtmf_id` | Input | absent | Positive decimal selector for `*76`. |
| `on_active`, `on_inactive` | Input | empty | Actions when the input changes state. |
| `startup_state` | Output | `off` | Initial logical state on first device acquisition. |
| `safe_state` | Output | `off` | Logical state on fault or shutdown. |
| `pulse_ms` | Output | `500` | Pulse duration; greater than zero. A pulse returns to the prior state. |

### Alarms

| Option | Default | Meaning |
| --- | --- | --- |
| `source` | empty | Named sensor or input. |
| `condition` | `unavailable` | `above`, `below`, `active`, `inactive`, or `unavailable`. |
| `trip_value`, `clear_value` | absent | Sensor trip and recovery levels. For `above`, clear must be no higher than trip; for `below`, clear must be no lower. |
| `reading` | `current` | Use the `current` or `smoothed` sensor value. |
| `trip_delay_ms`, `clear_delay_ms` | `0` | Time the trip or recovery condition must hold. |
| `latched` | `no` | Keep the alarm active until it has recovered and is reset. |
| `trip_message`, `clear_message`, `repeat_message` | empty | Named messages to play on trip, recovery, or repeat. |
| `repeat_interval_ms` | `0` | Time between repeat notices; zero disables repeats. |
| `on_trip`, `on_clear` | empty | Ordered action lists. |

Acknowledging an alarm silences its repeats. It does not clear an unsafe
condition. Reset succeeds only after the source has recovered. Alarm actions
run even when the telemetry queue is full. A lost reading stays unknown and
cannot prove that the alarm has recovered.

### Fans

| Option | Default | Meaning |
| --- | --- | --- |
| `output` | empty | Named output controlled by this fan. |
| `mode` | `ptt` | `ptt`, `temperature`, or `combined`. Combined uses the greater demand. |
| `ptt_nodes` | Owning node | Comma-separated nodes whose PTT runs the fan. |
| `run_on_ms` | `120000` | Fan run-on after PTT drops. |
| `temperature_sensor` | empty | Named temperature sensor. |
| `on_temperature`, `off_temperature` | absent | On and off levels for temperature control. Off must be no higher than on. |
| `temperature_curve` | empty | Ordered `temperature:duty` points, such as `30:0.2,50:1`. Duty is 0–1 and needs PWM. |
| `rpm_sensor` | empty | Optional speed sensor. |
| `minimum_rpm` | absent | Required positive speed when feedback is used. |
| `spinup_ms` | `1000` | Time allowed for the fan to reach speed. |

A fan owns its output. Do not also command that output directly. Missing
required temperature requests full cooling. Manual fan demand cannot reduce
protective cooling. An unavailable output or insufficient fan speed is a fault.

### Site actions and commands

Write comma-separated actions in `on_active`, `on_inactive`, `on_trip`, or
`on_clear`:

| Action | Meaning |
| --- | --- |
| `output:name:on`, `output:name:off`, `output:name:pulse` | Change a named output. |
| `fan:name:auto`, `fan:name:0.75` | Restore automatic demand or set a duty from 0 to 1. |
| `inhibit:on`, `inhibit:off` | Set or clear this source's transmit inhibit. |
| `macro:name` | Run a configured action macro. |

Send `*76<dtmf_id>#`, then unkey, to hear a meter or input. The reply goes to
the requesting RF source. DTMF can read site status but cannot configure meters
or operate outputs.

The Asterisk CLI provides these site controls:

```text
rpt_advanced link status <node>
rpt_advanced site output <node> <name> {on|off|pulse}
rpt_advanced site alarm <node> <name> {acknowledge|reset}
rpt_advanced site sensor <node> <name> reset-extrema
rpt_advanced site fan <node> <name> {automatic|duty-from-0-to-1}
rpt_advanced site inhibit <node> {on|off}
```

An accepted request is not proof of the electrical output level. Link status
shows requested and submitted values, reading age, and faults. Output readback
is shown as unavailable. Use a separate input or meter to check the circuit.
Clearing manual transmit inhibit does not clear an alarm's inhibit.

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
| `level_db` | 0 | Speech gain from -60 through 0 dB. It does not change sound-file or Morse level. |

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
| `frequency_hz` | 800 | Morse tone frequency in Hz, greater than zero and below half the playback rate (24,000 Hz for 48 kHz audio). |
| `speed_wpm` | 20 | PARIS words per minute, from 1 through 100. |
| `level_db` | -6 | Morse-tone level from -60 through 0 dBFS. |

Named identifier and announcement sets override these with `morse_frequency_hz`,
`morse_speed_wpm`, and `morse_level_db`.

Switches accept `yes` or `no`, ignoring case. Unsigned numbers use decimal
digits without signs or units. Audio levels use signed whole numbers in the
listed range.

No ID or announcement media is configured by default. Set `voice` to a local
Piper model path when using speech.

Morse accepts letters, digits, and `/ . , ? - = + @ ( ) ' ! " : ; _ $ &`.
Spaces, tabs, and line breaks separate words. Other characters are rejected.

If a sound file or speech cannot play, the next configured source is used. A
set with no playable audio and no Morse text is not scheduled.
