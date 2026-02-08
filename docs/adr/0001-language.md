# ADR 0001: Rust-first implementation

## Status
Accepted

## Context
Latchkey needs low latency request handling, strict resource controls, and a compact runtime profile for Kubernetes.

## Decision
Use Rust for both gateway and operator in a single workspace.

## Consequences
- Shared language and tooling for control and data plane.
- Strong compile-time guarantees and explicit error handling.
- Slightly higher onboarding cost compared with dynamic languages.
