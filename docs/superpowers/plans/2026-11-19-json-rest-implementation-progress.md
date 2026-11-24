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
- Initial compatible USBRadioPlus input: release `v0.1.0-alpha35`, source
  `b7dc3d35a242c4bdfea7166b6f959539d500a363`, package `0.1.0~alpha35`, product
  ABI 1, native request ABI 4 and direct-callback ABI 6. Its public INI format
  remains supported. The older alpha34 checkout is not the release input.
- Initial schedule input: manifest source
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
same-leaf parent-identity fixture was subsequently closed by milestone 7; live
systemd-delivery integration remains a later verification step. Account payload/binding validation, encrypted object
publication, private manifest adoption, accepted-state transactions, directory
provisioning, full discovery/profiles, HTTP/HAL and the coordinated controller
INI cutover remain pending. USBRadioPlus INI is preserved. Full production
coverage, Doxygen, packaging and staged-install validation remain required
before merge; this milestone does not claim that complete pull-request gate.


## Seventh milestone: typed encrypted private artifact I/O

**Completed, 2026-11-20.** Connected prepared sealed records to the guarded
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

### Seventh-milestone native checkpoint

Native Debian 13 arm64 verification passed **106 affected tests**, zero failures,
at published source `15b631664294bc3a7e5d36470311d3dfddeeb031` with isolated
Rust 1.88. The complete peer-support suite plus both product capture/manifest
regressions passed. The controller service was active before and after; no
installed package, binary, live configuration, actual credential or service
state changed. Compilation and synthetic fixtures stay in the home workspace.

Native logs and receipt remain under the node workspace's `.work/` directory.
Log SHA-256:
`c52b74ad7197486e52d67988a767234b81b86650810f0069174654de196afaef`.
Host RED/GREEN, fixture mutation proof, approved review and gate commands remain
under `.work/json-rest/milestone-7-*`.

The original encryption/key-loading implementation steps are now checked after
the typed persistence/readback path verified them end to end. Their account,
private-graph and startup/rotation integration steps stay unchecked. The M6
parent fixture gap is closed; live delivered-credential integration is still a
future acceptance test.

Milestone 7 is complete and execution pauses per the checklist. Trusted root/
store context and preprovisioned private directories remain caller prerequisites.
Authenticated opaque bytes are not validated account credentials. Account
payloads/resolved profiles, private manifest adoption, accepted-state/journal
transactions, provisioning/full discovery, HTTP/HAL and the coordinated
controller INI cutover remain pending. USBRadioPlus INI is preserved. Full
production coverage, Doxygen, packaging and staged-install checks are still
required before merge; these component checks do not claim that complete gate.


## Milestone 8: protocol profiles and JSON Patch

Typed modular profiles now resolve independent AllStarLink, EchoLink and phone-patch
settings from sparse site/port bodies. They preserve explicit attachment, metadata-only
ports, shared jitter source histories and stable configured account identities. Peer audio
has its own binding. Provider capability checks, private adoption and runtime activation
remain separate work.

The resource-local RFC 6901/6902 stage applies all six operations atomically to a sparse
authoring projection. Exact decimal tests avoid floating-point conversion. Neutral
write-only slots and opaque zeroizing companion edits preserve credential omission,
clear/mask and sequential path semantics without reading saved credentials. The shared
borrowed JSON decoder rejects duplicates and validates Unicode escapes.

Review closed an EchoLink canonical-host delimiter/length defect with typed directory
members and direct regression cases, plus direct Unicode-surrogate tests. Host evidence
covers 771 core unit tests, 88 core integration tests, one JSON test, five product wire
reply tests, 29 speech tests and 215 standalone tests (1,109 unique affected tests).
Strict Rustdoc and the complete push preflight passed, including formatting, Ruff,
Cppcheck, Clang-Tidy and workspace Clippy. Source revision
`df5db6462dd35130f8a89272ed368333f43c5714` also passed all 860 core/JSON tests
and 248 further affected tests in the native node home workspace (1,108 total).
The installed service remained active, and its configuration/artifacts were not changed.

