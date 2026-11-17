import json
from pathlib import Path
import unittest


class ScoopManifest(unittest.TestCase):
    def setUp(self):
        path = Path(__file__).resolve().parents[2] / "scoop" / "obscura.json"
        self.manifest = json.loads(path.read_text(encoding="utf-8"))

    def release_url(self, version):
        return ("https://github.com/h4ckf0r0day/obscura/releases/download/"
                f"v{version}/obscura-x86_64-windows.zip")

    def test_install_uses_a_literal_versioned_url_and_sha256(self):
        download = self.manifest["architecture"]["64bit"]
        self.assertEqual(download["url"], self.release_url(self.manifest["version"]))
        self.assertRegex(download["hash"], r"\A[0-9a-f]{64}\Z")

    def test_no_download_fallback_for_unsupported_architectures(self):
        self.assertEqual(set(self.manifest["architecture"]), {"64bit"})
        self.assertNotIn("url", self.manifest)
        self.assertNotIn("hash", self.manifest)

    def test_autoupdate_expands_scoop_version_for_the_same_asset(self):
        update = self.manifest["autoupdate"]
        downloads = update.get("architecture", {})
        self.assertEqual(set(downloads), {"64bit"})
        self.assertNotIn("url", update)
        for version in (self.manifest["version"], "10.20.30"):
            with self.subTest(version=version):
                url = downloads["64bit"]["url"].replace("$version", version)
                self.assertEqual(url, self.release_url(version))

    def test_cli_and_worker_both_have_path_shims(self):
        self.assertEqual(self.manifest["bin"], ["obscura.exe", "obscura-worker.exe"])


if __name__ == "__main__":
    unittest.main()
