# Latchkey developer workflow (LATCH-1).
#
# Every Rust invocation is routed through the pinned Nix toolchain
# (`nix develop -c cargo ...`); `just` itself comes from the locked nixpkgs
# graph (devShell, or `nix run .#bootstrap` which prepends the locked just).
# Recipes map to real commands only: anything not defined here fails closed
# with just's own "justfile does not contain recipe" error.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the available recipes.
default:
    @just --list

# Warm the environment and record a toolchain/lock fingerprint in the
# gitignored .setup-marker. Run once per toolchain or lock change; this is
# what the nix bootstrap entry invokes.
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"

    for f in rust-toolchain.toml flake.lock Cargo.lock; do
      if [[ ! -f "$f" ]]; then
        echo "setup: error: missing $f" >&2
        exit 1
      fi
    done

    fingerprint="$(cat rust-toolchain.toml flake.lock Cargo.lock | sha256sum | cut -d' ' -f1)"

    echo "setup: verifying the pinned toolchain (first run builds/warms it)..."
    toolchain_report="$(nix develop -c bash -c 'rustc --version && cargo --version && cargo fmt --version && cargo clippy --version && just --version')"

    {
      echo "fingerprint=$fingerprint"
      echo "date=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      printf '%s\n' "$toolchain_report"
    } > .setup-marker

    echo "setup: recorded .setup-marker (fingerprint=$fingerprint)"

# Verify environment health; fails with guidance when setup is missing or
# stale (toolchain/lock files changed since `just setup`).
doctor:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"

    fail() {
      echo "doctor: error: $*" >&2
      echo "doctor: fix: run 'just setup' (or 'nix run .#bootstrap'), once per toolchain/lock change" >&2
      exit 1
    }

    for f in rust-toolchain.toml flake.lock Cargo.lock; do
      [[ -f "$f" ]] || fail "missing $f"
    done

    expected="$(cat rust-toolchain.toml flake.lock Cargo.lock | sha256sum | cut -d' ' -f1)"
    [[ -f .setup-marker ]] || fail "no .setup-marker: setup has not been run"

    recorded="$(sed -n 's/^fingerprint=//p' .setup-marker)"
    [[ -n "$recorded" ]] || fail ".setup-marker is malformed (no fingerprint line)"
    if [[ "$recorded" != "$expected" ]]; then
      echo "doctor: recorded fingerprint: $recorded" >&2
      echo "doctor: current fingerprint:  $expected" >&2
      fail "setup marker is stale: rust-toolchain.toml/flake.lock/Cargo.lock changed since setup"
    fi

    nix develop -c bash -c 'rustc --version && cargo --version' >/dev/null \
      || fail "the pinned toolchain is not usable via 'nix develop'"

    echo "doctor: environment healthy (fingerprint=$recorded)"
    sed 's/^/doctor:   /' .setup-marker

# Check formatting with the pinned rustfmt.
fmt-check:
    nix develop -c cargo fmt --all -- --check

# Run clippy with warnings denied (dev profile).
lint:
    nix develop -c cargo clippy --all-targets --locked -- -D warnings

# Run the unit tests (dev/test profile; tests always unwind).
test-unit:
    nix develop -c cargo test --all-targets --locked

# Fast local development build (dev profile).
build:
    nix develop -c cargo build --locked

# Build the OCI image (static musl binary + CA roots).
image:
    nix build .#oci --out-link result-oci

# Cheap pull-request gate: formatting, lints, and unit tests.
pr-check: fmt-check lint test-unit

