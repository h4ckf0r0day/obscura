#!/usr/bin/env python3
"""Add Bionic's thread-local errno accessor to deno_core's uv compatibility layer.

deno_core 0.412.0 implements ``errno_location`` for macOS and glibc Linux,
but its fallback is a deliberate compile error for every other Unix target.
Android is a Unix target whose Bionic ABI exposes the TLS errno slot as
``__errno``.  Without this narrow patch, an ``aarch64-linux-android`` build
fails in deno_core before Obscura itself is compiled.

The patch is intentionally limited to ``target_os = "android"`` and reuses
libc's audited declaration of ``__errno``; no host or glibc symbol is used.
"""

from __future__ import annotations

import argparse
from pathlib import Path

MARKER = "// obscura-ci: use Bionic's __errno TLS accessor"
NEEDLE = '''  #[cfg(target_os = "linux")]
  fn errno_location() -> *mut c_int {
'''
ANDROID_IMPLEMENTATION = '''  // obscura-ci: use Bionic's __errno TLS accessor
  #[cfg(target_os = "android")]
  fn errno_location() -> *mut c_int {
    unsafe { libc::__errno() }
  }

'''
FALLBACK_NEEDLE = '''  #[cfg(not(any(target_os = "macos", target_os = "linux")))]
'''
FALLBACK_REPLACEMENT = '''  #[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
  )))]
'''


def patch_tty(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    if MARKER in text:
        print(f"{path}: Android errno patch already present")
        return

    count = text.count(NEEDLE)
    if count != 1:
        raise SystemExit(
            f"{path}: expected one Linux errno_location branch, found {count}"
        )
    if "libc::__errno()" in text:
        raise SystemExit(f"{path}: unexpected pre-existing libc::__errno use")

    fallback_count = text.count(FALLBACK_NEEDLE)
    if fallback_count != 1:
        raise SystemExit(
            f"{path}: expected one unsupported-platform errno branch, "
            f"found {fallback_count}"
        )

    patched = text.replace(NEEDLE, ANDROID_IMPLEMENTATION + NEEDLE, 1)
    patched = patched.replace(FALLBACK_NEEDLE, FALLBACK_REPLACEMENT, 1)
    path.write_text(patched, encoding="utf-8")
    print(f"{path}: added Android __errno implementation")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("tty_rs", type=Path)
    args = parser.parse_args()
    patch_tty(args.tty_rs)


if __name__ == "__main__":
    main()
