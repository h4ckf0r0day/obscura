import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


class RepairV8CacheTests(unittest.TestCase):
    def repair_steps(self):
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml").read_text()
        restores = workflow.split("- name: Restore Rust dependency cache")[1:]
        self.assertEqual(len(restores), 2)
        scripts = []
        for consumer in restores:
            before_build = consumer.split("- name: Build", 1)[0]
            marker = "- name: Repair incomplete V8 cache\n        run: |\n"
            self.assertIn(marker, before_build)
            scripts.append(textwrap.dedent(before_build.split(marker, 1)[1]).strip())
        return scripts

    def test_both_cache_consumers_repair_before_building(self):
        self.assertEqual(len(self.repair_steps()), 2)

    def check_archive(self, kind, should_clean, clean_exit=0):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "target/release/gn_out/obj/librusty_v8.a"
            archive.parent.mkdir(parents=True)
            if kind == "valid":
                archive.write_bytes(b"archive fixture")
            elif kind == "empty":
                archive.touch()
            elif kind == "directory":
                archive.mkdir()
            cargo = root / "cargo"
            cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$CACHE_CALLS"\nexit "$CACHE_EXIT"\n')
            cargo.chmod(0o755)
            calls = root / "calls"
            environment = {**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"], "CACHE_CALLS": str(calls), "CACHE_EXIT": str(clean_exit)}
            for script in self.repair_steps():
                with self.subTest(kind=kind, script=script):
                    calls.unlink(missing_ok=True)
                    checksum = archive.with_suffix(".sum")
                    checksum.write_text("cached URL")
                    run = subprocess.run(["bash", "-euc", script], cwd=root, env=environment, capture_output=True, text=True)
                    blocked = kind == "directory"
                    self.assertEqual(run.returncode, 1 if blocked else clean_exit if should_clean else 0, run.stderr)
                    self.assertEqual(checksum.exists(), not should_clean and not blocked)
                    if blocked:
                        self.assertIn("directory", run.stderr.lower())
                    if should_clean:
                        self.assertEqual(calls.read_text().splitlines(), ["clean", "--release", "-p", "v8"])
                    else:
                        self.assertFalse(calls.exists())

    def test_missing_archive_invalidates_only_v8(self):
        self.check_archive("missing", True)

    def test_empty_archive_invalidates_only_v8(self):
        self.check_archive("empty", True)

    def test_directory_fails_without_cleaning_dependencies(self):
        self.check_archive("directory", False)

    def test_valid_archive_preserves_warm_cache(self):
        self.check_archive("valid", False)

    def test_clean_failure_is_not_hidden(self):
        self.check_archive("missing", True, clean_exit=7)


if __name__ == "__main__":
    unittest.main()
