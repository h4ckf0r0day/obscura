#!/usr/bin/env python3
"""Crash/hang/panic sweep for the obscura engine.

Phase 1 (discover): 1-level BFS from the real-world seed list, rendering each
seed with obscura and pooling its links, to build a diverse corpus of real pages.
Phase 2 (sweep): run `obscura fetch` over the corpus at concurrency, classifying
each outcome from the exit code + stderr:

  CRASH       killed by a signal (SIGSEGV/SIGABRT/...) -> non-recoverable engine bug
  PANIC       a Rust panic printed to stderr (caught by op_dom or not) -> engine bug
  CAP_HIT     descendants() cap fired -> a cycle escaped the mutation guards
  HANG        obscura's own hard-deadline fired (rc 124) -> a residual hang
  HANG_HARD   even the hard-deadline did not fire (python had to kill it) -> worst case
  THIN        rc 0 but empty/trivial render on a reachable page
  OK          rendered real content

The bug classes (CRASH/PANIC/CAP_HIT/HANG/HANG_HARD) are the point of the sweep.
"""
import argparse, json, os, subprocess, sys, threading, time, urllib.parse
from collections import deque, Counter, defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
OB = os.environ.get("OBSCURA_BIN", "obscura")
SEEDS = os.path.join(HERE, "..", "realworld", "sites.txt")
RESULTS = os.path.join(HERE, "..", "results")
TIMEOUT, WAIT = 20, 5
HARD = TIMEOUT + WAIT + 10          # obscura's process hard-deadline
PYGUARD = HARD + 12                 # outer python kill, above obscura's own
EVAL = "JSON.stringify({tl:document.body?document.body.textContent.length:0,lk:document.querySelectorAll('a').length})"


def seeds():
    out = []
    for l in open(SEEDS):
        l = l.strip()
        if l and not l.startswith("#"):
            out.append(l)
    return out


def render_links(url):
    js = ("JSON.stringify({n:document.body?document.body.innerHTML.length:0,"
          "links:[...document.querySelectorAll('a[href]')].map(a=>a.href)})")
    try:
        p = subprocess.run([OB, "fetch", url, "--quiet", "--timeout", "20", "--wait", "4", "--eval", js],
                           capture_output=True, text=True, timeout=40)
        d = json.loads(p.stdout.strip())
        return d.get("links", [])
    except Exception:
        return []


def discover(seed_list, max_urls, per_seed, concurrency):
    pool, lock = [], threading.Lock()
    seen = set(seed_list)
    pool.extend(seed_list)
    work = deque(seed_list)

    def worker():
        while True:
            with lock:
                if not work or len(pool) >= max_urls:
                    return
                s = work.popleft()
            links = render_links(s)
            added = 0
            with lock:
                for l in links:
                    if added >= per_seed or len(pool) >= max_urls:
                        break
                    lu = urllib.parse.urldefrag(l)[0]
                    if lu.startswith("http") and lu not in seen:
                        seen.add(lu); pool.append(lu); added += 1
                print(f"\r[discover] seeds_done pool={len(pool)}/{max_urls}   ", end="", flush=True)

    ts = [threading.Thread(target=worker, daemon=True) for _ in range(concurrency)]
    for t in ts: t.start()
    for t in ts: t.join()
    print()
    return pool[:max_urls]


SIGNAMES = {6: "SIGABRT", 11: "SIGSEGV", 4: "SIGILL", 8: "SIGFPE", 7: "SIGBUS"}


def classify(rc, dt, out, err):
    if rc is None:
        return "HANG_HARD", f"obscura hard-deadline did not fire ({dt:.0f}s)"
    if rc < 0:
        return "CRASH", f"killed by {SIGNAMES.get(-rc, 'signal '+str(-rc))}"
    if "panicked at" in err:
        line = next((l for l in err.splitlines() if "panicked at" in l), "panic")
        return "PANIC", line.strip()[:220]
    if "descendants() cap hit" in err:
        return "CAP_HIT", "cycle escaped append_child/insert_before guards"
    if rc == 124 or "hard timeout exceeded" in err:
        return "HANG", f"hard-deadline at ~{dt:.0f}s"
    tl = 0
    try:
        tl = json.loads(out.strip()).get("tl", 0)
    except Exception:
        pass
    if tl >= 200:
        return "OK", f"{tl} chars"
    return "THIN", f"{tl} chars (rc0, no render)"