# Full merge-candidate gate: everything pr-check runs, plus flake checks,
# package + image builds, and native/OCI smoke tests.
candidate-check: fmt-check lint test-unit
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"

    fail() { echo "candidate-check: error: $*" >&2; exit 1; }

    echo "==> nix flake check"
    nix flake check

    echo "==> nix build .#package .#oci"
    nix build .#package .#oci
    if [[ ! -e result || ! -e result-2 ]]; then
      fail "expected nix to produce result (package) and result-oci (image) out-links"
    fi
    native_bin="result/bin/latchkey"
    image_tar="result-2"

    echo "==> native smoke: --help / --version / serve must fail"
    [[ -x "$native_bin" ]] || fail "$native_bin missing or not executable"

    version_line="$("$native_bin" --version)" || fail "native --version failed"
    expected_version="latchkey $(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)"
    [[ "$version_line" == "$expected_version" ]] \
      || fail "native --version printed '$version_line', want '$expected_version'"

    help_out="$("$native_bin" --help)" || fail "native --help failed"
    grep -q "not implemented" <<<"$help_out" \
      || fail "native --help must state the gateway is not implemented"

    if serve_out="$("$native_bin" serve 2>&1)"; then
      fail "'latchkey serve' unexpectedly succeeded: $serve_out"
    fi
    grep -q "the gateway is not implemented" <<<"$serve_out" \
      || fail "serve refusal must state the gateway is not implemented"
    echo "native serve refused with: $(head -n1 <<<"$serve_out")"

    echo "==> OCI smoke: extract image layers, run the static binary directly"
    smoke_dir="$(mktemp -d)"
    trap 'rm -rf "$smoke_dir"' EXIT
    mkdir -p "$smoke_dir/layers" "$smoke_dir/rootfs"

    if tar -tzf "$image_tar" >/dev/null 2>&1; then
      tar -xzf "$image_tar" -C "$smoke_dir/layers"
    else
      tar -xf "$image_tar" -C "$smoke_dir/layers"
    fi

    manifest="$smoke_dir/layers/manifest.json"
    [[ -f "$manifest" ]] || fail "image archive has no manifest.json"
    layers="$(sed -n 's/.*"Layers":\[\(.*\)\].*/\1/p' "$manifest" | tr ',' '\n' | tr -d '"')"
    [[ -n "$layers" ]] || fail "could not parse layer list from manifest.json"
    while IFS= read -r layer; do
      [[ -f "$smoke_dir/layers/$layer" ]] || fail "missing layer $layer"
      tar -xf "$smoke_dir/layers/$layer" -C "$smoke_dir/rootfs" 2>/dev/null \
        || tar -xzf "$smoke_dir/layers/$layer" -C "$smoke_dir/rootfs"
    done <<<"$layers"

    # The entrypoint must be wired at /bin/latchkey.
    [[ -e "$smoke_dir/rootfs/bin/latchkey" || -L "$smoke_dir/rootfs/bin/latchkey" ]] \
      || fail "image root has no /bin/latchkey entrypoint"

    img_bin="$(find "$smoke_dir/rootfs/nix/store" -path '*/bin/latchkey' -type f 2>/dev/null | head -n1 || true)"
    [[ -n "$img_bin" ]] || fail "latchkey binary not found in image layers"
    [[ -x "$img_bin" ]] || fail "extracted latchkey binary is not executable"

    # Static proof: a dynamically linked binary would embed an ld.so loader
    # path; the musl image binary must not.
    if grep -qa "ld-linux" "$img_bin"; then
      fail "OCI binary references ld-linux: it is not a static musl binary"
    fi

    img_version="$("$img_bin" --version)" || fail "OCI --version failed"
    [[ "$img_version" == "$expected_version" ]] \
      || fail "OCI --version printed '$img_version', want '$expected_version'"

    img_help="$("$img_bin" --help)" || fail "OCI --help failed"
    grep -q "not implemented" <<<"$img_help" \
      || fail "OCI --help must state the gateway is not implemented"

    if img_serve="$("$img_bin" serve 2>&1)"; then
      fail "OCI 'serve' unexpectedly succeeded: $img_serve"
    fi
    grep -q "the gateway is not implemented" <<<"$img_serve" \
      || fail "OCI serve refusal must state the gateway is not implemented"

    echo "candidate-check: all gates passed"
