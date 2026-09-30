Latchkey MVP Requirements
Status: Draft
Date: 2026-09-30
Purpose: Define the minimum viable Latchkey release capable of replacing the current Nexus + compatibility gateway path for ChatGPT and other MCP clients.
1. Goal
Latchkey is a lightweight, horizontally scalable MCP gateway.
Its MVP exists to solve one problem well:
Present a very small, stable MCP surface to clients while discovering, searching, and concurrently routing calls to many downstream MCP services.

The MVP is not intended to reproduce every Nexus feature. It should replace the parts of Nexus that are valuable in the current environment while deliberately deferring advanced authentication, policy, persistence, and orchestration features.
The intended initial path is:
ChatGPT / MCP client
        |
        v
OpenAI tunnel / ingress
        |
        v
    Latchkey
   /   |   \
  v    v    v
Anvil Lific GitHub ...
There should be no Nexus-style compatibility sidecar between the tunnel and Latchkey.
2. MVP decisions
Required for MVP
- One stateless service and one OCI image.
- Direct MCP server compatibility with current clients.
- Two public tools:
  - search
  - exec
- Downstream MCP discovery and routing.
- Kubernetes MCPService CRD configuration.
- Static downstream credentials sourced from Kubernetes Secrets.
- Minimal inbound authentication assumptions.
- OpenTelemetry traces, metrics, and structured logs.
- True concurrent downstream execution within a single Latchkey replica.
- Horizontal scaling without sticky sessions or shared persistent state.
- Very small CPU, memory, and image footprint.
- Health/readiness endpoints suitable for Kubernetes.
- Graceful downstream failure isolation.
Explicitly not an MVP gate
- Server-side Code Mode / TypeScript sandbox.
- OAuth delegation or token exchange.
- Per-user downstream credentials.
- Fine-grained scopes or authorization policy.
- Dynamic client registration.
- Multi-tenant isolation.
- Persistent database or cache.
- MCP event/subscription persistence.
- Admin UI.
- Semantic/vector search.
- stdio-managed downstream processes.
- General workflow engine.
- Automatic tool approval policy.
3. Protocol requirements
Latchkey must expose an MCP endpoint directly to clients.
3.1 Compatibility
The implementation must use a current MCP SDK with support for the 2026-07-28 MCP specification and compatibility with the immediately preceding MCP protocol era.
For the initial Rust implementation, the official rmcp SDK is preferred.
Latchkey must rely on the SDK for:
- protocol framing;
- version negotiation;
- Streamable HTTP behavior;
- cancellation;
- standard MCP errors;
- request/response metadata;
- trace-context propagation primitives.
Latchkey must not implement a custom MCP wire protocol.
3.2 Stateless upstream server
The public MCP endpoint must operate without server-local client session state for the MVP.
Any healthy Latchkey replica must be able to serve any request.
Requirements:
- no sticky sessions;
- no per-client in-memory state required for search or exec;
- no shared database required between replicas;
- replicas may be added or removed without draining protocol state;
- Kubernetes Service load balancing must be sufficient.
3.3 Public MCP surface
The MVP exposes exactly two gateway tools.
search
Search the current downstream tool catalog.
Suggested input:
{
  "query": "create an anvil session",
  "service": "anvil",
  "limit": 10
}
service and limit are optional.
Each result should contain enough information for a model or Code Mode caller to invoke the tool without another discovery round trip:
{
  "name": "anvil__session_create",
  "service": "anvil",
  "description": "Create an Anvil worker session",
  "input_schema": {}
}
Search requirements:
- local/in-memory;
- no downstream network call on the normal search path;
- exact-name matches rank highest;
- prefix/name matches outrank description-only matches;
- deterministic ordering for equivalent scores;
- namespace/service filtering;
- configurable result limit with a conservative upper bound;
- no embedding model or vector database in MVP.
A lightweight lexical scoring implementation is sufficient.
exec
Execute one fully qualified downstream tool.
Suggested input:
{
  "name": "anvil__session_create",
  "arguments": {
    "project": "..."
  }
}
Requirements:
- route using the current catalog;
- forward arguments without lossy transformation;
- preserve structured downstream MCP results;
- preserve useful downstream MCP errors;
- reject unknown tools locally;
- enforce configurable request deadlines;
- honor cancellation when the upstream request is cancelled;
- do not serialize independent tool calls.
The canonical tool name format is:
<service-prefix>__<downstream-tool-name>
Examples:
anvil__session_create
github__get_file_contents
lific__create_issue
4. Downstream MCP support
4.1 MVP transport
MVP downstream services use Streamable HTTP MCP.
stdio process management is deferred.
Each configured service has:
- endpoint URL;
- stable service prefix;
- enabled/disabled state;
- request timeout;
- optional static headers sourced from Secrets;
- discovery refresh configuration.
4.2 Discovery
Latchkey maintains an in-memory catalog of downstream tools.
Discovery occurs:
1. at process startup;
2. when an MCPService object is added or changed;
3. periodically as a safety net.
A failure to discover one downstream service must not make the whole gateway unavailable.
The last known successful catalog may remain available while a downstream is temporarily unhealthy, but exec must return a clear downstream-unavailable error if execution cannot be performed.
4.3 Concurrency
This is a hard MVP requirement.
A single slow downstream request must not block unrelated requests to that service or any other service.
The implementation must not place a global mutex around:
- a downstream client;
- tool execution;
- the catalog;
- transport send/receive;
- service reconciliation.
Use an SDK/transport configuration that supports concurrent Streamable HTTP requests.
The design should assume many simultaneous calls to the same downstream MCP server.
5. Kubernetes configuration
In Kubernetes mode, Latchkey is configured through a CRD; standalone mode is defined in section 5.3.
The MVP CRD is namespaced and should normally live in the same namespace as Latchkey. This keeps Secret access and RBAC simple.
Suggested API:
apiVersion: latchkey.thejeffer.net/v1alpha1
kind: MCPService
metadata:
  name: anvil
  namespace: latchkey
