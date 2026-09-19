# rpt_advanced

rpt_advanced is a Rust-owned Asterisk radio controller for USBRadioPlus. It
provides half- and full-duplex operation, transmitter hang time, and
prioritized file, speech, and Morse identification for independently configured
nodes. A minimal C module shim supplies Asterisk metadata only; controller,
media, radio, and control behavior live in versioned Rust shared objects.
The product library embeds the controller core once; separate Asterisk entry,
control-execution, FFmpeg/file, and Piper/speech providers expose versioned C
descriptors. File and speech providers are independently replaceable, and speech
does not require FFmpeg. See the [artifact ownership map](doc/architecture/README.md#current-product-artifacts).

Licensed under GNU GPL version 2 only (`GPL-2.0-only`); see [COPYING](COPYING).

Workflow implementations belong in
[rpt_advanced-workflows](https://github.com/cpeter1207/rpt_advanced-workflows).

Supported platforms and development requirements are defined in
[QUALITY.md](QUALITY.md). Contributor and coding-agent instructions are in
[AGENTS.md](AGENTS.md).

This is development software. The owner confirmed the current local audio fixes
on test node 524950. Full hardware acceptance and interoperability with every
supported peer remain unfinished; see [implementation status](doc/implementation-status.md).
Do not use it for unattended operation.

Install `rate_adjusting_pcm_ring2` 2.0.0-alpha.3 and
`rptadv-samplerate-adapter` 0.1.0-alpha.2 or newer matching shared-library releases
before building or installing rpt_advanced. They supply the lock-free playout ring and persistent
sample-rate conversion used by the product's media paths and Asterisk edge.

On Debian 13, with the ASL3 package repository configured, install the binary
package set from [the installation guide](doc/install.md) or build from source:

```sh
sudo apt-get install ./*.deb

sudo apt-get install build-essential cargo rustc libclang-dev pkg-config dh-sequence-asterisk \
  librate-adjusting-pcm-ring2-dev librptadv-samplerate-adapter-dev ffmpeg
make -j2
sudo make prefix=/usr install
```

Cargo builds the production Rust components; `make` remains the conventional
orchestration entry point. Use development headers matching the installed ASL3
Asterisk. A full Asterisk source tree is not needed. Installation does not
activate the controller or replace configuration. Read
[installation and activation](doc/install.md) before assigning a radio.

See the [configuration reference](doc/configuration.md) for nodes, IDs, and
link controls, and [testing guide](doc/testing.md) for automated and hardware
verification. The [AllStarLink status](doc/allstarlink-status.md) distinguishes
the local implementation from pending live interoperability validation.
Developer API documentation is generated from Rustdoc (plus Doxygen for the C
metadata shim and public C adapter headers) by `make docs`; `make ci` runs the
required quality gate in the documented development environment.

See [requirements](doc/requirements.md) and the precise
[implementation status](doc/implementation-status.md).
Architecture and durable design decisions are maintained in
[doc/architecture](doc/architecture/README.md).
