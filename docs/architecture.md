# Architecture

## System model

Latchkey is split into a control plane and data plane:
- Control plane: Kubernetes operator owns reconciliation and desired state.
- Data plane: gateway handles identity, policy checks, and MCP dispatch.

## Trust boundaries

1. Clients and agents are untrusted input producers.
2. Gateway is trusted for authn and authz but should run with narrow privileges.
3. Operator is privileged and off request path.
4. Tool servers are isolated workloads with only required secrets and egress.

## Request lifecycle

1. Principal authenticates and optionally exchanges for capability token.
2. Gateway validates token, enforces policy, and validates request shape.
3. Gateway forwards request to mapped tool server.
4. Tool server calls upstream with server-held credentials.
5. Gateway emits audit logs, metrics, and traces.

## Control plane boundaries

- CRDs define desired state.
- Operator reconciles deployable resources.
- Gateway consumes routing and policy snapshots.
