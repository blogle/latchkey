# CRDs

This repository defines four initial CRDs under `latchkey.dev/v1alpha1`:

## LatchkeyServer
- Describes a deployable MCP adapter workload.
- Key fields: image, replicas, transport, service settings, secret bindings, and health probes.

## LatchkeyTool
- Maps public tool names to server backends.
- Key fields: operation metadata, schema constraints, timeout, payload limits, and audit redaction hints.

## LatchkeyPrincipal
- Defines agent or client identity mapping.
- Key fields: auth mode, identity selectors, enablement, policy refs, and capability token policy.

## LatchkeyPolicy
- Binds subjects to scopes with constraints.
- Key fields: scopes, operation limits, rate constraints, break-glass, and audit level.

See `deploy/kustomize/base/crds/` for bootstrap CRD manifests and `crates/operator/src/crd.rs` for Rust type scaffolding.
