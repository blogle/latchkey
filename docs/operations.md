# Operations

## Health and readiness

- Gateway exposes `/healthz` and `/readyz`.
- Gateway MCP entrypoint is `POST /v1/mcp`.
- Operator logs startup and watcher state transitions.

## Logging

- JSON structured logs via `tracing`.
- Include request and resource context where available.

## Metrics

- Gateway exports a placeholder `/metrics` endpoint in bootstrap.
- Metrics schema and cardinality model should align to `docs/spec.md` before implementation grows.

## Tracing

- OTel hooks are not fully wired in bootstrap.
- Future work should preserve W3C trace context propagation.

## Runbooks (bootstrap)

- Local checks: `just ci`
- Local build: `just build`
- Reproducible bundle build: `nix build`
- Kind image load for dev overlay: `just kind-load-images`
- Local deploy: `just deploy-dev`
- In-cluster smoke test principal token: `demo-token` for principal `demo-agent`
- Optional dev secret for tool->upstream key: `kubectl -n latchkey-system create secret generic latchkey-upstream-credentials --from-literal=api-key=<dev-only-value>`