One preexisting standalone foreground fixture tried to create a collision at the live
service's fixed `/run/rpt-advanced/radio-control.sock` path and was blocked by permissions.
That fixture remains covered by the isolated host/CI run rather than being forced onto
the active node. The remaining native bin/CLI/registration, product wire and speech
checks passed separately; the original failure log is retained.

The optional broader full-product run reached the existing 2,000-peer stress test and
hit the constrained runner's 1,536 MiB memory limit, confirmed by Docker's OOM event.
This is recorded as an incomplete full-product stress gate, not a passing full suite;
the required larger pull-request/platform gate remains pending. Current development
DSOs and the documented library environment were used for the affected host tests.

Continue through accepted-state transactions, typed provider joins, authenticated HAL
and runtime cutover. Intermediate milestone completion no longer pauses execution.

## Continuous checkpoint: public accepted state and authenticated transport

Source `3d4a0426e0acbb8a6e18efb7a6f8f7894911f8e1` integrates the reviewed public catalog/manifest/state/journal transaction and loopback HTTPS components. Both the constrained Debian 13 amd64 host and native arm64 home workspace passed 1,005 affected tests. Strict Rustdoc and the complete fast preflight passed. Native service health remained active; installed configuration and service files were unchanged.

The HTTPS component includes real nginx/Apache fixtures, bounded nonblocking coordinator admission, current mTLS authorization/expiry and secret-safe response projection. HAL routing, complete private graphs and runtime adoption remain pending. Durable on-disk confirmation is historical evidence, not current runtime activation.

Command preparation now covers the audited action inventory, all 78 default DTMF mappings, shared ordered macro actions, exact raw parameters/fixed values, source/owner/grant constraints and known static cycles. Two review rounds closed all ten findings. Final host verification passed 791 core tests and 88 integration tests, documentation, formatting, Clippy and full fast checks. Runtime collectors, macro execution and RX mute remain separate pending joins.

The hron schedule provider source `238811360fc34908a8ea9d4174bfe49d874d2a0e` passed its full required Debian 13 amd64/arm64 PR gate, including production line/branch coverage. USBRadioPlus typed preparation source `a771f76cdb144d6d245e625020844d55e970feb0` includes independent peer DSP without physical selection; targeted host/native tests passed. The final provider source `0283a035d3d33b7bea4ed351a800fae3e3fad9db` now passes its full required AMD64/ARM64 gate, including actual 100% production line and branch coverage. The final change is a test fixture; production matches `0ab539e0bda75c5f28d191acde950824f4ec7c9c`. Both provider PRs remain drafts; no release or installation is implied.

Continuous execution proceeds through typed radio settings, private account graph adoption and hron event scheduling. The authorized stopping point remains implementation/runtime cutover, before any separately authorized deployment.

## Continuous checkpoint: account-bound private graphs

The reviewed EchoLink proxy/site-account slice and encrypted account graph now integrate on the command checkpoint. Public configuration and encrypted credential edits use the same accepted manifest/journal/state pointer. The index validates scope/instance/slot/account ownership, omission/masks, complete mailbox identity and key policy. Unknown configured state is omitted when locked; production hosts must supply their separate systemd write-key policy.

Independent review closed reserved public credential-container leakage, mixed-key leaf acceptance and escaped-password scratch allocation. The integrated constrained Debian 13 amd64 run passed 1,035 affected tests, strict Rustdoc and complete fast checks. Published source `5e150dcd047f82187c46fc322856d1ea78cea160` also passed all 1,035 affected tests in the native arm64 home workspace. The installed service remained active and unchanged. Runtime credential adoption, HAL routing, stopped-writer rotation, backup and garbage collection remain pending.


The authorized workflow repairs are merged: shared-workflows PR 18 and controller-workflows PR 60 permit the exact validated manifest-selected package versions to replace newer image packages. Immutable source/package pins and validation remain enforced. Controller CI now passes that downgrade step; a transient GitHub release API HTTP 502 interrupted its retry before production checks, so the failed job was retried. USBRadioPlus passed the full ARM64 package/install gate; its reviewed admission/normalization coverage fixes are published in `0ab539e0bda75c5f28d191acde950824f4ec7c9c`, with the final deserialization fixture subsequently published in `0283a035d3d33b7bea4ed351a800fae3e3fad9db` and its complete gate passing.


