"""Unit tests for the LATCH-3 just-workflow dispatcher scripts.

These run via `just script-test dispatch` (python unittest discovery over
scripts/dev/tests/test_dispatch.py). They exercise the real shell library
and fail-closed dispatchers in scripts/dev/ through subprocesses — no mocks
of the dispatch logic itself.
"""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
DEV = REPO / "scripts" / "dev"
LIB = DEV / "lib.sh"

FIXTURE_FILES = {
    "flake.lock": '{ "nodes": {} }\n',
    "rust-toolchain.toml": '[toolchain]\nchannel = "1.96.0"\n',
    "Cargo.toml": "[package]\nname = 'fixture'\nversion = '0.0.0'\nedition = '2024'\n",
    "Cargo.lock": "# fixture lock\n",
}
TOOLCHAIN_ID = (
    "rustc 1.96.0 (fixturehash 2026-05-25)\n"
    "binary: rustc\n"
    "commit-hash: fixturehash\n"
    "host: x86_64-unknown-linux-gnu\n"
    "release: 1.96.0\n"
)


def run_bash(script: str, *, root: Path | None = None, stdin: str | None = None):
    """Run a bash snippet with nounset/errexit, optionally pinning LK_ROOT."""
    env = dict(os.environ)
    if root is not None:
        env["LK_ROOT"] = str(root)
    return subprocess.run(
        ["bash", "-euo", "pipefail", "-c", script],
        input=stdin,
        capture_output=True,
        text=True,
        env=env,
        cwd=str(REPO),
    )


def lib_call(function: str, *args: str, root: Path | None = None, stdin: str | None = None):
    quoted = " ".join(f"'{a}'" for a in args)
    script = f'source "{LIB}"\n{function} {quoted}\n'
    return run_bash(script, root=root, stdin=stdin)


def make_fixture_root(with_inputs: bool = True) -> tuple[tempfile.TemporaryDirectory, Path]:
    tmp = tempfile.TemporaryDirectory(prefix="latchkey-fixture-")
    root = Path(tmp.name)
    if with_inputs:
        for name, content in FIXTURE_FILES.items():
            (root / name).write_text(content)
    dev = root / ".dev"
    dev.mkdir()
    (dev / "toolchain-id").write_text(TOOLCHAIN_ID)
    return tmp, root


class FingerprintTests(unittest.TestCase):
    """lk_fingerprint: setup version + checkout path + input hashes."""

    def test_is_stable_for_identical_inputs(self):
        tmp_a, root_a = make_fixture_root()
        tmp_b, root_b = make_fixture_root()
        self.addCleanup(tmp_a.cleanup)
        self.addCleanup(tmp_b.cleanup)
        first = lib_call("lk_fingerprint", root=str(root_a))
        second = lib_call("lk_fingerprint", root=str(root_a))
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(first.stdout.strip(), second.stdout.strip())
        self.assertRegex(first.stdout.strip(), r"^[0-9a-f]{64}$")
        # Two different fixture roots differ in checkout path -> distinct.
        other = lib_call("lk_fingerprint", root=str(root_b))
        self.assertEqual(other.returncode, 0, other.stderr)
        self.assertNotEqual(first.stdout.strip(), other.stdout.strip())

    def test_changes_when_a_fingerprint_input_changes(self):
        tmp, root = make_fixture_root()
        self.addCleanup(tmp.cleanup)
        before = lib_call("lk_fingerprint", root=str(root))
        self.assertEqual(before.returncode, 0, before.stderr)
        (root / "flake.lock").write_text('{ "nodes": { "changed": true } }\n')
        after = lib_call("lk_fingerprint", root=str(root))
        self.assertEqual(after.returncode, 0, after.stderr)
        self.assertNotEqual(before.stdout.strip(), after.stdout.strip())

    def test_changes_when_cargo_manifest_changes(self):
        tmp, root = make_fixture_root()
        self.addCleanup(tmp.cleanup)
        before = lib_call("lk_fingerprint", root=str(root))
        (root / "Cargo.toml").write_text(FIXTURE_FILES["Cargo.toml"] + "[features]\nextra = []\n")
        after = lib_call("lk_fingerprint", root=str(root))
        self.assertNotEqual(before.stdout.strip(), after.stdout.strip())

    def test_fails_when_a_required_input_is_missing(self):
        tmp, root = make_fixture_root()
        self.addCleanup(tmp.cleanup)
        (root / "Cargo.lock").unlink()
        proc = lib_call("lk_fingerprint", root=str(root))
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("Cargo.lock", proc.stderr)


