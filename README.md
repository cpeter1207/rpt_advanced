# rpt_advanced

Production repository for rpt_advanced.

Licensed under GNU GPL version 2 only (`GPL-2.0-only`); see [COPYING](COPYING).

Workflow implementations belong in
[rpt_advanced-workflows](https://github.com/cpeter1207/rpt_advanced-workflows).

Supported platforms and development requirements are defined in
[QUALITY.md](QUALITY.md). Contributor and coding-agent instructions are in
[AGENTS.md](AGENTS.md).

The controller and its tests are under development.
Run `make ci` in an ASL3 development environment with GCC, Clang tools, Cppcheck,
Doxygen, gcovr, Python, and Ruff. The build produces a development Asterisk module
with configuration load/reload support and hardware-clocked radio workers.
Identifier file/speech preparation and end-to-end radio verification remain unfinished.
Tests start a separate Asterisk process with temporary configuration and no radio
connections. `make install` installs the module but does not activate it.

See [requirements](doc/requirements.md) and the precise
[implementation status](doc/implementation-status.md). Do not install this
development foundation on a radio node.
