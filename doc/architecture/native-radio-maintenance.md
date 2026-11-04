# Shared native radio maintenance

The standalone host forwards typed maintenance to the native USBRadioPlus owner
already retained by `ActiveRadio`. It never opens another hardware or mixer
owner. The product's serialized control path borrows the current `RadioWorker`
lease; a sized optional host-services tail forwards status, the shared mutable
`UrpAstChannelCommand`, manual transmit intent, and manual cleanup. Command results
and provider errors cross this boundary unchanged. Active-configuration text is
the retained original source document, including comments and section presence.

Manual transmit and forced CTCSS are independent temporary intent. Clearing a
maintenance session clears that intent through the shared owner. The ordinary
controller callback retains its direct key and CTCSS intent. USBRadioPlus owns
the merge, physical observations, mixer scaling, EEPROM, and hardware-service
worker; RPT Advanced does not reproduce those semantics.

`DeviceHandoff::update` defaults to unsupported. The standalone implementation
uses the same native device and prepared mailbox for compatible changes. Its
optional host callback has three phases: `0` prepares, begins, and waits for both
callback acknowledgments; `1` commits after aggregate admission; `2` restores the
retained reverse after rejection. Status `1` selects ordinary handoff only in
phase zero. A failed stage also invokes phase two to release any partially
adopted preparation; abort is idempotent. Errors remain errors in every phase.
Polling uses the existing bounded
physical-control response deadline outside audio callbacks. An opaque prepared
wrapper is destroyed once after transfer; the shared mailbox retains the live
transaction. A failed restore stops the native owner to clear RF intent safely.

Processing diagnostics read existing shared counters. The native status-text
callback reports the hardware state's receive/transmit callback and failure
counts. Each prepared peer processor retains an independent SDK link-observer
lease before audio ownership begins. A weak node registry lets control readers
retain that immutable observer while audio owns the graph. The registry never
reads the mutable graph handle and adds no audio-path lock or counter. It reports
the retained prepared/active/retiring processor count and the SDK's processed,
bypassed, and failed block counts; missing or failing observation support is
explicitly reported as unavailable or failed. Processor retirement releases weak
membership and its observer after callback quiescence.

The descriptor additions are optional sized tails to the current product/host
ABIs. The standalone host retains a tail operation only after its complete field
is readable. Native SDK admission requires the complete current configuration,
maintenance, update and observer capabilities before constructing a typed
descriptor reference; truncated or incomplete artifacts fail before device
creation. The existing published prefixes and ABI versions retain their
current meaning. Shared dependencies remain released dynamic libraries; candidate
SDK staging is an isolated development check, not a deployment.

## Local maintenance transport

The standalone foreground owns `/run/rpt-advanced/radio-control.sock`. The
service creates its parent directory with mode 0700; the socket is mode 0600.
The server rejects an existing path instead of replacing it. It accepts one
maintenance connection at a time and uses bounded nonblocking I/O between
network polls. No socket operation runs in a radio callback.

Each frame is a four-byte big-endian length followed by UTF-8 JSON, limited to
65,536 bytes. Version 1 requests name an operation and, for radio operations, a
configured numeric node. Replies contain `version`, `ok`, and either `result` or
`error`. The shared tuner keeps one request outstanding on its persistent
connection. Operations are `nodes`, `configuration-path`, `status`,
`processing-stats`, `active-configuration`, `command`, `transmit`, and `reload`.
The server supplies its fixed configuration path; clients cannot select another
file or invoke shell commands.

Disconnect and shutdown clear that connection's temporary manual PTT, test tone,
CTCSS inhibit and subaudible overrides through serialized product control. They
do not clear ordinary controller intent. A tuner reload uses the same transaction
as SIGHUP and replies after admission; it retains its own manual intent until the
calibration finishes. An external SIGHUP ends the maintenance lease first.

The tuner executable is packaged independently of the Asterisk adapters. Its two
transports share configuration edit destinations, calibration, rollback and
cleanup. A valid rejected operation keeps the transport available for rollback;
malformed replies and transport failures close it and release the manual lease.
