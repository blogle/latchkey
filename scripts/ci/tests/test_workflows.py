from pathlib import Path
import json
import os
import subprocess
import tempfile
import textwrap
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
        self.assertIn("extra-conf: |", workflow)
        self.assertNotIn("extra_nix_config:", workflow)

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
        self.assertIn('b.get("base_stack", []) + b["pull_requests"]', workflow)
        self.assertIn("extra-conf: |", workflow)
        self.assertNotIn("extra_nix_config:", workflow)

    def test_dispatch_candidate_sha_can_differ_from_final_pr_head_but_tree_is_exact(self):
        workflow = (ROOT / ".github/workflows/candidate.yml").read_text()
        candidate = (ROOT / "scripts/ci/candidate.py").read_text()
        candidate_tests = (ROOT / "scripts/ci/tests/test_candidate.py").read_text()

        validation_step = workflow.split("name: Validate event inline before candidate checkout", 1)[1]
        inline_validator = textwrap.dedent(
            validation_step.split("run: |\n", 1)[1].split("\n          PY", 1)[0].split("\n", 1)[1]
        )
        candidate_sha, final_pr_head = "a" * 40, "b" * 40
        event = {
            "repository": {"full_name": "blogle/latchkey"},
            "inputs": {
                "requested_sha": candidate_sha,
                "batch_json": json.dumps({
                    "base_sha": "c" * 40,
                    "requested_tree": "d" * 40,
                    "pull_requests": [{"number": 1, "head_sha": final_pr_head, "base_sha": "c" * 40}],
                }),
            },
        }
        self.assertNotEqual(candidate_sha, final_pr_head)
        with tempfile.TemporaryDirectory(prefix="candidate-workflow-event-") as temp:
            event_path = Path(temp) / "event.json"
            event_path.write_text(json.dumps(event))
            subprocess.run(
                ["python3", "-c", inline_validator],
                check=True,
                env={**os.environ, "GITHUB_EVENT_PATH": str(event_path),
                     "GITHUB_REPOSITORY": "blogle/latchkey", "GITHUB_ACTOR": "trusted",
                     "EVENT_NAME": "workflow_dispatch", "EVENT_REF": "refs/heads/master",
                     "REQUESTED_SHA": candidate_sha, "TRUSTED_DISPATCH_ACTORS": "trusted"},
            )

        self.assertNotIn('prs[-1]["head_sha"] != requested', workflow)
        self.assertNotIn("dispatch batch final head does not match requested SHA", workflow)
        self.assertIn('if not re.fullmatch(r"[0-9a-f]{40}", requested)', workflow)
        self.assertIn('for key in ("head_sha", "base_sha")', workflow)
        self.assertIn('ref: ${{ github.event.pull_request.head.sha || inputs.requested_sha }}', workflow)
        self.assertIn('test "$(git -C candidate rev-parse HEAD)" = "$REQUESTED_SHA"', workflow)
        self.assertIn('if _sha(repository, "HEAD") != args.requested_sha:', candidate)
        self.assertIn('if git(repository, "rev-parse", "HEAD^{tree}").decode().strip() != requested_tree:', candidate)
        self.assertIn('if prefixes[-1].get("tree") != requested_tree:', candidate)
        self.assertIn('"final reconstructed tree"', candidate_tests)


if __name__ == "__main__":
    unittest.main()
