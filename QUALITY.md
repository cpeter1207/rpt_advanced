# Platform and quality requirements

These are the established USBRadioPlus requirements applied to rpt_advanced.
They are requirements, not claims of completed implementation or passing tests.

## Supported platforms

| Operating system | Architecture | Target |
| --- | --- | --- |
| Debian 13 (Trixie) | amd64 | Intel/AMD PC |
| Debian 13 (Trixie) | arm64 | 64-bit Raspberry Pi |

Build and installation testing uses the necessary ASL3 packages. Debian 12 is
aspirational and is built manually only when explicitly requested.

## Required quality gate

- No compiler errors or warnings; compile with warnings treated as errors.
- Formatting checks, Ruff, ShellCheck, Cppcheck, Clang-Tidy, Rustfmt, Clippy,
  and Rustdoc must pass for their applicable source types. Validation must not
  modify source files.
- No dead code or diagnostic suppression used to conceal defects.
- C-compatible surfaces have concise Doxygen comments; Rust has concise
  Rustdoc. Both documentation checks run with zero errors or warnings. Publish
  generated documentation to GitHub Pages after a merged pull request.
- Unit, functional, and integration tests must pass on Debian 13 amd64 and
  arm64. Require 100% line and branch coverage of production code on Debian 13
  amd64 only.
- Build, packaging, and installation checks must pass on native Debian 13 amd64
  and arm64.
- A production push runs only formatting, lint, and static analysis. It does
  not run Doxygen, the platform matrix, coverage, packaging, or installation
  checks.
- The complete gate runs for every pull request and is required before merging.
  A release relies on the successful gate already run for the merged pull
  request and performs only release-artifact validation; it does not repeat the
  complete gate.

The complete pull-request gate runs platform-independent formatting, lint,
static analysis, and Doxygen checks once, concurrently where independent,
before the native Debian 13 platform matrix. Run platform tests concurrently.
Keep failures tied to real validation failures, not fragile environmental or
timing assumptions.

`make rust-check` runs Rust formatting, Clippy with warnings denied, Rustdoc
with warnings denied, and the workspace tests. `make rust-coverage` reports
Rust coverage on Debian 13 amd64 using the Rust 1.85 MSRV toolchain. Task 12
adds production line and branch thresholds when the coverage harness is
finalized. `make check` continues to run the C reference suite while the
migration is in progress.

## Test containers

Provide prebuilt clean ASL3 test images for native Debian 13 amd64 and arm64
installation tests. Provide images derived from those clean images with
rpt_advanced installed for functional and integration tests. Build new installed
images for every release.

Clean up project-owned stale test containers at test start and on test exit,
whether tests pass or fail. Do not remove unrelated containers.

Use the deterministic launcher maintained in `rpt_advanced-workflows` for every
explicitly started test container. It labels only project-owned containers,
cleans those labels before launch, and installs an exit cleanup trap. Quality
images intentionally use their published `latest` tags; do not replace that
policy with ad-hoc local builds during normal validation.

## Workflow separation

Workflow implementations reside in
[rpt_advanced-workflows](https://github.com/cpeter1207/rpt_advanced-workflows).
Production-repository callers follow that repository's `main` branch so workflow
fixes do not require caller-pin changes. The push caller runs only formatting,
lint, and static analysis; the pull-request caller runs the complete quality
gate. A workflow-only update must not itself start a production build/test
cycle.

Validate workflow changes independently of production tests so a broken
production workflow can be repaired without a circular gate dependency.

## Setup status

The reusable workflows are active. Branch protection requires the full
pull-request quality gate, pull requests, linear history, and resolved review
conversations, and prohibits force pushes and branch deletion. The gate runs
applicable checks for the source types present. Ruff checks the Python Asterisk
integration runner; ShellCheck applies when shell source is introduced.
Module-interface code uses GNU C as required by Asterisk's headers; the
controller library uses strict C11. Clang analysis enables the block syntax
present in Asterisk's headers. Both source directories are included in static
analysis, Doxygen, and coverage.

See [implementation status](doc/implementation-status.md) for the distinction
between tested components and the remaining module, container, release, and
documentation-publication work. Version-tag releases invoke the reusable release
workflow, which verifies release images and artifacts without repeating the
complete pull-request gate before publishing the source archive and checksum.
