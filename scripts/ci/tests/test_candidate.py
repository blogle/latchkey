import sys
import unittest
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
    def test_ordinary_same_repo_pr_exact_sha(self):
        event = {"action": "synchronize", "repository": {"full_name": "blogle/latchkey"},
                 "pull_request": {"head": {"sha": SHA_A, "repo": {"full_name": "blogle/latchkey"}},
                                  "base": {"ref": "master"}}}
        validate_pr(event, "blogle/latchkey", SHA_A, SHA_A)

    def test_legitimate_queue_pr(self):
        event = {"repository": {"full_name": "blogle/latchkey"}, "pull_request": {
            "head": {"ref": "mergify/merge-queue/main/pr-1", "sha": SHA_A},
            "user": {"login": "mergify[bot]"}}}
        env = {"GITHUB_REPOSITORY": "blogle/latchkey", "REQUESTED_SHA": SHA_A,
               "EVENT_NAME": "pull_request"}
        validate_event(event, env, SHA_A)

    def test_spoofed_branch_prefix_rejected(self):
        event = {"repository": {"full_name": "blogle/latchkey"}, "pull_request": {
            "head": {"ref": "mergify/merge-queue/fake", "sha": SHA_A}, "user": {"login": "attacker"}}}
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
        self.assertNotEqual(key, candidate.cache_key(TREE, "lock-2", "rust-1.96", "default", "ci", "0.0.1"))

    def test_manual_dispatch_is_allowlisted_and_exact(self):
        event = {"repository": {"full_name": "blogle/latchkey"}, "inputs": {"requested_sha": SHA_A}}
        env = {"GITHUB_REPOSITORY": "blogle/latchkey", "REQUESTED_SHA": SHA_A,
               "EVENT_NAME": "workflow_dispatch", "EVENT_REF": "refs/heads/master",
               "GITHUB_ACTOR": "trusted", "TRUSTED_DISPATCH_ACTORS": "trusted"}
        validate_event(event, env, SHA_A)
        env["GITHUB_ACTOR"] = "untrusted"
        with self.assertRaisesRegex(ValueError, "actor"):
            validate_event(event, env, SHA_A)


if __name__ == "__main__":
    unittest.main()
