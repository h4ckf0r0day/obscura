import contextlib
import io
import json
import signal
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import perf_smoke


class ShippingArtifactTests(unittest.TestCase):
    def test_candidate_is_staged_before_workspace_test_features_are_enabled(self):
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml").read_text()
        build = workflow.index("- name: Build render release")
        stage = workflow.index("- name: Stage candidate binary for comparison")
        tests = workflow.index("- name: Test render configuration")
        self.assertLess(build, stage)
        self.assertLess(stage, tests,
                        "workspace nextest can overwrite the CLI with dev-dependency features")


class ProcessMeasurementTests(unittest.TestCase):
    def test_exit_wait_is_not_polled_and_keeps_limits_and_cleanup(self):
        process = Mock(pid=12345, returncode=0)
        process.wait.return_value = 0
        commands = []

        def start(command, **kwargs):
            commands.append(command)
            kwargs["stdout"].write(b"5000\n")
            kwargs["stdout"].flush()
            self.assertTrue(kwargs["start_new_session"])
            return process

        with patch.object(perf_smoke.subprocess, "Popen", side_effect=start), \
                patch.object(perf_smoke.os, "wait4", return_value=(
                    process.pid, 0, Mock(ru_utime=0.123456, ru_stime=0.027778, ru_maxrss=1024),
                )) as wait, \
                patch.object(perf_smoke.os, "killpg") as cleanup:
            _, rss, cpu = perf_smoke.run_once(Path("/fake/engine"), "http://localhost/", "5000", 5000)
        self.assertEqual(rss, 1024)
        self.assertAlmostEqual(cpu, 0.151234)
        wait.assert_called_once_with(process.pid, 0)
        process.wait.assert_not_called()
        self.assertEqual(commands[0][:4], ["timeout", "--signal=KILL", "45", "prlimit"])
        self.assertNotIn("/usr/bin/time", commands[0])
        self.assertIn("--fsize=2097152", commands[0])
        self.assertIn("--nofile=256", commands[0])
        cleanup.assert_called_once_with(process.pid, signal.SIGKILL)

    def test_hung_process_is_still_bounded_without_polling(self):
        with tempfile.TemporaryDirectory() as directory:
            engine = Path(directory) / "engine"
            engine.write_text(f"#!{sys.executable}\nimport time\ntime.sleep(3)\nprint(5000)\n")
            engine.chmod(0o755)
            started = time.monotonic()
            with patch.object(perf_smoke, "PROCESS_TIMEOUT_SECONDS", 0.1), \
                    self.assertRaisesRegex(RuntimeError, "timed out|exited with status"):
                perf_smoke.run_once(engine, "http://localhost/", "5000", 5000)
            self.assertLess(time.monotonic() - started, 2.0)


