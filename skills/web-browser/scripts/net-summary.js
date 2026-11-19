#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Summarise the network activity of the active broker tab.
 *
 * Primary mode (`net-summary.js [--slow=<n>] [--json]`) reads EXACT in-page data
 * from the current document. Two sources, in order:
 *
 *   1. Resource Timing — `performance.getEntriesByType("resource"|"navigation")`,
 *      giving per-request url / initiatorType / duration / transferSize /
 *      responseStatus, sorted slowest-first.
 *   2. DOM inventory — used when the engine has no Resource Timing. MEASURED on
 *      obscura 0.2.7: `PerformanceObserver.supportedEntryTypes` is exactly
 *      `["mark","measure"]`, `PerformanceResourceTiming` is `undefined`, and
 *      `getEntriesByType("resource"|"navigation")` always returns `[]` — even on
 *      a page that loaded scripts and images. `performance.timing` (legacy) IS
 *      populated, so navigation timings are still real. The fallback lists every
 *      sub-resource element the DOM knows about with its completion state; the
 *      duration/transferSize/status fields are reported as `null` rather than
 *      invented, and the output says which source was used.
 *
 * No event stream, no daemon beyond the broker: this reads the page, so it can
 * never double-count and never misses a URL.
 *
 * Secondary mode: `--from-log[=<file>]` reads a JSONL file in the OLD watch.js
 * format (~/.cache/agent-web/logs/<date>/<target>.jsonl by default).
 *
 *   DEPRECATED. On obscura 0.2.7 every `Network.*` event is emitted TWICE, so a
 *   raw count doubles; `Network.loadingFailed` carries no `url` and must be
 *   joined to `requestWillBeSent` by `requestId`; `Network.responseReceived`
 *   has no `request` object and always an empty `statusText`. This mode dedupes
 *   by (event type, requestId) and joins failures, so an existing log can still
 *   be summarised — but new data should come from the primary mode.
 */

import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { ensureBroker, evaluate } from "./broker-client.js";
import { parseFlags } from "./cli.js";

const DEBUG = process.env.DEBUG === "1";
const log = DEBUG ? (...args) => console.error("[debug]", ...args) : () => {};

const LOG_ROOT = join(homedir(), ".cache/agent-web/logs");

function printUsage() {
  console.log("Usage: net-summary.js [--slow=<n>] [--json] [--tab=<id>]");
  console.log("       net-summary.js --from-log[=<file>] [--json]");
  console.log("\nOptions:");
  console.log("  --slow=<n>       Show the n slowest resources (default 10).");
  console.log("  --json           Machine-readable output.");
  console.log("  --tab=<id>       Target a specific broker tab.");
  console.log("  --from-log[=f]   DEPRECATED: summarise an old watch.js JSONL log");
  console.log("                   (default: newest under ~/.cache/agent-web/logs).");
  console.log("\nExamples:");
  console.log("  net-summary.js");
  console.log("  net-summary.js --slow=20 --json");
  console.log("  net-summary.js --from-log=~/.cache/agent-web/logs/<date>/page-1.jsonl");
}

