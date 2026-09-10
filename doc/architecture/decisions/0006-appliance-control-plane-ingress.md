# ADR 0006: Appliance control-plane ingress

Status: Accepted

## Context

The standalone appliance must provide delegated web administration while
remaining suitable for a small ARM system and protecting its local control
services from direct Internet traffic.

## Decision

Run Keycloak and nginx or Apache on the appliance. The reverse proxy is the
only public control-plane ingress; REST and WebSocket services stay bound to
loopback. Configure the local services for a small user population and deploy
a WAF. Support IP allow and deny lists so the appliance can trust a proxy such
as Cloudflare and restrict ingress to its address ranges.

## Consequences

Proxy identity headers are accepted only from configured trusted proxy ranges.
The appliance needs a managed WAF ruleset, proxy-range update mechanism, and
safe behavior when proxy configuration is absent or stale. Resource budgets
must include Keycloak, the proxy, and WAF in addition to radio processing.
Trusted proxy ranges update automatically while retaining the last known-good
set on failure. Without a trusted proxy, HTTPS is limited to explicitly allowed
private/LAN sources and otherwise fails closed. Certificates are provisioned by
Let's Encrypt.
