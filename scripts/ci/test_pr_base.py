import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


class PullRequestBaseTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "CI fixture")
        self.git("config", "user.email", "ci@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "core.hooksPath", str(self.root / "no-hooks"))
        self.old_base = self.commit("initial")
        self.git("checkout", "-q", "-b", "feature")
        self.head = self.commit("feature")
        self.git("checkout", "-q", "main")
        self.current_base = self.commit("main-update")
        self.git("merge", "-q", "--no-ff", "--no-edit", "feature")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True, stderr=subprocess.PIPE).strip()

    def commit(self, name):
        (self.root / name).write_text(name)
        self.git("add", name)
        self.git("commit", "-q", "-m", name)
        return self.git("rev-parse", "HEAD")

    def resolve(self, base=None, head=None):
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml").read_text()
        step = workflow.split("- name: Resolve trusted base revision", 1)[1]
        script = textwrap.dedent(step.split("run: |\n", 1)[1].split('          policy_root=', 1)[0])
        environment = {**os.environ, "EVENT_BASE_SHA": base or self.old_base, "EVENT_HEAD_SHA": head or self.head}
        return subprocess.run(["bash", "-c", script + '\nprintf "%s\n" "$base_sha"\n'], cwd=self.root, env=environment, capture_output=True, text=True)

    def test_main_advancing_uses_the_tested_merge_parent(self):
        result = self.resolve()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), self.current_base)

    def test_matching_event_base_still_succeeds(self):
        result = self.resolve(base=self.current_base)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), self.current_base)

    def test_a_mismatched_pull_request_head_is_rejected(self):
        self.assertNotEqual(self.resolve(head=self.current_base).returncode, 0)

    def test_a_head_checkout_is_not_treated_as_a_merge(self):
        self.git("checkout", "-q", "feature")
        self.assertNotEqual(self.resolve().returncode, 0)

    def test_a_base_outside_the_tested_main_history_is_rejected(self):
        self.assertNotEqual(self.resolve(base=self.head).returncode, 0)

    def test_an_invalid_event_base_is_rejected(self):
        self.assertNotEqual(self.resolve(base="invalid").returncode, 0)


if __name__ == "__main__":
    unittest.main()
