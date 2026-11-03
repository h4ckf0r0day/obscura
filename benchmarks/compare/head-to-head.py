#!/usr/bin/env python3
"""Head-to-head: obscura vs headless Chrome on the same pages, cold process.

Both engines are launched as a fresh process per page and asked to load the page,
let its JavaScript settle, and serialize the resulting DOM to stdout:

  obscura:  obscura fetch <url> --dump html --wait <W>
  chrome:   google-chrome --headless --dump-dom --virtual-time-budget=<W>ms <url>

We measure wall-clock latency (Python timer) and peak memory (GNU `time -v`
Maximum resident set size) for each run, take the minimum over N runs (least
noisy), and report obscura vs Chrome side by side.

This is the scraping/automation path: fetch + run scripts + read the DOM. obscura
has no rendering/layout/paint pipeline, so this is not a full-browser comparison;
it is the comparison that matters for headless scraping and agent automation.

Usage:
  OBSCURA_BIN=/path/to/obscura python3 head-to-head.py [--runs N] [--wait S]
                                                       [--filter X] [--json]
"""
import argparse, json, os, re, socket, statistics, subprocess, threading, time
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
OC = os.path.join(os.path.dirname(HERE), "obstacle-course")
OBSCURA = os.environ.get("OBSCURA_BIN", "obscura")
CHROME = os.environ.get("CHROME_BIN", "google-chrome")
GNU_TIME = "/usr/bin/time"

_RSS_RE = re.compile(r"Maximum resident set size \(kbytes\): (\d+)")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


def timed(cmd):
    """Run `cmd` under GNU time -v; return (wall_ms, peak_rss_mb, stdout_len)."""
    full = [GNU_TIME, "-v"] + cmd
    t0 = time.time()
    try:
        p = subprocess.run(full, capture_output=True, text=True, timeout=60)
    except subprocess.TimeoutExpired:
        return None, None, -1
    wall = (time.time() - t0) * 1000.0
    m = _RSS_RE.search(p.stderr)
    rss_mb = (int(m.group(1)) / 1024.0) if m else None
    return wall, rss_mb, len(p.stdout)


def obscura_cmd(url, wait):
    # obscura --wait takes whole seconds; a float string is rejected.
    return [OBSCURA, "fetch", url, "--allow-private-network", "--quiet",
            "--timeout", "30", "--wait", str(int(round(wait))), "--dump", "html"]


def chrome_cmd(url, wait):
    return [CHROME, "--headless", "--no-sandbox", "--disable-gpu",
            "--disable-dev-shm-usage", f"--virtual-time-budget={int(wait * 1000)}",
            "--dump-dom", url]


def bench(cmd, warmup, runs):
    """Median wall/RSS over runs that actually produced a fully rendered DOM.

    The local fixture server can occasionally drop a sub-resource under rapid
    repeated requests, leaving a run un-rendered (tiny output). Those runs are
    discarded so we never report a fast-but-empty result; the comparison only
    counts runs where both the page loaded and its JS settled.
    """
    for _ in range(warmup):
        timed(cmd)
        time.sleep(0.1)
    samples = []
    for _ in range(runs):
        w, r, n = timed(cmd)
        if w is not None:
            samples.append((w, r, n))
        time.sleep(0.1)
    if not samples:
        return None, None, 0, 0
    maxlen = max(n for _, _, n in samples)
    if maxlen == 0:
        return None, None, 0, 0  # every run produced no output (error / no render)
    valid = [(w, r) for w, r, n in samples if n >= max(64, 0.8 * maxlen)]
    if not valid:
        valid = [(w, r) for w, r, n in samples]
    walls = [w for w, _ in valid]
    rsss = [r for _, r in valid if r is not None]
    return (statistics.median(walls) if walls else None,
            statistics.median(rsss) if rsss else None, maxlen, len(valid))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--warmup", type=int, default=1)
    ap.add_argument("--wait", type=float, default=3.0)
    ap.add_argument("--filter", help="only fixtures whose name/category contains this")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    manifest = json.load(open(os.path.join(OC, "manifest.json")))
    stages = manifest["stages"]
    if args.filter:
        stages = [s for s in stages if args.filter in s["name"] or args.filter in s.get("category", "")]

    class H(SimpleHTTPRequestHandler):
        def __init__(self, *a, **k): super().__init__(*a, directory=OC, **k)
        def log_message(self, *a, **k): pass
    port = free_port()
    srv = ThreadingHTTPServer(("127.0.0.1", port), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{port}"

    rows = []
    if not args.json:
        print(f"obscura vs headless Chrome  |  cold `fetch --dump html` vs `--headless --dump-dom`")
        print(f"min of {args.runs} runs (warmup {args.warmup}), wait/virtual-time {args.wait}s")
        print(f"  obscura: {OBSCURA}\n  chrome:  {CHROME}\n")
        print(f"{'page':<20}{'o-ms':>7}{'chr-ms':>8}{'spdup':>7}   {'o-MB':>7}{'chr-MB':>8}{'mem x':>7}")
        print("-" * 72)
    for st in stages:
        url = f"{base}/{st['file']}"
        o_ms, o_mb, _, _ = bench(obscura_cmd(url, args.wait), args.warmup, args.runs)
        c_ms, c_mb, _, _ = bench(chrome_cmd(url, args.wait), args.warmup, args.runs)
        speedup = (c_ms / o_ms) if (o_ms and c_ms) else None
        memx = (c_mb / o_mb) if (o_mb and c_mb) else None
        rows.append({"page": st["name"], "obscura_ms": o_ms, "chrome_ms": c_ms,
                     "speedup": speedup, "obscura_mb": o_mb, "chrome_mb": c_mb, "mem_ratio": memx})
        if not args.json:
            print(f"{st['name']:<20}{o_ms or 0:>7.0f}{c_ms or 0:>8.0f}{(speedup or 0):>6.1f}x"
                  f"   {o_mb or 0:>7.0f}{c_mb or 0:>8.0f}{(memx or 0):>6.1f}x")

    srv.shutdown()
    sp = [r["speedup"] for r in rows if r["speedup"]]
    mx = [r["mem_ratio"] for r in rows if r["mem_ratio"]]
    omb = [r["obscura_mb"] for r in rows if r["obscura_mb"]]
    cmb = [r["chrome_mb"] for r in rows if r["chrome_mb"]]
    summary = {
        "pages": len(rows),
        "median_speedup_vs_chrome": round(statistics.median(sp), 2) if sp else None,
        "median_memory_ratio_vs_chrome": round(statistics.median(mx), 2) if mx else None,
        "median_obscura_mb": round(statistics.median(omb), 1) if omb else None,
        "median_chrome_mb": round(statistics.median(cmb), 1) if cmb else None,
        "results": rows,
    }
    if args.json:
        print(json.dumps(summary, indent=2))
    else:
        print("-" * 72)
        print(f"median: obscura is {summary['median_speedup_vs_chrome']}x faster and uses "
              f"{summary['median_memory_ratio_vs_chrome']}x less memory than headless Chrome")
        print(f"median peak RSS: obscura {summary['median_obscura_mb']} MB vs Chrome {summary['median_chrome_mb']} MB")
        print("note: scraping path only (load + JS + DOM serialize); obscura has no render/paint pipeline")


if __name__ == "__main__":
    main()
