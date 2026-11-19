#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Navigate the active broker tab.
 *
 * Obscura's Page.navigate resolves after `load` (with a ~2 s subresource cap),
 * so there is nothing to wait for afterwards — waiting for Page.loadEventFired
 * here would hang forever.
 */

import {
  brokerRequest,
  cdpCall,
  ensureBroker,
  evaluate,
  sessionAdapter,
} from "./broker-client.js";
import { applyActiveEmulation } from "./emulation-state.js";
import { parseFlags } from "./cli.js";

const DEBUG = process.env.DEBUG === "1";
const log = DEBUG ? (...args) => console.error("[debug]", ...args) : () => {};

const { positional, get, has, unknown } = parseFlags(process.argv.slice(2), { values: ["--settle", "--timeout", "--tab", "--wait-until"] });

if (unknown.length > 0) {
  console.error(`✗ Unknown option: ${unknown.join(", ")}`);
}
if (positional.length === 0 || unknown.length > 0) {
  console.log("Usage: nav.js <url> [--new] [--settle=<ms>] [--timeout=<ms>] [--tab=<id>]");
  console.log("\nExamples:");
  console.log("  nav.js https://example.com");
  console.log("  nav.js https://example.com --new         # always a fresh tab");
  console.log("  nav.js https://example.com --settle=500  # extra paint time");
  console.log("\nNote: Page.navigate already waits for load; --settle is additive.");
  process.exit(1);
}

const url = positional[0];
const timeout = get("--timeout", 30000);
const settle = get("--settle", 0);

const globalTimeout = setTimeout(() => {
  console.error("✗ Global timeout exceeded (60s)");
  process.exit(1);
}, 60000);

try {
  await ensureBroker();
  log("broker ready");

  let tabId = get("--tab", undefined);

  if (has("--new")) {
    const tab = await brokerRequest({ op: "newTab", url: "about:blank" });
    tabId = tab.tabId;
    console.log(`✓ Opened tab ${tab.tabId}`);
  }

  await applyActiveEmulation(sessionAdapter(tabId), null);

  const started = Date.now();
  // Resolves after `load`; rejects on DNS/scheme/private-IP errors.
  await cdpCall("Page.navigate", { url }, { tabId, timeout });
  const elapsed = Date.now() - started;

  if (settle > 0) await new Promise((r) => setTimeout(r, settle));

  const state = await evaluate(
    "({url: location.href, title: document.title, ready: document.readyState})",
    { tabId },
  );

  console.log(`✓ ${state.url}`);
  console.log(`  title: ${state.title || "(empty)"}`);
  console.log(`  loaded in ${elapsed}ms, readyState=${state.ready}`);
  if (tabId) console.log(`  tab: ${tabId}`);
} catch (e) {
  console.error("✗", e.message);
  process.exitCode = 1;
} finally {
  clearTimeout(globalTimeout);
}
