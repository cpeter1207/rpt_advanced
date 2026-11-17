# IAX media provenance parity implementation plan

Status: implementation planning only; no production changes or builds performed.
Approved design: [IAX provenance parity](../specs/2026-11-17-iax-media-provenance-parity-design.md),
frozen SHA256 `a9d570a378762cea93ca75db6b318ab9163c7b20b3275ccc84d93416425180dd`.
Preserve its permanent profile latch, two delivery-credit boundaries, command
fences, receipt age and uninterrupted shared courtesy invariant.

## Locations and exact starting points

Development root: `/home/codex/rpt-development` (development host only).

- IAX repository: `shared-components/librptadviax2`; checkout HEAD is old
  `7dd0f98`, NOT the implementation base. Required base is controller manifest
  pin `ce9fe2da0d8480c1503f5eb80c3c95d8ac2e5360`. Existing other IAX worktrees
  are unrelated ongoing work; do not reuse or reset them. Create a new isolated
  worktree `iax-media-provenance-parity` from the exact pin after checking its
  path/branch are unused and reading provider AGENTS/quality requirements.
- Codec provenance worktree: `codec-unit-provenance`, currently
  `8596809c3ff8493d705bc2f65371e9f0b6e4fef2`; its reviewed client implements
  require_provenance/decode_with_provenance. Existing IAX client dependency is
  `becbff939a0817cc91df6dcb0d40e85e381ec87b`. Controller release manifest codec
  pin remains `5ed83bb4d52985e68a455fce4a3145ee6907c107`; do not confuse that
  release baseline with the candidate provenance source.
- Controller shared worktree: `phone-downlink-silence-repro`, observed HEAD
  `e35504b12ce830c0e3691076ce8720d604932199`. This is a moving integration
  worktree: obtain the coordinator's frozen integration hash before consumer
  edits. Shared courtesy checkpoint is `3d493d3138f8812677b3b072fdc1a157494be01c`.
- Qualification workflow worktree: `phone-cohort-ci`, observed HEAD
  `ae7761d47cfbb5339a79c50bd224dfbac2fa636e`; relevant files are
  `.github/workflows/phone-cohort.yml`, `tools/run_phone_cohort.sh`,
  `tools/phone_cohort.py`, `tools/test_phone_cohort.py`.

## Task 1: isolated provider implementation first

Only inventory and isolated failing-test preparation may proceed initially.
Steps 3-6 are conditional on the codec policy prerequisites below, not a grant
to implement an unproved provenance contract. Own only the new
IAX worktree; do not edit controller Cargo files, product tests, standalone
support or codec worktree. Coordinate dependency revision with the codec owner.

1. Inspect provider rules and current required v1/v2 consumers. Inventory actual
   supported codec profiles before changing admission. Retain currently required
   semantics, not compatibility-only historical alpha shims.
2. Add failing tests in `src/ffi_tests.rs`, `src/client_unit_tests.rs` and
   `src/client_protocol_progress_tests.rs` for the approved v3 sized record,
   invalid size/version/profile/flags, profile latching and all conflicting
   legacy/timed/untimed calls. Assert rejection before state mutation. Test
   private deliver/service-only/deliver and no restoration of ordinary/TEXT.
3. Update IAX `Cargo.toml`/lock to the agreed exact codec provenance candidate.
   Use actual APIs in codec client `rust/client/src/lib.rs:563-634`; require
   provenance before admission. Unsupported profiles fail explicitly; do not
   silently drop currently supported codec functionality to make tests pass.
4. Implement provider header/FFI changes in `include/rptadv_iax2_client.h` and
   `src/ffi.rs`, permanent profile validation in `src/client_service.rs` and
   `src/client_timed.rs`, and typed event data in the existing client boundary.
   Verify proposed v3 88-byte layout in C and Rust; zero unsupported output and
   preserve oversized caller tails. Update package/capability metadata together.
5. Implement retained homogeneous decode spans in `src/codec/providers.rs`,
   `src/codec/stream.rs`, `src/playout.rs` and their actual owning client state
   as needed. Decode the full packet once, preserve trimmed suffix origin/age,
   and return one bounded homogeneous segment per poll. Provider cursor advances
   on returned count, not future downstream acceptance. Native PLC takes priority;
   do not conceal recovered samples again or fill benign omitted prefixes.
6. Extend `src/codec/providers/tests.rs`, `src/playout_tests.rs`,
   `src/jitter_fixture_tests.rs`, `src/source_time_tests.rs` and timed/service
   tests: mixed origins, capacity split, immutable receipts, expiry/epochs,
   native repeated trim, no extra decode/PLC, bounded memory and service-only
   retirement while provider suffix is retained. Run resource-approved targeted
   red/green checks; warnings denied. Obtain independent review before publishing.

