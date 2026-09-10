# Testing

Run `make ci` in the ASL3 development environment. It runs static checks,
Doxygen, unit tests, line/branch coverage, staged installation, and an isolated
Asterisk process. The test-only radio does not access USB devices and is not an
installed artifact. It verifies two simultaneous nodes, half/full duplex, Morse
output, PTT cleanup, and native 48 kHz, 16 kHz linear, and 8 kHz mu-law transport.
An additional native-rate case prepares a WAV identifier through FFmpeg and
observes its recognizable PCM at the transmitter. The synthetic receiver then
asserts carrier; negative Morse samples in the otherwise positive receive audio
verify replacement of the prepared ID by its Morse fallback inside Asterisk.
Controller sequence tests also cover polite-ID deferral, every-release and
positive-interval announcements, identifier priority, idle announcement keying,
and receive-active announcement ducking. The Asterisk integration additionally
verifies that a periodic announcement waits through synthetic half-duplex
receive and emits from the hardware-paced idle silent frames after it clears.
Courtesy tests cover named receiver, generic-link, and permanent-peer routing;
source-specific rekey cancellation; generic fallback; and pre-rendered mono,
dual-tone, silence, level, syntax, duration, Nyquist, and bounded-length cases.
The `integration` portion also starts isolated Asterisk processes and exercises
the local IAX link implementation. It is a controlled source-level test: it
does not prove interoperability with a classic `app_rpt` node, authorize live
link activation, or establish radio hardware behavior. The current AllStarLink
changes require a fresh complete quality run before a merge or release.

To exercise USBRadioPlus's actual adapter against the synthetic hardware backend,
use a clean build directory and run
`make integration USBRADIOPLUS_SOURCE=/absolute/path/to/USBRadioPlus`.
The fixture links the adapter as a separate object from that checkout; it does
not copy it into rpt_advanced or alter USBRadioPlus. Start from a clean build
when switching fixture modes. This checks the real adapter's reservation and
audio callbacks, but not the USB hardware backend. Run the same required quality
gate after changing either project; a historical fixture result is not evidence
for the current source revision.

## Prebuilt test images

Public images are available for Debian 12 and 13, each containing native amd64
and arm64 variants:

| Image under `ghcr.io/cpeter1207/` | Purpose |
| --- | --- |
| `rpt-advanced-asl3-debian12` | Clean Debian 12 ASL3 installation |
| `rpt-advanced-asl3-debian13` | Clean Debian 13 ASL3 installation |
| `rpt-advanced-installed-debian12` | Debian 12 with the tested module and integration fixture |
| `rpt-advanced-installed-debian13` | Debian 13 with the tested module and integration fixture |

Tags are the full production commit hash and `latest`. Use the commit tag for
reproducible tests. The installed image derives from its clean ASL3 image and
tests the installed module, not a replacement compiled at container startup.
Its default command runs the isolated Asterisk audio and reload tests:

```sh
docker run --rm --label rpt_advanced.test=true \
  ghcr.io/cpeter1207/rpt-advanced-installed-debian13:latest
```

No USB device, host network, or privileged access is needed. Use the host's
native architecture; the CI matrix uses native runners, not QEMU. Publication
requires the production quality gate and clean/installed checks on all four
platforms before creating the multiarch tags. The fixture is only in the test
image; `make install` does not install it.

## Real Piper voice

The ordinary process tests use a fixture synthesizer and real FFmpeg. To also
exercise an installed Piper executable and local voice model:

```sh
make build/test_speech_process
RPT_TEST_PIPER_MODEL=/path/to/voice.onnx ./build/test_speech_process
```

Put `piper` on `PATH`, with its model's adjacent `.onnx.json` file available.
Piper's [installation instructions](https://github.com/OHF-Voice/piper1-gpl)
describe installation of `piper-tts`. The test restores the original `PATH`
after its fixture checks, then prepares real speech through the module's media
preparation code. It checks nonempty 48 kHz PCM and reports sample count and peak.

The existing `en_US-amy-low` model copied read-only from 524950 was tested with
Piper 1.8.0 on Debian 13 amd64. The phrase “This is the KG0BP repeater.” produced
136,704 samples at 48 kHz. This test did not change or transmit from the node.

Synthetic-radio results do not establish USB hardware performance, radio
deviation, or on-air audio quality. Those require separately approved hardware
testing. No test command here activates the module on a running radio node.

