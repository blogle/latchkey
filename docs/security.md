# Security

## Baseline stance

- Deny by default for authorization and network access.
- Keep credentials only in tool server execution boundaries.
- Prefer short-lived capability tokens for request-time access.

## Hardening defaults

- Run workloads as non-root.
- Drop Linux capabilities.
- Set read-only root filesystem where feasible.
- Disable privilege escalation.
- Apply resource limits to reduce abuse impact.

## Request guardrails

- Payload and response byte caps.
- Strict schema validation with unknown fields disabled by default.
- Operation-level authorization checks.
- Principal and tool scoped rate limits.
- Replay prevention using jti cache and short TTL.

## Operational controls

- Structured audit events per request.
- Redaction policy for request and response logging.
- Security regression checks in CI via `cargo deny`.
