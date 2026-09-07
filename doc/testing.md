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
6. Repeat transport checks with an explicitly supported converted rate and
   codec. Compare receive and transmitted audio for continuity. Run a sustained
   receive/repeat test, recording duration and USBRadioPlus queue/error counters
   before and after. Inspect for underruns, overruns, gaps, and growing latency;
   shared hardware pacing does not guarantee immunity to scheduling stalls.
7. Reload invalid configuration while active and verify the old settings still
   operate. Restore valid configuration and reload; allow for the documented
   media-preparation pause. Unload normally and verify PTT drops and the radio
   channel closes. Load again without restarting Asterisk and repeat reception.
8. If multiple radios are available, configure independent nodes and scoped ID
   overrides. Verify audio, PTT, and IDs stay on their assigned radios while
   flat defaults still apply to settings not overridden.

Record module revisions, OS/architecture, USB interface, radio wiring, selected
codec/rate, configuration, measurements, and any failed step. These procedures
are acceptance criteria, not a claim that physical testing has been performed.
