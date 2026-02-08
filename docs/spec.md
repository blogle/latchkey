# Spec: Latchkey MCP Gateway Operator Stack

Latchkey is a Kubernetes-native gateway and operator that issues short-lived, scoped permissions to AI agents so they can use tools safely when no one is home.

A Kubernetes-native, security-first control plane and data plane for hosting and brokering Model Context Protocol (MCP) tool servers. The system provisions MCP servers in-cluster, manages secret distribution to those servers, issues short-lived scoped access tokens to clients and agents, and proxies all MCP traffic through a single gateway endpoint.

This spec focuses on service behavior, APIs, security boundaries, CRDs, and guardrails. It avoids environment-specific choices and keeps integrations (for example, observability platforms) at the API boundary level.

---

## 1. Problem Statement

LLM agents need access to tools and services that require credentials (API keys, OAuth tokens, and so on). Giving agents direct access to those secrets increases blast radius: prompt injection or tool misuse can exfiltrate credentials or perform unintended actions.

We want a general-purpose system that:
- Provisions MCP tool servers in Kubernetes.
- Ensures agents never see real upstream credentials.
- Enforces least privilege per agent and per tool call.
- Provides strong auditing, observability, and guardrails.
- Uses off-the-shelf identity and security components where practical.

---

## 2. Goals

### 2.1 Core
1. Single gateway endpoint: clients talk only to the gateway; gateway dispatches to registered MCP servers.
2. Operator-managed lifecycle: install, upgrade, and scale MCP servers; manage routing metadata.
3. Secret isolation: real upstream credentials are accessible only to tool server workloads (or optional adapter workloads), never to agents.
4. Per-agent authorization: each agent has an identity and tool-level permissions.
5. Short-lived access: agents obtain an access token with very short TTL (target 1-2 minutes) for the intended tool usage.
6. Defense in depth: network isolation, rate limits, request size limits, schema validation, and replay protection.
7. Auditing and observability: every tool call is logged in a structured, queryable format; metrics and traces are emitted via standard interfaces.

### 2.2 Secret distribution
- Support both:
  - Kubernetes Secrets (including SealedSecrets workflows that create Kubernetes Secrets)
  - Vault (optional; via an abstract secret provider interface)
- Keep current simple mode viable: use Kubernetes Secrets produced by SealedSecrets.

### 2.3 General usability
- Provide stable CRDs and APIs suitable for open-source adoption.
- Provide deployment manifests for service-dedicated dependencies (for example, Redis for replay protection), but avoid bundling cluster-wide infrastructure.

---

## 3. Non-Goals

1. No LLM inference in this system. No token spending. No tool recommendation.
2. Not a general CI and CD system. It can deploy MCP servers but does not replace GitOps.
3. Not a full-blown secrets manager (Vault integration is optional and delegated).
4. Not a general service mesh. We may optionally integrate with mTLS and mesh, but do not require it.
5. Not a generic API gateway for arbitrary services; focus is MCP.

---

## 4. High-Level Architecture

### 4.1 Components

**Control Plane**
- **Latchkey Operator** (Kubernetes controller):
  - Watches CRDs for MCPServer, MCPTool, AgentPrincipal, and Policy.
  - Reconciles Deployments, Services, ConfigMaps, and NetworkPolicies.
  - Manages gateway routing config (as CRD status or ConfigMap).

**Data Plane**
- **Latchkey Gateway** (HTTP service):
  - Terminates client auth.
  - Authorizes tool usage.
  - Routes MCP requests to the correct MCP server.
  - Produces audit logs, metrics, and traces.

**Optional Supporting Services**
- Replay cache (Redis-compatible) for one-time token or jti replay prevention.
- Policy engine (optional) if externalized (for example, OPA). The gateway may embed a policy evaluator instead.

### 4.2 Trust Boundaries

- Agents and Clients: untrusted. Assume prompt injection and malicious inputs.
- Latchkey Gateway: trusted to enforce policy, but should have minimal cluster privileges.
- Latchkey Operator: highly privileged, but off request path.
- MCP tool servers: semi-trusted adapters; isolated; no cluster API access; only have access to their own secrets and egress.

### 4.3 Data flow summary

1. Client authenticates (via OIDC or local service account secret) and requests an access token for specific tool scopes.
2. Gateway issues or accepts a short-lived access token.
3. Client makes MCP request to gateway.
4. Gateway validates token, enforces tool, method, and payload guardrails, routes to MCP server.
5. MCP server calls upstream services using server-held credentials.
6. Gateway logs and emits telemetry.

---

## 5. Identity and Authentication

### 5.1 Supported auth modes

