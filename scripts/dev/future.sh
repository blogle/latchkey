#!/usr/bin/env bash
# Fail-closed dispatcher for future commands (LATCH-3).
#
#   future.sh <owning-ticket> <command> [ignored args...]
#
# Unimplemented commands must never succeed: they print the exact owning
# ticket and exit nonzero. The justfile routes every not-yet-built recipe
# here, so the frozen root surface stays honest while tickets land the real
# implementations (the owning script simply replaces this call site).
set -euo pipefail

ticket="${1:-}"
command="${2:-}"
if [[ -z "$ticket" || -z "$command" ]]; then
  echo "future.sh: usage: future.sh <owning-ticket> <command> [args...]" >&2
  exit 2
fi
shift 2 || true

{
  echo "$command: not implemented (owning ticket: $ticket)"
  echo "$command: failing closed with exit status 1; this command cannot succeed until $ticket lands its implementation."
  if [[ "$#" -gt 0 ]]; then
    echo "$command: ($# argument(s) received and deliberately ignored)"
  fi
} >&2
exit 1
