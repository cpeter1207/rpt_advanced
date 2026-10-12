# Move existing functional implementations into released shared libraries

Status: Draft for written review. The extraction scope and permission to create,
validate, merge, and release prerequisite libraries are approved. No production
implementation or node deployment is authorized by this document alone.

## Outcome and scope

Complete ADR 0011's initial radio and controller component extractions without
changing behavior. Move the implementations, their developer documentation, and
their tests. Add or change code only where a shared-object boundary requires it:
descriptor/lifecycle glue, argument translation, thin consumer clients, and
build, test, package, and workflow integration.

The approved components are squelch, CTCSS, DCS, portable GPIO signaling, tone,
Morse, DTMF, message templates, and generic scheduling. Existing product shared
objects and the radio integration library remain their owners' composition
boundaries. An ordinary private Rust crate does not need a new library merely
because it is a crate.

Do not implement other wishlist features, extract configuration or directory
policy, introduce DTMF PCM generation that does not exist today, or change node
configuration. Do not install anything on 524950 during this work.

## Implementation provenance

Paths below are relative to the sibling repositories under `2026-08-30`.
Record the exact source commit and original path in each new repository before
moving code. Preserve license notices. Keep the originating implementations in
their consumers until the replacement library passes its gate and is released;
then replace them with thin dynamic clients and remove the duplicate source.

| New repository | Implementation to move | Behavior that stays in the consumer |
| --- | --- | --- |
| `librptadvsquelch` | `librptadvradio/src/micor_squelch.rs` and VOX hold advancement in `receive_path.rs` | Receive filtering, metering, carrier-source selection and radio orchestration |
| `librptadvctcss` | `librptadvradio/src/ctcss_receive.rs`, `ctcss_transmit.rs`, tone table and their tests | Receive conditioning, `ctcss_render_state.rs`, PTT/timer policy and output routing |
| `librptadvdcs` | `librptadvradio/src/dcs_receive.rs`, `dcs_transmit.rs` and their tests | `dcs_turnoff.rs`, radio timer policy and output routing |
| `librptadvsignaling` | Pulse publication/evaluation in `rptadv-gpio-adapter/src/lib.rs`; binary-channel and RTX sequencing in `src/parallel.rs` | CM119 HID, EEPROM, libusb, ppdev/raw I/O, device lifetime and service ownership |
| `librptadvtone` | `rpt_advanced/rust/core/src/audio/tone.rs` and `tone_tests.rs` | Telemetry selection, mixing, gain policy and `AudioSource` integration |
| `librptadvmorse` | `rpt_advanced/rust/core/src/audio/morse.rs` and `morse_tests.rs` | Identifier selection, priority and fallback policy |
| `librptadvdtmf` | `rpt_advanced/rust/core/src/audio/dtmf.rs` and `dtmf_tests.rs` | Command collection, authorization, command-scoped muting and peer routing |
| `librptadvtemplate` | Scanner, validation and bounded expansion in `rpt_advanced/rust/core/src/template.rs` | Named substitutions, allowed values, authorization and message selection |
| `librptadvschedule` | Civil-time, recurrence and window calculations in `rpt_advanced/rust/core/src/schedule.rs` | Scheduled actions, clocks/time zones, links, retries, warnings, macros and reload policy |

Move implementation-specific tests with their implementation. Retain consumer
composition and integration tests. Split mixed tests only at that ownership
boundary; do not duplicate an implementation to keep a test compiling.

## Shared-object contract

Each new component starts with ABI/SONAME 1, an independently versioned release,
and Debian runtime and development packages. Reuse the existing radio-library
scaffold and shared-library workflows, reducing them to the component's needs.
Use public Rust `cdylib` artifacts implementing the stable C-compatible contract
of ADRs 0011 and 0022, not the unstable Rust ABI as a public package interface.
Clarify this terminology in the affected ADRs without changing their policy.

Each component exports one immutable descriptor containing structure size, ABI
version, capability identity and only the required function pointers. Use opaque
handles for owned state and explicit C-compatible buffers, scalars and callback
arguments. Do not export Rust layouts, collections, panics or a generic API escape
hatch. Preserve existing error outcomes through narrow error translation.

Consumers use the released dynamic object, with minimum-compatible dependencies.
No vendoring, static archive, Cargo path/git dependency embedding a released
implementation, or fallback copy of that implementation is permitted. Verify
ELF dependencies, SONAMEs, headers, pkg-config files and Debian dependencies.

Validate and resolve descriptors during setup. Allocate handles and workspaces
outside audio callbacks. Processing remains allocation-free, lock-free and
nonblocking; it cannot load code, execute a process, log or perform device I/O.
Destroy handles only after their users have stopped. No hot library replacement
is introduced.

The radio's existing session descriptor and public ABI 4 can remain unchanged:
the new dependencies are private members of its opaque sessions. CTCSS and DCS
retain independent receive and transmit ownership. Keep transactional state-copy
semantics in the receive frontend; replacing a copied value with an aliased
mutable handle is not a valid mechanical move.

## Behavior-preserving seams

