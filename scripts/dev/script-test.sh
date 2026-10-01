#!/usr/bin/env bash
# Run one Python unittest suite for the dev/check dispatcher scripts.
#
#   just script-test <suite>
#
# Discovers scripts/dev/tests/test_<suite>.py with the python3 recorded in
# .dev/env and refuses to report success unless a nonzero number of tests
# actually ran.
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

lk_source_env

test_file="$LK_ROOT/scripts/dev/tests/test_${suite}.py"
if [[ ! -f "$test_file" ]]; then
  available="$(lk_script_suites | paste -sd' ' - || true)"
  lk_fail "script-test: no suite '${suite}' (${test_file#"$LK_ROOT"/} does not exist); available suites: ${available:-none}"
fi

out="$(mktemp "${TMPDIR:-/tmp}/latchkey-script-test.XXXXXX")"
set +e
# Never write __pycache__ into the repo (gitignored anyway, but keep it clean).
PYTHONDONTWRITEBYTECODE=1 "$LATCHKEY_PYTHON3" -m unittest discover \
  -s "$LK_ROOT/scripts/dev/tests" \
  -p "test_${suite}.py" -v 2>&1 | tee "$out"
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
