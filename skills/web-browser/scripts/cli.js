/**
 * Small argument parser shared by the skill's commands.
 *
 * Supports `--flag`, `--opt=value`, and `--opt value`. Positional arguments and
 * everything after `--` are returned separately.
 */

const BOOLEAN_FLAGS = new Set([
  "--new",
  "--all",
  "--full-page",
  "--landscape",
  "--reset",
  "--list",
  "--follow",
  "--help",
  "--json",
  "--raw",
  "--quiet",
  "--keep-server",
  "--force",
  "--restart",
  "--reset-storage",
  "--no-broker",
  "--no-stealth",
  "--allow-private-network",
  "--obey-robots",
  "--reject",
  "--stealth",
  "--submit",
  "--js",
  "--no-scroll",
  "--keys",
  "--clear",
  "--install",
  "--attach",
]);

/**
 * @param {string[]} argv
 * @param {{booleans?: Set<string>, values?: string[]}} options
 *   `values` lists options that always consume the next argument, which matters
 *   when the value could look like a flag (e.g. --text="-1").
 */
/**
 * Options known to take a value. `--x=y` for anything outside this set, the
 * boolean set, or the caller's `values` list is reported as unknown.
 */
const VALUE_FLAGS = new Set([
  "--tab", "--timeout", "--settle", "--wait-until", "--eval", "--selector",
  "--wait", "--text", "--index", "--limit", "--path", "--device", "--scale",
  "--slow", "--from-log", "--since", "--for", "--file", "--port", "--workers",
]);

export function parseFlags(argv, options = {}) {
  const booleans = options.booleans ?? BOOLEAN_FLAGS;
  const greedy = new Set(options.values ?? []);
  const positional = [];
  const values = new Map();
  const seen = new Set();
  const unknown = [];

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--") {
      positional.push(...argv.slice(i + 1));
      break;
    }
    // A bare negative number is a value, not an option: `eval.js -1` means
    // "evaluate -1", not "unknown flag -1".
    if (!arg.startsWith("-") || arg === "-" || /^-\d+(\.\d+)?$/.test(arg) || /^-(Infinity|NaN)$/.test(arg)) {
      positional.push(arg);
      continue;
    }

    const eq = arg.indexOf("=");
    if (eq !== -1) {
      const name = arg.slice(0, eq);
      values.set(name, arg.slice(eq + 1));
      seen.add(name);
      // `--typo=1` must be reported just like `--typo`, otherwise every command
      // silently accepts misspelled options.
      if (!booleans.has(name) && !greedy.has(name) && !VALUE_FLAGS.has(name)) {
        unknown.push(arg);
      }
      continue;
    }

    if (booleans.has(arg)) {
      seen.add(arg);
      continue;
    }

    const next = argv[i + 1];
    if (next !== undefined && (greedy.has(arg) || !next.startsWith("-"))) {
      values.set(arg, next);
      seen.add(arg);
      i += 1;
      continue;
    }

    // Unknown bare option: record it so callers can report it.
    seen.add(arg);
    unknown.push(arg);
  }

  const get = (name, fallback) => {
    const raw = values.get(name);
    if (raw === undefined) return fallback;
    const num = Number(raw);
    return Number.isFinite(num) && raw.trim() !== "" ? num : raw;
  };

  return {
    positional,
    seen,
    unknown,
    has: (name) => seen.has(name),
    get,
    raw: (name) => values.get(name),
  };
}
