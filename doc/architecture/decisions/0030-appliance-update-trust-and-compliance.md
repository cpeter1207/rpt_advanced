# ADR 0030: Appliance update trust and compliance boundary

Status: Accepted

## Context

The appliance is sold in the United States and Canada, accepts field recovery
from USB-C, and is upgraded through Debian packages. It needs a testable
emissions/immunity target and a trust chain that allows normal release-key
rotation without placing an offline root key on an appliance or build system.

## Decision

The finished appliance targets FCC Part 15 Subpart B Class B and ISED ICES-003
Class B digital-device emissions. The production laboratory plan verifies every
external connector against the approved ESD, EFT/burst, and DC-input surge
levels in the appliance PRD. A test passes only if outputs remain safe: it may
not create an unintended PTT or unsafe site-I/O output, lose retained
configuration, or remain faulted.

Debian package updates use a product-scoped APT `Signed-By` keyring. The product
owner retains an offline root key and an offline recovery key. The root key
certifies time-limited online release-signing keys; only those release keys
sign repository metadata. The recovery key signs a recovery manifest containing
the image digest, compatible hardware identities, and expiration. Recovery
verifies that manifest before writing eMMC. Insecure APT sources and
authentication-bypass options are not supported.

A root-certified replacement release key is installed before the current key
expires. A signed revocation update removes a compromised release key, and
release publication stops until a replacement has been distributed. The root
and recovery private keys never exist on production build, repository,
manufacturer, or appliance systems.

Secure boot is a later SoM configuration decision. Until it is enabled, the
trust boundary covers remote package update and controlled recovery, but does
not claim protection when a physical attacker can replace boot storage. Enabling
secure boot must bind the boot verifier's root trust anchor to the SoM's
protected boot chain without changing the APT and recovery role model.

## Consequences

The appliance hardware repository owns the exact test levels, connector list,
key-ceremony procedures, and recovery implementation. The software packages
must ship only through the scoped signed repository and must provide compatible
keyring-rotation and revocation updates. DVT cannot close without an
accredited-laboratory compliance plan and a rehearsed root/release/recovery key
procedure.