def probe(url):
    t0 = time.time()
    env = dict(os.environ, RUST_LOG="warn", RUST_BACKTRACE="0")
    try:
        p = subprocess.run([OB, "fetch", url, "--quiet", "--timeout", str(TIMEOUT),
                            "--wait", str(WAIT), "--eval", EVAL],
                           capture_output=True, text=True, timeout=PYGUARD, env=env)
        dt = time.time() - t0
        cls, note = classify(p.returncode, dt, p.stdout or "", p.stderr or "")
    except subprocess.TimeoutExpired:
        dt = time.time() - t0
        cls, note = classify(None, dt, "", "")
    return {"url": url, "cls": cls, "note": note, "dt": round(dt, 1)}


def sweep(urls, concurrency):
    res, lock, work, done = [], threading.Lock(), deque(urls), [0]

    def worker():
        while True:
            with lock:
                if not work:
                    return
                u = work.popleft()
            r = probe(u)
            with lock:
                res.append(r); done[0] += 1
                if r["cls"] in ("CRASH", "PANIC", "CAP_HIT", "HANG", "HANG_HARD"):
                    print(f"\n  !! {r['cls']:<9} {r['url'][:70]}  {r['note']}", flush=True)
                if done[0] % 25 == 0:
                    print(f"\r[sweep] {done[0]}/{len(urls)}   ", end="", flush=True)

    ts = [threading.Thread(target=worker, daemon=True) for _ in range(concurrency)]
    for t in ts: t.start()
    for t in ts: t.join()
    print()
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--max", type=int, default=1500)
    ap.add_argument("--per-seed", type=int, default=45)
    ap.add_argument("--concurrency", type=int, default=12)
    ap.add_argument("--urls-file", default=os.path.join(RESULTS, "sweep-urls.txt"))
    args = ap.parse_args()

    if os.path.exists(args.urls_file):
        urls = [l.strip() for l in open(args.urls_file) if l.strip()][:args.max]
        print(f"[discover] cached {len(urls)} urls")
    else:
        print(f"[discover] 1-level BFS from {len(seeds())} seeds for up to {args.max} urls...")
        urls = discover(seeds(), args.max, args.per_seed, args.concurrency)
        os.makedirs(os.path.dirname(args.urls_file), exist_ok=True)
        open(args.urls_file, "w").write("\n".join(urls) + "\n")
    print(f"[sweep] probing {len(urls)} urls, concurrency {args.concurrency}, hard-deadline {HARD}s\n")

    res = sweep(urls, args.concurrency)
    counts = Counter(r["cls"] for r in res)
    print("\n" + "=" * 60)
    print(f"SWEEP: {len(res)} urls")
    for c in ("OK", "THIN", "HANG", "HANG_HARD", "CAP_HIT", "PANIC", "CRASH"):
        if counts.get(c):
            print(f"  {c:<10} {counts[c]}")
    bugs = [r for r in res if r["cls"] in ("CRASH", "PANIC", "CAP_HIT", "HANG", "HANG_HARD")]
    print(f"\n--- ENGINE-BUG findings: {len(bugs)} ---")
    # group panics by signature
    sigs = defaultdict(list)
    for r in bugs:
        sigs[(r["cls"], r["note"])].append(r["url"])
    for (cls, note), us in sorted(sigs.items()):
        print(f"\n[{cls}] {note}   ({len(us)} url(s))")
        for u in us[:6]:
            print(f"    {u}")
    os.makedirs(RESULTS, exist_ok=True)
    json.dump(res, open(os.path.join(RESULTS, "sweep.json"), "w"), indent=1)
    print("\n[wrote results/sweep.json]")


if __name__ == "__main__":
    main()
