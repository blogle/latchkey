from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[3]


class WorkflowBootstrapTests(unittest.TestCase):
    def test_pr_workflow_validates_event_inline_then_uses_trusted_release_validator(self):
        workflow = (ROOT / ".github/workflows/pr.yml").read_text()
        self.assertNotIn("\n    if:", workflow)
        self.assertIn("Validate PR provenance inline before candidate checkout", workflow)
        self.assertIn('pr.get("head", {}).get("repo", {}).get("full_name") != repository', workflow)
        self.assertIn('pr.get("base", {}).get("ref") != "master"', workflow)
        self.assertIn('pr.get("head", {}).get("sha") != requested', workflow)
        self.assertNotIn("scripts/ci/validate_pr.py", workflow)
        self.assertIn("python3 scripts/release.py --root", workflow)
        self.assertIn(
            "if: ${{ !startsWith(github.event.pull_request.head.ref, 'mergify/merge-queue/') }}\n"
            "        shell: bash\n        env:",
            workflow,
        )
        self.assertIn("--working-directory \"$GITHUB_WORKSPACE/candidate\" pr-check", workflow)
        self.assertIn("--working-directory \"$GITHUB_WORKSPACE/candidate\" test-integration contracts", workflow)

    def test_candidate_bootstrap_and_credentials_are_narrowly_scoped(self):
        workflow = (ROOT / ".github/workflows/candidate.yml").read_text()
        self.assertIn("Mergifyio/setup-cli@v1.3.0", workflow)
        self.assertNotIn("Mergifyio/setup-cli@v2", workflow)
        self.assertIn("Validate event inline before candidate checkout", workflow)
        for check in (
            'pr.get("head", {}).get("sha") != requested',
            'pr.get("base", {}).get("ref") != "master"',
            'pr.get("user", {}).get("login") != "mergify[bot]"',
            'os.environ["EVENT_REF"] != "refs/heads/master"',
            'os.environ.get("TRUSTED_DISPATCH_ACTORS", "").split(",")',
            'os.environ["GITHUB_ACTOR"] not in trusted',
        ):
            self.assertIn(check, workflow)
        self.assertNotIn("scripts/ci/validate_candidate_event.py", workflow)
        self.assertIn("working-directory: candidate", workflow)
        self.assertIn('mergify ci queue-info > "$RUNNER_TEMP/queue-info.json"', workflow)
        self.assertIn('resolver="$GITHUB_WORKSPACE/scripts/ci/resolve_candidate_batch.py"', workflow)
        self.assertIn('resolver="$GITHUB_WORKSPACE/candidate/scripts/ci/resolve_candidate_batch.py"', workflow)
        self.assertIn('candidate_script="$GITHUB_WORKSPACE/candidate/scripts/ci/candidate.py"', workflow)
        self.assertIn("GH_TOKEN: ${{ github.token }}", workflow)
        self.assertIn("pull-requests: read", workflow)


if __name__ == "__main__":
    unittest.main()
