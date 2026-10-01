#!/usr/bin/env python3
"""Apply a planned release version to only the root Cargo package metadata."""

from __future__ import annotations

import argparse
import os
import re
import sys
import tempfile
import tomllib
from pathlib import Path


VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")


def _replace_package_version(text: str, package_name: str, version: str, path: Path) -> str:
    """Replace the version in exactly one named [[package]] lock entry."""
    blocks = list(re.finditer(r"(?ms)^\[\[package\]\]\n.*?(?=^\[\[package\]\]|\Z)", text))
    matches = []
    for block in blocks:
        if re.search(rf'(?m)^name\s*=\s*"{re.escape(package_name)}"\s*$', block.group()):
            matches.append(block)
    if len(matches) != 1:
        raise ValueError(f"{path}: expected exactly one lock entry for {package_name!r}")
    block = matches[0]
    updated, count = re.subn(r'(?m)^(version\s*=\s*)"[^"]+"$', rf'\g<1>"{version}"', block.group(), count=1)
    if count != 1:
        raise ValueError(f"{path}: root lock entry has no version field")
    return text[: block.start()] + updated + text[block.end() :]


def _atomic_write(path: Path, content: str) -> None:
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", dir=path.parent, delete=False) as stream:
        temp_path = Path(stream.name)
        stream.write(content)
    try:
        os.replace(temp_path, path)
    finally:
        temp_path.unlink(missing_ok=True)


def inject(root: Path, planned_version: str) -> None:
    if not VERSION.fullmatch(planned_version):
        raise ValueError("planned version must be MAJOR.MINOR.PATCH")
    source_root = Path(__file__).resolve().parents[2]
    root = root.resolve()
    if root == source_root:
        raise ValueError("refusing to modify the checked-out source tree; use a disposable candidate worktree")

    manifest_path = root / "Cargo.toml"
    lock_path = root / "Cargo.lock"
    manifest_text = manifest_path.read_text(encoding="utf-8")
    lock_text = lock_path.read_text(encoding="utf-8")
    manifest = tomllib.loads(manifest_text)
    lock = tomllib.loads(lock_text)
    package = manifest.get("package", {})
    if package.get("name") != "latchkey" or not isinstance(package.get("version"), str):
        raise ValueError(f"{manifest_path}: expected root package named latchkey with a version")

    root_entries = [entry for entry in lock.get("package", []) if entry.get("name") == "latchkey"]
    if len(root_entries) != 1 or root_entries[0].get("version") != package["version"]:
        raise ValueError(f"{lock_path}: root latchkey lock entry does not match Cargo.toml")

    manifest_pattern = re.compile(r"(?ms)(^\[package\]\n.*?^version\s*=\s*)\"[^\"]+\"")
    updated_manifest, count = manifest_pattern.subn(rf'\g<1>"{planned_version}"', manifest_text, count=1)
    if count != 1:
        raise ValueError(f"{manifest_path}: cannot locate root [package] version")
    updated_lock = _replace_package_version(lock_text, "latchkey", planned_version, lock_path)

    new_lock = tomllib.loads(updated_lock)
    old_dependencies = [entry for entry in lock["package"] if entry["name"] != "latchkey"]
    new_dependencies = [entry for entry in new_lock["package"] if entry["name"] != "latchkey"]
    if old_dependencies != new_dependencies:
        raise ValueError("refusing version injection because non-root Cargo.lock entries changed")

    # Both inputs are validated before either file is changed. These files are
    # expected to live in a disposable candidate checkout, never the source tree.
    _atomic_write(manifest_path, updated_manifest)
    _atomic_write(lock_path, updated_lock)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="disposable Cargo project/worktree root")
    parser.add_argument("version", help="planned MAJOR.MINOR.PATCH version")
    args = parser.parse_args()
    try:
        inject(args.root, args.version)
    except (OSError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"root-version injection failed: {error}", file=sys.stderr)
        return 1
    print(f"root-version injection: latchkey -> {args.version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
