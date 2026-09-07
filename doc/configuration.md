# Configuration model

Configuration reading, storage, whole-file validation, node/ID discovery, and
settings resolution are implemented. The running module is still under
development. These are the supported settings.

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
`[524950]` overrides them. Flat `[identifier]` settings provide ID defaults;
`[identifier 524950]` overrides them for that node, and
`[identifier 524950 welcome]` overrides them for one set. File order does not
change this precedence. Empty media paths or text clear inherited values.

Node and ID-set names are case-sensitive and cannot contain whitespace or square
brackets. Scoped headers use one space between components. `general` and
`identifier` are reserved flat-section names. ID scopes must name an existing
node, which may be declared later in the file. Repeated section headers merge
options without creating duplicate nodes or ID sets. Unknown options and invalid
values are rejected even if a later entry would override them. There is no fixed
limit on the number of nodes or ID sets.

## Node settings

| Option | Default | Meaning |
| --- | --- | --- |
| `node_enabled` | yes | Start the configured node. |
| `full_duplex` | yes | Allow simultaneous reception and transmission. |
| `transmit_hang_ms` | 0 | Hold PTT this many milliseconds after audio ends. |
| `sample_rate_hz` | 0 | Zero selects the highest mutually supported rate up to the hardware-native rate. An explicit rate requires a supported Asterisk conversion path. |
| `radio_channel` | node section name | USBRadioPlus channel identifier without `RadioPlus/`. |
| `codec` | empty | Empty selects signed linear automatically; otherwise select an available Asterisk codec. |

## Identifier settings

| Option | Default | Meaning |
| --- | --- | --- |
| `interval_ms` | 600000 | Positive ID interval in milliseconds. |
| `priority` | 0 | Nonnegative priority through 2147483647; higher wins. |
| `first_key_only` | no | Identify only on first key after at least one interval of inactivity. |
| `regardless_of_activity` | no | Continue periodic IDs during inactivity; not used by first-key-only sets. |
| `sound_file` | empty | File path; missing or unusable files fall back to speech. |
| `speech_text` | empty | Speech text; empty disables speech. |
| `speech_model` | en_US-lessac-medium.onnx | Local Piper model path. Missing models fall back to Morse. |
| `speech_speed_percent` | 100 | Speaking rate relative to model default, from 1 through 1000 percent. |
| `morse_text` | empty | Morse text; empty disables the terminal fallback. |
| `morse_speed_wpm` | 20 | PARIS words per minute, from 1 through 100. |
| `morse_frequency_hz` | 800 | Positive tone frequency, also required to be below the playback rate's Nyquist limit. |

Switches accept `yes` or `no`, ignoring case. Numeric values use decimal digits
without signs or suffixes. Sample rate and Morse frequency fit unsigned 32-bit
values; interval and hang time fit unsigned 64-bit values. Invalid typed values
do not partially update a resolved settings object. Runtime capability checks
are separate from numeric parsing.

No media is configured by default. Zero ID sets is valid. For testing on 524950,
set `speech_model` to `/usr/lib/piper-tts/voices/en_US-amy-low.onnx` to use the
installed voice without downloading another model.

The Morse renderer accepts ASCII letters (case-insensitive), digits, and
`/ . , ? - = + @ ( ) ' ! " : ; _ $ &`. Spaces, tabs, and line breaks separate
words. Unsupported characters are rejected. Timing uses standard 1-unit dots,
3-unit dashes, 1-unit element gaps, 3-unit character gaps, and 7-unit word gaps.
Generated PCM has half-scale tone amplitude; transmitter processing follows it.

File and synthesized audio preparation uses the `ffmpeg` executable to produce
mono PCM at the selected playback rate. Preparation runs outside the audio loop;
prepared samples are consumed at the radio hardware's cadence. A failed decode
or conversion is a playback-source failure and must follow the ID fallback order.
Preparation occurs when nodes start or reload. Each external process is limited
to thirty seconds before that source fails. Reload can therefore pause radio
operation while files or speech are prepared. Temporary files are removed after
preparation; playback uses immutable in-memory PCM. Sets with neither prepared
audio nor configured Morse text are omitted from scheduling.
