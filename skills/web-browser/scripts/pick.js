#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Find elements by intent, without a visible window.
 *
 * The old pick.js required a headed browser the user could click in. Obscura's
 * serve mode is headless-only (there is no window to click), so picking by
 * mouse is not available. This replaces it with a ranked query: it scores the
 * elements on the page against the description you give and prints the best
 * matches with a selector you can feed straight into click.js.
 *
 * Scores are heuristic, not semantic embedding: exact text match, word overlap,
 * tag/role plausibility, and visibility. Every candidate is listed with its
 * score so you can see why something ranked where it did.
 */

import { ensureBroker, evaluate, sessionAdapter } from "./broker-client.js";
import { applyActiveEmulation } from "./emulation-state.js";
import { parseFlags } from "./cli.js";

const DEBUG = process.env.DEBUG === "1";
const log = DEBUG ? (...args) => console.error("[debug]", ...args) : () => {};

const { positional, get, has, unknown } = parseFlags(process.argv.slice(2), {
  values: ["--limit", "--tab"],
});

const description = positional.join(" ");

if (!description || unknown.length > 0) {
  console.log("Usage: pick.js '<what you want to click>' [--limit=<n>] [--json]");
  console.log("\nRanks page elements against your description and prints the best matches");
  console.log("with selectors to pass to click.js.");
  console.log("\nExamples:");
  console.log('  pick.js "the submit button"');
  console.log('  pick.js "login link" --limit=5');
  console.log('  pick.js "cookie accept" --json');
  console.log("\nNote: no interactive picking — Obscura serve is headless.");
  process.exit(1);
}

const limit = get("--limit", 5);

const globalTimeout = setTimeout(() => {
  console.error("✗ Global timeout exceeded (30s)");
  process.exit(1);
}, 30000);

const SCORER = `(description) => {
  const words = description.toLowerCase().split(/\\s+/).filter(w => w.length > 2);
  const STOP = new Set(["the","and","for","with","that","this","you","your","our","a","an","to","of","in","on","button","link"]);
  const keys = words.filter(w => !STOP.has(w));

  // Common web synonyms, so "login" finds "Sign in" and "signup" finds "Create account".
  const SYNONYMS = {
    login: ["log in","sign in","signin","log-in"],
    signup: ["sign up","register","create account","join"],
    logout: ["log out","sign out"],
    cart: ["basket","bag"],
    search: ["find","query"],
    submit: ["send","send button","go button","confirm","continue"],
    cancel: ["close","dismiss"],
    accept: ["agree","allow","ok","got it"],
    delete: ["remove","trash"],
  };
  const expansions = new Set();
  for (const k of keys) {
    for (const [base, alts] of Object.entries(SYNONYMS)) {
      if (k === base || k === base + "s" || alts.includes(k)) {
        expansions.add(base);
        alts.forEach(a => expansions.add(a));
      }
    }
  }

  const candidates = [...document.querySelectorAll(
    'a, button, [role=button], [role=link], input[type=submit], input[type=button], [onclick], summary, label'
  )];

  const visible = (el) => {
    const s = getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  };

  const selectorFor = (el) => {
    if (el.id && !/^[0-9]/.test(el.id)) return '#' + CSS.escape(el.id);
    const parts = [];
    let cur = el;
    let depth = 0;
    while (cur && cur.nodeType === 1 && depth < 4) {
      let part = cur.tagName.toLowerCase();
      if (cur.id && !/^[0-9]/.test(cur.id)) { parts.unshift('#' + CSS.escape(cur.id)); break; }
      const parent = cur.parentElement;
      if (parent) {
        const sameTag = [...parent.children].filter(c => c.tagName === cur.tagName);
        if (sameTag.length > 1) part += ':nth-of-type(' + (sameTag.indexOf(cur) + 1) + ')';
      }
      parts.unshift(part);
      cur = parent;
      depth++;
    }
    return parts.join(' > ');
  };

  const scored = [];
  for (const el of candidates) {
    if (!visible(el)) continue;
    const text = ((el.textContent || '') + ' ' + (el.value || '') + ' ' + (el.getAttribute('aria-label') || '') + ' ' + (el.title || '')).toLowerCase().trim();
    const href = (el.getAttribute('href') || '').toLowerCase();
    if (!text && !href) continue;
    const compact = text.replace(/\\s+/g, ' ').slice(0, 200);
    const haystack = compact + ' ' + href;

    let score = 0;
    const reasons = [];
    // exact / substring text match
    if (compact === description.toLowerCase()) { score += 100; reasons.push('exact text'); }
    else if (compact.includes(description.toLowerCase())) { score += 60; reasons.push('contains full phrase'); }
    // term overlap
    const hits = keys.filter(k => haystack.includes(k));
    if (hits.length) {
      score += hits.length * 12;
      reasons.push('terms: ' + hits.join(','));
    }
    // every key present is a strong signal
    if (keys.length > 1 && hits.length === keys.length) { score += 25; reasons.push('all terms'); }
    // synonym matches rank below literal ones
    const synHits = [...expansions].filter(s => !keys.includes(s) && haystack.includes(s));
    if (synHits.length) {
      score += synHits.length * 10;
      reasons.push('synonyms: ' + synHits.join(','));
    }
    if (href && keys.some(k => href.includes(k))) { score += 18; reasons.push('in href'); }
    // tag plausibility for "button"
    if (/button|submit/.test(description.toLowerCase())) {
      if (el.type === 'submit') { score += 30; reasons.push('is the submit control'); }
      else if (el.tagName === 'BUTTON' || el.type === 'button' || el.getAttribute('role') === 'button') { score += 15; reasons.push('is a button'); }
    }
    if (/link/.test(description.toLowerCase()) && el.tagName === 'A') { score += 15; reasons.push('is a link'); }
    // short text is more likely a control label
    if (compact.length <= 40) score += 8;
    if (score <= 0) continue;

    scored.push({
      score,
      reasons: reasons.join('; '),
      tag: el.tagName.toLowerCase(),
      text: compact.slice(0, 80),
      selector: selectorFor(el),
      href: el.href || null,
    });
  }

  scored.sort((a, b) => b.score - a.score);
  return scored.slice(0, Number(description.limit || 5));
}`;

try {
  await ensureBroker();
  log("broker ready");
  const tabId = get("--tab", undefined);
  await applyActiveEmulation(sessionAdapter(tabId), null);

  const query = `${has("--json") ? "" : ""}${description}`;
  const results = await evaluate(`(${SCORER})(${JSON.stringify(query)}).slice(0, ${Number(limit)})`, {
    tabId,
  });

  if (!results || results.length === 0) {
    console.log(`○ No element matched ${JSON.stringify(description)}`);
    console.log("  Try a shorter description, or use read.js --links / eval.js to inspect the page.");
    process.exit(0);
  }

  if (has("--json")) {
    console.log(JSON.stringify(results, null, 2));
  } else {
    console.log(`Best matches for ${JSON.stringify(description)}:`);
    for (const [i, item] of results.entries()) {
      console.log(`\n${i + 1}. [score ${item.score}] <${item.tag}> ${JSON.stringify(item.text)}`);
      console.log(`   ${item.reasons}`);
      console.log(`   selector: ${item.selector}`);
      if (item.href) console.log(`   href: ${item.href}`);
    }
    console.log(`\nClick one with: click.js '<selector>'`);
  }
} catch (e) {
  console.error("✗", e.message);
  process.exitCode = 1;
} finally {
  clearTimeout(globalTimeout);
}
