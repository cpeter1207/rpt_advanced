# Implementation status

## Implemented and tested

- Original C identifier scheduling policy: intervals, priorities, activity-based
  and unconditional periods, first-key-only sets, and completion hierarchy.
- File/speech/Morse preference policy, including Morse-only reception behavior.
- Half/full-duplex transmit ownership and configurable hang-time policy.
- Shared, node, and ID-set configuration-value inheritance, including explicit
  empty overrides and scope independence from file order.
- In-place configuration-line syntax parsing, including whitespace, semicolon
  comments, section names, and empty option values. File loading and schema
  validation remain separate work.
- Strict formatting, compiler diagnostics, Cppcheck, Clang-Tidy, Doxygen, and
  per-platform line/branch coverage gates.
- Doxygen publication to GitHub Pages after the main-branch quality gate passes.

The current build artifact is a static library of controller policy components,
not an Asterisk module. Tests use deterministic inputs and include a combined
identifier/duplex state sequence. They do not claim live-radio verification.

## Not yet implemented

- Configuration file loading, schema validation, and node lifecycle.
- Asterisk module, runtime media capability discovery, and audio transport.
- The USBRadioPlus compatibility adapter and sample-rate negotiation.
- Sound-file playback, Piper adapter, Morse generation, and playback interruption.
- Published project-specific clean/installed test images and release packaging.
  CI currently uses existing USBRadioPlus ASL3 quality
  images as its tool environment, not as rpt_advanced release artifacts.

Nothing has been installed on a radio node. No app_rpt implementation has been
copied. USBRadioPlus has not been modified.