## Continuous checkpoint: typed radio and calendar foundations

Published source `efa824b30b3b80f60933303ee18bed37ac54e1f1` integrates the reviewed RF/DSP/interface profiles, native typed station/peer preparation, public command/mailbox identities and hron event/calendar policy. These retain separate physical and peer owners, raw public authoring, capability failures, monotonic reservations and the four agreed occurrence policies. The local source boundary guard permits only the checked recurrence descriptor function-pointer forms; review closed direct C-import bypasses with regression fixtures.

The constrained Debian 13 AMD64 host and native ARM64 home workspace both passed 992 distinct affected tests: 830 core unit tests, 112 integrations, one JSON test, 35 product configuration tests, one host-zone test and 13 actual provider preparation tests. The child-process key fixture and an extra host rollback run make raw execution totals larger; earlier progress commentary counted the child fixture separately. Strict Rustdoc and complete fast checks passed on the development host. The native service remained active; installed files and configuration were unchanged.

The controller manifest now selects the released ABI3 PCM ring alpha6 required by USBRadioPlus, the recurrence provider alpha3 and typed-preparation provider alpha36. Reviewed shared-workflows PR 19 and controller-workflows PR 61 add the immutable USBRadioPlus source pin and isolated provider Rust 1.93 build while retaining the controller's Rust 1.88 baseline. Their validation gates and a separate actual AMD64/ARM64 SDK build/install proof passed. The user authorized both merges. Shared-workflows PR19 merged as `54475895ebd435bab6589e5da2c73e8d06384c99` and controller-workflows PR61 as `9f120a364be7bd415b8c743931497c3c57264352`; both main trees match the independently reviewed candidates. The proof-only PR62 must not merge. A fresh controller gate follows this recorded decision; the full controller gate has not yet passed.

Public HAL/store coordination, typed site defaults, Fluent/equipment/telemetry joins, the global command executor and real JSON process startup remain in progress. Serial/key-input and other documented native capability gaps must be completed before runtime cutover. This checkpoint does not activate modular JSON on the installed service.

## Continuous checkpoint: HAL coordination, Fluent and equipment

The reviewed coordinator now joins modular HAL routing, resource-local RFC6902 writes, atomic encrypted/private state, code-derived OpenAPI3.1 and per-boot protected operation receipts. Independent review closed selector/key-loss inhibition, indeterminate recovery and exact retry handling, private operand redaction and sequential PATCH behavior. Authenticated reads remain usable while activation is pending. Earlier component verification exercised real nginx/Apache upstream mTLS; the combined gate retains the separately staged proxy fixture as an explicit ignored test rather than claiming it ran without its packages.

Named JSON messages project into existing Fluent and the checked media compositor. The approved preparation retains installed-only references, same-candidate bindings, English/form fallback, checked Morse/media bounds and immutable runtime arguments. Site equipment now resolves cached GPIO/parallel observations, calibration and acquisition policy, preserves observation quality/time, validates candidate-wide actuator claims and avoids duplicate inherited fan owners. Incomplete site defaults remain authorable without a fabricated physical port.

The combined constrained Debian 13 AMD64 gate passed 1,230 distinct affected tests: 862 core unit tests, 121 core integrations, one shared JSON test, 116 peer-support tests, 117 selected product tests and 13 actual radio-provider preparation tests. Strict Rustdoc and full fast formatting/lint/static checks passed, including all-target workspace Clippy. A one-line existing phone-DTMF fixture annotation was independently reviewed after the generic protected-operand API exposed an inference error. A stale cross-snapshot peer-support artifact was cleared with package-scoped Cargo cleanup; dependencies/providers and global caches were retained. Final log SHA256 `8320fb7ded508fbd9ab4685a25841de2646487c27ee1ba62b6b9b0a3c682f0ad`. Published source `e91390b8f4e14c22a3beafdd5b0f18110eee02c1` also passed all 1,230 distinct affected tests in the native Debian 13 ARM64 home workspace with Rust 1.88. Native log SHA256 `75ae943d3a1fa622e8de61990eadc440a031e69dc79bd732ab6fa148e17156e9`; service health was active before and after, with installed configuration/artifacts unchanged.

