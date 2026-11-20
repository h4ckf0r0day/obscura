/**
 * Client for the persistent CDP broker.
 *
 * Every stateful command talks to the same broker process, because Obscura
 * scopes page targets to the WebSocket connection that created them.
 */

import { existsSync, readFileSync } from "node:fs";
import net from "node:net";
import { spawn } from "node:child_process";
import { remoteObjectValue } from "./cdp.js";
import {
  BROKER_PID_FILE,
  BROKER_SCRIPT,
  BROKER_START_TIMEOUT_MS,
  SOCKET_FILE,
  isEndpointUp,
  isProcessAlive,
  readToken,
  stealthEnabled,
} from "./obscura.js";

function socketPath() {
  return process.env.OBSCURA_BROKER_SOCKET || SOCKET_FILE;
}

export function brokerPid() {
  if (!existsSync(BROKER_PID_FILE)) return null;
  try {
    const pid = Number(readFileSync(BROKER_PID_FILE, "utf8").trim());
    return isProcessAlive(pid) ? pid : null;
  } catch {
    return null;
  }
}

export function isBrokerRunning() {
  return brokerPid() !== null && existsSync(socketPath());
}

/**
 * Send one request to the broker and resolve with its result.
 */
export function brokerRequest(request, { timeout = 40000 } = {}) {
  const path = socketPath();
  return new Promise((resolve, reject) => {
    const socket = net.connect(path);
    let buffer = "";
    let settled = false;

    const finish = (fn, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      fn(value);
    };

    const timer = setTimeout(() => {
      finish(reject, new Error(`Broker timeout after ${timeout}ms (${request.op || request.method})`));
    }, timeout);

    socket.on("connect", () => {
      socket.write(`${JSON.stringify(request)}\n`);
    });
    socket.on("data", (chunk) => {
      buffer += chunk.toString();
      const index = buffer.indexOf("\n");
      if (index === -1) return;
      const line = buffer.slice(0, index);
      let response;
      try {
        response = JSON.parse(line);
      } catch (e) {
        finish(reject, new Error(`Bad broker response: ${e.message}`));
        return;
      }
      if (response.ok) finish(resolve, response.result);
      else finish(reject, new Error(response.error));
    });
    socket.on("error", (e) => {
      if (e.code === "ENOENT" || e.code === "ECONNREFUSED") {
        finish(
          reject,
          new Error(
            "Broker is not running. Start it with start.js, or use read.js for one-shot reads.",
          ),
        );
        return;
      }
      finish(reject, e);
    });
  });
}

/**
 * Adapt the broker to the {send} shape that devices.js/emulation-state.js
 * expect. `sessionId` is ignored: the broker already targets one tab.
 */
export function sessionAdapter(tabId) {
  return {
    send: (method, params = {}, _sessionId, timeout) =>
      cdpCall(method, params, { tabId, timeout }),
  };
}

/**
 * Evaluate an expression and return its value, not the CDP envelope.
 */
export async function evaluate(expression, options = {}) {
  const result = await cdpCall(
    "Runtime.evaluate",
    { expression, returnByValue: true, awaitPromise: true },
    options,
  );
  if (result.exceptionDetails) {
    throw new Error(
      result.exceptionDetails.exception?.description ||
        result.exceptionDetails.text,
    );
  }
  // remoteObjectValue recovers NaN, -0, Infinity and BigInt from
  // `unserializableValue`; reading `.value` alone silently yields undefined.
  return remoteObjectValue(result.result);
}

export function cdpCall(method, params = {}, options = {}) {
  return brokerRequest(
    { method, params, tabId: options.tabId, timeout: options.timeout },
    { timeout: (options.timeout ?? 30000) + 5000 },
  );
}

export function listTabs() {
  return brokerRequest({ op: "tabs" }, { timeout: 5000 }).then((r) => r.tabs);
}

/**
 * Start the broker (and the browser server it attaches to) if not running.
 * Returns { started: boolean }.
 */
export async function ensureBroker({ quiet = false } = {}) {
  if (isBrokerRunning()) {
    // A live pid is not proof the socket answers; verify cheaply.
    try {
      await brokerRequest({ op: "health" }, { timeout: 2000 });
      return { started: false };
    } catch {
      // fall through to (re)start
    }
  }

  // Fail fast when there is no browser server to attach to: otherwise we would
  // wait out the whole broker start timeout to report a missing server.
  if (!(await isEndpointUp({ timeout: 1500 }))) {
    throw new Error(
      "No Obscura CDP server is running. Start it with `start.js`, or use read.js for a one-shot read.",
    );
  }

  // A stale pid file with no socket, or a live socket nobody answers on.
  const pid = brokerPid();
  if (pid && existsSync(socketPath())) {
    try {
      await brokerRequest({ op: "health" }, { timeout: 2000 });
      return { started: false };
    } catch {
      // fall through to a restart
    }
  }

  const token = readToken();
  const child = spawn(process.execPath, [BROKER_SCRIPT], {
    detached: true,
    stdio: "ignore",
    env: { ...process.env, ...(token ? { OBSCURA_CDP_TOKEN: token } : {}) },
  });

  let spawnFailed = false;
  child.on("error", () => {
    spawnFailed = true;
  });
  child.unref();

  const deadline = Date.now() + BROKER_START_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (spawnFailed) break;
    if (existsSync(socketPath())) {
      try {
        await brokerRequest({ op: "health" }, { timeout: 2000 });
        if (!quiet) process.stderr.write(`✓ broker started (pid ${brokerPid()})\n`);
        return { started: true };
      } catch {
        // Either still starting, or another broker won the lock: keep polling
        // until the deadline, then report what we know.
      }
    }
    await new Promise((r) => setTimeout(r, 100));
  }

  // A concurrent start.js may have won the race; that is not a failure.
  try {
    await brokerRequest({ op: "health" }, { timeout: 2000 });
    return { started: false };
  } catch {
    // fall through to the error below
  }

  throw new Error(
    "Broker did not start. Run `start.js --restart` to recover, or check " +
      "OBSCURA_STEALTH/storage settings and ~/.cache/agent-web/logs/serve.log.",
  );
}

export { remoteObjectValue, stealthEnabled };
