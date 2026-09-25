#!/usr/bin/env python3

import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path


SUITE_DIR = Path(__file__).resolve().parent
RUNNER = SUITE_DIR / "run.sh"
SITES = SUITE_DIR / "sites.txt"


class RepresentativeSuiteTests(unittest.TestCase):
    def run_wrapper(self, *arguments, extra_environment=None):
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary = Path(temporary_directory)
            obscura = temporary / "obscura"
            obscura.write_text("#!/usr/bin/env sh\nexit 0\n")
            obscura.chmod(obscura.stat().st_mode | stat.S_IXUSR)

            invocation = temporary / "invocation.txt"
            python = temporary / "python"
            python.write_text(
                "#!/usr/bin/env sh\n"
                ": \"${INVOCATION:?}\"\n"
                "printf '%s\\n' \"$@\" > \"$INVOCATION\"\n"
            )
            python.chmod(python.stat().st_mode | stat.S_IXUSR)

            environment = os.environ.copy()
            environment.update(
                {
                    "INVOCATION": str(invocation),
                    "OBSCURA_BIN": str(obscura),
                    "PYTHON_BIN": str(python),
                }
            )
            if extra_environment:
                environment.update(extra_environment)

            output = temporary / "output"
            result = subprocess.run(
                ["bash", str(RUNNER), str(output), *arguments],
                capture_output=True,
                text=True,
                env=environment,
                check=False,
            )
            invoked_arguments = (
                invocation.read_text().splitlines()
                if invocation.exists()
                else []
            )
            return result, invoked_arguments, str(output), str(obscura)

    def test_default_invocation_pins_capture_conditions_and_generic_probes(self):
        result, arguments, output, obscura = self.run_wrapper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            arguments[:2],
            [
                str(SUITE_DIR.parent / "paired-corpus.py"),
                str(SITES),
            ],
        )
        self.assertEqual(
            arguments[2:12],
            [
                "--obscura-bin",
                obscura,
                "--out",
                output,
                "--width",
                "1440",
                "--height",
                "1000",
                "--settle-ms",
                "3000",
            ],
        )
        self.assertIn("--animation-time-ms", arguments)
        self.assertEqual(arguments[arguments.index("--animation-time-ms") + 1], "0")
        purpose_index = arguments.index("--capture-purpose")
        self.assertEqual(arguments[purpose_index + 1], "representative-fidelity")
        self.assertEqual(arguments.count("--geometry-selector"), 7)
        self.assertNotIn("--scroll-y", arguments)

        selectors = [
            arguments[index + 1]
            for index, argument in enumerate(arguments)
            if argument == "--geometry-selector"
        ]
        self.assertEqual(
            selectors,
            [
                "header, nav, footer",
                "main, article",
                "section",
                "form, fieldset, input, button, select, textarea",
                "img, svg, video, canvas",
                "pre",
                "table",
            ],
        )

    def test_scroll_and_optional_comparison_binaries_are_forwarded(self):
        result, arguments, _, _ = self.run_wrapper(
            "bottom",
            extra_environment={
                "CHROMIUM_BIN": "/opt/chromium",
                "BASELINE_BIN": "/opt/obscura-baseline",
            },
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(arguments[-6:], [
            "--chromium-bin",
            "/opt/chromium",
            "--baseline-bin",
            "/opt/obscura-baseline",
            "--scroll-y",
            "bottom",
        ])

    def test_live_capture_does_not_freeze_chromium_animations(self):
        result, arguments, _, _ = self.run_wrapper(
            extra_environment={"CAPTURE_MODE": "live"},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--animation-time-ms", arguments)

    def test_zero_settle_latency_mode_is_forwarded(self):
        result, arguments, _, _ = self.run_wrapper(
            extra_environment={"SUITE_MODE": "latency", "SETTLE_MS": "0"},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        settle_index = arguments.index("--settle-ms")
        self.assertEqual(arguments[settle_index + 1], "0")
        purpose_index = arguments.index("--capture-purpose")
        self.assertEqual(arguments[purpose_index + 1], "cold-load-latency")

    def test_zero_settle_without_explicit_latency_mode_is_rejected(self):
        result, arguments, _, _ = self.run_wrapper(
            extra_environment={"SETTLE_MS": "0"},
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(arguments, [])
        self.assertIn("requires explicit SUITE_MODE=latency", result.stderr)

    def test_latency_mode_requires_zero_settle(self):
        result, arguments, _, _ = self.run_wrapper(
            extra_environment={"SUITE_MODE": "latency"},
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(arguments, [])
        self.assertIn("requires SETTLE_MS=0", result.stderr)

    def test_fractional_settle_is_rejected_before_capture(self):
        result, arguments, _, _ = self.run_wrapper(
            extra_environment={"SETTLE_MS": "1500"},
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(arguments, [])
        self.assertIn("SETTLE_MS must be", result.stderr)

    def test_invalid_capture_mode_is_rejected_before_capture(self):
        result, arguments, _, _ = self.run_wrapper(
            extra_environment={"CAPTURE_MODE": "almost-live"},
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(arguments, [])
        self.assertIn("CAPTURE_MODE must be", result.stderr)

    def test_site_corpus_is_https_and_has_no_duplicates(self):
        urls = [
            line.strip()
            for line in SITES.read_text().splitlines()
            if line.strip() and not line.lstrip().startswith("#")
        ]
        self.assertEqual(len(urls), 15)
        self.assertEqual(len(urls), len(set(urls)))
        self.assertTrue(all(url.startswith("https://") for url in urls))


if __name__ == "__main__":
    unittest.main()
