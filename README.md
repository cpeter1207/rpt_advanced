# rpt_advanced

rpt_advanced is a repeater controller with AllStarLink linking, station IDs,
courtesy tones, announcements, and scheduled links. It supports half-duplex
and full-duplex radio systems.
It can read site meters, control GPIO outputs and cooling fans, and play alarms.

Run it as a Linux service without Asterisk, or install the optional
`app-rpt-advanced` package to run it in Asterisk with USBRadioPlus.
Debian 13 on 64-bit Raspberry Pi and Intel/AMD PCs is supported.

Start with the [installation guide](doc/install.md). It covers package and
source installation, setup, checks, upgrades, and switching controllers.

- [Configuration reference](doc/configuration.md): settings and examples.
- [Radio operation](doc/implementation-status.md): IDs, tones, and announcements.
- [AllStarLink operation](doc/allstarlink-status.md): links and DTMF commands.

This is alpha software. Test receive audio, transmit audio, PTT, IDs, and links
before placing a repeater in service.

Developer references: [quality checks](QUALITY.md), [testing](doc/testing.md),
and [architecture](doc/architecture/README.md).
