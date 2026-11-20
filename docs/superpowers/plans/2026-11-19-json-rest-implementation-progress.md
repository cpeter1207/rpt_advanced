# JSON/REST implementation progress

Plan: [updated checklist](2026-11-19-json-rest-implementation-checklist.md).
Authority: [design](../specs/2026-11-18-json-rest-configuration-design.md),
including the 2026-11-19 instruction to warn and ignore unknown JSON keys and
unsupported values.

## Execution environment and inputs

- Development host: `radio.onionring.rocks`, source workspace
  `/home/codex/rpt-development/json-rest-configuration`, branch
  `codex/json-rest-configuration-20261119`.
- Controller base: `8188d5f0e6a738ac38e04e6fea2ba64c150e5ce9`, package
  `0.1.0-alpha24`, Rust 1.88, product descriptor ABI 7 / product DSO 2.
- Current compatible USBRadioPlus input: release `v0.1.0-alpha35`, source
  `b7dc3d35a242c4bdfea7166b6f959539d500a363`, package `0.1.0~alpha35`, product
  ABI 1, native request ABI 4 and direct-callback ABI 6. Its public INI format
  remains supported. The older alpha34 checkout is not the release input.
- Current schedule input: manifest source
  `9f4c39c2f1f0956c2c8ae3afbb34edf9dd30edc9`, release tag
  `v0.1.0-alpha.2` at `1635b0130b271f26f0e913ee46511259a709ce49`, ABI 1.
- Other provider source/package pins remain authoritative in
  `.github/dependencies.json`; no guessed future provider release or minimum
  version is introduced. The typed radio and hron extensions must be released,
  verified and pinned before their consumers are activated.
- Maintained workflow source: `rpt_advanced-workflows` revision `3110bc4`.
  Its constrained launcher pulls the latest image and records its digest on
  each run. Initial baseline image:
  `ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest` at
  `sha256:c3800ed0094d0d01284b70481d3189a76044042b2900594115db2e2a41f2d0fa`.
- Use the existing one-CPU/1536-MiB constrained runner, serial tests, reduced
  debug information and no incremental compilation. The host initially has
  3.9 GiB of free disk. Do not prune unrelated worktrees, images or containers.
- Iteration logs, RED/GREEN evidence and the execution ledger live under the
  ignored `.work/json-rest/` directory on the development host.

## Preflight decisions

| Boundary | Finding and implementation decision |
| --- | --- |
| Aggregate schema / modular models | The aggregate schema remains a logical inventory. Extract resource-local profiles; do not deserialize its operation envelope as disk state or claim it is the HAL API. Empty ordinary override objects are valid. |
| JSON input / canonical schemas | Warn, ignore and fall back for unknown keys and unknown/invalid/unsupported setting values. Canonical schemas describe normalized accepted/output values. Malformed JSON, duplicate members and unsafe structural/lifecycle relationships are errors when no safe deterministic candidate exists. Core collects value-free warnings; hosts own logging. |
| Scope identity / settings inheritance | Names, local keys, UUIDs and parent selectors stay local. `enabled`, `callsign` and `language` inherit. Arrays replace; named definition objects merge recursively. |
| Protocol defaults / live instances | Defaults do not attach protocols. EchoLink, AllStarLink and phone patch have independent optional instances; shared peer processing is separate. |
| Mailbox override / deletion | A local mailbox shadows a complete identity. Removing it reveals the complete same-key site identity if present, otherwise disables it; never merge or reuse the removed local hash/profile. No tombstone is introduced. |
| Controller / provider input | Remove controller INI at the coordinated cutover. Preserve USBRadioPlus INI; reuse typed native fields and add only the missing checked preparation inputs. Do not serialize controller JSON back through INI. |
| Provider compatibility | Preserve required supported consumers, not compatibility-only obsolete alpha code. Check descriptor sizes/versions and package cohorts before activation. |
| Scheduler transition policy | Recurrence pairing, overlap, missed checks and old permanent-route ownership remain explicit checklist prerequisites before scheduler implementation. No scheduler behavior is changed by the first resource-model slice. |

The DTMF preflight compared all 78 schema defaults with the published inventory:
77 code-derived mappings plus phone-only `*99`; the inactivity default is
3000 ms. No mapping changes are needed for this foundation.

## Code ownership map

