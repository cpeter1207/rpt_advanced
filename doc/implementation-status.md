# Implementation status

## Implemented and tested

- Original C identifier scheduling policy: intervals, priorities, activity-based
  and unconditional periods, first-key-only sets, and completion hierarchy.
- File/speech/Morse preference policy, including Morse-only reception behavior.
- Streaming signed-linear Morse renderer with configurable rate, speed, and tone
  frequency; fractional-sample timing and block-independent output tests.
- Sample-driven prepared-PCM playback with immediate, irreversible Morse fallback
  on reception. Completion remains terminal, and control events need not consume
  audio samples. This playback state is connected to radio transport.
- Asterisk frame-exchange boundary that sends one transmit block per received
  voice block, including silence. Carrier events change state without advancing
  audio. API-fixture tests cover ownership, malformed frames, and output failures;
  module startup now connects this exchange to per-node workers.
- Optional Asterisk codec conversion around linear controller processing, with
  buffered-conversion ownership tests. Native linear transport bypasses these
  converters.
- Exclusive RadioPlusAdvanced reservation with negotiated read/write formats and
  converter ownership. Failure tests cover unavailable devices, unsupported media,
  converter allocation, and channel-format setup; cleanup releases every resource.
  Module startup invokes this helper, calls the channel, and starts its worker.
- Asterisk codec-registry selection with bidirectional translation checks and
  hardware-bounded automatic rate selection, tested with deterministic API fixtures.
  The module's reservation path uses this selector.
- Piper process adapter with direct argument execution, file-backed text input,
  nonblocking completion polling, cancellation, and Asterisk child-reaper
  coordination. Prepared output is validated and bound to scheduled playback.
  Tests cover injected failures and real subprocess execution with a fixture
  executable; they do not claim verification of a real Piper voice model.
- File-backed FFmpeg preparation of mono playback PCM at the chosen rate, sharing
  process lifecycle handling with Piper. A real 22050-to-48000 Hz WAV conversion
  test checks sample count and level; malformed input is rejected.
- Runtime file-to-speech-to-Morse preparation, checked PCM loading, bounded child
  execution, and temporary-file cleanup. Unavailable sets without Morse do not
  occupy a scheduling slot. Tests cover each I/O failure, allocation failure,
  timeout cancellation, and actual FFmpeg conversion of a fixture synthesizer's WAV.
- Real-Asterisk two-node audio integration using a test-only 48 kHz radio driver.
  Tests exercise native linear, 16 kHz linear, and 8 kHz mu-law with actual Asterisk
  converters; verify half/full-duplex local repeat, Morse output, balanced PTT,
  and reload-driven channel cleanup. The fixture is not installed by the package.
- Real Piper 1.8.0 synthesis using 524950's existing Amy-low model, tested locally
  on Debian 13 amd64 through the module's preparation code. See
  [testing](testing.md) for the optional real-model invocation and its scope.
- Half/full-duplex transmit ownership and configurable hang-time policy.
- Integrated node controller joining ID scheduling, prepared PCM/Morse playback,
  local repeat, and PTT/hang policy. Sequence tests cover half-duplex deferral,
  receive interruption, priority satisfaction, and first-key identification after
  inactivity. Module startup binds it to its channel worker.
- Joinable channel worker driven by channel readiness, with monotonic ID timing,
  bounded shutdown checks, and unkey/hangup before releasing controller state.
  Tests cover injected failures and real threads driven by pipe-based hardware
  events, including stop and replacement with a different controller. Module
  configuration loading now starts these workers for every enabled node.
- Shared, node, and ID-set configuration-value inheritance, including explicit
  empty overrides and scope independence from file order.
- In-place configuration-line syntax parsing, including whitespace, semicolon
  comments, section names, and empty option values. File loading and schema
  validation remain separate work.
- Bounded unsigned-decimal and explicit yes/no value validation. Invalid values
  leave their destination unchanged; numeric overflow is rejected.
- Typed node and ID settings with shared/node/set inheritance, documented in
  [configuration](configuration.md), and atomic rejection of invalid values.
- Streaming file-syntax reader with physical-line diagnostics, unbounded line
  lengths, embedded-null rejection, and builder-error propagation.
- Owned configuration storage, including empty sections, with complete cleanup
  on syntax/allocation failures and file-to-settings integration tests.
- Whole-file option and scope validation, exact node-reference checking, and
  duplicate-free enumeration of named nodes and their identifier sets.
- Strict formatting, compiler diagnostics, Cppcheck, Clang-Tidy, Doxygen, and
  per-platform line/branch coverage gates.
- Doxygen publication to GitHub Pages after the main-branch quality gate passes.

The build produces a static controller library and `app_rpt_advanced.so`. The
module starts named radio workers with inherited, prepared file/speech/Morse IDs.
Invalid configuration
leaves running workers untouched; a valid reload stops and replaces them. Failed
radio startup releases partial resources and attempts to reopen the previous
configuration, reporting any restoration failure. Lifecycle tests use the real shared library and
Asterisk's public ABI, including configuration-path allocation and input errors.
An integration test loads, reloads, and unloads the installed module in an isolated
Asterisk process with temporary configuration and a synthetic radio. Other tests
include a combined identifier/duplex state sequence. They do not claim physical
radio or completed USBRadioPlus-to-rpt_advanced integration verification.

## Not yet implemented

- End-to-end verification of running node audio with the separate USBRadioPlus adapter.
- Published project-specific clean/installed test images and release packaging.
  CI uses published rpt_advanced quality images for Debian 12/13 and amd64/arm64.
  These add FFmpeg to the existing ASL3 quality tool environment; they are not
  clean-install or installed-release artifacts.

Nothing has been installed on a radio node. No app_rpt implementation has been
copied. USBRadioPlus changes are limited to its separate RadioPlusAdvanced adapter
and shared-engine integration. They passed the local and four native-platform
quality/coverage/install gates and were merged in USBRadioPlus pull request 16.
