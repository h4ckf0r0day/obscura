#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Persistent CDP broker.
 *
 * Obscura scopes page targets to the WebSocket connection that created them: a
 * fresh connection sees zero targets and its tabs die when it disconnects
 * (measured on obscura 0.2.7). Every CLI command starting its own connection
 * would therefore lose all page state between invocations.
 *
 * The broker holds one long-lived CDP connection and exposes it over a Unix
 * socket, so `nav.js`, `eval.js`, `click.js`, ... act on the same live tab.
 * One JSON request per line, one JSON response per line.
 *
 * Protocol: {"id"?: string, "method": "<CDP method>", "params"?: {}, "tabId"?: string}
 *        or {"op": "health" | "tabs" | "newTab" | "closeTab" | "shutdown", ...}
 * Response: {"ok": true, "result": ...} | {"ok": false, "error": "..."}
 */

import { existsSync, readFileSync, unlinkSync, writeFileSync, rmSync } from "node:fs";
import net from "node:net";
import { randomBytes } from "node:crypto";
import { connect } from "./cdp.js";
import {
  BROKER_PID_FILE,
  BROWSER_ROOT,
  LOCK_FILE,
  SOCKET_FILE,
  ensureDir,
  isProcessAlive,
  readState,
  writeState,
} from "./obscura.js";

function log(...args) {
  if (process.env.DEBUG === "1") console.error("[broker]", ...args);
}

/** Keep only the useful fields of a Runtime.exceptionThrown payload. */
function summarizeException(params) {
  const d = params?.exceptionDetails || {};
  return {
    text: d.text || null,
    url: d.url || null,
    lineNumber: d.lineNumber ?? null,
    columnNumber: d.columnNumber ?? null,
    description: d.exception?.description || null,
    scriptId: d.scriptId || null,
  };
}

class Broker {
  constructor(cdp) {
    this.cdp = cdp;
    this.tabs = new Map(); // tabId -> { targetId, sessionId, enabled }
    this.defaultTabId = null;
    this.rr = 0;
    // Ring buffer of CDP events for callers that cannot subscribe directly
    // (the broker protocol is one request / one response). Runtime.exceptionThrown
    // works on Obscura but only reaches a live listener, so buffer it here.
    this.events = [];
    this.eventSeq = 0;
    this.EVENT_LIMIT = 2000;
  }

  recordEvent(method, params) {
    this.events.push({
      seq: ++this.eventSeq,
      method,
      params: method === "Runtime.exceptionThrown" ? summarizeException(params) : params,
      at: Date.now(),
    });
    if (this.events.length > this.EVENT_LIMIT) {
      this.events.splice(0, this.events.length - this.EVENT_LIMIT);
    }
  }

  async newTab({ url = "about:blank", activate = true } = {}) {
    const { targetId } = await this.cdp.send("Target.createTarget", { url });
    const sessionId = await this.cdp.attachToPage(targetId);
    // Enable Runtime so exceptionThrown is emitted for this tab.
    try {
      await this.cdp.send("Runtime.enable", {}, sessionId, 5000);
    } catch (e) {
      log("Runtime.enable failed", e.message);
    }
    const tabId = `tab-${randomBytes(4).toString("hex")}`;
    const tab = { tabId, targetId, sessionId, url };
    this.tabs.set(tabId, tab);
    if (activate || !this.defaultTabId) this.defaultTabId = tabId;
    log("newTab", tabId, targetId);
    return tab;
  }

  resolveTab(tabId) {
    if (tabId) {
      const tab = this.tabs.get(tabId);
      if (!tab) throw new Error(`Unknown tabId "${tabId}"`);
      return tab;
    }
    const tab = this.tabs.get(this.defaultTabId);
    if (!tab) throw new Error("No open tab; run nav.js first");
    return tab;
  }

  /** Round-robin tab selection, used when a command is explicitly broadcast. */
  pickTab(tabId) {
    if (tabId) return this.resolveTab(tabId);
    const all = [...this.tabs.values()];
    if (all.length === 0) throw new Error("No open tab; run nav.js first");
    if (all.length === 1) return all[0];
    this.rr = (this.rr + 1) % all.length;
    return all[this.rr];
  }

  listTabs() {
    return [...this.tabs.values()].map((tab) => ({
      tabId: tab.tabId,
      targetId: tab.targetId,
      url: tab.url,
      default: tab.tabId === this.defaultTabId,
    }));
  }

