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

You may also download Debian packages for your architecture from a
[release](https://github.com/cpeter1207/rpt_advanced/releases).
Put the runtime packages from that release in an empty directory, then run:

```sh
sudo apt-get install ./*.deb
```

For standalone, include `rpt-advanced`, `librptadv-product1`, the file and speech
adapter packages, and the standalone control, resolver, and jitter adapter
packages. Packages ending in `-dev` are not needed to run the node.
Keep the signed repository enabled for other dependencies.

## Set up standalone

1. Stop Asterisk if it uses the same radio:

   ```sh
   sudo systemctl stop asterisk
   ```

2. Edit `/etc/rpt_advanced/rpt_advanced.conf`. Use the
   [configuration reference](configuration.md) to set your node number,
   callsign, radio device, COS source, CTCSS, levels, and IDs. Set
   `node_enabled = yes` for the node you want to run. The sample has no enabled
   node. Use `device_identifier` or `usb_serial` to select the radio interface.

3. Set the AllStarLink node password and registration options before enabling
   links. Follow the secrets-file instructions in the
   [configuration reference](configuration.md). Do not put passwords in a
   public example.

4. Check the configuration and installed libraries:

   ```sh
   sudo -u rpt-advanced rpt-advanced --check-config /etc/rpt_advanced/rpt_advanced.conf
   sudo -u rpt-advanced rpt-advanced --check-providers
   ```

5. Start the service and check its log:

   ```sh
   sudo systemctl enable --now rpt-advanced
   systemctl status rpt-advanced
   sudo journalctl -u rpt-advanced -n 50 --no-pager
   ```

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

## Apply settings and upgrades

Check the configuration, then reload it without stopping the service:

```sh
sudo -u rpt-advanced rpt-advanced --check-config /etc/rpt_advanced/rpt_advanced.conf
sudo systemctl kill --kill-whom=main --signal=HUP rpt-advanced
sudo journalctl -u rpt-advanced -n 50 --no-pager
```

A failed reload leaves the running settings in place. Check warnings as well
as errors. Changing the radio device may briefly interrupt audio.

To upgrade standalone:

```sh
sudo apt-get update
sudo apt-get install --only-upgrade rpt-advanced
sudo systemctl restart rpt-advanced
```

Restart after a package upgrade, then repeat the radio checks. A restart drops
live links; configured permanent links will retry.

## Set up the Asterisk controller

Stop standalone first. Keep `/etc/rpt_advanced/rpt_advanced.conf` for later use.
Configure `/etc/asterisk/usbradioplus.conf` for the radio and
`/etc/asterisk/rpt_advanced.conf` for the controller. Set `radio_channel` and
`node_enabled = yes` for each enabled node.

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
