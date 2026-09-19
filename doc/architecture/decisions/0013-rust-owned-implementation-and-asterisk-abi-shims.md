# ADR 0013: Rust owns implementation; C is limited to Asterisk ABI shims

Status: Partially superseded by ADRs 0020, 0021, and 0022

## Context

The project needs the memory-safety and ownership guarantees of Rust without
maintaining substantive controller, radio, and audio logic in two languages.
It must still interoperate with Asterisk/ASL3 and C libraries including
PortAudio, ALSA, FFmpeg, Hamlib, and existing published shared-library ABIs.

## Decision

Migrate all substantive owned implementation in USBRadioPlus,
`rate_adjusting_pcm_ring`, `librptadvradio`, rpt_advanced, and future extracted
components to Rust. Internal Rust code reaches external C implementations only
through the per-capability adapters defined by ADR 0022. The adapters use FFI
and stable C-compatible descriptor/function-table contracts; the core does
not. Published shared-library ABI and SONAME compatibility remain unchanged
unless a separately approved ABI change is required.

For Asterisk modules only, retain a tiny C loader shim when Asterisk's
macro-generated module metadata or loader ABI cannot be represented safely and
stably from Rust. The shim declares Asterisk-required metadata and forwards to
Rust exports. It contains no radio, audio, controller, configuration, policy,
or business logic. It changes only for an Asterisk module-ABI change.

Standalone binaries contain no project C implementation or Asterisk shim. They
use the complete standalone adapter manifest rather than linking system
libraries directly from controller or radio-core Rust code.

## Consequences

The project remains Rust-owned rather than a dual-language implementation.
External C dependencies remain normal ABI dependencies, not sources of
duplicated application logic. Rust real-time code preserves the existing rules:
no allocation, locks, blocking I/O, process execution, logging, or panic across
an FFI boundary in audio ticks.

The quality gate gains Rust formatting, Clippy with warnings denied, Rustdoc
with warnings denied, targeted coverage, and the existing native Debian matrix.
Each migrated component must retain behavior through reference and integration
tests before its C implementation is retired.

ADRs 0020 and 0021 supersede this record's Asterisk-only public-shim scope.
ADR 0022 supersedes direct core-Rust FFI and direct standalone system-library
linkage. Its Rust ownership and prohibition on substantive project C
implementation remain in force.
