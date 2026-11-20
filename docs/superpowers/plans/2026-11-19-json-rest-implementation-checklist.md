# Instructions
Use the following checklist to build and test these changes to rpt_advanced on radio.onionring.rocks.
Clean up temperary files as you go.
Use the test images already developed for rpt_advanced and its dependencies and update dependencies in the test images as needed.
Use off the shelf rust krates or libraries where ever possible.
Focus on reusing existing code and dependencies first. Only add new code or dependencies when absolutely necessary.
When working in allstarlink, echolink, or sip, insure that the connections stay live as much as possible.
Make sure local repeat and phone  audio is not lost when connecting or disconnecting links.
Make sure normal jitter adjustments and packet loss concealment do not take down any connections.
Review and update the agent rules, wishlist, and architecture documentation as you implement.
Update this checklist each time an item is complete.
Commit and push regularly.
Build and test major milestones on node524950.local before continuing. Per the 2026-11-20 instruction, execute milestones continuously until the implementation/runtime cutover is complete, then pause; do not change the installed service without explicit deployment authorization.
Do not invent requirements or edge cases.
Consolidate as many error conditions as you can to avoid unnecessary branching.
Log warnings or errors when errors occur. Log warnings when an error isn't fatal. Log errors when something fatal occurs that takes down a connection or the entire controller.
Use test-driven development.

# # Implementation Checklist
[x] - Use the [design](../specs/2026-11-18-json-rest-configuration-design.md), [schema](../../../repeater-management.schema.json), command inventory and DTMF inventory as the implementation contract; reconcile their remaining inconsistencies first.

[] - Record compatible controller, USBRadioPlus, schedule-provider and other provider revisions; pin dependencies and minimum supported ABI/toolchain versions.

[x] - Map changes to the existing core configuration/runtime, product lifecycle, standalone host, Asterisk host, provider adapters, CLI, tuner, packaging and documentation owners.

[x] - Complete the initial shared JSON decoder and sparse site/port metadata/inheritance foundation, preserve existing wire input types, and pass development-host checks plus native node524950 configuration tests.

[x] - Complete the UUID/key/owner registry and modular candidate assembly milestone, including explicit protocol presence, empty ordinary overrides and native node verification.

[x] - Complete bounded read-only capture of registered working files under an edit lock, selected-path fingerprints/rechecks and product candidate assembly; pass host and native node checks. Complete discovery and durable publication remain pending.

[x] - Complete guarded immutable public-object/manifest installation with file and directory sync, no-replace publication, exact-byte retries and native node verification. Accepted-state/journal publication remains pending.

[x] - Complete typed reference-only public manifests, protected hash-verified immutable graph readback and borrowed candidate assembly; pass host/native checks. Accepted-state selection, private graphs and activation remain pending.

[x] - Complete bounded sealed private-record crypto/codec and env-only systemd key loading with opaque zeroizing buffers, independent interoperability evidence and host/native checks. Account payloads, encrypted file publication and private graph adoption remain pending.

[x] - Complete typed prepared encrypted-object installation and authenticated readback under the edit lock, including exact retry/durability behavior, bounded reads and native verification. Account binding, private manifest adoption and accepted-state selection remain pending.

[x] - Record approved scheduler policies: pair a start with its next end and skip overlapping starts; ignore the in-progress occurrence when restart_recovery is false; skip missed calendar work while forcing pending monotonic deadlines; cancel without either boundary macro if nominal end arrives before start execution. Preserve existing permanent-route ownership and hold fences.

[] - Define separate public, writable, effective and status resource types; retire the aggregate operation envelope as the new persistence/API format.

[] - Split controller configuration into site metadata/defaults, port metadata, radio/audio/signaling/interface sections, protocol instances, events, macros, messages, mailboxes and equipment resources.

[] - Assign stable resource UUIDs and exact local keys; preserve friendly names without treating either as network node numbers or unsafe filesystem paths.

[] - Implement built-in → site → port inheritance with explicit inherit_from; keep identity/display metadata local while enabled, callsign and language inherit.

[] - Implement recursive named-map merging, whole-array/scalar replacement, local-override removal, action/source discriminator cleanup and complete mailbox identity shadowing.

[] - Validate every affected descendant after site changes, including references, effective required fields, protocol ownership and physical-device claims.

[] - Keep protocol attachment explicit: site defaults must not create AllStarLink, EchoLink or phone-patch accounts or sessions.

[] - Separate EchoLink from AllStarLink and move protocol-neutral peer DSP into its own resource; support no-protocol and EchoLink-only ports.

