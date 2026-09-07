# rpt_advanced

Production repository for rpt_advanced.

Workflow implementations belong in
[rpt_advanced-workflows](https://github.com/cpeter1207/rpt_advanced-workflows).

Supported platforms and development requirements are defined in
[QUALITY.md](QUALITY.md). Contributor and coding-agent instructions are in
[AGENTS.md](AGENTS.md).

The initial controller-policy components and their tests are being implemented.
Run `make ci` in an ASL3 development environment with GCC, Clang tools, Cppcheck,
Doxygen, and gcovr. This does not yet build a runnable Asterisk module.

See [requirements](doc/requirements.md) and the precise
[implementation status](doc/implementation-status.md). Do not install this
development foundation on a radio node.
