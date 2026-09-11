# ADR 0014: Rust implementation uses object-oriented composition

Status: Accepted

## Context

The project is migrating substantive implementation to Rust while retaining
independently versioned components and narrow C ABI boundaries. The design must
remain object-oriented rather than becoming a procedural collection of free
functions and shared state.

## Decision

Model each architectural component as a stateful object with private state,
public methods, and explicit ownership. Rust `struct` and `impl` blocks provide
the class-and-method model; traits provide interfaces and controlled dynamic
substitution. Use composition and constructor dependency injection rather than
inheritance, global state, or cross-layer procedural orchestration.

`NodeController` is the node-policy aggregate root. It owns and coordinates
objects such as `LinkManager`, `TopologyManager`, `TelemetryPlanner`,
`IdentifierPolicy`, `AnnouncementPolicy`, `DuplexPolicy`, and
`TimeoutPolicy`. `RadioCore` is the real-time aggregate root. It owns native
tick state and collaborates with signal-processing objects through narrowly
defined interfaces. Adapters are objects that translate external APIs and
hardware I/O into controller and radio-core contracts.

Examples of object boundaries include `PcmRing`, `ConfigDocument`, `Template`,
`ScheduleRule`, `ToneOscillator`, `MorseRenderer`, `DtmfDetector`,
`NoiseSquelch`, `CtcssDecoder`, `DcsDecoder`, `MediaPreparer`,
`DirectoryResolver`, `AccessPolicy`, `Iax2Session`, and adapter objects.

No Rust trait object, allocation, lock, blocking operation, logging call, or
panic path is introduced by an audio tick. Interfaces used by a real-time
object are constructed before the tick starts and are invoked only through
preallocated state. Rust traits never cross a shared-library ABI; C-compatible
opaque handles, callback tables, and buffer structures remain the external
boundary.

Private helper functions are permitted for local implementation detail, but
they do not own workflow, mutable state, policy, or cross-component control.

## Consequences

Each object has one owner, one responsibility, explicit lifecycle methods, and
unit tests focused on its observable behavior. Components can be replaced or
mocked through traits at construction boundaries without exposing their state.
The design remains idiomatic Rust while providing the encapsulation and
collaboration model expected from an object-oriented architecture.