Telemetry Task 35D round 2 has separately passed review and its 862-test component run; it is not in this checkpoint. Its revised catalog and radio/message/equipment public serializers are joining the HAL coordinator next. Actual command/macro/DTMF/mute/event execution and native station capabilities continue in isolated tasks. Real JSON process startup, native activation, client/package migration and final full CI remain pending. No installed service or configuration changed, and runtime cutover has not occurred.

## Decisions and remaining runtime work

The user clarified that key_down belongs to RX squelch. A held key or a GPIO microphone switch qualifies microphone input. A radioless node plays selected audio on its always-on speaker. The typed provider and renderer need to carry the actuator-free selection through to playback.

The native station task has reproduced and corrected ordinary ALSA/exclusive parallel operation and per-pin inverted parallel qualification at the real driver/provider boundary. Its additional serial/keyboard and external-tone capabilities remain isolated until reviewed and integrated. The unified runtime task is completing scheduler-specific message context and queued regenerated-DTMF provenance before independent review. Complete typed HAL projections have passed their first full component gate and are finishing field-origin coverage.

## Full controller gate: package version guard

After the approved workflow merges, full controller run44854722469 passed dependency installation and platform-independent checks. The AMD64 platform and coverage jobs then stopped at the existing artifact/package guard, which still expected USBRadioPlus alpha34 and schedule alpha2 while debian/control already requires alpha36 and recurrence alpha3. Coverage instrumentation and the platform test/install stage had not completed; no full-gate success or coverage result is claimed.

The reviewed test-only correction aligns those assertions with the declared requirements and calls the same package guard on the fast source path. The default and explicit package checks, four lowered-minimum negatives and full fast checks passed in the maintained constrained image. Log SHA256 `9f43224bc659805c436468c1dd6e376615540f4466861616120f678a20ed8555`. No production dependencies, ABI or runtime behavior changed. A fresh full controller gate follows publication.


## Continuous checkpoint: typed HAL projections and recoverable views

The independently reviewed telemetry catalog and typed HAL projections now share the authenticated coordinator. Public radio/audio/signaling/interface/control/RF/memory/scan fields, named messages, telemetry, equipment and permanent groups resolve from the same candidate. Effective leaves expose declaring or derived sources through RFC6901-keyed metadata without provider masks, generated graphs or invented resources. EchoLink status is compiled as text-only and its actual named-message dependencies are navigable. Effective schemas admit built-in empty RF lists and omit absent disabled URLs while retaining stricter authoring constraints.

Ordinary polls reuse immutable domain projections but still verify current private/storage state and working fingerprints. If a capability disappears, management retains verified saved-state metadata while affected domain views report unavailable. An explicit corrected reload may replace a fully published Failed state after confirmed rollback and no selected journal; save remains Confirmed-only. Pending, uncertain publication, key, integrity and concurrent-edit fences remain intact. Runtime inhibition clears only after confirmed owner adoption.

The combined maintained Debian13 AMD64 run passed 1,242 distinct affected tests: 873 core units, 121 core integrations, one JSON test, 116 peer-support tests, 118 selected product tests and 13 actual radio-provider preparation tests. Strict Rustdoc and complete fast formatting/lint/static checks passed. Log SHA256 d540b49144b6e0671dae21fb14e962c601da27ceabe7df46b1dc2f6e28904c1c. The two inherited source-composition differences preserve the already reviewed registry Clone and catalog discovery methods. Published source `b89e3d75a6834fc22e4f8735451a6fdae06787f2` passed the same 1,242 distinct affected tests and ten package checks in the native Debian13 ARM64 home workspace. Native log SHA256 `2a84fbf04194c77c8e3379457ba926026ba5253865870439c5a8b5c3188bb520`; service health was active before and after, with installed configuration and artifacts unchanged. The earlier real nginx/Apache mTLS component gate remains the proxy evidence; the combined run explicitly ignored its separately staged proxy fixture.

