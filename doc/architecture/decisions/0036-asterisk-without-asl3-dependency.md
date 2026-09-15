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
and frame APIs at 48 kHz. It is not an ASL3 hardware API. Its provider must not
make rpt_advanced transitively require `res_usbradio` after the hardware-adapter
cutover. Audio and GPIO adapters provide device services independently of
ASL3; direct hardware ownership does not belong in the ASL3 compatibility code.

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

The current C module dispatches scheduler, link-event, and DTMF work directly
through an Asterisk taskprocessor and starts its ticker with an Asterisk thread
helper. Isolating backend execution behind ADR 0038's adapter is the migration
gap; the Asterisk backend itself remains supported. Existing POSIX radio and
peer workers already demonstrate independent media ownership. Execution
contract tests cover ordering, reload, invalidation, and unload for every
backend, plus actual Asterisk integration for the current backend.
