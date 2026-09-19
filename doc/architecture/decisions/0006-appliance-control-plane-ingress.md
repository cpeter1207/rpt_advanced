# ADR 0006: Appliance control-plane ingress

Status: Accepted

## Context

The standalone appliance must provide delegated web administration while
remaining suitable for a small ARM system and protecting its local control
services from direct Internet traffic.

## Decision

Run Keycloak and nginx or Apache on the appliance. Keycloak supplies a local
user registry and can delegate to external OIDC identity providers. The reverse
proxy is the only public control-plane ingress; REST and WebSocket services
stay bound to loopback. Unauthenticated clients may view all status, while
every authenticated OIDC user receives full administrative access.

Configure the local services for a small user population and deploy a
lightweight WAF with an OWASP-style ruleset, few-user rate limits, and
non-blocking suspected-false-positive logging. Support IP allow and deny lists
so the appliance can trust a proxy such as Cloudflare and restrict ingress to
its address ranges.

## Consequences

Proxy identity headers are accepted only from configured trusted proxy ranges.
Trusted proxy ranges update automatically while retaining the last known-good
set on failure. Without a trusted proxy, HTTPS is limited to explicitly allowed
private/LAN sources and otherwise fails closed. Certificates are provisioned by
Let's Encrypt. Resource budgets must include Keycloak, the proxy, and WAF in
addition to radio processing. The interface-specific authorization and command
rules are defined by ADR 0023.
