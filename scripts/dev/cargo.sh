#!/usr/bin/env bash
# Cargo command dispatcher for the frozen just surface (LATCH-3).
#
#   cargo.sh fmt | fmt-check | lint | build
#   cargo.sh test-unit [lane]         -> cargo test --locked --lib <lane>::
#   cargo.sh test-integration <suite> [harness args...]
#                                       -> cargo test --locked --test <suite>
#
# All invocations go through the absolute cached toolchain recorded in
# .dev/env with a profile-keyed persistent CARGO_TARGET_DIR; no nix is ever
# executed. LATCHKEY_PROFILE=candidate selects the ci profile for cargo steps
# (used by scripts/checks/candidate-check.sh).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh disable=SC1091
source "$HERE/lib.sh"

cmd="${1:-}"
if [[ -z "$cmd" ]]; then
  lk_fail "cargo.sh: usage: cargo.sh <fmt|fmt-check|lint|build|test-unit|test-integration> [args...]"
fi
shift

# Profile selection:
#   * Default (unset LATCHKEY_PROFILE): cargo's own defaults, so the commands
#     stay exactly `cargo test --locked --lib ...` etc. — build/lint run under
#     dev and `cargo test` under its test profile, both living in the dev
#     profile tree (vanilla cargo shares one target/debug), keyed together.
#   * LATCHKEY_PROFILE=ci (candidate-check): every compiling cargo step gets
#     an explicit `--profile ci` and a ci-keyed target dir. Release/image
#     builds are explicit `nix build` steps, never a cargo default here.
profile_flags=()
if [[ -n "${LATCHKEY_PROFILE:-}" ]]; then
  profile_flags=(--profile "$LATCHKEY_PROFILE")
fi
base_profile="${LATCHKEY_PROFILE:-dev}"

lk_source_env

# Run cargo, requiring that at least one test actually ran. The parser reads
# the captured output (stdin); an optional first argument is forwarded to the
# parser (the suite name for integration suites). A suite/lane matching zero
# tests must fail.
run_counted() {
  local profile="$1" parser="$2" parser_arg="$3" what="$4" filter_desc="$5"
  shift 5
  local out rc count
  out="$(mktemp "${TMPDIR:-/tmp}/latchkey-test.XXXXXX")"
  set +e
  lk_cargo "$profile" "$@" 2>&1 | tee "$out"
  rc="${PIPESTATUS[0]}"
  set -e
  if [[ -n "$parser_arg" ]]; then
    count="$("$parser" "$parser_arg" <"$out")"
  else
    count="$("$parser" <"$out")"
  fi
  rm -f "$out"
  if [[ "$rc" -ne 0 ]]; then
    lk_err "$what: cargo exited with status $rc"
    exit "$rc"
  fi
  if [[ -z "$count" ]]; then
    lk_fail "$what: could not determine how many tests ran ($filter_desc); refusing to report success"
  fi
  if [[ "$count" -eq 0 ]]; then
    lk_fail "$what: matched 0 tests ($filter_desc); refusing to report success"
  fi
  echo "$what: $count test(s) ran ($filter_desc)"
}

case "$cmd" in
  fmt)
    lk_cargo "$base_profile" fmt --all
    ;;

  fmt-check)
    lk_cargo "$base_profile" fmt --all -- --check
    ;;

  lint)
    # Scoped lint: all targets, warnings denied, lockfile enforced.
    lk_cargo "$base_profile" clippy "${profile_flags[@]}" --all-targets --locked -- -D warnings
    ;;

  build)
    lk_cargo "$base_profile" build "${profile_flags[@]}" --locked
    ;;

  test-unit)
    lane="${1:-}"
    profile="$base_profile"
    if [[ -n "$lane" ]]; then
      lane="${lane%::}"
      if [[ -z "$lane" || ! "$lane" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
        lk_fail "test-unit: invalid lane '$lane' (expected a module path segment, e.g. 'tests' for tests::* or 'cli')"
      fi
      filter="${lane}::"
      desc="filter '$filter'"
      args=(test "${profile_flags[@]}" --locked --lib "$filter")
    else
      desc="all lib tests"
      args=(test "${profile_flags[@]}" --locked --lib)
    fi
    run_counted "$profile" lk_extract_lib_test_count "" "test-unit" "$desc" "${args[@]}"
    ;;

  test-integration)
    suite="${1:-}"
    if [[ -z "$suite" ]]; then
      lk_fail "test-integration: missing suite name (usage: just test-integration <suite> [-- harness-args...])"
    fi
    shift
    if [[ ! "$suite" =~ ^[A-Za-z_][A-Za-z0-9_-]*$ ]]; then
      lk_fail "test-integration: invalid suite name '$suite'"
    fi
    suite_file="tests/${suite}.rs"
    if [[ ! -f "$suite_file" ]]; then
      if owner="$(lk_suite_owner "$suite")"; then
        lk_fail "test-integration: $suite_file does not exist yet; owning ticket: $owner — this suite fails closed until that ticket lands it"
      else
        lk_fail "test-integration: unknown suite '$suite': $suite_file does not exist and no owning ticket is registered in scripts/dev/lib.sh (lk_suite_owner); refusing to guess"
      fi
    fi
    args=(test "${profile_flags[@]}" --locked --test "$suite")
    if [[ "$#" -gt 0 ]]; then
      args+=(-- "$@")
    fi
    run_counted "$base_profile" lk_extract_suite_test_count "$suite" "test-integration $suite" "suite '$suite'" "${args[@]}"
    ;;

  *)
    lk_fail "cargo.sh: unknown command '$cmd'"
    ;;
esac
