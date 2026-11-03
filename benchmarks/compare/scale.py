#!/usr/bin/env python3
"""Throughput and concurrency scaling: obscura vs headless Chrome.

Loads the same page N times through each engine at increasing concurrency
(a pool of C cold processes at a time) and reports sustained throughput
(pages/sec) and peak total resident memory (sum of the live engine processes,
sampled by psutil). This is the "scrape at scale" view: how many pages/sec a
worker box sustains and how much RAM that costs.

Both engines run a fresh process per page, which is obscura's normal model and
the worst case for Chrome (production Chrome usually reuses one browser across
tabs). The point is that obscura's per-page process is cheap enough to fan out
cold, where Chrome's is not.

Usage:
  OBSCURA_BIN=/path/to/obscura python3 scale.py [--pages N] [--levels 1,4,8]
                                                [--wait S] [--page fixtures/x.html] [--json]
"""
import argparse, json, os, socket, statistics, subprocess, threading, time
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import psutil

HERE = os.path.dirname(os.path.abspath(__file__))
OC = os.path.join(os.path.dirname(HERE), "obstacle-course")
OBSCURA = os.environ.get("OBSCURA_BIN", "obscura")
CHROME = os.environ.get("CHROME_BIN", "google-chrome")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


def obscura_cmd(url, wait):
    return [OBSCURA, "fetch", url, "--allow-private-network", "--quiet",
            "--timeout", "30", "--wait", str(int(round(wait))), "--dump", "html"]


def chrome_cmd(url, wait):
    return [CHROME, "--headless", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
            f"--virtual-time-budget={int(wait * 1000)}", "--dump-dom", url]


def run_pool(cmd_for, url, wait, n_pages, concurrency):
    """Run n_pages cold processes, `concurrency` at a time. Returns (pages_per_sec,
    peak_total_rss_mb). Peak RSS is the max over time of the summed RSS of the
    live worker processes (+ their children), sampled in a background thread."""
    procs = {}            # pid -> psutil.Process
    peak = [0.0]
    stop = threading.Event()

    def sampler():
        while not stop.is_set():
            total = 0
            for p in list(procs.values()):
                try:
                    total += p.memory_info().rss
                    for c in p.children(recursive=True):
                        total += c.memory_info().rss
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    pass
            peak[0] = max(peak[0], total)
            time.sleep(0.03)

    st = threading.Thread(target=sampler, daemon=True); st.start()
    t0 = time.time()
    running, launched, done = [], 0, 0
    while done < n_pages:
        while len(running) < concurrency and launched < n_pages:
            p = subprocess.Popen(cmd_for(url, wait), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                procs[p.pid] = psutil.Process(p.pid)
            except psutil.NoSuchProcess:
                pass
            running.append(p); launched += 1
        for p in running[:]:
            if p.poll() is not None:
                running.remove(p); procs.pop(p.pid, None); done += 1
        time.sleep(0.01)
    elapsed = time.time() - t0
    stop.set(); st.join(timeout=1)
    return n_pages / elapsed, peak[0] / (1024 * 1024)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pages", type=int, default=24)
    ap.add_argument("--levels", default="1,4,8")
    ap.add_argument("--wait", type=float, default=3.0)
    ap.add_argument("--page", default=None, help="fixture path (default: a React app)")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()
    levels = [int(x) for x in args.levels.split(",")]

    page = args.page
    if not page:
        man = json.load(open(os.path.join(OC, "manifest.json")))
        page = next((s["file"] for s in man["stages"] if s["name"] == "react"), man["stages"][0]["file"])

    class H(SimpleHTTPRequestHandler):
        def __init__(self, *a, **k): super().__init__(*a, directory=OC, **k)
        def log_message(self, *a, **k): pass
    port = free_port()
    srv = ThreadingHTTPServer(("127.0.0.1", port), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{port}/{page}"

    engines = [("obscura", obscura_cmd), ("chrome", chrome_cmd)]
    rows = []
    if not args.json:
        print(f"throughput + concurrency scaling  |  {args.pages} loads of {page} per level")
        print(f"  obscura: {OBSCURA}\n  chrome:  {CHROME}\n")
        print(f"{'engine':<10}{'workers':>8}{'pages/sec':>11}{'peak RSS MB':>13}")
        print("-" * 44)
    for name, cmd_for in engines:
        for c in levels:
            pps, rss = run_pool(cmd_for, url, args.wait, args.pages, c)
            rows.append({"engine": name, "workers": c, "pages_per_sec": round(pps, 2), "peak_rss_mb": round(rss, 1)})
            if not args.json:
                print(f"{name:<10}{c:>8}{pps:>11.2f}{rss:>13.1f}")
    srv.shutdown()
    if args.json:
        print(json.dumps({"pages": args.pages, "page": page, "results": rows}, indent=2))


if __name__ == "__main__":
    main()
