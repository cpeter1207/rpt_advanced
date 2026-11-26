# Runtime composition

`Runtime<A, C>` is the serialized application owner. `prepare_configuration`
stages an immutable `RuntimeConfiguration` resolved from registered JSON resources.
It prepares station media, command profiles, calendar owners and configured link
routes before publication. `adopt_configuration` checks the active revision and
generation and submits the candidate to the device handoff. Changed devices
quiesce with callback entry gated. Activation failure restores changed leases
in reverse order. Failed restoration leaves the affected station gated and observable.

`A` is unique TX-owned prepared adapter/link state. `C` is its control-owned
dispatcher/lifecycle state. Preparation must not start or redirect live users.
Use `adapter_control_mut(node, generation)` only after publication to start `C`;
the same method reaches retained old/removed generations for stop/drain. Once
exposed, `C` requires an explicit `detached(node, generation)` acknowledgment
before reclaim. No `Any`, adapter types, Asterisk calls, or private resampler
enter the core runtime.

Register each node's RX/TX handles once with `register_audio`. Their setup-time
Arc ownership is retained for the external callback lifetime. Acquiring or
releasing callback guards only touches fixed atomic hazard slots, never the Arc
count. A callback uses its guard for its entire call; paired shared-clock calls
use one paired guard. Missing generations mean adapter-safe silence/unkey.

An ordinary reload preserves the node host, hardware lease, link manager, peers,
retry intent, and matching schedule progress. The adapter redirects prepared
generation endpoints, stops/drains the old dispatcher, and acknowledges old
detachment. Both audio owners must adopt before reclamation. A second reload
rejects while any prior generation or removed node is retained. Stop gates work
and callbacks first; it never force-frees a stalled generation. Dropping a runtime
with incomplete external teardown deliberately retains resources.

`NativeMediaPreparer` is distinct from `DeviceHandoff`: Task 10 composes decoding/
synthesis plus the released resampling-ring capability to produce validated
48-kHz PCM. File failure falls back to speech, then Morse; unavailable identifier
and announcement sets are omitted. Courtesy tones are validated even when a
higher-priority file succeeds. Media/PCM destruction stays on control.

External dialing carries owned generation permits. Return slow answers through
`finish_connect` with a freshly captured clock. Reconnect reconciles current
windows before resuming retries. A returned `Detach` must stop/join the named
peer and call `peer_detached`; configured replacement attachment remains gated
until that acknowledgment. `take_effects` uses the same rule for reload removals.

The registered calendar tracks each event boundary and its macro on the station.
`advance_commands` offers eligible work, checks its generation and returns the
next domain effect with a receipt. The host settles that receipt after admission.
Full-status RF speech uses the direct-link report. `topology` supplies the
separate operator diagnostic.

Actual Asterisk channel/task/ticker registration, artifact/SONAME installation,
same-PID reload/unload, and the native platform coverage gate remain integration
work. No C runtime removal is justified solely by component tests.
