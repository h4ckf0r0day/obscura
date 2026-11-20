#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * One-shot page read via the Obscura CLI — no server, no broker.
 *
 * This is the fast path for "fetch a page and give me something": `obscura
 * fetch` starts, loads, extracts and exits, with no CDP round trips. Use the
 * broker commands (nav/eval/click) only when you need state or interaction.
 */

import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { getConnectionInfo, resolveObscuraBinary, stealthEnabled } from "./obscura.js";
import { parseFlags } from "./cli.js";

const DEBUG = process.env.DEBUG === "1";
const log = DEBUG ? (...args) => console.error("[debug]", ...args) : () => {};

const { positional, get, has, unknown } = parseFlags(process.argv.slice(2), {
  values: ["--eval", "--selector", "--wait", "--wait-until", "--timeout"],
  // Extraction modes belong to this script; the global boolean set lacks them.
  booleans: new Set([
    "--text", "--html", "--markdown", "--links", "--assets", "--original",
    "--cookies", "--json", "--raw", "--no-stealth", "--attach", "--help",
    "--allow-private-network",
  ]),
});

if (unknown.length > 0) {
  console.error(`✗ Unknown option: ${unknown.join(", ")}`);
}

const url = positional[0];
if (!url || unknown.length > 0) {
  console.log("Usage: read.js <url> [options]");
  console.log("\nExtraction:");
  console.log("  --text            Page text (default)");
  console.log("  --html            Rendered DOM as HTML");
  console.log("  --markdown        Readable markdown");
  console.log("  --links           All links");
  console.log("  --assets          Sub-resource URLs (NDJSON)");
  console.log("  --original        Raw response body, binary-safe");
  console.log("  --cookies         Cookie jar as JSON");
  console.log("\nNetwork:");
  console.log("  --allow-private-network  Permit localhost/LAN URLs (blocked by default)");
  console.log("\nExtraction with JS:");
  console.log("  --eval='<js>'     Run JS after load (default dump: text)");
  console.log("  --selector='<css>' Restrict extraction to a selector");
  console.log("\nTiming:");
  console.log("  --wait=<sec>      Fixed settle delay");
  console.log("  --wait-until=<s>  load | domcontentloaded | networkidle0");
  console.log("  --timeout=<sec>   Navigation timeout (default 30)");
  console.log("\nExamples:");
  console.log("  read.js https://example.com");
  console.log("  read.js https://example.com --markdown");
  console.log("  read.js https://news.ycombinator.com --eval='[...document.querySelectorAll(\".titleline>a\")].map(a=>({t:a.textContent,h:a.href}))' --json");
  console.log("\nNote: for stateful/interactive work use start.js + nav.js/eval.js/click.js.");
  process.exit(1);
}

const binary = resolveObscuraBinary();
if (!binary) {
  console.error("✗ Could not find the obscura binary (set OBSCURA_BIN)");
  process.exit(1);
}

const dumpFlag =
  (has("--html") && "html") ||
  (has("--markdown") && "markdown") ||
  (has("--links") && "links") ||
  (has("--assets") && "assets") ||
  (has("--original") && "original") ||
  (has("--cookies") && "cookies") ||
  "text";

const args = ["fetch", url];
const evalCode = get("--eval", undefined);
const selector = get("--selector", undefined);

const timeout = get("--timeout", 30);
args.push("--timeout", String(timeout));

args.push("--dump", evalCode ? "text" : dumpFlag);
if (evalCode) args.push("--eval", String(evalCode));
if (selector) args.push("--selector", String(selector));

const wait = get("--wait", undefined);
if (wait !== undefined) args.push("--wait", String(wait));
const waitUntil = get("--wait-until", undefined);
if (waitUntil) args.push("--wait-until", String(waitUntil));

if (stealthEnabled() && !has("--no-stealth")) args.push("--stealth");
const { token } = getConnectionInfo();
if (process.env.OBSCURA_ALLOW_PRIVATE_NETWORK || has("--allow-private-network")) {
  args.push("--allow-private-network");
}
if (process.env.OBSCURA_PROXY) args.push("--proxy", process.env.OBSCURA_PROXY);
if (process.env.OBSCURA_USER_AGENT) args.push("--user-agent", process.env.OBSCURA_USER_AGENT);

log(binary, args.join(" "));

const started = Date.now();
const outDir = mkdtempSync(join(tmpdir(), "agent-read-"));

const child = spawn(binary, args, {
  stdio: ["ignore", "pipe", "pipe"],
  env: {
    ...process.env,
    ...(token && has("--attach") ? { OBSCURA_CDP_TOKEN: token } : {}),
  },
});

const stdoutChunks = [];
const stderrChunks = [];
child.stdout.on("data", (c) => stdoutChunks.push(c));
child.stderr.on("data", (c) => stderrChunks.push(c));

const hardTimeout = setTimeout(() => {
  child.kill("SIGKILL");
}, (Number(timeout) + 30) * 1000);

// A spawn failure (EACCES/ENOENT) emits "error" and never "close", so clean up
// and report here instead of leaking the temp dir and dumping a stack trace.
let spawnError = null;
child.on("error", (error) => {
  spawnError = error;
  clearTimeout(hardTimeout);
  rmSync(outDir, { recursive: true, force: true });
  console.error(`✗ Could not run ${binary}: ${error.message}`);
  console.error("  Check OBSCURA_BIN, or that the binary is executable.");
  process.exitCode = 1;
});

child.on("close", (code) => {
  clearTimeout(hardTimeout);
  // "close" can follow a spawn "error"; the error path already reported it.
  if (spawnError) return;
  const stdout = Buffer.concat(stdoutChunks).toString("utf8");
  const stderr = Buffer.concat(stderrChunks).toString("utf8");
  const elapsed = Date.now() - started;

  rmSync(outDir, { recursive: true, force: true });

  if (code !== 0) {
    // Obscura prints failures on stderr; surface them without the banner noise.
    const message = (stderr || stdout).split("\n").filter(Boolean).slice(-3).join("\n");
    console.error("✗", message || `obscura exited with code ${code}`);
    process.exitCode = 1;
    return;
  }

  // `fetch` writes a "Page loaded: <url> - <title>" banner to stderr.
  const banner = stderr.split("\n").find((l) => l.startsWith("Page loaded:"));
  if (has("--raw") || dumpFlag === "original") {
    process.stdout.write(stdout);
    return;
  }

  let body = stdout;
  if (evalCode && has("--json")) {
    const lines = body.split("\n").filter(Boolean);
    const candidate = lines[lines.length - 1];
    try {
      body = JSON.stringify(JSON.parse(candidate), null, 2);
    } catch {
      // Not JSON; leave as-is.
    }
  }

  if (body.trim() === "") {
    console.log("(empty result)");
  } else {
    process.stdout.write(body.endsWith("\n") ? body : `${body}\n`);
  }
  if (DEBUG && banner) console.error(`[debug] ${banner} in ${elapsed}ms`);
});
