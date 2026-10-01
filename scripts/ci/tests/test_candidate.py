import sys
import os
import subprocess
import tempfile
import unittest
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts" / "ci"))
import candidate  # noqa: E402
from validate_candidate_event import validate as validate_event  # noqa: E402
from validate_pr import validate as validate_pr  # noqa: E402

SHA_A = "a" * 40
SHA_B = "b" * 40
SHA_C = "c" * 40
TREE = "d" * 40


class CandidateTests(unittest.TestCase):
    def _repo(self):
        directory = tempfile.TemporaryDirectory(prefix="candidate-fixture-")
        repo = directory.name
        def run(*args):
            return subprocess.run(["git", *args], cwd=repo, check=True, text=True,
                                  capture_output=True).stdout.strip()
        run("init", "-q")
        run("config", "user.name", "Candidate fixture")
        run("config", "user.email", "candidate@example.invalid")
        (Path(repo) / "base.txt").write_text("base\n")
        run("add", ".")
        run("commit", "-qm", "base")
        base = run("rev-parse", "HEAD")
        (Path(repo) / ".changes" ).mkdir()
        (Path(repo) / ".changes" / "A-1.toml").write_text('category = "Added"\nsummary = "A"\n')
        run("add", ".")
        run("commit", "-qm", "PR A")
        head_a = run("rev-parse", "HEAD")
        (Path(repo) / ".changes" / "B-2.toml").write_text('category = "Fixed"\nsummary = "B"\n')
        run("add", ".")
        run("commit", "-qm", "PR B")
        head_b = run("rev-parse", "HEAD")
        tree_b = run("rev-parse", "HEAD^{tree}")
        return directory, repo, base, head_a, head_b, tree_b

    def test_ordinary_same_repo_pr_exact_sha(self):
        event = {"action": "synchronize", "repository": {"full_name": "blogle/latchkey"},
                 "pull_request": {"head": {"sha": SHA_A, "repo": {"full_name": "blogle/latchkey"}},
                                  "base": {"ref": "master"}}}
        validate_pr(event, "blogle/latchkey", SHA_A, SHA_A)

    def test_legitimate_queue_pr(self):
        event = {"repository": {"full_name": "blogle/latchkey"}, "pull_request": {
            "head": {"ref": "mergify/merge-queue/main/pr-1", "sha": SHA_A,
                     "repo": {"full_name": "blogle/latchkey"}},
            "base": {"ref": "master"},
            "user": {"login": "mergify[bot]"}}}
        env = {"GITHUB_REPOSITORY": "blogle/latchkey", "REQUESTED_SHA": SHA_A,
               "EVENT_NAME": "pull_request"}
        validate_event(event, env, SHA_A)

    def test_spoofed_branch_prefix_rejected(self):
        event = {"repository": {"full_name": "blogle/latchkey"}, "pull_request": {
            "head": {"ref": "mergify/merge-queue/fake", "sha": SHA_A,
                     "repo": {"full_name": "blogle/latchkey"}}, "user": {"login": "attacker"}}}
        with self.assertRaisesRegex(ValueError, "Mergify GitHub App"):
            validate_event(event, {"GITHUB_REPOSITORY": "blogle/latchkey", "REQUESTED_SHA": SHA_A,
                                   "EVENT_NAME": "pull_request"}, SHA_A)

    def test_changed_sha_or_base_invalidates(self):
        event = {"action": "synchronize", "repository": {"full_name": "blogle/latchkey"},
                 "pull_request": {"head": {"sha": SHA_A, "repo": {"full_name": "blogle/latchkey"}},
                                  "base": {"ref": "master"}}}
        with self.assertRaisesRegex(ValueError, "SHA"):
            validate_pr(event, "blogle/latchkey", SHA_B, SHA_A)
        with self.assertRaisesRegex(ValueError, "target"):
            event["pull_request"]["base"]["ref"] = "develop"
            validate_pr(event, "blogle/latchkey", SHA_A, SHA_A)
        prefixes = [{"head_sha": SHA_A, "tree": TREE, "result": "passed", "version": "0.0.1", "suites": ["bootstrap-binary"]}]
        with self.assertRaisesRegex(ValueError, "tree"):
            candidate.validate_batch(SHA_B, SHA_C, [SHA_A], prefixes)

    def test_reordered_batch_rejected(self):
        prefixes = [{"head_sha": SHA_B, "tree": TREE, "result": "passed", "version": "0.0.2", "suites": ["bootstrap-binary"]},
                    {"head_sha": SHA_A, "tree": TREE, "result": "passed", "version": "0.0.1", "suites": ["bootstrap-binary"]}]
        with self.assertRaisesRegex(ValueError, "order"):
            candidate.validate_batch(SHA_C, TREE, [SHA_A, SHA_B], prefixes)

    def test_missing_cancelled_and_failed_child_checks_rejected(self):
        for children, expected_message in [([], "missing"),
                ([{"name": "prefix-1", "status": "cancelled"}], "cancelled"),
                ([{"name": "prefix-1", "status": "completed", "conclusion": "failure"}], "not completed")]:
            with self.subTest(expected_message=expected_message), self.assertRaisesRegex(ValueError, expected_message):
                candidate.validate_child_checks(children, ["prefix-1"])

    def test_prefix_validation_rejects_b_fixing_failing_a(self):
        # Prefixes are independent records; a passing later aggregate cannot
        # substitute for the failed evidence of the earlier squash commit.
        prefixes = [{"head_sha": SHA_A, "tree": TREE, "result": "failed", "version": "0.0.1", "suites": ["bootstrap-binary"]},
                    {"head_sha": SHA_B, "tree": TREE, "result": "passed", "version": "0.0.2", "suites": ["bootstrap-binary"]}]
        with self.assertRaisesRegex(ValueError, "every prefix must pass"):
            candidate.validate_batch(SHA_C, TREE, [SHA_A, SHA_B], prefixes)

    def test_cache_reuse_and_lock_change_invalidation(self):
        key = candidate.cache_key(TREE, "lock-1", "rust-1.96", "default", "ci", "0.0.1")
        self.assertEqual(key, candidate.cache_key(TREE, "lock-1", "rust-1.96", "default", "ci", "0.0.1"))
        for dimensions in [("e" * 40, "lock-1", "rust-1.96", "default", "ci", "0.0.1"),
                           (TREE, "lock-2", "rust-1.96", "default", "ci", "0.0.1"),
                           (TREE, "lock-1", "rust-1.97", "default", "ci", "0.0.1"),
                           (TREE, "lock-1", "rust-1.96", "features-x", "ci", "0.0.1"),
                           (TREE, "lock-1", "rust-1.96", "default", "release", "0.0.1"),
                           (TREE, "lock-1", "rust-1.96", "default", "ci", "0.0.2")]:
            self.assertNotEqual(key, candidate.cache_key(*dimensions))
        self.assertNotEqual(candidate.cache_key(TREE, "lock-1", "rust-1.96", "default", "ci", "0.0.1,0.0.2"),
                            candidate.cache_key(TREE, "lock-1", "rust-1.96", "default", "ci", "0.0.2,0.0.1"))

    def test_real_git_batch_reconstructs_two_ordered_prefixes(self):
        temp, repo, base, a, b, tree = self._repo()
        self.addCleanup(temp.cleanup)
        prefixes = candidate.reconstruct_prefixes(repo, base, [
            {"head_sha": a, "base_sha": base}, {"head_sha": b, "base_sha": a}], tree)
        self.assertEqual([item["head_sha"] for item in prefixes], [a, b])
        self.assertEqual(prefixes[-1]["tree"], tree)
        self.assertEqual(len(subprocess.run(["git", "rev-list", "--parents", "-n", "1", prefixes[1]["commit"]],
                                            cwd=repo, check=True, text=True, capture_output=True).stdout.split()), 2)

    def test_real_git_final_tree_mismatch_and_bad_prefix_reject(self):
        temp, repo, base, a, b, tree = self._repo()
        self.addCleanup(temp.cleanup)
        batch = [{"head_sha": a, "base_sha": base}, {"head_sha": b, "base_sha": a}]
        with self.assertRaisesRegex(ValueError, "final reconstructed tree"):
            candidate.reconstruct_prefixes(repo, base, batch, "f" * 40)
        prefixes = candidate.reconstruct_prefixes(repo, base, batch, tree)
        evidence = [{"head_sha": prefix["head_sha"], "tree": prefix["tree"],
                     "version": f"0.0.{index}", "suites": ["foundation"],
                     "result": "failed" if index == 1 else "passed"}
                    for index, prefix in enumerate(prefixes, 1)]
        with self.assertRaisesRegex(ValueError, "every prefix must pass"):
            candidate.validate_batch(base, tree, [a, b], evidence)

    def test_manual_dispatch_is_allowlisted_and_exact(self):
        batch = {"base_sha": SHA_B, "requested_tree": TREE,
                 "pull_requests": [{"number": 1, "head_sha": SHA_A, "base_sha": SHA_B}]}
        event = {"repository": {"full_name": "blogle/latchkey"}, "inputs": {
            "requested_sha": SHA_A, "batch_json": json.dumps(batch)}}
        env = {"GITHUB_REPOSITORY": "blogle/latchkey", "REQUESTED_SHA": SHA_A,
               "EVENT_NAME": "workflow_dispatch", "EVENT_REF": "refs/heads/master",
               "GITHUB_ACTOR": "trusted", "TRUSTED_DISPATCH_ACTORS": "trusted"}
        validate_event(event, env, SHA_A)
        env["GITHUB_ACTOR"] = "untrusted"
        with self.assertRaisesRegex(ValueError, "actor"):
            validate_event(event, env, SHA_A)

    def test_cli_executes_two_independent_prefixes_and_writes_hashed_evidence(self):
        temp = tempfile.TemporaryDirectory(prefix="candidate-cli-")
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        repo = root / "repo"
        repo.mkdir()
        def git(*args):
            return subprocess.run(["git", *args], cwd=repo, check=True, text=True,
                                  capture_output=True).stdout.strip()
        git("init", "-q")
        git("config", "user.name", "fixture")
        git("config", "user.email", "fixture@example.invalid")
        (repo / "Cargo.toml").write_text('[package]\nname = "latchkey"\nversion = "0.1.0"\nedition = "2024"\n')
        (repo / "Cargo.lock").write_text('version = 4\n\n[[package]]\nname = "latchkey"\nversion = "0.1.0"\n')
        (repo / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.96.0"\n')
        (repo / ".changes").mkdir()
        (repo / "scripts/ci").mkdir(parents=True)
        (repo / "ci").mkdir()
        shutil = __import__("shutil")
        shutil.copy(ROOT / "scripts/ci/candidate.py", repo / "scripts/ci/candidate.py")
        shutil.copy(ROOT / "scripts/ci/inject_root_version.py", repo / "scripts/ci/inject_root_version.py")
        shutil.copy(ROOT / "ci/capabilities.toml", repo / "ci/capabilities.toml")
        (repo / "scripts/release.py").write_text('''import json, subprocess, sys
commit = sys.argv[sys.argv.index("--commit") + 1]
tree = subprocess.check_output(["git", "rev-parse", commit + "^{tree}"], text=True).strip()
files = subprocess.check_output(["git", "ls-tree", "-r", "--name-only", commit, "--", ".changes"], text=True).splitlines()
print(json.dumps({"tree": tree, "version": f"1.2.{len(files)}"}))
''')
        git("add", ".")
        git("commit", "-qm", "base")
        base = git("rev-parse", "HEAD")
        heads = []
        for filename in ("LATCH-1.toml", "LATCH-2.toml"):
            (repo / ".changes" / filename).write_text('category = "Added"\nsummary = "fixture"\n')
            git("add", ".")
            git("commit", "-qm", filename)
            heads.append(git("rev-parse", "HEAD"))
        tree = git("rev-parse", "HEAD^{tree}")
        trusted = root / "trusted"
        (trusted / "scripts/ci").mkdir(parents=True)
        (trusted / "ci").mkdir()
        shutil.copy(ROOT / "scripts/ci/inject_root_version.py", trusted / "scripts/ci/inject_root_version.py")
        shutil.copy(ROOT / "ci/capabilities.toml", trusted / "ci/capabilities.toml")
        shutil.copy(ROOT / "justfile", trusted / "justfile")
        (trusted / "release-policy.toml").write_text("fixture policy\n")
        (trusted / "scripts/release.py").write_text('''import json, subprocess, sys
root = sys.argv[sys.argv.index("--root") + 1]
commit = sys.argv[sys.argv.index("--commit") + 1]
tree = subprocess.check_output(["git", "-C", root, "rev-parse", commit + "^{tree}"], text=True).strip()
files = subprocess.check_output(["git", "-C", root, "ls-tree", "-r", "--name-only", commit, "--", ".changes"], text=True).splitlines()
print(json.dumps({"tree": tree, "version": f"9.8.{len(files)}"}))
''')
        batch = root / "batch.json"
        batch.write_text(json.dumps({"schema":"latchkey-candidate-batch/v1", "base_sha":base,
            "requested_tree":tree,"pull_requests":[{"number":1,"head_sha":heads[0],"base_sha":base},
                {"number":2,"head_sha":heads[1],"base_sha":heads[0]}]}))
        tools = root / "tools"
        tools.mkdir()
        cargo = tools / "cargo"
        cargo.write_text('#!/usr/bin/env python3\nimport os,pathlib,re\nv=re.search(r\'version = "([^\"]+)"\',pathlib.Path("Cargo.toml").read_text()).group(1)\np=pathlib.Path(os.environ["CARGO_TARGET_DIR"])/"ci/latchkey"\np.parent.mkdir(parents=True,exist_ok=True)\np.write_text("#!/bin/sh\\nif [ \\"$1\\" = --version ]; then echo latchkey '+"' + v + '"+'; else echo help; fi\\n")\np.chmod(0o755)\n')
        cargo.chmod(0o755)
        cargo.write_text('''#!/usr/bin/env python3
import os, pathlib, re
marker = os.environ.get("TOKEN_MARKER")
if marker: pathlib.Path(marker).write_text(os.environ.get("GITHUB_TOKEN", "") + "|" + os.environ.get("GH_TOKEN", ""))
version = re.search(r'version = "([^"]+)"', pathlib.Path("Cargo.toml").read_text()).group(1)
binary = pathlib.Path(os.environ["CARGO_TARGET_DIR"]) / "ci/latchkey"
binary.parent.mkdir(parents=True, exist_ok=True)
binary.write_text("#!/bin/sh\\nif [ \\"$1\\" = --version ]; then echo latchkey " + version + "; else echo help; fi\\n")
binary.chmod(0o755)
''')
        cargo.chmod(0o755)
        just = tools / "just"
        just.write_text('#!/usr/bin/env python3\nimport os, sys\nfrom pathlib import Path\np=os.environ.get("TOKEN_MARKER")\nif p: Path(p).write_text(os.environ.get("GITHUB_TOKEN", "") + "|" + os.environ.get("GH_TOKEN", ""))\nsys.exit(1 if os.environ.get("FAIL_GATE") in sys.argv else 0)\n')
        just.chmod(0o755)
        evidence = root / "candidate-evidence"
        env = os.environ.copy()
        env.update({"PATH": f"{tools}:{env['PATH']}", "GITHUB_RUN_ID":"42",
                    "GITHUB_TOKEN":"must-not-leak", "GH_TOKEN":"also-must-not-leak",
                    "CANDIDATE_TEST_SUITES": '["just candidate-override"]',
                    "TOKEN_MARKER":str(root / "token-marker")})
        command = [sys.executable, str(repo / "scripts/ci/candidate.py"), "run", "--repository", str(repo),
                   "--trusted", str(trusted), "--source", str(repo), "--batch", str(batch), "--requested-sha", heads[-1],
                   "--target-dir", str(root / "target"), "--evidence", str(evidence)]
        subprocess.run(command, cwd=repo, env=env, check=True)
        manifest = json.loads((evidence / "manifest.json").read_text())
        versions = [p["release_version"] for p in manifest["ordered_prs"]]
        self.assertTrue(all(version.startswith("9.8.") for version in versions))
        self.assertNotEqual(versions[0], versions[1])
        self.assertEqual([p["head_sha"] for p in manifest["ordered_prs"]], heads)
        for prefix in manifest["ordered_prs"]:
            artifact = evidence / prefix["artifact"]["filename"]
            self.assertEqual(candidate.digest(artifact.read_bytes()), prefix["artifact"]["sha256"])
        self.assertTrue((evidence / "SHA256SUMS").is_file())
        self.assertNotIn("must-not-leak", (root / "token-marker").read_text())
        suite = tools / "foundation-check"
        suite.write_text('#!/usr/bin/env python3\nfrom pathlib import Path\nimport sys\nsys.exit(1 if len(list(Path(".changes").glob("*.toml"))) == 1 else 0)\n')
        suite.chmod(0o755)
        failed_evidence = root / "failed-evidence"
        failing_env = env.copy()
        failing_env["FAIL_GATE"] = "fmt-check"
        failed_command = command.copy()
        failed_command[failed_command.index(str(evidence))] = str(failed_evidence)
        failed = subprocess.run(failed_command, cwd=repo, env=failing_env, capture_output=True)
        self.assertNotEqual(failed.returncode, 0)
        self.assertFalse((failed_evidence / "manifest.json").exists())

    def test_candidate_capabilities_cannot_remove_bootstrap_binary(self):
        with tempfile.TemporaryDirectory(prefix="capability-policy-") as temp:
            root = Path(temp)
            trusted = root / "trusted"
            source = root / "candidate"
            (trusted / "ci").mkdir(parents=True)
            (source / "ci").mkdir(parents=True)
            (trusted / "ci/capabilities.toml").write_text('''schema_version=1
gates_are_cumulative=true
[[stages]]
id="foundation"
gates=[{id="bootstrap-binary", command="just build", owner="LATCH-3"}]
''')
            (source / "ci/capabilities.toml").write_text('''schema_version=1
gates_are_cumulative=true
[[stages]]
id="foundation"
gates=[]
''')
            git = lambda *args: subprocess.run(["git", *args], cwd=source, check=True,
                                               text=True, capture_output=True).stdout.strip()
            git("init", "-q")
            git("config", "user.name", "fixture")
            git("config", "user.email", "fixture@example.invalid")
            git("add", ".")
            git("commit", "-qm", "candidate")
            with self.assertRaisesRegex(ValueError, "removed/altered trusted gate bootstrap-binary"):
                candidate._capability_suites(str(trusted), str(source), git("rev-parse", "HEAD"))


if __name__ == "__main__":
    unittest.main()
