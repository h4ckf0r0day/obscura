#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Start the Obscura CDP server and the persistent broker.
 *
 * Obscura replaces Chrome entirely here: no Chrome/Chromium binary is located,
 * launched or required. `serve` is the CDP server, `broker.js` keeps one
 * connection alive so page state survives between commands.
 */

import { spawn } from "node:child_process";
import { closeSync, existsSync, openSync } from "node:fs";
import {
  BROWSER_ROOT,
  SERVER_LOG_FILE,
  STATE_FILE,
  STORAGE_DIR,
  clearState,
  ensureDir,
  ensureToken,
  findObscuraProcesses,
  getConnectionInfo,
  getHost,
  getPort,
  globalObscuraArgs,
  isProcessAlive,
  readState,
  resolveObscuraBinary,
  writeState,
} from "./obscura.js";
import { ensureBroker, brokerPid, isBrokerRunning } from "./broker-client.js";

const args = process.argv.slice(2);
const flags = new Set(args);
const known = new Set([
  "--restart",
  "--keep-volume",
  "--reset-storage",
  "--no-broker",
  "--no-stealth",
  "--allow-private-network",
  "--obey-robots",
  "--quiet",
]);

const unknown = args.filter((a) => !known.has(a) && !a.startsWith("--workers="));
if (unknown.length > 0) {
  console.log("Usage: start.js [options]");
  console.log("\nStarts the Obscura CDP server plus the persistent state broker.");
  console.log("\nOptions:");
  console.log("  --restart               Restart the server (and broker) even if running");
  console.log("  --reset-storage         Delete the persisted profile (cookies, localStorage) first");
  console.log("  --no-broker             Start only the CDP server, no state broker");
  console.log("  --no-stealth            Disable stealth (default on: it defeats Cloudflare)");
  console.log("  --allow-private-network Allow localhost/LAN URLs (default: blocked by Obscura)");
  console.log("  --obey-robots           Respect robots.txt");
  console.log("  --workers=N             Obscura worker processes (default 1)");
  console.log("  --quiet                 Minimal output");
  console.log("\nEnvironment:");
  console.log("  BROWSER_DEBUG_PORT / OBSCURA_PORT   CDP port (default 9222)");
  console.log("  OBSCURA_BIN                         Path to the obscura binary");
  console.log("  OBSCURA_STEALTH=0                   Same as --no-stealth");
  console.log("  OBSCURA_ALLOW_PRIVATE_NETWORK=1     Same as --allow-private-network");
  console.log("  OBSCURA_PROXY / OBSCURA_USER_AGENT  Proxy and UA for every request");
  process.exit(1);
}

if (flags.has("--no-stealth")) process.env.OBSCURA_STEALTH = "0";
if (flags.has("--allow-private-network")) {
  process.env.OBSCURA_ALLOW_PRIVATE_NETWORK = "1";
}

const quiet = flags.has("--quiet");
const say = (message) => {
  if (!quiet) console.log(message);
};

async function endpointAnswering() {
  const { httpUrl, token } = getConnectionInfo();
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 1000);
  try {
    const resp = await fetch(`${httpUrl}/json/version`, {
      signal: controller.signal,
      headers: token ? { Authorization: `Bearer ${token}` } : undefined,
    });
    return resp.ok ? true : resp.status === 401 ? "auth" : false;
  } catch {
    return false;
  } finally {
    clearTimeout(timeout);
  }
}

async function stopServer() {
  const state = readState();
  const port = getPort();
  const pids = new Set();
  if (state?.serverPid && isProcessAlive(state.serverPid)) pids.add(state.serverPid);
  for (const proc of findObscuraProcesses({ storageDir: STORAGE_DIR, port })) {
    pids.add(proc.pid);
  }
  if (pids.size === 0) return false;

  for (const pid of pids) {
    try {
      process.kill(pid, "SIGTERM");
    } catch {
      // already gone
    }
  }
  for (let i = 0; i < 40; i++) {
    if ([...pids].every((pid) => !isProcessAlive(pid))) return true;
    await new Promise((r) => setTimeout(r, 250));
  }
  for (const pid of pids) {
    try {
      process.kill(pid, "SIGKILL");
    } catch {
      // already gone
    }
  }
  return true;
}

