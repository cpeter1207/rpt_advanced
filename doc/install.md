# Installation

Debian 13 amd64 and arm64 packages are the supported release targets. Install
the standalone controller or the optional Asterisk adapter; neither package
requires the other. The standalone runtime has no Asterisk or ASL3 dependency.
It does require the separately released `libusbradioplus-product1` shared radio
runtime, which also has no Asterisk dependency.

## Standalone controller

Configure the signed USBRadioPlus package repository for the target architecture
as described in the [USBRadioPlus installation guide](https://github.com/cpeter1207/USBRadioPlus/blob/main/INSTALL.md),
so APT can obtain `libusbradioplus-product1`.
Download the rpt_advanced Debian 13 runtime packages from the same rpt_advanced
release into an otherwise empty directory, then install the package set with
APT resolving the separately released shared-library dependencies:

```sh
sudo apt-get install ./*.deb
```

The `rpt-advanced` package installs `/usr/bin/rpt-advanced`, a systemd service,
the example configuration at `/etc/rpt_advanced/rpt_advanced.conf`, and the
CM119 udev rule. It creates a non-root `rpt-advanced` service account. Edit the
configuration before enabling a node: the shipped example has no radio node
enabled. Configure a stable `device_identifier` or `usb_serial` in `[radio]` or
`[radio <node>]`, and set `node_enabled = yes` in the selected node section.
Consult [configuration](configuration.md) for device, signaling, and audio-graph
settings.

Enable and inspect the service:

```sh
sudo systemctl enable --now rpt-advanced
sudo systemctl status rpt-advanced
sudo journalctl -u rpt-advanced
```

Changes to the standalone configuration are applied by restarting the service:

```sh
sudo systemctl restart rpt-advanced
```

The process handles SIGHUP as a configuration reload request. The service runs
as `rpt-advanced`, with access to CM119 USB control and the audio group; do not
run it as root. Install Piper separately if speech announcements are wanted and
configure a local model readable by the service account. File playback uses the
FFmpeg adapter.

To stop or roll back the standalone service:

```sh
sudo systemctl disable --now rpt-advanced
sudo apt-get remove rpt-advanced
```

APT removal leaves `/etc/rpt_advanced` intact. Back up the configuration before
purging it or before changing hardware ownership.

## Optional Asterisk adapter

`app-rpt-advanced` is a deprecated compatibility adapter for ASL3 Asterisk. It
is packaged separately and may be installed without the standalone controller.
It requires ASL3 Asterisk and a matching USBRadioPlus `RadioPlusAdvanced`
channel adapter. Install its Debian 13 packages from the same rpt_advanced
release, with APT resolving their shared-library dependencies.

For the Asterisk path, the local RadioPlusAdvanced exchange is fixed at 48 kHz
signed-linear PCM. IAX peer codecs are negotiated separately and converted at
the peer boundary. Load the codec modules needed by the peers; installed but
unloaded codec modules are unavailable to negotiation. Use `core show
translation` to inspect available paths.

Copy the shipped example to `/etc/asterisk/rpt_advanced.conf` if it is not
already present. Configure the node's `radio_channel`, identification, and
`node_enabled = yes`. Do not assign the same physical radio to the standalone
service and an Asterisk channel adapter at the same time.

Load USBRadioPlus before the adapter:

```sh
sudo asterisk -rx 'module load chan_usbradioplus.so'
sudo asterisk -rx 'module load app_rpt_advanced.so'
sudo asterisk -rx 'module show like app_rpt_advanced'
```

Persistent loading is configured manually in Asterisk's `modules.conf`. The
adapter does not modify `modules.conf`, `rpt.conf`, or the active
`rpt_advanced.conf` during installation. Configure the normal ASL3 dialplan and
IAX settings separately for incoming AllStarLink connections.

Reload the Asterisk module after editing its configuration:

```sh
sudo asterisk -rx 'module reload app_rpt_advanced.so'
```

Invalid configuration keeps the active generation unchanged. A module reload
may replace radio workers, so use an approved maintenance window and verify
receive, PTT, audio, telemetry, link, and unkey behavior afterward. To roll back,
disable `app_rpt_advanced.so` in `modules.conf`, restore the previous module and
configuration, and restart Asterisk as needed.

## Build from source

Use the package release for routine installation. A source build requires Rust
1.85/Cargo, a C compiler, `libclang-dev`, `pkg-config`, FFmpeg, and the matching
`libusbradioplus-product-dev` package, including
`/usr/include/usbradioplus_product.h` and the `usbradioplus_product` pkg-config
file. It also requires the development packages for the versioned radio, GPIO,
audio, IAX2, samplerate, and PCM-ring libraries. An Asterisk build additionally
requires the public ASL3 Asterisk development headers; a full Asterisk source
tree is not used.

Build the standalone binary package without the Asterisk adapter:

```sh
dpkg-buildpackage -Pstandalone -b
```

Build the compatibility module package set with the default profile:

```sh
dpkg-buildpackage -b
```

The repository's `make distcheck` verifies the source archive and staged
installation. See [QUALITY.md](../QUALITY.md) and the [testing guide](testing.md)
for the supported build and verification process. No node should be activated
until package integrity and hardware behavior have been checked.
