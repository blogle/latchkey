#!/usr/bin/env bash
# Validate the production OCI contract without requiring a container runtime.
# The image is tested from its exported archive so accidental shells, source
# trees, fixtures, or deployment configuration are caught.
set -euo pipefail

image_tar=${1:?usage: image-contract.sh IMAGE-TAR}
root_dir=$(mktemp -d)
cleanup() {
  chmod -R u+w "$root_dir" 2>/dev/null || true
  rm -rf "$root_dir"
}
trap cleanup EXIT

mkdir -p "$root_dir/layers" "$root_dir/rootfs"
tar -xf "$image_tar" -C "$root_dir/layers"

manifest="$root_dir/layers/manifest.json"
[[ -f "$manifest" ]] || { echo "image has no manifest.json" >&2; exit 1; }
layers=$(grep -oE '"[^"]+layer\.tar"' "$manifest" | tr -d '"')
[[ -n "$layers" ]] || { echo "image has no layers" >&2; exit 1; }
while IFS= read -r layer; do
  tar -xf "$root_dir/layers/$layer" -C "$root_dir/rootfs"
done <<<"$layers"

config=$(grep -oE '"Config"[[:space:]]*:[[:space:]]*"[^"]+"' "$manifest" | cut -d'"' -f4)
[[ -n "$config" && -f "$root_dir/layers/$config" ]] \
  || { echo "image has no config JSON" >&2; exit 1; }

grep -q '"User":"65532:65532"' "$root_dir/layers/$config" \
  || { echo "image must run as UID/GID 65532" >&2; exit 1; }
grep -q '"Entrypoint":\["/bin/latchkey"\]' "$root_dir/layers/$config" \
  || { echo "image entrypoint must be /bin/latchkey" >&2; exit 1; }
grep -q '"Cmd":\["serve","--mode","standalone","--config","/etc/latchkey/config.toml","--listen","0.0.0.0:8080"\]' "$root_dir/layers/$config" \
  || { echo "image command does not match the standalone contract" >&2; exit 1; }

[[ -x "$root_dir/rootfs/bin/latchkey" ]] \
  || { echo "image has no executable /bin/latchkey" >&2; exit 1; }
binary=$(find "$root_dir/rootfs/nix/store" -path '*/bin/latchkey' -type f -print -quit)
[[ -n "$binary" && -x "$binary" ]] \
  || { echo "image has no executable latchkey store output" >&2; exit 1; }
if find "$root_dir/rootfs" \( -type f -o -type l \) | grep -E '/(bin/(sh|bash|busybox)|usr/bin/(cc|gcc|clang|cargo|rustc|make)|etc/latchkey/config\.toml)$' >/dev/null; then
  echo "image contains a forbidden shell, build tool, or baked config" >&2
  exit 1
fi
[[ -e "$root_dir/rootfs/etc/ssl/certs/ca-bundle.crt" ]] \
  || { echo "image has no CA root bundle" >&2; exit 1; }

version=$($binary --version)
[[ "$version" == latchkey\ * ]] || { echo "image --version failed" >&2; exit 1; }
$binary --help >/dev/null || { echo "image --help failed" >&2; exit 1; }

# The config is supplied outside the image, proving startup does not depend on
# a writable root or deployment-specific material baked into the image.
config_dir=$(mktemp -d)
printf 'version = 1\n' >"$config_dir/config.toml"
port=$((20000 + ($$ % 20000)))
status=0
if command -v timeout >/dev/null 2>&1; then
  timeout 2s "$binary" serve --mode standalone --config "$config_dir/config.toml" --listen "127.0.0.1:$port" >/dev/null 2>"$config_dir/stderr" || status=$?
  [[ "$status" -eq 124 || "$status" -eq 143 ]] \
    || { cat "$config_dir/stderr" >&2; echo "standalone process did not stay up" >&2; exit 1; }
else
  echo "image contract requires timeout to verify startup" >&2
  exit 1
fi
rm -rf "$config_dir"

echo "image contract passed: $version, nonroot, read-only-safe standalone startup"