class RegressionGateTests(unittest.TestCase):
    def compare(self, base_latency, candidate_latency, base_cpu, candidate_cpu,
                base_rss=40960, candidate_rss=40960):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            baseline, candidate, output = root / "base", root / "candidate", root / "report.json"
            baseline.touch()
            candidate.touch()
            clock = [0.0]
            measured_cpu = [0.0]
            measured_rss = [0]
            rounds = {False: 0, True: 0}

            def sample(values, index):
                return values[index] if isinstance(values, list) else values

            def start(command, **kwargs):
                changed = str(candidate) in command
                index = rounds[changed]
                rounds[changed] += 1
                clock[0] += sample(candidate_latency if changed else base_latency, index) / 1000
                measured_cpu[0] = sample(candidate_cpu if changed else base_cpu, index)
                measured_rss[0] = sample(candidate_rss if changed else base_rss, index)
                kwargs["stdout"].write(b"5\n")
                kwargs["stdout"].flush()
                process = Mock(pid=12345, returncode=0)
                process.wait.return_value = 0
                return process

            def wait(pid, _flags):
                return pid, 0, Mock(ru_utime=measured_cpu[0], ru_stime=0.0, ru_maxrss=measured_rss[0])

            logs = io.StringIO()
            error = None
            with patch.object(sys, "argv", ["perf_smoke.py", "--base", str(baseline), "--candidate", str(candidate), "--output", str(output)]), \
                    patch.object(perf_smoke, "SCENARIOS", {"probe": ("5", 5)}), \
                    patch.object(perf_smoke.subprocess, "Popen", side_effect=start), \
                    patch.object(perf_smoke.os, "wait4", side_effect=wait), \
                    patch.object(perf_smoke.time, "perf_counter", side_effect=lambda: clock[0]), \
                    patch.object(perf_smoke.os, "killpg"), \
                    patch.dict(perf_smoke.os.environ, {"GITHUB_STEP_SUMMARY": ""}), \
                    contextlib.redirect_stdout(logs):
                try:
                    perf_smoke.main()
                except SystemExit as failure:
                    error = str(failure)
            return error, json.loads(output.read_text()), logs.getvalue()

    def test_common_drift_does_not_compare_different_pairs(self):
        error, report, _ = self.compare(
            [100, 100, 100, 112, 112], [100, 100, 112, 112, 112],
            [0.2, 0.2, 0.2, 0.23, 0.23], [0.2, 0.2, 0.23, 0.23, 0.23],
            [40000, 40000, 40000, 50000, 50000],
            [40000, 40000, 50000, 50000, 50000],
        )
        self.assertIsNone(error)
        # Arm medians remain useful reporting fields even when phases differ.
        self.assertAlmostEqual(report["scenarios"]["probe"]["latency_ratio"], 1.12)

    def test_fifteen_percent_steady_latency_regression_is_rejected(self):
        error, report, logs = self.compare(1000, 1150, 1.0, 1.0)
        self.assertIsNotNone(error)
        self.assertIn("::error::probe latency", logs)
        self.assertAlmostEqual(report["scenarios"]["probe"]["latency_ratio"], 1.15)

    def test_cpu_regression_is_rejected_when_wall_time_is_unchanged(self):
        error, _, logs = self.compare(1000, 1000, 1.0, 1.15)
        self.assertIsNotNone(error)
        self.assertIn("::error::probe CPU", logs)

    def test_sustained_rss_regression_is_rejected(self):
        error, _, logs = self.compare(1000, 1000, 1.0, 1.0, 40000, 50000)
        self.assertIsNotNone(error)
        self.assertIn("::error::probe RSS", logs)

    def test_majority_pair_regressions_are_not_hidden_by_arm_medians(self):
        cases = [
            ("latency", {"base_latency": [100, 100, 1000, 1000, 1000],
                         "candidate_latency": [115, 115, 1150, 1000, 1000]}),
            ("CPU", {"base_cpu": [0.2, 0.2, 1.0, 1.0, 1.0],
                     "candidate_cpu": [0.23, 0.23, 1.15, 1.0, 1.0]}),
            ("RSS", {"base_rss": [40000, 40000, 100000, 100000, 100000],
                     "candidate_rss": [50000, 50000, 125000, 100000, 100000]}),
        ]
        for metric, changes in cases:
            with self.subTest(metric=metric):
                values = dict(base_latency=1000, candidate_latency=1000,
                              base_cpu=1.0, candidate_cpu=1.0)
                values.update(changes)
                error, _, logs = self.compare(**values)
                self.assertIsNotNone(error)
                self.assertIn(f"::error::probe {metric}", logs)

    def test_relative_and_absolute_limits_must_hold_in_the_same_majority(self):
        error, report, _ = self.compare(
            [10, 10, 1000, 1000, 100], [12, 12, 1050, 1050, 120],
            [0.01, 0.01, 1.0, 1.0, 0.2], [0.012, 0.012, 1.05, 1.05, 0.24],
            [1000, 1000, 100000, 100000, 40000],
            [1200, 1200, 110000, 110000, 50000],
        )
        self.assertIsNone(error)
        for metric in ("latency_ms", "cpu_seconds", "rss_kib"):
            self.assertEqual(report["scenarios"]["probe"]["paired"][metric]["exceeding_pairs"], 1)

    def test_an_isolated_spike_does_not_trigger_a_regression(self):
        error, _, _ = self.compare(
            1000, [1000, 1000, 2000, 1000, 1000],
            1.0, [1.0, 1.0, 2.0, 1.0, 1.0],
            40000, [40000, 40000, 80000, 40000, 40000],
        )
        self.assertIsNone(error)

    def test_absolute_floors_keep_short_timing_and_cpu_quantization_noise(self):
        error, _, _ = self.compare(50, 59, 0.02, 0.04, 10000, 18000)
        self.assertIsNone(error)

    def test_relative_limits_still_apply_above_absolute_floors(self):
        error, _, _ = self.compare(1000, 1050, 1.0, 1.05, 100000, 110000)
        self.assertIsNone(error)

    def test_precise_cpu_is_not_rounded_into_a_false_regression(self):
        error, report, _ = self.compare(300, 310, 0.2591, 0.2799)
        self.assertIsNone(error)
        self.assertEqual(report["scenarios"]["probe"]["base_cpu_seconds"], 0.2591)
        self.assertEqual(report["scenarios"]["probe"]["candidate_cpu_seconds"], 0.2799)

    def test_small_cpu_regression_still_exceeds_unchanged_relative_and_absolute_gates(self):
        error, _, logs = self.compare(300, 300, 0.25, 0.276)
        self.assertIsNotNone(error)
        self.assertIn("::error::probe CPU", logs)

    def test_negative_or_nonfinite_cpu_measurement_is_rejected(self):
        for value in (-1, float("nan"), float("inf")):
            with self.subTest(value=value), self.assertRaisesRegex(RuntimeError, "invalid CPU"):
                self.compare(100, 100, value, 0.12)

    def test_nonpositive_or_nonfinite_latency_is_rejected(self):
        for value in (0, -1, float("nan"), float("inf")):
            with self.subTest(value=value), self.assertRaisesRegex(RuntimeError, "invalid latency"):
                self.compare(value, 100, 0.12, 0.12)

    def test_nonpositive_or_nonfinite_rss_is_rejected(self):
        for value in (0, -1, float("nan"), float("inf")):
            with self.subTest(value=value), self.assertRaisesRegex(RuntimeError, "invalid CPU/RSS"):
                self.compare(100, 100, 0.12, 0.12, value, 40960)

    def test_zero_cpu_respects_the_absolute_floor_without_nonfinite_json(self):
        for candidate_cpu, fails in ((0.0, False), (0.01, False), (0.03, True)):
            with self.subTest(candidate_cpu=candidate_cpu):
                error, report, _ = self.compare(100, 100, 0.0, candidate_cpu)
                self.assertEqual(error is not None, fails)
                json.dumps(report, allow_nan=False)

    def test_paired_statistics_keep_round_alignment_and_all_samples(self):
        error, report, logs = self.compare(
            [100, 200, 300, 400, 500], [101, 202, 303, 404, 505], 0.12, 0.12,
        )
        self.assertIsNone(error)
        row = report["scenarios"]["probe"]
        for arm, expected in (("base", [100, 200, 300, 400, 500]),
                              ("candidate", [101, 202, 303, 404, 505])):
            actual = row["samples"][arm]["latency_ms"]
            self.assertEqual(len(actual), 5)
            for measured, wanted in zip(actual, expected):
                self.assertAlmostEqual(measured, wanted)
        paired = row["paired"]["latency_ms"]
        for measured, wanted in zip(paired["deltas"], [1, 2, 3, 4, 5]):
            self.assertAlmostEqual(measured, wanted)
        for measured in paired["ratios"]:
            self.assertAlmostEqual(measured, 1.01)
        self.assertAlmostEqual(paired["median_delta"], 3)
        self.assertEqual(paired["exceeding_pairs"], 0)
        self.assertIn("Paired change", logs)

    def test_cpu_medians_and_samples_are_preserved_in_report(self):
        error, report, _ = self.compare(100, 100, 0.12, 0.12)
        self.assertIsNone(error)
        row = report["scenarios"]["probe"]
        self.assertIn("base_cpu_seconds", row)
        self.assertIn("candidate_cpu_seconds", row)
        self.assertAlmostEqual(row["base_cpu_seconds"], 0.12)
        self.assertEqual(row["samples"]["base"]["cpu_seconds"], [0.12] * 5)


if __name__ == "__main__":
    unittest.main()
