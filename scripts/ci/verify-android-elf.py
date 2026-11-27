#!/usr/bin/env python3
"""Validate that an executable is an Android ARM64 Bionic ELF.

The Android artifact is not accepted merely because Cargo produced a file.
This check requires an ELF64 AArch64 executable with Android's linker64
interpreter and rejects host/glibc-style shared-library dependencies.  It
prints the parsed ``NEEDED`` entries so CI can copy ``libc++_shared.so`` only
when an executable actually imports it.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

INTERPRETER = "/system/bin/linker64"
FORBIDDEN_NEEDED = {
    "ld-linux-x86-64.so.2",
    "ld-linux-aarch64.so.1",
    "libc.so.6",
    "libdl.so.2",
    "libm.so.6",
    "libpthread.so.0",
    "librt.so.1",
    "libgcc_s.so.1",
    "libstdc++.so.6",
}


@dataclass(frozen=True)
class ElfInfo:
    path: Path
    needed: tuple[str, ...]


def run_readelf(readelf: str, flag: str, path: Path) -> str:
    result = subprocess.run(
        [readelf, flag, "-W", str(path)],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise ValueError(
            f"{readelf} {flag} failed for {path}: "
            f"{result.stderr.strip() or result.stdout.strip()}"
        )
    return result.stdout


def one_match(pattern: str, text: str, description: str, path: Path) -> str:
    matches = re.findall(pattern, text, flags=re.MULTILINE)
    if len(matches) != 1:
        raise ValueError(
            f"{path}: expected one {description}, found {len(matches)}"
        )
    return matches[0]


def inspect(readelf: str, path: Path) -> ElfInfo:
    if not path.is_file():
        raise ValueError(f"{path}: not a regular file")
    if not os.access(path, os.X_OK):
        raise ValueError(f"{path}: not executable")

    header = run_readelf(readelf, "-h", path)
    one_match(r"^\s*Class:\s+ELF64\s*$", header, "ELF64 class", path)
    one_match(r"^\s*Machine:\s+AArch64\s*$", header, "AArch64 machine", path)
    one_match(r"^\s*Type:\s+(?:DYN|EXEC)\b", header, "executable ELF type", path)

    program_headers = run_readelf(readelf, "-l", path)
    interpreter = one_match(
        r"Requesting program interpreter:\s*([^\]\s]+)\]",
        program_headers,
        "program interpreter",
        path,
    )
    if interpreter != INTERPRETER:
        raise ValueError(
            f"{path}: expected interpreter {INTERPRETER}, found {interpreter}"
        )

    dynamic = run_readelf(readelf, "-d", path)
    needed = tuple(re.findall(r"Shared library:\s*\[([^\]]+)\]", dynamic))
    if not needed:
        raise ValueError(f"{path}: ELF has no DT_NEEDED entries")
    for library in needed:
        if not library or library != os.path.basename(library):
            raise ValueError(f"{path}: invalid shared-library dependency {library!r}")
        if library in FORBIDDEN_NEEDED:
            raise ValueError(f"{path}: forbidden host dependency {library!r}")

    return ElfInfo(path=path, needed=needed)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("elf", nargs="+", type=Path)
    args = parser.parse_args()

    readelf = shutil.which("readelf") or shutil.which("llvm-readelf")
    if not readelf:
        raise SystemExit("readelf or llvm-readelf is required")

    infos: list[ElfInfo] = []
    try:
        for path in args.elf:
            infos.append(inspect(readelf, path))
    except ValueError as error:
        print(f"Android ELF verification failed: {error}", file=sys.stderr)
        raise SystemExit(1)

    for info in infos:
        libraries = ",".join(info.needed)
        print(f"{info.path}: ELF64 AArch64, interpreter {INTERPRETER}, NEEDED {libraries}")

    all_needed = {library for info in infos for library in info.needed}
    if "libc++_shared.so" in all_needed:
        print("libc++_shared.so required: yes")
    else:
        print("libc++_shared.so required: no")


if __name__ == "__main__":
    main()