/** In-page collector: exact data, one round trip, no event stream. */
const COLLECT = `(() => {
  const round = (n) => (typeof n === "number" && isFinite(n) ? Math.round(n * 1000) / 1000 : null);
  const resEntries = performance.getEntriesByType ? performance.getEntriesByType("resource") : [];
  const supported = (window.PerformanceObserver && PerformanceObserver.supportedEntryTypes) || [];

  const resources = resEntries.map((e) => ({
    url: e.name,
    initiatorType: e.initiatorType || null,
    startTime: round(e.startTime),
    duration: round(e.duration),
    transferSize: typeof e.transferSize === "number" ? e.transferSize : null,
    encodedBodySize: typeof e.encodedBodySize === "number" ? e.encodedBodySize : null,
    decodedBodySize: typeof e.decodedBodySize === "number" ? e.decodedBodySize : null,
    responseStatus: typeof e.responseStatus === "number" ? e.responseStatus : null,
    nextHopProtocol: e.nextHopProtocol || null,
  }));

  // Fallback inventory: what the DOM itself knows, with no invented numbers.
  const dom = [];
  const abs = (u) => { try { return new URL(u, location.href).href; } catch { return u; } };
  const push = (kind, el, url, status, extra) => dom.push({ kind, url: abs(url), status, ...extra });
  document.querySelectorAll("img").forEach((el) => {
    const ok = el.complete && el.naturalWidth > 0;
    const failed = el.complete && el.naturalWidth === 0;
    push("img", el, el.currentSrc || el.src, ok ? "ok" : failed ? "failed" : "pending",
      { width: el.naturalWidth || null, height: el.naturalHeight || null, lazy: el.loading === "lazy" });
  });
  document.querySelectorAll("script[src]").forEach((el) => push("script", el, el.src, "ok", {}));
  document.querySelectorAll("link[rel~=stylesheet][href]").forEach((el) => push("stylesheet", el, el.href, "ok", {}));
  document.querySelectorAll("link[rel~=icon][href]").forEach((el) => push("icon", el, el.href, "ok", {}));
  document.querySelectorAll("iframe[src]").forEach((el) => push("iframe", el, el.src, "ok", {}));
  document.querySelectorAll("video[src],audio[src],source[src]").forEach((el) => push("media", el, el.src, "ok", {}));

  const t = performance.timing || {};
  const ns = typeof t.navigationStart === "number" ? t.navigationStart : 0;
  const rel = (v) => (typeof v === "number" && v > 0 ? round(v - ns) : null);
  const navigation = {
    url: location.href,
    origin: location.origin,
    readyState: document.readyState,
    redirect: rel(t.redirectEnd),
    dns: null,
    connect: null,
    requestStart: rel(t.requestStart),
    responseStart: rel(t.responseStart),
    responseEnd: rel(t.responseEnd),
    domInteractive: rel(t.domInteractive),
    domContentLoaded: rel(t.domContentLoadedEventEnd),
    loadEventEnd: rel(t.loadEventEnd),
    now: round(performance.now()),
    elementCount: document.getElementsByTagName("*").length,
    documentHtmlChars: document.documentElement ? document.documentElement.outerHTML.length : null,
  };

  return {
    source: resources.length > 0 ? "resource-timing" : "dom-inventory",
    resourceTimingAvailable: resources.length > 0,
    // Reported so a caller can tell "no resources" from "no measurement".
    performanceObserverTypes: supported.slice(),
    resourceTimingCtor: typeof window.PerformanceResourceTiming,
    resourceCount: resources.length,
    resources,
    domResourceCount: dom.length,
    domResources: dom,
    navigation,
  };
})()`;

function latestLogFile() {
  if (!existsSync(LOG_ROOT)) return null;
  const dirs = readdirSync(LOG_ROOT)
    .filter((name) => /^\d{4}-\d{2}-\d{2}$/.test(name))
    .map((name) => join(LOG_ROOT, name))
    .filter((path) => {
      try {
        return statSync(path).isDirectory();
      } catch {
        return false;
      }
    })
    .sort();
  if (dirs.length === 0) return null;
  const files = readdirSync(dirs[dirs.length - 1])
    .filter((name) => name.endsWith(".jsonl"))
    .map((name) => join(dirs[dirs.length - 1], name))
    .map((path) => {
      try {
        return { path, mtime: statSync(path).mtimeMs };
      } catch {
        return { path, mtime: 0 };
      }
    })
    .sort((a, b) => b.mtime - a.mtime);
  return files[0]?.path || null;
}

function summariseLog(filePath) {
  const text = readFileSync(filePath, "utf8");
  const seen = new Set();
  const requests = new Map(); // requestId -> url, so failures can be attributed
  const statuses = new Map();
  const failures = [];
  let rawLines = 0;
  let duplicates = 0;
  let requestCount = 0;
  let responseCount = 0;

  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    let entry;
    try {
      entry = JSON.parse(line);
    } catch {
      continue;
    }
    rawLines += 1;
    const key = `${entry.type}:${entry.requestId ?? entry.ts ?? rawLines}`;
    if (entry.requestId) {
      if (seen.has(key)) {
        duplicates += 1;
        continue; // Network.* is emitted twice on obscura; keep the first.
      }
      seen.add(key);
    }
    if (entry.type === "network.request") {
      requestCount += 1;
      if (entry.requestId) requests.set(entry.requestId, entry.url ?? null);
    } else if (entry.type === "network.response") {
      responseCount += 1;
      const status = String(entry.status ?? "unknown");
      statuses.set(status, (statuses.get(status) || 0) + 1);
    } else if (entry.type === "network.failure") {
      // loadingFailed has no url in obscura; join by requestId.
      failures.push({
        requestId: entry.requestId ?? null,
        url: entry.url ?? requests.get(entry.requestId) ?? null,
        errorText: entry.errorText ?? null,
      });
    }
  }

  return {
    file: filePath,
    rawLines,
    duplicatesSkipped: duplicates,
    requests: requestCount,
    responses: responseCount,
    failures,
    statuses: [...statuses.entries()].sort((a, b) => Number(a[0]) - Number(b[0])),
  };
}

// `--from-log` is valid bare (newest log) and with `=file`; the `=` form is
// parsed before the boolean check, so listing it here only affects the bare form.
const { get, has, raw, positional, unknown } = parseFlags(process.argv.slice(2), {
  booleans: new Set(["--json", "--help", "--from-log"]),
});

