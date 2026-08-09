import vm from "node:vm";
import { parentPort, workerData, threadId } from "node:worker_threads";

import { loadModule } from "./module-loader.mjs";

if (!parentPort) throw new Error("The WASM V8 harness worker must run as a Worker");

const VERSION_NAMES = ["version", "getVersion", "get_version", "obscuraVersion", "obscura_version"];
const ABI_VERSION_NAMES = ["abiVersion", "abi_version"];
const PROBE_NAMES = ["probe", "selfTest", "self_test"];
const FACTORY_NAMES = [
  "createRuntime",
  "create_runtime",
  "createEngine",
  "create_engine",
  "newRuntime",
  "new_runtime",
  "createBrowser",
  "create_browser",
];
const CONSTRUCTOR_NAMES = ["EmbeddedRuntime", "Runtime", "Engine", "ObscuraRuntime", "Browser"];
const EVALUATE_NAMES = ["evaluate", "eval", "evaluateScript", "evaluate_script", "executeScript", "execute_script"];
const DISPOSE_NAMES = ["close", "dispose", "destroy", "free", "drop"];
const QUERY_TEXT_NAMES = ["query_text", "queryText"];
const QUERY_HTML_NAMES = ["query_html", "queryHtml"];
const DOCUMENT_ELEMENT_HTML_NAMES = ["document_element_html", "documentElementHtml"];
const MAX_VM_TIMEOUT_MS = 4_294_967_295;
const QUERY_BINDING = "__obscuraHostQueryElement__";
const DOCUMENT_HTML_BINDING = "__obscuraHostDocumentHtml__";
const installDocumentFacadeScript = new vm.Script(
  `(() => {
    "use strict";
    const hostQueryElement = globalThis.${QUERY_BINDING};
    const hostDocumentHtml = globalThis.${DOCUMENT_HTML_BINDING};
    const safeApply = Reflect.apply;
    const ContextError = Error;
    const ContextTypeError = TypeError;
    const ContextRangeError = RangeError;
    const ContextSyntaxError = SyntaxError;
    const ContextReferenceError = ReferenceError;
    const ContextEvalError = EvalError;
    const ContextURIError = URIError;

    const translatedError = (record) => {
      const name = typeof record?.name === "string" ? record.name : "Error";
      const message = typeof record?.message === "string" ? record.message : "Obscura bridge operation failed";
      const ErrorConstructor =
        name === "TypeError" ? ContextTypeError :
        name === "RangeError" ? ContextRangeError :
        name === "SyntaxError" ? ContextSyntaxError :
        name === "ReferenceError" ? ContextReferenceError :
        name === "EvalError" ? ContextEvalError :
        name === "URIError" ? ContextURIError : ContextError;
      const error = new ErrorConstructor(message);
      if (ErrorConstructor === ContextError && name !== "Error") error.name = name;
      return error;
    };

    const callHost = (callback, args) => {
      let response;
      try {
        response = safeApply(callback, undefined, args);
      } catch {
        throw new ContextError("Obscura bridge callback failed");
      }
      if (response === null || typeof response !== "object") {
        throw new ContextTypeError("Obscura bridge callback returned an invalid response");
      }
      if (response.ok === true) return response.value;
      if (response.ok === false) throw translatedError(response.error);
      throw new ContextTypeError("Obscura bridge callback returned an invalid response");
    };

    const querySelector = function querySelector(selector) {
      return callHost(hostQueryElement, [selector]);
    };
    Object.freeze(querySelector);

    const getDocumentOuterHtml = function getDocumentOuterHtml() {
      return callHost(hostDocumentHtml, []);
    };
    Object.freeze(getDocumentOuterHtml);

    const documentElement = Object.create(null);
    Object.defineProperty(documentElement, "outerHTML", {
      enumerable: true,
      get: getDocumentOuterHtml,
    });
    Object.freeze(documentElement);

    const document = Object.create(null);
    Object.defineProperties(document, {
      querySelector: {
        enumerable: true,
        value: querySelector,
        writable: false,
      },
      documentElement: {
        enumerable: true,
        value: documentElement,
        writable: false,
      },
    });
    Object.freeze(document);
    Object.defineProperty(globalThis, "document", {
      enumerable: true,
      value: document,
      writable: false,
      configurable: false,
    });
  })()`,
  { filename: "obscura-document-facade.js" },
);

let target;
let metadata;
let persistentRuntime;
let hostContext;
let bridgeCore;
let bridgeGeneration = 0;
let bridgeDisposed = 0;
let shuttingDown = false;

