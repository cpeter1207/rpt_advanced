# ADR 0036: Asterisk integration without an ASL3 dependency

Status: Accepted

## Context

Asterisk and ASL3 are different dependency boundaries. The temporary Asterisk
module form of rpt_advanced may use Asterisk, but implementing AllStarLink
interoperability does not require depending on ASL3's repeater implementation,
resource modules, private APIs, or distribution services.

## Decision

`app_rpt_advanced` may depend on supported public Asterisk APIs and ordinary
Asterisk channel/codec services. It must not depend on ASL3-specific headers,
symbols, patched APIs, modules, or required helper services. Its controller
core and portable radio components remain Asterisk-free under ADR 0005.

Minimize new Asterisk dependencies as well. Before introducing an Asterisk
service or type, prefer an existing project component or compatible independent
library. Any unavoidable Asterisk integration stays in a thin integration adapter;
it must not become a controller, DSP, hardware, or shared-library contract.
Retiring that adapter must not require redesigning the standalone controller.

rpt_advanced owns logical execution, scheduling, and lifecycle independently
of Asterisk's thread model. ADR 0038 supplies a replaceable control-path
adapter, currently backed by Asterisk's taskprocessor. Only that adapter owns
its taskprocessor handles and backend-specific thread handling; the controller
depends on the neutral execution contract. Other workers and native media
remain independent of Asterisk execution services. Do not build another
taskprocessor merely to remove the current backend.

An Asterisk callback may enter through an Asterisk-owned thread. If a specific
Asterisk operation requires thread registration, serialization, or a particular
execution context, the thin adapter supplies only that operation's required
handling. Document and test the concrete API requirement rather than assuming
that every Asterisk call needs its own taskprocessor. Such handling must not
determine the controller's thread topology or enter a native audio callback.
Module unload stops admissions and quiesces adapter calls before code or
runtime state is released, preserving ADR 0026's lifetime rules.

The hardware appliance depends on neither Asterisk nor ASL3: no build, package,
runtime, helper-service, configuration, or hardware-control requirement may
pull either into the appliance. It uses the standalone controller and the
independent radio/audio/GPIO adapters directly.

Minimize ASL3 dependencies and isolate any unavoidable ASL3-specific
compatibility code in the single ASL3 adapter defined by ADR 0028. That adapter
is not a required dependency of rpt_advanced or its shared libraries. Do not
move ASL3 types or helper calls into a common header or shared library merely
to make them accessible from both sides.

The project-owned `RadioPlusAdvanced` contract uses ordinary Asterisk channel
reservation/control APIs with direct 48 kHz PCM callbacks. It is not an ASL3
hardware API. Its provider must not make rpt_advanced transitively require
`res_usbradio` after the hardware-adapter cutover. Audio and GPIO adapters provide
device services independently of ASL3; direct hardware ownership does not belong
in the ASL3 compatibility code.

Direct attachment uses `URP_AST_OPTION_DIRECT_CALLBACKS` with `block=0` before
`ast_call`. ABI 2 appends a writable `uint32_t accepted_abi_version` to the
descriptor. The caller initializes it to zero; the provider writes 2 only after
retaining both callbacks. The consumer requires both a zero option result and
acknowledgment 2, then starts the channel. An unknown-option success, missing or
different acknowledgment, or failed option rejects activation and synchronously
hangs up before callback storage may be reclaimed. The descriptor itself is
borrowed only during the option call; callback code and contexts remain live
through synchronous channel hangup.

This initial-alpha contract replaces ABI 1 without a compatibility path under
ADR 0040. The product and host-services tables are unchanged, so their ABI and
SONAME do not change. Deploy the consumer together with an ABI 2 USBRadioPlus
provider; alpha18 alone is not evidence of support. A minimum package-version
dependency must name the first actual ABI 2 provider release, not an invented
future version. Runtime acknowledgment rejects older mixed installations safely.

### Explicit peer link processing (2026-09-25)

Before starting either an incoming or outgoing peer's media owner, the product
binds that peer to its node's existing radio lease through host-services ABI 3.
The thin Asterisk adapter calls `ast_channel_setoption` on that exact
`RadioPlusAdvanced` channel with `URP_AST_OPTION_LINK_ATTACH` and `block=0`.
The option borrows the peer channel pointer for the synchronous call. Its ABI 1
payload starts with acknowledgment zero, and the consumer requires both result
zero and acknowledgment one. Failure rejects peer admission before media starts.
This does not infer a profile from the node number or the first active radio.

USBRadioPlus owns the same configured per-peer link graph, channel datastore,
and reload/teardown lifecycle used by its existing link processor. A disabled
graph retains the explicit radio association so enabling it by reload works.
No DSP implementation, configuration parser, or Asterisk type enters the core;
the existing hardware callbacks perform no binding or control synchronization.

The required `peer_bind_radio` host operation changes only the host-services
table to ABI 3 (`rptadv.hst3`). Old host tables are rejected before any callback,
and old USBRadioPlus providers fail the option acknowledgment. Product and module
descriptor layouts and exported symbols remain unchanged, so their ABI revisions
and DSO SONAMEs are unchanged. Install the matching product and Asterisk adapter
together with a USBRadioPlus provider implementing this option; the first released
provider version will determine its package minimum. No guessed release minimum
or alpha compatibility path is introduced.

AllStarLink wire-protocol, directory, topology, and control interoperability
remain required features. Matching ASL3 behavior on those interfaces is not a
software dependency on ASL3. Do not remove that behavior to satisfy a linker
or package dependency check.

## Consequences

Audit imports, undefined symbols, module requirements, package dependencies,
and startup assumptions, not just source file names. Build and integration
validation must exercise rpt_advanced against supported ordinary Asterisk
headers/services without loading ASL3 resource modules. ASL3 compatibility
checks belong to the adapter; an ASL3-provided test image alone does not prove
independence from ASL3.

This clarifies the current migration boundary without adding another
controller, transport, codec implementation, or future compatibility layer.
The standalone application still has no Asterisk dependency.

## Implementation status — 2026-09-15

At acceptance, the C module directly used Asterisk's taskprocessor and thread
helper. The Rust migration has replaced that implementation: the metadata-only
C loader selects versioned descriptors; `librptadv_product.so.1` owns lifecycle,
workers, and the single embedded controller core. It has no Asterisk imports.
`librptadv_asterisk_adapter.so.1` supplies public host services through a C table,
and `librptadv_control_asterisk_adapter.so.1` alone owns taskprocessor resources.
The periodic trigger and radio/peer workers use Rust-owned threads. File and
speech providers receive paired child-reaper callbacks through the product's
host-services boundary rather than importing Asterisk themselves.

This closes the direct-taskprocessor migration gap without removing the
supported Asterisk backend or adding a standalone backend. It is an implemented
source boundary, not proof of every deployment environment: the ordinary
Asterisk/ASL3 independence checks and full platform/integration gate above remain
required. Current release and live-radio limits are tracked in
[implementation status](../../implementation-status.md).
