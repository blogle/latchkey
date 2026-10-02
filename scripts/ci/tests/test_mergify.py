from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[3]


class MergifyContractTests(unittest.TestCase):
    def test_merge_queue_contract_has_one_compatible_default_path(self):
        config = (ROOT / ".mergify.yml").read_text()
        queue_section = config.split("queue_rules:\n", 1)[1].split("\nmerge_queue:", 1)[0]
        queues = re.findall(
            r"(?ms)^  - name: (\S+)\n(.*?)(?=^  - name: |\Z)", queue_section
        )
        self.assertEqual([name for name, _ in queues], ["barrier", "default"])

        barrier, default = (body for _, body in queues)
        self.assertIn("queue_conditions: &queue_conditions", barrier)
        self.assertIn("- base=master", barrier)
        self.assertIn("- check-success=@github-actions/pr-fast", barrier)
        self.assertIn("merge_conditions: &merge_conditions", barrier)
        self.assertIn("- check-success=@github-actions/candidate-ready", barrier)
        self.assertIn("batch_size: 1", barrier)
        self.assertIn("queue_conditions: *queue_conditions", default)
        self.assertIn("merge_conditions: *merge_conditions", default)
        self.assertIn("batch_size: 3", default)

        for queue in (barrier, default):
            self.assertIn("merge_method: squash", queue)
            self.assertIn("update_method: rebase", queue)

        pull_requests = config.split("pull_request_rules:\n", 1)[1]
        pull_request_rules = re.findall(
            r"(?ms)^  - name: .*?\n(.*?)(?=^  - name: |\Z)", pull_requests
        )
        self.assertEqual(len(pull_request_rules), 2)
        for rule in pull_request_rules:
            self.assertIn("- base=master", rule)
            self.assertRegex(rule, r"(?m)^\s+queue:\n\s+name: (?:barrier|default)$")
            self.assertNotRegex(rule, r"(?m)^\s+(?:merge|squash|rebase):")

        self.assertEqual(len(re.findall(r"(?m)^\s+queue:\s*$", config)), 2)
        self.assertNotRegex(config, r"(?m)^\s+merge:\s*$")


if __name__ == "__main__":
    unittest.main()
