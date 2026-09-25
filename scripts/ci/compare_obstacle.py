#!/usr/bin/env python3
"""Compare obstacle-course correctness and report latency changes."""

from __future__ import annotations

import argparse
import json
import math
import os
import statistics
from pathlib import Path


MAX_RESULT_BYTES = 10 * 1024 * 1024
EXPECTED_STAGE_COUNT = 33


def load_result(path: Path) -> dict:
    if path.stat().st_size > MAX_RESULT_BYTES:
        raise ValueError(f"{path} is unexpectedly large")
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict) or not isinstance(value.get("results"), list):
        raise ValueError(f"{path} is not an obstacle-course result")
    return value


def result_map(value: dict) -> dict[str, dict]:
    mapped: dict[str, dict] = {}
    for result in value["results"]:
        if not isinstance(result, dict) or not isinstance(result.get("name"), str):
            raise ValueError("obstacle result has an invalid stage")
        name = result["name"]
        if name in mapped:
            raise ValueError(f"duplicate obstacle stage: {name}")
        median_ms = result.get("median_ms")
        if (
            not isinstance(result.get("pass"), bool)
            or isinstance(median_ms, bool)
            or not isinstance(median_ms, (int, float))
            or not math.isfinite(median_ms)
            or median_ms < 0
        ):
            raise ValueError(f"invalid obstacle result for {name}")
        mapped[name] = result
    return mapped


def add_summary(markdown: str) -> None:
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as output:
            output.write(markdown)
    else:
        print(markdown)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    args = parser.parse_args()

    base = result_map(load_result(args.base))
    candidate = result_map(load_result(args.candidate))
    if len(base) != EXPECTED_STAGE_COUNT:
        raise SystemExit(
            f"base obstacle result has {len(base)} stages; expected {EXPECTED_STAGE_COUNT}"
        )
    if len(candidate) != EXPECTED_STAGE_COUNT:
        raise SystemExit(
            f"candidate obstacle result has {len(candidate)} stages; expected {EXPECTED_STAGE_COUNT}"
        )
    if set(base) != set(candidate):
        missing = sorted(set(base) - set(candidate))
        extra = sorted(set(candidate) - set(base))
        raise SystemExit(f"obstacle stage mismatch; missing={missing}, extra={extra}")

    regressions = sorted(name for name in base if base[name]["pass"] and not candidate[name]["pass"])
    improvements = sorted(name for name in base if not base[name]["pass"] and candidate[name]["pass"])
    ratios = []
    slow = []
    for name in sorted(base):
        base_ms = float(base[name]["median_ms"])
        candidate_ms = float(candidate[name]["median_ms"])
        if base_ms > 0:
            ratio = candidate_ms / base_ms
            ratios.append(ratio)
            if ratio > 1.20 and candidate_ms - base_ms > 10:
                slow.append((name, base_ms, candidate_ms, ratio))

    base_passed = sum(1 for result in base.values() if result["pass"])
    candidate_passed = sum(1 for result in candidate.values() if result["pass"])
    median_ratio = statistics.median(ratios) if ratios else 1.0
    lines = [
        "## Obstacle comparison\n",
        "| Metric | Base | Candidate |\n",
        "| --- | ---: | ---: |\n",
        f"| Correct stages | {base_passed}/{len(base)} | {candidate_passed}/{len(candidate)} |\n",
        f"| Median per-stage latency ratio | 1.00x | {median_ratio:.2f}x |\n",
    ]
    if improvements:
        lines.append(f"\nImproved stages: {', '.join(improvements)}\n")
    if regressions:
        lines.append(f"\nRegressed stages: {', '.join(regressions)}\n")
    if slow:
        lines.append("\nStages above the reporting threshold:\n\n")
        for name, base_ms, candidate_ms, ratio in slow:
            lines.append(f"- `{name}`: {base_ms:.1f} ms to {candidate_ms:.1f} ms ({ratio:.2f}x)\n")
            print(f"::warning::obstacle latency {name}: {ratio:.2f}x base")
    add_summary("".join(lines))

    if regressions:
        raise SystemExit("candidate introduces new obstacle-course failures")


if __name__ == "__main__":
    main()