**Mode A: OIDC / OAuth2 (recommended)**
- External IdP such as Keycloak, Auth0, and so on.
- Supports:
  - service-to-service via client_credentials
  - optional human login flows (not required)
- Latchkey Gateway acts as a resource server (validates JWT) and optionally as a token exchanger (see 5.2).

**Mode B: Gateway-managed service accounts**
- Latchkey Gateway stores hashed client secrets for `LatchkeyPrincipal` resources.
- Clients obtain tokens by presenting client_id + client_secret to the gateway.
- This mode avoids external IdP dependencies but increases responsibility in this project.

The system should support Mode A and may support Mode B. Mode A is preferred for off-the-shelf security.

### 5.2 Token models

Two models are supported; implement at least one:

**Model 1: Direct JWT from IdP**
- Client uses client_credentials to obtain JWT from IdP.
- JWT contains scopes and roles that map to tools.
- TTL is controlled by IdP.

**Model 2: Latchkey-issued short-lived capability token (recommended for 1-2 minute goal)**
- Client presents a valid IdP JWT to `/token/exchange`.
- Latchkey Gateway validates identity and policy, then issues a capability token:
  - TTL 60-120 seconds
  - scope limited to tool(s) requested
  - includes replay protection (`jti`)
  - optionally bound to a single request hash
- Client uses capability token to perform the MCP call.

This model achieves the single tool request intent without making Latchkey a full identity provider.

### 5.3 Token format and claims

Capability token should be JWT (HS256 or RS256) with:
- `iss`: gateway identifier
- `aud`: `latchkey-gateway`
- `sub`: principal id
- `iat`, `exp` (<= 120s)
- `jti`: unique id for replay prevention
- `scope`: list of allowed tool scopes (fine-grained)
- `tool`: optional single tool name restriction
- `req`: optional request binding hash (sha256 over canonicalized request)

Latchkey Gateway must reject:
- expired tokens
- wrong audience
- revoked principals
- replayed `jti`
- scope mismatches

---

## 6. Authorization and Policy

### 6.1 Scope model

A tool scope is the primary authorization unit:
- `tools:<toolName>:call`
- `tools:<toolName>:read`
- `tools:<toolName>:write`
- `tools:<toolName>:admin` (discouraged)
- optional operation-level scopes: `tools:<toolName>:op:<operation>`

Policies map principals to scopes and constraints:
- tool allowlist
- allowed operations and methods
- parameter schema constraints
- rate limits and quotas
- maximum payload sizes

### 6.2 Authorization checks (mandatory)

For every MCP request:
- Verify identity token or capability token.
- Resolve tool to backend MCP server mapping.
- Enforce:
  - tool allowlist
  - operation allowlist (if applicable)
  - schema validation for params
  - method restrictions (for example, no destructive operations)
  - concurrency and rate limit

### 6.3 Break-glass model (recommended)

Provide a policy tier for dangerous operations:
- Requires a different scope group
- Optional second factor / approval out of band (integrate via webhook)
- All break-glass usage must be highly audited

---

## 7. Secret Distribution

### 7.1 Secret provider interface

Latchkey Operator supports `SecretSource` backends:
- KubernetesSecret
  - References a Secret key in a namespace.
  - Compatible with SealedSecrets.
- VaultSecret (optional)
  - References a Vault path + key.
  - Operator or sidecar injects secret into tool server pod.
  - Prefer short-lived credentials when upstream supports them.

### 7.2 Binding secrets to tool servers

Secrets must be bound only to MCP tool server workloads, never to agents.

Mechanisms:
- Mount as environment variables or files into MCP server pod.
- Optionally inject via initContainer or CSI.
- Ensure:
  - Secrets are not logged
  - File permissions are restrictive
  - Pod specs prevent hostPath mounts and privilege escalation

---

## 8. Kubernetes CRDs

### 8.1 `LatchkeyServer` (CRD)

Represents a deployable MCP server instance (tool adapter).

**Spec fields (illustrative):**
- `image`: container image
- `replicas`: desired replicas
- `transport`: `http` | `stdio` (stdio implies a sidecar and proxy pattern)
- `servicePort`: port exposed for gateway routing
- `resources`: cpu and memory
- `securityContext`: baseline hardening options
- `egressPolicy`: allowed egress destinations (CIDR and hostnames via egress proxy)
- `secrets`: list of `LatchkeySecretBindingRef`
- `health`: readiness and liveness probes
- `metadata`: labels and annotations

**Status fields:**
- `readyReplicas`
- `endpoints`: service DNS and ports
- `conditions`: Ready, Degraded, and so on

### 8.2 `LatchkeyTool` (CRD)

Declares a tool that clients can invoke, and maps it to a LatchkeyServer.

