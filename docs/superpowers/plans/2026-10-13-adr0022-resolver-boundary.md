# ADR 0022 Resolver Boundary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans for native execution or superpowers:subagent-driven-development if selected by the user. Complete each test cycle before moving on.

**Goal:** Finish the existing standalone external-capability boundary without changing directory lookup or radio behavior.

**Architecture:** Move the existing system DNS implementation into a versioned Rust resolver `cdylib`. Keep directory policy in the product and reuse its current host-services callbacks. The Asterisk composition retains its Asterisk-backed resolver.

**Tech Stack:** Rust 2024, stable C descriptor ABI, libc/libresolv inside the resolver adapter, existing Debian packaging and native quality containers.

**Spec:** `doc/architecture/decisions/0022-versioned-external-c-dependency-adapters.md`; related ADRs 0018, 0021, 0036, and 0040.

## Global Constraints

- Move implementations and tests; do not duplicate them or add directory features.
- Each selected composition requires its complete compatible provider manifest. Failed reload keeps the previous complete configuration.
- Retain loaded code and contexts until callers stop. No callback-time loading or hot replacement.
- No product/host-services ABI change is needed for this extraction alone.
- Ordinary operating-system facilities are not a new generic OS adapter. Hamlib and other unimplemented wishlist capabilities remain out of scope.
- Native audio remains 48 kHz F32; this work changes no audio settings or path.
- Debian 13 amd64 and arm64 are supported; production coverage is required on amd64 only.
- Use targeted container checks, not a local full gate. No deployment, external connection, release, or PR is authorized by this plan.

## Review Focus

- SRV absence differs from malformed SRV: preserve default-host and extnodes fallback decisions (Task 1).
- DNS callback output is borrowed only during the call; no dangling buffers or panic crosses the ABI (Task 1).
- Hostname dialing retains numeric parsing, address order, IPv4-mapped handling, and default port (Task 2).
- Invalid or incomplete providers reject startup/reload without damaging the active configuration (Task 2).
- Standalone must actually load the packaged resolver, not a stale or directly linked implementation (Task 3).

## Files and ownership

- New `rust/resolver-adapter/{Cargo.toml,build.rs,wrapper.h,src/lib.rs,src/tests.rs,include/rptadv_resolver_adapter.h}`: DNS FFI, existing SRV parsing/selection, address lookup, ABI validation/error translation.
- `rust/standalone/src/{directory,host_services,providers,runtime}.rs`, `rust/standalone/{build.rs,wrapper.h}`: provider lifetime and neutral forwarding; retain file lookup.
- `Cargo.toml`, `Cargo.lock`, `Makefile`, `Doxyfile`, `debian/{control,rules}`, new runtime/dev `.install` files: build/install the standalone-only adapter.
- Existing product directory tests and Python package/surface/service tests: prove unchanged policy and actual linkage.

## Task 1: Extract resolver implementation and preserve result semantics

**Interface:** Export `rptadv_resolver_adapter_descriptor_v1()`, returning an immutable descriptor with `struct_size`, `abi_version = 1`, capability `rptadv.resolver`, lifecycle functions, and these synchronous operations:

```c
int32_t create(void **context);
void destroy(void *context);
int32_t resolve_srv(void *context, const uint8_t *service, size_t length,
                    void (*result)(void *, const uint8_t *, size_t, uint16_t),
                    void *result_context);
int32_t resolve_addresses(void *context, const uint8_t *endpoint, size_t length,
                          void (*result)(void *, const uint8_t *, size_t),
                          void *result_context);
```

Text results use the existing hostname/`SocketAddr` representation. Zero means completed (possibly no result); positive means unavailable; negative means rejected input/output. No libc or Rust-owned type crosses the contract. Validate required pointers/lengths before use and document context concurrency.

