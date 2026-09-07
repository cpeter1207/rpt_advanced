# Configuration model

The settings resolver, streaming syntax reader, and owned document builder are
implemented. Node enumeration, whole-file schema validation, and the running
module are still under development. These are the supported resolver settings.

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
