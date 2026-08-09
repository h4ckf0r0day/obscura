import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { extname, isAbsolute, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const require = createRequire(import.meta.url);

function normalizeNamespace(value) {
  if (value && typeof value === "object" && value.default && typeof value.default === "object") {
    return Object.assign(Object.create(null), value.default, value);
  }
  return value;
}

async function loadJavaScript(modulePath) {
  const extension = extname(modulePath);
  if (extension === ".mjs") {
    return normalizeNamespace(await import(pathToFileURL(modulePath).href));
  }

  try {
    return normalizeNamespace(require(modulePath));
  } catch (error) {
    if (error?.code !== "ERR_REQUIRE_ESM") throw error;
    return normalizeNamespace(await import(pathToFileURL(modulePath).href));
  }
}

async function loadRawWasm(modulePath) {
  const bytes = await readFile(modulePath);
  const compiled = await WebAssembly.compile(bytes);
  const imports = WebAssembly.Module.imports(compiled);
  const exports = WebAssembly.Module.exports(compiled);

  if (imports.length !== 0) {
    const importSummary = imports.map(({ module, name, kind }) => `${module}.${name}:${kind}`).join(", ");
    return {
      namespace: Object.create(null),
      wasm: {
        imports,
        exports,
        instantiated: false,
        note: `Inspection only. Point --module at the wasm-bindgen Node.js wrapper to supply imports: ${importSummary}`,
      },
    };
  }

  const instance = await WebAssembly.instantiate(compiled, Object.create(null));
  return {
    namespace: instance.exports,
    wasm: { imports, exports, instantiated: true },
  };
}

export function resolveModulePath(input, cwd = process.cwd()) {
  if (!input) return null;
  return isAbsolute(input) ? input : resolve(cwd, input);
}

export async function loadModule(input, cwd = process.cwd()) {
  const modulePath = resolveModulePath(input, cwd);
  if (!modulePath) {
    throw new Error("No WASM module supplied. Set OBSCURA_NODE_WASM or pass --module <path>");
  }

  const extension = extname(modulePath);
  const startedAt = performance.now();
  let namespace;
  let wasm = null;
  let kind;

  if (extension === ".wasm") {
    ({ namespace, wasm } = await loadRawWasm(modulePath));
    kind = "raw-wasm";
  } else {
    namespace = await loadJavaScript(modulePath);
    kind = extension === ".node" ? "native-addon" : "wasm-bindgen-wrapper";
  }

  if ((typeof namespace !== "object" && typeof namespace !== "function") || namespace === null) {
    throw new TypeError(`Module ${modulePath} did not export an object or function`);
  }

  return {
    namespace,
    metadata: {
      kind,
      modulePath,
      loadMs: performance.now() - startedAt,
      wasm,
    },
  };
}