spec:
  endpoint: http://anvil-mcp.anvil.svc.cluster.local:8081/mcp

  # Defaults to metadata.name.
  prefix: anvil

  enabled: true
  timeout: 180s

  discovery:
    refreshInterval: 5m

  headersFrom:
    - header: Authorization
      secretKeyRef:
        name: anvil-mcp-credentials
        key: authorization
The Secret should contain the complete header value when possible, for example:
Bearer <token>
This avoids building provider-specific credential formatting into Latchkey.
5.1 CRD behavior
Applying a new MCPService must not require a Latchkey restart.
On add/update/delete:
- reconcile the service;
- perform discovery;
- atomically update the in-memory routing catalog;
- expose reconciliation state through CRD status.
Suggested status:
status:
  observedGeneration: 4
  toolCount: 17
  lastDiscoveredAt: "2026-09-30T20:00:00Z"
  conditions:
    - type: Ready
      status: "True"
A malformed or unavailable downstream service should receive a useful status condition without making unrelated services unavailable.
5.2 RBAC
The MVP should require only:
- read/watch/list MCPService resources;
- update MCPService/status;
- read/watch the Secrets needed for configured static headers.
For the first deployment, all credential Secrets should live in the Latchkey namespace.
Cross-namespace Secret references are deferred.
5.3 Mandatory standalone development and end-to-end mode
Latchkey must be runnable and testable end to end on a developer machine and in CI without a Kubernetes cluster, Kubernetes API server, Kubernetes client dependency at runtime, or any Kubernetes mock/fake. This is an MVP requirement.
The same production binary, MCP server, catalog, discovery, router, auth, telemetry, health handlers, timeout handling, cancellation, and shutdown path must be exercised in both modes. Only configuration and credential sources differ:
- Kubernetes mode watches namespaced MCPService objects and referenced Secrets.
- Standalone mode loads local service definitions with the same effective fields and behavior, plus static header values from local files or environment references. Local configuration must not contain credential values committed to the repository.
Both sources feed one shared reconciliation path. Standalone mode must never initialize a Kubernetes client or require a kubeconfig. A local file change or explicit reload must exercise add, update, disable, delete, and credential rotation without restarting the gateway. Invalid local configuration or one unavailable downstream must not corrupt other catalog entries.
The repository must provide a documented one-command local development path and an automated end-to-end suite that starts the actual Latchkey binary, real Streamable HTTP MCP test downstream servers, and a real MCP client. The suite must cover protocol negotiation, search, exec, catalog changes, static headers, failure isolation, cancellation, deadlines, graceful shutdown, trace propagation, secret redaction, and the 32-call concurrency benchmark. It must run on ordinary CI runners without kubectl, kind, a cluster, or Kubernetes mocks. Kubernetes integration tests are additional evidence for CRD watch, status, Secret and RBAC behavior; they do not replace the standalone end-to-end suite.
6. Authentication and security
Authentication is intentionally minimal in the MVP.
6.1 Inbound authentication
Latchkey is expected to run behind a trusted ingress, private network, tunnel, or authentication proxy.
MVP requirements:
- support unauthenticated MCP at the process level when protected externally;
- optionally support one static bearer token for direct deployments;
- never require a built-in OAuth server for MVP.
Advanced inbound identity is deferred.
6.2 Downstream authentication
MVP supports static downstream headers sourced from Kubernetes Secrets.
This is sufficient for API keys, PATs, bearer tokens, and pre-generated service credentials.
Deferred:
- OAuth authorization-code flows;
- per-user credential delegation;
- refresh-token management;
- scope negotiation;
- token exchange;
- dynamic client registration;
- downstream identity impersonation.
6.3 Secret handling
Hard requirements:
- Secret values must never appear in logs.
- Secret values must never appear in traces.
- Secret values must never appear in search results.
- Local credential values must never appear in logs, traces, or search results.
- Authorization headers must be redacted from structured logging.
- CRDs contain references, not credentials.
- Panic/error formatting must not dump request headers.
6.4 Sandbox boundary
Because Code Mode is not part of MVP, the gateway does not initially execute arbitrary user code.
This substantially reduces the initial security surface.
7. OpenTelemetry and observability
OpenTelemetry is an MVP requirement, not a later enhancement.
Latchkey should emit OTLP and integrate cleanly with an existing collector.
Collector unavailability must not prevent Latchkey from serving requests.
7.1 Traces
Required spans include:
mcp.request
gateway.search
gateway.exec
catalog.reconcile
downstream.discover
downstream.call
Useful span attributes include:
- gateway tool (search / exec);
- downstream service;
- downstream MCP tool;
- result status;
- timeout/cancellation status;
- protocol version;
- replica/pod identity.
Do not put full arbitrary argument payloads into traces by default.
Latchkey should propagate W3C trace context to downstream HTTP MCP services.
7.2 Metrics
At minimum:
- inbound request count;
- inbound request latency;
- active requests;
- search latency;
- exec latency;
- downstream request count;
- downstream request latency;
- downstream error count;
- downstream timeout count;
- downstream in-flight requests;
- discovery success/failure count;
- discovery duration;
- catalog service count;
- catalog tool count.
Metrics must avoid unbounded labels.
Service name is acceptable. Arbitrary argument values are not.
7.3 Logs
Structured JSON logs.
Each request log should include the trace ID when tracing is active.
Normal successful requests should not require verbose per-packet transport logs.
8. Performance and scalability requirements
Latchkey should be small enough that adding replicas is cheap.
8.1 Architecture
MVP has:
- one binary;
- one OCI image;
- no database;
- no Redis;
- no sidecar;
- no embedded search service;
- no background worker deployment;
- no leader election requirement.
Every replica independently watches configuration and maintains its own in-memory catalog.
8.2 Resource targets
Initial targets:
- Kubernetes request: approximately 10m CPU / 32Mi memory;
- idle RSS target: <= 48MiB;
- normal memory limit target: <= 96MiB;
- compressed OCI image target: <= 30MiB;
- effectively zero CPU while idle except watches/health/discovery timers.
These are engineering targets rather than protocol guarantees. Material regressions should require justification.
8.3 Horizontal scaling
Latchkey must scale from 1 to at least 10 replicas without:
- shared persistence;
- leader election;
- sticky sessions;
- duplicate side effects caused by internal retries.
Configuration convergence should be eventual and quick enough for normal Kubernetes operations.
8.4 Concurrency acceptance benchmark
The Nexus failure mode must have an explicit regression test.
Using a test downstream MCP server that records tool-call arrival times:
- start 32 concurrent exec requests;
- all 32 must be accepted concurrently by one Latchkey replica;
- gateway-added dispatch spread should remain below 100 ms on normal development/CI hardware;
- one intentionally slow request must not delay unrelated calls;
- cancellation of one request must not cancel or block others.
The exact latency number may be adjusted after the first implementation benchmark, but serialization is an automatic MVP failure.
9. Reliability requirements
- One unhealthy downstream must not affect other downstream services.
- A downstream timeout must release all gateway resources associated with the request.
- Client cancellation must propagate where supported.
- Graceful shutdown must stop accepting new work and allow bounded in-flight completion.
- Discovery failures must be visible through OTel and CRD status.
- Catalog updates must be atomic from the perspective of search and exec.
- Removed/disabled services must stop accepting new executions promptly.
- Latchkey must not automatically retry mutation tools unless the protocol or caller provides an explicit idempotency mechanism.
10. Kubernetes health endpoints
The same binary should expose lightweight HTTP health endpoints.
/healthz
Process is alive.
Must not depend on downstream MCP health.
/readyz
Process is ready to accept gateway traffic.
Readiness should require:
- MCP server initialized;
- the selected configuration source and shared reconciler functioning (CRD watcher in Kubernetes mode; local loader/reload in standalone mode);
- initial configuration reconciliation completed.
It should not require every configured downstream to be healthy.
A broken GitHub MCP server should not remove Anvil/Lific availability.
11. Code Mode decision
Recommendation: not an MVP gate
Server-side Code Mode is strategically valuable but should not block the first usable Latchkey release.
The first milestone already solves the critical problems:
- stable two-tool client surface;
- small tool context;
- dynamic service discovery;
- concurrent routing;
- observability;
- Kubernetes-native configuration;
- removal of Nexus and the compatibility shim.
Adding arbitrary TypeScript execution introduces a separate set of concerns:
- sandbox escape risk;
- memory limits;
- CPU/fuel accounting;
- wall-clock deadlines;
- output limits;
- cancellation;
- module policy;
- deterministic host bindings;
- code caching;
- denial-of-service controls.
Those concerns are large enough to deserve their own milestone.
11.1 MVP architectural requirement for future Code Mode
Although Code Mode is deferred, the MVP must expose internal abstractions that let a future sandbox use exactly the same catalog and execution path as the public tools.
Conceptually:
             +----------------+