## Hardware acceptance procedure

Run this only with the station owner's approval. Save the installed modules and
configuration first, use a dummy load, and follow [activation](install.md).
Do not give app_rpt and rpt_advanced ownership of the same radio. Use a receiver
or service monitor to observe transmitted audio and transmitter release.

1. Begin with one node, automatic sample rate, full duplex, zero hang time, and
   no ID sets. Confirm a `RadioPlusAdvanced` channel is open. With the receiver
   quiet, verify no unintended PTT. Apply carrier and voice: verify local repeat
   and prompt PTT release when carrier ends.
2. Set a measurable hang time, reload, and measure transmitter release after
   carrier ends. Switch to half duplex and reload. Confirm local receive audio
   is not retransmitted and the transmitter remains off during reception.
3. Configure a short test ID interval and recognizable file, speech, and Morse
   text. During full-duplex idle, hear the file. Remove only its configured path
   and reload to verify speech; use an unavailable model to verify Morse.
   Restore the known-good media after each test. Do not change the legal station
   ID to an unrelated callsign.
4. Assert carrier during file and speech IDs. Confirm immediate replacement by
   Morse and no resumption of the interrupted recording after carrier ends.
   Hold carrier before an ID is due: full duplex must use Morse; half duplex
   must wait until reception ends before transmitting its ID.
5. Configure two distinguishable priorities with the same interval. Verify the
   higher-priority ID satisfies the lower one. Verify activity-based IDs stop
   during prolonged inactivity and unconditional IDs continue. For a
   first-key-only welcome, wait a full inactive interval, then key: verify one
   welcome and no periodic welcomes during conversation.
6. Enable a polite ID with a bounded wait while receiving or while status and
   courtesy telemetry is queued. Confirm it waits for the clear channel when
   possible, then becomes eligible at its configured limit. Configure distinct
   every-release and positive-interval announcements. Confirm they play after
   ordinary hang and any due ID, do not self-repeat, duck during renewed local or
   linked receive, and that a positive interval keys from idle before releasing
   with the short natural tail.
7. Configure named courtesy tones for `input = receiver`, generic `input = link`,
   and one permanent direct peer. Use a single-tone receiver sequence and a
   link sequence containing a dual tone, pause, and quieter final segment. On a
   service monitor, verify frequency, duration, and relative level. Verify an
   unmatched or temporary link uses the generic link tone, while the permanent
   peer uses its override. Rekey each source before its delay expires and verify
   that only its own pending courtesy tone is cancelled; rekey during a started
   tone should duck, not interrupt, that playback.
8. Repeat transport checks with an explicitly supported converted rate and
   codec. Compare receive and transmitted audio for continuity. Run a sustained
   receive/repeat test, recording duration and USBRadioPlus queue/error counters
   before and after. Inspect for underruns, overruns, gaps, and growing latency;
   shared hardware pacing does not guarantee immunity to scheduling stalls.
9. Reload invalid configuration while active and verify the old settings still
   operate. Restore valid configuration and reload; allow for the documented
   media-preparation pause. Unload normally and verify PTT drops and the radio
   channel closes. Load again without restarting Asterisk and repeat reception.
10. If multiple radios are available, configure independent nodes and scoped ID
   and announcement overrides. Verify audio, PTT, IDs, and announcements stay on
   their assigned radios while flat defaults still apply to settings not overridden.
11. Pending explicit approval and completion of the full platform quality gate,
   use an isolated test peer before a public node. Verify transceive, monitor,
   local-monitor, permanent-link recovery, disconnect-all/reconnect-all, remote
   `*4<node>` command mode with local `#` exit, allow/deny rejection, and the
   `*70`, `*72`, and `*73` status replies. Confirm that an Asterisk restart
   removes every link. Treat the CLI/log topology as best-effort: it can be
   incomplete or stale, and `R000000` means a bounded `L ` advertisement was
   truncated. Confirm self-links, duplicate direct links, retained permanent
   retries, and targets advertised by an attached peer are rejected with the RF
   loop-rejection telemetry. Do not perform this test against 524950 or another
   live node without separate approval.

Record module revisions, OS/architecture, USB interface, radio wiring, selected
codec/rate, configuration, measurements, and any failed step. These procedures
are acceptance criteria, not a claim that physical testing has been performed.