Full controller run44858065660 passed independent checks and all executed workspace tests on both native architectures, but both install-check jobs and the instrumentation job then failed a separate stale alpha34 token in tests/test_package_combinations.py. This is not a measured coverage failure. A separately reviewed four-token test-only repair aligns that guard with the already required alpha36. Its native home reproduction, ten passing package checks and both lowered-minimum negatives passed. The maintained host run passed the same checks and the complete fast gate; log SHA256 `1b6e10598715e1549894125331e0411b9a5b423cbe70aef1102306169c76b0aa`. The installed service and configuration remain unchanged.

The Task38 and Task39 findings reported at that checkpoint were subsequently corrected and passed scoped rereview. Their reviewed foundations now join the typed HAL coordinator as described below. Native runtime/Engine adoption and coordinated startup/CLI/package migration are still pending. The user-defined cutover stopping point has not been reached.

## Continuous checkpoint: registered execution and physical signaling

Reviewed command, macro, DTMF, runtime mute and calendar execution now share the registered owner and generation model. Ordered completion, permission/source rechecks, unchanged calendar progress, private reply audiences and source loss are covered by actual product/Host fixtures. Native receive muting retains accepted converter credit, source coordinates and PLC history; deferred Echo audio and PTT keep their original receipt across retries. This remains the execution foundation: real modular process startup and network-account activation are subsequent work.

The reviewed station cohort covers ordinary audio, exclusive PTT, serial modem/parallel/sound-chip GPIO inputs, explicit Linux keyboard receive input, external tone qualification, safe control-line release and checked CAT ownership. Sparse authoring and effective HAL agree on keyboard scancodes, DTE serial names, invalid-value fallback, inheritance and source metadata. A source-less GPIO pin remains local when an unusable inherited serial source falls back. Named messages support peers-only playback; automatic identifiers and tails retain local or combined playback with warning-and-inheritance fallback for unsupported values.

The exact functional source is private verification commit `7b7680aed88b84aea6560982b2af74c943548b29`, tree `bb240eaf0399a687ce31ea19db7bf0d5fe1f8e70`. Its 698-blob manifest SHA256 is `576906f200b3806eb4f3f90bde999dbfcb359ac47f178e10f988f8032b091742`. The maintained AMD64 run and native ARM64 run each passed **1,773 distinct affected Rust tests**: 893 core units, 122 core integration tests, one JSON test, 116 peer-support tests, 621 product tests and 20 actual radio-preparation tests. Both also passed ten package-combination checks. The two explicit product stress-name filters excluded three cases, and the existing proxy fixture annotation remains visible; the earlier real nginx/Apache integration receipts are retained. Real Draft202012 validation ran in the authenticated HTTP scenarios on both architectures.

The initial host outer run hit ENOSPC after all eligible product tests passed and left an empty exit receipt; it remains classified as interrupted. The successful behavior output was preserved. Only the unfinished radio-preparation, full workspace fast/strict Clippy, strict Rustdoc and package checks were rerun on identical source; recovery exited0 with log SHA256 `b0a88f9061ead5d8d298b3658a5afe52fe1bcfa11c1b7ad08a69492469435b52`. The 698 source hashes matched before and after. Package-specific cache cleanup and removal of verified duplicate archive-validation extractions restored space; immutable source archives, Git trees, SDKs, dependencies and all evidence were retained.