[] - Replace controller ConfigDocument/INI parsing with syntax-checked resource-local JSON decoding and typed effective settings; log warnings and ignore unknown keys and unknown/invalid/unsupported setting values, retaining inherited/default values.

[] - Remove controller-side INI reconstruction and original-document forwarding, including processing_configuration-based configuration comparisons.

[] - Reuse already typed USBRadioPlus native fields; add a size/version-checked typed preparation extension for remaining DSP/profile/remote settings currently carried as INI text.

[] - Share USBRadioPlus resolved-setting builders between its unchanged public INI loader and the typed controller interface; preserve its existing INI files, grammar and older consumers.

[] - Represent all provider-consumed hardware, RX/TX/voice/telemetry/link DSP, remote backend/options, memory and scan settings without silently dropping fields or profile-presence semantics.

[] - Preserve arbitrary valid controller keys and provider strings across the typed boundary; test names, significant whitespace, semicolons and hashes without inventing unsupported INI escaping.

[] - Replace product configuration inspection, radio preparation, remote-command preparation and peer-processing paths that currently reparse controller INI.

[] - Update standalone native configuration, processing, radio activation/host and provider bindings to own prepared typed requests with correct lifetimes and rollback.

[] - Update product headers/bindings and provider dependency checks; reject missing/incompatible preparation extensions before activation.

[] - Switch standalone and Asterisk startup/check/reload paths to the modular JSON store; reject controller INI without a fallback or compatibility reader.

[] - Remove controller iax-secrets/private INI file loading and route those credentials through the encrypted store.

[] - Audit auxiliary controller-owned configuration readers and migrate any remaining INI definitions to JSON; preserve explicitly external protocol-data and USBRadioPlus formats.

[] - Define a JSON static-node table/resource and replace static_directory_file INI loading in standalone and Asterisk; preserve static-first resolution and source authentication.

[] - Keep downloaded AllStarLink directory-feed decoding separate from controller configuration; retain fallback behavior through a typed resolver/provider boundary.

[] - Test malformed/duplicate static identities, addresses, source checks and missing/static/external directory fallback with the new JSON data.

[] - Replace installed rpt_advanced.conf examples, service conditions, command-line defaults and package paths with the JSON working-tree/store layout.

[] - Document administrator-led conversion of existing controller settings before upgrade; do not retain an INI importer in the deployed controller.

[] - Replace controller INI fixtures with JSON and test old INI rejection; separately retain and run USBRadioPlus public INI regression tests.

[] - Verify JSON preparation preserves radio/DSP/remote behavior, profile inheritance, meaningful reload comparisons, ABI rejection, rollback and Asterisk reload/unload.

[] - Implement service-owned storage directories/files with the documented ownership and permissions; keep editable working files separate from accepted immutable objects.

[] - Implement immutable public objects, encrypted private objects, manifests, publication journals and the small versioned state pointer.

[] - Keep logical configuration revisions, immutable object versions and runtime generations distinct; preserve stable identities across ordinary edits.

[] - Securely capture bounded working paths using resource identities; reject path traversal, symlink escapes, duplicate IDs and malformed/dangling store references.

[] - Implement configuration/edit locking, bounded requests and one outstanding activation; allow management reads while writes or activation are pending.

[] - Stage and validate every public/private change, then prepare the runtime transition before selecting new accepted state.

[] - Durably sync changed objects, manifest and journal before atomically replacing and syncing state.json as the sole cross-resource commit point.

[] - Publish working files using recorded source/target hashes; reconcile interruptions without overwriting external edits.

[] - Track desired, active and last-known-good revisions separately; promote last-known-good only after confirmed owner adoption.

[] - Implement explicit apply/reload/restore recovery with affected transmitters inhibited after failed/interrupted activation; preserve corrupt or unsupported state.

[] - Keep authenticated management usable with zero ports, no active radio, missing protocol credentials or a locked private store.

[] - Implement reachability-based garbage collection and manifest-plus-object backup/restore, including pending journals and separately retained encryption keys.

[] - Fault-inject object, manifest, journal, pointer, directory-sync, publication, activation and rollback failures; verify exact saved/active/recovery outcomes after restart.

[] - Implement explicit reload of hand-edited JSON using accepted revision, a bounded tree fingerprint and the cooperative edit lock; add no automatic file watcher.

[] - Preserve UUIDs while importing JSON working-tree edits; apply ordinary create/delete/reference checks to new or removed resources.

[] - Treat missing ordinary section files as empty overrides and missing optional protocol files as absent instances.

[] - Preserve invalid/concurrent edited bytes and active state; expose external_edit/fingerprint diagnostics and block conflicting writes or key maintenance.

