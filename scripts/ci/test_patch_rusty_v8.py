import importlib.util
from pathlib import Path
import tempfile
import unittest


MODULE_PATH = Path(__file__).with_name("patch-rusty-v8.py")
SPEC = importlib.util.spec_from_file_location("patch_rusty_v8", MODULE_PATH)
PATCH = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(PATCH)


class Arm64CacheFlushPatchTests(unittest.TestCase):
    SOURCE = """#if defined(V8_OS_WIN)
#elif defined(V8_OS_DARWIN)
#elif defined(V8_OS_LINUX)
  __builtin___clear_cache(begin, begin + length);
#else
  // inline ARM64 cache maintenance
#endif
"""

    def test_android_uses_inline_path_and_patch_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "cpu-arm64.cc"
            path.write_text(self.SOURCE)
            PATCH.patch_arm64_cache_flush(path)
            patched = path.read_text()
            self.assertIn(PATCH.CACHE_FLUSH_REPLACEMENT, patched)
            self.assertIn("__builtin___clear_cache", patched)
            PATCH.patch_arm64_cache_flush(path)
            self.assertEqual(patched, path.read_text())

    def test_unexpected_source_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "cpu-arm64.cc"
            path.write_text("#elif defined(V8_OS_POSIX)\n")
            with self.assertRaisesRegex(SystemExit, "expected one"):
                PATCH.patch_arm64_cache_flush(path)

    def test_mixed_original_and_patched_branches_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "cpu-arm64.cc"
            path.write_text(
                self.SOURCE + PATCH.CACHE_FLUSH_REPLACEMENT
            )
            with self.assertRaisesRegex(SystemExit, "expected one"):
                PATCH.patch_arm64_cache_flush(path)


if __name__ == "__main__":
    unittest.main()
