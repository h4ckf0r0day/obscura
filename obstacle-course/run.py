#!/usr/bin/env python3
"""Obscura obstacle course runner.

Serves obstacle-course/ over a local HTTP origin and drives `obscura fetch`
against each fixture, reporting per-stage correctness + latency (min/median over
timed runs). It is a feature + speed showcase: stages span extraction (--dump),
the JS/DOM/Web-API surface (--eval), scraping/stealth, charset decoding, and
modern frameworks.

Stage types (manifest.json):
  - "eval"  : run `--eval <check>`; the JSON-decoded result must equal `expect`.
  - "dump"  : run `--dump <mode>`; stdout must contain every string in
              `expect_contains` and none in `expect_excludes`.

obscura blocks loopback by default, so fetches pass --allow-private-network.

Usage: OBSCURA_BIN=/path/to/obscura python3 run.py [--json] [--filter X] [--runs N]
"""
import argparse, json, os, socket, statistics, subprocess, sys, threading, time
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


def run_fetch(obscura_bin, url, cmd_args, wait_secs, timeout_secs):
    """One `obscura fetch`; returns (wall_ms, stdout_str_or_None)."""
    t0 = time.time()
    try:
        proc = subprocess.run(
            [obscura_bin, "fetch", url, "--allow-private-network", "--quiet",
             "--timeout", str(timeout_secs), "--wait", str(wait_secs)] + cmd_args,
            capture_output=True, text=True, timeout=timeout_secs + 15,
        )
    except subprocess.TimeoutExpired:
        return (time.time() - t0) * 1000.0, None
    return (time.time() - t0) * 1000.0, proc.stdout


def stage_args(st):
    if st.get("type", "eval") == "dump":
        return ["--dump", st["mode"]]
    return ["--eval", st["check"]]


def check_result(st, stdout):
    if stdout is None:
        return False, "<no output / timeout>"
    if st.get("type", "eval") == "dump":
        out = stdout
        miss = [s for s in st.get("expect_contains", []) if s not in out]
        bad = [s for s in st.get("expect_excludes", []) if s in out]
        if miss:
            return False, f"missing {miss!r}"
        if bad:
            return False, f"should not contain {bad!r}"
        return True, st.get("desc", "")
    # eval: result is JSON.stringify(...)-wrapped
    try:
        got = json.loads(stdout.strip())
    except Exception:
        got = stdout.strip()
    if got == st["expect"]:
        return True, st.get("desc", "")
    return False, f"expected {st['expect']!r}, got {got!r}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--port", type=int, default=0)
    ap.add_argument("--runs", type=int)
    ap.add_argument("--warmup", type=int)
    ap.add_argument("--filter", help="only stages whose name or category contains this")
    args = ap.parse_args()

    obscura_bin = os.environ.get("OBSCURA_BIN", "obscura")
    manifest = json.load(open(os.path.join(HERE, "manifest.json")))
    runs = args.runs if args.runs is not None else manifest.get("runs", 5)
    warmup = args.warmup if args.warmup is not None else manifest.get("warmup", 1)
    wait_secs = manifest.get("wait_secs", 3)
    timeout_secs = manifest.get("timeout_secs", 20)

    class Handler(SimpleHTTPRequestHandler):
        def __init__(self, *a, **k):
            super().__init__(*a, directory=HERE, **k)
        def log_message(self, *a, **k):
            pass

    port = args.port or free_port()
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{port}"

    stages = manifest["stages"]
    if args.filter:
        f = args.filter
        stages = [s for s in stages if f in s["name"] or f in s.get("category", "")]

    results, ok_count = [], 0
    if not args.json:
        print(f"obstacle course: {len(stages)} stages, {runs} runs (warmup {warmup}), "
              f"wait {wait_secs}s\n  bin: {obscura_bin}\n")
        print(f"{'stage':<18}{'cat':<12}{'result':<7}{'min ms':>8}{'med ms':>8}   detail")
        print("-" * 86)

    cur_cat = None
    for st in stages:
        url = f"{base}/{st['file']}"
        cargs = stage_args(st)
        for _ in range(warmup):
            run_fetch(obscura_bin, url, cargs, wait_secs, timeout_secs)
        times, last = [], None
        for _ in range(runs):
            wall, out = run_fetch(obscura_bin, url, cargs, wait_secs, timeout_secs)
            times.append(wall); last = out
        passed, detail = check_result(st, last)
        ok_count += 1 if passed else 0
        rec = {
            "name": st["name"], "category": st.get("category", ""), "type": st.get("type", "eval"),
            "pass": passed, "min_ms": round(min(times), 1), "median_ms": round(statistics.median(times), 1),
            "detail": detail,
        }
        results.append(rec)
        if not args.json:
            print(f"{st['name']:<18}{st.get('category',''):<12}{('PASS' if passed else 'FAIL'):<7}"
                  f"{rec['min_ms']:>8}{rec['median_ms']:>8}   {detail}")

    server.shutdown()
    summary = {"stages": len(stages), "passed": ok_count, "results": results}
    if args.json:
        print(json.dumps(summary, indent=2))
    else:
        print("-" * 86)
        fast = [r["median_ms"] for r in results]
        print(f"correctness: {ok_count}/{len(stages)} stages passed   |   "
              f"median latency: {round(statistics.median(fast),1)}ms across stages")
    sys.exit(0 if ok_count == len(stages) else 1)


if __name__ == "__main__":
    main()
