#!/usr/bin/env node
// Live view for obscura serve.
//
// Streams what a headless obscura browser sees to any local browser tab,
// so you can watch an agent (MCP, Puppeteer, Playwright) work in real time.
//
// Usage:
//   obscura serve --port 9222
//   node tools/live-view.mjs [cdpPort] [httpPort]
//
// No dependencies. Requires Node 21+ (native WebSocket).
//
// Why polling instead of Page.startScreencast:
//   Screencast frames stream to the CDP session that drives the page.
//   Once an agent session takes over, a passive viewer session stops
//   receiving frames even though the page keeps repainting. An explicit
//   Page.captureScreenshot always reflects current state regardless of
//   which session navigated.
//
// Pacing is adaptive: captures run fast while the page is changing and
// back off when it is idle, so watching feels live without burning CPU.

import http from "node:http";
import { spawn } from "node:child_process";

const cdpPort = process.argv[2] ?? 9222;
const httpPort = Number(process.argv[3] ?? 8080);
const FAST_MS = 250; // capture cadence right after a visible change
const IDLE_MS = 1500; // capture cadence while nothing changes
const MIN_FRAME_BYTES = 100; // ignore blank placeholder frames

async function ensureServe() {
  try {
    const r = await fetch(`http://127.0.0.1:${cdpPort}/json/version`);
    if (r.ok) return;
  } catch {}
  // try spawning obscura directly (works in normal installs and in proot ubuntu)
  try {
    const child = spawn("obscura", ["serve", "--port", String(cdpPort)], {
      stdio: "ignore",
      detached: true,
    });
    child.unref();
  } catch {}
  // fallback for Termux proot setups where obscura lives inside ubuntu
  try {
    const child = spawn("proot-distro", ["login", "ubuntu", "--", "/usr/local/bin/obscura", "serve", "--port", String(cdpPort)], {
      stdio: "ignore",
      detached: true,
    });
    child.unref();
  } catch {}
  for (let i = 0; i < 30; i++) {
    await new Promise((r) => setTimeout(r, 1000));
    try {
      const r = await fetch(`http://127.0.0.1:${cdpPort}/json/version`);
      if (r.ok) return;
    } catch {}
  }
}

const clients = new Set();
let latest = null;
let sessionId = null;
let targetId = null;
let ws = null;
let call = null;
let generation = 0;

// prefer a page with real content over a blank tab left behind by
// an earlier session
function pickTarget(targets) {
  const pages = targets.filter((t) => t.type === "page");
  if (!pages.length) return null;
  return (
    pages.find((t) => t.url && t.url !== "about:blank" && !t.url.startsWith("data:")) ??
    pages[pages.length - 1]
  );
}

// Target.getTargets only reports targets known to the calling socket,
// which is useless for watching someone else's session. The HTTP
// target list shows every page across all connections.
async function listAllPages() {
  const res = await fetch(`http://127.0.0.1:${cdpPort}/json/list`);
  return res.json();
}

const page = `
<!doctype html>
<html>
<head><meta charset="utf-8"><title>Obscura live view</title>
<style>body{margin:0;background:#111;display:grid;place-items:center;height:100vh}
img{max-width:100%;max-height:100%}
#st{position:fixed;top:6px;left:10px;color:#8f8;font:13px monospace}</style></head>
<body><div id="st">connecting</div>
<img id="s" alt="live page">
<script>
const img = document.getElementById("s");
const st = document.getElementById("st");
let old = null;
const es = new EventSource("/events");
es.onopen = () => (st.textContent = "live");
es.onerror = () => (st.textContent = "reconnecting");
es.onmessage = (e) => {
  const bytes = Uint8Array.from(atob(e.data), (c) => c.charCodeAt(0));
  const url = URL.createObjectURL(new Blob([bytes], { type: "image/jpeg" }));
  img.src = url;
  if (old) URL.revokeObjectURL(old);
  old = url;
};
</script></body>
</html>`;

const server = http.createServer((req, res) => {
  if (req.url === "/events") {
    res.writeHead(200, {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-store",
      Connection: "keep-alive",
    });
    res.socket.setNoDelay(true);
    if (latest) res.write(`data:${latest}\n\n`);
    clients.add(res);
    req.on("close", () => clients.delete(res));
  } else if (req.url === "/favicon.ico") {
    res.writeHead(204);
    res.end();
  } else {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(page);
  }
});

server.on("error", (err) => {
  console.error(`cannot listen on port ${httpPort}: ${err.message}`);
  process.exit(1);
});

server.listen(httpPort, "127.0.0.1", () => {
  console.log(`live view: http://localhost:${httpPort}`);
});

