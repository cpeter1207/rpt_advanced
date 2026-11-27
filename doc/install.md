# Install and run rpt_advanced

Use Debian 13 on a 64-bit Raspberry Pi (`arm64`) or Intel/AMD PC (`amd64`).
Keep a copy of your working configuration before an upgrade. Test on a dummy
load before placing the repeater in service.

## Choose a controller

- Install `rpt-advanced` to run the standalone Linux service. It does not need
  Asterisk or ASL3.
- Install `app-rpt-advanced` and `usbradioplus` to run the controller in
  ASL3 Asterisk.

Both choices may be installed on one node. Only one may use a given radio at
a time. Stop the other service before starting it.

## Install packages

Set up the signed [project package repository](https://cpeter1207.github.io/USBRadioPlus/).
Skip this setup if it is already listed in your APT sources.

```sh
sudo apt-get install ca-certificates curl gnupg
curl -fsSLO https://cpeter1207.github.io/USBRadioPlus/usbradioplus-archive-keyring.gpg
gpg --show-keys --with-fingerprint usbradioplus-archive-keyring.gpg
```

Check that the displayed key fingerprint is
`A0D5 A79E 0F5C 45E9 E636 7995 0951 502B AC79 5E55`.
Do not use a key whose fingerprint differs.

```sh
sudo install -d -m 0755 /etc/apt/keyrings
sudo install -m 0644 usbradioplus-archive-keyring.gpg /etc/apt/keyrings/usbradioplus.gpg
printf '%s\n' 'deb [signed-by=/etc/apt/keyrings/usbradioplus.gpg] https://cpeter1207.github.io/USBRadioPlus trixie main' | sudo tee /etc/apt/sources.list.d/usbradioplus.list
```

Install standalone:

```sh
sudo apt-get update
sudo apt-get install rpt-advanced
```

For Asterisk operation instead:

```sh
sudo apt-get update
sudo apt-get install app-rpt-advanced usbradioplus
```

APT installs the required shared libraries. Do not copy library files from a
different release over the installed files.

EchoLink uses `librptadvecholink1` with its GSM, Speex, and jitter providers.
Both controller packages install them. Set up a validated account and private
password file before enabling it; see [EchoLink links](echolink.md).

For optional site hardware, install the matching provider package:

```sh
sudo apt-get install librptadv-gpiod-adapter1 librptadv-hwmon-adapter1
```

`gpiod` supplies Linux GPIO lines. `hwmon` supplies hardware meters and supported
PWM fan outputs. IIO meters use the kernel's device files. Give the service
account access only to the devices it needs. To add a provider that is not
loaded, first set up and check the configuration with the required site items
enabled. Then restart the standalone service, or unload and load the Asterisk
module, to activate that provider.

You may also download Debian packages for your architecture from a
[release](https://github.com/cpeter1207/rpt_advanced/releases).
Put the runtime packages from that release in an empty directory, then run:

```sh
sudo apt-get install ./*.deb
```

For standalone, include `rpt-advanced`, `librptadv-product2`, the file and speech
adapter packages, and the standalone control, resolver, and jitter adapter
packages. Include `librptadv-iax2-client2` and the codec-provider runtime
packages required by that release. Packages ending in `-dev` are not needed to run the node.
Keep the signed repository enabled for other dependencies.

## Set up standalone

1. Stop Asterisk if it uses the same radio:

   ```sh
   sudo systemctl stop asterisk
   ```

2. For a new store, create an empty service-owned directory with mode 0700
   and record the chosen store UUID. Copy the packaged example from
   `/usr/share/doc/rpt-advanced/examples/rpt_advanced.service.json` to
   `/etc/rpt_advanced/rpt_advanced.service.json`. Set the absolute store path,
   its actual UUID, the write key ID, loopback listening address, accepted HTTP
   authorities, server certificate chain and authorized client identities.
   Keep the document root- or service-owned and writable only by its owner.

3. Provision the credential names listed in that document. The TLS entries use
   the `rpt-advanced.tls.` prefix. Supply DER certificates and a DER server
   private key. Each admitted client has a distinct service identity and pinned
   leaf certificate issued by a configured client CA. The store key uses
   `rpt-advanced.aead.<write_key_id>`. The standalone unit imports these
   namespaces into its private systemd credentials directory. Provide at least
   the listener credentials before startup. A missing store key leaves verified
   public configuration readable through authenticated management.

   For a new store, run `rpt-advanced-store init ROOT UUID KEY_ID` as the
   service account, with the delivered 32-byte store key in the same credential
   namespace. Use the root, UUID and key ID from the service JSON. This creates
   the selected zero-port site with activation pending.

4. Start the service and check its log.

   ```sh
   sudo systemctl enable --now rpt-advanced
   systemctl status rpt-advanced
   sudo journalctl -u rpt-advanced -n 50 --no-pager
   ```

   The authenticated management service starts with zero radio ports. Configure
   the new site by first applying its pending desired revision through authenticated
   management. After confirmation, register each port and select its radio device
   before enabling it.
   A standalone preflight accepts `rpt-advanced --check-config <service-json>`
   in a process with the service account's access to the store and the same
   systemd credentials. It checks the selected generation without opening radio
   hardware.

The service runs as the `rpt-advanced` user. Do not run it as root.
Speech needs Piper and a local voice model. Put the model where that user can
read it, outside a home directory, and set its path in the configuration.

## Check the radio

1. Leave links disconnected during the local test.
2. Transmit a short voice call into the receiver with the required CTCSS or DCS.
3. Check that receive audio is clear and PTT releases after the hang time.
4. Send `*722` and unkey. Listen for the time announcement.
5. Check any configured courtesy tone and station ID.
6. Connect a permitted test node. Check audio in both directions, then disconnect.

If a test fails, check the log and radio settings before changing audio gains.
See [radio operation](implementation-status.md) and
[link operation](allstarlink-status.md).

## Read controller status

Use the authenticated operator CLI to read a point-in-time snapshot.

```sh
rpt-advanced-cli status snapshot
rpt-advanced-cli status snapshot --site --port PORT_ID --topics radio,meters
```

With no selectors, the command reads the site summary. Discover port resource IDs
through the HAL status links. Repeat `--port` to select multiple ports. The
command assembles all pages from one capture and writes one JSON document to
stdout. If a capture expires, run the command again to take a fresh snapshot.

## Apply settings and upgrades

To tune a standalone radio, install the tuner and select your node:

```sh
sudo apt-get install usbradioplus-tune
sudo usbradioplus-tune --backend standalone --node 524950
```

Replace `524950` with your node number. Keep the standalone service running.
The tuner uses that service's configuration file. Its menus provide receive
and transmit calibration, hardware levels, meters, processing status, and EEPROM
tuning. Use a dummy load for transmit tests. Close the tuner when done; it
releases its test tone and temporary PTT request.

After editing the working JSON files, request a checked reload.

```sh
sudo systemctl reload rpt-advanced
sudo journalctl -u rpt-advanced -n 50 --no-pager
```

The service queues the request with the current desired validator and working
fingerprint. Check the operation receipt and the active revision through the
authenticated management API. Restart the unit to refresh its
systemd credentials or its listener provisioning.

To upgrade standalone:

```sh
sudo apt-get update
sudo apt-get install --only-upgrade rpt-advanced
sudo systemctl restart rpt-advanced
```

Restart after a package upgrade, then repeat the radio checks. A restart drops
live links; configured permanent links will retry.

## Set up the Asterisk controller

Stop standalone if it owns this store or radio. Provision the same service
document format at `rpt_advanced.service.json` in Asterisk's configuration
directory, usually `/etc/asterisk`. Give the Asterisk service account access
to the protected store. Configure `ImportCredential=rpt-advanced.tls.*` and
`ImportCredential=rpt-advanced.aead.*` in its systemd service drop-in. The
radio driver's own `/etc/asterisk/usbradioplus.conf` remains provider-owned.

In `/etc/asterisk/modules.conf`, load the radio driver before the controller:

```ini
load = chan_usbradioplus.so
load = app_rpt_advanced.so
noload = app_rpt.so
```

Remove any conflicting `load` or `noload` line for those modules. Configure
the ASL3 IAX channel and dialplan for incoming links. Packages do not edit
`modules.conf`, `rpt.conf`, or an existing controller configuration for you.

Start Asterisk and check the module:

```sh
sudo systemctl start asterisk
sudo asterisk -rx 'module show like app_rpt_advanced'
sudo asterisk -rx 'rpt_advanced link status 524950'
```

Replace `524950` with your node number. After editing controller settings, run:

```sh
sudo asterisk -rx 'module reload app_rpt_advanced.so'
```

Check the log and repeat the radio tests. In Asterisk, use
`core show translation` to check loaded audio codecs if a link cannot connect.

## Switch controllers

Keep all working configuration files. Stop one service before starting the
other; never let both control the same interface.

Switch to standalone:

```sh
sudo systemctl disable --now asterisk
sudo systemctl enable --now rpt-advanced
```

Switch to the configured Asterisk controller:

```sh
sudo systemctl disable --now rpt-advanced
sudo systemctl enable --now asterisk
```

To use ASL3 `app_rpt` instead of `app_rpt_advanced`, stop Asterisk, select
`app_rpt.so` in `modules.conf`, and disable `app_rpt_advanced.so` there.
Keep the working `rpt.conf` and USBRadioPlus channel settings. Start Asterisk
and test the radio before connecting links.

## Remove or restore a package

Stop the service before removing it:

```sh
sudo systemctl disable --now rpt-advanced
sudo apt-get remove rpt-advanced
```

`remove` keeps configuration files. `purge` removes them. To restore a saved
release, install its matching runtime packages with APT, restore its working
configuration, restart the selected service, and repeat the radio checks.

## Build packages from source

Use released packages for normal node operation. To build, install the tools
and development packages listed in `debian/control`. This includes
Rust/Cargo 1.85 or later, a C compiler, `pkg-config`, `libclang-dev`,
`libspeexdsp-dev`, and the radio and audio library development packages.
The Asterisk package also needs matching ASL3 Asterisk development headers.

Extract the source archive, enter its directory, and build standalone packages:

```sh
dpkg-buildpackage -Pstandalone -us -uc -b
```

To include the Asterisk adapter, run `dpkg-buildpackage -us -uc -b` instead.
Install the resulting runtime packages with APT. See the
[testing guide](testing.md) for build checks.

## Typed physical radio on Asterisk

Install `usbradioplus` version `0.1.0~alpha38-1` or later when an Asterisk
port uses a typed physical radio. This optional package supplies the driver
station binding in `libusbradioplus_asterisk.so.1`. The standalone host uses
the station ownership operations in `libusbradioplus-product1`.
