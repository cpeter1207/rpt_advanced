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


## Fourth milestone: durable immutable artifact installation

**Completed, 2026-11-20.** Extended the protected host storage boundary with
bounded installation of immutable public objects and manifests. Typed targets
select only those namespaces. Reuse the existing guard, no-follow descriptor
checks and locked libraries. Protected object/manifest parent directories must
already exist; bootstrap and resource creation will provision/sync them later.

Write an independent exclusive staging file, sync it, publish its final name
without replacement and sync the parent directory. An exact-byte retry confirms
durability; conflicting bytes are preserved. A failure after publication reports
uncertain durability, and no final artifact is deleted to pretend nothing
happened. Returned receipts and diagnostics remain value-free by default.

This milestone does not choose a manifest/state wire format, install private
records, write a journal/state pointer, publish working files, accept an opaque
candidate or activate runtime settings. Complete inventory/discovery and the
transaction protocol remain pending. Installed unreferenced artifacts do not
constitute accepted configuration.

### Fourth-milestone host verification

All seven installer behavioral groups failed against inert scaffolding before
implementation and passed after it. The complete affected peer-support suite
and product capture regressions then passed **64 tests**, zero failures.
Rustdoc with warnings denied and the full fast formatting/lint/static gate
passed, including Cppcheck, Clang-Tidy and workspace Clippy. The maintained
constrained runner freshly pulled digest `c3800ed0`; exact commands and evidence
remain under `.work/json-rest/milestone-4-*`.

Independent specification/code review approved the implementation. Two minor
test follow-ups are recorded: the socket fixture approaches the native pathname
limit under long TMPDIRs, and the scratch-abort case does not directly exercise
the existing inode-identity cleanup safeguard. They were resolved by milestone 5. Full production coverage remains a
pre-merge requirement.

The only dependency change adds an edge to the already locked uuid parser,
with default features disabled. No new package version, native ABI, core model
or product caller is introduced. Existing working-file capture stays read-only.

### Fourth-milestone native checkpoint

Native Debian 13 arm64 verification passed **64 affected tests**, zero failures,
at published source `1e2ef0395d0638cf21a344babdb0bd5e996f9d37` using isolated
Rust 1.88. The complete peer-support suite and product capture regressions
passed, including the actual node's temporary-path conditions. The controller
service was active before and after; installed packages, binaries, live
configuration and service state were unchanged.

The native log and receipt are under the node workspace's `.work/` directory.
Log SHA-256:
`b2b278b45df0093d658f8dea3be80253d76f83c854d3b218e704ff9a1a0ec105`.
Host RED/GREEN, full affected checks and the approved review remain under
`.work/json-rest/milestone-4-*`. The two minor test follow-ups recorded in
this document and the execution ledger were subsequently resolved by milestone 5.

Milestone 4 is complete and execution pauses per the checklist. Protected
object/manifest parents must be provisioned before installation; the later
bootstrap/create path must synchronize that provisioning. Installed artifacts
alone do not accept settings. Full discovery and profiles, immutable reference
formats, private records, journal/state-pointer transactions, working-file
publication, HTTP/HAL and the coordinated controller INI cutover remain pending.
USBRadioPlus INI is preserved. Full production coverage, Doxygen, packaging and
staged-install validation remain part of the required pull-request gate.


## Fifth milestone: public manifest and verified readback

**Completed, 2026-11-20.** Added a typed reference-only public manifest and
protected, bounded readback of explicitly selected immutable artifacts. Freeze
format-1 fields as format_version/id/revision/site_uuid/port_keys/resources,
with complete version/object/sha256 reference triplets. Required metadata and
port protocol instances reference objects; empty ordinary slots retain UUIDs.
Private graphs are unsupported and cannot silently lose reachability.

The caller supplies trusted manifest ID, logical revision and digest. Product
owns the manifest registry and exact verified bytes, then assembles the existing
borrowed candidate on demand. This avoids a self-referential model. Logical
revision, object version and manifest identity have distinct types; raw port
keys remain exact JSON strings and never become paths.

This milestone does not select state, activate settings, complete profiles or
invent a private/journal/recovery format. The future selecting owner must bind
its manifest digest to accepted state. Root/object hash validation and storage
structure do not turn opaque section bodies into public or effective settings.