function broadcast(base64) {
  latest = base64;
  for (const res of clients) {
    try {
      if (res.writable) res.write(`data:${base64}\n\n`);
    } catch {
      clients.delete(res);
    }
  }
}

// make sure sessionId points at targetId, attaching if needed. a
// freshly created target is auto-attached under "{targetId}-session",
// anything else needs an explicit flatten attach.
async function ensureAttached(call) {
  if (sessionId && targetId) return;
  if (!targetId) {
    const wanted = pickTarget(await listAllPages());
    if (!wanted) throw new Error("no page target yet");
    targetId = wanted.id;
  }
  try {
    const attached = await call("Target.attachToTarget", {
      targetId,
      flatten: true,
    });
    sessionId = attached.sessionId;
  } catch {
    sessionId = `${targetId}-session`;
  }
  await call("Page.enable", {}, sessionId);
}

async function connect() {
  await ensureServe();
  const gen = ++generation;
  sessionId = null;
  ws = new WebSocket(`ws://127.0.0.1:${cdpPort}/devtools/browser`);

  let id = 0;
  const pending = new Map();
  call = (method, params = {}, sess) =>
    new Promise((resolve, reject) => {
      const mid = ++id;
      const timer = setTimeout(() => {
        pending.delete(mid);
        reject(new Error(`${method} timed out`));
      }, 15000);
      pending.set(mid, {
        resolve: (v) => {
          clearTimeout(timer);
          resolve(v);
        },
        reject: (e) => {
          clearTimeout(timer);
          reject(e);
        },
      });
      const msg = { id: mid, method, params };
      if (sess) msg.sessionId = sess;
      try {
        ws.send(JSON.stringify(msg));
      } catch (e) {
        clearTimeout(timer);
        pending.delete(mid);
        reject(e);
      }
    });

  // race open against close or error so a refused connection retries
  // instead of hanging forever
  await new Promise((resolve, reject) => {
    ws.addEventListener("open", resolve, { once: true });
    ws.addEventListener("close", () => reject(new Error("connection closed before open")), { once: true });
    ws.addEventListener("error", () => reject(new Error("connect failed")), { once: true });
  });

  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const p = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? p.reject(new Error(msg.error.message)) : p.resolve(msg.result);
    }
  });
  ws.addEventListener("close", () => {
    if (gen !== generation) return;
    sessionId = null;
    setTimeout(() => connect().catch(retry), 2000);
  });

  // attach to whatever page exists now; the capture loop below keeps
  // re-checking and follows the agent if it opens another page later
  await ensureAttached(call);

  // capture on an interval instead of Page.startScreencast: screencast
  // frames stream to the session that drives the page, so a passive
  // viewer stops receiving them once an agent takes over. An explicit
  // capture always reflects the current page state.

  // capture on an interval instead of Page.startScreencast: screencast
  // frames stream to the session that drives the page, so a passive
  // viewer stops receiving them once an agent takes over. An explicit
  // capture always reflects the current page state.
  let lastSent = null;
  let delay = FAST_MS;
  let misses = 0;
  const capture = async () => {
    if (gen !== generation || !call || ws.readyState !== WebSocket.OPEN) return;
    try {
      // the agent may open or switch pages at any time, so re-check
      // the global target list every tick and follow it
      const wanted = pickTarget(await listAllPages());
      if (wanted && wanted.id !== targetId) {
        sessionId = null;
        targetId = wanted.id;
      }
      await ensureAttached(call);
      const shot = await call(
        "Page.captureScreenshot",
        { format: "jpeg", quality: 70 },
        sessionId
      );
      misses = 0;
      if (shot.data && shot.data.length > MIN_FRAME_BYTES && shot.data !== lastSent) {
        lastSent = shot.data;
        delay = FAST_MS; // page is changing, keep the pace up
        broadcast(shot.data);
      } else {
        delay = IDLE_MS; // identical frame, back off
      }
    } catch {
      // transient failures during navigation are normal; a missing
      // target means the agent closed the page, rescan next tick
      misses++;
      if (misses >= 3) {
        sessionId = null;
        targetId = null;
        misses = 0;
      }
      delay = IDLE_MS;
    }
    setTimeout(() => {
      capture().catch(retry);
    }, delay);
  };
  capture().catch(retry);
}

function retry(err) {
  console.error(err.message);
  sessionId = null;
  setTimeout(() => connect().catch(retry), 2000);
}

process.on("SIGINT", () => {
  try { ws?.close(); } catch {}
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(0), 500);
});

connect().catch(retry);
