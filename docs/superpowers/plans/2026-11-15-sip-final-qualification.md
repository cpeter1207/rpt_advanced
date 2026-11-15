# SIP Final Union Qualification Plan

**Goal:** Qualify the final merged phone/SIP source and dependency cohort without mistaking focused candidate checks for the complete native gate.

**Source:** preserved G.722/P14 union `ab54cf0`, plus source-archive correction `3991e45` replayed as `0f9b01f6beac64011ae08697fde697e6c90797dd`. The original ab54 tree remains unchanged. The source-archive delta touches Makefile and its real-tar regression, not Rust runtime code. Batch A is on a separate branch and is not included here.

## Cohort boundaries

- Existing focused/wire/strict receipts use an isolated unreleased 16 kHz detector. Owned DTMF PR5 merged as06948a9 after both native PR gates. Alpha4 is now released; the isolated final branch selects the verified release as recorded below.
- Keep the currently qualified Sofia1.13.19 dependency unchanged. A failed no-OpenSSL probe is not a replacement qualification and does not establish distribution clearance.
- Final published artifacts require exact released runtime/development provider pins, package versions, artifact hashes, source SHA and native architecture recorded together. Do not silently substitute the candidate DSO or an image's older installed detector.
- Native means Debian13 amd64 and arm64 on their native runners; amd64-local tests do not establish arm64. The running node is not a CI runner.
- Every local heavy command uses the maintained constrained launcher and a separately granted serial slot. Previous receipts used image4e62696d8e6809b7b716c3c81d70b59ed9ab5b109a7539dc97c855dc7d41e844; new released-cohort runs use verified imagec3800ed0094d0d01284b70481d3189a76044042b2900594115db2e2a41f2d0fa. Stop on first failure and release the slot. Do not lengthen fixtures to hide regressions.

## Bounded local aggregate passes

The commands below are source scopes, not claims of execution. Select the exact candidate or released cohort explicitly in the existing private environment before running them.

1. Full core and product: `cargo test --locked -p rpt-advanced-core -- --test-threads=1`, then `cargo test --locked -p rptadv-product -- --test-threads=1`. One595-second outer slot; previous P14 product alone took418.79s. A timeout is an incomplete result, not a pass.
2. Standalone non-outgoing remainder: `cargo test --locked -p rpt-advanced-standalone -- --test-threads=1 --skip host_services::product_phone_tests::sip::outgoing`. The outgoing tests are deferred to step3, not omitted from aggregate coverage. Include binary, integration and doc tests selected by Cargo. Bound595s.
3. All outgoing fixtures: compile the standalone lib harness with `cargo test --locked -p rpt-advanced-standalone --lib --no-run --message-format=json`; select the one actual test executable from compiler-artifact output. Inside the same qualified container, list every `host_services::product_phone_tests::sip::outgoing::*` test. Current source defines14. Save the sorted inventory, partition it5/5/4, and execute each exact case once with `RPTADV_TEST_OUTGOING_PRODUCT=1`, the unchanged90-second per-case timeout, and595-second outer limits per partition. Reconcile the union of receipts against the inventory; zero-test selections fail. The four composed outgoing cases already run do not replace this full inventory pass.
4. Remaining workspace packages: `cargo test --locked --workspace --exclude rpt-advanced-core --exclude rptadv-product --exclude rpt-advanced-standalone -- --test-threads=1`, bound595s. Preserve existing ignored-test reporting and record missing prerequisites as blockers.
5. Full portable source gate: `make quality` uses Rustfmt, C formatting, Ruff, Cppcheck/Clang-Tidy, dependency/product boundary checks, all-workspace Clippy and Rustdoc, and Doxygen. Run without rewriting source. Partition only at existing targets (`make lint`, `make static-analysis`, `make docs`) if necessary, preserving every target and source SHA.
6. C/native boundary and package sources: build exact owned artifacts, then run existing product-abi, loader, jitter-ring, radio-interlock, site-gpio, site-hwmon and sip-registration checks. Run the source-archive real-tar regression and package-combination checks. The source-archive fix is not a full `distcheck` pass.