function propertyNames(value) {
  const names = new Set();
  let cursor = value;
  for (let depth = 0; cursor && cursor !== Object.prototype && depth < 3; depth += 1, cursor = Object.getPrototypeOf(cursor)) {
    for (const name of Object.getOwnPropertyNames(cursor)) {
      if (name !== "constructor") names.add(name);
    }
  }
  return [...names].sort();
}

function member(object, names) {
  for (const name of names) {
    const value = object?.[name];
    if (typeof value === "function") return { name, fn: value.bind(object) };
  }
  return null;
}

function valueMember(object, names) {
  for (const name of names) {
    const value = object?.[name];
    if (value !== undefined && typeof value !== "function") {
      return { name, value };
    }
  }
  return null;
}

function decodeJsonText(value) {
  if (typeof value !== "string") return value;
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}

function moduleResult(value) {
  // The migration's Node-API fallback deliberately crosses the isolate
  // boundary as JSON text. Ordinary JS/WASM APIs return structured values and
  // must not have legitimate strings such as "42" silently retyped.
  if (metadata?.kind === "native-addon" || typeof target?.EmbeddedRuntime === "function") {
    return decodeJsonText(value);
  }
  return value;
}

function sourceText(value, label = "source") {
  if (typeof value !== "string") throw new TypeError(`${label} must be a string`);
  return value;
}

function vmTimeout(value, fallback = 1_000) {
  const timeoutMs = value ?? fallback;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > MAX_VM_TIMEOUT_MS) {
    throw new RangeError(`timeoutMs must be an integer between 1 and ${MAX_VM_TIMEOUT_MS} milliseconds`);
  }
  return timeoutMs;
}

function optionalTimeout(value) {
  return value === undefined ? undefined : vmTimeout(value);
}

function synchronousResult(value, label) {
  if ((typeof value === "object" && value !== null) || typeof value === "function") {
    if (typeof value.then === "function") {
      throw new TypeError(`${label} returned a Promise; this operation requires a synchronous result`);
    }
  }
  return value;
}

function describeApi() {
  const version = member(target, VERSION_NAMES) ?? valueMember(target, VERSION_NAMES);
  const abiVersion = member(target, ABI_VERSION_NAMES) ?? valueMember(target, ABI_VERSION_NAMES);
  const probe = member(target, PROBE_NAMES);
  const factory = member(target, FACTORY_NAMES);
  const constructor = member(target, CONSTRUCTOR_NAMES);
  const evaluate = member(target, EVALUATE_NAMES);

  return {
    exports: propertyNames(target),
    capabilities: {
      version: version?.name ?? null,
      abiVersion: abiVersion?.name ?? null,
      probe: probe?.name ?? null,
      runtimeFactory: factory?.name ?? constructor?.name ?? null,
      moduleEvaluate: evaluate?.name ?? null,
      obscuraCore: member(target, ["ObscuraCore"])?.name ?? null,
    },
  };
}

async function createRuntime() {
  const factory = member(target, FACTORY_NAMES);
  if (factory) {
    const runtime = await factory.fn();
    if ((typeof runtime !== "object" && typeof runtime !== "function") || runtime === null) {
      throw new TypeError(`${factory.name} returned no runtime object`);
    }
    if (!member(runtime, DISPOSE_NAMES)) {
      throw new TypeError(`${factory.name} returned a runtime with no deterministic disposal method`);
    }
    return runtime;
  }

  const constructor = member(target, CONSTRUCTOR_NAMES);
  if (constructor) {
    const runtime = new constructor.fn();
    if (!member(runtime, DISPOSE_NAMES)) {
      throw new TypeError(`${constructor.name} created a runtime with no deterministic disposal method`);
    }
    return runtime;
  }

  return null;
}

async function disposeRuntime(runtime) {
  if (!runtime) return null;
  const dispose = member(runtime, DISPOSE_NAMES);
  if (!dispose) return null;
  await dispose.fn();
  return dispose.name;
}

async function evaluateWithModule(source, timeoutMs) {
  source = sourceText(source);
  timeoutMs = optionalTimeout(timeoutMs);
  const direct = member(target, EVALUATE_NAMES);
  if (direct) return moduleResult(await direct.fn(source, timeoutMs));

  if (!persistentRuntime) persistentRuntime = await createRuntime();
  if (!persistentRuntime) throw new Error("Module has neither an evaluate export nor a runtime factory");

  const evaluate = member(persistentRuntime, EVALUATE_NAMES);
  if (!evaluate) {
    const runtime = persistentRuntime;
    persistentRuntime = null;
    const message = `Created runtime has no evaluate method; found: ${propertyNames(runtime).join(", ")}`;
    try {
      await disposeRuntime(runtime);
    } catch (cleanupError) {
      throw new AggregateError([new Error(message), cleanupError], "Invalid runtime cleanup failed");
    }
    throw new Error(message);
  }
  return moduleResult(await evaluate.fn(source, timeoutMs));
}