Native ARM64 used Rust1.88, one job, HOME-only source/output/temp files and the matched provider SDK. Core/JSON log SHA256 `c3a372f9d651824ed0c806fd3be7f07c7d53706e57e351fdae07e3efc77a07fc`; peer log `a0dc2b7a65d158880a8cc92c3a5eeeaa38e57b98d487e234b17c44260f4d457a`; product log `fa40aa980b53b523ba2c9413fed2a417bedd2ad04146bc8f4d3de9b7e0344cce`; radio log `1fbdefd045491ad391c47e65ccb5bcea6faedb548adc2fddc45b2445b7e3d59f`. Complete native receipts are sealed in archive SHA256 `b6053094bb1cd1309a6410f2d0e76a56e1c1bd979b251906b8ca01fd1f27493e`. The installed executable, service, libraries and configuration identity matched before/after, snapshot SHA256 `e6b19ac2729ee1c2d7033530f7031afd072b85a46cb67ccfdd1d4661d6391727`; the service remained active with the same PID.

Verified candidate source pins are GPIO `47d0d1ef764cd8725d4979b373cf27b2fc1ecd57` (ABI1, alpha5), radio `83c715249516b176fca0297bffb2b145e98c8d27` (ABI5, alpha13) and USBRadioPlus `34f3d529a0e1158bb422d94aa79e658adefd2eb4` (product ABI1, alpha37). Their exact official commit trees and matching AMD64/ARM64 DSOs/headers were checked. Public USBRadioPlus INI remains supported. These are verified development candidates, not a release claim; further runtime handoff work is isolated.

Full CI remains open. The prior b89 controller run exposed the source archive's omitted schema and 1,794 uncovered production lines across85files. The reviewed archive inclusion correction passed extraction/build checks and is included here. The unchanged100%production line/branch policy remains required. Radio PR12 passed its full native hosted gate; GPIO PR7 still needs serial/keyboard fixture coverage in the measured report. Shared-workflows PR20 has passed review,31workflow tests,Actionlint and real nativeAMD64/ARM64 Rust1.87 radio-package builds; merging that separate CI repair awaits explicit user approval. No installed service or configuration was switched.

The three pending station choices remain explicit: manual-key input versus physical PTT actuator placement; physical memory channel selection; and omitted transmit-power behavior. No implicit hardware selection or fabricated capability was introduced. Actual Engine/bootstrap, both frontend station handoffs, network participants, private phone integration, clients/provisioning, maintenance and final coverage/cutover work continue in isolated tasks.

## Continuous checkpoint: shared native bootstrap and station ownership

The reviewed shared native runtime now joins modular prepared configuration to the actual Engine, Host, station and core owners used by standalone and Asterisk. Both process compositions load the protected JSON service/store configuration and checked systemd TLS/key credentials, support management-only operation with zero radios, and expose confirmed desired/active outcomes. The writer's independent lifetime lease survives refused or uncertain shutdown. A failed replacement inhibits the affected station while an unchanged incumbent continues local repeat; a recovered callback remains dekeyed and silent until the matching serialized native owner confirms adoption. Site and manual inhibition remain independent. Locked or failed storage returns unavailable management-write status; a healthy unadopted store keeps its conflict result. Controller INI inspection compatibility and old client helpers still require the final retirement pass.

Independent reviews closed the original bootstrap/recovery/inspection findings. The final scoped review is SHA256 `9e484755082f706de804c88adb308b324ebb90f514f86f885978f48831a7e7f7`. The reviewed owned tree is `9ac6fdbdccd29e12513c398bc7543493ae45c16a`; its separately approved physical-station composition is `f1ff8885bfc5f76017e9ac62582a024b1304cbbc`. The latter retains the exact qualified alpha38 SDK cohort: USBRadioPlus `91f175c56a0b62c7dc84e24064f67ff49f142d8d`, GPIO `66726ead2d43945d9e6e090d369288d950bba754`, and radio `83c715249516b176fca0297bffb2b145e98c8d27`. The separate typed-binding qualification passed 121 affected tests on both architectures. Public USBRadioPlus INI stays supported.

The affected AMD64 recovery gate passed 103 aggregate cases and 13 actual authenticated HTTP/native-owner scenarios. ARM64 passed the same 103 aggregate cases and eight focused HTTP scenarios. The new tests hold the real post-core native handoff with manual/site inhibition clear and measure the actual changed callback: zero keyed and audible B frames before confirmation or after rejection, while A keeps repeating with zero dekeys. A separate case preserves manual inhibition through successful recovery. The final fixture-only correction restored two original UTF-8 values and passed both affected tests plus all 37 lifecycle module cases and one additionally matched nested case. Rust reported 38 passed, zero failed; the original wrapper's incorrect expected-count exit1 is preserved, with independent exact-name/status accounting exit0 rather than a fabricated wrapper success.

