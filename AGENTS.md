# Latchkey Agent Guidelines

## Workflow
- Follow a spec-first approach: update docs and ADRs before implementation details.
- Keep stubs small, bounded, and easy to audit.
- Prefer deterministic tooling from `flake.nix`.

## Security posture
- Deny by default for authz, network policy, and endpoint exposure.
- Never add plaintext secrets, API keys, or PII to git.
- Keep gateway credentials scoped and short lived.

## Required checks before PR
- `just fmt-check`
- `just lint`
- `just test`
- `just deny`

## Repository rules
- Do not commit generated exports or local state files.
- Keep new dependencies minimal and document why they are needed.
- Align behavioral changes with `docs/spec.md` and ADRs.
