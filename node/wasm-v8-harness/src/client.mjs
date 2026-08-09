import { Worker } from "node:worker_threads";

const workerUrl = new URL("./worker.mjs", import.meta.url);
const MAX_TIMER_MS = 2_147_483_647;

function validateTimeout(timeoutMs, label) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > MAX_TIMER_MS) {
    throw new RangeError(`${label} must be an integer between 1 and ${MAX_TIMER_MS} milliseconds`);
  }
  return timeoutMs;
}

function remoteError(value) {
  const error = new Error(value?.message ?? "Harness worker failed");
  error.name = value?.name ?? "Error";
  if (value?.stack) error.stack = value.stack;
  if (value?.code) error.code = value.code;
  return error;
}

export class WasmV8Worker {
  #worker;
  #pending = new Map();
  #nextId = 1;
  #closed = false;
  #closing = false;
  #closePromise;
  #terminationPromise;
  #ready;
  #readyResolve;
  #readyReject;

  constructor(modulePath, { cwd = process.cwd() } = {}) {
    this.#ready = new Promise((resolve, reject) => {
      this.#readyResolve = resolve;
      this.#readyReject = reject;
    });
    this.#worker = new Worker(workerUrl, { workerData: { modulePath, cwd } });
    this.#worker.on("message", (message) => this.#onMessage(message));
    this.#worker.on("error", (error) => this.#fail(error, true));
    this.#worker.on("exit", (code) => {
      const error =
        code !== 0
          ? new Error(`Harness worker exited with code ${code}`)
          : new Error("Harness worker has exited");
      this.#fail(error, true);
    });
  }

  static async launch(modulePath, options = {}) {
    const worker = new WasmV8Worker(modulePath, options);
    try {
      await worker.ready(options.readyTimeoutMs);
      return worker;
    } catch (error) {
      await worker.terminate();
      throw error;
    }
  }

  async ready(timeoutMs = 10_000) {
    validateTimeout(timeoutMs, "readyTimeoutMs");
    let timer;
    try {
      return await Promise.race([
        this.#ready,
        new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error(`Worker did not become ready within ${timeoutMs}ms`)), timeoutMs);
          timer.unref?.();
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
  }

  request(operation, payload, timeoutMs = 30_000) {
    return this.#request(operation, payload, timeoutMs, false);
  }

  #request(operation, payload, timeoutMs = 30_000, allowClosing = false) {
    if (this.#closed || (this.#closing && !allowClosing)) {
      return Promise.reject(new Error(this.#closed ? "Harness worker is closed" : "Harness worker is closing"));
    }
    if (typeof operation !== "string" || operation.length === 0) {
      return Promise.reject(new TypeError("Harness operation must be a non-empty string"));
    }
    try {
      validateTimeout(timeoutMs, "request timeout");
    } catch (error) {
      return Promise.reject(error);
    }
    const id = this.#nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        const error = new Error(`Harness operation ${operation} timed out after ${timeoutMs}ms`);
        reject(error);
        // A Promise or native call may still be running after the caller's
        // deadline. Kill the isolation boundary so timed-out work cannot later
        // mutate state or satisfy subsequent requests out of order.
        void this.#abort(error);
      }, timeoutMs);
      timer.unref?.();
      this.#pending.set(id, { resolve, reject, timer });
      try {
        this.#worker.postMessage({ id, operation, payload });
      } catch (error) {
        this.#pending.delete(id);
        clearTimeout(timer);
        reject(error);
      }
    });
  }

  inspect() {
    return this.request("inspect");
  }

  abiVersion() {
    return this.request("abiVersion");
  }

  hostEvaluate(source, options = {}) {
    return this.request("hostEvaluate", { source, timeoutMs: options.timeoutMs }, options.requestTimeoutMs);
  }

  moduleEvaluate(source, options = {}) {
    return this.request(
      "moduleEvaluate",
      { source, timeoutMs: options.evaluateTimeoutMs },
      options.requestTimeoutMs ?? options.timeoutMs,
    );
  }

  bridgeEvaluate(source, options = {}) {
    return this.request(
      "bridgeEvaluate",
      { source, html: options.html, timeoutMs: options.timeoutMs },
      options.requestTimeoutMs,
    );
  }

  bridgeStatus() {
    return this.request("bridgeStatus");
  }

  releaseBridge() {
    return this.request("bridgeRelease");
  }

  createDrop(iterations, options = {}) {
    return this.request(
      "createDrop",
      { iterations, source: options.source, timeoutMs: options.evaluateTimeoutMs },
      options.requestTimeoutMs ?? options.timeoutMs,
    );
  }

  async close() {
    if (this.#closePromise) return this.#closePromise;
    if (this.#closed) return;
    this.#closing = true;
    this.#closePromise = (async () => {
      try {
        await this.#request("shutdown", undefined, 5_000, true);
      } finally {
        this.#closed = true;
        try {
          await this.#worker.terminate();
        } finally {
          this.#fail(new Error("Harness worker is closed"), true);
        }
      }
    })();
    return this.#closePromise;
  }

  async terminate() {
    if (this.#closed) return;
    await this.#abort(new Error("Harness worker was terminated"));
  }

  #onMessage(message) {
    if (message.type === "ready") {
      this.#readyResolve(message.result);
      return;
    }
    if (message.type === "fatal") {
      void this.#abort(remoteError(message.error));
      return;
    }
    if (message.type !== "response") return;

    const pending = this.#pending.get(message.id);
    if (!pending) return;
    this.#pending.delete(message.id);
    clearTimeout(pending.timer);
    if (message.error) pending.reject(remoteError(message.error));
    else pending.resolve(message.result);
  }

  async #abort(error) {
    if (this.#terminationPromise) return this.#terminationPromise;
    if (this.#closed) return;
    this.#closing = true;
    this.#closed = true;
    this.#fail(error);
    this.#terminationPromise = (async () => {
      try {
        await this.#worker.terminate();
      } catch {
        // Preserve the operation error that required termination.
      }
    })();
    return this.#terminationPromise;
  }

  #fail(error, closed = false) {
    if (closed) {
      this.#closed = true;
      this.#closing = true;
    }
    this.#readyReject(error);
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.#pending.clear();
  }
}
