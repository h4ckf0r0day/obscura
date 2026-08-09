let nextRuntimeId = 1;

class Runtime {
  constructor() {
    this.id = nextRuntimeId++;
    this.closed = false;
  }

  evaluate(source, timeoutMs) {
    if (this.closed) throw new Error("runtime is closed");
    if (source === "__never__") return new Promise(() => {});
    if (source === "__timeout__") return timeoutMs;
    return Function(`"use strict"; return (${source})`)();
  }

  close() {
    this.closed = true;
  }
}

module.exports = {
  version: "mock-wasm-bindgen-1",
  abi_version: 1,
  probe() {
    return '{"ok":true}';
  },
  createRuntime() {
    return new Runtime();
  },
};