function getHostContext() {
  if (hostContext) return hostContext;
  const sandbox = Object.create(null);
  hostContext = vm.createContext(sandbox, {
    name: "obscura-page",
    codeGeneration: { strings: true, wasm: false },
    microtaskMode: "afterEvaluate",
  });
  return hostContext;
}

function hostEvaluate(source, timeoutMs = 1_000) {
  const script = new vm.Script(sourceText(source), { filename: "obscura-evaluate.js" });
  const result = script.runInContext(getHostContext(), { timeout: vmTimeout(timeoutMs) });
  return synchronousResult(result, "hostEvaluate");
}

function syncCall(callable, ...args) {
  const value = callable.fn(...args);
  return synchronousResult(value, callable.name);
}

function bridgeApi(core = bridgeCore) {
  return {
    queryText: member(core, QUERY_TEXT_NAMES),
    queryHtml: member(core, QUERY_HTML_NAMES),
    documentElementHtml: member(core, DOCUMENT_ELEMENT_HTML_NAMES),
    dispose: member(core, DISPOSE_NAMES),
  };
}

function requireBridgeCore() {
  if (!bridgeCore) throw new Error("No ObscuraCore is loaded; supply html to bridgeEvaluate first");
  return bridgeCore;
}

function queryElement(selector) {
  requireBridgeCore();
  const api = bridgeApi();
  if (!api.queryText || !api.queryHtml) {
    throw new Error("ObscuraCore must expose query_text/queryText and query_html/queryHtml");
  }
  const outerHTML = syncCall(api.queryHtml, String(selector));
  if (outerHTML == null) return null;
  const textContent = syncCall(api.queryText, String(selector));
  if (typeof outerHTML !== "string" || (textContent != null && typeof textContent !== "string")) {
    throw new TypeError("ObscuraCore query methods must return strings or nullish values");
  }

  const element = Object.create(null);
  Object.defineProperties(element, {
    textContent: { enumerable: true, value: textContent ?? "", writable: false },
    outerHTML: { enumerable: true, value: outerHTML, writable: false },
  });
  return Object.freeze(element);
}

function documentOuterHtml() {
  requireBridgeCore();
  const html = bridgeApi().documentElementHtml;
  if (!html) {
    throw new Error(
      "ObscuraCore must expose document_element_html/documentElementHtml for document.documentElement.outerHTML",
    );
  }
  const value = syncCall(html);
  if (typeof value !== "string") {
    throw new TypeError("ObscuraCore document element serializer must return a string");
  }
  return value;
}

function bridgeErrorRecord(error) {
  let name = "Error";
  let message = "Obscura bridge operation failed";
  try {
    if (typeof error?.name === "string") name = error.name;
  } catch {
    // Keep the context-independent fallback.
  }
  try {
    if (typeof error?.message === "string") message = error.message;
    else if (error != null) message = String(error);
  } catch {
    // Keep the context-independent fallback.
  }
  const record = Object.create(null);
  Object.defineProperties(record, {
    name: { enumerable: true, value: name },
    message: { enumerable: true, value: message },
  });
  return Object.freeze(record);
}

function bridgeResponse(generation, call) {
  const response = Object.create(null);
  try {
    if (!bridgeCore || generation !== bridgeGeneration) {
      throw new Error("Stale Obscura document bridge");
    }
    Object.defineProperties(response, {
      ok: { enumerable: true, value: true },
      value: { enumerable: true, value: call() },
    });
  } catch (error) {
    Object.defineProperties(response, {
      ok: { enumerable: true, value: false },
      error: { enumerable: true, value: bridgeErrorRecord(error) },
    });
  }
  return Object.freeze(response);
}

function installDocumentFacade() {
  const context = getHostContext();
  if (Object.hasOwn(context, "document")) return;
  const generation = bridgeGeneration;
  Object.defineProperties(context, {
    [QUERY_BINDING]: {
      value: (selector) => bridgeResponse(generation, () => queryElement(selector)),
      configurable: true,
    },
    [DOCUMENT_HTML_BINDING]: {
      value: () => bridgeResponse(generation, documentOuterHtml),
      configurable: true,
    },
  });
  try {
    installDocumentFacadeScript.runInContext(context, { timeout: 1_000 });
  } catch (error) {
    hostContext = null;
    throw error;
  } finally {
    Reflect.deleteProperty(context, QUERY_BINDING);
    Reflect.deleteProperty(context, DOCUMENT_HTML_BINDING);
  }
}

