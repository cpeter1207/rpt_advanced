# Implementation status

## Implemented and tested

- Original C identifier scheduling policy: intervals, priorities, activity-based
  and unconditional periods, first-key-only sets, and completion hierarchy.
- File/speech/Morse preference policy, including Morse-only reception behavior.
- Streaming signed-linear Morse renderer with configurable rate, speed, and tone
  frequency; fractional-sample timing and block-independent output tests.
- Sample-driven prepared-PCM playback with immediate, irreversible Morse fallback
  on reception. Completion remains terminal, and control events need not consume
  audio samples. This playback state is not yet connected to radio transport.
- Asterisk frame-exchange boundary that sends one transmit block per received
  voice block, including silence. Carrier events change state without advancing
  audio. API-fixture tests cover ownership, malformed frames, and output failures;
  node workers and the separate hardware adapter are not yet connected.
- Optional Asterisk codec conversion around linear controller processing, with
  buffered-conversion ownership tests. Native linear transport bypasses these
  converters.
- Exclusive RadioPlusAdvanced reservation with negotiated read/write formats and
  converter ownership. Failure tests cover unavailable devices, unsupported media,
  converter allocation, and channel-format setup; cleanup releases every resource.
  This helper does not call or key a radio. Module startup still needs to invoke it.
- Asterisk codec-registry selection with bidirectional translation checks and
  hardware-bounded automatic rate selection, tested with deterministic API fixtures.
  The reservation helper uses this selector; module startup is not yet connected.
- Piper process adapter with direct argument execution, file-backed text input,
  nonblocking completion polling, cancellation, and Asterisk child-reaper
  coordination. Synthesis output validation and playback remain unfinished.
  Tests cover injected failures and real subprocess execution with a fixture
  executable; they do not claim verification of a real Piper voice model.
- File-backed FFmpeg preparation of mono playback PCM at the chosen rate, sharing
  process lifecycle handling with Piper. A real 22050-to-48000 Hz WAV conversion
  test checks sample count and level; malformed input is rejected.
- Half/full-duplex transmit ownership and configurable hang-time policy.
- Integrated node controller joining ID scheduling, prepared PCM/Morse playback,
  local repeat, and PTT/hang policy. Sequence tests cover half-duplex deferral,
  receive interruption, priority satisfaction, and first-key identification after
  inactivity. It is not yet connected to a live channel worker.
- Joinable channel worker driven by channel readiness, with monotonic ID timing,
  bounded shutdown checks, and unkey/hangup before releasing controller state.
  Tests cover injected failures and real threads driven by pipe-based hardware
  events, including stop and replacement with a different controller. Module
  configuration loading does not yet start these workers.
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
module currently provides configuration loading, failure-atomic reload, and
cleanup, not radio operation. Lifecycle tests use the real shared library and
Asterisk's public ABI, including configuration-path allocation and input errors.
An integration test loads, reloads, and unloads the installed module in an isolated
Asterisk process with temporary configuration. Other tests include a combined
identifier/duplex state sequence. They do not claim live-radio verification.

## Not yet implemented

- Radio-node lifecycle, runtime media capability discovery, and audio transport.
- Connecting the separate USBRadioPlus adapter to module startup and negotiation.
- Sound-file playback, Piper adapter, connecting Morse to transmission, and playback interruption.
- Published project-specific clean/installed test images and release packaging.
  CI uses published rpt_advanced quality images for Debian 12/13 and amd64/arm64.
  These add FFmpeg to the existing ASL3 quality tool environment; they are not
  clean-install or installed-release artifacts.

Nothing has been installed on a radio node. No app_rpt implementation has been
copied. USBRadioPlus changes are limited to its separate RadioPlusAdvanced adapter
and shared-engine integration. They passed the local quality/coverage/install gate
and are undergoing the native platform checks in USBRadioPlus pull request 16.
