# Asterisk control executor

This C `cdylib` owns only the public Asterisk taskprocessor boundary and has no
production dependency on core. The `rlib` target supports Rust tests, not static
production linkage. Its ELF SONAME is `librptadv_control_asterisk_adapter.so.1`.
`rptadv_control_descriptor_v1` returns capability `rptadv.control`,
exact ABI version 1, its byte size, and open/submit/stop-and-drain/close functions.
The selected product must validate the readable version/size prefix, identity and
every required operation before opening it. Use one executor at the existing
application scope, not one per node or peer. Opening prefixes the configured name
with Asterisk's process-wide sequence so separate lifetimes never share a worker.
No external taskprocessor reference may be acquired for that private executor.

Accepted tasks run once in FIFO acceptance order. The pending-work bound counts
queued and running tasks. Submission returns 0 for acceptance, 1 for stopped
admission, 2 for a full queue, or 3 for an Asterisk enqueue failure. Only 0
transfers the opaque payload. On rejection the original caller still owns it;
there is no inline fallback.

Stop gates admission and waits for all accepted callback invocations. A drain
from its own taskprocessor returns -1 rather than deadlocking. Close also
returns -1 without consuming the handle in that case; the external lifecycle
owner must retry after the callback returns. Gate external producers before
closing a handle. No callback, task payload, or adapter code may be unloaded
before close completes. Drain finishes payload execution/release; the callback
may still be returning inside provider code. Successful close performs the final
taskprocessor unreference, which joins the unique default worker, and is the
code-unload barrier. The loader must retain the provider DSO through close.

Node policy, event trigger timestamps, generation invalidation, DTMF loss
handling, and external dial-result revalidation remain in `rpt-advanced-core`.
No audio path calls this capability. The Rust client implements the neutral
`ControlExecutor` port through this C-compatible table, keeping Rust task
layouts inside the client.

The parent migration still has to install the versioned SONAME/package and wire
the application entry adapter to this table before removing its C taskprocessor
calls. The crate tests exercise ABI ownership with a deterministic public-API
fixture; real Asterisk load/reload/unload remains an integration gate.