### Fifth-milestone host verification

Six core manifest groups, six host readback groups and five product loader
groups failed against inert implementations, then passed after implementation.
The final maintained constrained Debian 13 host gate passed **910 tests**, zero
failures, across the complete core/shared-JSON/peer-support suites and both
product capture/manifest suites. Rustdoc with warnings denied and the full fast
formatting/lint/static gate passed, including Cppcheck, Clang-Tidy and workspace
Clippy. Every run freshly pulled the recorded `c3800ed0` image digest.

Separate host and core/product specification/code reviews approved both tasks
with no findings. Both deferred M4 fixture notes are resolved: the socket uses
a short procfd binding, and an observed replacement at the owned scratch name
is explicitly preserved. The older M2 test-coordinate readability note remains
unrelated deferred work. No dependencies, package versions or C ABI changed.
The unreleased Rust API now calls the shared read/write cap ArtifactLimits;
there is no obsolete InstallLimits compatibility alias.

Reports, exact commands, RED/GREEN evidence and approved reviews remain under
`.work/json-rest/milestone-5-*`. Required profile validation, accepted state and
runtime activation are still separate from this reference/integrity validation.

### Fifth-milestone native checkpoint

Native Debian 13 arm64 verification passed **910 tests**, zero failures, at
published source `85a18357804d1924eb99a73cf8546eccd4af9416` using isolated
Rust 1.88. This includes the complete core/shared-JSON/peer-support suites and
both product capture/manifest suites. The controller service was active before
and after; no installed package, binary, live configuration or service state
was changed. All fixtures and compilation remain in the home workspace.

The native log and receipt are retained in the node workspace's `.work/`.
Log SHA-256:
`1281f60363f47dc66776a7495b1f579ac49b8f2eefb503338a0846891b12d97c`.
Host reports, approved scoped reviews and exact commands remain under
`.work/json-rest/milestone-5-*`.

Milestone 5 is complete and execution pauses per the checklist. Public manifest
selection still requires trusted ID/revision/digest input from its future owner.
The owned graph is staging data until profile/capability and private-state
validation. Full discovery/profiles, directory provisioning, private records,
journal/state-pointer transactions, working publication, HTTP/HAL and the
coordinated controller INI cutover remain pending. USBRadioPlus INI is preserved.
The complete native pull-request gate still requires production coverage,
Doxygen, packaging and staged-install validation before merge.


## Sixth milestone: sealed private records and systemd keys

**Completed, 2026-11-20.** Added a bounded XChaCha20-Poly1305 envelope and an
env-only systemd credential loader. The codec emits only version/key-ID/nonce/
ciphertext, authenticates trusted store/record/generated-version context and
key routing, and generates independent fresh nonce/version values. Keys and
plaintext have opaque zeroizing owners; no raw key construction is public.

Systemd's manager-owned named-ACL delivery differs from legacy password files.
The loader follows the documented credential namespace, bounded raw length and
pinned descriptor checks without requiring its files to be service-owned 0600.
Missing/invalid material is unavailable, never a replacement key or empty secret.

An independent libsodium reference fixture and generator/provenance are retained
under `.work/json-rest/milestone-6-reference-fixture*`. Only synthetic fixture
data and existing installed reference tooling were used. This stage does not
parse account payloads, publish private files, adopt private manifests, select
state or activate credentials. All live service/configuration paths stay unchanged.

### Sixth-milestone host verification

Fourteen crypto groups and ten synthetic loader groups failed against inert
scaffolding, then passed after implementation. The broader maintained host
run passed **934 tests**, zero failures, and affected Rustdoc with warnings
denied. Its lint stage caught only a Rustfmt assertion wrap after a test-only
redaction improvement. The owner applied that cosmetic formatting and checked
the workspace; the final complete fast gate then passed formatting, lint,
Cppcheck, Clang-Tidy and workspace Clippy. No behavior changed after the suite.

Both scoped specification/code reviews approved the implementation. Crypto had
no findings. One minor loader-test follow-up remains: directory replacement
also replaces the leaf inode; prepare a same-leaf replacement directory to
independently exercise the existing parent-identity guard before full coverage.
Live systemd delivery is outside these synthetic fixtures and remains a later
integration/deployment check.

