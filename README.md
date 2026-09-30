# Latchkey

A lightweight, stateless MCP gateway: discover downstream tools, search locally, and route calls concurrently.

**Status: requirements baseline; the MVP is not implemented.** The old gateway/operator prototype is preserved on branch `backup/pre-mvp-reset-2026-09-30` and annotated tag `pre-mvp-reset-2026-09-30`.

[MVP requirements](docs/spec.md) is the unmodified attachment supplied on 2026-09-30. It supersedes the old specification, roadmap and ADRs.

The MVP has one binary, one OCI image, exactly `search` and `exec`, SDK-owned MCP transport, a namespaced `MCPService` CRD, Secret-backed static headers, in-memory search, concurrent execution and OpenTelemetry. Replicas operate independently.

Code Mode, OAuth delegation, per-user policy, persistence, durable events, stdio processes, admin UI and semantic search are deferred.

- [Repository and reference implementation review](docs/review.md)
- [Architecture and acceptance plan](docs/implementation-plan.md)
- [Reset and rollback record](docs/reset.md)

There is no runnable service, deployment or build pipeline in this baseline. Keep the existing Nexus path active until migration acceptance is demonstrated.
