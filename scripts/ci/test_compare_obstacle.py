import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class ObstacleReleaseGate(unittest.TestCase):
    def compare(self, candidate_passes, base_passes=False, base_ms=1, candidate_ms=1):
        stages = [{"name": f"stage-{n}", "pass": True, "median_ms": 1}
                  for n in range(33)]
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory) / "base.json"
            candidate = Path(directory) / "candidate.json"
            stages[0]["pass"] = base_passes
            stages[0]["median_ms"] = base_ms
            base.write_text(json.dumps({"results": stages}))
            stages[0]["pass"] = candidate_passes
            stages[0]["median_ms"] = candidate_ms
            candidate.write_text(json.dumps({"results": stages}))
            return subprocess.run(
                [sys.executable, str(Path(__file__).with_name("compare_obstacle.py")),
                 "--base", str(base), "--candidate", str(candidate)],
                capture_output=True, text=True,
                env={**os.environ, "GITHUB_STEP_SUMMARY": str(Path(directory) / "summary.md")},
            )

    def test_shared_baseline_failure_does_not_pass_the_release_gate(self):
        result = self.compare(False)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_candidate_fixing_the_baseline_failure_passes(self):
        result = self.compare(True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_large_slowdown_fails_even_when_all_stages_pass(self):
        result = self.compare(True, base_passes=True, base_ms=50, candidate_ms=500)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("stage-0", result.stdout + result.stderr)

    def test_newly_working_stage_is_not_a_performance_regression(self):
        result = self.compare(True, base_ms=50, candidate_ms=500)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_small_absolute_or_relative_changes_remain_within_noise_allowance(self):
        for base_ms, candidate_ms in [(1, 2), (1000, 1100)]:
            with self.subTest(base_ms=base_ms, candidate_ms=candidate_ms):
                result = self.compare(True, base_passes=True, base_ms=base_ms,
                                      candidate_ms=candidate_ms)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