async function stopBroker() {
  if (!isBrokerRunning()) return false;
  const pid = brokerPid();
  try {
    process.kill(pid, "SIGTERM");
  } catch {
    return false;
  }
  for (let i = 0; i < 20; i++) {
    if (!isProcessAlive(pid)) return true;
    await new Promise((r) => setTimeout(r, 100));
  }
  try {
    process.kill(pid, "SIGKILL");
  } catch {
    // already gone
  }
  return true;
}

async function main() {
  ensureDir(BROWSER_ROOT);
  ensureDir(STORAGE_DIR);
  ensureDir(process.env.HOME ? `${process.env.HOME}/.cache/agent-web/logs` : BROWSER_ROOT);

  const binary = resolveObscuraBinary();
  if (!binary) {
    console.error("✗ Could not find the obscura binary");
    console.error("  Install it from https://github.com/h4ckf0r0day/obscura/releases");
    console.error("  or set OBSCURA_BIN=/path/to/obscura");
    process.exit(1);
  }

  if (flags.has("--reset-storage")) {
    const { rmSync } = await import("node:fs");
    rmSync(STORAGE_DIR, { recursive: true, force: true });
    ensureDir(STORAGE_DIR);
    say("✓ Storage profile reset");
  }

  const port = getPort();
  const host = getHost();
  const token = ensureToken();

  const restart = flags.has("--restart");
  const answering = await endpointAnswering();

  if (answering === "auth") {
    console.error(`✗ :${port} is served by something that rejects our token`);
    console.error("  Set OBSCURA_CDP_TOKEN to the token it was started with, or use another port.");
    process.exit(1);
  }

  if (answering && !restart) {
    const state = readState();
    if (state?.serverPid && isProcessAlive(state.serverPid)) {
      say(`✓ Obscura already serving CDP on :${port} (pid ${state.serverPid})`);
    } else {
      say(`✓ A CDP endpoint is already answering on :${port}`);
      say("  Refusing to reuse an unknown instance; use --restart to replace it.");
      process.exit(1);
    }
  } else {
    if (answering || restart) {
      await stopBroker();
      await stopServer();
      clearState();
    }

    const serveArgs = [
      "serve",
      "--port", String(port),
      "--host", host,
      "--storage-dir", STORAGE_DIR,
      ...globalObscuraArgs(),
    ];
    if (flags.has("--obey-robots")) serveArgs.push("--obey-robots");
    const workersArg = args.find((a) => a.startsWith("--workers="));
    if (workersArg) serveArgs.push(`--workers=${workersArg.split("=")[1]}`);

    const logFd = openSync(SERVER_LOG_FILE, "a");
    const child = spawn(binary, serveArgs, {
      detached: true,
      stdio: ["ignore", logFd, logFd],
      env: { ...process.env, OBSCURA_CDP_TOKEN: token },
    });
    child.unref();
    closeSync(logFd);

    const deadline = Date.now() + 20000;
    let up = false;
    while (Date.now() < deadline) {
      if (await endpointAnswering()) {
        up = true;
        break;
      }
      await new Promise((r) => setTimeout(r, 100));
    }

    if (!up) {
      console.error(`✗ Obscura did not start listening on :${port}`);
      console.error(`  Log: ${SERVER_LOG_FILE}`);
      process.exit(1);
    }

    writeState({
      serverPid: child.pid,
      port,
      host,
      storageDir: STORAGE_DIR,
      stealth: globalObscuraArgs().includes("--stealth"),
      startedAt: new Date().toISOString(),
    });
    say(`✓ Obscura CDP server on :${port} (pid ${child.pid}, stealth ${globalObscuraArgs().includes("--stealth") ? "on" : "off"})`);
    say(`  storage: ${STORAGE_DIR}`);
  }

  if (flags.has("--no-broker")) {
    say("  broker: skipped (--no-broker)");
    return;
  }

  const { started } = await ensureBroker({ quiet });
  const state = readState();
  writeState({ ...(state || {}), brokerPid: brokerPid(), port });
  if (!started) say(`✓ Broker already running (pid ${brokerPid()})`);
  say(`  state survives between commands; stop.js tears everything down`);
}

main().catch((e) => {
  console.error("✗", e.message);
  process.exit(1);
});
