from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts" / "ci"))
import inject_root_version  # noqa: E402


class RootVersionTests(unittest.TestCase):
    def test_injection_changes_only_root_manifest_and_lock_entry(self):
        with tempfile.TemporaryDirectory(prefix="root-version-overlay-") as temporary:
            root = Path(temporary)
            shutil.copy2(ROOT / "Cargo.toml", root / "Cargo.toml")
            shutil.copy2(ROOT / "Cargo.lock", root / "Cargo.lock")
            before_lock_text = (root / "Cargo.lock").read_text()
            before = tomllib.loads(before_lock_text)

            inject_root_version.inject(root, "9.9.9")

            manifest = tomllib.loads((root / "Cargo.toml").read_text())
            after_lock_text = (root / "Cargo.lock").read_text()
            after = tomllib.loads(after_lock_text)
            self.assertEqual(manifest["package"]["version"], "9.9.9")
            root_lock_marker = 'name = "latchkey"\nversion = "0.0.0"'
            self.assertEqual(before_lock_text.count(root_lock_marker), 1)
            self.assertEqual(
                after_lock_text,
                before_lock_text.replace(root_lock_marker, 'name = "latchkey"\nversion = "9.9.9"', 1),
            )
            self.assertEqual(
                [entry for entry in before["package"] if entry["name"] != "latchkey"],
                [entry for entry in after["package"] if entry["name"] != "latchkey"],
            )
            root_entry = next(entry for entry in after["package"] if entry["name"] == "latchkey")
            self.assertEqual(root_entry["version"], "9.9.9")
            self.assertEqual(
                root_entry["dependencies"],
                next(entry for entry in before["package"] if entry["name"] == "latchkey")["dependencies"],
            )

    def test_locked_build_prints_the_planned_root_version(self):
        cargo = shutil.which("cargo")
        if cargo is None:
            self.skipTest("pinned Cargo toolchain is unavailable")
        with tempfile.TemporaryDirectory(prefix="root-version-build-") as temporary:
            root = Path(temporary)
            shutil.copytree(
                ROOT,
                root,
                dirs_exist_ok=True,
                ignore=shutil.ignore_patterns(".git", ".dev", "target", "result*", "__pycache__"),
            )
            inject_root_version.inject(root, "9.9.9")
            env = os.environ.copy()
            env["CARGO_TARGET_DIR"] = str(ROOT / "target")
            subprocess.run(
                [cargo, "build", "--locked", "--offline", "--manifest-path", str(root / "Cargo.toml")],
                cwd=root,
                env=env,
                check=True,
            )
            output = subprocess.run(
                [str(ROOT / "target" / "debug" / "latchkey"), "--version"],
                check=True,
                text=True,
                capture_output=True,
            ).stdout.strip()
            self.assertEqual(output, "latchkey 9.9.9")

    def test_invalid_version_does_not_modify_files(self):
        with tempfile.TemporaryDirectory(prefix="root-version-invalid-") as temporary:
            root = Path(temporary)
            shutil.copy2(ROOT / "Cargo.toml", root / "Cargo.toml")
            shutil.copy2(ROOT / "Cargo.lock", root / "Cargo.lock")
            before = ((root / "Cargo.toml").read_bytes(), (root / "Cargo.lock").read_bytes())
            with self.assertRaisesRegex(ValueError, "MAJOR.MINOR.PATCH"):
                inject_root_version.inject(root, "9.9.9-beta")
            self.assertEqual(before, ((root / "Cargo.toml").read_bytes(), (root / "Cargo.lock").read_bytes()))

    def test_checked_out_source_tree_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "disposable candidate worktree"):
            inject_root_version.inject(ROOT, "9.9.9")


if __name__ == "__main__":
    unittest.main()
