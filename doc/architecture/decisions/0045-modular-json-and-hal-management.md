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

Store accepted resources and private records as immutable objects selected by
one durably committed manifest/state pointer. Keep editable working files
separate and import them only on explicit reload. Validate, save and activate
writes automatically while retaining desired/active/last-known-good distinctions
and inhibited-transmitter management recovery.

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
