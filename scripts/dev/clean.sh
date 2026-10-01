#!/usr/bin/env bash
# Remove selected local outputs (LATCH-3).
#
# Deletes only this checkout's local state: the cached dev environment
# (.dev/, including profile-keyed Cargo target dirs), the legacy ./target/
# tree, nix result out-links, and stray *.log files. It NEVER evicts shared
# caches: no nix store paths, no substituter caches, no other worktrees.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh disable=SC1091
source "$HERE/lib.sh"

cd "$LK_ROOT"

removed=()
for path in .dev target result result-1 result-oci .setup-marker; do
  if [[ -e "$path" || -L "$path" ]]; then
    rm -rf -- "$path"
    removed+=("$path")
  fi
done
shopt -s nullglob
for log in *.log; do
  rm -f -- "$log"
  removed+=("$log")
done
shopt -u nullglob

if [[ "${#removed[@]}" -eq 0 ]]; then
  echo "clean: nothing to remove"
else
  echo "clean: removed: ${removed[*]}"
fi
echo "clean: shared caches untouched (nix store, substituters, other worktrees)"
echo "clean: run 'just setup' to materialize the dev environment again"