[] - Implement shared account-bound credential writes for AllStarLink, EchoLink, proxy and phone patch; expose only safe configured-state metadata.

[] - Implement omission-preserve, nonempty-replace and explicit clear/mask semantics; never reuse a secret after its account/endpoint binding changes.

[x] - Encrypt recoverable records with XChaCha20-Poly1305, a fresh 24-byte nonce/version and the documented format/purpose/store/credential/version bindings; exclude logical revision and desired/last-known-good slots.

[x] - Read the separate 32-byte key only from `$CREDENTIALS_DIRECTORY/rpt-advanced.aead.<key_id>` with the documented validated key ID; keep it out of controller JSON, arguments, HTTP, logs and proxy/CLI credentials.

[] - Implement locked-store startup, stopped-writer key rotation and retained-key recovery without inventing replacement keys or false configured flags.

[] - Keep mailbox/admin Argon2id hashes private; preserve same-scope omission, complete mailbox shadowing and valid identity requirements.

[] - Implement neutral secret write slots and explicit add/replace/remove tracking; keep real secrets out of public staging documents.

[] - Reject secret or ancestor reads/tests/copy/move, including copy/move into secret slots; preserve records when ordinary parent replacement merely omits them.

[] - Test credential replacement, masks, inheritance, account changes, deletion/recreation, nonce freshness, ciphertext bindings and redaction in GET, errors, logs and receipts.

[] - Replace the local Unix maintenance socket and framing with a host-owned loopback HTTPS listener using the shared coordinator.

[] - Bind only the intended loopback addresses and require authorized mutual TLS on every route, including health, schemas, reads, writes and recovery operations.

[] - Provision distinct server, nginx, Apache and CLI identities, a management-client issuer and exact permitted-identity allowlist outside radio configuration.

[] - Validate chain, purpose, expiry and current identity authorization on new, kept and resumed connections; disable plaintext/anonymous fallback and TLS early data.

[] - Provision the dedicated Unix administration group, protected CLI key and reload-helper access; keep proxy keys separate and restricted.

[] - Configure nginx/Apache backend certificate and hostname verification, preserved request IDs/preconditions, disabled API caching and safe handling of credential bodies.

[] - Apply frontend user/path/method, CSRF, Host/Origin, CORS and JSON-media policies; strip unused forwarded identity headers and disable method overrides.

[] - Test authorized/unauthorized/expired/removed identities, server-name/CA errors, retained/resumed sessions, Unix group access and both proxy configurations.

[] - Implement HAL navigation, stable relation cardinalities, per-document CURIEs and self/up/profile/child/effective/status/change links with origin-relative URLs.

[] - Implement GET everywhere, PUT/PATCH on writable resources, POST for creatable collections, DELETE wherever POST exists and GET-only status/effective/discovery views.

[] - Implement independent zero-or-one protocol collections and named event/macro/message/equipment collections with stable member identities.

[] - Implement complete collection reference inventories separately from paginated summaries; reject invented IDs, duplicates and ownership transfers.

[] - Implement atomic member/collection retirement, dangling-reference checks, credential removal and safe owner shutdown; cleared collection endpoints remain discoverable.

[] - Implement resource-local PUT replacement and RFC6902 PATCH using RFC6901 escaping, standard array rules and all six operations.

[] - Reject duplicate JSON keys, cross-resource addressing, writes to server-owned fields, missing-parent fabrication and inherited-array index editing.

[] - Apply the full patch before discriminator cleanup, inheritance and final validation; expose no intermediate or partially accepted state.

[] - Implement strong target/view-specific ETags and If-Match, including credential-only changes; return the documented error/status codes and headers.

[] - Publish writable/read-only schemas, relation documentation and code-derived OpenAPI; reject obsolete merge-patch and Unix-socket request formats.

[] - Test method matrices, discovery links, empty/optional resources, collection lifecycle, escaped keys, patch ordering, stale validators and failed atomic mutations.

[] - Implement runtime-operation drafts with boot/request UUIDs, GET, conditional PUT and PATCH submission; freeze submitted intent before dispatch.

[] - Persist/deduplicate operation admission and dispatch; never replay indeterminate effects, silently evict identities or accept writes to an old boot identifier.

[] - Separate desired-state apply/reload/restore admission from active-revision domain commands; support recovery operations without an active radio.

[] - Implement bounded exclusive maintenance leases and cleanup on cancel, expiry, connection/owner loss and generation change.

[] - Normalize configuration/status/maintenance responses to sanitized public JSON; do not return old raw controller/provider configuration or credentials.