### Radio functions

- Move the MICOR comparator sample update and coefficients unchanged. Keep its
  existing settling and strong-loss behavior. Move the existing VOX hold update,
  passing its already-computed qualification and sample-offset data. Leave the
  shared envelope/meter primitive in the radio library so tuning measurements
  do not acquire a duplicate implementation.
- CTCSS receive still consumes conditioned 8 kHz mono; transmit still generates
  native 48 kHz mono. Preserve the frequency table, truncation behavior, phase
  reversal qualification, 55 Hz tail detection, carrier-loss reset and decoder
  sleep behavior. Do not move radio-side conditioning into the detector.
- DCS receive still consumes 48 kHz interleaved stereo using the left channel;
  transmit remains 48 kHz mono. Move correction, polarity, timing and tail
  generation intact. Preserve the distinct phase-preserving prepared-config
  update used by live reload; it must not become a resetting configure call.
- Portable signaling retains pulse publication order, generation tracking and
  per-bit replacement. Deadlines still begin when the service observes a
  publication. The hardware adapter supplies time and pin-write/delay callbacks.
  Preserve RTX bit order, initial/subsequent delays, binary selection and
  existing power-bit behavior. No Linux transport moves into this library.
- Do not consolidate the differing PCM conversion helpers: their current
  rounding and truncation rules are observably different.

### Controller functions

- Tone keeps its existing grammar, duration and segment limits, phase progression,
  dual-tone gain and silence behavior. Morse keeps its alphabet, fractional
  sample timing, spacing and phase resets. Only replace Morse's existing
  waveform arithmetic with an equivalent call to the tone library. Do not encode
  Morse as tone-sequence text, which would change timing and limits.
- DTMF retains detector windows, digit completion order, carrier-loss completion
  and sample muting. A borrowed callback carries completed digits across the ABI.
  No command parser, authorization or new generator is added to the library.
- Templates receive the caller's allowed keys, conservative expansion widths,
  values and output bound. Preserve case-sensitive `${name}` syntax, UTF-8 byte
  counting, malformed/unknown substitution errors and ordinary literal `$`/`}`
  handling. Keep the controller's seven existing keys and 127-byte bound.
- Scheduling receives caller-supplied civil time. Preserve date validation,
  weekday/date selection, overnight windows, overlap and next-occurrence
  calculations. Preserve the existing `(date identity, remaining milliseconds)`
  result meaning. The library does not read a clock, launch a task or decide a
  link action.

Keep current Rust-facing method shapes in thin clients wherever practical.
Preserve clone/copy/equality behavior actually used by callers. Do not publish
test-only internals or introduce general-purpose extension mechanisms.

## Dependency and release sequence

1. Reuse and extend the companion workflow/package-index integration needed for
   these repositories. Workflow maintenance must remain independently testable.
2. Bootstrap each source repository and establish its required PR gate before
   production implementation. Bootstrap commits are not release candidates.
3. Move squelch, CTCSS, DCS and portable signaling in ADR 0011 order. For each:
   move tests, add boundary checks, pass its PR gate, merge and release, then
   switch the owning consumer to the released library.
4. Repeat for tone, Morse, DTMF, templates and scheduling. Morse dynamically
   depends on the released tone library. Independent preparation may run in
   parallel; consumers must not depend on unpublished implementations.
5. Publish the signed package index and refresh affected native quality images
   and dependency checks before downstream gates use the new packages. Reuse
   existing authenticated index dispatch, without embedding credentials.
6. Finish consumer gates and update architecture status with concrete repository,
   package and dynamic-link evidence. Commit and push requested work; do not
   claim conditional future component directions are implemented.

No standalone or adapter behavior is redesigned during this sequence. Loading
and packaging failures must remain visible; missing libraries must not trigger
silent in-process substitutes.

## Verification and acceptance

Run each moved component's current tests first as its behavior baseline. Reuse
those cases as the provider tests, then add only tests needed for the ABI and
integration. Before replacing a consumer, compare baseline and shared-library
outputs for the same input and callback partitioning, including exact PCM where
the existing algorithm is deterministic. Preserve state and phase across reload
and ownership handoffs.

Required boundary checks cover descriptor identity/size/version, valid lifecycle,
buffer/error handling, missing/incompatible dependencies, staged installation,
dynamic linkage and artifact contents. Retain consumer tests for real signal
paths, signaling tails, command/muting behavior, scheduling and configuration
reload. Store temporary comparison and coverage evidence under ignored `.work/`.

Formatting, lint and static analysis pass before pushes. Full GitHub PR gates
cover documentation, build/tests/install/packages on native Debian 13 amd64 and
arm64, with 100% production line and branch coverage on amd64 only. Releases
use already-validated merged main revisions. Do not run a local full gate or add
automatic Debian 12 work. Update developer documentation before commits and
operator-facing package documentation before PRs.

Completion means all nine required implementations reside in their released
libraries; consumers dynamically use them with the former implementations
removed; required gates pass; and ADR statuses accurately distinguish these
completed boundaries from explicitly future requirements.