if (unknown.length > 0) {
  console.error(`✗ Unknown option: ${unknown.join(", ")}`);
  printUsage();
}
if (has("--help")) printUsage();
if (positional.length > 0) {
  console.error(`✗ Unexpected argument: ${positional.join(" ")}`);
  printUsage();
}

const slowCount = Number(get("--slow", 10));
if (!Number.isFinite(slowCount) || slowCount < 0) {
  console.error("✗ --slow must be a non-negative number");
  process.exit(1);
}

const globalTimeout = setTimeout(() => {
  console.error("✗ Global timeout exceeded (60s)");
  process.exit(1);
}, 60000);

try {
  if (has("--from-log")) {
    // `--from-log=file` sets a value; bare `--from-log` falls back to the newest.
    const explicit = raw("--from-log");
    const file = explicit ? String(explicit) : latestLogFile();
    if (!file) throw new Error(`No watch.js JSONL log found under ${LOG_ROOT}`);
    const summary = summariseLog(file);
    if (has("--json")) {
      console.log(JSON.stringify(summary, null, 2));
    } else {
      console.log(`file: ${summary.file}   (deprecated event path)`);
      console.log(
        `requests: ${summary.requests}  responses: ${summary.responses}  failures: ${summary.failures.length}`,
      );
      console.log(
        `dedupe: ${summary.duplicatesSkipped} duplicate event(s) skipped of ${summary.rawLines} line(s) ` +
          `(obscura emits every Network.* event twice)`,
      );
      for (const [status, count] of summary.statuses) console.log(`status ${status}: ${count}`);
      for (const failure of summary.failures.slice(0, 10)) {
        console.log(`- ${failure.errorText || "unknown"} ${failure.url || `(${failure.requestId})`}`);
      }
      if (summary.failures.length > 10) {
        console.log(`- ... ${summary.failures.length - 10} more`);
      }
    }
  } else {
    await ensureBroker();
    log("broker ready");
    const tabId = get("--tab", undefined);
    const data = await evaluate(COLLECT, { tabId, timeout: 20000 });

    const resources = (data.resources || []).slice().sort((a, b) => (b.duration ?? 0) - (a.duration ?? 0));

    if (has("--json")) {
      // `slowest` is the ordered list; `resources` keeps page order so a caller
      // still sees everything.
      console.log(JSON.stringify({ ...data, slowest: resources.slice(0, slowCount) }, null, 2));
    } else {
      const nav = data.navigation;
      console.log(`url: ${nav.url}`);
      console.log(
        `source: ${data.source}` +
          (data.resourceTimingAvailable
            ? ""
            : `  (engine has no Resource Timing: supported entry types = [${data.performanceObserverTypes.join(", ")}], ` +
              `PerformanceResourceTiming = ${data.resourceTimingCtor})`),
      );
      console.log(
        `document: ${nav.elementCount} elements, ${nav.documentHtmlChars} chars of HTML, readyState=${nav.readyState}`,
      );
      console.log(
        `timing(ms): responseEnd=${nav.responseEnd} domInteractive=${nav.domInteractive} ` +
          `domContentLoaded=${nav.domContentLoaded} load=${nav.loadEventEnd} now=${nav.now}`,
      );

      if (data.resourceTimingAvailable) {
        console.log(
          `resources: ${data.resources.length} (slowest ${Math.min(slowCount, data.resources.length)})`,
        );
        for (const item of resources.slice(0, slowCount)) {
          const size = item.transferSize === null ? "?" : `${item.transferSize}B`;
          const status = item.responseStatus === null || item.responseStatus === undefined ? "" : ` ${item.responseStatus}`;
          console.log(
            `  ${String(Math.round(item.duration)).padStart(6)}ms  ${String(size).padStart(9)}  ` +
              `${(item.initiatorType || "?").padEnd(9)}${status.padEnd(4)} ${item.url}`,
          );
        }
      } else {
        const dom = data.domResources || [];
        console.log(
          `resources: ${dom.length} in the DOM — no per-resource duration/size/status is exposed by this engine`,
        );
        for (const item of dom.slice(0, slowCount)) {
          const extra = item.kind === "img" && item.width ? ` ${item.width}x${item.height}` : "";
          console.log(`  ${item.status.padEnd(8)} ${item.kind.padEnd(10)} ${item.url}${extra}`);
        }
        if (dom.length > slowCount) console.log(`  ... ${dom.length - slowCount} more (raise --slow)`);
      }
    }
  }
} catch (e) {
  console.error("✗", e.message);
  process.exitCode = 1;
} finally {
  clearTimeout(globalTimeout);
}
