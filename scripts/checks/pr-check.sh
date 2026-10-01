#!/usr/bin/env bash
# Fast pull-request gate (LATCH-3): fmt-check + lint + script-test +
# test-unit. Deliberately NO OCI image build, NO `nix flake check`, NO E2E,
# and no nix invocation at all — it runs entirely from the cached .dev/env.
# CI keeps calling `nix develop -c just pr-check`; the recipe itself must
# keep working with failing nix/nix-store sentinels first in PATH.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../dev/lib.sh disable=SC1091
source "$HERE/../dev/lib.sh"

cd "$LK_ROOT"

echo "==> fmt-check"
"$HERE/../dev/cargo.sh" fmt-check

echo "==> lint (clippy, all targets, -D warnings)"
"$HERE/../dev/cargo.sh" lint

echo "==> script-test (python dispatcher suites)"
# Generic multi-root suite discovery (LATCH-4 glue): reuse the exact rule
# script-test.sh applies per suite, so pr-check always runs precisely the
# suites `just script-test <name>` can resolve (dev tests, tests/<name>/ dirs,
# ci tests — see lk_script_suites in scripts/dev/lib.sh).
suites=()
while IFS= read -r suite; do
  [[ -n "$suite" ]] || continue
  suites+=("$suite")
done < <(lk_script_suites)
if [[ "${#suites[@]}" -eq 0 ]]; then
  lk_fail "pr-check: no script-test suites discovered by lk_script_suites (see scripts/dev/lib.sh)"
fi
for suite in "${suites[@]}"; do
  echo "==> script-test $suite"
  "$HERE/../dev/script-test.sh" "$suite"
done

echo "==> test-unit (all lib tests)"
"$HERE/../dev/cargo.sh" test-unit

echo "pr-check: all fast gates passed (fmt-check, lint, script-test, test-unit); no OCI, no nix flake check, no E2E"
