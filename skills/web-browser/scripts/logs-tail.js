#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Tail console output and uncaught errors from the active broker tab.
 *
 * Why a page hook instead of CDP events: on obscura 0.2.7 `Log.entryAdded` never
 * fires (measured: 0 events for console calls, CSP violations, 404s and failed
 * fetches), and the broker protocol is strictly one request / one response, so
 * there is no way for a script to subscribe to `Runtime.consoleAPICalled`
 * without changing broker.js. Patching the page's own console is honest, needs
 * no new broker op, and survives navigation when installed as a preload script
 * (Page.addScriptToEvaluateOnNewDocument, verified on obscura 0.2.7: applies to
 * later navigations, reloads and subframes).
 *
 * Error capture is deliberately layered, because the engine is lossy here:
 *   - `unhandledrejection` DOES fire (measured).
 *   - `window.onerror` / `error` listeners are installed too, but on obscura
 *     0.2.7 they did NOT fire for a top-level script throw in measurement; an
 *     async (timer) throw instead surfaces as a `console.error("Timer error: …")`
 *     call, which the console patch catches. `Runtime.exceptionThrown` (not
 *     reachable through the broker) only covers SYNCHRONOUS throws, so it would
 *     miss the async half anyway.
 * The header of each printed entry carries `source` so a reader can tell which
 * path produced it.
 *
 * Each entry is printed as: `[level] message (at url:line)`.
 */

import { brokerRequest, cdpCall, ensureBroker, evaluate } from "./broker-client.js";
import { parseFlags } from "./cli.js";

const DEBUG = process.env.DEBUG === "1";
const log = DEBUG ? (...args) => console.error("[debug]", ...args) : () => {};

// Bumped when the hook's shape changes: an older in-page hook is replaced.
const HOOK_VERSION = 4;
const MAX_ENTRIES = 2000; // ring buffer cap; keeps each poll well under 1 MB
const DEFAULT_POLL_MS = 200;

function printUsage() {
  console.log("Usage: logs-tail.js [--follow] [--limit=<n>] [--since=<index>] [--clear]");
  console.log("                    [--poll=<ms>] [--for=<sec>] [--json] [--tab=<id>]");
  console.log("\nOptions:");
  console.log("  --follow        Keep polling until the limit, --for, or Ctrl-C.");
  console.log("  --limit=<n>     Print at most n entries this run (default 200).");
  console.log("  --since=<i>     Start at entry index i (0-based; printed on each run).");
  console.log("  --clear         Empty the in-page buffer first.");
  console.log("  --poll=<ms>     Poll interval while following (default 200).");
  console.log("  --for=<sec>     Stop following after this many seconds.");
  console.log("  --json          One JSON object per line.");
  console.log("  --install       Only install the preload hook, then exit (do this");
  console.log("                  BEFORE nav.js so parse-time logs are captured).");
  console.log("\nExamples:");
  console.log("  logs-tail.js                 # whatever the page logged so far");
  console.log("  logs-tail.js --follow        # live tail, Ctrl-C to stop");
  console.log("  logs-tail.js --follow --for=5 --clear");
  console.log("\nInstall the hook before navigating, so parse-time logs are caught:");
  console.log("  logs-tail.js --install && nav.js https://example.com && logs-tail.js --follow");
  process.exit(1);
}

/**
 * The in-page hook. Installed as a preload script (survives navigation) and
 * re-run with `evaluate` for the current document when missing.
 */