- [ ] Move the existing deterministic SRV parser tests into the new crate. Add table-driven descriptor/borrow/status tests and resolver fixtures; no public DNS calls.
- [ ] Assert `absent_srv => success + no result`, `malformed_srv => unavailable`, `bad_borrow => rejected`; preserve priority/weights, compressed names, zero-port rejection, malformed and oversized packet handling.
- [ ] Run `cargo test --locked -p rptadv-resolver-adapter` in the maintained quality container; first demonstrate missing extraction/contract tests failing.
- [ ] Move SRV FFI, parser/selection, and system address resolution from standalone into the adapter. Keep `file_record` in standalone. Contain panics at descriptor entry points using existing adapter conventions.
- [ ] Repeat the targeted crate tests, `cargo fmt --all -- --check`, and `cargo clippy -p rptadv-resolver-adapter --all-targets -- -D warnings`; require success. Update Rustdoc and commit the focused extraction when buildable.

## Task 2: Wire the mandatory provider without changing lookup policy

**Consumes:** Task 1 descriptor. **Produces:** Standalone host-service DNS callbacks backed exclusively by the retained resolver provider; the existing product callback table and directory policy remain unchanged.

- [ ] Extend provider fixtures for missing library/symbol, null descriptor, wrong ABI/capability/size, and each absent function. Assert startup failure and reload rejection preserving the prior runtime.
- [ ] Add destination-resolution regressions: numeric endpoint bypass, hostname/default port 4569, first-result selection and resolver failure. Retain product tests for static precedence, DNS/extnodes fallback, and source-identity mismatch.
- [ ] Run `cargo test --locked -p rpt-advanced-standalone` and `cargo test --locked -p rptadv-product directory`; record the expected new fixture failures.
- [ ] Add the resolver to the standalone manifest, retain its library/context through product shutdown, and forward existing DNS callbacks. Route hostname resolution in `parse_destination` through it. Revalidate the complete selected manifest on reload; do not reload executable code.
- [ ] Remove standalone's `resolv` link, resolver header and resolver bindgen allowlists. Add only the neutral adapter header. Remove now-unused implementation/helpers.
- [ ] Repeat the targeted tests, plus provider lifecycle/drop-order tests. Verify successful reload and provider failure cannot change active configuration. Commit the focused wiring with updated developer docs.

## Task 3: Package, validate the boundary, and correct its status

**Artifact:** `librptadv_resolver_adapter.so.1`, runtime package `librptadv-resolver-adapter1`, development header/symlink package. Keep this out of Makefile `ADAPTERS` if that list links providers into the Asterisk loader; use standalone artifact/install lists.

- [ ] Extend `tests/test_rust_product_surface.py`, `tests/test_package_combinations.py`, and `tests/test_standalone_service.py` to expect the resolver provider/header/package and reject direct standalone resolver imports.
- [ ] Run those tests before adding packaging and confirm their new expectations fail.
- [ ] Wire workspace, build/install, Debian dependency/runtime/dev manifests, and developer-doc inputs using the existing file/speech adapter pattern. Do not invent an unreleased minimum dependency version.
- [ ] Run the three Python files, `make standalone-check`, and the affected staged artifact/package checks. Verify with `readelf -d` and `nm -D` that standalone does not directly need/import libresolv APIs and the resolver owns them.
- [ ] Run scoped Rustdoc with warnings denied, formatting, lint and static analysis. Record completed checks and affected-file coverage in ignored `.work/quality-progress.md`; do not repeatedly rerun a full coverage report.
- [ ] Update ADR 0022 and ADR 0036's resolver ownership text, architecture manifest, and relevant wishlist status using the actual tested boundary. Mark current selected external capabilities implemented; explicitly leave unimplemented capabilities planned.
- [ ] Review the diff for policy/audio/config changes, missing ownership checks, and retained dead code. Commit only the scoped implementation and documentation.

## Reproducible execution

From Git Bash, use the existing launcher (paths relative to the sibling checkouts):

```sh
../rpt_advanced-workflows/tools/run-in-quality-container.sh \
  ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest "$PWD" \
  cargo test --locked -p rptadv-resolver-adapter
```

It pulls `latest`, records the digest, cleans stale matching test containers and removes its own container on exit. Use the same launcher for the commands above. If required dependencies are absent, repair the maintained image rather than installing an ad-hoc local toolchain.

## Completion evidence

The descriptor/provider and directory regressions pass, installed artifacts prove the dependency boundary, and documentation states only verified implementation status. A subsequent authorized PR uses the hosted full platform gate; no live-node test is needed for the extraction itself.