| Responsibility | Existing implementation owners |
| --- | --- |
| Shared JSON syntax | `rust/json/src/lib.rs`; used by core configuration, product replies, standalone control/registration and speech model input |
| Configuration and effective settings | `rust/core/src/config/{document,parse,scope,schema,settings}.rs` and domain files `site.rs`, `echolink.rs`, `phone_mailboxes.rs`, `dtmf_macros.rs`, `messages.rs`, `tails.rs` |
| Shared lifecycle and inspection | `rust/product/src/{lifecycle,configuration,configuration_radio,host}.rs` |
| Standalone startup and radio preparation | `rust/standalone/src/{foreground,configuration,native_configuration,radio_activation,radio_host,providers}.rs` |
| Asterisk composition | `rust/asterisk/src/{lifecycle,product,services}.rs` and its CLI modules |
| Local maintenance/API replacement | `rust/standalone/src/local_control.rs`, product coordinator/control boundaries and CLI/tuner consumers |
| Directory resolution | Product `directory.rs`, standalone `directory.rs`, Asterisk `link/directory.rs` |
| Radio provider preparation | USBRadioPlus `rust/product/src/{native_configuration,native_processing,native}.rs` and `rust/product/include/usbradioplus_product.h` |
| Scheduling | Core `schedule.rs`, `runtime/scheduler.rs`, `runtime/link_schedule.rs`; `librptadvschedule` provider header/implementation |
| Commands and media | Core `command.rs`, `dtmf_macro.rs`, `phone_dtmf.rs`, `messages.rs`, `composition.rs`; product `control.rs`, `radio_control.rs`, `media.rs` |
| Packaging and quality | Controller `Makefile`, `debian/`, thin GitHub workflow callers; maintained implementations in `rpt_advanced-workflows` |

## First implementation milestone

Implement the control-plane JSON resource foundation with TDD: owned JSON
decoding, warning diagnostics, local scope metadata and sparse inheritance.
Retain the existing live configuration path until the resource store and typed
provider preparation are complete together. This foundation must not restart
connections, interrupt local/phone audio or alter jitter/PLC behavior.

Baseline verified on the development host: **112 existing configuration tests
passed**, with no production edits, using `cargo test --locked -p
rpt-advanced-core --lib config::` in the maintained image.

The first resource-foundation milestone is verified; the remaining implementation is pending. Unfinished checklist
items remain unchecked. Node milestones build/test in a home-directory
workspace without changing installed binaries or live configuration.

## Resource-foundation implementation evidence

The source now includes a pure internal JSON decoder, sparse local/effective
site and port metadata, exact port keys, recursive object/array inheritance and
whole-record mailbox shadowing. Unknown keys and invalid optional metadata
values produce value-free warning records and fall through to inherited/default
settings. Host logging for the new configuration path remains part of the later
store/startup cutover.

Existing active JSON wire readers use the same decoder so Serde feature
unification cannot reinterpret literal object members as numbers or strings.
Speech model types and encoded registration details have real-consumer
regressions. No radio/device, jitter, PLC, connection or installed configuration
owner was changed.

Development-host verification passed: **846 tests** across the complete core
and shared-JSON suites plus selected product, standalone and speech suites;
repository formatting, lint and static analysis (including Cppcheck,
Clang-Tidy and workspace Clippy) passed. All three review follow-ups are
addressed. This is not the complete native pull-request coverage/package gate.

The original DTMF integration fixture forbade a rate the released alpha4
provider supports; untouched-base reproduction confirmed the mismatch. Its
negative fixture now uses zero, rejected by both provider versions. The test
image's newer IAX header was aligned inside the disposable container to the
manifest's source-equivalent official alpha9 packages, with verified checksums.

Native node verification passed: **124 configuration tests**, zero failures,
on Debian 13 arm64 at source commit
`229209b3e3cb7e93967b2512b09e60b6ca39e2be`. The Rust 1.88 compiler is isolated
under `/home/asl3/rpt-development/toolchains/rust-1.88.0`; source and test
artifacts remain in the node user's home workspace. The existing standalone
controller was active during and after the build; no installed binary or live
configuration was changed.

The first milestone is complete. Execution resumed for milestone 2 at the user’s request.
Storage, full resource profiles, HTTP/HAL, credentials, runtime activation and
controller INI removal remain unchecked. USBRadioPlus INI is preserved. The
complete native pull-request coverage/package gate remains pending.

## Second milestone: resource identity and ownership

**Completed, 2026-11-19.** Assembled an in-memory modular candidate from
identified captured JSON bytes. The expected registry owns UUIDs, exact port
keys, resource kinds and scope/owner bindings. Metadata remains typed; other
section bodies are explicitly unvalidated staging inputs, not public HAL output
or runtime-ready settings.

Known ordinary section slots keep their registry identity when a file is absent
and supply an empty local override. All ordinary sections can have site defaults,
including control, memories and scan lists. Site protocol documents are defaults;
only supplied port protocol documents establish independent AllStarLink, EchoLink
or phone-patch presence. Disabled port settings do not erase configured presence.

The selected ordinary file shape is `id`, `kind`, `configuration`, plus the
exact `key` on port metadata. Owner/scope come from typed capture and registry
context. Extra owner/scope file fields warn and are ignored. This does not add
fields to the authoring contract or apply a configuration wrapper to flat events.

Filesystem capture, locks/fingerprints, complete setting profiles, public
serialization, private records and activation remain later steps. Secure capture
must be implemented with those ownership/bounds requirements together; no partial
file reader or live INI replacement is introduced by this model milestone.

### Second-milestone implementation and host verification

