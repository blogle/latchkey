from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[3]


class MergifyQueuePolicyTests(unittest.TestCase):
    def test_queue_policy_avoids_injection_and_preserves_f06_gates(self):
        config = (ROOT / ".mergify.yml").read_text(encoding="utf-8")
        queues = config.split("merge_queue:", 1)[0]

        self.assertNotIn("branch_protection_injection_mode", config)
        self.assertEqual(re.findall(r"^  - name: (.+)$", queues, re.MULTILINE), ["barrier", "default"])
        self.assertIn("queue_conditions: &queue_conditions", queues)
        self.assertIn("queue_conditions: *queue_conditions", queues)
        self.assertIn("- base=master", queues)
        self.assertIn("- check-success=@github-actions/pr-fast", queues)
        self.assertIn("merge_conditions: &merge_conditions", queues)
        self.assertIn("- check-success=@github-actions/candidate-ready", queues)
        self.assertIn("merge_conditions: *merge_conditions", queues)

        pr_workflow = (ROOT / ".github/workflows/pr.yml").read_text(encoding="utf-8")
        self.assertRegex(pr_workflow, r"(?m)^  pull_request:$")
        self.assertRegex(pr_workflow, r"(?m)^    name: pr-fast$")

        for name, batch_size in (("barrier", 1), ("default", 3)):
            with self.subTest(queue=name):
                match = re.search(
                    rf"^  - name: {name}\n(?P<body>.*?)(?=^  - name:|\Z)",
                    queues,
                    re.MULTILINE | re.DOTALL,
                )
                self.assertIsNotNone(match)
                body = match.group("body")
                self.assertRegex(body, rf"(?m)^    batch_size: {batch_size}$")
                self.assertRegex(body, r"(?m)^    merge_method: squash$")
                self.assertRegex(body, r"(?m)^    update_method: rebase$")

        self.assertNotIn("bypass", queues)
        self.assertEqual(re.findall(r"^    merge_method: (.+)$", queues, re.MULTILINE), ["squash", "squash"])


if __name__ == "__main__":
    unittest.main()
