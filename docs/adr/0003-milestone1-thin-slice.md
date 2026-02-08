# ADR 0003: Milestone 1 Thin Slice Shape

## Status

Accepted

## Context

Milestone 1 requires a working in-cluster request path with minimal but real authn/authz,
audit logging, and coarse guardrails. We need a thin slice that is easy to run locally,
auditable, and compatible with the security-first target model in `docs/spec.md`.

## Decision

For Milestone 1, Latchkey uses a static gateway-secret MVP mode with a single MCP route:

- Gateway exposes `POST /v1/mcp` and validates `Authorization: Bearer <token>`
  against `LATCHKEY_STATIC_TOKENS`.
- Gateway enforces a principal tool allowlist from `LATCHKEY_TOOL_ALLOWLIST`.
- Gateway applies coarse per-principal rate limiting, request size limits, and timeout.
- Gateway emits structured audit events for allow and deny decisions.
- Gateway forwards allowed calls to one in-cluster tool server endpoint.

The tool path for this milestone is:

- `gateway -> latchkey-tool-server -> latchkey-upstream-stub`
- Tool server sends an API key from `UPSTREAM_API_KEY` to the upstream stub.
- Upstream stub validates the key and returns a sanitized payload.

## Consequences

- This keeps MVP auth simple and deterministic while preserving a migration path to OIDC.
- The thin slice demonstrates secret isolation without exposing credentials to clients.
- Policy source is environment-driven for now; CRD-backed policy remains Milestone 2+.
