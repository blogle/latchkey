#!/usr/bin/env bash
# Shared helpers for the LATCH-3 cached native just workflow.
#
# Sourced by every dispatcher under scripts/dev/ and scripts/checks/; it must
# have no side effects at source time (the Python unittest suite imports it
# repeatedly in subshells). Design rules:
#
#   * Nothing here ever runs `nix`. Ordinary recipes must work with failing
#     `nix`/`nix-store` sentinels first in PATH: they only hash files and
#     source the cached `.dev/env` that `just setup` materialized.
#   * The setup fingerprint hashes flake.lock + rust-toolchain.toml + Cargo
#     manifests/lock + LK_SETUP_VERSION + the checkout path. Bump
#     LK_SETUP_VERSION whenever the dev-environment composition (flake
#     devShell) or this fingerprint scheme changes.
#   * Cached tool invocations use the absolute store paths recorded in
#     .dev/env (LATCHKEY_* variables), never ambient PATH lookups.

# Bump on any dev-shell composition or fingerprint-scheme change so the next
# `just setup` re-materializes .dev/env.
LK_SETUP_VERSION="1"

# Repo root: scripts/dev/lib.sh -> ../.. ; overridable for fixtures/tests.
if [[ -z "${LK_ROOT:-}" ]]; then
  LK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi

# ---- messages ---------------------------------------------------------------

lk_err() { printf 'latchkey: error: %s\n' "$*" >&2; }

# Fail immediately with the single repair command the ticket mandates.
lk_die() {
  lk_err "$*"
  lk_err "fix: run 'just setup true' (the single forced-refresh command), then 'just doctor' to verify"
  exit 1
}

lk_fail() {
  lk_err "$*"
  exit 1
}

# Print a command line in copy-pasteable, safely quoted form.
lk_print_cmd() {
  printf '+'
  printf ' %q' "$@"
  printf '\n'
}

# ---- setup fingerprint ------------------------------------------------------

# Files whose content feeds the setup fingerprint (in addition to the setup
# version and the absolute checkout path).
LK_FINGERPRINT_INPUTS=(flake.lock rust-toolchain.toml Cargo.toml Cargo.lock)

lk_sha() { sha256sum <"$1" | awk '{print $1}'; }

# Fingerprint of the dev environment: setup version + checkout path + hashes
# of flake.lock, rust-toolchain.toml, Cargo.lock and every Cargo manifest.
lk_fingerprint() {
  local root="${1:-$LK_ROOT}"
  local f
  for f in "${LK_FINGERPRINT_INPUTS[@]}"; do
    if [[ ! -f "$root/$f" ]]; then
      lk_err "fingerprint: missing required input $root/$f"
      return 1
    fi
  done
  {
    printf 'latchkey-setup-fingerprint-v1\n'
    printf 'setup-version=%s\n' "$LK_SETUP_VERSION"
    printf 'checkout=%s\n' "$root"
    for f in "${LK_FINGERPRINT_INPUTS[@]}"; do
      printf '%s=' "$f"
      lk_sha "$root/$f"
    done
    # Every Cargo manifest in the tree (workspace members), deterministic.
    (
      cd "$root" || exit 1
      find . \( -name Cargo.toml \) \
        -not -path './.dev/*' -not -path './target/*' -not -path './.git/*' \
        | LC_ALL=C sort
    ) | while IFS= read -r manifest; do
      printf 'manifest:%s=' "${manifest#./}"
      lk_sha "$root/$manifest"
    done
  } | sha256sum | awk '{print $1}'
}

# ---- cached environment gate ------------------------------------------------

# Fail early (before any tool runs) unless .dev/env matches the current
# fingerprint. Never rebuilds: repair is always `just setup true`.
lk_require_setup() {
  local envfile="$LK_ROOT/.dev/env"
  local fpfile="$LK_ROOT/.dev/fingerprint"
  if [[ ! -f "$envfile" ]]; then
    lk_die "cached dev environment missing: ${envfile#"$LK_ROOT"/} not found (setup has not run in this checkout)"
  fi
  if [[ ! -f "$fpfile" ]]; then
    lk_die "cached dev environment incomplete: ${fpfile#"$LK_ROOT"/} not found"
  fi
  local stored current
  stored="$(cat "$fpfile")"
  if [[ -z "$stored" ]]; then
    lk_die "cached dev environment incomplete: ${fpfile#"$LK_ROOT"/} is empty"
  fi
  current="$(lk_fingerprint)" || lk_die "cannot compute the environment fingerprint (required input files missing)"
  if [[ "$stored" != "$current" ]]; then
    lk_err "recorded fingerprint: $stored"
    lk_err "current fingerprint:  $current"
    lk_die "cached dev environment is stale: flake.lock, rust-toolchain.toml, Cargo manifests/lock, setup version, or checkout path changed since setup"
  fi
}

# Source .dev/env (with nounset temporarily relaxed; the nix-generated script
# references optional variables) and verify the recorded tool paths resolve.
lk_source_env() {
  lk_require_setup
  local envfile="$LK_ROOT/.dev/env"
  local had_u=0
  [[ $- == *u* ]] && had_u=1
  set +u
  # shellcheck disable=SC1090
  source "$envfile"
  [[ $had_u -eq 1 ]] && set -u
  if [[ -z "${LATCHKEY_CARGO:-}" || ! -x "${LATCHKEY_CARGO:-}" ]]; then
    lk_die ".dev/env does not record a usable cargo (LATCHKEY_CARGO); the cached environment is corrupt"
  fi
  # Belt and braces: guarantee the cached toolchain dir precedes any ambient
  # PATH entry even if the generated PATH block was tampered with.
  local cargodir
  cargodir="$(dirname "$LATCHKEY_CARGO")"
  case ":$PATH:" in
    *":$cargodir:"*) ;;
    *) PATH="$cargodir:$PATH" ;;
  esac
  export PATH
}

