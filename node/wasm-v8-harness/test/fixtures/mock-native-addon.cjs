class EmbeddedRuntime {
  constructor() {
    this.closed = false;
  }

  evaluate(expression) {
    if (this.closed) throw new Error("runtime is closed");
    return JSON.stringify(Function(`"use strict"; return (${expression})`)());
  }

  close() {
    this.closed = true;
  }
}

module.exports = {
  probe(expression = "1 + 1") {
    return JSON.stringify({ ok: true, result: Function(`"use strict"; return (${expression})`)() });
  },
  EmbeddedRuntime,
};
