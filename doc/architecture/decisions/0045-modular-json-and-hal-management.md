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

Typed protocol profiles normalize sparse public AllStarLink, EchoLink and phone
settings, resolve each port independently and retain value-free source fallback
diagnostics. Shared jitter preserves source histories for later provider limits.
Network accounts remain separate from local port keys/UUIDs; final public
identity projections are inputs to private adoption, not evidence that a stored
credential is usable. Canonical endpoint validation occurs after normalization;
directory arrays enter shared typed settings without delimiter serialization.

The resource-local management patch stage uses maintained RFC 6901/6902
primitives and exact recursive decimal tests. It applies all operations to a
bounded disposable sparse authoring body before inheritance/normalization.
Credential owners receive neutral slots independently of saved private state.
Secret and ancestor reads, and indirect secret destinations, are forbidden;
explicit writes become opaque zeroizing companion edits. Stored credentials
never enter this stage. Shared borrowed JSON validation protects raw private
scalars and decoded member names until the credential handler resolves them.
The host still owns original request buffers, final policy and durable commit.

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

The public-manifest staging codec uses format_version/id/revision/site_uuid/
port_keys/resources and exact-byte SHA-256 references. Empty ordinary bindings
retain identities; metadata and port protocol instances require referenced
objects. A selecting owner supplies the trusted manifest ID, revision and digest.
The protected readback layer never infers accepted state from files it finds.
Private graphs remain explicitly unsupported until their codec exists; unknown
extra members retain the warning policy. Candidate assembly borrows the owned
manifest registry without making raw section bodies runtime-ready.

The first sealed-record primitive uses a four-member format-1 envelope
(format_version/key_id/nonce/ciphertext) and binary, versioned authenticated
context. Store/credential/version UUIDs remain trusted external selection, and
key ID is authenticated routing metadata. Nonce and object version are fresh
per seal. Decryption returns opaque zeroizing bytes, not a validated account.
The systemd key loader has no path/key fallback and accommodates manager-owned
named-ACL delivery. Private graph/payload integration and encrypted file
publication remain separate before credentials can be used at runtime.

The typed private-artifact bridge admits writes only from its encryption
preparation method. It privately derives record/version UUID paths and reuses
the existing immutable engine. A retry borrows the same prepared envelope,
including its original nonce/version. Readback intersects limits, verifies the
ciphertext digest, decodes with trusted external context, loads only the systemd
key and authenticates before returning opaque bytes. Missing keys, corruption
and authentication failures preserve input. This does not establish account
bindings or make a private manifest acceptable to the public-only loader.

The host can explicitly provision a protected empty store and atomically
compare/replace its state selector under the existing edit guard. Product
installs the prepared public objects, verifies the selected graph, stages its
immutable journal and rechecks the complete catalog inventory before switching.
It replays only hash-matched changes and records a clean fingerprint after
full readback. Recovery first proves the visible selector durable before working writes.
Explicit bootstrap can resume only with its expected protected UUID marker.
Baseline publication syncs edited regular files and working directories before
declaring clean. Restore can deliberately replace an interrupted journal with
a verified retained graph and an exact observed working fingerprint. Port
order and named membership survive reload, and bounded directory retirement
frees capacity for new ports.
A confirmed on-disk result is historical and never marks a running owner active.
Private shards may be provisioned only for typed prepared records. The
account-binding owner validates the reference-only encrypted index and leaves
against final protocol/mailbox/command identities. Public and private changes
select through one manifest/state pointer. Every production host supplies its
external write-key policy; locked graphs retain public reads and block writes
and activation. Runtime credential handoff remains a separate join.

Format-1 state and journal fields, import grammar and manifest member order
are specified in the linked storage design. Core owns those durable identity
and path rules. A helper and receiver use identical full-inventory capture.
A startup with interrupted activation stays management-only until recovery.

Protected-store backup, restore, reachability collection and re-encryption
belong to the existing product transaction/private owners. Their executable
consumer uses a separately versioned optional dynamic product descriptor; the
existing product table and ordinary radio/HTTP resource schemas remain unchanged.
Read-only backup captures consistent selected immutable bytes under the edit
guard. Writing/deletion additionally requires the separately opened, same-root
exclusive directory WriterLease held for the complete offline session.

The service checks ordinary writer admission against its provisioned store UUID
under the edit guard. A pending or invalid maintenance receipt leaves a fresh
startup read-only with transmit inhibited and releases its attempted writer
lease. Management can also serve reads while an offline writer holds the
directory lease. A competing owner of a verifiably healthy store excludes a
second writer. When a reader holds the edit guard at opening, the lease is
provisional. The first guarded admission check promotes it or releases it on
denial. If the public coordinator cannot inspect at startup, it releases the
unresolved lease. The native service can finish that inspection at its single
scheduled cold-start and releases the ownerless lease if still blocked.
After offline completion, a submitted explicit apply can reacquire the
lease and check admission before writing its operation ledger. The management
worker and native runtime share the admitted lease through stop and drain,
including when native adoption or bookkeeping is uncertain.

