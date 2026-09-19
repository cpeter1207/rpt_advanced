# ADR 0021: Versioned adapter shared objects isolate C-to-Rust boundaries

Status: Accepted

## Context

rpt_advanced must keep controller, radio, audio, and policy implementation in
Rust while interoperating with C-facing systems such as Asterisk and PortAudio.
Those dependencies must not shape internal Rust code or prevent an adapter from
being retired as the standalone implementation replaces an outer integration.

## Decision

Every boundary where C calls an owned Rust component is a separate, versioned
Rust `dylib` adapter shared object. The adapter exposes only the smallest C-ABI
function set needed for that interoperability and imports only the external C
API it needs. It has a normal SONAME, Debian runtime package, development
package when an external consumer needs headers, and
minimum-compatible-version dependency policy.

The Rust implementation remains Rust up to the adapter boundary. Internal Rust
components expose adapter-neutral Rust ports and contain no external-library
types, headers, callback signatures, or adapter-specific policy. An adapter
may call inward through a private Rust `dylib`; when C must call owned code, the
adapter supplies the narrow `extern "C"` entry point or callback bridge. Opaque
contexts, explicit buffer arguments, and explicit lifecycle functions are used
instead of leaking Rust-owned state across the C ABI.

An adapter contains only argument translation, lifecycle ownership, external
callback registration, and forwarding. It contains no controller, radio, audio
processing, configuration, or node-policy logic. The Asterisk module adapter
is the primary case. A PortAudio adapter follows the same rule whenever its
callback ABI requires a bridge.

The adapter/core connection uses the stable C-compatible descriptor and
function-table contract defined by ADR 0022, not the private Rust ABI of a
`dylib`. Its selected product composition requires every listed adapter to be
present and ABI-compatible. An adapter replacement activates only at a
controlled process restart or Asterisk module reload after callbacks and
contexts have stopped; live code unloading or hot replacement is not allowed.

An adapter is removable by removing its outer composition, package, and
configuration selection. Removing it must not require a change to internal
Rust component code. Real-time adapter callbacks remain lock-free, do not
allocate, block, log, or execute processes, and do not permit a panic to cross
the C ABI.

## Consequences

Each external integration can be versioned, tested, packaged, replaced, or
retired independently. Internal Rust components remain reusable by the
standalone controller and do not accumulate Asterisk, PortAudio, or other
C-library dependencies.

The project accepts the packaging and ABI-test cost of one small shared object
per C-facing integration in exchange for strict separation of concerns. ADR
0021 supersedes ADR 0020's public-boundary shim placement; ADR 0020 continues
to govern private Rust `dylib` component boundaries. ADR 0022 applies the same
separation rule in the other direction, from Rust to external C dependencies.
