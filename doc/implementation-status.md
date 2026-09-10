# Implementation status

## Implemented and tested

- Original C identifier scheduling policy: intervals, priorities, activity-based
  and unconditional periods, first-key-only sets, bounded polite deferral while
  receiver/link activity or queued telemetry is present, and completion hierarchy.
- Named announcements with inherited file/speech/Morse media: zero-interval
  every-release playback, positive rate-limited playback, configuration-order
  serialization after ordinary hang and due IDs, idle PTT initiation, and smooth
  receive-active ducking.
- Named courtesy-tone inputs with shared and per-node media defaults: receiver,
  generic-link, and permanent-direct-peer routing; file/speech/generated-tone/Morse
  fallback; source-specific pending cancellation; and receive-active ducking.
  Generated sequences are pre-rendered on reload and support bounded mono-tone,
  dual-tone, silence, duration, and per-segment-level patterns.
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
- Integrated node controller joining ID and announcement scheduling, prepared
  PCM/Morse playback, local repeat, and PTT/hang policy. Sequence tests cover
  half-duplex deferral, receive interruption, priority satisfaction, polite
  deferral, first-key identification after inactivity, and announcement ordering,
  release, idle-keying, and ducking. Module startup binds it to its channel worker.
- Joinable channel worker driven by channel readiness, with monotonic ID timing,
  bounded shutdown checks, and unkey/hangup before releasing controller state.
  Tests cover injected failures and real threads driven by pipe-based hardware
  events, including stop and replacement with a different controller. Module
  configuration loading now starts these workers for every enabled node.
- Shared, node, and ID/announcement/courtesy-set configuration-value inheritance,
  including explicit empty overrides and scope independence from file order.
- In-place configuration-line syntax parsing, including whitespace, semicolon
  comments, section names, and empty option values. File loading and schema
  validation remain separate work.
- Bounded unsigned-decimal and explicit yes/no value validation. Invalid values
  leave their destination unchanged; numeric overflow is rejected.
- Typed node, ID, announcement, and courtesy settings with shared/node/set
  inheritance, documented in [configuration](configuration.md), and atomic
  rejection of invalid values.
- Streaming file-syntax reader with physical-line diagnostics, unbounded line
  lengths, embedded-null rejection, and builder-error propagation.
- Owned configuration storage, including empty sections, with complete cleanup
  on syntax/allocation failures and file-to-settings integration tests.
- Whole-file option and scope validation, exact node-reference checking, and
  duplicate-free enumeration of named nodes and their identifier, announcement,
  and courtesy sets.
- Local AllStarLink link control: verified inbound admission, duplicate and
  topology-loop rejection, direct IAX peer routing, linking-only DTMF,
  permanent-link recovery, direct-peer remote command mode, `*722` local-time
  speech with Morse fallback, lock-free prepared-speech RF status playback, and
  best-effort `L ` topology exchange. See
  [AllStarLink status](allstarlink-status.md) for validation limits.
- Local-time scheduled events with strict daily, weekly, and one-time triggers;
  inherited named message templates; serialized speech/Morse telemetry; and
  configuration-order controller macros limited to direct link operations.
- The quality policy requires strict formatting, compiler diagnostics, Cppcheck,
  Clang-Tidy, Doxygen, and per-platform line/branch coverage gates. Doxygen is
  published to GitHub Pages after the main-branch quality gate passes.

The build produces a static controller library and `app_rpt_advanced.so`. The
module starts named radio workers with inherited, prepared file/speech/Morse IDs,
announcements, and courtesy tones.
Invalid configuration
leaves running workers untouched; a valid reload stops and replaces them. Failed
radio startup releases partial resources and attempts to reopen the previous
configuration, reporting any restoration failure. Lifecycle tests use the real shared library and
Asterisk's public ABI, including configuration-path allocation and input errors.
An integration test loads, reloads, and unloads the installed module in an isolated
Asterisk process with temporary configuration and a synthetic radio. Other tests
include combined identifier/announcement/courtesy/duplex state sequences. The
native matrix also links the actual USBRadioPlus adapter to the synthetic
hardware backend.

## Remaining verification

- Physical identifier playback/interruption and the remaining hardware acceptance
  cases in [testing](testing.md).
- A fresh complete quality gate and Debian 12/13 amd64/arm64 matrix for the
  current AllStarLink source changes.
- Explicitly approved live interoperability testing with classic `app_rpt` and
  higher-rate capable peers. The AllStarLink integration has not been deployed
  on 524950 or any other live node.
- Execution verification of the version-tag release workflow. Source archive
  rebuilding is covered by the platform gate; no project release has been cut.

The project provides clean ASL3 and installed-module test images for Debian
12/13 and amd64/arm64; see [testing](testing.md). The quality images remain
separate development environments with compilers and analysis tools. Their
existence is not a quality result for the current working tree.

## Historical local-radio test, 2026-09-07

The following radio-only observation predates the AllStarLink implementation.
It neither installed nor enabled the current link controller.

With the owner's approval, 524950 runs the controller on Debian 13 arm64 with
ASL3 Asterisk 22.9.0 / ASL 3.9.3 and the actual CM119 interface. Asterisk reports
`RadioPlusAdvanced`, `slin48` in both directions, and no transcoding. The owner
confirmed clean repeat audio and approximately one second of configured hang time.
At 39,967 native frames, counters showed zero FIFO underruns/overruns, SRC errors,
or USB short/error writes. ADC rail counts were nonzero; this does not establish
receive-level calibration or absence of analog clipping.

Configuration reload and controller unload/replacement/load succeeded with the
same Asterisk PID. The temporary 15-second speech-ID test has been restored to
the normal ten-minute activity-based interval. The owner confirmed that an
incoming signal interrupted speech and switched playback to Morse.

Normal unloading of the pre-existing app_rpt crashed Asterisk before the new
modules were installed. Controlled recovery required a service restart. Startup
also exposed driver ordering: USBRadioPlus provides an optional ordering
dependency so the controller can start after its radio driver. After a controlled
service restart, the controller opened the native 48 kHz radio channel without
transcoding. The original modules and configuration are retained for rollback.

No app_rpt implementation has been copied. USBRadioPlus changes are limited to
its separate RadioPlusAdvanced adapter and shared-engine integration. The
current working tree still requires its fresh quality and interoperability
verification before release or deployment.
