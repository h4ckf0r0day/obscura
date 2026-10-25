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
            Path(command[command.index("-o") + 1]).write_text("1024\n")
            self.assertTrue(kwargs["start_new_session"])
            return process

        with patch.object(perf_smoke.subprocess, "Popen", side_effect=start), \
                patch.object(perf_smoke.os, "killpg") as cleanup:
            _, rss = perf_smoke.run_once(Path("/fake/engine"), "http://localhost/", "5000", 5000)
        self.assertEqual(rss, 1024)
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


if __name__ == "__main__":
    unittest.main()