[] - Map every radio/audio/signaling/interface/control/limits/power/memory/scan field into prepared settings; implement site sensor references to typed interface properties, including GPIO/parallel pins.

[] - Implement duplex_mode in place of full_duplex/radio_duplex_mode, remove redundant radio_channel, and add independent RX/TX audio enables, enabled squelch/VAD/CTCSS/DCS qualifiers and separate polarity inversion.

[] - Implement enableable RX deemphasis, TX processing, fixed three-band dynamics and final peak limiter while preserving generated DSP graphs.

[] - Remove controller VOX and public raw audio.graph controls; warn and fall back for unsupported qualifier values, rejecting only when no safe deterministic combination remains.

[] - Implement generic sound-chip GPIO, parallel, serial modem-line and key_down sources with domain-specific pin definitions and scancodes.

[] - Enforce single GPIO pin for squelch/PTT, supported serial pins, flow-control exclusions, shared-device ownership and safe control-line release.

[] - Implement exact checked MHz/native-unit conversions, derived TX frequencies, RF limits and capability-declared settings/actions.

[] - Implement the specified memory array/offset/CTCSS/mode shape, startup memory and scan controls, including one-based selectors and priority membership.

[] - Apply the shared jitter object through AllStarLink, EchoLink and phone adapters; validate inherited ordering, transport capabilities and latency budgets.

[] - Finish independent protocol registration/session activation, retirement, status and failure rollback with write-only credentials.

[] - Finish phone provider admission, authentication, media attachment/routing, bounded buffers, private replies, isolation and coherent duplex/keying.

[] - Remove phone tx/keying_mode, duplicate lists/call-limit settings and check_deadline_ms; use shared VAD/jitter and common DTMF inactivity.

[] - Implement phone VAD context defaults and disabled-VAD continuity; retain other enabled qualifiers and shared kerchunk fallback.

[] - Verify VAD-disabled healthy phone/SSB reception is continuous while unmuted and does not bypass independent qualifiers or TX safety.

[] - Implement one shared typed command dispatcher across runtime, macros and DTMF input projections, covering the full audited catalog and new RX-mute targets.

[] - Preserve action/source/capability/owner restrictions, private replies and unavailable phone foundations; schema acceptance must not create authority or a live source.

[] - Expose generic parameter text on every action; interpret and validate it in the selected handler rather than globally as a node ID.

[] - Implement typed mappings with command/protected plus shared action fields; remove macro trigger/protected metadata and use call_macro aliases.

[] - Implement mapping inheritance and final action/operation/target discriminator cleanup without retaining stale action-specific fields.

[] - Install all 77 source-derived DTMF bindings plus phone-only *99; preserve radio aliases/fallback, disabled codes and permanent-only 806/816 semantics.

[] - Preserve exact fixed-radio spellings, variable parameter decoders, leading-zero/priority-zero behavior and operation-specific radio access policy.

[] - Replace global-star initiation and eager exact dispatch with whole-candidate, longest-match dispatch at eligible completion boundaries.

[] - Implement one literal terminator, default #, eligible real unkey and configurable inactivity_timeout_ms, default 3000, beside mappings.

[] - Replace hardcoded timeout and synthetic-# completion paths across collectors, RF publishers, peer sessions and phone with explicit reasons and per-source monotonic deadlines.

[] - Implement deadline rearm/invalidation, consume-before-dispatch, source-loss cancellation and whole-candidate overflow discard; preserve selected-remote/peer-unkey/pad-test scope.

[] - Pass every unmatched tail unchanged into parameter; silently discard noncommands and leave missing/invalid parameter handling and replies to handlers.

[] - Implement mapping lock-code checks and grant rechecks after waits; prohibit persisted credentials and fabricated RF/private-call/host context.

[] - Implement ordered macro execution with inline call_macro, static cycle rejection, shared depth/action/symbol/duration budgets and generation cancellation.

[] - Implement isolated execute_dtmf collectors with explicit/inactivity completion within budget, no injected delimiter/unkey and no tail sharing.

[] - Test every command family, aliases, protection, decoder bounds, custom terminators, unkey/inactivity races, overflow/loss, nested calls, cancellation and no repeated effects.

[] - Implement rx_mute_toggle for the owning port, one exact live peer, all connected network peers, a link-type-filtered peer set and the actual phone caller.

[] - Implement mixed/all-muted/empty group rules, unique or exact caller selection, live-generation validation and captured desired states for retry-safe toggles.

[] - Keep mute bits runtime-only, preserve surviving live identities across reload and reset new/reconnected streams without retained mute intent.

[] - Gate both source PCM and receive activity/keying before mix/repeat/parrot/fanout; drain muted input and discard queued muted speech.

