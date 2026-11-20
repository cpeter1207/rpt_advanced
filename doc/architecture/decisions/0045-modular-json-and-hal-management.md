# ADR 0045: Modular JSON configuration and authenticated HAL management

Status: Accepted 2026-11-19; implementation in progress

## Context

The owner approved a modular JSON configuration contract, JSON Patch updates,
HAL discovery and authenticated local management, then authorized implementation
using the updated checklist. The old controller INI parser and Unix maintenance
socket cannot satisfy that contract. The separately released USBRadioPlus
product still needs its public INI configuration format.

## Decision

Use separate JSON resources and sparse built-in/site/port inheritance. Keep
resource UUIDs/local keys/names separate from network identities. Protocol
defaults do not create accounts or sessions; AllStarLink, EchoLink and phone
patch have independent optional instances. Named maps merge recursively,
arrays/scalars replace, and a local mailbox shadows a complete identity.

Preserve ADR 0019's warning/default policy for JSON: log warnings and ignore
unknown keys and unknown/invalid/unsupported setting values. Resolve ignored
fields through inheritance/defaults. Normalize accepted state before canonical
schema validation; malformed JSON, duplicate members and unsafe structural or
lifecycle relationships remain errors when no safe deterministic state exists.
Core returns value-free diagnostics; the host/control owner logs warnings for
recoverable failures and errors for fatal connection/controller failures.

The internal `rptadv-json` foundation owns token-aware JSON syntax decoding,
shared by core configuration and existing host/product input boundaries. It has
no controller policy, native descriptor or host dependency. Preserve actual JSON
wire types when enabling shared Serde features; objects must not become numbers
or strings through library-internal token names. Scope metadata and inheritance
remain in core. No new external ABI or independently deployed provider is added.

The core modular candidate uses an authoritative UUID/key/kind/owner registry
and identified captured bytes. Ordinary missing records mean empty overrides
with retained identity. Site protocol defaults and explicit port instances have
separate presence semantics. All ordinary kinds support site defaults. The
candidate is staging data until domain validation; raw bodies are not public
HAL or accepted runtime settings. Filesystem capture and publication stay with
the host/product control plane.

The first filesystem slice captures only an authoritative registered selection.
The existing host-glue crate owns safe bounded reads, a preprovisioned stable
edit lock and content fingerprints; a thin product adapter owns the UUID/kind
path mapping into the core candidate. Readers do not create or repair files.
Rechecks open the current working directory again and preserve external edits.
Fingerprints cover selected paths and absence markers, not an undiscovered tree;
full reload requires a complete allowed-inventory/discovery integration later.

Store accepted resources and private records as immutable objects selected by
one durably committed manifest/state pointer. Keep editable working files
separate and import them only on explicit reload. Validate, save and activate
writes automatically while retaining desired/active/last-known-good distinctions
and inhibited-transmitter management recovery.

The initial immutable installer writes only public object/manifest artifacts in
preprovisioned protected directories, under the existing edit guard. It stages
independent bytes, syncs the file, publishes without replacement and syncs the
parent. Conflicting immutable bytes are preserved; equal retries verify content
and synchronize it. A post-rename durability failure is explicit, not a claim
that storage is unchanged. This primitive does not validate or select accepted
configuration and cannot write state, private, journal or working namespaces.
Directory provisioning and the complete transaction protocol remain separate.

Replace the Unix maintenance socket with loopback HTTPS/HAL. Require authorized
mTLS on every backend route, with separate proxy and CLI identities. Expose
GET everywhere, PUT/PATCH on writable resources, and POST/DELETE on creatable
collections. Use RFC 6901/6902 resource-local updates, conditional writes and
write-only encrypted protocol credentials supplied through a separate systemd
credential. ADRs 0006/0016 frontend OIDC policy does not bypass backend mTLS.

Remove controller INI at the coordinated cutover, including reconstructed
processing text, private-secret and static-node INI readers. Preserve external
directory-feed decoding and USBRadioPlus's public INI format. Reuse typed native
fields and extend checked preparation inputs only where needed; never serialize
controller JSON back through provider INI as a hidden compatibility layer.

Reuse existing lifecycle/media owners. Preserve connections and local/phone
audio across ordinary link changes; normal jitter/PLC adjustments do not tear
down sessions. Configuration work, diagnostics, storage and HTTP remain outside
audio callbacks. Build/test on the designated node at major milestones without
altering its installed service or live configuration.

## Consequences

The linked design and checklist contain the complete scheduler, action, DTMF,
message, radio and secret-store contract. The first implementation milestone is
a JSON resource-model foundation plus shared wire decoding; it does not cut over configuration storage or management transport.
Provider extensions, API/storage activation and native amd64/arm64 verification
remain tracked implementation work. No obsolete alpha compatibility shim or
new feature is implied by this staging.

- [Design](../../../docs/superpowers/specs/2026-11-18-json-rest-configuration-design.md)
- [Checklist](../../../docs/superpowers/plans/2026-11-19-json-rest-implementation-checklist.md)
- [Baseline and progress](../../../docs/superpowers/plans/2026-11-19-json-rest-implementation-progress.md)
