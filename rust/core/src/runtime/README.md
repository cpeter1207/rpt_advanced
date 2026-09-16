# Runtime composition

`Runtime<A, C>` is the serialized application owner. `reload` resolves the whole
`ConfigDocument`, prepares controller media, command maps, event templates/macros,
and permanent/replacement routes before touching a published generation. Failed
preparation keeps the old revision. Changed devices quiesce with callback entry
gated; an activation failure restores every already-changed lease in reverse
order. Failed restoration leaves the affected node gated and observable.

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

For events, `next_event` snapshots the trigger minute, `queue_event` admits its
message exactly once, `event_command` validates the reservation before applying
its macro, and `complete_event` settles it. Queue rejection leaves it pending;
stale dispatches cannot synthesize or enqueue. Full-status RF speech remains the
short direct-link report; `topology` supplies the separate operator diagnostic.

Actual Asterisk channel/task/ticker registration, artifact/SONAME installation,
same-PID reload/unload, and the native platform coverage gate remain integration
work. No C runtime removal is justified solely by component tests.
