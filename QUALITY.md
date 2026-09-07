# Platform and quality requirements

These are the established USBRadioPlus requirements applied to rpt_advanced.
They are requirements, not claims of completed implementation or passing tests.

## Supported platforms

| Operating system | Architecture | Target |
| --- | --- | --- |
| Debian 12 (Bookworm) | amd64 | Intel/AMD PC |
| Debian 12 (Bookworm) | arm64 | 64-bit Raspberry Pi |
| Debian 13 (Trixie) | amd64 | Intel/AMD PC |
| Debian 13 (Trixie) | arm64 | 64-bit Raspberry Pi |

Build and installation testing must use the necessary ASL3 packages.

## Required quality gate

- No compiler errors or warnings; compile with warnings treated as errors.
- Formatting checks, Ruff, ShellCheck, Cppcheck, and Clang-Tidy must pass for
  their applicable source types. Validation must not modify source files.
- No dead code or diagnostic suppression used to conceal defects.
- All code documented with concise Doxygen comments, with zero Doxygen errors
  or warnings. Publish generated documentation to GitHub Pages.
- Unit, functional, and integration tests must pass, with 100% line and branch
  coverage measured separately on every supported platform. Do not substitute
  combined matrix coverage for a platform's results.
- Build, packaging, and installation checks must pass on every supported
  platform.
- The same required gate must run for production-code pushes and pull requests
  and must block pull-request merging and release creation on failure.

Run platform-independent formatting, lint, static analysis, and Doxygen checks
once, concurrently where independent, before the platform matrix. Run platform
tests and coverage concurrently. Keep failures tied to real validation failures,
not fragile environmental or timing assumptions.

## Test containers

Provide prebuilt clean ASL3 test images for each supported OS/architecture
combination for installation tests. Provide images derived from those clean
images with rpt_advanced installed for functional and integration tests. Build
new installed images for every release.

Clean up project-owned stale test containers at test start and on test exit,
whether tests pass or fail. Do not remove unrelated containers.

## Workflow separation

Workflow implementations reside in
[rpt_advanced-workflows](https://github.com/cpeter1207/rpt_advanced-workflows).
Production-repository callers follow that repository's `main` branch so workflow
fixes do not require caller-pin changes. A workflow-only update must not itself
start a production build/test cycle.

Validate workflow changes independently of production tests so a broken
production workflow can be repaired without a circular gate dependency.

## Setup status

The reusable quality workflow is active. Branch protection requires its quality
gate, pull requests, linear history, and resolved review conversations, and
prohibits force pushes and branch deletion. The gate runs applicable checks for
the source types present; Ruff and ShellCheck become applicable when Python or
shell source is introduced. The current foundation is C only.

See [implementation status](doc/implementation-status.md) for the distinction
between tested components and the remaining module, container, release, and
documentation-publication work. There is no release workflow yet.
