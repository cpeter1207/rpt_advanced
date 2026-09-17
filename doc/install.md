# Installation and activation

## Prerequisites

Supported targets are Debian 13 on amd64 and arm64, using ASL3 Asterisk and
matching public Asterisk development headers (provided through
`dh-sequence-asterisk`). Source builds need Rust 1.85, Cargo,
`libclang-dev`, `pkg-config`, `librate-adjusting-pcm-ring2-dev`, and
`librptadv-samplerate-adapter-dev` in addition to `build-essential`. USBRadioPlus
must provide the `RadioPlusAdvanced` channel technology with direct-callback
attachment ABI 2. Install its matching provider together with this consumer;
channel availability or alpha18's version alone does not prove support. An
unacknowledged attachment fails before media starts. No radio hardware is
required for the automated synthetic-radio tests.

Load the Asterisk codec modules required by the IAX peers you intend to use.
The local RadioPlusAdvanced exchange is always 48 kHz signed-linear PCM, so it
does not require `codec_resample.so`. rpt_advanced converts between a
negotiated peer PCM rate and that fixed local rate at the peer boundary.
`codec_resample.so` can still be needed by unrelated Asterisk channel or
dialplan paths. Installed but unloaded codec modules are unavailable to IAX
negotiation; use `core show translation` to inspect available paths.

FFmpeg prepares sound files before a radio worker starts. The independent speech
adapter reads Piper's WAV output directly and does not invoke FFmpeg.
For speech, install an offline Piper executable named `piper` in Asterisk's
service PATH and configure a local voice model. The service account must be able
to read the model, its companion JSON file, and all configured sound files.
Missing or failed file/speech preparation falls back to the configured Morse ID.
See [configuration](configuration.md) for the complete media hierarchy.

## Build and install

Install the Debian package when it is available:

```sh
sudo apt-get install ./rpt-advanced_*.deb
```

For a source build, from the source directory:

```sh
make -j2
sudo make prefix=/usr install
```

Cargo builds the Rust product; `make` orchestrates the conventional build and
installation. The installed module is
`/usr/lib/<Debian multiarch triplet>/asterisk/modules/app_rpt_advanced.so`.
Its versioned Rust adapter DSOs are installed privately under
`/usr/lib/<Debian multiarch triplet>/rpt_advanced/`:
`librptadv_asterisk_adapter.so.1`, `librptadv_product.so.1`,
`librptadv_file_adapter.so.1`, `librptadv_speech_adapter.so.1`, and
`librptadv_control_asterisk_adapter.so.1`. Set `asteriskmoddir` explicitly if
Asterisk uses another module directory. Use `DESTDIR` for staging; it prefixes
install destinations without changing runtime configuration paths. `make
install` does not edit `modules.conf`, `rpt.conf`, or any active configuration.

`make dist` creates a source tarball under `build/`. Extract it on a machine
with the prerequisites above and run the same build/install commands there;
the archive does not require Git or the Asterisk source tree. `make distcheck`
builds and stages installation from the extracted archive. This packaging check
is part of the required platform gate. No compiled module or voice model is
included in the source archive.

The runtime package provides no static controller archive or legacy controller
headers. It installs the module, required versioned product and adapter DSOs, license under
`share/doc/rpt-advanced/copyright`, and the disabled example under
`share/doc/rpt-advanced/examples/`. The product, file, speech, and control
development packages separately provide their public C headers and unversioned linker names.

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
No dialplan application call is required for the local radio: enabled nodes
start when the module loads. For persistent loading, configure `modules.conf`
to load USBRadioPlus before `app_rpt_advanced.so`; avoid a conflicting `noload`
entry.
The module declares USBRadioPlus as an optional ordering dependency so Asterisk
starts the configured driver first. Enabled nodes still require the adapter;
an entirely disabled configuration can load without a radio driver.

## AllStarLink linking

The local radio and AllStarLink link paths have different activation needs.
Incoming IAX links require an Asterisk dialplan route to
`RptAdvanced(<local-node>)` and normal ASL IAX registration/peer configuration;
the module verifies the claimed caller through the configured node directory or
ASL lookup before it accepts the channel. See
[configuration](configuration.md) for the access lists, DTMF mappings, link
lifetime, status, and topology behavior.

Do not add that dialplan route or enable live linking from the current source
without explicit approval. The AllStarLink integration has not yet undergone
live interoperability testing with classic `app_rpt` and has not been deployed
to 524950. Use the isolated tests in [testing](testing.md) and the limitations
in [AllStarLink status](allstarlink-status.md) before any approved staging.

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
