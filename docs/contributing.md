# Contributing

## Workflow

1. Start from the spec and ADRs before code changes.
2. Keep PRs narrow and auditable.
3. Document behavior changes in `docs/spec.md` or ADRs.

## Local setup

1. Install Nix and enable flakes.
2. Enter dev shell:
   - `nix develop`
3. Run quality gates:
   - `just ci`

## Kubernetes loop

- Apply base and dev overlay:
  - `kubectl apply -k deploy/kustomize/overlays/dev`

## Required checks before PR

- `just fmt-check`
- `just lint`
- `just test`
- `just deny`
