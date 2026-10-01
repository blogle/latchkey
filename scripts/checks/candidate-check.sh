#!/usr/bin/env bash
# Full merge-candidate gate (LATCH-3), keeping the LATCH-1 semantics:
# fmt-check, lint, test-unit, `nix flake check`, `nix build .#package .#oci`,
# and native + OCI --help/--version/serve-refusal smoke tests — plus the
# script-test suites that pr-check runs (candidate is a superset of pr-check).
#
# Candidate profile: every cargo step runs with --profile ci (opt-level 1,
# incremental off) instead of the dev profile. Release/image builds stay
# explicit (nix build below), never implicit in cargo steps. Real-cluster
# tests are opt-in via LATCHKEY_TEST_KUBECONFIG and are NOT run here.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../dev/lib.sh disable=SC1091
source "$HERE/../dev/lib.sh"

cd "$LK_ROOT"

fail() { echo "candidate-check: error: $*" >&2; exit 1; }

echo "==> fmt-check"
"$HERE/../dev/cargo.sh" fmt-check

echo "==> lint (clippy, ci profile, all targets, -D warnings)"
LATCHKEY_PROFILE=ci "$HERE/../dev/cargo.sh" lint

echo "==> script-test (python dispatcher suites)"
suites=()
for f in "$LK_ROOT"/scripts/dev/tests/test_*.py; do
  [[ -e "$f" ]] || continue
  suites+=("$(basename "$f" .py | sed 's/^test_//')")
done
if [[ "${#suites[@]}" -eq 0 ]]; then
  fail "no script-test suites found under scripts/dev/tests/"
fi
for suite in "${suites[@]}"; do
  echo "==> script-test $suite"
  "$HERE/../dev/script-test.sh" "$suite"
done

echo "==> test-unit (lib tests, ci profile)"
LATCHKEY_PROFILE=ci "$HERE/../dev/cargo.sh" test-unit

echo "==> nix flake check"
nix flake check

echo "==> nix build .#package .#oci"
# nix names multi-installable out-links: result (first) and result-1
# (second), in argument order.
nix build .#package .#oci
if [[ ! -e result || ! -e result-1 ]]; then
  fail "expected nix to produce result (package) and result-1 (image) out-links"
fi
native_bin="result/bin/latchkey"
image_tar="result-1"

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
# Extracted store paths are read-only (as in the nix store); make them
# writable again so cleanup succeeds.
trap 'chmod -R u+w "$smoke_dir" 2>/dev/null || true; rm -rf "$smoke_dir"' EXIT
mkdir -p "$smoke_dir/layers" "$smoke_dir/rootfs"

if tar -tzf "$image_tar" >/dev/null 2>&1; then
  tar -xzf "$image_tar" -C "$smoke_dir/layers"
else
  tar -xf "$image_tar" -C "$smoke_dir/layers"
fi

manifest="$smoke_dir/layers/manifest.json"
[[ -f "$manifest" ]] || fail "image archive has no manifest.json"
# Manifests are pretty-printed; each layer reference sits on its own
# line and always ends in layer.tar.
layers="$(grep -oE '"[^"]+layer\.tar"' "$manifest" | tr -d '"' || true)"
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

echo "candidate-check: all gates passed (fmt-check, lint[ci], script-test, test-unit[ci], nix flake check, package+OCI build, native+OCI smoke)"