The core now contains `ResourceId`, `ResourceRegistry` and `ModularCandidate`
under `config::resources::candidate`. Metadata resources share their scope UUID.
The immutable registry binds exact keys and unique scope/kind slots; assembly
cross-checks captured envelopes and fails without partial state. Missing known
ordinary bodies remain empty overrides with their existing identity. Only an
explicit port protocol capture creates protocol presence, even when the port
is disabled. Site protocol defaults create no memberships.

Raw bodies remain opaque staging data; typed metadata reuses the existing
normalizer and inheritance. Unknown owner/scope envelope fields warn and cannot
reassign ownership. The `uuid` parser/formatter adds one locked package
(1.26.1, default features disabled), with no unrelated dependency upgrades or
new native ABI. UUID generation remains part of future create/import work.

All six new behavioral tests failed before implementation. Host verification
passed **130 configuration tests**, then **829 complete core/shared tests**,
Rustdoc with warnings denied and the full fast formatting/lint/static-analysis
gate in the maintained constrained Debian 13 image with the pinned IAX overlay.
Review approved specification and production quality. One nonblocking test-only
readability note is deferred: two malformed-JSON table rows contain coordinates
that their variant/redaction assertions intentionally ignore.

Native node verification passed: **130 configuration tests**, zero failures,
on Debian 13 arm64 using isolated Rust 1.88 at source commit
`ef4aa261a2587581d30e29cfe3f01661d9dc5fd9`. The controller service was active
before and after the build. Both source workspaces are clean; no installed
binary, connection, DSP or live configuration has been changed.

Milestone 2 is complete; execution resumed for milestone 3 at the user’s request. Secure
filesystem capture, complete domain profiles, durable storage, credentials,
HTTP/HAL and the coordinated controller INI cutover remain pending. USBRadioPlus
INI remains supported. The complete native pull-request coverage/package gate
has not been claimed by these scoped model checks.


## Third milestone: guarded capture of registered working files

**Completed, 2026-11-20.** Added bounded read-only capture under a preprovisioned
service-owned edit lock, with protected descriptor-relative no-follow access,
owned byte snapshots and SHA-256 content fingerprints for guarded rechecks.
The product adapter maps registry UUID/kind bindings to fixed paths and passes
those same bytes to the existing candidate assembler. Local keys and names never
become filesystem paths. No public serialization or native ABI is added.

This selection is deliberately limited to registered metadata, ordinary sections
and protocol defaults/instances. It does not discover new ports, named
collections or scheduler events; the complete allowed inventory and working-tree
reload remain later work. Reads do not create, repair, publish or activate files.
Caller-supplied bounds introduce no new configuration settings or policy defaults.

Native provider prerequisites were downloaded, checksum-verified and extracted
under the node user's home workspace: USBRadioPlus alpha35 and manifest-matched
IAX alpha9. Installed packages and the running service remain unchanged.

### Third-milestone host verification

Ten protected-filesystem tests and four real product-adapter tests failed
against inert implementations, then passed after implementation. The final
maintained constrained Debian 13 host gate passed **886 tests**, zero failures,
Rustdoc with warnings denied and full fast formatting/lint/static analysis,
including Cppcheck, Clang-Tidy and workspace Clippy. The freshly pulled image
resolved the recorded `c3800ed0` digest and used the verified matched IAX overlay.

Independent specification/code review approved the slice. The native checkpoint
then exposed a test-fixture socket path exceeding Linux's pathname limit under
the home-directory TMPDIR. The same failure was reproduced in the maintained
container before replacing timestamp names with compact process/counter names.
All ten capture tests passed under the exact 72-byte TMPDIR after the fix; the
scoped re-review and full fast checks passed again. This also resolved the
initial review's minor fixture-uniqueness note. Production code was unchanged.

The Linux host-glue crate reuses locked ring 0.17.14 and adds rustix 1.1.6 for safe
filesystem/locking APIs (transitive errno 0.3.15 and linux-raw-sys 0.12.1).
Existing dependency versions are unchanged; core remains independent of host
filesystem crates.

### Third-milestone native checkpoint

Native Debian 13 arm64 verification passed **886 tests**, zero failures, at
published source `9a5325e8d59318ee7d13ac88e3713e43bc6a01ad` with isolated
Rust 1.88. This includes the complete core, shared JSON and peer-support suites
and the four product capture tests. The controller service was active before
and after the build; no installed binary, package, live configuration or service
state was changed. Native temporary fixture DSOs used the executable home
filesystem because the node's `/tmp` is mounted noexec.

The native log and receipt remain under the node workspace's `.work/` directory;
the log SHA-256 is
`408b44e7647a75c4aec96dc020b7b9507cd1c6f31109b912a5ea4e84e8cb94d3`.
Host reports, review and exact commands remain under `.work/json-rest/`.

Milestone 3 is complete and execution pauses per the checklist. Capture covers
only registered paths. Complete discovery, resource profiles, durable accepted
state/publication, credentials, HTTP/HAL and the coordinated controller INI
cutover remain pending. USBRadioPlus INI is preserved. The complete native
pull-request coverage, Doxygen, packaging and staged-install gate is still
required before merge and has not been claimed by this milestone.
