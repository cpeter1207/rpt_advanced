# HAL completion and separate CLI implementation plan

> **For agentic workers:** Use superpowers:executing-plans or coordinated component implementers. ROOT owns live writes and the Native ARM64 execution lane.

**Goal:** Restore the prior station behavior, finish the approved controller HAL contract, and prepare the controller and separate CLI Debian package for release.

**Architecture:** Keep the existing modular JSON store, mTLS transport, coordinator, typed actions, runtime owners and dynamic providers. Publish immutable observations through HAL. Bind temporary tuning to an authenticated connection and real runtime generation. The CLI uses HAL and existing typed operations without linking the controller or radio runtime.

**Tech stack:** Rust 1.88; existing ureq/rustls/serde_json/url/uuid; Debian 13 amd64/arm64. Reuse the current `codex/json-rest-configuration-20261119` worktree.

**Spec:** `docs/superpowers/specs/2026-11-18-json-rest-configuration-design.md` and `docs/superpowers/plans/2026-11-19-json-rest-implementation-checklist.md`.

## Constraints and ownership

- Existing trial2 build remains installed until another explicit deployment approval. Live configuration restoration and primary-only Blind Hams settings are authorized now.
- Preserve ordinary ASL/Echo/SIP and radio operation; do not rewrite the working engines. No private credentials, PINs, caller authentication text or raw provider configuration in API output/logs.
- Unknown/invalid/unsupported JSON settings warn without values and inherit/default when safe. Unsafe ambiguity fails.
- No runtime INI compatibility in the completed controller; USBRadioPlus retains its INI and tuner.
- ROOT: live restoration, transport-connected maintenance leases, maintenance dispatch, final integration/gates.
- Agent80: typed cached domain status, provider observation joins and associated status routes/schemas/tests. Coordinate shared files rather than overwrite concurrent edits.
- Agent85: static ASL JSON authoring and existing typed-directory binding, after restoration verification.
- Agent76: independent Rust CLI crate, client tests, Cargo/Make integration and `rpt-advanced-cli` Debian package. Coordinate service/reload changes with ROOT.
- Run behavioral RED before production implementation and targeted GREEN afterwards. Do not label SDK/cache/compiler failures behavioral REDs.
- Fresh published quality image, maintained labeled launcher, unprivileged UID1001 build outputs on DEV. ROOT alone runs Native jobs with resource limits and no access to real devices during fixtures.

## Task 1: restore prior live settings

**Inputs:** `legacy-schedule-85/requests.json` SHA e930b7de1812e5ba62e7c36eb0e7adf2868d9592056c7c715d8b4083f61148cb.

- [x] Recover and compare prior settings; preserve the existing ten courtesy definitions.
- [x] Back up the selected encrypted graph with its existing delivered key.
- [x] Add P: 800 Hz, 25 WPM, -24 dB; retain the existing 500ms courtesy delay.
- [x] Restore five Chicago nets, one permanent group, 15 macros, nine messages and 63 original warning opportunities using fresh ETags and dependency-first writes.
- [x] Verify every authored/effective definition, confirmed activation, existing tone identities and unchanged controller PID/invocation.
- [x] Verify latest user steering: Blind Hams has only 506315 enabled, no backups.

Actual result: revision `3d66a9b8-e8db-414d-99ca-c1e28f702954`, owner39, trial2 PID370426 unchanged. Protected backup `/var/lib/rpt-advanced-trial1/prior-settings-backup.vauuos`. No controller replacement.

## Task 2: complete cached live status

**Files:** `rust/product/src/management/{representation,schema}.rs`, `lifecycle_management.rs`, typed host/provider status modules, existing Core status accessors; corresponding `configuration_transaction/tests/http/review` fixtures. Agent80 maintains its exact file/test map.

- [ ] Run `RPTADV_HAL_REVIEW_CASE=live-status` with existing exact Product HTTP test selector; first RED is account owner `null` where the owning port generation is37.
- [ ] Publish registration and availability bound to the selected port/account/generation; add the checked typed Echo bridge instead of parsing status text.
- [ ] Publish real receiver, peer and call type/activity/mute/attachment observations, bounded pagination, cached meters/equipment quality/readback and existing event issues.
- [ ] Collect outside audio callbacks and HTTP; fence revision/generation again before immutable publication. Do not fabricate unsupported observations.
- [ ] Verify independent ASL/Echo/SIP wire fixtures, two-call pagination/privacy, stale owner withdrawal and status-only ETag changes.

## Task 3: exclusive connected tuning

**Files:** new `rust/product/src/management/maintenance.rs`; `rust/peer-support/src/https.rs` and `https/http.rs`; Core management routes/schema and trusted command-source grant; Product lifecycle dispatch/native adapter; existing physical radio-maintenance owner. ROOT owns shared maintenance changes.

