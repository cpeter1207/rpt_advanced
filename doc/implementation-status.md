# Capabilities and limits

`rpt_advanced` controls a repeater and its AllStarLink-compatible links. It is
alpha software for Debian 13 amd64 and arm64. Choose either the standalone
`rpt-advanced` service or the optional `app-rpt-advanced` Asterisk adapter.
The packages install separately. See
[installation](install.md) before choosing a path.

## Radio operation

The controller uses 48 kHz audio at the local radio. It handles receiver audio,
PTT, transmit hang time, time-out and lockout, local repeat, and half or full
duplex. It can play identifiers, announcements, courtesy tones, and scheduled
messages. Media may come from a sound file, Piper speech, a generated tone, or
Morse, according to the configured set. Piper and its voice model must be
installed separately for speech; sound files use FFmpeg. Configure the node,
radio, signaling, and media in the [configuration reference](configuration.md).

When several initial IDs are configured, the controller plays one per
qualifying key-up and takes turns through the eligible sets. Message content
can combine words, sound files, tones, and named messages. Release tails can
continue the transmission after hang time or rekey after a delay. Configure
these under [identifier settings](configuration.md#identifier-settings),
[composed messages](configuration.md#composed-messages), and
[release tails](configuration.md#release-tails).

Messages can run once at a local date and time, daily, on selected weekdays,
or at stepped minute/hour boundaries. Each scheduled message waits for quiet
inputs up to its configured start deadline, then skips or selects Morse.
Messages never overlap. Assign `dtmf_id` to a message, ID, tail, or event to
play it with `*77<selector>#`; use `*78` to cancel the current message.
See [scheduled messages](configuration.md#scheduled-messages-and-macros).

The standalone service uses local audio devices and CM119 signaling through the
separately packaged USBRadioPlus radio runtime. Configure `[radio]` or
`[radio <node>]` and enable the node before starting it. The Asterisk adapter
uses a `RadioPlusAdvanced` channel supplied by USBRadioPlus; it also needs the
ASL3 dialplan and IAX setup for incoming calls. Do not assign the same radio
to both paths at once. The [installation guide](install.md) gives the commands
for each path.

## Links and control

Direct IAX2 links support transceive, monitor, and local-monitor modes. Local
DTMF controls connecting, disconnecting, status, and remote-command mode.
Verified node identity and the configured allow and deny lists control incoming
links. Permanent links can reconnect; configured permanent links can return
after startup. The controller can also run local-time events and configured
link macros. See [AllStarLink links](allstarlink-status.md) for operating
commands and [configuration](configuration.md) for settings.

The local `*82` keypad test reads back captured keys after unkey. Remote-command
mode (`*4`) sends addressed digits directly or through relays and stays selected
until `#`. Optional DTMF regeneration plays eligible incoming link digits only
on the local transmitter; it is off by default. See the
[DTMF settings and commands](configuration.md#link-command-mappings).

Local DTMF status replies use local RF. A linked peer's status request replies
only to that peer after it unkeys. On the Asterisk path, the Asterisk CLI can
show direct-link status and send an administrative DTMF command. The standalone
service does not offer those CLI commands; use its service status and journal
for process checks.

## Site meters and control

Named sensors can report voltage, current, temperature, and other readings
from Linux hwmon or IIO devices. Digital inputs and outputs can use spare radio
GPIO, a parallel port, or Linux GPIO lines. Configure alarms, transmit inhibits,
output pulses, and fan cooling in the [site settings](configuration.md#site-meters-inputs-outputs-alarms-and-fans).

Read meters or inputs with `*76<selector>#` and play named messages with
`*77<selector>#`, then unkey. The Asterisk CLI shows reading age and quality,
and provides output, fan, alarm, and manual-inhibit controls.

## Check receive audio with parrot

Set `parrot_enabled = yes` for the node, reload, then make a short transmission.
After all receive activity ends, the controller speaks the peak and RMS levels
in whole dBFS, then plays back the recording. It keeps the first 30 seconds at
48 kHz. If speech is unavailable, it plays only the recording.

New receive activity cancels a waiting or playing response. Set
`parrot_enabled = no` and reload to turn it off. The `*804` and `*805` commands
can also enable and disable it when admin DTMF is unlocked; see
[command settings](configuration.md#link-command-mappings).

Parrot playback also goes to linked peers. Do not link two parrot-enabled nodes:
they may replay each other's recordings.

## Before operating

The supplied example has no enabled radio node. Set up the radio, check the
configuration, and verify receive audio, transmit audio, PTT release, IDs, and
links on the intended hardware. Peer codec and higher-rate behavior still need
checking with the peers you plan to use. A package build or a successful
configuration check does not establish RF or peer performance.

For standalone, run `rpt-advanced --check-config /etc/rpt_advanced/rpt_advanced.conf` and follow the
[service instructions](install.md#set-up-standalone). For Asterisk, follow
the [adapter instructions](install.md#set-up-the-asterisk-controller) and reload the
module after a configuration change. Invalid reloads leave the active
configuration in place. See [configuration](configuration.md) for all values
and [testing](testing.md) for acceptance checks.
