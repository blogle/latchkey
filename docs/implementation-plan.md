# MVP architecture and acceptance plan

The supplied [spec](spec.md) remains authoritative. This document proposes implementation sequencing, not additional product scope.

## Architecture

One Rust binary runs Tokio, the official rmcp endpoint, health handlers and nonblocking OTel export. In Kubernetes mode it also runs the kube-rs configuration source. In standalone mode it does not initialize kube-rs or need a kubeconfig. Use rustls, serde and schemars. Pin SDK/toolchain/dependencies after the first protocol/concurrency spike; rmcp 3.0.1 is a verified candidate, not proof of full acceptance.

Thin server handlers call `Catalog::search` and `Router::exec`. Publish an immutable snapshot containing both tool metadata and matching route/client-generation references. Search and exec each load a consistent snapshot; no reader holds a lock across I/O. Concurrent reconciliation must merge per-service changes without overwriting another service's successful update. Reject duplicate prefixes and ambiguous canonical names rather than silently replacing tools.

Run discovery asynchronously per service, with bounded concurrency and timeouts. Startup, CRD add/update and periodic refresh trigger complete paginated discovery. A slow service must not block others or hold publication locks. Track Kubernetes UID/generation and Secret revision so an old discovery cannot resurrect a deleted/disabled service or overwrite newer credentials. Remove/disable invalidates admission promptly; already admitted calls get bounded completion. Last-known successful metadata may survive temporary discovery failure; execution must report unavailability when routing cannot succeed. Never continue using revoked credentials as a fallback.

Namespaced `MCPService` lives under `latchkey.thejeffer.net/v1alpha1`, with endpoint, prefix defaulting to metadata.name, enabled, timeout, refreshInterval and Secret header references. Secret changes trigger reconciliation. Publish observedGeneration, toolCount, lastDiscoveredAt and Ready conditions. Scope RBAC to the deployment namespace and required resources/status/Secrets. Kubernetes watch setup may require list as well as get/watch; document exact Secret permissions and avoid unrelated namespaces.

Define a small configuration-source interface that emits normalized service upserts/removals and credential changes into the same reconciler. Kubernetes watches are one implementation. Standalone mode reads a local file with the same effective service fields and credential references resolved from local files or environment variables. It supports reload by file change or explicit signal/API, applying one atomic validated snapshot and preserving unaffected services on malformed input. Never store local credential values in tracked fixtures. The local loader and Kubernetes watcher may need different status sinks, but discovery, catalog publication, routing, health and telemetry are shared.

Search is purely local: use explicit rank tiers (whole canonical-name exact, name/prefix, description) and canonical-name tie-breaks. Include name, service, description and full input_schema. Suggested limit default 10, maximum 50; validate these as documented configuration choices. Filter by service before ranking. Define empty-query behavior deterministically.

Exec resolves the exact catalog entry, passes argument JSON without coercion and preserves all SDK result content, structured content, metadata, isError and useful MCP error data. Unknown names fail locally. Separate catalog lifetime from execution transport lifetime. Prove the chosen SDK transport model allows concurrent calls to the same service; a shared Arc is insufficient. Disable session reinitialization replay and other mutation retries. Use request-specific cancellation, never cancellation of a shared service to terminate one call. Deadlines include connection/handshake work and release resources.

Credentials never enter catalog results, traces or logs. Use complete Secret header values, mark sensitive headers and sanitize transport errors. Do not log arguments by default. Static inbound bearer auth is optional behind the trusted ingress; no OAuth or policy system is needed.

Emit the required six span categories and all metrics from spec section 7; propagate W3C context using SDK/HTTP hooks. Use bounded metric dimensions, structured JSON and trace IDs. Exporter outages must not block gateway traffic. `/healthz` is process-only; `/readyz` reflects initialized MCP, functioning watcher/reconciler and completed initial reconciliation attempts, not universal downstream health. Stop admission before bounded shutdown draining; then cancel remaining work.

## Delivery sequence

1. **SDK protocol and concurrency spike.** Establish modern 2026-07-28 and preceding 2025-11-25 compatibility; include 2025-06-18 for the existing downstream era. Use official framing and stateless upstream mode. Benchmark independent execution against JSON and SSE responders, including delayed headers. Select client lifecycle only after this passes.
2. **Standalone vertical slice.** One binary with a local configuration source, real MCP fixtures, search/exec, immutable catalog/router, failure isolation, cancellation, deadlines and OTel. Include full result/error round trips, unknown-tool rejection, no replay and live local reload. Publish a documented one-command local run and end-to-end CI suite before Kubernetes wiring.
3. **Kubernetes configuration.** Connect the embedded watcher/reconciler to the same normalized configuration path; add MCPService schema/status, Secret rotation, pagination, periodic discovery, atomic updates and delete/disable races. Keep unrelated services available through failures. Test CRD/RBAC behavior separately against a real cluster.
4. **Packaging and operational checks.** Reproducible Nix build/dev shell, one minimal non-root OCI image, namespaced RBAC, deployment/probes and appropriate network access. CI on master and PRs: formatting, lint, tests, dependency checks, image/build checks and cluster-free end-to-end checks. Measure resources instead of assuming small Rust binaries meet targets.
5. **Migration proof.** Deploy alongside the current path. Validate Anvil, Lific and GitHub, then direct tunnel/client search→exec, repeated read-only fan-out and 1–10 replicas. Record evidence before switching ingress and disabling Nexus/compat. Keep the old ingress path available for rollback during verification.

## Acceptance evidence

| Area | Required proof |
| --- | --- |
| Surface/protocol | Exactly search and exec; SDK version negotiation; current/preceding clients; direct ingress without shim; successive requests can hit different replicas |
| Search | Exact matches dominate; name/prefix outranks description; deterministic ties; filter/limits; schemas intact; no network on normal path |
| Results | Text, image/resource content, structured content, metadata, isError and protocol errors survive; arguments unchanged |
| Concurrency | Barrier-start 32 exec calls to one service on one replica; downstream records all arrival times before releasing replies; dispatch spread <100 ms on normal CI hardware; separate JSON/SSE/delayed-header cases |
| Isolation | Slow call does not delay same-service or other-service calls; cancel one and prove others finish; timeout releases resources; unavailable downstream does not fail readiness for healthy services |
| Reconciliation | Add/update/delete/disable; rotation; restart/relist; duplicate prefixes; paginated tools; stale-generation completion; concurrent service updates; accurate status |
| Standalone end to end | On ordinary CI with no Kubernetes process/mock/tooling: launch the real binary, MCP downstream fixtures and MCP client; prove live local reload, credentials, protocol, routing, failures, cancellation, trace propagation, shutdown and the 32-call benchmark |
| Kubernetes integration | Against a real cluster, prove CRD status, Secret resolution/rotation, watch behavior and RBAC; this is additional to standalone tests |
| Retry safety | Fault-inject expired session/network failures; side-effect counter proves no implicit mutation replay |
| Telemetry | Required spans/metrics, downstream traceparent, log trace IDs, collector outage; sentinel Secrets/arguments absent from captured logs/traces/search |
| Scale/resources | 1–10 replicas without affinity/shared state/leader; idle RSS <=48 MiB; normal limit target <=96 MiB; request target 10m CPU/32Mi; compressed image <=30 MiB; document test hardware/load and regressions |
| Operations | Startup reconciliation and watcher health drive readiness; bounded SIGTERM drain; Anvil/Lific/GitHub plus direct tunnel migration evidence |

All evidence remains pending in this requirements-only baseline. Future Code Mode should call the same internal catalog/router; do not add sandbox dependencies or a third tool now.