MCP search ->|                |
MCP exec   ->| Catalog/Router |-> downstream MCP
             |                |
future code->|                |
             +----------------+
Do not implement search and exec as logic tied directly to MCP request handlers.
They should call reusable internal APIs such as:
Catalog::search(...)
Router::exec(...)
11.2 Post-MVP Code Mode shape
A future third public tool could be:
code(source: string) -> JSON
The sandbox would execute TypeScript/JavaScript and expose only constrained host functions:
await latchkey.search("anvil session");
await latchkey.exec("anvil__session_create", { ... });
Preferred implementation direction:
- JavaScript runtime isolated with WASM or an equivalently strong sandbox boundary;
- TypeScript transpilation without package installation;
- no direct network access;
- no filesystem access;
- no environment-variable access;
- no Kubernetes API access;
- no direct Secret access;
- only search and exec host capabilities;
- Promise.all and normal async orchestration supported;
- strict memory limit;
- strict execution/fuel limit;
- wall-clock timeout;
- bounded stdout/result size.
Code Mode becomes an MVP+1 gate, not an MVP gate.
12. Non-goals for MVP
The following should not be implemented unless a concrete blocker appears during migration.
Authentication/policy
- OAuth broker;
- authorization-server metadata hosting;
- dynamic client registration;
- token exchange;
- scope intersection;
- per-tool ACL language;
- per-user policy;
- credential database.
MCP platform features
- durable tasks;
- durable subscriptions;
- ChatGPT MCP Events delivery;
- webhook subscription storage;
- prompts/resources aggregation;
- sampling proxy;
- elicitation proxy.
These can be added later using the MCP SDK rather than designed into the first gateway.
Operations
- web admin UI;
- database-backed configuration;
- GitOps replacement;
- separate operator/controller binary;
- HA coordinator;
- distributed cache.
Search
- embeddings;
- vector database;
- LLM-based ranking;
- remote search service.
The tool catalog is expected to remain small enough for lexical in-memory search during MVP.
13. Suggested implementation shape
Preferred stack:
Rust
Tokio
official rmcp SDK
kube-rs
tracing + OpenTelemetry
serde / schemars
rustls
Internal modules should remain small:
src/
  main.rs
  server.rs          # public MCP search/exec surface
  catalog.rs         # immutable/current tool catalog
  search.rs          # lexical ranking
  router.rs          # downstream tool resolution
  downstream.rs      # MCP client lifecycle/concurrency
  controller.rs      # MCPService watch/reconcile
  config.rs          # Kubernetes and standalone sources feeding one reconciler
  auth.rs            # static secret/header handling only
  telemetry.rs
  health.rs
