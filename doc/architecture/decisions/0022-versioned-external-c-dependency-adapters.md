# ADR 0022: Versioned adapters isolate external implementations

Status: Implemented for the current selected capabilities; future capabilities remain planned

## Context

Rust-owned controller and radio components need capabilities supplied by
external implementations such as FFmpeg, Hamlib, and Piper.
Direct FFI or process calls would make those implementation choices part of
internal Rust code and make a replacement require changes throughout the
controller.

## Decision

Every call from owned Rust code to an external implementation goes through a
separate, removable, versioned Rust `cdylib` adapter shared object. An adapter
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
capability: FFmpeg graph, sample-rate conversion, system DNS resolution,
packet jitter buffering, Hamlib, speech synthesis,
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

FFmpeg, Hamlib, Piper, PortAudio/ALSA, the Asterisk integration,
and future external dependencies are replaceable implementation details rather
than controller dependencies. Each adapter has its own SONAME, Debian runtime
package, compatibility tests, and minimum-compatible-version policy.

The stable descriptor/function-table contract, rather than the private Rust
ABI of a `dylib`, makes independently built adapter replacements possible. Its
exact symbol names and capability-specific table fields are implementation
details, but every published descriptor follows the lifecycle, versioning, and
real-time rules in this record.

## Implementation boundary (2026-10-13)

Standalone selects `librptadv_resolver_adapter.so.1`, capability
`rptadv.resolver`, descriptor ABI 1. The adapter owns libresolv bindings,
SRV packet decoding/priority/weight selection, and system address resolution.
Synchronous borrowed hostname/port and SocketAddr text results cross its C
contract; lookup precedence and source authentication stay in the product.
The Asterisk composition retains its Asterisk-backed resolver.

The standalone manifest requires the resolver alongside its existing providers.
Startup and reload validate the retained manifest and complete lifecycle tables;
reload never loads replacement code. Product shutdown precedes resolver-context
destruction and library release. A rejected reload preserves the active
configuration. Resolver runtime and development packages are independently
installable and do not become Asterisk-module dependencies.

This completes the current composition's external-capability extraction, not
unimplemented Hamlib, remote-base, or other wishlist capabilities. Filesystem,
socket, clock, and other ordinary operating-system facilities remain with their
existing owners; this decision does not introduce a generic OS adapter.

## Packet-jitter capability (2026-10-14)

ADR 0037 adds a mandatory standalone jitter adapter, initially backed by
SpeexDSP's generic adaptive packet buffer. Only this separate Rust `cdylib`
links SpeexDSP and owns its types, FFI and lifetime. Protocol/controller code
uses neutral bounded packet operations; codecs and PCM-ring PLC remain separate.
It ships its own ABI-1 descriptor, SONAME and runtime/development packages,
following the resolver adapter's workspace pattern. The standalone composition
requires it at startup and retains it through peer-task shutdown. Local composed
tests cover the boundary; hosted and live acceptance are tracked by ADR 0037.
The Asterisk composition is unchanged. This implementation has not been deployed
to the running node.