class TargetKeyTests(unittest.TestCase):
    """lk_target_key: compiler identity, profile, lock, manifest, config."""

    def test_is_separate_per_profile_and_stable(self):
        tmp, root = make_fixture_root()
        self.addCleanup(tmp.cleanup)
        dev_a = lib_call("lk_target_key", "dev", root=root)
        dev_b = lib_call("lk_target_key", "dev", root=root)
        ci = lib_call("lk_target_key", "ci", root=root)
        test = lib_call("lk_target_key", "test", root=root)
        self.assertEqual(dev_a.returncode, 0, dev_a.stderr)
        self.assertEqual(dev_a.stdout.strip(), dev_b.stdout.strip())
        self.assertNotEqual(dev_a.stdout.strip(), ci.stdout.strip())
        self.assertNotEqual(dev_a.stdout.strip(), test.stdout.strip())
        self.assertNotEqual(ci.stdout.strip(), test.stdout.strip())

    def test_changes_with_lockfile_and_toolchain_identity(self):
        tmp, root = make_fixture_root()
        self.addCleanup(tmp.cleanup)
        before = lib_call("lk_target_key", "dev", root=root)
        (root / "Cargo.lock").write_text("# changed lock\n")
        after_lock = lib_call("lk_target_key", "dev", root=root)
        self.assertNotEqual(before.stdout.strip(), after_lock.stdout.strip())
        (root / ".dev" / "toolchain-id").write_text(TOOLCHAIN_ID.replace("1.96.0", "1.97.0"))
        after_tc = lib_call("lk_target_key", "dev", root=root)
        self.assertNotEqual(after_lock.stdout.strip(), after_tc.stdout.strip())

    def test_fails_without_toolchain_identity(self):
        tmp, root = make_fixture_root()
        self.addCleanup(tmp.cleanup)
        (root / ".dev" / "toolchain-id").unlink()
        proc = lib_call("lk_target_key", "dev", root=root)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("toolchain-id", proc.stderr)

    def test_profile_dir_mapping(self):
        for profile, tree in (("dev", "debug"), ("test", "debug"), ("ci", "ci"), ("release", "release")):
            proc = lib_call("lk_profile_dir", profile)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertEqual(proc.stdout.strip(), tree)


LIB_OUTPUT_PASS = """   Compiling latchkey v0.0.0 (path+file:///checkout)
     Running unittests src/lib.rs (target/debug/deps/latchkey-abc)
running 3 tests
test tests::alpha ... ok
test tests::beta ... ok
test tests::gamma ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests latchkey
"""

LIB_OUTPUT_ZERO = """     Running unittests src/lib.rs (target/debug/deps/latchkey-abc)
running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
"""

LIB_OUTPUT_WRONG_TARGET = """     Running tests/other.rs (target/debug/deps/other-abc)
running 5 tests

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
"""

SUITE_OUTPUT = """     Running tests/contracts.rs (target/debug/deps/contracts-abc)
running 2 tests
test check_search ... ok
test check_exec ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
"""


class TestCountParserTests(unittest.TestCase):
    """Cargo output parsers backing the nonzero-test gate."""

    def test_lib_count(self):
        proc = lib_call("lk_extract_lib_test_count", stdin=LIB_OUTPUT_PASS)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(proc.stdout.strip(), "3")

    def test_lib_zero_is_reported_as_zero(self):
        proc = lib_call("lk_extract_lib_test_count", stdin=LIB_OUTPUT_ZERO)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(proc.stdout.strip(), "0")

    def test_lib_count_absent_without_lib_suite(self):
        proc = lib_call("lk_extract_lib_test_count", stdin=LIB_OUTPUT_WRONG_TARGET)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(proc.stdout.strip(), "")

    def test_suite_count_scoped_to_suite_header(self):
        proc = lib_call("lk_extract_suite_test_count", "contracts", stdin=SUITE_OUTPUT)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(proc.stdout.strip(), "2")
        other = lib_call("lk_extract_suite_test_count", "missing", stdin=SUITE_OUTPUT)
        self.assertEqual(other.stdout.strip(), "")


class OwnerRegistryTests(unittest.TestCase):
    """Missing integration suites fail citing their owning ticket."""

    def test_contracts_owned_by_latch_2(self):
        proc = lib_call("lk_suite_owner", "contracts")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("LATCH-2", proc.stdout)

    def test_unknown_suite_has_no_owner(self):
        proc = lib_call("lk_suite_owner", "definitely-not-a-suite")
        self.assertNotEqual(proc.returncode, 0)


class FutureDispatchTests(unittest.TestCase):
    """future.sh fails closed with the exact owning ticket."""

    def test_future_command_fails_with_ticket(self):
        proc = subprocess.run(
            [str(DEV / "future.sh"), "LATCH-4", "release-plan", "deadbeef"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(proc.returncode, 1)
        self.assertIn("LATCH-4", proc.stderr)
        self.assertIn("release-plan", proc.stderr)
        self.assertIn("failing closed", proc.stderr)

    def test_future_command_requires_ticket_and_name(self):
        proc = subprocess.run([str(DEV / "future.sh")], capture_output=True, text=True)
        self.assertNotEqual(proc.returncode, 0)


class IntegrationGateTests(unittest.TestCase):
    """test-integration fails closed until the suite file exists."""

    def test_missing_contracts_suite_cites_latch_2(self):
        if (REPO / "tests" / "contracts.rs").exists():
            self.skipTest("tests/contracts.rs now exists; the gate branch is covered by cargo itself")
        proc = subprocess.run(
            [str(DEV / "cargo.sh"), "test-integration", "contracts"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(proc.returncode, 1)
        self.assertIn("LATCH-2", proc.stderr)
        self.assertIn("tests/contracts.rs", proc.stderr)

    def test_unknown_suite_fails_without_guessing(self):
        if (REPO / "tests" / "no-such-suite.rs").exists():
            self.skipTest("suite file unexpectedly exists")
        proc = subprocess.run(
            [str(DEV / "cargo.sh"), "test-integration", "no-such-suite"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(proc.returncode, 1)
        self.assertIn("unknown suite", proc.stderr)


if __name__ == "__main__":
    unittest.main()
