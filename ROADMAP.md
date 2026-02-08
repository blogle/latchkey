# Latchkey Roadmap (Living)

This is a **living roadmap**. It will change as we learn. The goal is to keep it **actionable, current, and honest**.

## How to use this document
- **One source of truth:** if work is planned, it lives here (or is linked from here).
- **Work in small increments:** ship vertical slices, not “layers.”
- **Bias to end-to-end:** every phase should result in a runnable system.
- **Always record decisions:** add short Decision Notes under the milestone where the decision was made.
- **Keep it crisp:** prefer checklists and acceptance criteria over prose.

## Formatting rules
- Each milestone has:
  - **Goal** (1–2 lines)
  - **Deliverables** (bullets)
  - **Acceptance Criteria** (verifiable checks)
- Use task lists (`- [ ]`) for work items.
- When a milestone is “done,” mark its acceptance criteria as satisfied and add a **Completion Note** (1–3 bullets).
- No diagrams in this file. Put diagrams in `docs/architecture.md` and link them.

## Agent operating rules
- **Spec first, impl second.** If behavior isn’t specified, add a short spec note or ADR before coding.
- **Do not expand scope silently.** If you discover missing requirements, add them as a new milestone or task.
- **Security posture is default-deny.** If you loosen a constraint for dev, it must be isolated to the dev overlay.
- **Performance matters.** Choose bounded queues, timeouts, size limits, and low overhead defaults from day one.

---

# Roadmap

## Milestone 0 — Project Skeleton and Build System
**Goal:** A clean repo that builds, tests, and deploys stubs with Nix.

**Deliverables**
- Workspace layout (`crates/core`, `crates/gateway`, `crates/operator`)
- `flake.nix` devshell + reproducible builds + OCI images
- Docs skeleton: `AGENTS.md`, `README.md`, `docs/*`, ADR folder
- CI: fmt, clippy, tests, dependency checks
- Kustomize base + dev overlay

**Acceptance Criteria**
- [x] `nix develop` works on a fresh clone
- [x] `just fmt lint test build` passes
- [x] `nix build` outputs binaries and OCI images
- [x] kustomize deploy brings up stub gateway/operator pods

**Completion Note**
- Default `nix build` now emits a single bootstrap bundle containing both binaries and OCI images.
- Dev overlay now rewrites image references to locally built images (`latchkey-*:dev`) for in-cluster bootstrap.
- Kind runbook is wired through `just kind-up`, `just kind-load-images`, and `just deploy-dev`.

---

## Milestone 1 — First End-to-End Flow (Thin Slice)
**Goal:** A working e2e request path through gateway → tool server → upstream stub, with auth, audit, and policy **minimal but real**.

This is intentionally not feature complete. It’s the skeleton we iterate on.

**Deliverables**
- Minimal MCP proxy path in gateway: `POST /v1/mcp` forwarding to a single registered tool server
- A single example MCP tool server (in-cluster) that calls a stub upstream service
- Minimal identity model:
  - either **static gateway-secret principal** for MVP
  - or **OIDC JWT validation** if easy to wire early
- Minimal authorization:
  - tool allowlist by principal (hard-coded config or CRD-backed)
- Structured audit event emitted for every call (allow/deny)
- Basic guardrails:
  - request size limit
  - timeout
  - rate limit (coarse)

**Acceptance Criteria**
- [ ] From a pod in-cluster: call gateway with an auth token and successfully invoke a tool
- [ ] A principal without permission is denied and an audit event shows `decision=deny`
- [ ] Gateway logs include `request_id`, `principal_id`, `tool_name`, latency, outcome
- [ ] The tool server can access its secret; the client cannot retrieve it via gateway response
- [ ] Load test: sustained calls do not exceed a small fixed CPU/RSS budget (record baseline)

**Decision Notes**
- (add as decisions are made: auth MVP mode, tool routing config source, etc.)

---

## Milestone 2 — CRDs and Operator: Declarative Tool Servers and Routing
**Goal:** CRDs are the source of truth. Operator reconciles tool servers and publishes routing metadata consumed by the gateway.

**Deliverables**
- CRDs implemented and versioned:
  - `LatchkeyServer`, `LatchkeyTool`, `LatchkeyPrincipal`, `LatchkeyPolicy`
- Operator reconciliation for:
  - `LatchkeyServer` → Deployment/Service (+ baseline SecurityContext)
  - `LatchkeyTool` → routing entry and limits metadata
- Gateway consumes routing config (ConfigMap or CRD watch/cache)
- Tight RBAC:
  - operator: only what it needs
  - gateway: read-only where possible

**Acceptance Criteria**
- [ ] Creating a `LatchkeyServer` results in a running tool server pod + Service
- [ ] Creating a `LatchkeyTool` makes the tool invokable via gateway without manual config edits
- [ ] Deleting a `LatchkeyTool` makes it non-invokable within a bounded time
- [ ] Operator does not grant tool servers cluster API access

---

## Milestone 3 — Policy Engine v1 (Scopes + Constraints)
**Goal:** Real authorization is enforced per request with stable semantics.

**Deliverables**
- Scope model implemented:
  - `tools:<tool>:call` (+ optional op-level scopes)
- Policy resolution:
  - principal → policies → allowed scopes
- Constraints (v1):
  - per-tool rate limits
  - max payload bytes
  - timeout per tool
  - allowed operation subset (if tool exposes ops)
- Schema validation:
  - JSON Schema per operation/tool
  - unknown fields denied by default

