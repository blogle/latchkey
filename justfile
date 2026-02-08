set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

build:
    cargo build --workspace

deny:
    cargo deny check

image:
    nix build .#gateway-image .#operator-image

kind-up:
    kind create cluster --name latchkey

deploy-dev:
    kubectl apply -k deploy/kustomize/overlays/dev

ci: fmt-check lint test deny