Deliverable: exact IAX source hash, API/header changes, test evidence, supported
codec matrix, package/capability requirements and unresolved gaps. Do not claim
consumer parity from provider completion.

## Task 2: consumers and explicit capability admission

Wait for Task 1's stable header/API and coordinator ownership handoff. The RTP
bridge worker currently owns controller product tests/Cargo and standalone test
support: do not concurrently edit those files or pins.

1. In `rust/standalone/src/iax.rs` and `iax/timed.rs`, update the native wrapper
   and explicit required-capability admission. Preserve ordinary/private profile
   identity; never downgrade an operational error into an old poll call.
2. In `host_services/iax_phone.rs`, consume explicit origins with immutable birth.
   Provider poll already committed the returned segment; adapter partial sink
   credit advances only its retained offset. No delivery poll over retained PCM.
   Private service-only can signal retirement without overwriting buffers; discard
   old media, retire downstream owners, then acknowledge exactly that target.
   Preserve an unaccepted digit across retirement, never an old PCM suffix.
3. Wire the ordinary consumer through `rust/product/src/link/session.rs`'s
   existing SourceInput audio/concealed/gap path. Trace wrapper-to-session
   ownership before editing; enforce retained output without private service
   activation. Both adapters require proof, not protocol-name assumptions.
4. Only then replace SIP-only continuation eligibility in
   `rust/product/src/link/private_phone.rs` with shared evidence-based policy.
   Concealment/loss cannot renew speech, generate DTMF, authorize commands, or
   defeat retirement/mute/replacement. All downstream coordinates remain emitted.
5. Extend `host_services/iax_phone_tests.rs`, `iax_phone_jitter_tests.rs`,
   `iax/executor/tests.rs` and the owning product/core suites under coordinated
   ownership. Cover partial acceptance, service-only retirement, refused digit,
   auth loss/detach/replacement, receipt expiry, ordinary linked activity and
   private phone phases. Preserve positive courtesy assertions plus resumed and
   unrelated selected-audio cancellation under shared core 3d493d3.

## Task 3: exact-source qualification and provenance

1. Freeze all source pins after targeted tests and independent review. Extend the
   isolated cohort machinery to build the exact IAX candidate with the matching
   codec candidate; do not inherit an unattested release binary silently. Review
   workflow trigger/publication scope before any authorized branch push.
2. Record per-architecture source commits, dependency and image digests, build
   inputs, package hashes, ELF hashes/build IDs and actual loaded ELF paths.
   Existing alpha13 amd64 SHA256 is recorded in the spec; package integrity plus
   matching version is NOT a source attestation and is not ARM evidence.
3. Run a freshly pulled/published verified native Debian 13 amd64/arm64 cohort.
   Respect available disk and serialized compile slots; no heavy local containers
   until coordinated. Verify repeated native IAX trim through actual product ring
   with paced/stalled consumers, exact omitted duration and bounded queue/age.
   Run both transport positive/cancel courtesy and command regression matrices.
4. Complete required pre-push checks, independent implementation review, full PR
   gates/production coverage, package and staged-install validation. Hosted ARM
   results remain distinct from physical station/listening acceptance.

No release/APT publication, integration/main change or live-node access is part
of this plan. Request any necessary publication approval through the coordinator;
never bypass a denied action. Preserve first-failure logs and exact run IDs.

## Readiness, blockers and estimate

The earlier provider-ready assessment and 2-3 day estimate are superseded by
the [codec provenance policy](../specs/2026-11-17-codec-provenance-policy.md).
The approved IAX transport design does not establish codec provenance support
or authorize unproved classification/API changes.

Ready: pinned IAX baseline inspection and isolated provider RED tests, with a
reserved resource-safe slot. The current codec candidate covers only 9 of 16
active formats; disabling the other formats or treating successful decode as
fresh authority is not an acceptable shortcut. Before provider production
steps 3-6: complete native branch proof, resolve the policy's semantic choices,
review any necessary API change, and agree the exact codec support matrix/pin.
Ordinary current-parameter reconstruction is not old PLC merely because it uses
filter history. Independently retained or concealed contributors must preserve
conservative programme age without rejuvenation, in either mixed-origin
direction. LPC10 failed correction must be traced through delayed parameters,
smoothing and synthesis buffering, including a proved clear without permanent
taint. Do not introduce a new analysis API without a demonstrated need.

Before consumer work: stable reviewed provider interface and controller ownership
handoff. Before parity claim: both consumer paths, matching native cohort and
binary/source provenance; no existing alpha13 attestation establishes the latter.

Revised estimate: 5.5-9 focused engineering days for codec prerequisites, provider,
adapters and qualification; conditionally 7.5-13 days if native proof demonstrates
that a new analysis API is necessary. Review findings, full-gate remediation and
physical acceptance can add time. These are planning ranges, not deployment or
parity-completion claims.
