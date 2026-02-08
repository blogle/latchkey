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
    nix build .#gateway-image .#operator-image .#tool-server-image .#upstream-stub-image

kind-up:
    kind create cluster --name latchkey

kind-load-images:
    gateway_archive="$(nix build .#gateway-image --print-out-paths --no-link)" && operator_archive="$(nix build .#operator-image --print-out-paths --no-link)" && tool_server_archive="$(nix build .#tool-server-image --print-out-paths --no-link)" && upstream_stub_archive="$(nix build .#upstream-stub-image --print-out-paths --no-link)" && kind load image-archive "$gateway_archive" --name latchkey && kind load image-archive "$operator_archive" --name latchkey && kind load image-archive "$tool_server_archive" --name latchkey && kind load image-archive "$upstream_stub_archive" --name latchkey

deploy-dev:
    kubectl apply -k deploy/kustomize/overlays/dev

deploy-kind-dev: kind-load-images deploy-dev

ci: fmt-check lint test deny
