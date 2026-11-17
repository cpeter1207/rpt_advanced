# Bounded SIP parser implementation plan

> Execute natively with test-first changes and an independent final review.

**Goal:** Supply an experimental, uncomposed SIP framing/header/URI prerequisite without activating a provider.

**Architecture:** One owned Rust parser in the existing shared `rptadv-peer-support` leaf is reusable by both hosts. It borrows admitted wire bytes and indexes spans, never opens a socket or retains transaction/call state. The future provider retains its own admission reservations and lifecycle.

**Tech stack:** Safe Rust, standard library, existing Rust 1.88 workspace. No added dependency, native parser, global allocator hooks, public C ABI, DNS, cryptography, DSP, or media change.

**Spec:** [Phone design](../specs/2026-11-09-phone-patch-and-half-duplex-design.md), Task 4, ADRs 0022/0044; reviewed parser refinement 2026-11-12.

## Bounds and interfaces

The immutable admitted message buffer is at most 32768 bytes; the start-line and headers including their terminating CRLF occupy at most 16384 bytes; the body is at most 16384 bytes. At most 128 logical headers and 4096 unfolded bytes per field are retained. These are implementation admission limits, not SIP limits. The fixed header index, parser/framing state, and caller's reusable 4096-byte unfolding scratch together must fit 8192 bytes, separately from admitted wire storage. Parsing must allocate zero heap bytes.

`Parser::parse(wire, Transport)` returns a borrowing `Message` or redacted `ParseError`. Only TCP can return `Incomplete`. `Message::consumed` includes discarded UDP suffix bytes, but excludes TCP pipelined bytes; `discarded` makes UDP behavior explicit. A borrowed-buffer `TcpDecoder` owns its buffer lease for its whole lifetime, incrementally scans each received byte, consumes only one frame, and resets offsets before reuse. Its message borrow prevents reset while any view remains live.

SIP/SIPS URI syntax is separate from `require_cleartext_routing`: valid SIPS parses with its explicit scheme and unsupported secure routing fails closed. URI headers remain syntax, never synthesized request headers or authorization. Existing account/provider/host ownership and `phone::media` remain unchanged.

## Review focus

- Duplicate compact/long Content-Length or overflow cannot change framing.
- Arbitrary TCP fragmentation and buffer reuse cannot reuse stale offsets or discard a pipeline.
- Quoted commas, escaped bytes, nested comments and folded values cannot split a header incorrectly.
- URI userinfo delimiters, IPv6, percent escapes and name-address ambiguity cannot change the target.
- Resource limits apply before retained state grows; parse/scratch layout and allocations are measured.

## Steps

### 1. Frame and bound messages

Files: `rust/peer-support/src/sip/mod.rs`, `tests/sip.rs`, shared leaf module declaration.

- [x] Add literal UDP/TCP length, binary body, leading CRLF, compact name, invalid line, duplicate length and size/count tests; run and observe the missing implementation fail.
- [x] Implement fixed span index, message views, checked length and bounded incremental decoder; run targeted tests until green.
- [x] Add fragmentation and decoder-reset regressions plus index/workspace layout assertions; verify and make one coherent local recovery commit.

### 2. Parse header syntax without generic comma splitting

Files: `sip/headers.rs`, `tests/sip_headers.rs` and authoritative RFC fixtures.

- [x] Write and observe failing tests for case/compact aliases, folded quoted fields, escaped/control bytes, repeated list order, comments/name-addresses and authentication commas.
- [x] Implement bounded unfolding and field-aware list/name-address accessors. Add typed accessors only required by this profile.
- [x] Run targeted checks and commit the verified prerequisite.

### 3. Parse SIP URI syntax and qualify supported routing

Files: `sip/uri.rs`, `tests/sip_uri.rs`, fixture provenance and ADR/selection updates.

- [x] Write and observe failing RFC 3261/5954/8217 cases and independent RFC 4475/5118 fixtures, with invalid escapes, host/port/parameter/header and secure-route cases.
- [x] Implement borrowed component views, separate profile rejection, and no URL-parser shortcut.
- [x] Verify zero allocation across framing/header/URI paths with a test-only counting allocator, boundary matrices, formatting, Clippy/Rustdoc and the affected crate suite.
- [x] Finish fresh independent review and targeted branch coverage, commit remaining evidence and report full-gate/provider acceptance separately.

## Validation constraints

All builds/tests use the freshly pulled maintained Debian 13 image and labeled launcher with `RPTADV_CONTAINER_PROFILE=constrained` after the lead grants the sole server slot. One CPU, 1536 MiB, no extra swap, 256 PIDs, serial execution and a bounded deadline. No host build, deployment, live account/DNS/call/RF test or toolchain upgrade. Full native gates remain required before merge; targeted green does not claim that gate.

## Verified recovery checkpoint, 2026-11-12

After observed RED regressions, raw escaped-CRLF is rejected before unfolding,
including generic parameter quotes and comments. `Header::name_addr` supplies a
checked wire-to-address path; `unfold` remains explicitly normalization-only.
Header parameter duplicates compare case-insensitively without URI decoding.
URI matching applies section 19.1.4's reserved-byte exception, and encoded
transport names cannot bypass the separate supported-profile check. Response
Reason-Phrase validates section 25's octet grammar without Unicode/percent
decoding. Full header capacity without a terminator fails immediately.

Scoped formatting, Clippy/Rustdoc with warnings denied, and all 91 peer-support
crate tests passed in the constrained native amd64 container. The five SIP
checks comprise 14 framing, 17 header, 11 URI, one allocation and one index-layout
test (the latter among nine crate unit tests). The expanded allocation guard
observed zero allocations; explicit decoder, scratch and all exposed view sizes
total 5488 bytes, with the 32768-byte wire buffer separate. This is not a compiler
stack-peak or process-RSS measurement. Full native gates remain outstanding. Independent source review and targeted
parser coverage are complete, including the corrected URI comparison and lexical
invariants. No SIP provider is
activated and Task 4's signaling/session acceptance remains open.

The final review correction compares URI components as tagged single-decoded
units: an escaped reserved byte stays distinct from both its literal byte and
a decoded percent followed by literal hex characters. Thus `p%2f`, `p%252f`
and `p/` remain distinct, while `%2F` and `%2f` match. Raw field validation
precedes folding; normalized list scanning cannot introduce a new lexical
error. The CSeq method cannot be empty after whole-value boundary trimming and
a successful separator lookup. Removed only those redundant checks, retaining
empty/trailing list errors and all actual syntax validation.

## Parser coverage closure

The final test-only addition passes a real Contact list containing
`<sip:a,b@host>,<sip:c@host>` through framing, list separation and typed URI parsing.
It closes the URI-comma branch without changing production code. The reviewed
parser has 912/912 executable production lines and 412/412 branch arms covered:
framing 329/329 and 126/126; headers 347/347 and 162/162; URI 236/236 and 124/124.
The unchanged exporter retains 13 objects and one profile, excluding only the
repository's standard test/build fixture sources, with no production exclusions.

The complete affected-crate suite passes 91 tests. This is not whole-crate 100%
coverage: the same report measures peer-support at 2318/2324 production lines
and 748/750 branch arms, with residuals in untouched EchoLink host/account files.
The full controller/native platform gate remains pending. No provider session,
media behavior, authorization/RF rule, vendor source or toolchain requirement
changes as part of this prerequisite.