The optional bounded format-1 state `retained_recovery` extension persists
explicit physical manifest roots at the same atomic selector commit. Missing
means empty for existing stores; unsafe rows fail closed. Ordinary transactions
preserve roots, but normal startup does not authenticate historical private
graphs. Maintenance does authenticate them before collection/copy/rotation.
Rotation refuses unresolved working edits and any selected publication journal;
it never infers an unrecorded prior private root. After reconciliation it retains
original physical roots and replacement refs while preserving
logical revisions and historical activation; it never claims process adoption.
Old ciphertext and keys remain separately recoverable, and no automatic pruning,
key export, replacement-key fallback or installed-service operation is implied.

Replace the Unix maintenance socket with loopback HTTPS/HAL. Require authorized
mTLS on every backend route, with separate proxy and CLI identities. Expose
GET everywhere, PUT/PATCH on writable resources, and POST/DELETE on creatable
collections. Use RFC 6901/6902 resource-local updates, conditional writes and
write-only encrypted protocol credentials supplied through a separate systemd
credential. ADRs 0006/0016 frontend OIDC policy does not bypass backend mTLS.

The transport component owns bounded TLS/HTTP work and an opaque secret-safe
request body. Its coordinator claim is nonblocking and rechecks current
identity authorization and expiry. Hosts consume coalesced listener faults.
This component is staged independently of HAL routing and runtime adoption.

The status API extends the same mTLS and origin policy to the read-only
`rptadv.status.v1` WebSocket and HAL snapshots. Both use Product's public
observation capture. Snapshot selection and subscription registration share one
publication boundary. Keep ordered state changes and replaceable measurements
in separate bounded lanes. On lost state, close the stream and require a fresh
capture. Retain only bounded reader-bound REST pages. Audio callbacks publish
only bounded observations and never wait for a reader. The CLI reads point-in-time
snapshots through HAL discovery. See the
[status-stream design](../../../docs/superpowers/specs/2026-11-25-controller-status-stream-design.md).

Ship the operator client as the separately installable `rpt-advanced-cli`
Debian package. It has no Product or radio-library dependency. Members of
`rpt-advanced-admin` may use the separately provisioned CLI credential. The
standalone service depends on the matching CLI package and uses it for a
synchronous, authenticated `ExecReload`. Its client CA, certificate and
key arrive as `rpt-advanced.cli.*` systemd credentials, independently of
the backend server identity and configuration encryption key. A unit
environment setting selects the management origin; changing the listener
requires the corresponding reload-client setting. Missing client
credentials fail the reload without opening an anonymous path.

Temporary maintenance uses a bounded boot-scoped lease resource bound to
the authenticated TLS connection, actor, port, active revision and runtime
generation. The connection owner supplies its lifetime proof; request
headers cannot create one. Typed operations retain their durable request
identity and carry that exact lease. The physical owner rechecks admission
and clears manual PTT, tone and signaling overrides after cancellation,
expiry, connection loss or owner replacement. A lease never retains a
generation-work guard while awaiting operator input.

Remove controller INI at the coordinated cutover, including reconstructed
processing text, private-secret and static-node INI readers. Preserve external
directory-feed decoding and USBRadioPlus's public INI format. Reuse typed native
fields and extend checked preparation inputs only where needed; never serialize
controller JSON back through provider INI as a hidden compatibility layer.

Typed action preparation shares the audited command inventory across macros
and DTMF mappings. It preserves exact parameter text and fixed setting types,
resolves the longest mapping before argument/grant checks, and rejects known
macro cycles. Preparation grants no runtime authority: source, owner, generation
and provider capability admission remain mandatory at execution.

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

## nginx ingress and console package

The owner selected nginx and an independently installable React console
package on 2026-11-27. The console package depends on nginx and can run on a
separate web host. The controller provides deliberately enabled nginx
configuration assets. Its management service stays loopback-only and
continues to authenticate the trusted proxy by mTLS.

The separate console host uses its own verified TLS forwarding identity to
reach the controller-host nginx ingress. The controller-host nginx remains the
OIDC and browser-policy owner and forwards authenticated subject assertions
to the loopback API. It uses the configured public browser origin for Origin
and CSRF checks and authentication callbacks. This leaves the app, API and
status stream on one browser origin in either deployment. WebRTC signaling
uses that path. The media and data association retains the separate transport
defined by ADR 0046.
