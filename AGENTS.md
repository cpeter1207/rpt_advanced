# rpt_advanced development rules

Follow [QUALITY.md](QUALITY.md). These requirements match USBRadioPlus's
platform and quality requirements; they do not define product features.

Before production development, establish the required automated quality gate.
Every code change must keep that gate passing. Do not commit, merge, tag, or
release code that fails compilation with warnings treated as errors, formatting,
Ruff, ShellCheck, Cppcheck, Clang-Tidy, Doxygen, tests, install checks, or 100%
line and branch coverage. Remove dead code instead of suppressing diagnostics
or excluding it from coverage.

Document all code with concise, meaningful Doxygen comments. Update tests,
manuals, examples, and install artifacts with every affected interface.

Run platform-independent checks once, concurrently where independent. Run
platform tests and coverage concurrently across Debian 12 and 13 on amd64 and
arm64. Pushes, pull requests, and releases must use the same required gate.

Use prebuilt Intel container images for local validation. When those checks
pass, commit and push so GitHub runs the parallel platform matrix in known-good
prebuilt images on native runners. Local QEMU failures are not a prerequisite
to resolve before pushing. The complete GitHub matrix must pass before merge
or release.

Keep workflow implementations in rpt_advanced-workflows; this repository may
contain only the thin callers needed to invoke them. Workflow maintenance must
not depend on production tests passing.

Never deploy to a node or alter its configuration without explicit approval.
Do not add features or requirements that the user has not requested.

Configuration reload and module unload/load must work without restarting
Asterisk. Preserve this requirement until a concrete technical limitation makes
it impractical; discuss that limitation with the user before relaxing it. Test
reload and cleanup while the same Asterisk process remains running.
