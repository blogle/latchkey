#!/usr/bin/env bash
# Run one Python unittest suite for the dev/check dispatcher scripts.
#
#   just script-test <suite>
#
# Resolves <suite> with the generic multi-root rule (LATCH-4): first match of
# scripts/dev/tests/test_<suite>.py, tests/<suite>/ (test_*.py), or
# scripts/ci/tests/test_<suite>.py — with the python3 recorded in .dev/env —
# and refuses to report success unless a nonzero number of tests actually ran.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh disable=SC1091
source "$HERE/lib.sh"

suite="${1:-}"
if [[ -z "$suite" ]]; then
  lk_fail "script-test: missing suite name (usage: just script-test <suite>); available: $(lk_script_suites | paste -sd' ' - || echo none)"
fi
if [[ ! "$suite" =~ ^[A-Za-z0-9_-]+$ ]]; then
  lk_fail "script-test: invalid suite name '$suite'"
fi

# Generic multi-root suite resolution (LATCH-4 glue; mirrors the listing rule
# in lk_script_suites — first match wins): suite <X> is
#   1. scripts/dev/tests/test_<X>.py, else
#   2. the directory tests/<X>/ containing test_*.py (unittest discovers it), else
#   3. scripts/ci/tests/test_<X>.py
# Resolution runs before lk_source_env so an unknown suite fails fast (and
# lists the rule + available suites) without touching the cached environment.
start_dir=""
pattern=""
# Portable test_*.py presence check: `compgen` is unavailable in the cached
# dev-shell bash (nixpkgs bash-interactive ships without programmable
# completion), so probe the glob with an array (an unmatched glob stays
# literal under default options, so -e/-L on the first element is a match test).
dir_suite_tests=("$LK_ROOT/tests/${suite}"/test_*.py)
if [[ -f "$LK_ROOT/scripts/dev/tests/test_${suite}.py" ]]; then
  start_dir="$LK_ROOT/scripts/dev/tests"
  pattern="test_${suite}.py"
elif [[ -d "$LK_ROOT/tests/${suite}" && ( -e "${dir_suite_tests[0]}" || -L "${dir_suite_tests[0]}" ) ]]; then
  start_dir="$LK_ROOT/tests/${suite}"
  pattern="test_*.py"
elif [[ -f "$LK_ROOT/scripts/ci/tests/test_${suite}.py" ]]; then
  start_dir="$LK_ROOT/scripts/ci/tests"
  pattern="test_${suite}.py"
else
  available="$(lk_script_suites | paste -sd' ' - || true)"
  lk_fail "script-test: no suite '${suite}' (looked for scripts/dev/tests/test_${suite}.py, tests/${suite}/, scripts/ci/tests/test_${suite}.py); available suites: ${available:-none}"
fi

lk_source_env

out="$(mktemp "${TMPDIR:-/tmp}/latchkey-script-test.XXXXXX")"
set +e
# Never write __pycache__ into the repo (gitignored anyway, but keep it clean).
PYTHONDONTWRITEBYTECODE=1 "$LATCHKEY_PYTHON3" -m unittest discover \
  -s "$start_dir" \
  -p "$pattern" -v 2>&1 | tee "$out"
rc="${PIPESTATUS[0]}"
set -e
count="$(sed -n 's/^Ran \([0-9][0-9]*\) tests\?.*$/\1/p' "$out" | tail -n1)"
rm -f "$out"
if [[ "$rc" -ne 0 ]]; then
  lk_err "script-test: suite '$suite' failed (python exit status $rc)"
  exit "$rc"
fi
if [[ -z "$count" ]]; then
  lk_fail "script-test: could not determine how many tests ran in suite '$suite'; refusing to report success"
fi
if [[ "$count" -eq 0 ]]; then
  lk_fail "script-test: suite '$suite' ran 0 tests; refusing to report success"
fi
echo "script-test: suite '$suite' ran $count test(s)"