**Acceptance Criteria**
- [ ] A principal with no scopes cannot call any tool
- [ ] Schema violations fail fast with a consistent error class and audit reason
- [ ] Rate limited requests return a clear error and increment a metric counter
- [ ] Policies are testable with golden test vectors (allow/deny matrix)

---

## Milestone 4 — Capability Tokens (Short-Lived, Scoped)
**Goal:** Implement the 1–2 minute “capability token” model for autonomous agents.

**Deliverables**
- `/v1/token/exchange`:
  - validates upstream identity (OIDC JWT) or gateway-secret auth (if MVP path retained)
  - mints capability token with TTL 60–120 seconds
  - restricts to requested tool/op subset
- Capability token validation on `/v1/mcp`
- Replay protection:
  - `jti` cache with exp+skew
  - reject replayed tokens
- Optional request binding (phase-gated):
  - token includes request hash
  - gateway verifies binding

**Acceptance Criteria**
- [ ] Capability tokens expire within configured TTL and are rejected after expiration
- [ ] Replaying the same token is rejected and audited
- [ ] Requested scopes are reduced to least-privilege subset and enforced

---

## Milestone 5 — Secret Distribution v1 (Kubernetes Secrets, SealedSecrets-Friendly)
**Goal:** Secrets are only accessible to tool server pods and never to agents.

**Deliverables**
- `SecretSource: KubernetesSecret` support in `LatchkeyServer`
- Secret mount patterns:
  - env + file mount options
  - restrictive permissions
- Logging redaction rules:
  - audit logs redact configured fields
  - guard against accidental secret logging

**Acceptance Criteria**
- [ ] Tool server can use its upstream credential to call a real upstream API (or realistic stub)
- [ ] Gateway and clients cannot read secrets via any endpoint or logs
- [ ] Redaction rules are unit tested and demonstrated in integration tests

---

## Milestone 6 — Network and Runtime Hardening (Default-Deny)
**Goal:** Lock the cluster posture down so the only allowed flows are the intended ones.

**Deliverables**
- NetworkPolicy baseline:
  - agents → gateway only
  - gateway → tool servers + IdP only
  - tool servers → explicit upstream allowlist only
- Pod hardening:
  - runAsNonRoot, drop caps, no privilege escalation
  - readOnlyRootFilesystem where possible
  - resource requests/limits tuned
- Failure posture:
  - conservative timeouts
  - bounded concurrency
  - circuit breaker behavior for backend failures (if implemented)

**Acceptance Criteria**
- [ ] Policy tests confirm blocked egress paths in dev overlay (or dedicated security test env)
- [ ] Pods run with hardened security context and no privileged access
- [ ] Gateway remains stable under malformed/oversized requests (no OOM)

---

## Milestone 7 — Observability and Audit Sinks (Production Usable)
**Goal:** Operators can answer “who did what, when, and why” without guesswork.

**Deliverables**
- Structured audit events with stable schema
- Pluggable audit sink interface:
  - stdout required
  - webhook or OTel logs optional
- Metrics:
  - request counts, latencies, deny reasons, rate limits, replay rejects
- Tracing boundary:
  - spans for token exchange and mcp dispatch
  - W3C propagation support

**Acceptance Criteria**
- [ ] Every request generates exactly one audit event with required fields
- [ ] Metrics endpoint exposes required counters/histograms
- [ ] Traces show gateway → tool server timing (at least internally)

---

## Milestone 8 — Tool Risk Partitioning + Break-Glass
**Goal:** Dangerous tools are gated and auditable with explicit escalation.

**Deliverables**
- Tool risk classification: read/write/destructive
- Break-glass policy tier:
  - separate scopes
  - optional webhook approval hook (integration boundary only)
- High-visibility audit mode for break-glass usage

**Acceptance Criteria**
- [ ] Destructive ops are denied by default
- [ ] Break-glass scope allows destructive ops and produces elevated audit entries
- [ ] Approval hook can block or permit (stubbed ok)

---

## Milestone 9 — Packaging, Versioning, and Upgrade Story
**Goal:** A real open-source consumable with safe upgrades and stable APIs.

**Deliverables**
- Versioned CRDs (with upgrade notes)
- Helm chart or polished kustomize packaging (pick one primary)
- Release process:
  - changelog
  - container tags
  - compatibility matrix (CRD version ↔ gateway/operator)

**Acceptance Criteria**
- [ ] A user can install/upgrade from a tagged release with documented steps
- [ ] CRD changes are clearly communicated and backward-compat tracked

---

## Milestone 10 — Completion Criteria (v1.0)
**Goal:** Latchkey meets the spec’s acceptance criteria end-to-end.

**Deliverables**
- Feature set matches spec MVP+ phases (as chosen)
- Security model validated against the threat summary
- Documentation is complete and consistent with behavior

**Acceptance Criteria**
- [ ] Agents only reach gateway; gateway routes to tool servers; tool servers reach only required upstreams
- [ ] No upstream credentials are exposed to agents
- [ ] Unauthorized calls are denied and audited
- [ ] Capability tokens are short-lived and replay-protected (if enabled)
- [ ] Every tool call yields an audit event and relevant metrics
- [ ] Tool servers are provisioned/updated declaratively via CRDs

---

# Backlog (Unscheduled)
- Vault SecretSource
- OPA / external policy engine integration
- mTLS (gateway↔tool servers) as defense-in-depth
- Request-binding tokens (PoP-style) enforcement
- Tool discovery endpoints (`/v1/tools`) with strict visibility control
- Multi-tenant namespace mode (stronger isolation)
- Fuzzing harness for gateway request parsing
- Performance profiling + hard budgets per request path