[] - Preserve raw qualifiers, actual unkey, transport processing and DTMF while muted; retain provenance to suppress muted-source regenerated tones without blocking commands.

[] - Publish mute/activity/type/call status and test every target/filter, stale identity, group state, muted *99 unmute and independent-source/TX behavior.

[] - Replace point/window scheduler types with flat events containing required start, optional end, independent warning macros and no configuration wrapper.

[] - Add the separate size/version-checked hron recurrence interface while preserving released schedule ABI1/window layouts for existing consumers.

[] - Pin hron 2.2.1/provider dependencies and raise provider builds to Rust 1.93 or later; test the consumer MSRV independently.

[] - Implement owned compiled recurrence handles, complete grammar validation, stable source strings, effective zones, equality, lifetimes, ranges and bounded diagnostics.

[] - Bind missing zones explicitly to host-local IANA identity; implement/prove the real-zone AND virtual-UTC-civil DST predicate and use it for previews.

[] - Carry one captured real/civil/monotonic tick context and deliberate instant occurrence keys; preserve semantic equality, reservations and unchanged-reload progress.

[] - Implement inclusive B…B+P minute opportunities, sufficient continuous inactivity, final forcing, admission-once suppression and unchanged deadlines.

[] - Define the existing node activity observation used for politeness and use UTC minute cadence with monotonic elapsed deadlines.

[] - Run controller_start once after safe activation, disallow end/restart_recovery and prevent replay on ordinary configuration reload.

[] - Implement active-window restart recovery, startup-before-recovery order, suppression of duplicate ordinary starts and expiry at nominal end.

[] - Implement same-instant sequence_number ordering and best-effort end-first fallback through politeness, backpressure and complete nested macro execution.

[] - Make exact-owned permanent connection setup idempotent; preserve genuine loop/ownership failures and existing network-outage retry.

[] - Support adjacent nets with no intermediate permanent restoration and a final end-macro call to the reusable permanent-links macro.

[] - Test scheduler boundary policies, P=0/1/2, DST/Lord Howe/Apia, current-minute startup, clock jumps, no catch-up, failures, queues, reload retention and stale effects.

[] - Project named JSON messages/locales into existing Fluent resources and the checked compositor; retain installed catalogs and per-form English fallback.

[] - Implement one-string structural media references separately from native text-only Fluent patterns; never reparse formatted output as commands/media.

[] - Implement approved flat runtime bindings and immutable attempt snapshots for identities, event data and cached sensor quality/values.

[] - Validate IDs, references, form compatibility, cycles and binding conflicts; reject required-English/structural failures and preserve optional-translation fallback.

[] - Retain 127-byte UTF-8 ordinary forms, checked compound limits, complete Morse, existing media fallback and queue/TX ownership.

[] - Implement local/peers/combined message routing with no local fallback for empty peer selections and no broadcast of private replies.

[] - Test translations, native references/selectors, media nesting rejection, context/quality failures, byte limits, routing and nested message actions.

[] - Update CLI, tuner, Asterisk composition and reload helpers to discover HAL resources, use mTLS, edit specific JSON resources and submit typed operations.

[] - Remove controller Unix-socket clients and INI read/write/save paths; keep USBRadioPlus-owned INI tooling and configuration behavior intact.

[] - Update configuration inspection, tuning persistence and status tools to report saved versus active JSON revisions and operation outcomes.

[] - Update systemd units, permissions, credentials, listener provisioning, package dependencies, installation defaults and clean/upgrade procedures.

[] - Update README, configuration/reference/API docs, ADRs, examples, wishlist and implementation-status records for every removed or replaced contract.

[] - Convert existing installations offline, verify effective JSON settings and private credential bindings, and back up old state before switching service startup.

[] - Run native component, ABI, provider, transport, scheduler, message, DSP and callback-isolation tests; JSON shape validation alone does not complete runtime work.

[] - Run real nginx/Apache/CLI integration, crash-recovery/fault-injection and clean install/upgrade tests on Debian 13 amd64 and arm64.

[] - Run formatting, lint/static analysis, the existing full project gate, package/service tests and regression checks for USBRadioPlus INI consumers.

[] - Perform a final coverage audit of every schema field, action/mapping, endpoint/method and lifecycle state; remove unreachable compatibility code and unused dependencies.

[] - Verify production rpt_advanced has no INI configuration reader, importer, original-source forwarding or installed legacy config path; allow only preserved external-provider formats.

[] - Publish compatible controller/provider releases together with the completed runtime test evidence, upgrade instructions and the updated checked-off implementation checklist.
