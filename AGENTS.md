# Latchkey agent instructions

- Treat `docs/spec.md` as the authoritative MVP requirements. Do not silently weaken acceptance criteria or import the archived architecture.
- Follow a spec-first workflow. Record implementation decisions separately from the supplied requirements.
- Build one service/image with an embedded reconciler and exactly `search` and `exec`.
- The same binary must offer standalone mode that never contacts or mocks Kubernetes. Local and Kubernetes configuration feed the same reconciler and execution paths. Keep real-binary, real-MCP-client, real-downstream end-to-end tests in CI without a cluster.
- Use official rmcp for the wire protocol. Prove current and preceding-era compatibility and concurrent JSON/SSE execution before choosing a shared-client design.
- Keep `Catalog::search` and `Router::exec` independent of MCP handlers. Never hold global locks across downstream I/O.
- No Code Mode, OAuth broker, per-user policy, database, semantic search, durable events, stdio supervision or separate operator in MVP.
- Never log credentials, headers, arbitrary tool arguments or raw sensitive transport errors. CRDs contain Secret references only.
- No automatic mutation replay. Make SDK retry/reinitialization behavior explicit.
- Do not commit local state or generated review exports. Minimize dependencies and retain attribution for any future external code adoption.
- Before implementation PRs, provide formatting, lint, tests, dependency-policy and build checks using the chosen reproducible toolchain. Run protocol/concurrency checks on `master` and PRs.
- This baseline has no Cargo project or executable checks; do not report runtime tests as passing.
