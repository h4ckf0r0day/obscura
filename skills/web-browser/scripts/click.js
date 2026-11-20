#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Click an element for real.
 *
 * Measured on Obscura 0.2.7: Input.dispatchMouseEvent is viewport-relative and
 * does NOT scroll for you — an out-of-viewport coordinate is delivered to the
 * document but hits nothing. So this scrolls the element to centre first, then
 * refuses coordinates outside the viewport instead of clicking into the void.
 */

import { cdpCall, ensureBroker, evaluate, sessionAdapter } from "./broker-client.js";
import { applyActiveEmulation } from "./emulation-state.js";
import { parseFlags } from "./cli.js";

const DEBUG = process.env.DEBUG === "1";
const log = DEBUG ? (...args) => console.error("[debug]", ...args) : () => {};

const { positional, get, has, unknown } = parseFlags(process.argv.slice(2), { values: ["--text", "--index", "--tab", "--timeout"] });

if (unknown.length > 0) console.error(`✗ Unknown option: ${unknown.join(", ")}`);

const selector = positional[0];
if (!selector || unknown.length > 0) {
  console.log("Usage: click.js <selector|js-expression> [options]");
  console.log("\nOptions:");
  console.log("  --text=<substring>  Pick the element whose text matches (buttons/links)");
  console.log("  --index=<n>         Which match to click (default 0)");
  console.log("  --js                Treat the argument as a JS expression returning the element");
  console.log("  --no-scroll         Skip scrolling (element must already be in view)");
  console.log("  --tab=<id>          Target tab");
  console.log("\nExamples:");
  console.log("  click.js 'button[type=submit]'");
  console.log("  click.js 'a' --text='Learn more'");
  console.log("  click.js 'document.querySelector(\"#go\")' --js");
  process.exit(1);
}

const timeout = get("--timeout", 15000);
const index = get("--index", 0);

const globalTimeout = setTimeout(() => {
  console.error("✗ Global timeout exceeded (60s)");
  process.exit(1);
}, 60000);

try {
  await ensureBroker();
  log("broker ready");

  const tabId = get("--tab", undefined);
  await applyActiveEmulation(sessionAdapter(tabId), null);

  // Resolve the element. --js allows an arbitrary expression; otherwise the
  // selector is escaped safely via JSON.stringify.
  const byJs = has("--js");
  const textMatch = get("--text", undefined);
  const elementExpr = byJs
    ? String(selector)
    : textMatch
      ? `(() => {
          const wanted = ${JSON.stringify(String(textMatch))}.toLowerCase();
          const nodes = [...document.querySelectorAll(${JSON.stringify(selector)})];
          return nodes.filter(n => (n.textContent || n.value || "").toLowerCase().includes(wanted))[${index}] || null;
        })()`
      : `document.querySelectorAll(${JSON.stringify(selector)})[${index}] || null`;

  const box = await evaluate(
    `(() => {
      const el = ${elementExpr};
      if (!el) return null;
      ${has("--no-scroll") ? "" : `el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });`}
      const r = el.getBoundingClientRect();
      return {
        x: r.x + r.width / 2,
        y: r.y + r.height / 2,
        w: Math.round(r.width),
        h: Math.round(r.height),
        tag: el.tagName.toLowerCase(),
        text: (el.textContent || el.value || "").trim().slice(0, 60),
        iw: innerWidth,
        ih: innerHeight,
        href: el.href || null,
      };
    })()`,
    { tabId },
  );

  if (!box) {
    console.error(`✗ No element matched ${byJs ? "expression" : JSON.stringify(selector)}${textMatch ? ` with text ${JSON.stringify(textMatch)}` : ""}`);
    process.exitCode = 1;
  } else if (box.w === 0 || box.h === 0) {
    console.error(`✗ Element <${box.tag}> has zero size (hidden?)`);
    process.exitCode = 1;
  } else if (box.x < 0 || box.y < 0 || box.x >= box.iw || box.y >= box.ih) {
    console.error(
      `✗ Element centre (${Math.round(box.x)},${Math.round(box.y)}) is outside the ${box.iw}x${box.ih} viewport; nothing would be hit`,
    );
    process.exitCode = 1;
  } else {
    const point = { x: box.x, y: box.y };
    await cdpCall("Input.dispatchMouseEvent", { type: "mouseMoved", ...point }, { tabId, timeout });
    await cdpCall(
      "Input.dispatchMouseEvent",
      { type: "mousePressed", button: "left", clickCount: 1, ...point },
      { tabId, timeout },
    );
    await cdpCall(
      "Input.dispatchMouseEvent",
      { type: "mouseReleased", button: "left", clickCount: 1, ...point },
      { tabId, timeout },
    );

    console.log(`✓ Clicked <${box.tag}> at ${Math.round(box.x)},${Math.round(box.y)}`);
    if (box.text) console.log(`  text: ${box.text}`);
    if (box.href) console.log(`  href: ${box.href}`);

    // Report the URL after a settle, so a click that navigates is obvious.
    await new Promise((r) => setTimeout(r, 250));
    const after = await evaluate("({url: location.href, title: document.title})", { tabId });
    console.log(`  now: ${after.url}`);
  }
} catch (e) {
  console.error("✗", e.message);
  process.exitCode = 1;
} finally {
  clearTimeout(globalTimeout);
}
