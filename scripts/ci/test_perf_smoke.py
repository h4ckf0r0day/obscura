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


class ProcessMeasurementTests(unittest.TestCase):
    def test_exit_wait_is_not_polled_and_keeps_limits_and_cleanup(self):
        process = Mock(pid=12345, returncode=0)
        process.wait.return_value = 0
        commands = []

        def start(command, **kwargs):
            commands.append(command)
            kwargs["stdout"].write(b"5000\n")
            kwargs["stdout"].flush()
            Path(command[command.index("-o") + 1]).write_text("0.12 0.03 1024\n")
            self.assertTrue(kwargs["start_new_session"])
            return process

        with patch.object(perf_smoke.subprocess, "Popen", side_effect=start), \
                patch.object(perf_smoke.os, "killpg") as cleanup:
            _, rss, cpu = perf_smoke.run_once(Path("/fake/engine"), "http://localhost/", "5000", 5000)
        self.assertEqual(rss, 1024)
        self.assertAlmostEqual(cpu, 0.15)
        process.wait.assert_called_once_with()
        self.assertEqual(commands[0][:4], ["timeout", "--signal=KILL", "45", "prlimit"])
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
    def compare(self, base_latency, candidate_latency, base_cpu, candidate_cpu):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            baseline, candidate, output = root / "base", root / "candidate", root / "report.json"
            baseline.touch()
            candidate.touch()
            clock = [0.0]

            def start(command, **kwargs):
                changed = str(candidate) in command
                clock[0] += (candidate_latency if changed else base_latency) / 1000
                cpu = candidate_cpu if changed else base_cpu
                metrics = f"{cpu:.2f} 0.00 40960\n" if command[command.index("-f") + 1] != "%M" else "40960\n"
                Path(command[command.index("-o") + 1]).write_text(metrics)
                kwargs["stdout"].write(b"5\n")
                kwargs["stdout"].flush()
                process = Mock(pid=12345, returncode=0)
                process.wait.return_value = 0
                return process

            logs = io.StringIO()
            error = None
            with patch.object(sys, "argv", ["perf_smoke.py", "--base", str(baseline), "--candidate", str(candidate), "--output", str(output)]), \
                    patch.object(perf_smoke, "SCENARIOS", {"probe": ("5", 5)}), \
                    patch.object(perf_smoke.subprocess, "Popen", side_effect=start), \
                    patch.object(perf_smoke.time, "perf_counter", side_effect=lambda: clock[0]), \
                    patch.object(perf_smoke.os, "killpg"), \
                    patch.dict(perf_smoke.os.environ, {"GITHUB_STEP_SUMMARY": ""}), \
                    contextlib.redirect_stdout(logs):
                try:
                    perf_smoke.main()
                except SystemExit as failure:
                    error = str(failure)
            return error, json.loads(output.read_text()), logs.getvalue()

    def test_fifteen_percent_steady_latency_regression_is_rejected(self):
        error, report, logs = self.compare(1000, 1150, 1.0, 1.0)
        self.assertIsNotNone(error)
        self.assertIn("::error::probe latency", logs)
        self.assertAlmostEqual(report["scenarios"]["probe"]["latency_ratio"], 1.15)

    def test_cpu_regression_is_rejected_when_wall_time_is_unchanged(self):
        error, _, logs = self.compare(1000, 1000, 1.0, 1.15)
        self.assertIsNotNone(error)
        self.assertIn("::error::probe CPU", logs)

    def test_absolute_floors_keep_short_timing_and_cpu_quantization_noise(self):
        error, _, _ = self.compare(50, 55, 0.02, 0.04)
        self.assertIsNone(error)

    def test_nonfinite_cpu_measurement_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError, "invalid CPU"):
            self.compare(100, 100, float("nan"), 0.12)

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
