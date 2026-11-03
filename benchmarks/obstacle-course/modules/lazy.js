// Dynamically imported ES module (the code-split chunk). Only loaded via
// import() at runtime, never in the initial graph. It re-exports work that
// itself depends on the static util module to prove the loader resolves a
// nested module graph for the lazy chunk too.
import { sum } from './util.js';

export function fib(n) {
  let a = 0, b = 1;
  for (let i = 0; i < n; i++) {
    const t = a + b;
    a = b;
    b = t;
  }
  return a;
}

export function total(nums) {
  return sum(nums);
}
