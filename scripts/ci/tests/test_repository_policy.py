import importlib.util
import json
import re
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "repository-policy.py"
SPEC = importlib.util.spec_from_file_location("repository_policy", SCRIPT)
policy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(policy)


def initial_snapshot():
    return {
        "schema": "latchkey-repository-policy-snapshot/v1",
        "repo": "blogle/latchkey",
        "default_branch": "master",
        "settings": {"allow_squash_merge": True, "allow_merge_commit": True,
                      "allow_rebase_merge": True, "allow_auto_merge": False,
                      "delete_branch_on_merge": False},
        "branch_protection": None,
        "rulesets": [],
    }


class FakeGh:
    def __init__(self, snapshot=None, admin=True):
        self.snapshot = snapshot or initial_snapshot()
        self.admin = admin
        self.writes = []

    def request(self, method, endpoint, data=None):
        if method == "GET" and endpoint.endswith("/branches/master/protection"):
            return self.snapshot["branch_protection"]
        if method == "GET" and endpoint.endswith("/rulesets?includes_parents=false&per_page=100"):
            return self.snapshot["rulesets"]
        if method == "GET" and "/rulesets/" in endpoint:
            ruleset_id = int(endpoint.rsplit("/", 1)[1])
            return next(row for row in self.snapshot["rulesets"] if row.get("id") == ruleset_id)
        if method == "GET" and endpoint == "repos/blogle/latchkey":
            return {"default_branch": self.snapshot["default_branch"],
                    **self.snapshot["settings"], "permissions": {"admin": self.admin}}
        self.writes.append((method, endpoint, data))
        return {"id": 99}


