#!/usr/bin/env bash
# Print reproducible OCI archive and extracted filesystem measurements.
set -euo pipefail

image_tar=${1:?usage: image-footprint.sh IMAGE-TAR}
root_dir=$(mktemp -d)
cleanup() {
  chmod -R u+w "$root_dir" 2>/dev/null || true
  rm -rf "$root_dir"
}
trap cleanup EXIT

mkdir -p "$root_dir/layers" "$root_dir/rootfs"
tar -xf "$image_tar" -C "$root_dir/layers"
while IFS= read -r layer; do
  tar -xf "$root_dir/layers/$layer" -C "$root_dir/rootfs"
done < <(grep -oE '"[^"]+layer\.tar"' "$root_dir/layers/manifest.json" | tr -d '"')

printf 'compressed_bytes=%s\n' "$(wc -c <"$image_tar")"
printf 'rootfs_bytes=%s\n' "$(du -s -B1 "$root_dir/rootfs" | cut -f1)"
printf 'rootfs_files=%s\n' "$(find "$root_dir/rootfs" -type f | wc -l)"
