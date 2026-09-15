# ADR 0016: Control APIs are versioned and code-derived

Status: Accepted

## Context

rpt_advanced exposes versioned REST and status-streaming WebSocket APIs as a
stable foundation for user interfaces. Hand-maintained API documentation can
drift from routed behavior, schemas, authentication rules, and status
responses.

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

REST URLs and WebSocket streams are versioned from their first release. The
WebSocket is status-streaming only; it publishes peak, RMS, clipping, FIFO/ring
statistics, COR, CTCSS, PTT, and link state at 20 updates per second. A slow
client receives the newest meter sample and stale samples are dropped.

Asterisk CLI, REST, and DTMF invoke the controller's supported control/status
catalog as their respective source permits. WebSocket intentionally has no
administrative operations. Configuration backup, validation, rollback, and
audit operations are available only through Asterisk CLI and REST; DTMF and
WebSocket do not expose them. Detailed cross-interface command and DTMF policy
is defined by ADR 0023.

## Consequences

The deployed API documentation and implementation share one source of truth.
API additions require typed route/schema documentation and targeted contract
coverage. Swagger UI is documentation only; WebSocket remains status-streaming
only and administrative operations use the applicable CLI or REST interface.