  /**
   * Ask every tab for its live URL. Tabs navigate on their own (click.js,
   * in-page location changes), so the cached url is not trustworthy.
   */
  async refreshUrls() {
    // A page stuck in a busy loop blocks the whole connection, so cap the whole
    // refresh: a stale URL is far better than making `tabs.js` hang. Tabs we
    // could not reach keep their last known URL.
    const timeoutMs = Number(process.env.OBSCURA_URL_REFRESH_MS ?? 1200);
    if (this.refreshing) return this.listTabs(); // one refresh at a time
    this.refreshing = true;
    setTimeout(() => { this.refreshing = false; }, timeoutMs + 200).unref?.();
    const deadline = new Promise((resolve) => setTimeout(resolve, timeoutMs));
    const work = Promise.all(
      [...this.tabs.values()].map(async (tab) => {
        try {
          const result = await this.cdp.send(
            "Runtime.evaluate",
            { expression: "location.href", returnByValue: true },
            tab.sessionId,
            timeoutMs,
          );
          const url = result?.result?.value;
          if (typeof url === "string" && url) tab.url = url;
        } catch {
          // Unreachable (navigating or wedged): keep the last known URL.
        }
      }),
    );
    await Promise.race([work, deadline]);
    return this.listTabs();
  }

  async closeTab(tabId) {
    const tab = this.resolveTab(tabId);
    try {
      await this.cdp.send("Target.closeTarget", { targetId: tab.targetId });
    } catch (e) {
      log("closeTarget failed", e.message);
    }
    this.tabs.delete(tab.tabId);
    if (this.defaultTabId === tab.tabId) {
      this.defaultTabId = this.tabs.keys().next().value ?? null;
    }
    // Never leave the broker tab-less: every command would fail with
    // "No open tab" and nav.js is the only way back. Replace the last tab.
    if (this.tabs.size === 0) {
      await this.newTab({ url: "about:blank" });
      return { closed: tab.tabId, replacedWith: this.defaultTabId };
    }
    return { closed: tab.tabId };
  }

  async handle(request) {
    const op = request.op || "cdp";

    if (op === "health") {
      // Liveness = the CDP socket is still open. Do NOT ping with a CDP call:
      // a page stuck in a busy loop blocks every request, so a ping would make
      // `health` (called by every command) wait out its timeout while the
      // broker is perfectly healthy.
      let upstream = "ok";
      const state = this.cdp.ws?.readyState;
      if (state !== undefined && state !== 1) {
        upstream = "closed";
        setImmediate(() => shutdown("upstream socket closed"));
      }
      return {
        pid: process.pid,
        startedAt: this.startedAt,
        upstream,
        tabs: this.listTabs(),
        defaultTabId: this.defaultTabId,
      };
    }

    if (op === "tabs") {
      // Never block the reply on page evaluation: a page in a busy loop blocks
      // the whole CDP connection, and `tabs.js` must still answer. Kick off a
      // best-effort refresh for the next call and return what we know now.
      this.refreshUrls().catch(() => {});
      return { tabs: this.listTabs() };
    }

    if (op === "events") {
      const since = Number(request.since ?? 0);
      const method = request.method;
      const limit = Number(request.limit ?? 200);
      return {
        events: this.events
          .filter((e) => e.seq > since && (!method || e.method === method))
          .slice(-limit),
        lastSeq: this.eventSeq,
      };
    }

    if (op === "newTab") return this.newTab(request);

    if (op === "closeTab") return this.closeTab(request.tabId);

    if (op === "select") {
      const tab = this.resolveTab(request.tabId);
      this.defaultTabId = tab.tabId;
      return { defaultTabId: this.defaultTabId };
    }

    if (op === "shutdown") {
      setImmediate(() => shutdown("requested"));
      return { shutdown: true };
    }

    if (op !== "cdp") throw new Error(`Unknown op "${op}"`);

    // Track the tab URL after navigation so `tabs` stays truthful.
    if (request.method === "Page.navigate") {
      const tab = this.resolveTab(request.tabId);
      const result = await this.cdp.send(
        request.method,
        request.params || {},
        tab.sessionId,
        request.timeout ?? 30000,
      );
      tab.url = request.params?.url ?? tab.url;
      return result;
    }

    if (request.method === "Target.getTargets") {
      const tabs = await this.refreshUrls();
      return { targetInfos: tabs.map((tab) => ({
        targetId: tab.targetId, type: "page", url: tab.url, title: "",
      })) };
    }

    const tab = request.broadcast
      ? this.pickTab(request.tabId)
      : this.resolveTab(request.tabId);

    return this.cdp.send(
      request.method,
      request.params || {},
      tab.sessionId,
      request.timeout ?? 30000,
    );
  }
}