**Spec fields:**
- `toolName`: stable public name
- `serverRef`: target LatchkeyServer
- `toolSelector`: mapping to MCP method and tool name on server
- `operations`: optional operation list, with:
  - `opName`
  - `allowed`: true and false
  - `schema`: JSON schema for params
  - `risk`: read | write | destructive
- `limits`:
  - `maxPayloadBytes`
  - `maxResponseBytes`
  - `timeoutMs`
  - `rateLimit`: requests per minute
- `audit`:
  - `redactionRules` (fields to hash and redact)
- `visibility`: which principals and groups can discover the tool (optional)

### 8.3 `LatchkeyPrincipal` (CRD)

Represents an agent and client identity recognized by Latchkey.

**Spec fields:**
- `principalId`: stable identifier
- `authMode`: `oidc` | `gateway-secret`
- OIDC:
  - `issuer`
  - `subjectMatch` or `clientId`
  - optional claim mappings (group and role)
- Gateway-secret:
  - `clientId`
  - `secretRef` (Kubernetes Secret; stored hashed at rest by gateway if possible)
- `policyRefs`: list of Policy bindings
- `enabled`: bool
- `tokenPolicy`:
  - `capabilityTokensEnabled`: bool
  - `capabilityTTLSeconds`: [60..120]
  - `requireRequestBinding`: bool
  - `allowToolDiscovery`: bool

### 8.4 `LatchkeyPolicy` (CRD)

Binds principals and groups to scopes and constraints.

**Spec fields:**
- `subjects`: principal ids or OIDC claim selectors
- `scopes`: list of tool scopes
- `constraints`:
  - allowed operations subset
  - per-scope rate limits
  - time windows
  - max concurrency
- `breakGlass`: bool
- `auditLevel`: normal | verbose

---

## 9. Latchkey Gateway API Surface

The gateway exposes two categories: auth and token, and MCP proxy.

### 9.1 Token exchange endpoints

- `POST /v1/token/exchange`
  - Input: bearer token from IdP + requested scopes + optional tool and op.
  - Output: short-lived capability token.
  - Must enforce:
    - principal enabled
    - requested scopes subset of allowed
    - TTL bounds
  - Must log issuance (audit).

- `POST /v1/token/introspect` (optional)
  - For debugging and policy evaluation visibility.
  - Must require admin scope and redact sensitive claims.

### 9.2 Tool discovery endpoints (optional, guarded)

- `GET /v1/tools`
  - Returns tools visible to principal.
  - Should be optional or restricted to reduce tool enumeration risk.

- `GET /v1/tools/{toolName}`
  - Returns schema and limits (sanitized).

### 9.3 MCP proxy endpoints

- `POST /v1/mcp`
  - Generic MCP request router. Body is MCP request.
  - Gateway determines destination tool server based on request tool name.
  - Enforces authz, size and time limits, and validation.
  - Returns MCP response.

- `POST /v1/mcp/servers/{serverName}`
  - Debug and diagnostic direct route (admin only).

### 9.4 Admin endpoints

- `GET /healthz`, `GET /readyz`
- `GET /metrics` (Prometheus)

No imperative create tool APIs are required if CRDs are the source of truth.

---

## 10. Routing and Dispatch

### 10.1 Service discovery
Latchkey Gateway routes to tool servers using:
- Kubernetes DNS service name (ClusterIP)
- optionally endpoint slices for L7 load balancing

### 10.2 Session affinity
If MCP transport requires session stickiness, gateway must support:
- `session_id` in request metadata
- consistent hashing to route same session to same server instance

### 10.3 Failure behavior
- Retries: default off for non-idempotent operations
- Circuit breakers per tool server
- Timeouts per tool (default conservative)
- Backpressure and max inflight requests per principal

---

## 11. Guardrails and Hardening

### 11.1 Network controls
- Agents should have egress restricted to only Latchkey Gateway.
- Latchkey Gateway should have egress only to tool servers and IdP endpoints.
- Tool servers should have egress only to upstream APIs they need.

### 11.2 Runtime controls
- Run as non-root
- Drop Linux capabilities
- readOnlyRootFilesystem where possible
- deny hostPath, hostNetwork, privileged
- set resource limits to prevent DoS

### 11.3 Request controls
For each tool:
- max payload bytes
- max response bytes
- strict JSON schema validation on params
- disallow unknown fields by default
- enforce timeouts per operation
- rate limit per principal + per tool

### 11.4 Replay and misuse prevention
For capability tokens:
- store `jti` in a replay cache for `exp + skew`
- reject repeats
- optionally bind token to request hash

### 11.5 Tool risk partitioning
Classify operations:
- read: safe default
- write: allowed with constraints
- destructive: off by default; break-glass scope only

