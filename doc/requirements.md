# Controller requirements

Implementation is original. app_rpt documentation and code may be studied for
behavior and Asterisk interfaces, but no app_rpt implementation is copied.

## Nodes and radio operation

Configuration reload and module unload/load must not require an Asterisk restart.
Keep this requirement unless a concrete technical limitation makes it impractical
and the user agrees to a change. Failed configuration reloads retain the active
configuration. Module unload must release owned channels, workers, and resources
so a replacement module can load into the same running Asterisk process.

There is no configured-node count limit. Each named node has one receiver and
one transmitter. Flat configuration sections supply shared defaults; scoped
node sections override them. ID sets inherit from node ID defaults, which
inherit from shared ID defaults. Explicit empty ID text or paths clear an
inherited value.

Use USBRadioPlus through a thin compatibility adapter. Changes to USBRadioPlus
are restricted to that adapter and the integration needed for it; its existing
app_rpt behavior is preserved. Discover Asterisk's available audio formats and
translation capabilities at runtime and specify the selected PCM sample rate
to USBRadioPlus. Do not hard-code a list of codecs or sample rates.

Full duplex permits simultaneous reception and transmission. Half duplex does
not repeat local receive audio and does not transmit during reception. An ID
that becomes due during half-duplex reception waits until reception ends.
Transmit hang time is configurable. Link connectivity is not implemented in
this scope; a later link source may transmit in half duplex.

## Identification

An arbitrary number of ID sets may be configured; zero is valid. Each set has
an interval and priority. When several sets are due, select the highest-priority
set. Its successful completion satisfies that set and sets of lower priority.
Higher numeric priorities win; equal priorities use configuration order.

Each periodic set chooses activity-based or unconditional timing. Activity-based
sets identify for activity since their last satisfied period and do not continue
identifying throughout inactivity. Generated IDs and transmitter hang time do
not themselves count as conversation activity.

A first-key-only set identifies on the first transmitter key following at least
one full interval without activity. It is not also a periodic set. This supports
a welcome message separately from conversational IDs. Runtime uses monotonic
time so wall-clock changes do not alter intervals.

Playback preference is an explicitly named sound file, then offline speech,
then Morse. A missing sound file falls back to speech. Missing speech text or
an unavailable/failed synthesizer falls back to Morse. There is no fallback
beyond Morse. If receiving in full duplex, select Morse directly. Reception
interrupts file/speech playback and replaces it with that set's Morse ID.

Speech and Morse have configured text and speed; Morse also has a per-set tone
frequency. Piper is the default offline speech engine behind a replaceable
adapter. No other engine is implemented now.

The default Piper voice is `en_US-lessac-medium`; its local model path is
configurable. Testing on 524950 may use the already installed
`/usr/lib/piper-tts/voices/en_US-amy-low.onnx` without installing another voice.
Automatic sample-rate selection chooses the highest mutually supported rate up
to the detected hardware-native rate. CM119 is the initial supported hardware.
Explicit rates remain supported where Asterisk provides the required conversions.

## Reference material

- [app_rpt configuration](https://allstarlink.github.io/config/rpt_conf/)
- [app_rpt source](https://github.com/AllStarLink/app_rpt/tree/6966503d14cefb49a5bd269edb8524e549de0a85)
- [Asterisk architecture](https://docs.asterisk.org/Fundamentals/Asterisk-Architecture/Asterisk-Architecture-The-Big-Picture/)
- [Piper CLI](https://github.com/OHF-Voice/piper1-gpl/blob/main/docs/CLI.md)

Reference review is not a claim of an independent clean-room process. Source
code and tests in this repository must be newly written from these requirements.
