# ADR 0005: Standalone core and optional Asterisk adapters

Status: Accepted

## Context

rpt_advanced must initially interoperate with ASL3 while also becoming a small,
fast, standalone radio controller with no Asterisk dependency. USBRadioPlus is
currently an Asterisk channel driver, but standalone operation needs direct
audio I/O and an Asterisk-free radio interface.

## Decision

Keep the controller core, I/O boundaries, and inter-thread communication
lock-free and independent of Asterisk. Build standalone rpt_advanced and
USBRadioPlus as dynamically linked shared objects with a lockstep ABI.
Standalone operation uses PortAudio through its versioned external-dependency
adapter for audio I/O. Asterisk/ASL3 support is provided by thin optional
adapters over that shared core and may be retired without changing standalone
behavior. Standalone operation is the long-term replacement for both the
rpt_advanced Asterisk module and USBRadioPlus's Asterisk dependency.

The module's Asterisk dependency is not an ASL3 dependency. ADR 0036 confines
ASL3-specific code to its optional compatibility adapter; `app_rpt_advanced`
uses public Asterisk APIs and independent radio services.
Its core execution and lifecycle contracts do not depend on Asterisk's thread
model. The replaceable control-path adapter uses Asterisk's taskprocessor in
the current module and a non-Asterisk backend in standalone (ADR 0038). Only
backend/API-specific thread handling stays in adapters under ADR 0036.

The RPT Advanced appliance is one selected standalone audio implementation. It
uses one through four direct-SAI, synchronous codec radio-port modules on a
common carrier clock; it is not a literal CM119 design. The same portable
radio-core contract continues to support separately attached CM119 devices
where they are appropriate. Appliance radio ports are selected by their stable
carrier-port identities, while attached CM119 devices use stable Linux device
identities.

`librptadvradio` is the separately versioned Debian-packaged home for radio
behavior shared by USBRadioPlus and rpt_advanced. It does not depend
on OSS or PortAudio. Radio adapters own CM119 mixer, GPIO, PTT, COR, EEPROM,
and device-selection I/O through a platform-neutral callback contract.
`app_rpt_advanced` links `librptadvradio` and the PortAudio adapter, which alone
links PortAudio and ALSA.

The shared core, PCM rings, and adapter contracts use canonical normalized
`f32` PCM. Asterisk conversion remains at the Asterisk adapter boundary.
The PortAudio adapter uses a `paFloat32` callback so that its core-side PCM
handoff needs no sample-format conversion; PortAudio/ALSA owns conversion to
or from the selected hardware format. ADR 0029 defines this format rule and
the signed-16 shared-ring migration.

The direct PortAudio/ALSA CM119 adapter coexists as a selectable per-node path
with Asterisk adapters. Configuration rejects concurrent ownership of one
device. Each configured radio owns one logical full-duplex 48 kHz stream under
[ADR 0035](0035-fixed-48khz-native-audio.md), using the device's `defaultLowInputLatency` and
`defaultLowOutputLatency`. Independently clocked capture and playback use
separate PortAudio callbacks. Under the accepted 2026-09-13 amendment in ADR
0027, capture calls the local receive worker, which performs receive DSP and
writes processed PCM to the local receive inbound ring. Playback calls the
transmit worker, which mixes native-rate output from that ring, linked-peer
inbound rings, and the telemetry playout ring, then adds profile-selected
CTCSS/DCS and fills PortAudio's output buffer directly. Inbound rings alone
own source-rate and drift correction. Neither callback performs controller,
configuration, or blocking work; their frame counts are independently bounded.
The current USBRadioPlus migration implements the independently paced callback
entry points. The local/link/telemetry inbound-ring topology and generational
station-host lifecycle described here remain pending and were not part of
released alpha18.

Where the adapter knows capture and playback share a clock and can present
aligned frames, it may invoke receive then transmit in the same callback.
ADR 0027's shared-clock mode retains the local ring as unity-rate pass-through
with no adaptive correction and a target reserve equal only to configured
squelch delay. Unknown clock relationships keep separate asynchronous callbacks
and ring recovery.

One ASL3 compatibility implementation remains over the same shared core after
the ADR 0028 cutover. It owns Asterisk/`f32` PCM conversion and fixed
Asterisk-frame assembly. The audio adapter owns hardware callback I/O; neither
boundary makes either shared radio worker depend on Asterisk frame size or APIs.

ADR 0039 retires USBRadioPlus's native software-repeat and native parrot modes
from future ASL3 support. rpt_advanced requires neither mode. This does not
remove the shared native DSP, hardware adapters, or the separate controller
transport; ordinary app_rpt compatibility remains outside those retired modes.

ADR 0028 defines the staged replacement of the current `res_usbradio`
hardware ownership with the shared audio, radio-control, and GPIO adapters.
That extraction must preserve compatibility behavior while leaving
`librptadvradio` and standalone operation independent of Asterisk.
Every supported CM119 path uses the PortAudio/ALSA audio adapter; no OSS
fallback remains after the migration.

Standalone IAX2 interoperability targets current ASL3 behavior, including
ASL's HTTPS registration mechanism rather than retired IAX registration. NAT
shortcomings beyond ASL3 parity are out of scope. Initial autopatch, reverse
patch, and paging use an external IAX or SIP connection; a native lock-free
IAX2 telephony replacement remains a future architectural direction.

The standalone service runs under its own non-root account under `systemd` and
is upgraded by Debian packages through `apt`. USBRadioPlus and rpt_advanced
packages use exact matching versions for their lockstep ABI. Radio devices use
stable Linux identities and map one-to-one to nodes; an ambiguous or missing
device fails closed. Logging uses `/var/log` with rotation when resources
permit, or an external logging service otherwise. SSH/Linux accounts remain
separate from REST/WebSocket administrative identities.

## Consequences

No Asterisk type, lock, callback, or lifecycle rule may enter the shared core.
The Debian packages for rpt_advanced and USBRadioPlus must use exact matching
versions. IAX2 and ASL HTTP-registration support belong to standalone
rpt_advanced; adapter code remains limited to translation at the boundary.
Appliance ingress, OIDC, TLS, WAF, and trusted-proxy policy are defined by
ADR 0006.
