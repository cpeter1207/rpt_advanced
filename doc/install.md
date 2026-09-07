# Installation and activation

## Prerequisites

Supported test targets are Debian 12 and 13 on amd64 and arm64, using ASL3
Asterisk and matching `asl3-asterisk-dev` headers. Build with `build-essential`.
USBRadioPlus must provide the `RadioPlusAdvanced` channel technology; older
releases without that adapter cannot serve this controller. No radio hardware
is required for the automated synthetic-radio tests.

FFmpeg prepares sound files and synthesized speech before a radio worker starts.
For speech, install an offline Piper executable named `piper` in Asterisk's
service PATH and configure a local voice model. The service account must be able
to read the model, its companion JSON file, and all configured sound files.
Missing or failed file/speech preparation falls back to the configured Morse ID.
See [configuration](configuration.md) for the complete media hierarchy.

## Build and install

From the source directory:

```sh
make -j2
sudo make prefix=/usr install
```

The module is installed as
`/usr/lib/<Debian multiarch triplet>/asterisk/modules/app_rpt_advanced.so`.
Set `asteriskmoddir` explicitly if Asterisk uses another module directory.
Use `DESTDIR` for staging; it prefixes install destinations without changing
runtime configuration paths. `make install` does not edit `modules.conf`,
`rpt.conf`, or any active configuration.

The current install also provides the controller static archive and headers
under the selected prefix, the license under `share/doc/rpt_advanced/copyright`,
and the disabled example under `share/doc/rpt_advanced/examples/`.

## Activate a test node

Back up the existing configuration and any previously installed module first.
Do not assign the same USBRadioPlus radio to app_rpt and rpt_advanced concurrently.
Stop its existing controller before enabling it here. Other radios may continue
to use app_rpt.

Copy the installed example to Asterisk's configuration directory, normally
`/etc/asterisk/rpt_advanced.conf`. Set the named node's `radio_channel` to its
USBRadioPlus channel name, without a technology prefix. Configure identification
appropriate for the station, then set that node's `node_enabled = yes`.
Do not enable the untouched example: it has no identification text.

Load USBRadioPlus first, then load the controller:

```sh
sudo asterisk -rx 'module load app_rpt_advanced.so'
sudo asterisk -rx 'module show like app_rpt_advanced'
sudo asterisk -rx 'core show channels'
```

Inspect Asterisk's log for configuration or radio-start failures. A loaded module
alone does not prove that a radio is working. Verify receive indication, PTT,
audio, identification, and transmitter release with appropriate test equipment.
No dialplan application call is required: enabled nodes start when the module
loads. For persistent loading, configure `modules.conf` to load USBRadioPlus
before `app_rpt_advanced.so`; avoid a conflicting `noload` entry.

## Reload and rollback

After editing configuration:

```sh
sudo asterisk -rx 'module reload app_rpt_advanced.so'
```

Invalid configuration leaves the running nodes unchanged. A valid reload stops
and replaces workers, including preparing ID media, so audio can pause. A failed
radio start attempts to restore the previous configuration; inspect the log for
restoration failures. Asterisk itself need not restart.

To stop the controller, without a forced unload:

```sh
sudo asterisk -rx 'module unload app_rpt_advanced.so'
```

Confirm that its radio channels have closed before restoring the previous
controller. To roll back a module upgrade, unload it, restore the saved module
and configuration, and load it again. Restore the previous `modules.conf`
selection if activation was made persistent. Do not overwrite a loaded module
or use a forced unload as a substitute for orderly shutdown.
