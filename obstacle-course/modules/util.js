// Statically imported ES module. Code-splitting baseline: this ships in the
// initial module graph and is resolved before the entry module runs.
export const PREFIX = 'mod';

export function sum(nums) {
  return nums.reduce(function (a, b) { return a + b; }, 0);
}

export function range(n) {
  const out = [];
  for (let i = 0; i < n; i++) out.push(i);
  return out;
}