const HOOK_SOURCE = `(function () {
  if (window.__agentLogs && window.__agentLogs.version === ${HOOK_VERSION}) return;

  var buffer = { version: ${HOOK_VERSION}, entries: [], nextSeq: 1, dropped: 0, max: ${MAX_ENTRIES} };
  window.__agentLogs = buffer;

  var serialize = function (value) {
    try {
      if (typeof value === "string") return value;
      if (value instanceof Error) return String(value.stack || value);
      if (typeof value === "undefined") return "undefined";
      if (typeof value === "function") return "[function " + (value.name || "anonymous") + "]";
      if (typeof value === "object" && value !== null) return JSON.stringify(value);
      return String(value);
    } catch (e) {
      try { return String(value); } catch (e2) { return "[unserializable]"; }
    }
  };

  // The hook itself, the CDP eval channel and engine internals are never the
  // caller, so they are skipped when looking for a page location.
  var isInternal = function (url) {
    return (
      url.indexOf("<preload>") !== -1 ||
      url.indexOf("<eval-remote>") !== -1 ||
      url.indexOf("<obscura:") !== -1 ||
      url.indexOf("ext:") === 0 ||
      url.indexOf("node:") === 0
    );
  };

  // First non-internal location in a stack-like string.
  var whereFromStack = function (stack, skipFirstLine) {
    var sawEval = false;
    try {
      var lines = String(stack || "").split("\\n");
      for (var i = skipFirstLine ? 1 : 0; i < lines.length; i++) {
        var line = lines[i].trim();
        if (!line) continue;
        var withParens = line.match(/\\((.+):(\\d+):(\\d+)\\)$/);
        var bare = line.match(/^at (.+):(\\d+):(\\d+)$/);
        var hit = withParens || bare;
        if (!hit) continue;
        if (isInternal(hit[1])) {
          // Logged from a CDP evaluate rather than page code: no page location
          // exists, and pretending otherwise would point at the wrong file.
          if (hit[1].indexOf("<eval-remote>") !== -1) sawEval = true;
          continue;
        }
        return { url: hit[1], line: Number(hit[2]), viaEval: false };
      }
    } catch (e) { /* ignore */ }
    return { url: null, line: null, viaEval: sawEval };
  };

  var callerFromStack = function () {
    return whereFromStack(new Error().stack, true);
  };

  var push = function (level, source, message, where) {
    // Errors arrive with a stack pasted into the message (the engine logs
    // "Timer error:" followed by frames). The stack in the message names the real
    // throw site; the hook's own stack only ever shows engine frames. Keep the
    // first line as the message and the rest as detail, so one entry prints as
    // one header line plus optional indented frames.
    var caller = (where && where.url && where) || whereFromStack(message, false);
    if (!caller.url) caller = callerFromStack();
    var text = String(message);
    var newline = text.indexOf("\\n");
    buffer.entries.push({
      seq: buffer.nextSeq++,
      ts: Date.now(),
      level: level,
      source: source,
      message: newline === -1 ? text : text.slice(0, newline),
      detail: newline === -1 ? null : text.slice(newline + 1).replace(/^\\s*\\n?/, ""),
      url: caller.url,
      line: caller.line,
      viaEval: caller.viaEval === true,
    });
    if (buffer.entries.length > buffer.max) {
      buffer.entries.shift();
      buffer.dropped += 1;
    }
  };

  var create = function (args) {
    return [].slice.call(args).map(serialize).join(" ");
  };

  var levels = ["log", "info", "warn", "error", "debug"];
  for (var i = 0; i < levels.length; i++) {
    (function (level) {
      var original = console[level];
      console[level] = function () {
        push(level, "console", create(arguments));
        return original.apply(console, arguments);
      };
    })(levels[i]);
  }

  window.addEventListener("unhandledrejection", function (event) {
    var reason = event && event.reason;
    push("error", "unhandledrejection", reason && reason.stack ? String(reason.stack) : serialize(reason));
  });

  window.addEventListener("error", function (event) {
    if (!event) return;
    push("error", "error-event", String(event.message || event.error || "unknown"), {
      url: event.filename || location.href,
      line: typeof event.lineno === "number" ? event.lineno : null,
    });
  }, true);

  window.onerror = function (message, url, line) {
    push("error", "onerror", String(message), {
      url: url || location.href,
      line: typeof line === "number" ? line : null,
    });
    return false;
  };
})();`;

/**
 * Read entries from `since` onwards. Returns the hook buffer or null when the
 * hook is not installed in the current document.
 */
const READ = `(() => {
  const store = window.__agentLogs;
  if (!store || !Array.isArray(store.entries)) return null;
  return {
    version: store.version || 0,
    nextSeq: store.nextSeq,
    dropped: store.dropped,
    total: store.entries.length,
    entries: store.entries.slice(SINCE),
  };
})()`;

function formatEntry(entry) {
  const where = entry.url
    ? `${entry.url}:${entry.line ?? "?"}`
    : entry.viaEval
      ? "<cdp-eval>"
      : "unknown";
  const head = `[${entry.level}] ${entry.message} (at ${where})`;
  if (!entry.detail) return head;
  // Continuation frames, indented so the header stays parsable.
  const frames = entry.detail
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .slice(0, 8)
    .map((line) => `    ${line}`);
  return [head, ...frames].join("\n");
}

// cli.js owns the shared boolean set; these are the bare flags this command takes.
const { get, has, unknown } = parseFlags(process.argv.slice(2), {
  booleans: new Set(["--follow", "--clear", "--json", "--help", "--install"]),
});

if (unknown.length > 0) {
  console.error(`✗ Unknown option: ${unknown.join(", ")}`);
  printUsage();
}
if (has("--help")) printUsage();

const follow = has("--follow");
const clear = has("--clear");
const installOnly = has("--install");
const asJson = has("--json");
const tabId = get("--tab", undefined);
const limit = Number(get("--limit", 200));
const since = Number(get("--since", 0));
const pollMs = Number(get("--poll", DEFAULT_POLL_MS));
const forSec = Number(get("--for", 0));

