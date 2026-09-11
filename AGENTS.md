# rpt_advanced development rules

Follow [QUALITY.md](QUALITY.md). These requirements match USBRadioPlus's
platform and quality requirements; they do not define product features.

Before production development, establish the required automated quality gate.
Every code change must keep that gate passing. Do not commit, merge, tag, or
release code that fails compilation with warnings treated as errors, formatting,
Ruff, ShellCheck, Cppcheck, Clang-Tidy, Doxygen, tests, install checks, or 100%
line and branch coverage of production code on Debian 13 amd64. Test code is
excluded from the coverage requirement. Remove dead code instead of suppressing
diagnostics or excluding it from coverage.

Document all code with concise, meaningful Doxygen comments. Update tests,
manuals, examples, and install artifacts with every affected interface.

Run platform-independent checks once, concurrently where independent. Run
Debian 13 platform build, test, packaging, and staged-install checks
concurrently on amd64 and arm64, with production coverage on amd64 only.
Debian 12 support is aspirational: do not run automated Debian 12 tests or
build Debian 12 packages as part of ordinary pushes, pull requests, or
releases. Build Debian 12 packages manually only when explicitly requested.
Automated releases publish Debian 13 packages only; node installations use
Debian 13 arm64 packages. Pushes, pull requests, and releases must use the
same required gate.

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
