/**
 * Minimal CDP client for Obscura - no puppeteer, no hangs.
 *
 * Obscura-specific notes (measured against obscura 0.2.7):
 * - Targets are connection-scoped: a fresh WebSocket sees zero targets and
 *   tabs die with the connection. Stateful flows need broker.js, not this file.
 * - Runtime.awaitPromise is not implemented. Runtime.evaluate with
 *   awaitPromise:true does await correctly, so never call awaitPromise.
 * - Bearer auth uses OBSCURA_CDP_TOKEN; the ws package is required because the
 *   native WebSocket constructor ignores a headers option.
 */

import WebSocket from "ws";
import { getConnectionInfo } from "./obscura.js";

function exceptionMessage(result) {
  return (
    result.exceptionDetails.exception?.description || result.exceptionDetails.text
  );
}

export function remoteObjectValue(remoteObject) {
  if (!remoteObject) return undefined;
  if ("value" in remoteObject) return remoteObject.value;

  switch (remoteObject.unserializableValue) {
    case "NaN":
      return NaN;
    case "Infinity":
      return Infinity;
    case "-Infinity":
      return -Infinity;
    case "-0":
      return -0;
  }

  if (remoteObject.type === "bigint" && remoteObject.unserializableValue) {
    return BigInt(remoteObject.unserializableValue.slice(0, -1));
  }

  return undefined;
}

/**
 * Resolve the browser endpoint, starting nothing. Returns {httpUrl, wsUrl, token}.
 */
export async function resolveEndpoint({ timeout = 5000 } = {}) {
  const info = getConnectionInfo();
  const headers = info.token
    ? { Authorization: `Bearer ${info.token}` }
    : undefined;

  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), timeout);
  let resp;
  try {
    resp = await fetch(`${info.httpUrl}/json/version`, {
      signal: controller.signal,
      headers,
    });
  } catch (e) {
    clearTimeout(timeoutId);
    if (e.name === "AbortError") {
      throw new Error(
        `No CDP endpoint on ${info.httpUrl}. Run start.js first (or use read.js for one-shot reads).`,
      );
    }
    throw new Error(
      `No CDP endpoint on ${info.httpUrl} (${e.message}). Run start.js first.`,
    );
  }
  clearTimeout(timeoutId);

  if (resp.status === 401) {
    throw new Error(
      `CDP endpoint ${info.httpUrl} requires auth. Set OBSCURA_CDP_TOKEN to the token used by start.js.`,
    );
  }
  if (!resp.ok) {
    throw new Error(`CDP endpoint ${info.httpUrl} returned HTTP ${resp.status}`);
  }

  const { webSocketDebuggerUrl } = await resp.json();
  return {
    httpUrl: info.httpUrl,
    wsUrl: webSocketDebuggerUrl,
    token: info.token,
  };
}

export async function connect(timeout = 5000) {
  const endpoint = await resolveEndpoint({ timeout });
  return connectTo(endpoint.wsUrl, { token: endpoint.token, timeout });
}

export async function connectTo(wsUrl, { token, timeout = 5000 } = {}) {
  return new Promise((resolve, reject) => {
    const options = token
      ? { headers: { Authorization: `Bearer ${token}` } }
      : undefined;
    const ws = new WebSocket(wsUrl, options);

    const connectTimeout = setTimeout(() => {
      ws.terminate();
      reject(new Error("WebSocket connect timeout"));
    }, timeout);

    ws.on("open", () => {
      clearTimeout(connectTimeout);
      resolve(new CDP(ws));
    });
    ws.on("error", (e) => {
      clearTimeout(connectTimeout);
      reject(e);
    });
  });
}

export class CDP {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.callbacks = new Map();
    this.eventHandlers = new Map();

    ws.on("message", (data) => {
      let msg;
      try {
        msg = JSON.parse(data.toString());
      } catch {
        return;
      }
      if (msg.id && this.callbacks.has(msg.id)) {
        const { resolve, reject } = this.callbacks.get(msg.id);
        this.callbacks.delete(msg.id);
        if (msg.error) {
          reject(new Error(msg.error.message));
        } else {
          resolve(msg.result);
        }
        return;
      }

      if (msg.method) {
        this.emit(msg.method, msg.params || {}, msg.sessionId || null);
      }
    });

