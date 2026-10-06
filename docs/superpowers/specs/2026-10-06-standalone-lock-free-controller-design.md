# Standalone lock-free controller design

## Goal and scope

Implement the open **Standalone lock-free controller** wishlist entry as a
standalone Rust service with no build-time, package, link-time, or runtime
dependency on Asterisk or ASL3. Reuse the existing Rust controller and portable
radio behavior rather than creating a second controller implementation.

The service supports current AllStarLink behavior over IAX2, including ASL's
HTTPS registration mechanism. It uses the existing 48 kHz native audio
contract and the selected PortAudio/ALSA adapter. Existing decisions in ADRs
0002, 0005, 0011, 0018, 0020--0022, 0025--0029, 0035--0038 remain binding.
In particular, peer ingress is bounded SPSC fan-in to one media owner per peer;
no standalone audio I/O or inter-thread handoff takes a mutex.

This change does not implement other open wishlist entries such as EchoLink,
remote-base operation, complete REST/WebSocket control interfaces, site I/O,
or voting. It does not remove or replace the ASL3 adapters.

## Product and adapter lifetimes

- `rpt-advanced` is the standalone, non-root systemd service. It composes the
  portable product/core with the standalone control backend, IAX2 transport,
  native audio, and other selected device dependencies through versioned
  dynamic-library contracts.
- `app_rpt_advanced.so` remains an optional, deprecated Asterisk adapter. It
  remains available during this work and can be removed in a later, separately
  approved change.
- `chan_usbradioplus.so` remains the maintained ASL3 USB-radio channel adapter.
  It must not become a dependency of the standalone service or constrain its
  development. USBRadioPlus remains independently installable without either
  the standalone controller or `app_rpt_advanced.so`.
- Shared production components use dynamic shared libraries and stable,
  versioned C-compatible descriptors at cross-language boundaries. Rust-only
  implementation crates may remain part of their owning shared library unless
  independently released. No consumer vendors or statically links a separately
  released component.
- The standalone package may be installed without the optional ASL3 adapters
  (`app_rpt_advanced.so` and `chan_usbradioplus.so`). It still declares the
  native audio/device adapter packages required by its selected composition;
  installing without those providers is not a supported runnable configuration.
- The `app_rpt_advanced` package may be installed with USBRadioPlus and its
  shared dependencies, without installing the standalone service. This
  preserves the existing ASL3 deployment while the deprecated adapter remains
  supported.

## Runtime composition

The standalone executable performs process setup only: load the configuration,
resolve a complete adapter manifest, initialize the product, and wait for
shutdown signals. Configuration, controller policy, RF behavior, and shared
radio processing remain in the reusable Rust product/core. The executable does
not call Asterisk APIs or implement controller policy.

The standalone host-services provider supplies the existing product boundary
with local-time, logging, radio, peer-dial/read/write, and directory services.
Each implementation is adapter-owned and dynamically linked. The PortAudio
adapter owns device enumeration and callback registration; the hardware/radio
adapter owns its documented control operations. IAX2 transport and HTTPS
registration are provided outside the controller core. Missing or
ABI-incompatible required providers fail startup rather than selecting a
reduced or hidden fallback composition.

Control tasks are serialized by one standalone control owner. Inter-thread
handoffs use bounded, preallocated, lock-free queues with single-producer,
single-consumer ownership. Peer packet producers use the topology already
specified in ADR 0037: separate bounded SPSC queues feed a fixed pool of media
owners; one stable owner per peer alone mutates its jitter, decoder, and inbound
PCM state. Queue-full admission rejects and counts new work. No thread is
created per peer, and a stalled producer cannot block unrelated ready queues.
Audio callbacks call only the native receive or transmit worker with bounded
preallocated data; they do not run control, network, disk, or configuration
work.

The service runs under a dedicated non-root user, is managed by systemd, reads
the established `rpt_advanced.conf`, and writes logs using the existing
standalone logging policy. Device selection uses stable identity and fails
closed on an absent or ambiguous device. Valid configuration reload replaces
runtime generations without requiring an Asterisk process or restart.

## Debian package combinations

The packaging graph must permit all of these supported cases:

1. Install the standalone service and required portable/native runtime
   libraries, without either ASL3 adapter or any Asterisk package.
2. Install USBRadioPlus and its required shared radio/audio libraries without
   the standalone service, `librptadv_product`, or `app_rpt_advanced.so`.
3. Install USBRadioPlus plus the optional `app_rpt_advanced.so` package and
   shared product/control dependencies, without the standalone service.
4. Install the standalone service and USBRadioPlus together without installing
   either ASL3 adapter; their shared libraries remain dynamically linked and
   version-compatible.

Package dependencies reflect actual ELF and adapter-manifest dependencies.
The standalone package has no `${asterisk:Depends}` or dependency on
ASL-specific modules; the USBRadioPlus package does not depend on any
rpt_advanced product package. The optional app adapter declares its explicit
USBRadioPlus/channel and shared-library dependencies. Development packages are
separate from runtime packages where headers/linker names are published.

## Acceptance criteria

1. The standalone target builds from the project sources without Asterisk
   headers, libraries, linker flags, or helper executables.
2. Its executable and required libraries have no dynamic Asterisk or ASL3
   dependency; package dependency inspection confirms the same.
3. Standalone startup, configured local radio, incoming/outgoing IAX2 media,
   DTMF controls currently in scope, link status/topology behavior, shutdown,
   and reload are exercised without an Asterisk process.
4. Tests prove bounded SPSC ingress, single-owner peer state, rejection on full
   queues, per-producer ordering, fairness with a stalled producer, and no
   locks or blocking operations on native audio callbacks.
5. Tests install and inspect each package combination above, including dynamic
   ABI resolution and absence of unrelated product dependencies.
6. Debian 13 amd64 and arm64 build/test/install checks pass; production line
   and branch coverage is 100% on Debian 13 amd64 under repository policy.
7. The existing ASL3 adapter remains buildable and its integration behavior is
   unchanged. USBRadioPlus installs and functions without the standalone
   service.

## Assumption for review

“Install the standalone controller without the adapters” means without the
optional ASL3 adapters (`app_rpt_advanced.so`, `chan_usbradioplus.so`). A
functional standalone node still installs its selected PortAudio/ALSA and
radio-control adapter packages. If “adapters” was intended to include those
native hardware providers, that would conflict with the approved PortAudio
hardware path and needs correction before planning.
