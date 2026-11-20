#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Stop the broker and the Obscura CDP server started by start.js.
 *
 * Safety rule: a pid recorded in a state file is not proof of identity. Before
 * signalling anything this verifies the process cmdline still looks like our
 * broker or our Obscura server, so a recycled pid or a hand-edited file cannot
 * make us kill an unrelated process.
 */

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import {
  BROKER_PID_FILE,
  STORAGE_DIR,
  clearState,
  findObscuraProcesses,
  getPort,
  isProcessAlive,
  readState,
} from "./obscura.js";

const args = process.argv.slice(2);
const allowed = new Set(["--all", "--keep-server", "--force"]);
const unknown = args.filter((a) => !allowed.has(a));
if (unknown.length > 0) {
  console.error("Usage: stop.js [--keep-server] [--all] [--force]");
  console.error("  --keep-server  Stop only the broker, leave the CDP server running");
  console.error("  --all          Stop every skill Obscura server, not just this port");
  console.error("  --force        SIGKILL instead of waiting for a clean exit");
  process.exit(1);
}

const flags = new Set(args);
const state = readState();
const port = getPort();

function cmdline(pid) {
  try {
    return readFileSync(`/proc/${pid}/cmdline`, "utf8").split("\0").join(" ");
  } catch {
    return "";
  }
}

/** True only if the pid is still the process we think it is. */
function isOurBroker(pid) {
  return isProcessAlive(pid) && /broker\.js/.test(cmdline(pid));
}

function isOurServer(pid) {
  if (!isProcessAlive(pid) || pid === process.pid) return false;
  const parts = cmdline(pid).split(" ").filter(Boolean);
  if (!/(?:^|\/)obscura(?:-worker)?$/.test(parts[0] || "")) return false;
  return /(?:^|\s)serve(?:\s|$)/.test(cmdline(pid));
}

async function waitGone(pids, attempts, delay) {
  for (let i = 0; i < attempts; i++) {
    if (pids.every((pid) => !isProcessAlive(pid))) return true;
    await new Promise((r) => setTimeout(r, delay));
  }
  return pids.every((pid) => !isProcessAlive(pid));
}

async function signal(pids, label, isOurs) {
  const live = pids.filter(isOurs);
  const rejected = pids.filter((pid) => !isOurs(pid));
  for (const pid of rejected) {
    console.error(`  skipped pid ${pid}: not a ${label} we started (stale pid file?)`);
  }
  if (live.length === 0) return false;

  for (const pid of live) {
    try {
      process.kill(pid, "SIGTERM");
    } catch {
      // already gone
    }
  }

  if (await waitGone(live, 40, 150)) {
    console.log(`✓ ${label} stopped (${live.join(", ")})`);
    return true;
  }

  if (flags.has("--force")) {
    for (const pid of live) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {
        // already gone
      }
    }
    if (await waitGone(live, 20, 100)) {
      console.log(`✓ ${label} killed (${live.join(", ")})`);
      return true;
    }
    console.error(`✗ Could not stop ${label} (${live.join(", ")})`);
    process.exitCode = 1;
    return false;
  }

  console.error(`✗ ${label} did not exit cleanly (${live.join(", ")})`);
  console.error("  Re-run with --force to SIGKILL it.");
  process.exitCode = 1;
  return false;
}

// Collect every broker pid we can prove is ours: state.json, broker.pid, and a
// cmdline scan (which also catches brokers orphaned by an interrupted start).
const brokerCandidates = new Set();
for (const source of [state?.brokerPid, readBrokerPidFile()]) {
  if (source && isProcessAlive(source)) brokerCandidates.add(source);
}
for (const pid of scanForBrokers()) brokerCandidates.add(pid);

function readBrokerPidFile() {
  if (!existsSync(BROKER_PID_FILE)) return null;
  try {
    const pid = Number(readFileSync(BROKER_PID_FILE, "utf8").trim());
    return Number.isInteger(pid) && pid > 0 ? pid : null;
  } catch {
    return null;
  }
}

/**
 * Brokers orphaned by an interrupted start never reach state.json, so scan for
 * them. Anchored on the script path being the *program* argument: a shell whose
 * command line merely mentions "broker.js" (a grep, an editor, an agent tool
 * call) must never match, or stop.js would kill its own parent.
 */
function scanForBrokers() {
  try {
    const brokerPath = new URL("./broker.js", import.meta.url).pathname;
    const out = execFileSync("ps", ["-axo", "pid=,command="], { encoding: "utf8" });
    return out
      .split("\n")
      .map((line) => line.trim().match(/^(\d+)\s+(\S+)\s+(.*)$/))
      .filter((m) => {
        if (!m) return false;
        const [, pid, argv0, rest] = m;
        if (Number(pid) === process.pid) return false;
        if (!/node$/.test(argv0)) return false;
        // The broker script must be the first argument, exactly.
        return rest.split(" ")[0] === brokerPath;
      })
      .map((m) => Number(m[1]));
  } catch {
    return [];
  }
}

await signal([...brokerCandidates], "broker", isOurBroker);

if (!flags.has("--keep-server")) {
  const serverCandidates = new Set();
  if (state?.serverPid && isProcessAlive(state.serverPid)) {
    serverCandidates.add(state.serverPid);
  }
  const scanOptions = flags.has("--all")
    ? { storageDir: STORAGE_DIR }
    : { storageDir: STORAGE_DIR, port };
  for (const proc of findObscuraProcesses(scanOptions)) {
    serverCandidates.add(proc.pid);
  }
  const stopped = await signal([...serverCandidates], "Obscura CDP server", isOurServer);
  if (!stopped && serverCandidates.size === 0) {
    console.log("✓ No skill Obscura server was running");
  }
}

clearState();
console.log("✓ State cleared; the persisted profile is preserved");

// execFileSync leaves a Socket handle open, which keeps the event loop alive and
// makes stop.js appear to hang after it has already finished. Nothing else is
// pending here, so exit explicitly with the accumulated status.
process.exit(process.exitCode ?? 0);
