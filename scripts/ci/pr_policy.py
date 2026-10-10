#!/usr/bin/env python3
"""Fast, deterministic PR policy checks with no shell evaluation of PR data."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath


SHA_RE = re.compile(r"^[0-9a-f]{40}$")
USES_RE = re.compile(r"^\s*-?\s*uses\s*:\s*(.+?)\s*$")
FULL_ACTION_REF_RE = re.compile(r"^[^@]+@[0-9a-f]{40}$")
PINNED_DOCKER_REF_RE = re.compile(r"^docker://[^@]+@sha256:[0-9a-f]{64}$")
MAX_CONTROL_FILE_BYTES = 2 * 1024 * 1024
ALWAYS_BLOCKED_WORKFLOW_ADDITIONS = (
    "pull_request_target",
    "workflow_run",
)
WRITE_PERMISSION_RE = re.compile(
    r"\b(?:actions|checks|contents|id-token|packages|pull-requests)\s*:\s*['\"]?write['\"]?(?:\s|[,}]|$)"
)
WRITE_ALL_RE = re.compile(r"\bpermissions\s*:\s*['\"]?write-all['\"]?(?:\s|$)")
SECRET_REFERENCE_RE = re.compile(r"\$\{\{\s*secrets\s*(?:\.|\[)")
MAINTAINER_PRIVILEGED_WORKFLOWS = {
    ".github/workflows/docker.yml",
    ".github/workflows/release.yml",
}
UNTRUSTED_CONTEXT_RE = re.compile(
    r"\$\{\{\s*github\.event\.(?:pull_request\.(?:title|body|head\.ref)|"
    r"issue\.(?:title|body)|comment\.body|head_commit\.message)"
)
BLOCKED_SUFFIXES = {".dll", ".dylib", ".exe", ".o", ".obj", ".pyc", ".so"}


def git(*args: str, text: bool = False) -> bytes | str:
    result = subprocess.run(
        ["git", *args],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=text,
    )
    return result.stdout


def annotation(kind: str, message: str) -> None:
    safe = message.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
    print(f"::{kind}::{safe}")


def changed_files(base: str, head: str) -> list[str]:
    raw = git("diff", "--name-only", "-z", "--diff-filter=ACMR", f"{base}...{head}")
    assert isinstance(raw, bytes)
    return [part.decode("utf-8", "surrogateescape") for part in raw.split(b"\0") if part]


def check_artifacts(files: list[str], errors: list[str], warnings: list[str]) -> None:
    for name in files:
        path = PurePosixPath(name)
        parts = set(path.parts)
        if "target" in parts or "__pycache__" in parts or path.suffix.lower() in BLOCKED_SUFFIXES:
            errors.append(f"generated build artifact is not allowed: {name}")
        elif path.suffix.lower() in {".png", ".jpg", ".jpeg", ".webp"} and not name.startswith(
            ("crates/obscura-render/assets/", ".github/")
        ):
            warnings.append(f"review newly committed image evidence: {name}")


def check_control_symlinks(files: list[str], errors: list[str]) -> None:
    protected = [name for name in files if name.startswith((".github/", "scripts/ci/"))]
    if not protected:
        return
    raw = git("ls-files", "-s", "-z", "--", *protected)
    assert isinstance(raw, bytes)
    for record in raw.split(b"\0"):
        if record.startswith(b"120000 "):
            errors.append("symlinks are not allowed in CI control paths")
            return


def check_new_vendor(base: str, files: list[str], errors: list[str]) -> None:
    roots = sorted(
        {"/".join(PurePosixPath(name).parts[:2]) for name in files if name.startswith("vendor/")}
    )
    for root in roots:
        exists = subprocess.run(
            ["git", "cat-file", "-e", f"{base}:{root}"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if exists.returncode != 0:
            errors.append(f"new vendored dependency requires explicit maintainer handling: {root}")


def workflow_patch(base: str, head: str, files: list[str]) -> list[tuple[str, str]]:
    workflows = [name for name in files if name.startswith(".github/workflows/")]
    additions: list[tuple[str, str]] = []
    for name in workflows:
        patch = git("diff", "--unified=0", f"{base}...{head}", "--", name, text=True)
        assert isinstance(patch, str)
        additions.extend(
            (name, line[1:])
            for line in patch.splitlines()
            if line.startswith("+") and not line.startswith("+++")
        )
    return additions


def check_workflows(
    base: str,
    head: str,
    files: list[str],
    errors: list[str],
    warnings: list[str],
) -> None:
    workflows = [name for name in files if name.startswith(".github/workflows/")]
    for name in workflows:
        path = Path(name)
        if path.is_symlink():
            # check_control_symlinks reports the policy violation. Do not follow
            # the link: it may target an unbounded device or a runner-local file.
            continue
        try:
            if path.stat().st_size > MAX_CONTROL_FILE_BYTES:
                errors.append(f"workflow is unexpectedly large: {name}")
                continue
            content = path.read_text(encoding="utf-8")
        except OSError as error:
            errors.append(f"cannot read workflow {name}: {error}")
            continue
        for line_number, line in enumerate(content.splitlines(), 1):
            match = USES_RE.match(line)
            if not match:
                continue
            action = match.group(1).split("#", 1)[0].strip().strip("'\"")
            if action.startswith("./"):
                continue
            if action.startswith("docker://"):
                if not PINNED_DOCKER_REF_RE.fullmatch(action):
                    errors.append(
                        f"{name}:{line_number} container action is not pinned to a sha256 digest"
                    )
                continue
            if not FULL_ACTION_REF_RE.fullmatch(action):
                errors.append(f"{name}:{line_number} action is not pinned to a full commit SHA")

    for name, line in workflow_patch(base, head, files):
        compact = line.strip().lower()
        if not compact or compact.startswith("#"):
            continue
        if any(token in compact for token in ALWAYS_BLOCKED_WORKFLOW_ADDITIONS):
            errors.append(f"privileged workflow addition requires a separate maintainer-reviewed change: {line.strip()}")
        privileged = (
            WRITE_PERMISSION_RE.search(compact)
            or WRITE_ALL_RE.search(compact)
            or SECRET_REFERENCE_RE.search(compact)
        )
        if privileged:
            if name in MAINTAINER_PRIVILEGED_WORKFLOWS:
                warnings.append(f"CODEOWNER must review privileged publishing change in {name}")
            else:
                errors.append(
                    f"privileged workflow addition is not allowed outside publishing workflows: {line.strip()}"
                )
        if UNTRUSTED_CONTEXT_RE.search(line):
            errors.append("untrusted GitHub metadata must not be interpolated into workflow commands")


def check_scope(base: str, head: str, files: list[str], warnings: list[str]) -> None:
    shortstat = git("diff", "--shortstat", f"{base}...{head}", text=True)
    assert isinstance(shortstat, str)
    numbers = [int(value) for value in re.findall(r"\d+", shortstat)]
    changed_count = numbers[0] if numbers else len(files)
    added = numbers[1] if len(numbers) > 1 else 0
    if changed_count > 50:
        warnings.append(f"large PR changes {changed_count} files; verify that the scope is focused")
    if added > 5000:
        warnings.append(f"large PR adds {added} lines; inspect generated or vendored content")

    runtime = any(name.startswith("crates/") and "/src/" in name for name in files)
    tests = any("/tests/" in name or name.startswith("render-repros/") for name in files)
    if runtime and not tests:
        warnings.append("runtime code changed without a focused integration-test file")
    rendering = any(name.startswith("crates/obscura-render/") for name in files)
    render_validation = any(
        name.startswith("render-repros/") or name.startswith("crates/obscura-render/tests/")
        for name in files
    )
    if rendering and not render_validation:
        warnings.append("rendering code changed without a deterministic rendering fixture")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", required=True)
    parser.add_argument("--head", required=True)
    args = parser.parse_args()
    if not SHA_RE.fullmatch(args.base) or not SHA_RE.fullmatch(args.head):
        raise SystemExit("base and head must be full lowercase commit SHAs")

    files = changed_files(args.base, args.head)
    errors: list[str] = []
    warnings: list[str] = []
    check_artifacts(files, errors, warnings)
    check_control_symlinks(files, errors)
    check_new_vendor(args.base, files, errors)
    check_workflows(args.base, args.head, files, errors, warnings)
    check_scope(args.base, args.head, files, warnings)

    for message in sorted(set(warnings)):
        annotation("warning", message)
    for message in sorted(set(errors)):
        annotation("error", message)

    print(f"Policy inspected {len(files)} changed files: {len(errors)} errors, {len(warnings)} warnings")
    if errors:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