class RepositoryPolicyTests(unittest.TestCase):
    def test_mergify_shared_interface_barriers_are_symmetric_and_narrow(self):
        config = (Path(__file__).resolve().parents[3] / ".mergify.yml").read_text(
            encoding="utf-8"
        )
        positive = re.search(r"^\s*- files ~= (.+)$", config, re.MULTILINE)
        negative = re.search(r"^\s*- -files ~= (.+)$", config, re.MULTILINE)
        self.assertIsNotNone(positive)
        self.assertIsNotNone(negative)
        self.assertEqual(positive.group(1), negative.group(1))
        barrier = re.compile(positive.group(1))

        for path in (
            "release-policy.toml",
            "docs/contracts.md",
            ".changes/README.md",
            "docs/ci.md",
            "Cargo.lock",
            ".github/workflows/pr.yml",
            ".github/workflows/candidate.yml",
        ):
            with self.subTest(path=path):
                self.assertIsNotNone(barrier.search(path))

        self.assertIsNone(barrier.search("docs/architecture.md"))

    def test_snapshot_diff_is_deterministic_and_detects_settings(self):
        baseline = initial_snapshot()
        plan = policy.diff(baseline, baseline)
        self.assertEqual(plan["settings"]["allow_merge_commit"], {"before": True, "after": False})
        self.assertEqual(plan["ruleset"]["operation"], "create-or-update")

    def test_apply_requires_confirmation_elsewhere_and_stale_snapshot_refuses_writes(self):
        baseline = initial_snapshot()
        changed = dict(baseline)
        changed["settings"] = dict(baseline["settings"], allow_squash_merge=False)
        api = FakeGh(changed)
        with self.assertRaisesRegex(ValueError, "changed since snapshot"):
            policy.apply_policy(api, "blogle/latchkey", baseline)
        self.assertEqual(api.writes, [])

    def test_apply_posts_squash_rules_with_only_mergify_bypass_and_is_repeatable(self):
        api = FakeGh()
        policy.apply_policy(api, "blogle/latchkey", initial_snapshot())
        method, endpoint, body = api.writes[-1]
        self.assertEqual(method, "POST")
        self.assertEqual(endpoint, "repos/blogle/latchkey/rulesets")
        self.assertEqual(body["bypass_actors"], [{
            "actor_id": 10562, "actor_type": "Integration", "bypass_mode": "pull_request",
        }])
        checks = next(r for r in body["rules"] if r["type"] == "required_status_checks")
        self.assertEqual(checks["parameters"]["required_status_checks"],
                         [{"context": "Mergify Merge Queue", "integration_id": 10562}])
        rules = {r["type"] for r in body["rules"]}
        self.assertNotIn("merge_queue", rules)
        self.assertEqual(body["conditions"]["ref_name"]["include"], ["refs/heads/master"])

    def test_apply_is_noop_when_snapshot_already_matches_desired_policy(self):
        baseline = initial_snapshot()
        state = dict(baseline)
        state["settings"] = dict(policy.DESIRED_REPO)
        state["rulesets"] = [dict(policy.DESIRED_RULESET, id=18)]
        api = FakeGh(state)
        policy.apply_policy(api, "blogle/latchkey", baseline)
        self.assertEqual(api.writes, [])

    def test_ruleset_normalization_ignores_github_added_defaults(self):
        current = dict(policy.DESIRED_RULESET, id=18, current_user_can_bypass="never")
        current["rules"] = [
            {"type": "pull_request", "parameters": {
                **policy.DESIRED_RULESET["rules"][0]["parameters"],
                "required_reviewers": [], "require_extra_approval_for_unattributed_changes": True,
            }},
            *policy.DESIRED_RULESET["rules"][1:],
        ]
        self.assertEqual(policy.normalized_ruleset(current), policy.DESIRED_RULESET)

    def test_ruleset_normalization_accepts_only_the_exact_mergify_bypass_actor(self):
        current = dict(policy.DESIRED_RULESET, id=18, current_user_can_bypass="never")
        current["bypass_actors"] = [{
            **policy.DESIRED_RULESET["bypass_actors"][0], "actor_name": "Mergify",
        }]
        self.assertEqual(policy.normalized_ruleset(current), policy.DESIRED_RULESET)

        invalid_actor_lists = (
            [{"actor_id": 10562, "actor_type": "User", "bypass_mode": "pull_request"}],
            [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "pull_request"}],
            [policy.DESIRED_RULESET["bypass_actors"][0],
             {"actor_id": 1, "actor_type": "User", "bypass_mode": "always"}],
        )
        for actors in invalid_actor_lists:
            with self.subTest(actors=actors):
                current["bypass_actors"] = actors
                self.assertNotEqual(policy.normalized_ruleset(current), policy.DESIRED_RULESET)

    def test_apply_requires_admin(self):
        with self.assertRaisesRegex(ValueError, "admin permission"):
            policy.apply_policy(FakeGh(admin=False), "blogle/latchkey", initial_snapshot())

    def test_plan_is_read_only(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "snapshot.json"
            path.write_text(json.dumps(initial_snapshot()), encoding="utf-8")
            api = FakeGh()
            with patch.object(policy, "Gh", return_value=api), patch("builtins.print"):
                self.assertEqual(policy.main(["plan", "--snapshot", str(path)]), 0)
            self.assertEqual(api.writes, [])

    def test_ancestry_requires_single_parent_and_complete_squash_message(self):
        with patch.object(policy, "subprocess") as mocked:
            mocked.run.side_effect = [
                subprocess.CompletedProcess([], 0, "before\n", ""),
                subprocess.CompletedProcess([], 0, "LATCH-6: Merge policy (#12)\n", ""),
                subprocess.CompletedProcess([], 0, "before\n", ""),
            ]
            api = type("Api", (), {"request": lambda *_: {
                "merged": True, "merge_commit_sha": "after", "title": "LATCH-6: Merge policy"
            }})()
            policy.verify_ancestry("before", "after", 12, "blogle/latchkey", api)
            mocked.run.side_effect = [
                subprocess.CompletedProcess([], 0, "parent1 parent2\n", ""),
                subprocess.CompletedProcess([], 0, "LATCH-6 title (#12)\n", ""),
                subprocess.CompletedProcess([], 0, "parent1\n", ""),
            ]
            with self.assertRaisesRegex(ValueError, "one-parent"):
                policy.verify_ancestry("parent1", "after", 12, "blogle/latchkey", api)


if __name__ == "__main__":
    unittest.main()