The combined runtime plus the already published neutral jitter work has 715 tracked files, qualified code tree `6a926a09f678bb3d642d005204b7e33e49d0697f`. Composition over the exact tested functional base preserved every nonoverlapping file byte/mode; only installation guidance and a packaging-policy file needed clean automatic merging. Full maintained-image formatting, C/Python analysis, package/source policy checks and all-workspace/all-target Clippy with warnings denied passed. All 715 executed source byte/mode checks and 19 retained SDK identities verified before/after; command, guard, scoped cleanup and outer statuses were zero. Log SHA256 `f4756076a492f94d17fec27eaffdf4dfef4489f8af63f3afcc66cd2306b81656`. This published checkpoint adds only this progress note and its completed checklist row to that qualified code tree.

All native work used isolated HOME source, targets, synthetic credentials and fixture radio providers. The installed snapshot remained `f1ebaedd6f109b2d65345bf9480bd5b1689abcdfb79eb4c7ef96454480fd0414`, including original checkout, service PID84677 and monotonic start181601797315. No installed service/configuration, operational account or physical radio was changed.

The neutral jitter creation/live-policy component is separately reviewed and published. Its IAX private-call consumer subsequently passed 469 Rust cases, real Debian package/C/Asterisk and strict documentation checks on AMD64, and an actual attached ARM64 call preserving queued audio, policy rollback and media epoch. That provider's new source is still under independent review; its pins/releases and production phone participant integration are not claimed complete here. Maintenance review found durable-GC, opaque graph metadata, restore identity/LKG/budget and cross-process retry gaps; their original-owner fixes and real failure reproductions remain open. Shared site-only/inherited equipment routing, complete protocol accounts, CLI/tuner migration, final controller INI/Unix removal and the full coverage/cutover audit continue next.

Current hosted controller checks pass native AMD64/ARM64 build, tests, staged install and packaging. The coverage exporter still fails on inconsistent LLVM function metadata before a current threshold verdict; the existing 100 percent line/branch requirement is unchanged. GPIO PR7's later full hosted gate and measured production coverage passed. Shared-workflows PR20 and GPIO PR7 remain separately unmerged pending their requested user approvals. Actual implementation/runtime cutover is not complete, so continuous execution continues and the requested stopping point has not been reached.

## Standalone radio trial checkpoint

The matched ARM64 renderer, USBRadioPlus and controller passed their focused checks in runs 17954, 46512 and 75003. Holding the keyboard key opens the microphone receive carrier. A GPIO microphone switch can supply that carrier, including with inverted polarity. A speaker-only output renders selected audio without physical PTT confirmation or a key-up delay. The controls also exercised a configured physical radio actuator using simulated device I/O.

Run 19781 passed 21 of 22 controls, including AllStarLink, direct EchoLink and radio-originated SIP. The incoming SIP fixture failed when its BYE reply did not start with SIP/2.0 200. Run 94404 repeated the incoming case on the same foreground executable and product library with a fixture that answered OPTIONS during the call. It measured a 1 kHz station signal at the caller and a 500 Hz caller signal at the station, received 200 for BYE and registration withdrawal, joined the foreground and released its writer lease.

Run 89099 initialized an empty protected store with one site, zero ports and Pending activation. After interruption at the first pointer, reopening recovered the same desired revision and cleared the journal. The same run reproduced an adapter timing defect with 170 OPTIONS requests and a 106 ms first interval. Run 83152 used the same registration test executable and the corrected SIP adapter from source tree `21a32bc1`. It observed two OPTIONS requests 15,007 ms apart, successful registration and withdrawal, and joined owners.

The full fast checks and hosted CI for this aggregate, the staged release rehearsal and the physical radio trial remain pending.
