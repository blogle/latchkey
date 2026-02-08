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
   - `just ci`
3. Start the gateway stub:
   - `cargo run -p latchkey-gateway`
4. Start the operator stub (requires Kubernetes config):
   - `cargo run -p latchkey-operator`

## Toolchain source of truth
- `flake.nix` defines the pinned development and CI environment.

## Project layout
- `crates/gateway`: HTTP gateway service stubs and middleware skeleton.
- `crates/operator`: CRD types and watch loop scaffolding.
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