async function disposeBridgeCore() {
  const core = bridgeCore;
  bridgeCore = null;
  // Loading or releasing a document is a page boundary. Discard the old V8
  // realm so globals and queued microtask state cannot leak into the next page.
  hostContext = null;
  if (!core) return null;
  const dispose = member(core, DISPOSE_NAMES);
  if (!dispose) return null;
  await dispose.fn();
  bridgeDisposed += 1;
  return dispose.name;
}

async function replaceBridgeCore(html) {
  const constructor = member(target, ["ObscuraCore"]);
  if (!constructor) {
    throw new Error("Module has no ObscuraCore constructor");
  }
  const next = new constructor.fn(sourceText(html, "html"));
  const nextApi = bridgeApi(next);
  if (!nextApi.queryText || !nextApi.queryHtml || !nextApi.documentElementHtml || !nextApi.dispose) {
    await disposeRuntime(next);
    throw new Error(
      "ObscuraCore must expose query_text/queryText, query_html/queryHtml, document_element_html/documentElementHtml, and free/dispose",
    );
  }
  try {
    await disposeBridgeCore();
  } catch (error) {
    try {
      await disposeRuntime(next);
    } catch (cleanupError) {
      throw new AggregateError([error, cleanupError], "Failed to replace and clean up ObscuraCore");
    }
    throw error;
  }
  bridgeCore = next;
  bridgeGeneration += 1;
  installDocumentFacade();
}

function bridgeStatus() {
  const api = bridgeCore ? bridgeApi() : {};
  return {
    loaded: Boolean(bridgeCore),
    generation: bridgeGeneration,
    disposed: bridgeDisposed,
    api: {
      queryText: api.queryText?.name ?? null,
      queryHtml: api.queryHtml?.name ?? null,
      documentElementHtml: api.documentElementHtml?.name ?? null,
      dispose: api.dispose?.name ?? null,
    },
  };
}

async function bridgeEvaluate({ html, source, timeoutMs = 1_000 } = {}) {
  if (html !== undefined) await replaceBridgeCore(html);
  requireBridgeCore();
  installDocumentFacade();
  return hostEvaluate(source, timeoutMs);
}

async function inspectRuntime() {
  const runtime = await createRuntime();
  if (!runtime) return null;
  try {
    return {
      members: propertyNames(runtime),
      evaluate: member(runtime, EVALUATE_NAMES)?.name ?? null,
      dispose: member(runtime, DISPOSE_NAMES)?.name ?? null,
    };
  } finally {
    await disposeRuntime(runtime);
  }
}

async function createDrop({ iterations = 1, source = "1 + 1", timeoutMs } = {}) {
  if (!Number.isSafeInteger(iterations) || iterations < 1) {
    throw new RangeError("iterations must be a positive safe integer");
  }

  source = sourceText(source);
  timeoutMs = optionalTimeout(timeoutMs);
  const api = describeApi();
  if (!api.capabilities.runtimeFactory) {
    return { skipped: true, reason: "module has no runtime factory", iterations: 0 };
  }

  const startedAt = performance.now();
  let evaluated = 0;
  let disposed = 0;
  let lastResult;
  for (let index = 0; index < iterations; index += 1) {
    const runtime = await createRuntime();
    if (!runtime) throw new Error("Runtime factory returned no value");
    try {
      const evaluate = member(runtime, EVALUATE_NAMES);
      if (!evaluate) throw new Error("Created runtime has no evaluate method");
      lastResult = moduleResult(await evaluate.fn(source, timeoutMs));
      evaluated += 1;
    } finally {
      if (await disposeRuntime(runtime)) disposed += 1;
    }
  }
  const elapsedMs = performance.now() - startedAt;

  return {
    skipped: false,
    iterations,
    evaluated,
    disposed,
    elapsedMs,
    operationsPerSecond: iterations / (elapsedMs / 1_000),
    lastResult,
  };
}

async function hostStress({ iterations = 10_000, source = "1 + 1", timeoutMs = 1_000 } = {}) {
  if (!Number.isSafeInteger(iterations) || iterations < 1) {
    throw new RangeError("iterations must be a positive safe integer");
  }
  const sandbox = Object.create(null);
  const context = vm.createContext(sandbox, {
    name: "obscura-page-stress",
    codeGeneration: { strings: true, wasm: false },
    microtaskMode: "afterEvaluate",
  });
  const script = new vm.Script(sourceText(source), { filename: "obscura-stress.js" });
  timeoutMs = vmTimeout(timeoutMs);
  const startedAt = performance.now();
  let lastResult;
  for (let index = 0; index < iterations; index += 1) {
    lastResult = synchronousResult(
      script.runInContext(context, { timeout: timeoutMs }),
      "hostStress evaluation",
    );
  }
  const elapsedMs = performance.now() - startedAt;
  return {
    iterations,
    elapsedMs,
    operationsPerSecond: iterations / (elapsedMs / 1_000),
    lastResult,
  };
}

