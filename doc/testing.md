# Testing

Run `make ci` in the ASL3 development environment. It runs static checks,
Doxygen, unit tests, line/branch coverage, staged installation, and an isolated
Asterisk process. The test-only radio does not access USB devices and is not an
installed artifact. It verifies two simultaneous nodes, half/full duplex, Morse
output, PTT cleanup, and native 48 kHz, 16 kHz linear, and 8 kHz mu-law transport.

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
