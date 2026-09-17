# rpt_advanced development rules

Initial-alpha clarification (2026-09-13): ADR 0040 supersedes instructions below
that require backward compatibility with earlier project alpha artifacts. Do
not retain compatibility-only code or interfaces. Preserve required current
behavior and external interoperability; update consumers and artifact-version
checks together so incompatible combinations fail safely.

## Shared rpt_advanced project baseline

This baseline applies to every production, shared-library, and workflow
repository in the rpt_advanced project. Repository-specific rules may add
constraints but must not weaken it.

Before a push, run platform-independent formatting, lint, and static
analysis—including Cppcheck—without rewriting source files. Do not run
Cppcheck in each platform job. GitHub repeats only those fast checks for an
ordinary push.

The full quality gate runs for every pull request and must pass before that
pull request can merge. It runs platform-independent formatting, lint, static
analysis, and Doxygen once, concurrently where independent; then it runs
platform-dependent build, tests, packaging, and staged-install checks
concurrently on native Debian 13 amd64 and arm64. Require 100% line and branch
coverage of production code only on Debian 13 amd64; test code is excluded from
coverage. Debian 12 support is aspirational: do not run automated Debian 12
tests or build Debian 12 packages as part of ordinary pushes, pull requests, or
releases. Build Debian 12 packages manually only when explicitly requested.
Automated releases publish Debian 13 packages only; node installations use
Debian 13 arm64 packages. A release uses a main revision that has already
passed the pull-request gate and performs only release-artifact validation; it
does not repeat the full gate.

Local recovery commits may follow affected targeted checks, but must not be
represented as fully verified until the pull-request gate passes.
Treat compiler warnings as errors and fail applicable formatting, Ruff,
ShellCheck, Cppcheck, Clang-Tidy, Doxygen, tests, installation checks, and 100%
line and branch coverage of production code on Debian 13 amd64. Remove
unreachable or dead code instead of suppressing diagnostics or excluding it
from coverage.

Update concise developer-facing in-source documentation, tests, and affected
build, install, and package artifacts before every implementation commit.
Defer user-facing documentation—manuals, examples, and other operator-facing
material—until immediately before creating a pull request. Consumers of a
shared project library must use its released, versioned dynamic shared object
rather than vendor or statically link a duplicate implementation.
Preserve published ABI/API compatibility whenever practical; when a change is
necessary, document its compatibility, SONAME/package consequences, and
migration. Start and clean only project-owned, labeled test containers
deterministically. Never deploy to a node or alter its configuration without
explicit approval.

Follow [QUALITY.md](QUALITY.md). These requirements match USBRadioPlus's
platform and quality requirements; they do not define product features.

Before production development, establish the required automated quality gate.
Run formatting, lint, and static analysis before a commit intended to push;
GitHub repeats those checks for the push. Do not merge a pull request until its
full gate has passed. Do not release code unless it is already a validated main
revision; the release workflow may run artifact checks but must not repeat the
full gate. Do not push code that fails compilation with warnings
treated as errors, formatting, Ruff, ShellCheck, Cppcheck, or Clang-Tidy.
Require Doxygen, tests, install checks, and 100% production-code line and
branch coverage only for the pull-request gate. Remove dead code instead of
suppressing diagnostics or excluding it from coverage. Local commits are
encouraged as small recovery points after affected targeted checks.

Document all code with concise, meaningful Doxygen comments. Update tests,
manuals, examples, and install artifacts with every affected interface.

Before designing or changing code, read `WISHLIST.md`,
`doc/architecture/README.md`, and every ADR relevant to the affected boundary.
Use recorded requirements and decisions as implementation constraints, but do
not implement unrequested future requirements or expand the requested scope.
Update the applicable architecture documentation and ADR when a change alters a
recorded boundary, ownership rule, invariant, or decision.

Reread every applicable `AGENTS.md` before starting a new requirement, before
the full quality gate or a deployment decision, and after a resumed or extended
work period. Treat newly changed instructions as immediately controlling.

Never rely on a locally cached quality image. Before a local quality or
container test, pull the required `:latest` image, record or inspect its
manifest digest, and use that freshly pulled image for the run. When a quality
base image changes, rebuild and verify every derived quality image before its
consumer gate uses it. Hosted jobs must likewise consume a freshly published,
verified native multi-architecture manifest rather than a known stale image.