### 11.6 Sensitive output handling
- Redact known secret patterns in logs.
- Allow per-tool redaction rules.
- Consider response filtering policies (for example, block returning credentials).

---

## 12. Auditing

### 12.1 Audit event model
Every request produces an audit event:

Fields (structured JSON):
- `timestamp`
- `principal_id`
- `auth_mode` (oidc and capability)
- `token_jti` (if capability)
- `tool_name`
- `operation` (if applicable)
- `request_id` (gateway-generated)
- `session_id` (if present)
- `decision` (allow and deny)
- `deny_reason` (if denied)
- `latency_ms`
- `backend_server`
- `backend_latency_ms`
- `status` (success and error)
- `error_class` (timeout, validation, backend, and so on)
- `request_params_redacted` (or hashed)
- `response_summary` (size, schema version)

### 12.2 Audit sinks
Audit events must be emitted to:
- stdout (for cluster log collectors), and or
- a pluggable sink interface:
  - HTTP webhook
  - file
  - syslog
  - OpenTelemetry logs (preferred)

This spec defines the event format; integrations are out of scope.

---

## 13. Observability

### 13.1 Metrics (Prometheus-compatible)
Expose `/metrics` with:
- `requests_total{tool,principal,decision}`
- `request_latency_ms_bucket{tool}`
- `backend_latency_ms_bucket{server}`
- `rate_limited_total{principal,tool}`
- `validation_fail_total{tool}`
- `token_exchange_total{principal}`
- `replay_reject_total`
- `inflight_requests{principal}`

### 13.2 Tracing (OpenTelemetry)
Emit spans for:
- token exchange
- policy evaluation
- MCP dispatch
- backend call

Propagation:
- support W3C traceparent headers.

### 13.3 Health endpoints
- `/healthz` liveness
- `/readyz` readiness (checks: policy loaded, replay cache reachable if enabled)

---

## 14. Deployment Model

### 14.1 Required components (dedicated to this service)
- Latchkey Operator deployment
- Latchkey Gateway deployment
- CRDs installed cluster-wide

### 14.2 Optional dependencies (bundled manifests acceptable)
- Redis (or compatible) for replay cache
- Postgres only if required for state; prefer CRD source of truth

The project should provide Helm and or Kustomize:
- `charts/latchkey`
- `deploy/kustomize/base` and `deploy/kustomize/overlays/*`

### 14.3 External dependencies (integration-only)
- OIDC provider (Keycloak, Auth0, and so on): treated as external; define OIDC boundary only.
- Metrics and tracing backends: integration-only.

---

## 15. Security Threat Model (Summary)

### 15.1 Threats addressed
- Prompt injection leading to:
  - credential exfiltration (prevented by server-side secrets)
  - unauthorized tool calls (policy enforcement + scopes)
  - destructive actions (risk partitioning + break-glass)
- Token theft:
  - short TTL
  - replay protection (jti)
  - optional request binding
- Lateral movement:
  - network policy and egress allowlists
  - tool server isolation
- Abuse and DoS:
  - rate limiting, payload limits, timeouts
  - resource limits

### 15.2 Threats not fully solved
- If a permitted tool is dangerous, the agent can still misuse it.
  - Mitigation: granular operations + schema constraints + approvals for destructive actions.
- Compromise of operator:
  - mitigated by separating operator from request path; still high impact.
- Vulnerable tool servers:
  - treat as untrusted; sandbox; minimal privileges; version pinning.

---

## 16. Roadmap (Suggested Phases)

**Phase 1 (MVP)**
- CRDs: LatchkeyServer, LatchkeyTool, LatchkeyPrincipal, LatchkeyPolicy
- Operator deploys tool servers and publishes routing config
- Gateway routes MCP calls, enforces allowlist + schema + rate limits
- Basic audit logs and metrics
- Kubernetes Secret-based secret binding (SealedSecrets compatible)

**Phase 2**
- Capability token exchange with 1-2 minute TTL + jti replay cache
- Tool discovery endpoint (guarded)
- Vault SecretSource
- OTel tracing

**Phase 3**
- Request binding to token (single-call tokens)
- Break-glass workflow hooks
- Advanced policy expressions (OPA integration)

---

## 17. Acceptance Criteria

1. Agents can only reach Latchkey Gateway; gateway can reach tool servers; tool servers can reach only required upstreams.
2. Agents never have direct access to upstream API keys.
3. A principal with no scope cannot invoke any tool; attempts are denied and audited.
4. Capability tokens (if enabled) expire within configured TTL and cannot be replayed.
5. Every tool call yields an audit event with the defined schema.
6. Metrics expose request counts, latency, deny reasons, and rate-limit behavior.
7. Tool servers are provisioned and updated declaratively via CRDs.