# ---- profile-keyed persistent target dirs -----------------------------------

# Cargo maps profiles to on-disk artifact trees; two cargo profiles that share
# a tree (dev/test -> debug) still get separate persistent base dirs here so
# the cache is keyed by the profile a recipe selects, never by assumption.
lk_profile_dir() {
  case "$1" in
    dev | test) printf 'debug\n' ;;
    *) printf '%s\n' "$1" ;;
  esac
}

# Persistent target dir key: compiler identity (.dev/toolchain-id, i.e.
# `rustc -vV` including its commit hash and host), profile, features (via the
# Cargo manifest hash), lockfile fingerprint, and cargo config/rustflags.
# Per-worktree by construction: the key lives under this checkout's .dev/.
lk_target_key() {
  local profile="$1"
  local tcfile="$LK_ROOT/.dev/toolchain-id"
  if [[ ! -f "$tcfile" ]]; then
    lk_die "missing ${tcfile#"$LK_ROOT"/} (setup did not complete); run 'just setup true'"
  fi
  local cfg_hash="none"
  if [[ -f "$LK_ROOT/.cargo/config.toml" ]]; then
    cfg_hash="$(lk_sha "$LK_ROOT/.cargo/config.toml")"
  fi
  {
    printf 'latchkey-target-key-v1\n'
    printf 'profile=%s\n' "$profile"
    printf 'compiler=%s\n' "$(tr '\n' '|' <"$tcfile")"
    printf 'profile-tree=%s\n' "$(lk_profile_dir "$profile")"
    printf 'lock=%s\n' "$(lk_sha "$LK_ROOT/Cargo.lock")"
    printf 'manifest-features=%s\n' "$(lk_sha "$LK_ROOT/Cargo.toml")"
    printf 'cargo-config=%s\n' "$cfg_hash"
    printf 'setup-version=%s\n' "$LK_SETUP_VERSION"
  } | sha256sum | awk '{print $1}'
}

lk_target_dir() {
  local profile="$1"
  local key
  key="$(lk_target_key "$profile")"
  local dir="$LK_ROOT/.dev/target/$key"
  mkdir -p "$dir"
  printf '%s\n' "$dir"
}

lk_export_target() {
  local dir
  dir="$(lk_target_dir "$1")"
  export CARGO_TARGET_DIR="$dir"
}

# Run the absolute cached cargo with the profile-keyed target dir.
lk_cargo() {
  local profile="$1"
  shift
  lk_export_target "$profile"
  lk_print_cmd env "CARGO_TARGET_DIR=$CARGO_TARGET_DIR" "$LATCHKEY_CARGO" "$@"
  "$LATCHKEY_CARGO" "$@"
}

# ---- cargo test output parsing ---------------------------------------------

# Print the number of lib unit tests cargo reported (the "running N tests"
# line of the `Running unittests .../src/lib.rs` suite). Prints nothing when
# the lib suite never reported; callers must treat that as failure.
lk_extract_lib_test_count() {
  awk '
    /Running unittests .*src\/lib\.rs/ { in_lib = 1; next }
    in_lib && /^running [0-9]+ tests?$/ { print $2; exit }
    /^Running / { if (in_lib) exit }
  '
}

# Same for one integration suite: `Running tests/<suite>.rs (...)` (cargo
# indents the header; match the literal substring anywhere in the line).
lk_extract_suite_test_count() {
  awk -v hdr="Running tests/$1.rs (" '
    index($0, hdr) > 0 { in_suite = 1; next }
    in_suite && /^running [0-9]+ tests?$/ { print $2; exit }
    /^Running / { if (in_suite) exit }
  '
}

# ---- future-command ownership ----------------------------------------------

# Owning ticket for an integration suite whose tests/<suite>.rs does not exist
# yet. Extend here (not in the justfile) when a future ticket lands a suite.
lk_suite_owner() {
  case "$1" in
    contracts) printf 'LATCH-2 (F02)\n' ;;
    *) return 1 ;;
  esac
}

# List script-test suites (LATCH-4: generic multi-root discovery).
#
# One rule, three roots, first match wins when a suite name exists in more
# than one root; the listing below is the deduplicated union (sorted):
#   1. scripts/dev/tests/test_<suite>.py  — single-file dispatcher suite
#   2. tests/<suite>/                     — directory suite: unittest
#                                           discovers test_*.py inside it
#   3. scripts/ci/tests/test_<suite>.py   — single-file CI suite
# script-test.sh resolves a requested suite with the same rule; keep the two
# in sync (the release_policy suite exercises root 2 end to end).
lk_script_suites() {
  local f d found
  {
    for f in "$LK_ROOT"/scripts/dev/tests/test_*.py; do
      [[ -e "$f" ]] || continue
      basename "$f" .py | sed 's/^test_//'
    done
    for d in "$LK_ROOT"/tests/*/; do
      [[ -d "$d" ]] || continue
      found=0
      for f in "$d"test_*.py; do
        [[ -e "$f" ]] && found=1
      done
      [[ "$found" -eq 1 ]] || continue
      basename "$d"
    done
    for f in "$LK_ROOT"/scripts/ci/tests/test_*.py; do
      [[ -e "$f" ]] || continue
      basename "$f" .py | sed 's/^test_//'
    done
  } | LC_ALL=C sort -u
}