    ws.on("close", () => {
      for (const { reject } of this.callbacks.values()) {
        reject(new Error("CDP connection closed"));
      }
      this.callbacks.clear();
    });
  }

  on(method, handler) {
    if (!this.eventHandlers.has(method)) {
      this.eventHandlers.set(method, new Set());
    }
    this.eventHandlers.get(method).add(handler);
    return () => this.off(method, handler);
  }

  off(method, handler) {
    const handlers = this.eventHandlers.get(method);
    if (!handlers) return;
    handlers.delete(handler);
    if (handlers.size === 0) {
      this.eventHandlers.delete(method);
    }
  }

  emit(method, params, sessionId) {
    const handlers = this.eventHandlers.get(method);
    if (!handlers || handlers.size === 0) return;
    for (const handler of handlers) {
      try {
        handler(params, sessionId);
      } catch {
        // Ignore handler errors to keep the CDP session alive.
      }
    }
  }

  send(method, params = {}, sessionId = null, timeout = 15000) {
    return new Promise((resolve, reject) => {
      const msgId = ++this.id;
      const msg = { id: msgId, method, params };
      if (sessionId) msg.sessionId = sessionId;

      const timeoutId = setTimeout(() => {
        this.callbacks.delete(msgId);
        reject(new Error(`CDP timeout: ${method}`));
      }, timeout);

      this.callbacks.set(msgId, {
        resolve: (result) => {
          clearTimeout(timeoutId);
          resolve(result);
        },
        reject: (err) => {
          clearTimeout(timeoutId);
          reject(err);
        },
      });

      this.ws.send(JSON.stringify(msg));
    });
  }

  async getPages() {
    const { targetInfos } = await this.send("Target.getTargets");
    return targetInfos.filter((t) => t.type === "page");
  }

  async attachToPage(targetId) {
    const { sessionId } = await this.send("Target.attachToTarget", {
      targetId,
      flatten: true,
    });
    return sessionId;
  }

  /**
   * Evaluate an expression by value. Promise return values are awaited by the
   * browser via awaitPromise:true (Obscura has no Runtime.awaitPromise).
   */
  async evaluate(sessionId, expression, timeout = 30000) {
    const result = await this.send(
      "Runtime.evaluate",
      {
        expression,
        returnByValue: true,
        awaitPromise: true,
      },
      sessionId,
      timeout,
    );

    if (result.exceptionDetails) {
      throw new Error(exceptionMessage(result));
    }
    return result.result?.value;
  }

  /**
   * Console-style evaluation: statement lists, top-level return, and a
   * completion value. Uses replMode for non-promise results and falls back to
   * an async IIFE when the snippet needs top-level await.
   */
  async evaluateRepl(sessionId, expression, timeout = 30000) {
    const objectGroup = `agent-eval-${Date.now()}-${Math.random()}`;

    try {
      const result = await this.send(
        "Runtime.evaluate",
        {
          expression: `(async () => { return (${expression}); })()`,
          objectGroup,
          returnByValue: true,
          awaitPromise: true,
        },
        sessionId,
        timeout,
      );

      if (result.exceptionDetails) {
        // Statement lists (with ; or declarations) are not valid expressions.
        const fallback = await this.send(
          "Runtime.evaluate",
          {
            expression,
            objectGroup,
            returnByValue: true,
            replMode: true,
          },
          sessionId,
          timeout,
        );
        if (fallback.exceptionDetails) {
          throw new Error(exceptionMessage(fallback));
        }
        return remoteObjectValue(fallback.result);
      }

      return remoteObjectValue(result.result);
    } finally {
      await this.send(
        "Runtime.releaseObjectGroup",
        { objectGroup },
        sessionId,
        1000,
      ).catch(() => {});
    }
  }

  async screenshot(sessionId, timeout = 15000) {
    const { data } = await this.send(
      "Page.captureScreenshot",
      { format: "png" },
      sessionId,
      timeout,
    );
    return Buffer.from(data, "base64");
  }

  async navigate(sessionId, url, timeout = 30000) {
    await this.send("Page.navigate", { url }, sessionId, timeout);
  }

  async getFrameTree(sessionId) {
    const { frameTree } = await this.send("Page.getFrameTree", {}, sessionId);
    return frameTree;
  }

  async evaluateInFrame(sessionId, frameId, expression, timeout = 30000) {
    const { executionContextId } = await this.send(
      "Page.createIsolatedWorld",
      { frameId, worldName: "cdp-eval" },
      sessionId,
    );

    const result = await this.send(
      "Runtime.evaluate",
      {
        expression,
        contextId: executionContextId,
        returnByValue: true,
        awaitPromise: true,
      },
      sessionId,
      timeout,
    );

    if (result.exceptionDetails) {
      throw new Error(exceptionMessage(result));
    }
    return result.result?.value;
  }

  close() {
    try {
      this.ws.close();
    } catch {
      // ignore
    }
  }
}
