# ADR 0002: OIDC boundary with optional capability exchange

## Status
Accepted

## Context
Latchkey must support short-lived, scoped access while minimizing identity-system complexity.

## Decision
- Prefer external OIDC issuer tokens for base identity.
- Support optional gateway capability token exchange for 60-120 second request-scoped tokens.

## Consequences
- Off-the-shelf IdP remains source of identity truth.
- Gateway retains control over narrowly scoped request-time authorization.
- Replay prevention and key handling remain gateway responsibilities.