Fresh command/output receipts must distinguish success, failure, timeout and never-run work. Any later production/provider/merge change requires the affected combination rechecked; docs-only reconciliation can retain the proven runtime SHA.

## Canonical native and publication gates

The repository's required PR workflow remains authoritative. It runs portable quality once and native Debian13 amd64/arm64 build/tests/packaging/staged-install/integration checks. Canonical native targets are `make platform-verify` (check, install-check, integration, distcheck) and `make ci`; amd64 additionally requires `make coverage` with100% production line and branch coverage and the pinned branch-instrumented toolchain. This includes the actual Asterisk lifecycle/link integration fixtures and rebuilt source archive. The local partitioned passes above are early qualification, not substitutes for this complete gate.

Before publication, verify the exact provider release assets and pins, the source distribution and license decision, and the separate workflow SIP-package staging fix. Before hardware admission, verify the actual installed cohort and follow the parent's explicit staging/live authorization. No existing candidate pass implies these gates have completed.

## Estimated serial test time

Core/product roughly8-10minutes; full outgoing inventory roughly15-20minutes at current fixture durations, split across three bounded slots; standalone/workspace remainder and portable/C boundary checks roughly5-15minutes depending on cached builds. Native package/integration/coverage and hosted-runner waiting are separate and cannot be honestly bounded from the focused receipts. Reuse cached builds but never stale executables or silently skipped cases.

## Released alpha4 and reconciled local evidence

- DTMF tag `v0.1.0-alpha.4` targets merged `06948a9a78933ed36299279a49bfaaa189eb3377`. Release run43745348688 completed verified-source, native Debian13 amd64/arm64 and artifact publication checks. Runtime/dev packages are version `0.1.0~alpha4-1` and match published SHA256SUMS. The official amd64 runtime package hash is `eb12741c54074fa2616dbf895ccbc4e901bebd0d2e6f4544a904027af325fed3`.
- The private test extraction selects official DSO SHA256 `b8d34917520b0219152495f524f0d0904bb6cf8386892a5c66e0431e162e1e14`, with no candidate path or system installation. Existing composed product/SIP artifact hashes remain unchanged.
- Manifest-only published `caecd9773e0077c51db40b6624a0464594d4b7dc` is based on current feature `ac3b8dd`, not older main. Every non-DTMF pin/package list equals that feature and original image source `eeb7e770`. Native image run43747025587 passed both build/smoke jobs and promoted manifest `c3800ed0094d0d01284b70481d3189a76044042b2900594115db2e2a41f2d0fa`. Local receipts verify SDK35, IAX13, PortAudio5, ring6 and DTMFalpha4.
- The first released full run on36160d6 passed823 core unit tests but found the obsolete core integration assertion that16k must fail. Test-only `b16ca1e` adds16k to exact whole/37/1-sample digit/mute-span equivalence and retains unsupported12k rejection. Targeted proof and16k SIP readiness passed.
- The subsequent full run onb16ca1e passed823 core unit and94 integration tests. Product completed883 passed,1 stale16k-negative failure and1 existing ignored load experiment in408.55s. The exact failure log is retained as `.work/released-full-core-product-b16ca1e-failed.log`.
- Test-only `07411a6` changes that private-source negative to12k, preserves legacy16k success and adds16k native-source onset/muting/exact-one-command coverage. Its targeted8 tests, all5 product integration tests and documentation stage passed on the released image/DSO. Read-only scan found no additional stale detector-rate oracle; ordinary EchoLink/native-radio and invalid RTP clock negatives are unrelated and unchanged.
- Local `7d45ba3` replays only the verified DTMF manifest SHA replacement. These are reconciled component receipts, not a fresh full product run on the corrected head. No production source changed in either rate-oracle correction. Run the complete aggregate on the next coherent final candidate, and retain the canonical native/full PR requirements above. Standalone real wire, full package/coverage and hardware qualification on the released final cohort remain pending.

Release: https://github.com/cpeter1207/librptadvdtmf/releases/tag/v0.1.0-alpha.4
Image run: https://github.com/cpeter1207/rpt_advanced-workflows/actions/runs/43747025587
