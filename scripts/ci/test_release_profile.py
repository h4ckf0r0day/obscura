from pathlib import Path
import re
import unittest


class ReleaseProfileTests(unittest.TestCase):
    def test_shipping_uses_the_codegen_measured_by_ci(self):
        manifest = (Path(__file__).resolve().parents[2] / "Cargo.toml").read_text()
        profile = re.search(
            r"^\[profile\.release-dist\]\s*\n(.*?)(?=^\[|\Z)",
            manifest, re.MULTILINE | re.DOTALL,
        )
        self.assertIsNotNone(profile)
        values = {}
        for line in profile.group(1).splitlines():
            line = line.split("#", 1)[0].strip()
            if line:
                key, value = line.split("=", 1)
                values[key.strip()] = value.strip().strip("\"'")
        self.assertEqual(values.get("inherits"), "release")
        self.assertEqual(set(values) - {"inherits", "strip"}, set(),
                         "Shipping must use the runtime code generation benchmarked by CI")
        self.assertNotRegex(manifest, r"(?m)^\s*\[profile\.release-dist\.")


if __name__ == "__main__":
    unittest.main()
