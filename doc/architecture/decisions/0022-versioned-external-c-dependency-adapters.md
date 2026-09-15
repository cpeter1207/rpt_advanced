# ADR 0022: Versioned adapters isolate external implementations

Status: Accepted

## Context

Rust-owned controller and radio components need capabilities supplied by
external implementations such as FFmpeg, libsamplerate, Hamlib, and Piper.
Direct FFI or process calls would make those implementation choices part of
internal Rust code and make a replacement require changes throughout the
controller.

## Decision

Every call from owned Rust code to an external implementation goes through a
separate, removable, versioned Rust `dylib` adapter shared object. An adapter
owns the external library's headers, FFI bindings, process protocol when
applicable, callbacks, lifetime rules, and error translation. Internal Rust
code uses only its adapter-neutral port and cannot import external-library
types, invoke an external process, or call the external API directly.

Each adapter exports one stable C-compatible descriptor/function-table
contract. The descriptor identifies the capability and ABI version, declares
its structure size, and supplies explicit lifecycle, status/error, and
capability-specific operation functions. Opaque handles and explicit buffer
arguments cross the contract; Rust types, layouts, and panics do not. The
contract intentionally has no generic escape hatch to an external library's
native API. Capability-specific functions are added only when an internal Rust
port requires them.

The complete adapter architecture has one adapter per independently replaceable
capability: FFmpeg graph, sample-rate conversion, Hamlib, speech synthesis,
PortAudio/ALSA, control-path execution, and Asterisk entry. This record governs
the external-service adapters; ADR 0021 governs the Asterisk entry adapter.
ADR 0038 defines the control-path adapter, initially backed by Asterisk's
taskprocessor and independently replaceable without changing controller policy.
The generic speech-synthesis adapter hides
whether Piper is invoked as a library or a subprocess. It can therefore be
replaced without changing core speech policy or callers.

Each product composition declares a complete adapter manifest. Every adapter
listed by that composition is mandatory; a standalone composition does not
list the Asterisk entry adapter or an Asterisk-backed control-path adapter.
Startup and configuration reload validate that the
complete selected manifest is present and ABI-compatible. A missing,
unloadable, or incompatible adapter is an error: startup fails, or a live
reload retains the prior complete configuration. No selected composition runs
with a reduced feature set, silent fallback, or direct in-process substitute.

Adapter replacement is a package or composition change activated only during a
controlled process restart or Asterisk module reload after all associated
callbacks and contexts have stopped. Runtime hot replacement, `dlclose`, and
loading executable code from a real-time tick are prohibited.

Every real-time-capable adapter separates setup and control from its
preallocated, lock-free tick operation. A tick must not allocate, block, log,
execute a process, load code, or take a lock. Replacing one external
implementation therefore means replacing only its adapter shared object and
declared external package dependency; the replacement implements the same
descriptor and capability contract without modifying internal Rust components.
ADR 0021 governs C-to-Rust entry adapters; this record governs Rust-to-external
dependency adapters.

The PortAudio/ALSA adapter's real-time entry accepts only the bounded frame
count supplied at stream setup and delegates native processing through the
variable-frame contract in ADR 0027. It opens the callback as `paFloat32`,
which exchanges canonical normalized `f32` PCM with the core without a
sample-format conversion. PortAudio/ALSA owns conversion at the physical
device edge. The entry fills the complete callback output; all blocking,
partial-I/O, and Asterisk compatibility work remains outside that entry.

The audio, radio-control, and GPIO adapters are separate selected capabilities,
not a single hardware-services facade. ADR 0028 assigns their ownership and
requires them to replace the current `res_usbradio` dependency.

## Consequences

FFmpeg, libsamplerate, Hamlib, Piper, PortAudio/ALSA, the Asterisk integration,
and future external dependencies are replaceable implementation details rather
than controller dependencies. Each adapter has its own SONAME, Debian runtime
package, compatibility tests, and minimum-compatible-version policy.

The stable descriptor/function-table contract, rather than the private Rust
ABI of a `dylib`, makes independently built adapter replacements possible. Its
exact symbol names and capability-specific table fields are implementation
details, but every published descriptor follows the lifecycle, versioning, and
real-time rules in this record.
