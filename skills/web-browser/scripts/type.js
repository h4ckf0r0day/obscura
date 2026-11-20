#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Type into a form field.
 *
 * Measured on Obscura 0.2.7:
 * - Input.insertText fires only `input` (no keydown/keyup).
 * - Input.dispatchKeyEvent {type:"keyDown", text} fires keydown -> input -> keyup.
 * Framework-controlled inputs (React et al.) often ignore insertText, so
 * --keys is available to drive real key events instead.
 */

import { cdpCall, ensureBroker, evaluate, sessionAdapter } from "./broker-client.js";
import { applyActiveEmulation } from "./emulation-state.js";
import { parseFlags } from "./cli.js";

const DEBUG = process.env.DEBUG === "1";
const log = DEBUG ? (...args) => console.error("[debug]", ...args) : () => {};

const { positional, get, has, unknown } = parseFlags(process.argv.slice(2), { values: ["--tab", "--timeout"] });

if (unknown.length > 0) console.error(`✗ Unknown option: ${unknown.join(", ")}`);

const selector = positional[0];
const text = positional.slice(1).join(" ");

if (!selector || text === "" || unknown.length > 0) {
  console.log("Usage: type.js <selector> <text> [options]");
  console.log("\nOptions:");
  console.log("  --keys        Send real keydown/keyup per character (for framework inputs)");
  console.log("  --submit      Press Enter afterwards (submits a form)");
  console.log("  --clear       Clear the field first");
  console.log("  --tab=<id>    Target tab");
  console.log("\nExamples:");
  console.log("  type.js '#username' alice");
  console.log("  type.js 'input[name=q]' 'hello world' --submit");
  console.log("  type.js '#react-field' value --keys --clear");
  process.exit(1);
}

const timeout = get("--timeout", 15000);

const globalTimeout = setTimeout(() => {
  console.error("✗ Global timeout exceeded (60s)");
  process.exit(1);
}, 60000);

try {
  await ensureBroker();
  log("broker ready");

  const tabId = get("--tab", undefined);
  await applyActiveEmulation(sessionAdapter(tabId), null);

  // Focus the field and make sure it is on screen before any input event.
  const focused = await evaluate(
    `(() => {
      const el = document.querySelector(${JSON.stringify(selector)});
      if (!el) return null;
      el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
      el.focus();
      if (${has("--clear") ? "true" : "false"}) {
        if ("value" in el) {
          el.value = "";
          el.dispatchEvent(new Event("input", { bubbles: true }));
        } else if (el.isContentEditable) {
          el.textContent = "";
        }
      }
      const r = el.getBoundingClientRect();
      return {
        tag: el.tagName.toLowerCase(),
        type: el.type || null,
        iw: innerWidth, ih: innerHeight,
        x: r.x + r.width / 2, y: r.y + r.height / 2,
        focused: document.activeElement === el,
      };
    })()`,
    { tabId },
  );

  if (!focused) {
    console.error(`✗ No element matched ${JSON.stringify(selector)}`);
    process.exitCode = 1;
  } else if (!focused.focused) {
    console.error(`✗ <${focused.tag}> could not take focus (disabled or read-only?)`);
    process.exitCode = 1;
  } else {
    if (has("--keys")) {
      for (const ch of text) {
        await cdpCall(
          "Input.dispatchKeyEvent",
          { type: "keyDown", key: ch, text: ch, unmodifiedText: ch },
          { tabId, timeout },
        );
        await cdpCall("Input.dispatchKeyEvent", { type: "keyUp", key: ch }, { tabId, timeout });
      }
    } else {
      await cdpCall("Input.insertText", { text }, { tabId, timeout });
    }

    if (has("--submit")) {
      await cdpCall(
        "Input.dispatchKeyEvent",
        { type: "keyDown", key: "Enter", text: "\r", windowsVirtualKeyCode: 13 },
        { tabId, timeout },
      );
      await cdpCall(
        "Input.dispatchKeyEvent",
        { type: "keyUp", key: "Enter", windowsVirtualKeyCode: 13 },
        { tabId, timeout },
      );
      await new Promise((r) => setTimeout(r, 400));
    }

    const state = await evaluate(
      `(() => {
        const el = document.querySelector(${JSON.stringify(selector)});
        return {
          value: el && "value" in el ? el.value : (el ? el.textContent : null),
          url: location.href,
          title: document.title,
        };
      })()`,
      { tabId },
    );

    console.log(`✓ Typed ${text.length} char(s) into <${focused.tag}>${focused.type ? `[type=${focused.type}]` : ""}${has("--keys") ? " via key events" : " via insertText"}`);
    console.log(`  value: ${JSON.stringify(state.value)}`);
    if (has("--submit")) console.log(`  now: ${state.url}`);
  }
} catch (e) {
  console.error("✗", e.message);
  process.exitCode = 1;
} finally {
  clearTimeout(globalTimeout);
}
