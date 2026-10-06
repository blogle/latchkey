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

echo "==> native smoke: --help / --version"
[[ -x "$native_bin" ]] || fail "$native_bin missing or not executable"

version_line="$("$native_bin" --version)" || fail "native --version failed"
expected_version="latchkey $(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)"
[[ "$version_line" == "$expected_version" ]] \
  || fail "native --version printed '$version_line', want '$expected_version'"

help_out="$("$native_bin" --help)" || fail "native --help failed"
grep -q "USAGE" <<<"$help_out" || fail "native --help must include usage"

echo "==> OCI image contract"
"$HERE/image-contract.sh" "$image_tar"
"$HERE/image-footprint.sh" "$image_tar"

echo "candidate-check: all gates passed (fmt-check, lint[ci], script-test, test-unit[ci], nix flake check, package+OCI image contract)"
