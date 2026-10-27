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

### Protocol-neutral codec capabilities — accepted, implementation pending (2026-10-26)

Codec implementations belong behind independently replaceable, versioned
dynamic adapters. Their contracts are protocol-neutral: other protocol adapters
can use the same codecs without importing or loading the IAX2 implementation.
Codec adapters own codec bitstreams and encoder/decoder state. Each protocol
adapter owns its format-ID mapping, negotiation and transport packet framing.
Neither network I/O nor controller policy belongs in a codec adapter.

Prefer released GPLv2-compatible implementations. Original implementations are
allowed only for trivial gaps without a suitable compatible released library.
Siren7, Siren14 and G.719 remain explicitly deferred. This decision defines a
reusable codec boundary, not authorization to implement another protocol.
Provider replacement retains this ADR's controlled reload/restart rules.

The approved text-message codec likewise belongs to this protocol-neutral
boundary. It applies to incoming human messages from EchoLink and every other
text-capable protocol supported now or added later. It uses the
speech-synthesis capability outside audio callbacks and produces incoming
media, not polite controller telemetry. Controller policy routes its speech
only to the local transmitter, mixed with receive audio including concurrent
audio from its originating peer. Text-derived speech never enters peer audio
egress. Forward eligible messages to other peers as text only when their
protocols support forwarding; each peer chooses its own rendering. Retain the
origin and normal no-echo policy. Protocol-control text is not a speech input.
If synthesis fails, skip only the local spoken rendition and log an error,
without Morse fallback or interruption of other audio. Text forwarding is
independent of local synthesis. This does not implement any planned protocol
adapter; the text-message codec remains unimplemented.

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
uses neutral bounded packet operations; codec-native concealment remains enabled
and takes priority. The shared PCM ring supplies fallback concealment only for
missing spans the codec cannot recover; it must not conceal native PLC output
again. Codec work and fallback PCM-ring processing remain separate.
It ships its own ABI-1 descriptor, SONAME and runtime/development packages,
following the resolver adapter's workspace pattern. The standalone composition
requires it at startup and retains it through peer-task shutdown. The standalone
jitter capability passed the native-platform gate, shipped in signed Debian
packages, and was validated on node 524950 as recorded in ADR 0037. The Asterisk
composition is unchanged.

## Site GPIO capability (2026-10-19)

`librptadv_gpiod_adapter.so.1` implements capability `rptadv.gpiod`, descriptor
ABI 1, in a separate optional runtime/development package. Only this adapter
imports libgpiod. Its complete sized records select an exact device path and
line, polarity and startup/safe levels. Line acquisition applies the startup
level atomically with direction; cleanup attempts the configured safe level.
Callers serialize each live context and must confirm a safe write before
retiring it when ownership handoff requires confirmation. Device failure does
not permit a different chip to be selected. Output submission does not claim
electrical readback or PWM support.

The descriptor, device-boundary fixtures, dynamic-library linkage, package
staging and selected-provider composition are verified locally. The shared
product supplies bounded acquisition and station policy on its existing
control owner. Physical wiring has not been qualified; the feature's full
hosted gate remains outstanding.
The adapter is not an unconditional dependency of either controller entrypoint.

The shared product selects site providers from enabled endpoints on enabled
nodes. Startup retains each selected DSO after checking its complete size,
version, capability and operation table. Reload validates against that retained
set before gating controller work; it never loads replacement executable code.
Adding a previously unloaded capability requires a controlled product restart
or module reload. Removing its last endpoint does not unload the provider while
the product is alive. Ordinary IIO filesystem access and the existing radio's
CM119/parallel owner do not add optional library requirements.

## Site measurement capability (2026-10-19)

`librptadv_hwmon_adapter.so.1` implements the optional `rptadv.hwmon`
capability with an ABI-1 descriptor and separate runtime/development packages.
Only this adapter imports libsensors. A counted initialization lease retains
its immutable tables until the last measurement closes; acquisition does not
hold that lifecycle lock. An explicit empty library configuration prevents host
compute expressions from applying a second calibration. The controller receives
finite source-unit measurements and owns calibration, freshness and policy.

An endpoint matches an opened directory's device/inode identity, including
hwmon class symlinks. Reads use its retained directory FD, not a reusable
enumeration path. Failures report fault without substituting zero or another
device. Supported kernel PWM attributes are retained separately and require
existing manual mode. PWM submission does not claim electrical/tachometer
readback and does not implement software pulse generation. Safe handoff requires
a successful explicit safe write; final destruction is best-effort.

Hardware-free tests cover source units, library lifetime, alias/reuse handling,
independent slow reads, PWM capability/safe-state behavior and selected-provider
composition. Shared-product acquisition and policy use these adapters outside
audio callbacks; these fixtures do not qualify physical sensor or fan hardware.

### Site reload ownership (2026-10-20)

The new, not-yet-released GPIO and hwmon tables also require `set_safe`, a
serialized memory-only retirement-level update. Matching complete descriptor
sizes are required; product and providers must be built together for this first
release. Descriptor ABI and SONAME remain 1. No released table is replaced.

A physical endpoint's role and polarity define its retained request. Startup
levels apply only on first acquisition. Editing startup or safe policy retains
the handle, current demand, acknowledgments and pulse deadline. The device
owner updates retirement metadata only at committed handoff, before resuming;
an aborted preparation retains the old safe level. An invalid provider response
keeps that owner fenced and diagnosed, never silently reopened.

Reload fences changed owners individually. Unrelated acquisition, output
timers, cooling and alarm protection continue while they acknowledge. Cached
radio output masks do not prove current service: the successful shared service
cycle must remain fresh. Stale status retains historical submission values
without claiming current availability or electrical readback.

### Finite codec source coverage (2026-10-27)

The provider's existing ABI-1 encode result separates consumed input, covered
real source frames and decoded wire duration. A finite flush emits at most one
legal packet per call until delayed source content is drained. Priming packets
may cover no real source; tail packets may consume no new input. Neither is
discarded when it contains encoded bytes. Capacity failures do not advance the
codec or its clocks. Reset abandons a segment's pending PCM and delayed tail.

Only providers know their released codec's drain budget. Speex and Opus query
their encoder API; Opus's documented total preskip is not labeled encoder-only
buffering. Fixed released G.729, G.723.1 and LPC10 pipelines retain their tail
padding without disabling decoder enhancement or native concealment. Causal
filter delay is not silently folded into a forward-lookahead claim.

The product's shared resampler owner trims setup history, drains the real
source-duration budget and excludes zero padding before codec flush. Egress
preserves original packet age and advances wire timestamps by decoded packet
frames, including finite padding. A blocked send retries the same prepared
bytes without another encode. Both hosts use the same conversion owner; the
Asterisk PCM path retains its existing 20 ms pacing.
