# rpt_advanced

`rpt_advanced` is a standalone, Rust-based radio controller for AllStarLink-compatible
repeaters. It owns radio control and link behavior without requiring Asterisk or ASL3.
An optional, deprecated Asterisk adapter is packaged separately for existing ASL3 systems.

Install the `rpt-advanced` Debian package to run the standalone system service, or
`app-rpt-advanced` to use the Asterisk adapter. The packages can be installed
independently; shared runtime libraries are versioned dependencies. See the
[installation guide](doc/install.md) before selecting either path.

Configuration is documented in the [configuration reference](doc/configuration.md).
Build and verification requirements are in [QUALITY.md](QUALITY.md), and automated
testing is described in [doc/testing.md](doc/testing.md). Durable architecture decisions
are recorded under [doc/architecture](doc/architecture/README.md).

This is alpha software. Review [implementation status](doc/implementation-status.md)
and [AllStarLink compatibility status](doc/allstarlink-status.md) before deployment.