In Kubernetes mode, the Kubernetes reconciler and MCP gateway run in the same process. Standalone mode replaces only the configuration source; the gateway and shared reconciler remain the same.
14. Definition of done
Latchkey MVP is complete when all of the following are true.
Gateway
- One OCI image deploys one stateless service.
- Public MCP surface contains only search and exec.
- OpenAI tunnel can point directly at Latchkey with no compatibility service.
- Current MCP clients can negotiate and invoke both gateway tools.
- Tool names are stable and namespaced.
Configuration
- MCPService CRD exists.
- Adding a CRD dynamically adds a downstream service.
- Updating a CRD reconfigures it without restarting Latchkey.
- Deleting/disabling a CRD removes it from routing.
- CRD status exposes discovery/health state.
- Static auth headers can be sourced from Kubernetes Secrets.
- The same binary runs end to end in standalone mode with local service definitions and file/environment credential references, without any Kubernetes component or mock.
- Local configuration reload exercises add/update/disable/delete and credential rotation without a restart.
Standalone development and testing
- A documented one-command local run path starts Latchkey and real Streamable HTTP downstream fixtures.
- Automated tests use the actual binary and MCP client to exercise the public endpoint and downstream behavior, including the 32-call benchmark.
- The standalone end-to-end suite passes in CI without Kubernetes, kubectl, kind, or Kubernetes mocks.
- Kubernetes integration tests independently verify CRD watches, status, Secret resolution/rotation, and RBAC.
Search / execution
- Search returns useful ranked tool matches with schemas.
- Exec routes to the correct downstream service.
- Downstream results and structured errors survive the gateway.
- A failed downstream does not affect unrelated services.
- Concurrent calls are not serialized.
Observability
- OTLP traces are emitted.
- W3C trace context propagates downstream.
- Core gateway/downstream metrics exist.
- Structured logs contain trace IDs.
- Credentials and arbitrary tool arguments are not leaked.
Scalability
- One replica passes the concurrent dispatch benchmark.
- Multiple replicas require no sticky sessions.
- Multiple replicas require no shared database/cache.
- Resource and image-size targets are measured and documented.
Migration proof
- Anvil is usable through Latchkey.
- Lific is usable through Latchkey.
- GitHub MCP is usable through Latchkey.
- A ChatGPT session can use search then exec through the OpenAI tunnel.
- Repeated concurrent read-only calls show reliable fan-out.
- The Nexus + compat path can be disabled without losing required gateway behavior.
15. MVP+1 priorities
Once the MVP is stable, investigate in roughly this order:
1. Server-side Code Mode / TypeScript sandbox.
2. Better authorization and per-user identity propagation.
3. Scoped downstream credentials and policy.
4. MCP Events / durable subscriptions.
5. Richer service health/circuit breaking.
6. Optional semantic search if lexical search becomes insufficient.
7. Additional downstream transports only if a real service requires them.
16. Core design principle
Latchkey should remain a gateway, not become an application platform by accident.
For the MVP:
CRDs describe services.
Latchkey discovers tools.
search finds tools.
exec invokes tools.
OTel explains what happened.
Replicas scale horizontally.
Everything else must earn its way into the design.