let broker = null;
let server = null;

function cleanupFiles() {
  for (const file of [SOCKET_FILE, BROKER_PID_FILE, LOCK_FILE]) {
    try {
      rmSync(file, { force: true });
    } catch {
      // ignore
    }
  }
}

async function shutdown(reason) {
  log("shutdown:", reason);
  try {
    server?.close();
  } catch {
    // ignore
  }
  try {
    broker?.cdp.close();
  } catch {
    // ignore
  }
  cleanupFiles();
  process.exit(0);
}

/**
 * Take an exclusive start lock so two concurrent start.js runs cannot both spawn
 * a broker. The loser exits instead of unlinking the winner's socket, which
 * used to leave an unreachable broker holding an open CDP connection.
 */
function acquireStartLock() {
  ensureDir(BROWSER_ROOT);
  try {
    writeFileSync(LOCK_FILE, String(process.pid), { flag: "wx" });
    return true;
  } catch {
    try {
      const holder = Number(readFileSync(LOCK_FILE, "utf8").trim());
      if (holder && holder !== process.pid && isProcessAlive(holder)) return false;
      writeFileSync(LOCK_FILE, String(process.pid));
      return true;
    } catch {
      return false;
    }
  }
}

export async function main() {
  if (!acquireStartLock()) {
    process.stderr.write("another broker is already starting\n");
    process.exit(3);
  }
  if (existsSync(SOCKET_FILE)) unlinkSync(SOCKET_FILE);
  ensureDir(BROWSER_ROOT);

  const cdp = await connect(5000);
  broker = new Broker(cdp);
  broker.startedAt = new Date().toISOString();

  // Buffer the events that a one-shot CLI cannot subscribe to.
  cdp.on("Runtime.exceptionThrown", (params) =>
    broker.recordEvent("Runtime.exceptionThrown", params),
  );
  cdp.on("Runtime.consoleAPICalled", (params) =>
    broker.recordEvent("Runtime.consoleAPICalled", params),
  );

  // A default tab so the first command has somewhere to run. Obscura exposes a
  // phantom page over /json/list that is not reachable here, so create real one.
  await broker.newTab({ url: "about:blank" });

  server = net.createServer((socket) => {
    let buffer = "";
    socket.on("data", async (chunk) => {
      buffer += chunk.toString();
      let index;
      while ((index = buffer.indexOf("\n")) !== -1) {
        const line = buffer.slice(0, index);
        buffer = buffer.slice(index + 1);
        if (!line.trim()) continue;
        let request;
        try {
          request = JSON.parse(line);
        } catch (e) {
          socket.write(`${JSON.stringify({ ok: false, error: `Bad JSON: ${e.message}` })}\n`);
          continue;
        }
        try {
          const result = await broker.handle(request);
          socket.write(`${JSON.stringify({ ok: true, result })}\n`);
        } catch (e) {
          socket.write(`${JSON.stringify({ ok: false, error: e.message })}\n`);
        }
      }
    });
    socket.on("error", (e) => log("socket error", e.message));
  });

  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(SOCKET_FILE, resolve);
  });

  writeFileSync(BROKER_PID_FILE, `${process.pid}\n`);

  // Register the broker without clobbering the server entry written by start.js.
  const previous = readState() || {};
  writeState({
    ...previous,
    brokerPid: process.pid,
    socket: SOCKET_FILE,
    brokerStartedAt: broker.startedAt,
  });

  log("listening on", SOCKET_FILE);
  process.stdout.write("BROKER_READY\n");

  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGINT", () => shutdown("SIGINT"));
  process.on("exit", cleanupFiles);
}

const invokedDirectly =
  process.argv[1] && process.argv[1].endsWith("broker.js");

if (invokedDirectly) {
  main().catch((e) => {
    console.error("✗ broker failed:", e.message);
    cleanupFiles();
    process.exit(1);
  });
}