if (!Number.isFinite(limit) || limit < 0) {
  console.error("✗ --limit must be a non-negative number");
  process.exit(1);
}
if (!Number.isFinite(since) || since < 0) {
  console.error("✗ --since must be a non-negative number");
  process.exit(1);
}
if (!Number.isFinite(pollMs) || pollMs < 20) {
  console.error("✗ --poll must be >= 20ms");
  process.exit(1);
}
if (!Number.isFinite(forSec) || forSec < 0) {
  console.error("✗ --for must be a non-negative number of seconds");
  process.exit(1);
}

const globalTimeout = setTimeout(() => {
  console.error("✗ Global timeout exceeded (see --for for a bounded tail)");
  process.exit(1);
}, (forSec > 0 ? forSec * 1000 : 3600_000) + 120_000);

async function installPreload() {
  // Survives navigation, so logs from scripts that run at parse time are caught.
  log("installing preload hook");
  await cdpCall(
    "Page.addScriptToEvaluateOnNewDocument",
    { source: HOOK_SOURCE, runImmediately: true },
    { tabId, timeout: 10000 },
  );
}

async function ensureCurrentDocumentHook() {
  const version = await evaluate("(window.__agentLogs && window.__agentLogs.version) || 0", {
    tabId,
    timeout: 10000,
  });
  if (version === HOOK_VERSION) return false;
  log("installing hook in the current document");
  await evaluate(HOOK_SOURCE, { tabId, timeout: 10000 });
  return true;
}

let printed = 0;
let cursor = since;
// Separate cursor for the broker's CDP event buffer (uncaught exceptions).
let exceptionCursor = 0;
let stopped = false;

async function drain({ json }) {
  const payload = await evaluate(READ.replace("SINCE", String(cursor)), { tabId, timeout: 20000 });
  if (!payload) {
    const installed = await ensureCurrentDocumentHook();
    if (installed) console.error("! hook was missing in this document; installed now (earlier logs are lost)");
    return;
  }
  const entries = payload.entries || [];
  for (const entry of entries) {
    if (printed >= limit) {
      stopped = true;
      break;
    }
    if (json) console.log(JSON.stringify(entry));
    else console.log(formatEntry(entry));
    printed += 1;
  }
  cursor = payload.nextSeq - 1;
  if (payload.dropped > 0 && entries.length === 0) {
    log(`page dropped ${payload.dropped} old entries (buffer cap ${MAX_ENTRIES})`);
  }
}

/**
 * Print uncaught exceptions that the broker buffered from Runtime.exceptionThrown.
 * Obscura reports synchronous throws there, but never through page hooks, so this
 * is the only path that sees them. Indexed in the same output stream as console
 * entries, tagged level "exception".
 */
async function drainExceptions({ json } = {}) {
  try {
    const { events, lastSeq } = await brokerRequest(
      { op: "events", method: "Runtime.exceptionThrown", since: exceptionCursor, limit: 200 },
      { timeout: 5000 },
    );
    for (const event of events || []) {
      const d = event.params || {};
      const first = (d.description || d.text || "uncaught exception").split("\n")[0];
      // CDP line numbers are 0-based; report 1-based in both shapes.
      const line = d.lineNumber === null || d.lineNumber === undefined ? null : d.lineNumber + 1;
      const where = d.url ? ` (at ${d.url}:${line ?? "?"})` : "";
      if (json) {
        console.log(JSON.stringify({ level: "exception", message: first, url: d.url, line }));
      } else {
        console.log(`[exception] ${first}${where}`);
        if (d.description && d.description.includes("\n")) {
          for (const line of d.description.split("\n").slice(1, 4)) console.log(`    ${line.trim()}`);
        }
      }
    }
    if (typeof lastSeq === "number") exceptionCursor = lastSeq;
  } catch (e) {
    log("exception buffer unavailable:", e.message);
  }
}

try {
  await ensureBroker();
  log("broker ready");

  await installPreload();

  if (clear) {
    await ensureCurrentDocumentHook();
    // Reset the sequence too, so `--since` indexes restart at 0 after a clear.
    await evaluate(
      "((s) => { s.entries.length = 0; s.dropped = 0; s.nextSeq = 1; return true; })(window.__agentLogs)",
      { tabId, timeout: 10000 },
    );
    cursor = 0;
    log("cleared");
  }

  if (installOnly) {
    console.log("✓ console/error hook installed (preload + current document)");
  } else {
    await ensureCurrentDocumentHook();
    await drainExceptions({ json: asJson });
    await drain({ json: asJson });

    if (follow && !stopped) {
      const deadline = forSec > 0 ? Date.now() + forSec * 1000 : Infinity;
      while (!stopped && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, pollMs));
        await drainExceptions({ json: asJson });
        await drain({ json: asJson });
      }
      if (stopped) console.error(`! stopped at --limit=${limit}`);
    }

    if (!asJson) {
      console.error(`entries printed: ${printed}; next --since=${cursor}`);
    }
  }
} catch (e) {
  console.error("✗", e.message);
  process.exitCode = 1;
} finally {
  clearTimeout(globalTimeout);
}
