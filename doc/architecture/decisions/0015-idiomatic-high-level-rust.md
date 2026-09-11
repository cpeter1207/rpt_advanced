# ADR 0015: Use idiomatic high-level Rust behind narrow ABI boundaries

Status: Accepted

## Context

The project is migrating substantive implementation from C to Rust. Merely
translating pointer-oriented procedural code would retain C's ownership,
validation, and error-handling weaknesses without obtaining Rust's principal
benefits.

## Decision

Use Rust as a high-level, object-oriented language inside each component.
Model ownership and lifecycle with owned values, borrowing, RAII, and explicit
constructors. Model domain values with newtypes and validated constructors—for
example node identity, sample rate, dB level, duration, configuration section,
and peer identity—rather than unvalidated primitive values passed throughout
the system.

Use enums and pattern matching for commands, events, protocol states,
telemetry/media states, and recoverable errors. Use `Result` with typed,
contextual errors at ordinary control-plane boundaries. Use traits and generic
composition for replaceable services and test doubles; do not use inheritance,
global mutable state, or callback-driven procedural state machines as the
primary design.

Keep `unsafe` confined to small audited FFI and real-time buffer modules. Safe
objects own resource cleanup, length validation, callback lifetimes, and
cross-thread contracts. C-compatible types remain only at dynamic-library and
external-library boundaries; they are translated immediately to typed Rust
objects.

High-level abstractions must not weaken real-time behavior. Audio ticks use
preallocated memory, deterministic bounded work, lock-free communication, and
non-panicking methods. Allocation, blocking I/O, process management, and rich
error construction remain on control-plane or media-preparation objects.

## Consequences

New Rust code is designed around invariants the compiler can enforce rather
than manual cleanup, null checks, integer sentinel values, and scattered state
flags. Tests exercise object behavior through public methods and traits while
unsafe boundaries receive focused audits and tests. The implementation may use
Rust's standard-library collections and abstractions when they are outside a
real-time tick; no broad asynchronous framework is implied by this decision.
