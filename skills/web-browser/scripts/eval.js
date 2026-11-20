#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Evaluate JavaScript in the active tab.
 *
 * Obscura has no Runtime.awaitPromise, but Runtime.evaluate with
 * awaitPromise:true does await correctly. The whole evaluation therefore runs
 * inside one async IIFE and returns by value; there is no REPL round trip.
 */

import {
  cdpCall,
  ensureBroker,
  evaluate,
  remoteObjectValue,
  sessionAdapter,
} from "./broker-client.js";
import { applyActiveEmulation } from "./emulation-state.js";
import { parseFlags } from "./cli.js";

const DEBUG = process.env.DEBUG === "1";
const log = DEBUG ? (...args) => console.error("[debug]", ...args) : () => {};

const { positional, get, has, unknown } = parseFlags(process.argv.slice(2), { values: ["--tab", "--timeout"] });

if (unknown.length > 0) console.error(`✗ Unknown option: ${unknown.join(", ")}`);

const code = positional.join(" ");
if (!code || unknown.length > 0) {
  console.log("Usage: eval.js '<code>' [--tab=<id>] [--json] [--timeout=<ms>]");
  console.log("\nExamples:");
  console.log("  eval.js 'document.title'");
  console.log("  eval.js 'document.querySelectorAll(\"a\").length'");
  console.log("  eval.js 'await fetch(\"/api\").then(r => r.json())'");
  console.log("  eval.js '[...document.querySelectorAll(\"a\")].map(a => a.href)' --json");
  console.log("\nPromises and top-level await are awaited. Use single quotes to avoid escaping.");
  process.exit(1);
}

const timeout = get("--timeout", 30000);

const globalTimeout = setTimeout(() => {
  console.error(`✗ Global timeout exceeded (${Math.ceil(timeout / 1000) + 30}s)`);
  process.exit(1);
}, timeout + 30000);

function format(value) {
  if (has("--json")) {
    return JSON.stringify(value, (_key, v) => (typeof v === "bigint" ? v.toString() : v), 2);
  }
  // Non-JSON scalars: print them truthfully instead of "undefined".
  if (typeof value === "bigint") return `${value}n`;
  if (typeof value === "number" && !Number.isFinite(value)) return String(value);
  if (typeof value === "number" && Object.is(value, -0)) return "-0";
  if (Array.isArray(value)) {
    return value
      .map((item) => {
        if (item && typeof item === "object" && !Array.isArray(item)) {
          return Object.entries(item)
            .map(([k, v]) => `${k}: ${typeof v === "object" ? JSON.stringify(v) : v}`)
            .join("\n");
        }
        return String(item);
      })
      .join("\n\n");
  }
  if (value && typeof value === "object") {
    return Object.entries(value)
      .map(([k, v]) => `${k}: ${typeof v === "object" ? JSON.stringify(v) : v}`)
      .join("\n");
  }
  return String(value);
}

try {
  await ensureBroker();
  log("broker ready");
  await applyActiveEmulation(sessionAdapter(get("--tab", undefined)), null);

  // Statement lists and bare expressions both work inside the IIFE; `return`
  // picks up the completion value of a trailing expression.
  const expression = `(async () => { return (${code}); })()`;

  const tabId = get("--tab", undefined);
  let result;

  // Obscura's returnByValue flattens an array/object containing a BigInt into a
  // string ("10,1" for [10n,1]). For --json, serialise inside the page instead,
  // where a proper replacer can preserve BigInt as a JSON string.
  const jsonExpression = `(() => {
    const v = (${code});
    return JSON.stringify(v, (k, x) => typeof x === "bigint" ? x.toString() : x, 2) ?? "null";
  })()`;

  try {
    if (has("--json")) {
      const text = await evaluate(jsonExpression, { tabId, timeout });
      console.log(text);
    } else {
      result = await evaluate(expression, { tabId, timeout });
      console.log(format(result));
    }
  } catch (e) {
    // Not a single expression (e.g. `const x = 1; ...`): run it as statements.
    // Obscura reports this as exceptionDetails rather than a CDP error.
    if (!/SyntaxError|Unexpected token|Unexpected identifier/i.test(e.message)) {
      throw e;
    }
    log("expression failed, retrying as statement list");
    // A block scopes the statements so repeated `const x = ...` across separate
    // eval.js runs does not collide in the page's global scope.
    const fallback = await cdpCall(
      "Runtime.evaluate",
      {
        expression: `{ ${code}\n}`,
        returnByValue: true,
        awaitPromise: true,
        replMode: true,
      },
      { tabId, timeout },
    );
    if (fallback.exceptionDetails) {
      throw new Error(
        fallback.exceptionDetails.exception?.description ||
          fallback.exceptionDetails.text,
      );
    }
    // remoteObjectValue recovers NaN/-0/Infinity/BigInt, which live in
    // `unserializableValue`; `value` alone drops them to undefined.
    result =
      remoteObjectValue(fallback.result) ?? fallback.result?.description;
    console.log(format(result));
  }
} catch (e) {
  console.error("✗", e.message);
  process.exitCode = 1;
} finally {
  clearTimeout(globalTimeout);
}
