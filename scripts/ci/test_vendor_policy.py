import contextlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import pr_policy


class VendorPolicyTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        previous = os.getcwd()
        self.addCleanup(os.chdir, previous)
        os.chdir(self.directory.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Policy test")
        self.git("config", "user.email", "policy@example.test")
        Path("README.md").write_text("fixture\n")
        self.base = self.commit()

    def git(self, *args):
        return subprocess.check_output(["git", *args], text=True).strip()

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def vendor(self, value, root="vendor/wreq-util"):
        path = Path(root) / "src/lib.rs"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value)
        head = self.commit()
        return head, self.git("rev-parse", f"{head}:{root}")

    def check(self, head, approved):
        with patch.object(pr_policy, "APPROVED_VENDOR_SNAPSHOTS", approved, create=True), \
             patch.object(sys, "argv", ["pr_policy", "--base", self.base, "--head", head]), \
             contextlib.redirect_stdout(io.StringIO()):
            try:
                pr_policy.main()
            except SystemExit as error:
                return error.code
        return 0

    def test_approved_snapshot_passes(self):
        head, tree = self.vendor("pub fn original() {}\n")
        self.assertEqual(self.check(head, {"vendor/wreq-util": tree}), 0)

    def test_modified_snapshot_is_rejected(self):
        _, approved = self.vendor("pub fn original() {}\n")
        modified, _ = self.vendor("pub fn changed() {}\n")
        self.assertEqual(self.check(modified, {"vendor/wreq-util": approved}), 1)

    def test_other_new_vendor_is_rejected(self):
        head, tree = self.vendor("pub fn original() {}\n", "vendor/other")
        self.assertEqual(self.check(head, {"vendor/wreq-util": tree}), 1)

    def test_requested_head_is_checked_instead_of_checkout(self):
        approved_head, approved_tree = self.vendor("pub fn original() {}\n")
        modified, _ = self.vendor("pub fn changed() {}\n")
        self.git("checkout", "-q", approved_head)
        self.assertEqual(self.check(modified, {"vendor/wreq-util": approved_tree}), 1)


if __name__ == "__main__":
    unittest.main()
