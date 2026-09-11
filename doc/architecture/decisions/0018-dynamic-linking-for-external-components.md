# ADR 0018: External and project components are dynamically linked

Status: Accepted

## Context

rpt_advanced, USBRadioPlus, and their extracted libraries need independent
updates, security fixes, packaging, and ABI compatibility without copying or
statically embedding third-party source into unrelated deliverables.

## Decision

Every external system dependency and every separately released project
component is dynamically linked. Do not vendor third-party source, link static
archives, or embed another released project's implementation into a consumer.
Each such dependency has a versioned shared object, normal SONAME compatibility
policy, Debian runtime package, and development package. Consumers declare a
minimum compatible ABI/package version and package their runtime dependencies.

This applies equally to project libraries such as the rate-adjusting PCM ring,
radio and controller components, and to external dependencies such as
PortAudio, ALSA, FFmpeg, Hamlib, libsamplerate, and Asterisk interfaces.

Rust crates used only to implement one owned Rust shared object or executable
may compile into that owning artifact. They are source-level implementation
dependencies, not independently deployed ABI components. A Rust crate becomes
a dynamically linked component only when it is intentionally released through
a stable external ABI under the component policy.

The minimal C Asterisk loader shim is compiled into its owning adapter module.
It is not a separately reusable component or external dependency and contains
no substantive application logic.

## Consequences

Build and packaging rules reject vendored and static external linkage. Release
verification checks dynamic dependency metadata and package dependencies. Rust
can remain idiomatic and use Cargo crates internally without relying on
unstable Rust-to-Rust dynamic ABI, while every independently deployed component
remains replaceable through a stable shared-library contract.