Prefer simple, linear control flow and minimize branches, especially in error
handling. Share validation and error paths when their observable behavior is
the same; introduce a branch only for a genuine behavioral, safety, or
diagnostic distinction. Do not weaken validation or hide useful errors merely
to reduce coverage surface.

Treat test-surface size as an implementation cost. Prefer one shared,
data-driven test over duplicated cases for equivalent behavior, and design
interfaces so only genuinely distinct observable behavior introduces a branch
or test case. Preserve full required coverage, safety, and diagnostics.

After implementation work begins, continue through the next safe in-scope
steps without pausing for ordinary progress reports or intermediate results.
Stop only for a material user decision, an external condition that genuinely
prevents progress, or a ready-for-manual-test handoff. Continue to provide
concise progress updates and never deploy without explicit approval.

When implementing a requirement, also address another documented requirement
when the same work directly and safely enables it, especially an
architecture-only improvement that does not change application behavior. Do
not add speculative features or delay the requested core behavior's manual
validation. Establish and test that core behavior first; then complete related
opportunistic work that remains in scope.

Before implementing a new requirement, identify its documented prerequisites.
Implement missing prerequisites first. If a prerequisite changes application
behavior, prepare it for manual validation and obtain user confirmation before
implementing behavior that depends on it. Node 524950 is the designated
dummy-load test target when explicit deployment approval is given; its
availability does not authorize deployment by itself. Architecture-only
prerequisites may be completed without a manual-test handoff.

When an explicitly approved patch targets an arm64 node, use a native build in
the target user's home-directory workspace rather than `/tmp`. Keep the source
revision and installed artifacts synchronized with the local worktree.

When a new requirement benefits from live-node validation, first implement the
simplest core behavior that satisfies it with the fewest edge cases. Preserve
required automated quality checks and never deploy without explicit approval.
After approved live testing, obtain user confirmation that the core behavior is
correct before adding noncritical edge-case handling. Safety, data integrity,
configuration validity, and required compatibility are core behavior and must
not be deferred.

Treat every published ABI as stable. Reuse existing ABI functions, opaque
objects, callbacks, fields, and extension points before adding a new contract.
Change an ABI only as a last resort when no compatible implementation exists;
document the reason, compatibility impact, SONAME/package action, and required
migration in the applicable ADR and release documentation.

Treat unknown configuration sections and parameter names as warnings and ignore
them. Treat unknown, malformed, or unsupported values as warnings and use the
normal inherited sensible default. Do not reject a configuration for those
conditions; reject it only when no safe deterministic configuration can be
constructed after defaults are applied. Emit useful warnings and test both
warning fallback and true-error behavior.

Run the full pull-request gate's platform-independent checks once,
concurrently where independent. Run Debian 13 platform tests concurrently on
amd64 and arm64, with production coverage on amd64 only. Pushes run only the
fast preflight; pull requests use the required full gate; releases verify
artifacts from a previously validated main revision. Debian 12 validation and
packages are manual-only.

Use prebuilt Intel container images for local validation. When the fast checks
pass, commit and push so GitHub can run them in known-good images. Local QEMU
failures are not a prerequisite to resolve before pushing. The complete GitHub
matrix must pass before merge; release inputs are already merged revisions.

Run explicitly started project test containers through the labeled launcher in
`rpt_advanced-workflows`. Use the maintained `latest` images and clean only
containers labeled `rpt_advanced.test=true` before and after each run.

Keep iteration evidence in the ignored `/.work/` directory. For a coverage,
Doxygen, test, lint, or defect repair, run only the affected source component
and its targeted checks first. Record completed source-component coverage,
Doxygen checks, and targeted test cases in `/.work/quality-progress.md` so they
are not needlessly rerun or reread. Work through the remaining components from
the most recent full report. Run the complete quality gate only after targeted
checks have resolved every recorded component-level issue.

Keep workflow implementations in rpt_advanced-workflows; this repository may
contain only the thin callers needed to invoke them. Workflow maintenance must
not depend on production tests passing.

Never deploy to a node or alter its configuration without explicit approval.
Do not add features or requirements that the user has not requested.

Configuration reload and module unload/load must work without restarting
Asterisk. Preserve this requirement until a concrete technical limitation makes
it impractical; discuss that limitation with the user before relaxing it. Test
reload and cleanup while the same Asterisk process remains running.
