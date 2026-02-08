# Latchkey

Latchkey is a Kubernetes-native MCP gateway and operator that brokers short-lived, scoped tool access for AI agents.

## Architecture at a glance
- **Control plane:** Kubernetes operator reconciles Latchkey CRDs into deployable resources.
- **Data plane:** Gateway authenticates callers, enforces policy, and routes MCP calls.
- **Tool plane:** MCP server workloads hold upstream credentials and execute tool calls.

## Quickstart
1. Enter the reproducible environment:
   - `nix develop`
2. Run local quality gates:
   - `just fmt lint test build`
3. Build reproducible binaries and images:
   - `nix build`
4. Start the gateway stub:
   - `cargo run -p latchkey-gateway`
5. Start the operator stub (requires Kubernetes config):
   - `cargo run -p latchkey-operator`

## Dev cluster bootstrap
1. Create a kind cluster:
   - `just kind-up`
2. Load locally built images into kind:
   - `just kind-load-images`
3. Deploy the dev overlay:
   - `just deploy-dev`
4. Verify pods:
   - `kubectl -n latchkey-system get pods`

## Milestone 1 smoke test
1. Run an in-cluster request with static auth:
   - `kubectl -n latchkey-system run mcp-client --rm -it --restart=Never --image=curlimages/curl --command -- sh -c 'curl -sS -X POST http://latchkey-gateway/v1/mcp -H "Authorization: Bearer demo-token" -H "Content-Type: application/json" -d "{\"tool_name\":\"demo.echo\",\"params\":{\"message\":\"hello\"}}"'`
2. Inspect gateway audit logs:
   - `kubectl -n latchkey-system logs deploy/latchkey-gateway`

## Toolchain source of truth
- `flake.nix` defines the pinned development and CI environment.

## Project layout
- `crates/gateway`: HTTP gateway service stubs and middleware skeleton.
- `crates/operator`: CRD types and watch loop scaffolding.
- `crates/tool-server`: example in-cluster MCP tool server for milestone 1.
- `crates/upstream-stub`: stub upstream API used by the example tool server.
- `crates/core`: shared model and helper primitives.
- `deploy/kustomize`: base manifests and dev overlay.
- `docs`: spec, architecture, security, CRDs, operations, ADRs, and contribution guide.

## Documentation
- `docs/spec.md`
- `docs/architecture.md`
- `docs/security.md`
- `docs/crds.md`
- `docs/operations.md`
- `docs/contributing.md`
- `docs/adr/0001-language.md`
- `docs/adr/0002-auth-model.md`
- `docs/adr/0003-milestone1-thin-slice.md`
