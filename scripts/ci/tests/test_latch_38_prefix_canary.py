import os
import unittest


class CandidatePrefixCanaryTests(unittest.TestCase):
    def test_fails_only_in_trusted_ci_profile(self):
        self.assertNotEqual(
            os.environ.get("CARGO_PROFILE"),
            "ci",
            "LATCH-38 intentional candidate-prefix failure",
        )


if __name__ == "__main__":
    unittest.main()
