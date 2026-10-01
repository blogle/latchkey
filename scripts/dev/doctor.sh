#!/usr/bin/env bash
# Diagnose the cached dev environment (LATCH-3). Never runs nix and never
# rebuilds: the single repair/forced-refresh command it reports is
# `just setup true`.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh disable=SC1091
source "$HERE/lib.sh"

fail() {
  echo "doctor: error: $*" >&2
  echo "doctor: fix: run 'just setup true' (the single forced-refresh command; doctor never rebuilds)" >&2
  echo "doctor: then re-run 'just doctor' to verify" >&2
  exit 1
}

cd "$LK_ROOT"

for f in "${LK_FINGERPRINT_INPUTS[@]}"; do
  [[ -f "$f" ]] || fail "missing required input $f"
done

[[ -f .dev/env ]] || fail "no cached environment: .dev/env not found (setup has not run, or .dev/ was removed)"
[[ -f .dev/fingerprint ]] || fail ".dev/fingerprint not found (setup did not complete)"

recorded="$(cat .dev/fingerprint)"
[[ -n "$recorded" ]] || fail ".dev/fingerprint is empty/malformed"

current="$(lk_fingerprint)" || fail "cannot compute the current fingerprint (required inputs missing)"
if [[ "$recorded" != "$current" ]]; then
  echo "doctor: recorded fingerprint: $recorded" >&2
  echo "doctor: current fingerprint:  $current" >&2
  fail "stale setup: flake.lock, rust-toolchain.toml, Cargo manifests/lock, setup version, or checkout path changed since 'just setup'"
fi

[[ -e .dev/gcroot ]] || fail "GC root .dev/gcroot missing or dangling (devShell would not survive nix-collect-garbage)"
[[ -f .dev/toolchain-id ]] || fail ".dev/toolchain-id missing (setup did not complete)"

# Source the cached env and verify every recorded tool still resolves.
had_u=0
[[ $- == *u* ]] && had_u=1
set +u
# The generated env file only exists after `just setup` (SC1091: runtime path).
# shellcheck disable=SC1091
source .dev/env
[[ $had_u -eq 1 ]] && set -u

for var in LATCHKEY_CARGO LATCHKEY_RUSTC LATCHKEY_RUSTFMT LATCHKEY_PYTHON3 LATCHKEY_SHELLCHECK; do
  value="${!var:-}"
  [[ -n "$value" ]] || fail ".dev/env does not record $var (corrupt; forced refresh required)"
  [[ -x "$value" ]] || fail "$var points at a missing binary: $value (nix store GC'd?)"
done

[[ "$LATCHKEY_SETUP_FINGERPRINT" == "$recorded" ]] \
  || fail ".dev/env metadata disagrees with .dev/fingerprint (interrupted setup)"

# Re-verify the compiler identity live: cheap, no nix, catches a toolchain
# that drifted from the recorded .dev/toolchain-id.
if ! actual_id="$("$LATCHKEY_RUSTC" -vV 2>/dev/null)"; then
  fail "recorded rustc is not runnable: $LATCHKEY_RUSTC"
fi
if [[ "$actual_id" != "$(cat .dev/toolchain-id)" ]]; then
  fail "toolchain identity drift: recorded $(printf '%s' "$(cat .dev/toolchain-id)" | sed -n '1p'), live $(printf '%s' "$actual_id" | sed -n '1p')"
fi

echo "doctor: environment healthy (fingerprint=$recorded)"
echo "doctor: checkout:   $LK_ROOT"
echo "doctor: setup date: ${LATCHKEY_SETUP_DATE:-unknown} (version ${LATCHKEY_SETUP_VERSION:-unknown})"
echo "doctor: toolchain:  $(sed -n '1p' .dev/toolchain-id)"
echo "doctor: cargo:      $LATCHKEY_CARGO"
echo "doctor: gcroot:     $LK_ROOT/.dev/gcroot"
echo "doctor: target key (dev profile): $(lk_target_key dev)"
echo "doctor: forced refresh (only when stale): just setup true"
