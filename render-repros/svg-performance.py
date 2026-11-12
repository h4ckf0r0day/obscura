#!/usr/bin/env python3
"""Reproduce the SVG PR's table benchmark using the unchanged CI evaluator."""
import argparse
import importlib.util
from pathlib import Path
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--rounds', type=int, default=15)
    arguments = parser.parse_args()
    if arguments.rounds < 1:
        parser.error('--rounds must be positive')

    evaluator = Path(__file__).resolve().parents[1] / 'scripts/ci/perf_smoke.py'
    spec = importlib.util.spec_from_file_location('ci_performance', evaluator)
    benchmark = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(benchmark)
    benchmark.ROUNDS = arguments.rounds
    benchmark.SCENARIOS = {'table-rows-hot': benchmark.SCENARIOS['table-rows-hot']}
    sys.argv = [
        str(evaluator), '--base', str(arguments.base.resolve()),
        '--candidate', str(arguments.candidate.resolve()),
        '--output', str(arguments.output.resolve()),
    ]
    benchmark.main()


if __name__ == '__main__':
    main()
