# Repository review — 2026-09-30

## Recommendation and history

Reset the active tree to the supplied MVP requirements, preserving history and backup refs. The prototype implements a different product. A normal replacement commit provides a fresh tree without rewriting history.

Repository: [blogle/latchkey](https://github.com/blogle/latchkey). Default branch: `master`. Reviewed tip: `4e270e493db9939d8e31916aa02019b89864101d`. At inspection: clean clone, no open PRs, no branch protection or repository rulesets. No running deployment was inspected or changed.

All four commits are dated 2026-02-08:

| Commit | Change |
| --- | --- |
| d94e511 | Bootstrap repository skeleton |
| ce29e40 | Add roadmap for agent steering |
| 965a99d | Complete Milestone 0 bootstrap workflow |
| 4e270e4 | Add Milestone 1 thin-slice MCP flow |

## MVP gap analysis

Source paths below refer to the [pinned old tree](https://github.com/blogle/latchkey/tree/4e270e493db9939d8e31916aa02019b89864101d).

| Requirement | Existing evidence | Decision |
| --- | --- | --- |
| Direct SDK MCP; modern and preceding era | `crates/gateway/src/main.rs` accepts custom `{tool_name, operation, params}` JSON at `/v1/mcp`; Cargo has no rmcp | Replace protocol layer |
| Exactly search/exec and namespaced discovery | No search/catalog; one configured `/v1/tool` backend | New catalog/search/router |
| One process/image | Workspace and flake build gateway, operator, tool-server and upstream-stub | One binary with embedded reconciler |
| MCPService dynamic configuration | Operator only logs cluster-wide watch events; four CRDs describe servers/tools/principals/policies | New namespaced CRD, status, Secret resolution/rotation |
| Minimal authentication | Principal tokens, allowlists, demo defaults, local rate windows | Optional one inbound token; downstream static Secret headers |
| Preserve MCP results/errors | Custom result wrapper; invalid success JSON becomes `{status: ok}`; errors become generic 502 | Preserve SDK results, content and useful MCP errors |
| Concurrency/cancellation/deadlines | Fixed five-second reqwest timeout; no MCP cancellation or acceptance benchmark | Unproven; replace and benchmark |
| Stateless replicas | No actual MCP state to validate; local principal limits vary per replica | Explicit stateless SDK setup and replica-alternation tests |
| OTel | JSON logs only; metrics endpoint returns a static zero placeholder | OTLP traces/metrics, W3C propagation, trace-ID logs |
| Readiness/isolation/shutdown | Always-200 readiness; one backend; gateway has no graceful shutdown hook | Reconciliation-aware readiness and bounded drain |
| Footprint | Four image definitions; no MVP measurements | Measure RSS, CPU and compressed image |
| CI | `.github/workflows/ci.yml` triggers pushes on `main`, although default branch is `master` | Recreate CI for master/PRs with new implementation |

The old rate-limit mutex is short-lived: its existence is not evidence that downstream execution is serialized. The former spec additionally requests token exchange, optional Redis, managed tool deployments, policy and session affinity. These conflict with the new scope. Preserve the MIT license and useful Nix/non-root packaging lessons, not the old contracts.

## Nexus 0.6.0

Reviewed [Nexus-Router/nexus](https://github.com/Nexus-Router/nexus/tree/5cece2396d4bc567be1be2ba744178d685179089), formerly grafbase/nexus, tag `0.6.0`. This matches the version identified by the user's compatibility repository; the live cluster version was not verified.

Useful patterns, relative to that pinned tree:

- `crates/mcp/src/server/search.rs`: search results include input schemas and deterministic name tie-breaking.
- `crates/mcp/src/downstream.rs`: FuturesUnordered discovery, namespaced routing, local unknown-tool rejection and preserved MCP results/error data.
- `crates/mcp/src/lib.rs`: stateless SDK server. Server telemetry wrappers separate instrumentation from routing.

Do not import Tantivy, per-identity caches, forwarded user auth, LLM routing, Redis rate limiting, prompt/resource aggregation or extra transports. Its client lists one page of tools; Latchkey needs complete paginated discovery. `cache.rs` holds a global refresh lock across dynamic-credential initialization, which is unnecessary here. Static `call_tool` has no application mutex; this review does **not** establish the deployed Nexus bottleneck's root cause. This release uses rmcp 0.7.0 and MCP 2025-06-18.

## Skarn

Reviewed [Rani367/Skarn](https://github.com/Rani367/Skarn/tree/96f08037dd780031c238cf8f15fd6b65ecdfdde6).

`crates/skarn-gateway/src/registry.rs` offers a small immutable registry with namespace resolution and deterministic lexical ordering. `downstream.rs` uses ArcSwap snapshots and SDK clients; its bridge illustrates a reusable internal execution boundary.

Adapt the concepts only. Search hits omit schemas, and additive token scoring does not guarantee whole-name exact matches outrank description matches. Connect/refresh iterate sequentially; refresh drops failed services instead of retaining last successful discovery. Result conversion prefers structured content or extracted text, discarding other MCP content; tool errors become strings. These behaviors do not satisfy this spec.

The gateway directly depends on Code Mode/sandbox crates and rmcp 1.8.0. Do not depend on it wholesale. No runtime, shell-output compression, resource bridge, stdio supervision or third public tool belongs in MVP.

## Compatibility shim and official SDK

Reviewed [blogle/nexus-mcp-compat](https://github.com/blogle/nexus-mcp-compat/tree/36f44a4995424c05d02e5650a9dc86f61f59ba44). Its rmcp 3.0.1 setup demonstrates modern discovery, stateless server configuration, host validation and static musl/OCI packaging. Its bridge test is a useful fixture concept. Do not blindly copy request-per-connection overhead, always-ready health, raw upstream error formatting or modern-only protocol advertisement.

Verified [official rmcp 3.0.1](https://github.com/modelcontextprotocol/rust-sdk/tree/3bb3c8dbce8ff4c1f8c9e91ebe03ca5cd42f3e81) source:

- `crates/rmcp/src/model.rs` defines 2026-07-28, 2025-11-25 and 2025-06-18. `LATEST` still names 2025-11-25; configure and test protocol support explicitly.
- `transport/streamable_http_server/tower.rs` exposes `with_legacy_session_mode(false)`. Modern requests are always stateless; older requests also need stateless configuration.
- `transport/streamable_http_client.rs` awaits each POST in its worker before taking another outgoing message. The reqwest implementation parses a JSON response before returning; SSE processing is spawned after headers arrive. **One shared SDK client can still serialize slow JSON calls or delayed response headers.** Cloning Arc handles does not prove concurrent dispatch.
- `reinit_on_expired_session` defaults to true and can replay a request after a 404. Disable it for execution and audit HTTP retry behavior so mutations are not silently resubmitted.

First engineering gate: benchmark 32 calls with JSON, SSE and delayed headers. Evaluate independent SDK transports per active call with reusable HTTP connection pooling, or a proven SDK-supported concurrent transport/upstream SDK fix. Include handshake cost, memory, metadata and cancellation. Do not introduce custom MCP framing. No runtime concurrency or resource result is claimed by this review.

No external code was copied. Nexus includes MPL-2.0 licensing; Skarn offers MIT/Apache-2.0. Check file terms and retain notices if implementation code is later adopted.

## Validation boundary

This is source/history review, not runtime certification. Legacy runtime checks were not run: they cannot establish the new contract. Protocol interoperability, 32-call concurrency, telemetry delivery, resource targets, live downstreams and tunnel migration remain implementation acceptance work. Keep existing services active until migration proof is recorded.
