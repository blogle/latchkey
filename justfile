# Latchkey developer workflow (LATCH-1 evolved by LATCH-3).
#
# Root command dispatch is FROZEN: exactly the recipes below exist. Recipes
# are thin argv-preserving front-ends over owned dispatchers in scripts/dev/*
# and scripts/checks/*; lane/suite selection is parameterized (file names +
# arguments), never by editing this file. Unimplemented/future commands fail
# closed naming their owning ticket and can never succeed.
#
# Environment model: `just setup` materializes a GC-rooted, fingerprint-keyed
# dev environment into gitignored .dev/ via `nix print-dev-env` (only when
# flake.lock + rust-toolchain.toml + Cargo manifests/lock + setup version +
# checkout path change, or on forced `just setup true`). Every other recipe
# sources the cached .dev/env and invokes absolute cached tool paths — no
# nix evaluation happens in fmt/lint/test/build recipes. `just doctor` only
# diagnoses; it reports `just setup true` as the single forced-refresh
# command. See docs/development.md.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Forward parameters as a safely quoted argv array (shebang recipes receive
# them positionally; interpolation never re-tokenizes user input).
set positional-arguments

# List the available recipes (default).
default:
    @just --list

# Re-materialize .dev/ when the fingerprint changed; `just setup true` forces.
setup refresh='false':
    #!/usr/bin/env bash
    set -euo pipefail
    exec "{{justfile_directory()}}/scripts/dev/setup.sh" "$@"

# Diagnose the cached environment (never rebuilds; reports `just setup true`).
doctor:
    "{{justfile_directory()}}/scripts/dev/doctor.sh"

# Format the Rust sources in place (pinned rustfmt).
fmt:
    "{{justfile_directory()}}/scripts/dev/cargo.sh" fmt

# Check formatting without writing.
fmt-check:
    "{{justfile_directory()}}/scripts/dev/cargo.sh" fmt-check

# Clippy over all targets with warnings denied (`LATCHKEY_PROFILE` overrides).
lint:
    "{{justfile_directory()}}/scripts/dev/cargo.sh" lint

# Fast dev-profile build (persistent profile-keyed Cargo target dir).
build:
    "{{justfile_directory()}}/scripts/dev/cargo.sh" build

# Lib tests: `just test-unit` (all), `just test-unit <lane>` -> <lane>:: tests.
test-unit lane='':
    #!/usr/bin/env bash
    set -euo pipefail
    exec "{{justfile_directory()}}/scripts/dev/cargo.sh" test-unit "$@"

# Integration suite: `just test-integration contracts -- --nocapture`.
test-integration suite *args:
    #!/usr/bin/env bash
    set -euo pipefail
    exec "{{justfile_directory()}}/scripts/dev/cargo.sh" test-integration "$@"

# Python unittest suite for the dispatcher scripts (`just script-test dispatch`).
script-test suite:
    #!/usr/bin/env bash
    set -euo pipefail
    exec "{{justfile_directory()}}/scripts/dev/script-test.sh" "$@"

# Fast gate: fmt-check + lint + script-test + test-unit (no OCI/flake/E2E).
pr-check:
    "{{justfile_directory()}}/scripts/checks/pr-check.sh"

# Full gate: pr-check + nix flake check + package/OCI builds + smoke (ci profile).
candidate-check:
    "{{justfile_directory()}}/scripts/checks/candidate-check.sh"

# Build the OCI image (static musl binary + CA roots).
image:
    nix build .#oci --out-link result-oci

# Benchmarks: not built yet (owning ticket below).
bench suite:
    #!/usr/bin/env bash
    set -euo pipefail
    exec "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-8 bench "$@"

# Dev-cluster helpers: not built yet (owning ticket below).
dev:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-20 dev

# Reload dev-cluster helpers: not built yet (owning ticket below).
dev-reload:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-20 dev-reload

# Stop dev-cluster helpers: not built yet (owning ticket below).
dev-stop:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-20 dev-stop

# Generate CRDs: not built yet (owning ticket below).
generate-crd:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-16 generate-crd

# Plan a release for a commit: not built yet (owning ticket below).
release-plan sha:
    #!/usr/bin/env bash
    set -euo pipefail
    exec "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-4 release-plan "$@"

# Publish a release: not built yet (owning ticket below).
release-publish sha:
    #!/usr/bin/env bash
    set -euo pipefail
    exec "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-7 release-publish "$@"

# Verify a published release: not built yet (owning ticket below).
release-verify sha:
    #!/usr/bin/env bash
    set -euo pipefail
    exec "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-7 release-verify "$@"

# Foundation audit: not built yet (owning ticket below).
foundation-audit:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-8 foundation-audit

# Standalone E2E (real processes/HTTP, no cluster): not built yet (owning ticket below).
e2e-standalone:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-21 e2e-standalone

# Render Kubernetes manifests: not built yet (owning ticket below).
k8s-render:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-24 k8s-render

# Create the opt-in test cluster: not built yet (owning ticket below).
k8s-test-up:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-24 k8s-test-up

# Run cluster tests (LATCHKEY_TEST_KUBECONFIG opt-in): not built yet (owning ticket below).
k8s-test-run:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-24 k8s-test-run

# Destroy the opt-in test cluster: not built yet (owning ticket below).
k8s-test-down:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-24 k8s-test-down

# MVP acceptance run: not built yet (owning ticket below).
mvp-acceptance:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-28 mvp-acceptance

# Migration preflight (M01): not built yet (owning ticket below).
migration-preflight:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-29 migration-preflight

# Migration rehearsal (M01): not built yet (owning ticket below).
migration-rehearse:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-29 migration-rehearse

# Migration cutover (M02): not built yet (owning ticket below).
migration-cutover:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-30 migration-cutover

# Migration verification (M02): not built yet (owning ticket below).
migration-verify:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-30 migration-verify

# Migration rollback (M02): not built yet (owning ticket below).
migration-rollback:
    "{{justfile_directory()}}/scripts/dev/future.sh" LATCH-30 migration-rollback

# Remove selected local outputs only (.dev/, target/, result*, logs); keeps caches.
clean:
    "{{justfile_directory()}}/scripts/dev/clean.sh"
