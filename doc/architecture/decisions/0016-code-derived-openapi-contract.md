# ADR 0016: REST contract is code-derived OpenAPI with colocated documentation

Status: Accepted

## Context

rpt_advanced exposes a versioned REST API as a stable foundation for user
interfaces. Hand-maintained API documentation can drift from routed behavior,
schemas, authentication rules, and status responses.

## Decision

Generate an OpenAPI 3.1 contract from the Rust REST route and schema code.
Serve the generated contract at `/api/v1/openapi.json` and an interactive
Swagger UI at `/api/v1/docs`. Both are available without OIDC authentication,
matching read-only status access. The existing loopback-only listener and
nginx/Apache reverse-proxy policy continue to control external exposure.

Use a maintained Rust code-first OpenAPI generator and schema annotations.
Tests fail when a routed REST endpoint lacks a documented operation. The
contract documents every versioned endpoint, request, response, status code,
authentication requirement, and applicable WebSocket streaming handoff.

## Consequences

The deployed API documentation and implementation share one source of truth.
API additions require typed route/schema documentation and targeted contract
coverage. Swagger UI is documentation only; WebSocket remains status-streaming
only and administrative operations remain REST operations.
