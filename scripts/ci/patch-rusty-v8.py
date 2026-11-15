#!/usr/bin/env python3
"""Prepare a rusty_v8 source tree for an Android source build.

Three source-layout differences between a standalone rusty_v8 checkout and a
Chromium checkout have to be repaired in CI:

* The source-build branch must run only for the Android Cargo target. The host
  copy used by Cargo build scripts can use rusty_v8's published x86_64
  archive; otherwise V8 would be compiled twice against the wrong sysroot.
* The standalone V8 build configuration does not declare
  ``android_ndk_version``. Chromium normally supplies that declaration from
  gclient-generated arguments, which are not part of the published crate.
* Chromium's bindgen filter can forward host x86 flags into an Android
  bindgen invocation. The CI environment variable enables removing those
  incompatible flags before libclang parses V8's headers.
"""

from __future__ import annotations

import argparse
from pathlib import Path

BUILD_MARKER = "// obscura-ci: build V8 from source only for Android"
BUILD_NEEDLE = '  if env_bool("V8_FROM_SOURCE") {'
BUILD_REPLACEMENT = """  // obscura-ci: build V8 from source only for Android
  if env_bool("V8_FROM_SOURCE")
    && env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android")
  {
"""

ANDROID_CONFIG_MARKER = "# obscura-ci: declare Android NDK version"
ANDROID_NDK_VERSION = "r26"

BINDGEN_FILTER_ENV = "CHROMIUM_BINDGEN_STRIP_HOST_X64_FLAGS_FOR_ANDROID"
BINDGEN_FILTER_NEEDLE = """  def do_filter(args):
    i = 0
    while i < len(args):
"""
BINDGEN_FILTER_REPLACEMENT = """  def do_filter(args):
    strip = os.environ.get("CHROMIUM_BINDGEN_STRIP_HOST_X64_FLAGS_FOR_ANDROID") == "1"
    i = 0
    while i < len(args):
      if strip and args[i] in ("--target=x86_64-unknown-linux-gnu", "-msse3", "-m64"):
        i += 1
        continue
"""


def patch_build_script(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    if BUILD_MARKER in text:
        print(f"{path}: source-build patch already present")
        return

    count = text.count(BUILD_NEEDLE)
    if count != 1:
        raise SystemExit(
            f"{path}: expected one V8_FROM_SOURCE branch, found {count}"
        )

    path.write_text(text.replace(BUILD_NEEDLE, BUILD_REPLACEMENT), encoding="utf-8")
    print(f"{path}: patched V8_FROM_SOURCE for Android-only builds")


def patch_android_config(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    if ANDROID_CONFIG_MARKER in text:
        print(f"{path}: Android NDK version declaration already present")
        return
    if "android_ndk_version" in text:
        raise SystemExit(
            f"{path}: android_ndk_version exists without the expected CI marker"
        )

    newline = "\r\n" if "\r\n" in text else "\n"
    block = (
        f"{ANDROID_CONFIG_MARKER}{newline}"
        f"declare_args() {{{newline}"
        f'  android_ndk_version = "{ANDROID_NDK_VERSION}"{newline}'
        f"}}{newline}{newline}"
    )

    # Insert after the copyright header and before the first GN statement.
    lines = text.splitlines(keepends=True)
    insert_line = 0
    for index, line in enumerate(lines):
        if line.startswith("#") or not line.strip():
            insert_line = index + 1
        else:
            break

    insert_at = sum(len(line) for line in lines[:insert_line])
    path.write_text(
        text[:insert_at] + block + text[insert_at:], encoding="utf-8", newline=""
    )
    print(
        f"{path}: declared android_ndk_version={ANDROID_NDK_VERSION}"
    )


def patch_bindgen_filter(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    if BINDGEN_FILTER_ENV in text:
        print(f"{path}: bindgen host-x64 filter patch already present")
        return

    newline = "\r\n" if "\r\n" in text else "\n"
    needle = BINDGEN_FILTER_NEEDLE.replace("\n", newline)
    replacement = BINDGEN_FILTER_REPLACEMENT.replace("\n", newline)
    if text.count(needle) != 1:
        raise SystemExit(
            f"{path}: expected one bindgen filter block, found {text.count(needle)}"
        )

    if not any(line.lstrip().startswith("import os") for line in text.splitlines()):
        text = f"import os{newline}{text}"
    text = text.replace(needle, replacement, 1)
    path.write_text(text, encoding="utf-8", newline="")
    print(f"{path}: patched bindgen filter for Android host-x64 flags")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("build_rs", type=Path)
    parser.add_argument("android_config", type=Path, nargs="?")
    parser.add_argument("bindgen_filter", type=Path, nargs="?")
    args = parser.parse_args()

    patch_build_script(args.build_rs)
    if args.android_config is not None:
        patch_android_config(args.android_config)
    if args.bindgen_filter is not None:
        patch_bindgen_filter(args.bindgen_filter)


if __name__ == "__main__":
    main()
