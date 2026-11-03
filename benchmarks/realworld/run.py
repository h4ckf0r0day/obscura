#!/usr/bin/env python3
"""Real-world render-success benchmark.

Fetches each URL in sites.txt with obscura (and optionally headless Chrome),
runs the page's JavaScript to settle, and reports whether the engine produced a
usable rendered document (clean exit, a non-trivial serialized DOM, and a
readable <title>), plus per-page latency and peak memory. This measures "does
obscura handle the real web", as opposed to the synthetic obstacle-course
fixtures.

SPAs in the list only pass if their client-side JavaScript actually ran, since
their shipped HTML is near-empty. The live web drifts and some sites rate-limit
or block automation, so the success rate and timings are a snapshot.

Usage:
  OBSCURA_BIN=/path/to/obscura python3 run.py [--wait S] [--sites FILE]
                                              [--chrome] [--json]
"""
import argparse, json, os, re, statistics, subprocess, time

HERE = os.path.dirname(os.path.abspath(__file__))
OBSCURA = os.environ.get("OBSCURA_BIN", "obscura")
CHROME = os.environ.get("CHROME_BIN", "google-chrome")
GNU_TIME = "/usr/bin/time"
_RSS = re.compile(r"Maximum resident set size \(kbytes\): (\d+)")
_TITLE = re.compile(r"<title[^>]*>(.*?)</title>", re.S | re.I)
MIN_DOC = 512  # bytes of serialized DOM below which we treat the page as not rendered


def load_sites(path):
    return [l.strip() for l in open(path) if l.strip() and not l.strip().startswith("#")]


def _result(p, ms):
    m = _RSS.search(p.stderr); rss = (int(m.group(1)) / 1024.0) if m else None
    html = p.stdout
    tm = _TITLE.search(html); title = re.sub(r"\s+", " ", tm.group(1)).strip()[:60] if tm else ""
    ok = p.returncode == 0 and len(html) >= MIN_DOC and bool(title)
    why = "" if ok else ("exit %d" % p.returncode if p.returncode else ("empty" if len(html) < MIN_DOC else "no title"))
    return {"ok": ok, "ms": ms, "rss_mb": rss, "title": title, "bytes": len(html), "why": why}


def fetch_obscura(url, wait):
    cmd = [GNU_TIME, "-v", OBSCURA, "fetch", url, "--quiet", "--timeout", "30",
           "--wait", str(int(round(wait))), "--dump", "html"]
    t0 = time.time()
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    except subprocess.TimeoutExpired:
        return {"ok": False, "ms": 60000, "rss_mb": None, "title": "", "bytes": 0, "why": "timeout"}
    return _result(p, (time.time() - t0) * 1000.0)


def fetch_chrome(url, wait):
    cmd = [GNU_TIME, "-v", CHROME, "--headless", "--no-sandbox", "--disable-gpu",
           "--disable-dev-shm-usage", f"--virtual-time-budget={int(wait * 1000)}", "--dump-dom", url]
    t0 = time.time()
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    except subprocess.TimeoutExpired:
        return {"ok": False, "ms": 60000, "rss_mb": None, "title": "", "bytes": 0, "why": "timeout"}
    return _result(p, (time.time() - t0) * 1000.0)


def summarize(rows, key):
    ok = [r[key] for r in rows if r[key]["ok"]]
    lat = [e["ms"] for e in ok]
    rss = [e["rss_mb"] for e in ok if e["rss_mb"]]
    return {
        "rendered_ok": len(ok),
        "success_rate": round(100 * len(ok) / max(len(rows), 1), 1),
        "median_latency_ms": round(statistics.median(lat), 1) if lat else None,
        "median_rss_mb": round(statistics.median(rss), 1) if rss else None,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--wait", type=float, default=4.0)
    ap.add_argument("--sites", default=os.path.join(HERE, "sites.txt"))
    ap.add_argument("--chrome", action="store_true", help="also run headless Chrome side by side")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    sites = load_sites(args.sites)
    rows = []
    if not args.json:
        hdr = f"real-world render-success  |  {len(sites)} pages, wait {args.wait}s"
        print(hdr + ("  (obscura vs headless Chrome)" if args.chrome else ""))
        print(f"  obscura: {OBSCURA}" + (f"\n  chrome:  {CHROME}" if args.chrome else "") + "\n")
        if args.chrome:
            print(f"{'url':<40}{'obscura':>20}{'chrome':>20}")
            print(f"{'':<40}{'ok / ms / MB':>20}{'ok / ms / MB':>20}")
        else:
            print(f"{'result':<8}{'ms':>7}{'RSS MB':>8}{'KB':>7}  {'url':<40}{'title / why'}")
        print("-" * (80 if args.chrome else 100))
    for url in sites:
        o = fetch_obscura(url, args.wait)
        row = {"url": url, "obscura": o}
        if args.chrome:
            row["chrome"] = fetch_chrome(url, args.wait)
        rows.append(row)
        if not args.json:
            if args.chrome:
                c = row["chrome"]
                ot = f"{'OK' if o['ok'] else 'FAIL'} {o['ms']:.0f} {o['rss_mb'] or 0:.0f}"
                ct = f"{'OK' if c['ok'] else 'FAIL'} {c['ms']:.0f} {c['rss_mb'] or 0:.0f}"
                print(f"{url[:40]:<40}{ot:>20}{ct:>20}")
            else:
                tag = "OK" if o["ok"] else "FAIL"
                detail = o["title"] if o["ok"] else o["why"]
                print(f"{tag:<8}{o['ms']:>7.0f}{(o['rss_mb'] or 0):>8.0f}{o['bytes'] / 1024:>7.0f}  {url[:40]:<40}{detail}")

    summary = {"pages": len(rows), "obscura": summarize(rows, "obscura"), "results": rows}
    if args.chrome:
        summary["chrome"] = summarize(rows, "chrome")
    if args.json:
        print(json.dumps(summary, indent=2))
    else:
        print("-" * (80 if args.chrome else 100))
        o = summary["obscura"]
        print(f"obscura: rendered {o['rendered_ok']}/{summary['pages']} ({o['success_rate']}%), "
              f"median {o['median_latency_ms']}ms, {o['median_rss_mb']}MB")
        if args.chrome:
            c = summary["chrome"]
            print(f"chrome:  rendered {c['rendered_ok']}/{summary['pages']} ({c['success_rate']}%), "
                  f"median {c['median_latency_ms']}ms, {c['median_rss_mb']}MB")


if __name__ == "__main__":
    main()
