# rpt_advanced development rules

## Shared rpt_advanced project baseline

This baseline applies to every production, shared-library, and workflow
repository in the rpt_advanced project. Repository-specific rules may add
constraints but must not weaken it.

Run platform-independent formatting, lint, static analysis—including
Cppcheck—and Doxygen once, concurrently where independent. Do not run Cppcheck
in each platform job. Run platform-dependent tests, coverage, build, packaging,
and staged-install checks concurrently across Debian 12 and 13 on native amd64
and arm64. Quality checks must not rewrite source files.

Complete the full quality gate before pushing, opening or updating a pull
request, merging, tagging, or releasing. Local recovery commits may follow
affected targeted checks, but must not be represented as fully verified or used
for a push, pull request, merge, tag, or release until the full gate passes.
Treat compiler warnings as errors and fail applicable formatting, Ruff,
ShellCheck, Cppcheck, Clang-Tidy, Doxygen, tests, installation checks, and 100%
line and branch coverage. Remove unreachable or dead code instead of
suppressing diagnostics or excluding it from coverage.

Update concise Doxygen comments, tests, user documentation, examples, and
build, install, and package artifacts whenever an interface changes. Consumers
of a shared project library must use its released, versioned dynamic shared
object rather than vendor or statically link a duplicate implementation.
Preserve published ABI/API compatibility whenever practical; when a change is
necessary, document its compatibility, SONAME/package consequences, and
migration. Start and clean only project-owned, labeled test containers
deterministically. Never deploy to a node or alter its configuration without
explicit approval.

Follow [QUALITY.md](QUALITY.md). These requirements match USBRadioPlus's
platform and quality requirements; they do not define product features.

Before production development, establish the required automated quality gate.
Run the full gate before a commit intended to push, a pull request, merge, tag,
or release. Do not push, merge, tag, or release code that fails compilation
with warnings treated as errors, formatting, Ruff, ShellCheck, Cppcheck,
Clang-Tidy, Doxygen, tests, install checks, or 100% line and branch coverage.
Remove dead code instead of suppressing diagnostics or excluding it from
coverage. Local commits are encouraged as small recovery points after affected
targeted checks; batch related small requirements before the next full-gate
commit and push.

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

Run platform-independent checks once, concurrently where independent. Run
platform tests and coverage concurrently across Debian 12 and 13 on amd64 and
arm64. Pushes, pull requests, and releases must use the same required gate.

Use prebuilt Intel container images for local validation. When those checks
pass, commit and push so GitHub runs the parallel platform matrix in known-good
prebuilt images on native runners. Local QEMU failures are not a prerequisite
to resolve before pushing. The complete GitHub matrix must pass before merge
or release.

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