RustCrypto chacha20poly1305 0.11.0 uses explicit alloc/zeroize features. Thirteen
package versions were added, all with declared Rust 1.85 support and compiled
on Rust 1.88. Existing package versions were preserved. The independent
libsodium fixture is embedded in tests; ignored generator/provenance files are
not runtime test dependencies. Opaque wrappers prevent printing the existing
zeroize buffer type's contents. No C ABI or installed runtime path changed.

Exact commands, source/fixture/package evidence, approved reviews and log
receipts remain under `.work/json-rest/milestone-6-*`. The final broad gate used
the verified task-local launcher and freshly pulled `c3800ed0` image digest.

### Sixth-milestone native checkpoint

Native Debian 13 arm64 verification passed **934 tests**, zero failures, at
published source `240cba5101040b8b5e518b50d38fcecd40f54cbc` using isolated
Rust 1.88. This includes the complete core/shared-JSON/peer-support suites and
both product capture/manifest suites. The independent crypto vector and all
synthetic key-loader tests passed on the native architecture. The controller
service was active before and after; no installed package, binary, live
configuration, actual service credential or service state was changed.

The native log/receipt remain in the node workspace's `.work/` directory.
Log SHA-256:
`bfd21c8703c20b64c8afc60eae07ec338e71cee6eadf0061436c30d4002f4451`.
Host commands, RED/GREEN, approved reviews, fixture provenance and formatting
repair evidence remain under `.work/json-rest/milestone-6-*`.

Milestone 6 is complete and execution pauses per the checklist. The loader's
same-leaf parent-identity fixture and live systemd-delivery integration remain
recorded follow-ups. Account payload/binding validation, encrypted object
publication, private manifest adoption, accepted-state transactions, directory
provisioning, full discovery/profiles, HTTP/HAL and the coordinated controller
INI cutover remain pending. USBRadioPlus INI is preserved. Full production
coverage, Doxygen, packaging and staged-install validation remain required
before merge; this milestone does not claim that complete pull-request gate.


## Seventh milestone: typed encrypted private artifact I/O

**Host implementation verified, 2026-11-20; native checkpoint pending.** Connected prepared sealed records to the guarded
immutable engine. Preparation owns one identity and exact encoded ciphertext;
installation admits only that typed input, so retries never reseal. Private
paths derive only from trusted record/version UUIDs. Public raw-byte target
constructors cannot select private storage.

Readback intersects artifact/sealed caps, verifies ciphertext hash and shape
before systemd key lookup, then authenticates before exposing opaque plaintext.
Key, digest, authentication and durability outcomes stay distinct and value-free.
The existing loader's parent-identity fixture is isolated using a precreated
same-leaf replacement directory as part of this dependency validation.

Resolved account profiles are not available in current opaque section bodies;
this step deliberately avoids inventing an ASL-only payload contract or silently
adopting a partial private graph. Account payload validation, private manifest
references and accepted-state transactions remain prerequisites before use.
No installed service or actual credential path is changed or inspected.

### Seventh-milestone host verification

Seven bridge groups failed against the inert implementation and then passed.
The full peer-support suite and product capture/manifest regressions passed
**106 affected tests**, zero failures. Affected Rustdoc with warnings denied
and the complete fast formatting/lint/static gate passed, including Cppcheck,
Clang-Tidy and workspace Clippy. Maintained constrained runs freshly pulled the
recorded `c3800ed0` image digest. No dependency/package/C ABI changes were needed.

Independent specification/code review approved Tasks 18/19 without findings.
The prior loader parent-identity fixture now preserves leaf identity/version;
it passed normally and failed in an isolated copy with only the parent guard
ignored, proving the test's sensitivity. Early test-harness TMPDIR/in-flight
scaffold setup failures were corrected before measured validation; their exact
scope and successful reruns are recorded in the component reports.

The bridge has no public raw private-target or decoded-envelope constructor.
Preparation preserves one ciphertext identity across no-replace/durability
retries; readback verifies bounded ciphertext before env-only key lookup and
returns authenticated opaque bytes. Payload/account validation remains separate.
Evidence is retained under `.work/json-rest/milestone-7-*`.
