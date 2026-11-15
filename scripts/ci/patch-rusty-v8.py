#!/usr/bin/env python3
"""Restrict rusty_v8's source build to the Android Cargo target.

The Android build needs V8 compiled from source, while the host copy used by
Cargo build scripts can use rusty_v8's published x86_64 archive. Keeping
V8_FROM_SOURCE global otherwise compiles V8 twice and makes the host build
depend on Android's sysroot.
"""

from __future__ import annotations

import argparse
from pathlib import Path

MARKER = "// obscura-ci: build V8 from source only for Android"
NEEDLE = '  if env_bool("V8_FROM_SOURCE") {'
REPLACEMENT = """  // obscura-ci: build V8 from source only for Android
  if env_bool("V8_FROM_SOURCE")
    && env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android")
  {
"""


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("build_rs", type=Path)
    args = parser.parse_args()

    text = args.build_rs.read_text(encoding="utf-8")
    if MARKER in text:
        print(f"{args.build_rs}: patch already present")
        return
    count = text.count(NEEDLE)
    if count != 1:
        raise SystemExit(
            f"{args.build_rs}: expected one V8_FROM_SOURCE branch, found {count}"
        )
    args.build_rs.write_text(text.replace(NEEDLE, REPLACEMENT), encoding="utf-8")
    print(f"{args.build_rs}: patched V8_FROM_SOURCE for Android-only builds")


if __name__ == "__main__":
    main()