async function shutdown() {
  shuttingDown = true;
  const errors = [];
  const runtime = persistentRuntime;
  persistentRuntime = null;
  try {
    await disposeRuntime(runtime);
  } catch (error) {
    errors.push(error);
  }
  try {
    await disposeBridgeCore();
  } catch (error) {
    errors.push(error);
  }
  hostContext = null;
  if (errors.length > 0) throw new AggregateError(errors, "Harness shutdown cleanup failed");
  return { closed: true };
}

async function dispatch(operation, payload) {
  switch (operation) {
    case "inspect":
      return { ...metadata, ...describeApi(), runtime: await inspectRuntime(), threadId };
    case "version": {
      const version = member(target, VERSION_NAMES);
      if (version) return await version.fn();
      return valueMember(target, VERSION_NAMES)?.value ?? null;
    }
    case "abiVersion": {
      const abiVersion = member(target, ABI_VERSION_NAMES);
      return abiVersion ? await abiVersion.fn() : valueMember(target, ABI_VERSION_NAMES)?.value ?? null;
    }
    case "probe": {
      const probe = member(target, PROBE_NAMES);
      return probe
        ? decodeJsonText(await probe.fn(payload?.source))
        : { available: false, reason: "module has no probe export" };
    }
    case "hostEvaluate":
      return hostEvaluate(payload?.source, payload?.timeoutMs);
    case "bridgeEvaluate":
      return await bridgeEvaluate(payload);
    case "bridgeStatus":
      return bridgeStatus();
    case "bridgeRelease":
      return { dispose: await disposeBridgeCore(), ...bridgeStatus() };
    case "moduleEvaluate":
      return await evaluateWithModule(payload?.source, payload?.timeoutMs);
    case "createDrop":
      return await createDrop(payload);
    case "hostStress":
      return hostStress(payload);
    case "shutdown":
      return await shutdown();
    default:
      throw new Error(`Unknown harness operation: ${operation}`);
  }
}

function serializeError(error) {
  let errorText = "Harness worker failed";
  try {
    if (error != null) errorText = String(error);
  } catch {
    // Keep the stable fallback for hostile thrown values.
  }
  const safeValue = (name, fallback) => {
    try {
      const value = error?.[name];
      return value == null ? fallback : String(value);
    } catch {
      return fallback;
    }
  };
  return {
    name: safeValue("name", "Error"),
    message: safeValue("message", errorText),
    stack: safeValue("stack", undefined),
    code: safeValue("code", undefined),
  };
}

function postError(id, error) {
  parentPort.postMessage({ type: "response", id, error: serializeError(error) });
}

async function handleMessage(message) {
  const { id, operation, payload } = message ?? {};
  if (!Number.isSafeInteger(id) || id < 1 || typeof operation !== "string") return;
  if (shuttingDown) {
    postError(id, new Error("Harness worker is shutting down"));
    return;
  }
  try {
    const result = await dispatch(operation, payload);
    try {
      parentPort.postMessage({ type: "response", id, result });
    } catch (error) {
      postError(id, error);
    }
  } catch (error) {
    postError(id, error);
  } finally {
    if (operation === "shutdown") setImmediate(() => parentPort.close());
  }
}

try {
  ({ namespace: target, metadata } = await loadModule(workerData.modulePath, workerData.cwd));
  // Operations mutate persistent runtime and bridge ownership state. Preserve
  // message order instead of allowing async listener invocations to overlap.
  let operationQueue = Promise.resolve();
  parentPort.on("message", (message) => {
    operationQueue = operationQueue.then(() => handleMessage(message)).catch((error) => {
      // handleMessage normally converts every failure into a response. If the
      // response channel itself fails, reset the chain and close this worker
      // instead of leaving all later queue entries attached to a rejection.
      shuttingDown = true;
      try {
        parentPort.postMessage({ type: "fatal", error: serializeError(error) });
      } catch {
        // The port is already unusable.
      }
      parentPort.close();
    });
  });
  parentPort.postMessage({
    type: "ready",
    result: { ...metadata, ...describeApi(), threadId },
  });
} catch (error) {
  parentPort.postMessage({ type: "fatal", error: serializeError(error) });
  parentPort.close();
}
