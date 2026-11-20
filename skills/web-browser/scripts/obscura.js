/**
 * Obscura discovery, configuration and paths.
 *
 * One place decides where the skill keeps its state and how it talks to the
 * browser, so start/stop/broker/clients never disagree.
 */

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";

const HOME = process.env["HOME"] || homedir();
export const CACHE_ROOT = join(HOME, ".cache", "agent-web");
export const BROWSER_ROOT = join(CACHE_ROOT, "browser");
export const LOG_ROOT = join(CACHE_ROOT, "logs");
export const STATE_FILE = join(BROWSER_ROOT, "state.json");
export const TOKEN_FILE = join(BROWSER_ROOT, "token");
export const SOCKET_FILE = join(BROWSER_ROOT, "broker.sock");
export const BROKER_PID_FILE = join(BROWSER_ROOT, "broker.pid");
export const LOCK_FILE = join(BROWSER_ROOT, "broker.lock");
export const STORAGE_DIR = join(BROWSER_ROOT, "storage");
export const BROKER_SCRIPT = new URL("./broker.js", import.meta.url).pathname;
export const SERVER_LOG_FILE = join(LOG_ROOT, "serve.log");
export const BROKER_START_TIMEOUT_MS = 20000;

export const DEFAULT_PORT = 9222;

export function ensureDir(path) {
  if (!existsSync(path)) mkdirSync(path, { recursive: true });
}

export function getPort() {
  const raw = process.env.BROWSER_DEBUG_PORT || process.env.OBSCURA_PORT;
  const port = Number(raw || DEFAULT_PORT);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`Invalid port "${raw}" (expected 1-65535)`);
  }
  return port;
}

export function getHost() {
  return process.env.OBSCURA_HOST || "127.0.0.1";
}

/**
 * Resolve CDP endpoint info without touching the network.
 */
export function getConnectionInfo() {
  const host = getHost();
  const port = getPort();
  return {
    host,
    port,
    httpUrl: `http://${host}:${port}`,
    token: readToken(),
  };
}

export function readToken() {
  const fromEnv = process.env.OBSCURA_CDP_TOKEN;
  if (fromEnv) return fromEnv;
  if (!existsSync(TOKEN_FILE)) return null;
  try {
    const token = readFileSync(TOKEN_FILE, "utf8").trim();
    return token || null;
  } catch {
    return null;
  }
}

export function ensureToken() {
  const existing = readToken();
  if (existing) return existing;
  // Obscura requires at least 32 bytes.
  const token = randomBytes(32).toString("hex");
  ensureDir(BROWSER_ROOT);
  writeFileSync(TOKEN_FILE, `${token}\n`, { mode: 0o600 });
  return token;
}

/**
 * Sensible runtime flags for Obscura, from environment.
 *
 * OBSCURA_STEALTH=0 disables stealth. Stealth is on by default: measured
 * against Cloudflare, non-stealth was blocked 3/3 and stealth passed 5/5.
 */
export function stealthEnabled() {
  const value = process.env.OBSCURA_STEALTH;
  if (value === undefined) return true;
  return !/^(0|false|no|off)$/i.test(value);
}

export function allowPrivateNetwork() {
  const value = process.env.OBSCURA_ALLOW_PRIVATE_NETWORK;
  return value !== undefined && !/^(0|false|no|off)$/i.test(value);
}

/** Global flags shared by fetch/scrape/serve/mcp. */
export function globalObscuraArgs({ stealth } = {}) {
  const args = [];
  if (stealth ?? stealthEnabled()) args.push("--stealth");
  if (allowPrivateNetwork()) args.push("--allow-private-network");
  if (process.env.OBSCURA_PROXY) args.push("--proxy", process.env.OBSCURA_PROXY);
  if (process.env.OBSCURA_USER_AGENT) {
    args.push("--user-agent", process.env.OBSCURA_USER_AGENT);
  }
  return args;
}

export function resolveObscuraBinary() {
  const explicit = process.env.OBSCURA_BIN;
  // An explicit override wins even when it is wrong: silently falling back to a
  // different binary hides a configuration mistake.
  if (explicit) return explicit;
  try {
    const found = execFileSync("sh", ["-c", "command -v obscura"], {
      encoding: "utf8",
    }).trim();
    if (found) return found;
  } catch {
    // fall through
  }
  for (const candidate of ["/usr/local/bin/obscura", "/usr/bin/obscura"]) {
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

export function isProcessAlive(pid) {
  if (!pid || typeof pid !== "number") return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

export function readState() {
  if (!existsSync(STATE_FILE)) return null;
  try {
    return JSON.parse(readFileSync(STATE_FILE, "utf8"));
  } catch {
    return null;
  }
}

export function writeState(state) {
  ensureDir(BROWSER_ROOT);
  writeFileSync(STATE_FILE, `${JSON.stringify(state, null, 2)}\n`);
}

export function clearState() {
  rmSync(STATE_FILE, { force: true });
}

export async function isEndpointUp({ timeout = 1000 } = {}) {
  const { httpUrl, token } = getConnectionInfo();
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), timeout);
  try {
    const resp = await fetch(`${httpUrl}/json/version`, {
      signal: controller.signal,
      headers: token ? { Authorization: `Bearer ${token}` } : undefined,
    });
    return resp.ok;
  } catch {
    return false;
  } finally {
    clearTimeout(timeoutId);
  }
}

/** Find running Obscura serve processes that use this skill's storage dir. */
export function parseObscuraProcesses(output, { storageDir, port }) {
  return output.split("\n").flatMap((line) => {
    const match = line.trim().match(/^(\d+)\s+(.+)$/);
    if (!match) return [];
    const [, pid, command] = match;
    if (!/(?:^|\/)obscura(?:-worker)?(?:\s|$)/.test(command)) return [];
    if (!/(?:^|\s)serve(?:\s|$)/.test(command)) return [];
    const usesStorage = storageDir
      ? command.includes(`--storage-dir=${storageDir}`) ||
        command.includes(`--storage-dir ${storageDir}`)
      : false;
    const portMatch = command.match(/(?:^|\s)(?:--port|-p)(?:=|\s)(\d+)(?:\s|$)/);
    const commandPort = portMatch ? Number(portMatch[1]) : DEFAULT_PORT;
    if (!usesStorage && !(port && commandPort === port)) return [];
    return [{ pid: Number(pid), port: commandPort, usesStorage }];
  });
}

export function findObscuraProcesses(options = {}) {
  const output = execFileSync("ps", ["-axo", "pid=,command="], {
    encoding: "utf8",
  });
  return parseObscuraProcesses(output, options);
}
