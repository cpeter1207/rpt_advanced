# ADR 0020: Rust dylibs separate private components

Status: Partially superseded by ADRs 0021 and 0022

## Context

rpt_advanced needs deliberately separated, Rust-owned components without
turning every internal boundary into a public C ABI. Public C-facing boundaries
need a different, stable compatibility contract and are now governed by ADRs
0021 and 0022.

## Decision

A deliberately separated private rpt_advanced component is a Rust `dylib`.
Private components and their callers are project-owned Rust code; their Rust
ABI is not a public compatibility contract. They are built, tested, and
released with a compatible pinned Rust toolchain and their owning product.

Public C-facing boundaries are outside this record's scope. They use separate,
versioned Rust `dylib` adapters and the stable C-compatible descriptor/
function-table contract in ADRs 0021 and 0022. A minimal C loader remains
permissible when Asterisk macro-generated metadata or its loader ABI requires
C source, but it is part of the Asterisk adapter rather than a private
component boundary.

The stable SONAME, Debian packaging, and minimum-compatible-version rules of
ADR 0018 continue to govern external system dependencies and separately
released public project libraries. A private Rust `dylib` is deliberately not
such a public ABI; it is an internal component boundary.

## Consequences

Crate boundaries express component ownership and dependency direction, while
private `dylib` boundaries also make component separation explicit at runtime.
The project pins compatible Rust build inputs for every set of cooperating
private dylibs. A change to a private Rust interface can be made together with
its callers. External-adapter ABI and packaging discipline belongs to ADRs
0021 and 0022.

ADR 0021 supersedes this record's former public-boundary shim placement. This
record continues to govern private Rust `dylib` component boundaries.