**Implementation choices:** Port link `rpt:maintenance` leads to GET-only `/maintenance/ports/{port}/`. It advertises `rpt:lease` as `/maintenance/ports/{port}/{boot}/{lease}/`, with current boot and a client-chosen lease UUID. A member supports GET, conditional PUT/PATCH and DELETE. Initial PUT uses If-None-Match:* and `configuration={active:true,expected_active_revision,ttl_ms:30000}`. Renewals use the fresh member ETag; maximum duration per renewal is60000ms. These are lease resource updates, not POST runtime actions.

**Authority:** The transport supplies a nonfabricable connection-lifetime proof. A lease binds that connection, mTLS actor, port, active revision and generation. Typed operation bodies carry `maintenance_lease` only for existing `radio_maintenance` actions, one matching target and expected active revision. Recheck at admission and dispatch. Lease cancellation/expiry/disconnect/owner loss/generation change invalidates pending work and clears actual manual PTT/test tone/CTCSS/subaudible overrides. Do not reacquire or restore PTT automatically after connection loss.

- [ ] Add failing real-transport tests for exclusive claim, competing actor/connection, expiry/disconnect, stale generation and cleanup.
- [ ] Carry connection lifetime through admitted requests; do not confuse a completed HTTP response with connection loss.
- [ ] Implement the lease HAL resources using existing conditional/patch helpers and a bounded retained identity inventory.
- [ ] Reuse Host/worker radio-maintenance dispatch and cleanup with a trusted grant. HAL status/effective reads supply public queries; never forward raw native configuration.
- [ ] Verify actual provider callbacks, cleanup after every exit path and replay-safe operation receipts in isolated Native fixtures.

## Task 4: static ASL JSON

**Files:** Core `config/resources/profiles/{allstarlink}.rs` and profile tests; Product `directory.rs`, `host_asl.rs` and tests; appropriate projection/schema branches; public schema.

- [ ] Run frozen f3 plus regression4b20a75b: `config::resources::profiles::tests::allstarlink_static_nodes_normalize_complete_local_tables`; expected RED is loss of valid `static_nodes` from local authoring.
- [ ] Resolve ordinary inherited atomic `static_nodes` tables keyed by exact ASL decimal identity; typed rows have canonical host, nonzero port and independent numeric permitted source address.
- [ ] Feed the existing source-checked registered directory; update retained-directory equality so changes withdraw old proofs. Preserve terminal static source mismatch and ordinary DNS/external-feed fallback.
- [ ] Verify normalization, inheritance/empty override, diagnostics, duplicate identity behavior and runtime source admission.

## Task 5: separate CLI and client migration

**Files:** `rust/cli`, workspace Cargo files, Make install targets and separate Debian package ownership. Detailed component plan: `.work/json-rest/operator-cli76/plan.md`.

- [x] Genuine mTLS discovery RED then GREEN, including server-selected HAL navigation.
- [x] Genuine mutation RED then GREEN for correct methods/media/ETags, stale412, and reload operation template/fingerprint/receipt behavior.
- [ ] Finish generic resource operations, status, apply/reload/restore, boot-scoped operation submit/show/wait and terminal/indeterminate handling.
- [ ] Implement tuning against Task3's connected lease; keep one authenticated client and renew before expiry. Clear on quit/error and report lost ownership.
- [ ] Package only `/usr/bin/rpt-advanced-cli` and its own provisioning/docs in separate Architecture:any `rpt-advanced-cli`; no Product/radio/Asterisk DSO linkage.
- [ ] Complete administration-group credential access and authenticated reload-helper transition; retire remaining controller socket/tuner-client paths while preserving USBRadioPlus tooling.

## Task 6: release preparation

- [ ] Audit schema fields, routes/methods and action/result coverage; finish coordinated remaining controller INI/Asterisk paths required by the approved cutover.
- [ ] Review/integrate component changes and update checklist only against observed evidence.
- [ ] Run targeted component/ABI/provider/transport/DSP/scheduler/media checks and Native ARM64 checkpoints first.
- [ ] Complete fresh-image fast checks before pushing; prepare commits and PR with concise validation/known limits.
- [ ] Complete native Debian13 amd64/arm64 build/test/install/upgrade/package matrix and100% production line/branch coverage on amd64 before merge/release readiness.
- [ ] Update user-facing reference/API/CLI/migration/service docs immediately before PR, including separate package ownership and compatible provider versions.
- [ ] Prepare releasable artifacts and deployment/rollback notes; do not publish or replace the installed controller without the appropriate final authorization.

React remains last and unstarted. Present the JSON→Fluent mapping for approval before React work. Preserve the user's usage cutoff: pause below10% remaining.
