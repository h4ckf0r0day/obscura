"use strict";
(function () {

// Pre-declare all internal globals as non-enumerable so they are invisible
// to Object.keys(window) / for-in enumeration. Must run before any var
// declarations or property assignments below: once a property is defined
// with enumerable:false here, subsequent `var x = value` assignments will
// find the property already exists and only update the value, leaving the
// descriptor intact. Direct globalThis.x = value assignments also only
// update the value without touching enumerable when the property is
// writable:true and configurable:true.
(function _preHideInternals() {
  var _names = [
    // runtime-set by Rust (runtime.rs / page.rs)
    '__obscura_errors', '__obscura_init', '__obscura_hide_list',
    '__obscura_objects', '__obscura_oid', '__obscura_ua',
    '__obscura_platform', '__obscura_ua_platform', '__obscura_ua_platform_version',
    '__obscura_stealth', '__obscura_markTrusted', '__obscura_core_handoff',
    '__obscura_frameId', '__obscura_parentFrameId', '__obscura_frameWindows',
    '__obscura_frameObjects', '__obscura_deliverMessage',
    '__obscura_frameElements', '__obscura_liveFrameIds', '__obscura_forgetFrame',
    '__obscura_registerLinkedStylesheet',
    '__markParserScripts', '__obscura_hasPendingDynamicScripts',
    '__obscura_hasPendingLoadDelayingScripts',
    '__obscura_nextPendingTimeoutDelay',
    '__obscura_hw', '__obscura_mem',
    '__documentReadyState__', '__currentUrl',
    // internal helpers (var-declared throughout the file)
    '__processDynScriptQueue', '_decodeDataScriptUrl', '_markNative', '_fpRand', '_fpNoise',
    '_fpCache', '_getFp', '_fp', '_splitAsciiWhitespace',
    '_getElementsByClassName', '_docEncoding', '_docIsUtf8',
    '_isSpecialScheme', '_applyDocQueryEncoding', '_anchorBase',
    '_elemHrefURL', '_setElemHrefPart', '_pad', '_daysInMonth',
    '_isoWeek1Monday', '_inputParseNumber', '_inputFormatNumber',
    '_htmlAttrName', '_convertNodes', '_fragmentContextPayload', '_parseHTMLFragment', '_xmlWellFormed', '_elementClassFor', '_wrap', '_wrapEl',
    '_resolveUrl', '_registerIframe', '_base64ToUint8Array',
    '_bodyToUint8Array', '_arrayBufferFromBytes',
    '_installWasmStreamingFallback', '_urlParseOp', '_urlSetOp',
    '_urlResolveOp', '_decodeBodyWithCharset', '_utf8DecodeBytes',
    '_selectionFor', '_isConstructorCE', '_isValidCustomElementName', '_shadowRootForHost',
    '_blobPartToBytes', '_bytesToBinaryString', '_formEncode', '_hexv',
    '_commonFonts', '_isXMLDocument', '_isValidPITarget', '_isHTMLEl',
    '_nodeList', '_rngNodeLength', '_rngNodeIndex', '_rngSame', '_rngRoot',
    '_rngAncestors', '_rngOrder', '_rngCmp', '_rngCheckOffset',
    '_idbRequest', '_idbObjectStore', '_idbTransaction', '_idbDatabase',
    '_makeListenerBox',
    // WebIDL interfaces. A real browser exposes these on the global as
    // enumerable:false; here they were assigned with `globalThis.X = X`, which
    // defaults to enumerable:true and is detectable in one line:
    //   Object.getOwnPropertyDescriptor(window, 'Node').enumerable
    // Pre-declaring them non-enumerable here is enough -- per the note above,
    // the later `globalThis.X = X` assignments only update the value.
    'Node', 'Element', 'Document', 'DocumentFragment', 'DocumentType',
    'Animation', 'KeyframeEffect', 'DocumentTimeline',
    'Text', 'Comment', 'CDATASection', 'ProcessingInstruction', 'CharacterData',
    'CSSStyleDeclaration', 'DOMTokenList', 'NamedNodeMap', 'Screen', 'NetworkInformation',
    'MessageChannel', 'MessagePort', 'BroadcastChannel', 'CustomElementRegistry',
    'Scheduler',
    'XMLHttpRequestEventTarget', 'HTMLMediaElement', 'HTMLVideoElement',
    'HTMLAudioElement', 'WebGL2RenderingContext',
    'SVGElement', 'SVGGraphicsElement', 'SVGGeometryElement', 'SVGPathElement',
    'SVGSVGElement',
  ];
  var _desc = { value: undefined, writable: true, enumerable: false, configurable: true };
  for (var _i = 0; _i < _names.length; _i++) {
    try { Object.defineProperty(globalThis, _names[_i], _desc); } catch (_e) {}
  }
})();

// Handoff for child frame realms. deno_core binds ops into the main context
// only, so a realm restored from the snapshot arrives with its own empty
// `Deno.core.ops`. The host reads this to take the main realm's bound op table
// and to find each new realm's own table to fill, then deletes the global in
// the same step, so page script never sees it (see runtime.rs
// `take_ops_handoff` / `share_ops_with_realm`).
globalThis.__obscura_core_handoff = Deno.core;

globalThis.__obscura_errors = [];

globalThis.addEventListener = globalThis.addEventListener || function(){};
globalThis.onunhandledrejection = function(e) { if (e?.preventDefault) e.preventDefault(); };

globalThis.onerror = function(msg, src, line, col, error) {
  globalThis.__obscura_errors.push({msg: String(msg), src: String(src||""), line, error: String(error||"")});
};
globalThis.__windowListeners = {};
globalThis.addEventListener = function(type, fn) {
  if (!globalThis.__windowListeners[type]) globalThis.__windowListeners[type] = [];
  globalThis.__windowListeners[type].push(fn);
};
globalThis.removeEventListener = function(type, fn) {
  if (globalThis.__windowListeners[type]) {
    globalThis.__windowListeners[type] = globalThis.__windowListeners[type].filter(h => h !== fn);
  }
};
globalThis.dispatchEvent = function(event) {
  if (!event) return true;
  const handlers = globalThis.__windowListeners[event.type] || [];
  for (const h of handlers) { try { h.call(globalThis, event); } catch(e) { console.error(e); } }
  return !event.defaultPrevented;
};

let _domMutationEpoch = 0;
let _treeMutationEpoch = 0;
const _DOM_MUTATION_COMMANDS = new Set([
  "append_child", "insert_before", "remove_child",
  "set_attribute", "remove_attribute",
  "set_text_content", "set_inner_html", "set_inner_html_context",
  "set_fragment_html_executable",
]);
const _DOM_TREE_MUTATION_COMMANDS = new Set([
  "append_child", "insert_before", "remove_child",
  "set_inner_html", "set_inner_html_context", "set_fragment_html_executable",
]);
// Which realm this bootstrap closure belongs to. Every wrapper's methods come
// from its own realm's prototypes, so a DOM call names the document it belongs
// to instead of letting the host guess from whoever is calling. That is what
// makes `iframe.contentDocument.title` read the frame's document rather than
// the caller's. Set by __obscura_init; 0 is the page.
let _realmFrameId = 0;

const _dom = (cmd, a1, a2) => {
  const result = Deno.core.ops.op_dom(cmd, String(a1 ?? ""), String(a2 ?? ""), _realmFrameId);
  if (_DOM_MUTATION_COMMANDS.has(cmd)) {
    _domMutationEpoch++;
    // Resize observation is tied to rendering-invalidating DOM work. The
    // hook is installed later in bootstrap, before page script can run.
    if (typeof globalThis.__obscura_recompute_resizes === "function") {
      globalThis.__obscura_recompute_resizes();
    }
    // Intersection geometry is invalidated synchronously as well. Deferring
    // this solely through MutationObserver misses the IO phase of the current
    // rendering opportunity when an rAF callback changes layout.
    if (typeof globalThis.__obscura_recompute_intersections === "function") {
      globalThis.__obscura_recompute_intersections();
    }
  }
  // Native mutation ops report their verified postcondition. Only a real tree
  // change invalidates ancestry caches; rejected cycles and invalid roots must
  // not make JS believe a move happened.
  if (result === "true" && _DOM_TREE_MUTATION_COMMANDS.has(cmd)) {
    _treeMutationEpoch++;
  }
  return result;
};

const _nativeFns = new Set();
// Exact toString override for members whose native form is not just
// `function <name>()`, e.g. accessors (`function get x() { [native code] }`)
// or functions whose `.name` does not match the real builtin.
const _nativeStr = new Map();
const _origToString = Function.prototype.toString;
Function.prototype.toString = function toString() {
  if (_nativeStr.has(this)) { return _nativeStr.get(this); }
  if (_nativeFns.has(this)) {
    return `function ${this.name || ''}() { [native code] }`;
  }
  return _origToString.call(this);
};
function _markNative(fn) { if (typeof fn === 'function') _nativeFns.add(fn); return fn; }
// Mark a function with an exact native-code toString (used for accessors).
function _markNativeAs(fn, str) { if (typeof fn === 'function') _nativeStr.set(fn, str); return fn; }
_nativeFns.add(Function.prototype.toString);

// unusualWindowProperties: obscura's internal globals are made non-enumerable
// (see _preHideInternals and __obscura_init), which hides them from
// Object.keys / for-in. But fingerprinting scripts enumerate the global object
// with Object.getOwnPropertyNames and Reflect.ownKeys, which return
// non-enumerable properties too, so the internals still leak (pixelscan's
// unusualWindowProperties check). Filter the engine's own globals out of the
// reflection APIs when they target the global object. The canonical name set is
// __obscura_hide_list, precomputed at snapshot-build time; referencing it lazily
// means the list is already populated by the time any page calls these.
(function _hideInternalsFromReflection() {
  var _cache = null, _cacheLen = -1;
  function _set() {
    var list = globalThis.__obscura_hide_list;
    if (!list) { return null; }
    if (_cache && _cacheLen === list.length) { return _cache; }
    _cache = new Set(list);
    _cache.add('__obscura_hide_list');
    _cacheLen = list.length;
    return _cache;
  }
  function _isGlobal(t) { return t === globalThis; }
  function _filter(t, names) {
    if (!_isGlobal(t)) { return names; }
    var set = _set();
    if (!set) { return names; }
    var out = [];
    for (var i = 0; i < names.length; i++) { if (!set.has(names[i])) { out.push(names[i]); } }
    return out;
  }
  var _oGOPN = Object.getOwnPropertyNames;
  var _oOwnKeys = Reflect.ownKeys;
  var _oKeys = Object.keys;
  var _oGOPDs = Object.getOwnPropertyDescriptors;
  function define(obj, prop, impl) {
    try { Object.defineProperty(obj, prop, { value: _markNative(impl), writable: true, enumerable: false, configurable: true }); } catch (e) {}
  }
  define(Object, 'getOwnPropertyNames', function getOwnPropertyNames(t) { return _filter(t, _oGOPN(t)); });
  define(Reflect, 'ownKeys', function ownKeys(t) { return _filter(t, _oOwnKeys(t)); });
  define(Object, 'keys', function keys(t) { return _filter(t, _oKeys(t)); });
  define(Object, 'getOwnPropertyDescriptors', function getOwnPropertyDescriptors(t) {
    var all = _oGOPDs(t);
    if (_isGlobal(t)) {
      var set = _set();
      if (set) { var ks = _oGOPN(all); for (var i = 0; i < ks.length; i++) { if (set.has(ks[i])) { delete all[ks[i]]; } } }
    }
    return all;
  });
})();

[Error, TypeError, ReferenceError, SyntaxError, RangeError, URIError, EvalError].forEach(E => {
  try {
    Object.defineProperty(E.prototype, 'name', {
      value: E.name, writable: true, enumerable: false, configurable: false,
    });
  } catch(e) {}
});

const _stackCache = new WeakMap();
const _origStackDesc = Object.getOwnPropertyDescriptor(Error.prototype, 'stack');
if (_origStackDesc && _origStackDesc.get) {
  Object.defineProperty(Error.prototype, 'stack', {
    configurable: false, enumerable: false,
    get: function() {
      if (!_stackCache.has(this)) _stackCache.set(this, _origStackDesc.get.call(this));
      return _stackCache.get(this);
    }
  });
}

let _fpSeed = 0;
// Dynamic module/in-order script queue. Module evaluation remains serialized
// to prevent a re-entrant RefCell panic in deno_core's
// futures_unordered_driver when SPAs insert multiple <script type=module>
// elements at once. Ordinary dynamically inserted classic scripts are async
// by default, so their fetches run independently and execute when ready just
// like browser ScriptRunner tasks; serializing those fetches made unrelated
// analytics/widgets form one long load-blocking waterfall.
let __dynScriptQueue = [];
let __dynScriptBusy = false;
let __dynClassicPending = 0;
let __dynLoadDelayingPending = 0;
Object.defineProperty(globalThis, '__obscura_hasPendingDynamicScripts', {
  value: function() {
    return __dynClassicPending > 0 || __dynScriptBusy || __dynScriptQueue.length > 0;
  },
  writable: false,
  enumerable: false,
  configurable: false,
});
// HTML tracks scripts which delay the document load event separately from
// arbitrary asynchronous script work. A connected external script prepared
// before `load` joins that set until its load/error processing finishes;
// dynamic import() and scripts created by a load handler are post-load work.
// Keep this bridge hidden for the same reason as the general queue status.
Object.defineProperty(globalThis, '__obscura_hasPendingLoadDelayingScripts', {
  value: function() { return __dynLoadDelayingPending > 0; },
  writable: false,
  enumerable: false,
  configurable: false,
});
function _decodeDataScriptUrl(url) {
  const comma = url.indexOf(',');
  if (!url.startsWith('data:') || comma < 5) {
    throw new TypeError('Invalid dynamic script data URL');
  }

  const meta = url.slice(5, comma);
  const fragment = url.indexOf('#', comma + 1);
  const payload = url.slice(comma + 1, fragment < 0 ? url.length : fragment);
  if (meta.split(';').some(part => part.toLowerCase() === 'base64')) {
    let encoded = payload.replace(/[\r\n\t\f ]/g, '');
    const remainder = encoded.length % 4;
    if (remainder === 1 || !/^[A-Za-z0-9+/]*={0,2}$/.test(encoded) || /=/.test(encoded.slice(0, -2))) {
      throw new TypeError('Invalid dynamic script data URL base64');
    }
    if (remainder > 0) encoded += '='.repeat(4 - remainder);
    if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)) {
      throw new TypeError('Invalid dynamic script data URL base64');
    }
    return new TextDecoder().decode(_base64ToUint8Array(encoded));
  }

  const bytes = [];
  for (let i = 0; i < payload.length; i++) {
    const code = payload.charCodeAt(i);
    if (code === 0x25 && i + 2 < payload.length) {
      const hi = _hexv(payload.charCodeAt(i + 1));
      const lo = _hexv(payload.charCodeAt(i + 2));
      if (hi >= 0 && lo >= 0) {
        bytes.push(hi * 16 + lo);
        i += 2;
        continue;
      }
    }
    if (code < 0x80) {
      bytes.push(code);
    } else {
      const character = String.fromCodePoint(payload.codePointAt(i));
      if (character.length === 2) i++;
      const encoded = new TextEncoder().encode(character);
      for (let j = 0; j < encoded.length; j++) bytes.push(encoded[j]);
    }
  }
  return new TextDecoder().decode(new Uint8Array(bytes));
}
// A script element executes at most once.  The authoritative flag lives in
// native per-document state so it survives wrapper churn, fragment parsing,
// moves, and cloneNode().
globalThis.__markParserScripts = function(nids) {
  for (const nid of nids || []) Deno.core.ops.op_script_mark_started(+nid);
};
async function __fetchDynClassicScript(task) {
  let body;
  if (task.url.startsWith('data:')) {
    body = _decodeDataScriptUrl(task.url);
  } else {
    const raw = await Deno.core.ops.op_fetch_url(
      task.url, "GET", "{}", "", task.pageOrigin, "no-cors", "same-origin"
    );
    const parsed = JSON.parse(raw);
    // The HTML script-fetch algorithm treats an unsuccessful HTTP response
    // as a network error. Evaluating its response body is both observably
    // unlike browsers and dangerous: JSON error payloads and diagnostic HTML
    // must never become script source.
    if (!(parsed.status >= 200 && parsed.status <= 299)) {
      throw new Error('HTTP ' + (parsed.status || 0));
    }
    body = parsed.body;
  }
  return body;
}
function __startDynClassicFetch(task) {
  // Attach both reactions immediately. An in-order script may finish fetching
  // before an earlier queue member; retaining a settled value avoids an
  // unhandled-rejection report while its execution turn is still blocked.
  task.fetchResult = __fetchDynClassicScript(task).then(
    body => ({ body }),
    error => ({ error }),
  );
}
async function __runDynScriptTask(task) {
  try {
    if (task.isModule) {
      await import(task.url);
    } else {
      if (!task.fetchResult) __startDynClassicFetch(task);
      const fetched = await task.fetchResult;
      if (fetched.error) throw fetched.error;
      const body = fetched.body;
      if (body) {
        // A fetched async script is executed by a ScriptRunner task, not by
        // the fetch promise's microtask continuation. Besides matching event
        // loop ordering, this prevents a batch of concurrently completed
        // third-party scripts from being charged to (and pinning) whichever
        // parser script happened to trigger the microtask checkpoint.
        await new Promise(resolve => {
          const execute = () => {
            globalThis.__currentScriptNid = task.nid;
            try { (0, eval)(body); }
            catch(e) { console.error('Dynamic script error (' + task.url + '):', e.message); }
            finally { globalThis.__currentScriptNid = task.prevNid || 0; }
            resolve();
          };
          if (_scheduleAfter(0, execute) === undefined) execute();
        });
      }
    }
    // Fire load via dispatchEvent only: it invokes the element's onload
    // property handler and any addEventListener('load') listeners, read live
    // off the element. Calling onload separately would double-fire it.
    try { task.dispatchEvent(new Event('load')); } catch(e) {}
  } catch(e) {
    console.error('Dynamic script fetch error:', e.message);
    try { task.dispatchEvent(new Event('error')); } catch(ex) {}
  } finally {
    if (task.delaysLoad) {
      task.delaysLoad = false;
      __dynLoadDelayingPending = Math.max(0, __dynLoadDelayingPending - 1);
    }
  }
}
async function __runAsyncClassicScript(task) {
  __dynClassicPending++;
  try {
    await __runDynScriptTask(task);
  } finally {
    __dynClassicPending--;
  }
}
async function __processDynScriptQueue() {
  if (__dynScriptBusy) return;
  __dynScriptBusy = true;
  // try/finally so the busy flag is always cleared even if a task throws
  // outside its own guard; otherwise the queue would wedge and silently
  // block every later module or explicitly in-order script on the page.
  try {
    while (__dynScriptQueue.length > 0) {
      await __runDynScriptTask(__dynScriptQueue.shift());
    }
  } finally {
    __dynScriptBusy = false;
  }
}
// Resolve a resource URL (script src / link href) against <base href> or the
// document URL, the way the inline dynamic-script path does. Guarded so a bad
// base or href never throws into appendChild.
function _resolveResourceUrl(src) {
  let baseHref = null;
  try {
    const baseEl = globalThis.document?.querySelector('base[href]');
    baseHref = baseEl ? baseEl.getAttribute('href') : null;
  } catch(e) { baseHref = null; }
  const docUrl = globalThis.location?.href || 'http://localhost/';
  let baseUrl;
  try { baseUrl = baseHref ? new URL(baseHref, docUrl).href : docUrl; }
  catch(e) { baseUrl = docUrl; }
  try {
    return src.startsWith('http') || src.startsWith('data:')
      ? src
      : new URL(src, baseUrl).href;
  } catch(e) { return src; }
}

const _linkedStylesheetNodes = new WeakMap();
const _linkElementSheets = new WeakMap();

function _linkedStylesheetHref(link, explicitHref) {
  const raw = explicitHref || link?.getAttribute?.("href") || link?.href || "";
  return raw ? _resolveResourceUrl(String(raw)) : "";
}

function _linkedStylesheetIsOriginClean(href) {
  try {
    const documentUrl = new URL(globalThis.document?.URL || globalThis.location?.href || "about:blank");
    const stylesheetUrl = new URL(href, documentUrl.href);
    return stylesheetUrl.origin === documentUrl.origin;
  } catch(e) {
    // An unresolved relative URL in an about:blank-style synthetic document
    // has no distinct remote origin and is safe to expose.
    return !/^[a-z][a-z0-9+.-]*:/i.test(String(href || ""));
  }
}

function _registerLinkedStylesheet(link, sourceNode, explicitHref) {
  if (!link || !sourceNode) return null;
  const href = _linkedStylesheetHref(link, explicitHref);
  _linkedStylesheetNodes.set(link, sourceNode);
  let sheet = _linkElementSheets.get(link);
  if (!sheet) {
    sheet = new CSSStyleSheet();
    _linkElementSheets.set(link, sheet);
  }
  sheet._bindLinkedOwner(link, sourceNode, href, _linkedStylesheetIsOriginClean(href));
  return sheet;
}
globalThis.__obscura_registerLinkedStylesheet = _registerLinkedStylesheet;

// A fetched sheet becomes an inline <style>, so relative url() references
// must keep resolving against the stylesheet URL rather than document.URL.
// Scan instead of using a regexp: data URLs and quoted URLs can contain
// parentheses, quotes, and whitespace.
function _rebaseCssUrls(css, baseUrl) {
  let out = "";
  let i = 0;
  let quote = "";
  let comment = false;
  while (i < css.length) {
    if (comment) {
      if (css[i] === "*" && css[i + 1] === "/") {
        out += "*/"; i += 2; comment = false;
      } else {
        out += css[i++];
      }
      continue;
    }
    if (quote) {
      const ch = css[i++];
      out += ch;
      if (ch === "\\" && i < css.length) out += css[i++];
      else if (ch === quote) quote = "";
      continue;
    }
    if (css[i] === "/" && css[i + 1] === "*") {
      out += "/*"; i += 2; comment = true; continue;
    }
    if (css[i] === '"' || css[i] === "'") {
      quote = css[i]; out += css[i++]; continue;
    }
    if (css.slice(i, i + 4).toLowerCase() !== "url(") {
      out += css[i++]; continue;
    }
    let end = i + 4;
    let innerQuote = "";
    while (end < css.length) {
      const ch = css[end];
      if (innerQuote) {
        if (ch === "\\") { end += 2; continue; }
        if (ch === innerQuote) innerQuote = "";
      } else if (ch === '"' || ch === "'") {
        innerQuote = ch;
      } else if (ch === ")") {
        break;
      }
      end++;
    }
    if (end >= css.length) {
      out += css.slice(i);
      break;
    }
    const raw = css.slice(i + 4, end).trim();
    const value = raw.length >= 2
      && ((raw[0] === '"' && raw[raw.length - 1] === '"')
        || (raw[0] === "'" && raw[raw.length - 1] === "'"))
      ? raw.slice(1, -1)
      : raw;
    let resolved = value;
    if (value && !/^(?:[a-z][a-z0-9+.-]*:|\/\/|#)/i.test(value)) {
      try { resolved = new URL(value, baseUrl).href; } catch(e) {}
    }
    out += `url("${resolved.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}")`;
    i = end + 1;
  }
  return out;
}

function _cssImportApplies(media) {
  const compact = media.replace(/\s+/g, "").toLowerCase();
  if (!compact) return true;
  if (compact.includes("prefers-color-scheme:dark")) return false;
  if (compact.includes("print")
    && !compact.includes("screen")
    && !compact.includes("all")) return false;
  if (compact.includes("min-width") || compact.includes("max-width")
      || compact.includes("prefers-")) {
    try { return matchMedia(media).matches; } catch(e) {}
  }
  return true;
}

async function _fetchLinkedCss(url, pageOrigin, depth = 0, seen = new Set()) {
  if (depth > 4 || seen.has(url)) return "";
  seen.add(url);
  const raw = await Deno.core.ops.op_fetch_url(
    url, "GET", "{}", "", pageOrigin, "no-cors", "same-origin"
  );
  const parsed = JSON.parse(raw);
  if (parsed.blocked || parsed.status >= 400 || parsed.status === 0) {
    throw new Error("Stylesheet fetch failed: " + url);
  }
  let css = parsed.body || "";
  const imports = [];
  // @import is only valid before ordinary rules. Removing it here lets the
  // renderer consume the imported rules from the materialized <style>.
  css = css.replace(
    /@import\s+(?:url\(\s*)?(?:"([^"]+)"|'([^']+)'|([^'"\s;)]+))\s*\)?\s*([^;]*);/gi,
    (statement, doubleQuoted, singleQuoted, bare, media) => {
      const target = doubleQuoted || singleQuoted || bare || "";
      if (_cssImportApplies(media || "")) {
        try {
          imports.push(new URL(target, url).href);
        } catch(e) {}
      }
      return "";
    }
  );
  const imported = await Promise.all(imports.map(importUrl =>
    _fetchLinkedCss(importUrl, pageOrigin, depth + 1, new Set(seen))
  ));
  imported.push(_rebaseCssUrls(css, url));
  return imported.filter(Boolean).join("\n");
}

// A dynamically-inserted <link rel="stylesheet" href> must fetch, enter the
// live cascade, and then fire load. Framework route chunks commonly await this
// event before revealing their content; firing it while discarding the CSS
// left the DOM loaded but unstyled. Issue #409.
async function _loadLinkedStylesheet(c) {
  // obscura does not yet reflect the `rel` IDL attribute back to the content
  // attribute, so `link.rel = "stylesheet"` leaves getAttribute('rel') null.
  // Read both so the property-assignment form (the common framework pattern)
  // and the parsed-from-HTML form are both recognized.
  const rel = (c.getAttribute('rel') || c.rel || '').toString().toLowerCase();
  if (!rel.split(/\s+/).includes('stylesheet')) return;
  const href = c.getAttribute('href');
  if (!href) return;
  const fullUrl = _resolveResourceUrl(href);
  let pageOrigin = "";
  try { pageOrigin = new URL(fullUrl).origin; } catch(e) {}
  try {
    const css = await _fetchLinkedCss(fullUrl, pageOrigin);
    const previous = _linkedStylesheetNodes.get(c);
    if (previous?.parentNode) previous.parentNode.removeChild(previous);
    const media = c.getAttribute("media") || "";
    const style = document.createElement("style");
    style.setAttribute("data-obscura-linked", fullUrl);
    style.textContent = css;
    _registerLinkedStylesheet(c, style, fullUrl);
    if (c.parentNode && !c.disabled && _cssImportApplies(media)) {
      c.parentNode.insertBefore(style, c.nextSibling);
    }
    try { c.dispatchEvent(new Event('load', { bubbles: true })); } catch(e) {}
  } catch(e) {
    try { c.dispatchEvent(new Event('error', { bubbles: true })); } catch(e) {}
  }
}

function _fpRand(salt) {
  let h = (_fpSeed ^ (salt || 0)) | 0;
  h = Math.imul(h ^ (h >>> 16), 0x45d9f3b);
  h = Math.imul(h ^ (h >>> 13), 0x45d9f3b);
  return ((h ^ (h >>> 16)) >>> 0) / 0xFFFFFFFF;
}
function _fpNoise(x, y, channel) {
  return (_fpRand(x * 7919 + y * 6271 + channel * 8923) - 0.5) * 4;
}

var _fpCache = null;
function _getFp() {
  if (_fpCache) return _fpCache;
  const _uaPlat = globalThis.__obscura_ua_platform || 'Windows';
  const isMac = _uaPlat === 'macOS';
  const isLinux = _uaPlat === 'Linux';
  const gpuPool = isMac ? [
    'ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)',
    'ANGLE (Apple, ANGLE Metal Renderer: Apple M1 Pro, Unspecified Version)',
    'ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)',
    'ANGLE (Apple, ANGLE Metal Renderer: Apple M2 Pro, Unspecified Version)',
    'ANGLE (Apple, ANGLE Metal Renderer: Apple M3, Unspecified Version)',
    'ANGLE (Intel Inc., ANGLE Metal Renderer: Intel(R) Iris(TM) Plus Graphics, Unspecified Version)',
  ] : isLinux ? [
    'ANGLE (Intel, Mesa Intel(R) UHD Graphics 630 (CFL GT2), OpenGL 4.6)',
    'ANGLE (Intel, Mesa Intel(R) Iris(R) Xe Graphics (TGL GT2), OpenGL 4.6)',
    'ANGLE (Intel, Mesa Intel(R) UHD Graphics 770 (RPL-S), OpenGL 4.6)',
    'ANGLE (AMD, AMD Radeon RX 580 (polaris10, LLVM 15.0.7, DRM 3.54, LLVM 15.0.7), OpenGL 4.6)',
    'ANGLE (AMD, AMD Radeon RX 6700 XT (navi22, LLVM 16.0.6, DRM 3.54, LLVM 16.0.6), OpenGL 4.6)',
    'ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 OpenGL 4.6)',
    'ANGLE (NVIDIA, NVIDIA GeForce RTX 4070 OpenGL 4.6)',
  ] : [
    'ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)',
    'ANGLE (NVIDIA, NVIDIA GeForce GTX 1660 SUPER Direct3D11 vs_5_0 ps_5_0, D3D11)',
    'ANGLE (NVIDIA, NVIDIA GeForce RTX 2070 SUPER Direct3D11 vs_5_0 ps_5_0, D3D11)',
    'ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0, D3D11)',
    'ANGLE (Intel, Intel(R) Iris(R) Xe Graphics Direct3D11 vs_5_0 ps_5_0, D3D11)',
    'ANGLE (AMD, AMD Radeon RX 580 Direct3D11 vs_5_0 ps_5_0, D3D11)',
    'ANGLE (AMD, AMD Radeon RX 6700 XT Direct3D11 vs_5_0 ps_5_0, D3D11)',
    'ANGLE (NVIDIA, NVIDIA GeForce RTX 4070 Direct3D11 vs_5_0 ps_5_0, D3D11)',
    'ANGLE (NVIDIA, NVIDIA GeForce GTX 1080 Ti Direct3D11 vs_5_0 ps_5_0, D3D11)',
    'ANGLE (Intel, Intel(R) UHD Graphics 770 Direct3D11 vs_5_0 ps_5_0, D3D11)',
    'ANGLE (AMD, AMD Radeon RX 5700 XT Direct3D11 vs_5_0 ps_5_0, D3D11)',
    'ANGLE (NVIDIA, NVIDIA GeForce RTX 3080 Direct3D11 vs_5_0 ps_5_0, D3D11)',
  ];
  const gpuVendorPool = isMac ? [
    'Google Inc. (Apple)','Google Inc. (Apple)','Google Inc. (Apple)',
    'Google Inc. (Apple)','Google Inc. (Apple)',
    'Google Inc. (Intel Inc.)',
  ] : isLinux ? [
    'Google Inc. (Intel)','Google Inc. (Intel)','Google Inc. (Intel)',
    'Google Inc. (AMD)','Google Inc. (AMD)',
    'Google Inc. (NVIDIA)','Google Inc. (NVIDIA)',
  ] : [
    'Google Inc. (NVIDIA)','Google Inc. (NVIDIA)','Google Inc. (NVIDIA)',
    'Google Inc. (Intel)','Google Inc. (Intel)',
    'Google Inc. (AMD)','Google Inc. (AMD)',
    'Google Inc. (NVIDIA)','Google Inc. (NVIDIA)',
    'Google Inc. (Intel)','Google Inc. (AMD)','Google Inc. (NVIDIA)',
  ];
  const idx = Math.floor(_fpRand(42) * gpuPool.length);
  const screenPool = [[1920,1080],[2560,1440],[1366,768],[1536,864],[1440,900],[1680,1050],[1280,720],[3840,2160]];
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  let cfp = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUg';
  for (let i = 0; i < 40; i++) cfp += chars[Math.floor(_fpRand(500 + i) * 64)];
  cfp += '==';
  _fpCache = {
    gpu: gpuPool[idx], gpuVendor: gpuVendorPool[idx],
    audioBaseLatency: 0.002 + _fpRand(100) * 0.008,
    audioSampleRate: [44100, 48000][Math.floor(_fpRand(101) * 2)],
    compThreshold: -24 + (_fpRand(102) - 0.5) * 4,
    compKnee: 30 + (_fpRand(103) - 0.5) * 4,
    compRatio: 12 + (_fpRand(104) - 0.5) * 4,
    batteryLevel: 0.5 + _fpRand(200) * 0.5,
    batteryCharging: _fpRand(201) > 0.3,
    screen: screenPool[Math.floor(_fpRand(300) * screenPool.length)],
    canvasFingerprint: cfp,
  };
  return _fpCache;
}
function _fp(key) { return _getFp()[key]; }
globalThis._eventRegistry = globalThis._eventRegistry || {};
globalThis._formValues = globalThis._formValues || {};
globalThis._formChecked = globalThis._formChecked || {};
const _eventRegistry = globalThis._eventRegistry;
const _formValues = globalThis._formValues;
const _formChecked = globalThis._formChecked;
const _domParse = (cmd, a1, a2) => { try { return JSON.parse(_dom(cmd, a1, a2)); } catch { return null; } };

// HTML "ASCII whitespace": U+0009 TAB, U+000A LF, U+000C FF, U+000D CR, U+0020 SPACE.
// Class token splitting (classList, getElementsByClassName) uses exactly this set.
// JS \s is wider (U+000B, U+00A0, U+2028, etc.), so it must not be used here.
const _ASCII_WS = /[ \t\n\f\r]+/;
function _splitAsciiWhitespace(s) {
  // WebIDL DOMString coercion: null -> "null", undefined -> "undefined".
  return String(s).split(_ASCII_WS).filter(Boolean);
}
// Shared getElementsByClassName: split the argument into an ordered set of
// tokens on ASCII whitespace, then return descendants (in tree order) whose
// class attribute contains every token, as an HTMLCollection (so namedItem and
// named access work on the result). `root` must expose querySelectorAll.
function _getElementsByClassName(root, classNames) {
  const tokens = _splitAsciiWhitespace(classNames);
  if (tokens.length === 0) return HTMLCollection._from([]);
  // Fast path: a single CSS-identifier token goes straight to the native
  // selector engine (the common case). Only multi-token sets or exotic class
  // names (NBSP, leading digits, etc.) fall back to the O(n) JS scan below.
  if (tokens.length === 1 && /^[A-Za-z_-][\w-]*$/.test(tokens[0])) {
    return HTMLCollection._from(root.querySelectorAll("." + tokens[0]));
  }
  const all = root.querySelectorAll("*");
  const matched = [];
  for (let i = 0; i < all.length; i++) {
    const el = all[i];
    const elTokens = _splitAsciiWhitespace(el.getAttribute ? (el.getAttribute("class") || "") : "");
    let ok = true;
    for (let t = 0; t < tokens.length; t++) {
      if (elTokens.indexOf(tokens[t]) < 0) { ok = false; break; }
    }
    if (ok) matched.push(el);
  }
  return HTMLCollection._from(matched);
}
const _consoleFn = (level, args) => {
  try { Deno.core.ops.op_console_msg(level, args.map(a => {
    if (a === null) return "null";
    if (a === undefined) return "undefined";
    if (a instanceof Error) {
      const _pst = Error.prepareStackTrace;
      if (_pst !== undefined) Error.prepareStackTrace = undefined;
      const _s = a.stack || a.message || String(a);
      if (_pst !== undefined) Error.prepareStackTrace = _pst;
      return _s;
    }
    if (typeof a === "object") {
      try {
        const s = JSON.stringify(a);
        return s === "{}" && a.message ? a.message : s;
      } catch { return String(a); }
    }
    return String(a);
  }).join(" ")); } catch {}
};

globalThis.console = {
  log: (...a) => _consoleFn("log", a), warn: (...a) => _consoleFn("warn", a),
  error: (...a) => _consoleFn("error", a), info: (...a) => _consoleFn("log", a),
  debug: () => {}, dir: () => {}, trace: () => {}, table: () => {}, group: () => {},
  groupEnd: () => {}, groupCollapsed: () => {}, time: () => {}, timeEnd: () => {},
  timeLog: () => {}, count: () => {}, countReset: () => {}, clear: () => {},
  assert: (c, ...a) => { if (!c) _consoleFn("error", ["Assertion failed:", ...a]); },
};

let _tid = 0;
const _clearedTimers = new Set();
const _intervals = new Set();
const _nativeTimerIds = new Map();
const __obscuraPendingTimeoutDeadlines = new Map();
Object.defineProperty(globalThis, '__obscura_nextPendingTimeoutDelay', {
  value: function() {
    const now = performance.now();
    let nearest = Infinity;
    for (const deadline of __obscuraPendingTimeoutDeadlines.values()) {
      nearest = Math.min(nearest, Math.max(0, deadline - now));
    }
    return Number.isFinite(nearest) ? nearest : -1;
  },
  writable: false,
  enumerable: false,
  configurable: false,
});

let _frameTimerSeq = 0;
const _cancelledFrameTimers = new Set();

const _scheduleAfter = (delay, fn) => {
  const d = Math.max(0, Number(delay) || 0);
  // HTML timers queue tasks even when their delay is zero. Treating a
  // zero-delay timer as a Promise reaction turns recursive framework
  // schedulers into an unbounded microtask checkpoint: timers and networking
  // never regain control and V8 can burn seconds before navigation completes.
  // deno_core's timer queue requires a Tokio reactor even to enqueue. Some
  // low-level embedders intentionally do a synchronous geometry mutation and
  // capture without pumping an event loop. Such a host cannot observe queued
  // tasks, so leave them pending instead of aborting or incorrectly turning a
  // task into a microtask. Normal browser and CDP execution always takes the
  // task-queue path below.
  if (!Deno.core.ops.op_async_runtime_available()) {
    return undefined;
  }
  // A child frame realm cannot use deno_core's timer queue: op_timer_queue
  // reads per-context state that only a deno_core-created context carries, and
  // a realm restored from the snapshot has none, so queueing from a frame
  // dereferences uninitialized memory. A host sleep does the same job, and
  // because its continuation is an ordinary microtask, V8 reports the frame as
  // the microtask context and the ops the callback makes still resolve against
  // the frame's own document. Frame timer ids are negative so clearTimeout can
  // tell the two queues apart.
  if (globalThis.__obscura_frameId) {
    const frameTimerId = -(++_frameTimerSeq);
    Deno.core.ops.op_sleep(d).then(() => {
      if (_cancelledFrameTimers.delete(frameTimerId)) return;
      Deno.core.ops.op_begin_render_task?.();
      fn();
    });
    return frameTimerId;
  }
  // The callback runs only when the embedder pumps the event loop, after the
  // current microtask checkpoint.
  return Deno.core.queueUserTimer(0, false, d, () => {
    // HTML timer/observer/rAF delivery starts a new task. Freeze animation
    // time lazily on that task's first style/layout read so a callback that
    // waited in the host queue samples its actual delivery instant.
    Deno.core.ops.op_begin_render_task?.();
    return fn();
  });
};

const _cancelScheduled = (nativeId) => {
  if (nativeId < 0) _cancelledFrameTimers.add(nativeId);
  else Deno.core.cancelTimer(nativeId);
};

// Timers accept a string first arg per the HTML spec (e.g. the Aliyun WAF
// `acw_sc__v2` challenge drives `setTimeout('reload(arg2)', 2)`). A string is
// compiled and run in global scope, identical to a real browser; otherwise the
// call silently no-ops and JS-triggered navigations (cookie → reload) never fire.
const _coerceTimerFn = (fn) => {
  if (typeof fn === "string") {
    // Per HTML, a string handler is compiled and run as a classic script in
    // global scope *at fire time*. Indirect eval ((0, eval)) runs in the true
    // global scope, so top-level var/function declarations become globals (a
    // `new Function(fn)` wrapper kept them local); deferring to fire time also
    // surfaces a SyntaxError when the timer elapses, matching a real browser,
    // instead of swallowing it eagerly at scheduling. The dynamic-script path
    // uses the same indirect eval for the same reason.
    const src = fn;
    return () => { (0, eval)(src); };
  }
  return typeof fn === "function" ? fn : null;
};

globalThis.setTimeout = (fn, delay = 0, ...args) => {
  const f = _coerceTimerFn(fn);
  if (f === null) return ++_tid;
  const id = ++_tid;
  const normalizedDelay = Math.max(0, Number(delay) || 0);
  const nativeId = _scheduleAfter(normalizedDelay, () => {
    _nativeTimerIds.delete(id);
    __obscuraPendingTimeoutDeadlines.delete(id);
    if (_clearedTimers.has(id)) return;
    try { f(...args); } catch(e) { console.error("Timer error:", e); }
  });
  if (nativeId !== undefined) {
    _nativeTimerIds.set(id, nativeId);
    __obscuraPendingTimeoutDeadlines.set(id, performance.now() + normalizedDelay);
  }
  return id;
};

globalThis.clearTimeout = (id) => {
  _clearedTimers.add(id);
  __obscuraPendingTimeoutDeadlines.delete(id);
  const nativeId = _nativeTimerIds.get(id);
  if (nativeId !== undefined) {
    _cancelScheduled(nativeId);
    _nativeTimerIds.delete(id);
  }
};

globalThis.setInterval = (fn, delay = 0, ...args) => {
  const f = _coerceTimerFn(fn);
  if (f === null) return ++_tid;
  const id = ++_tid;
  _intervals.add(id);
  const tick = () => {
    if (!_intervals.has(id)) return;
    try { f(...args); } catch(e) { console.error("Interval error:", e); }
    if (!_intervals.has(id)) return;
    const nativeId = _scheduleAfter(delay, tick);
    if (nativeId !== undefined) _nativeTimerIds.set(id, nativeId);
  };
  const nativeId = _scheduleAfter(delay, tick);
  if (nativeId !== undefined) _nativeTimerIds.set(id, nativeId);
  return id;
};

globalThis.clearInterval = (id) => {
  _intervals.delete(id);
  globalThis.clearTimeout(id);
};

// Animation callbacks are a rendering-phase batch, not zero-delay
// microtasks.  In particular, a callback which queues itself must yield to
// timers, networking, and the embedder between frames.  The old setTimeout(0)
// alias eventually used Promise.resolve(), so a normal animation loop formed
// an unbounded microtask chain and pinned V8 until the watchdog terminated it.
const _RAF_FRAME_DELAY_MS = 16;
let _rafPending = new Map();
let _rafCurrentBatch = null;
let _rafFrameScheduled = false;
let _rafRunningFrame = false;
let _renderOpportunityScheduled = false;
let _renderOpportunityRunning = false;

function _renderOpportunityHasWork() {
  return _rafFrameScheduled || _resizeRenderCheckpointPending
    || _intersectionRenderCheckpointPending;
}

// Gecko and the HTML rendering algorithm use one refresh opportunity for
// every rendering phase. Keeping rAF, ResizeObserver, and
// IntersectionObserver on independent 16ms timers triples host wakeups and
// lets registration order change which geometry a callback sees. Run the
// phases once, in browser order, from one task instead:
//
//   animation frame callbacks -> layout/ResizeObserver -> intersections
//
// A phase which queues more work while this task is running belongs to the
// next opportunity unless a later phase in this opportunity can consume it.
function _scheduleRenderingOpportunity() {
  if (_renderOpportunityScheduled || _renderOpportunityRunning
      || !_renderOpportunityHasWork()) return;
  _renderOpportunityScheduled = true;
  _scheduleAfter(_RAF_FRAME_DELAY_MS, _runRenderingOpportunity);
}

function _runRenderingOpportunity() {
  _renderOpportunityScheduled = false;
  _renderOpportunityRunning = true;
  try {
    if (_rafFrameScheduled) _runAnimationFrameBatch();
    if (_resizeRenderCheckpointPending) _runResizeRenderCheckpoint();
    if (_intersectionRenderCheckpointPending) _runIntersectionRenderCheckpoint();
  } finally {
    _renderOpportunityRunning = false;
    _scheduleRenderingOpportunity();
  }
}

function _scheduleAnimationFrame() {
  if (_rafFrameScheduled || _rafRunningFrame || _rafPending.size === 0) return;
  _rafFrameScheduled = true;
  _scheduleRenderingOpportunity();
}

function _runAnimationFrameBatch() {
  _rafFrameScheduled = false;
  if (_rafPending.size === 0) return;

  // Swap before invoking anything. A callback requested while this batch is
  // running therefore belongs to the next frame. Every callback in this
  // batch receives the same rendering timestamp.
  const batch = _rafPending;
  _rafPending = new Map();
  _rafCurrentBatch = batch;
  _rafRunningFrame = true;
  const timestamp = performance.now();
  try {
    for (const [id, callback] of batch) {
      // cancelAnimationFrame() may remove a later callback while an earlier
      // callback in the same frame is running.
      if (!batch.has(id)) continue;
      batch.delete(id);
      try { callback(timestamp); }
      catch (e) { console.error("Animation frame error:", e); }
    }
  } finally {
    _rafRunningFrame = false;
    _rafCurrentBatch = null;
    _scheduleAnimationFrame();
  }
}

globalThis.requestAnimationFrame = (fn) => {
  if (typeof fn !== "function") {
    throw new TypeError(
      "Failed to execute 'requestAnimationFrame' on 'Window': parameter 1 is not of type 'Function'."
    );
  }
  const id = ++_tid;
  _rafPending.set(id, fn);
  _scheduleAnimationFrame();
  return id;
};

globalThis.cancelAnimationFrame = (id) => {
  _rafPending.delete(id);
  if (_rafCurrentBatch) _rafCurrentBatch.delete(id);
};
globalThis.queueMicrotask = globalThis.queueMicrotask || ((fn) => Promise.resolve().then(fn));

// Browser posted tasks need an event-loop boundary but no clock delay. Tokio's
// timer wheel imposes roughly a one-millisecond floor even for delay zero,
// which turns MessageChannel and scheduler chains into artificial latency.
// Keep one shared priority/FIFO queue in JavaScript and use a yield-only async
// op solely to wake one delivery. Scheduling the next wake after the callback
// gives V8 a microtask checkpoint between every pair of posted tasks.
const _browserPostedTaskQueues = Array.from({ length: 6 }, () => []);
let _browserPostedTaskWakePending = false;

function _browserPostedTaskScheduleWake() {
  if (_browserPostedTaskWakePending) return;
  if (!Deno.core.ops.op_async_runtime_available()) return;
  _browserPostedTaskWakePending = true;
  Deno.core.ops.op_posted_task().then(
    _browserPostedTaskRunOne,
    () => {
      _browserPostedTaskWakePending = false;
      if (_browserPostedTaskQueues.some(queue => queue.length)) {
        _scheduleAfter(0, _browserPostedTaskRunOne);
      }
    },
  );
}

function _browserPostedTaskEnqueue(callback, priority) {
  _browserPostedTaskQueues[priority].push(callback);
  _browserPostedTaskScheduleWake();
}

function _browserPostedTaskRunOne() {
  _browserPostedTaskWakePending = false;
  let callback = null;
  for (let priority = _browserPostedTaskQueues.length - 1; priority >= 0; priority--) {
    const queue = _browserPostedTaskQueues[priority];
    if (queue.length) {
      callback = queue.shift();
      break;
    }
  }
  if (!callback) return;

  Deno.core.ops.op_begin_render_task?.();
  try { callback(); }
  catch (error) { console.error("Posted task error:", error); }
  finally {
    if (_browserPostedTaskQueues.some(queue => queue.length)) {
      _browserPostedTaskScheduleWake();
    }
  }
}

// Prioritized Task Scheduling. A scheduler task is a real event-loop task,
// ordered strictly by effective priority and FIFO within one priority. Yield
// continuations rank immediately above ordinary tasks of the same priority.
// This keeps background prefetch work behind visible hydration while still
// giving every callback its own microtask checkpoint.
const _schedulerConstructionKey = {};
const _schedulerInstances = new WeakSet();
const _schedulerPriorityRank = {
  "background": 0,
  "user-visible": 1,
  "user-blocking": 2,
};
let _schedulerCurrentState = null;

function _schedulerRemoveAbort(task) {
  if (task.signal && task.abortHandler) {
    task.signal.removeEventListener("abort", task.abortHandler);
    task.abortHandler = null;
  }
}

function _schedulerEnqueue(task, continuation) {
  if (task.canceled) return;
  const effectivePriority = _schedulerPriorityRank[task.priority] * 2
    + (continuation ? 1 : 0);
  _browserPostedTaskEnqueue(() => _schedulerRunTask(task), effectivePriority);
}

function _schedulerRunTask(task) {
  if (task.canceled) return;

  task.started = true;
  const previousState = _schedulerCurrentState;
  _schedulerCurrentState = task.state;
  try {
    if (task.callback === null) {
      task.resolve(undefined);
    } else {
      const callback = task.callback;
      task.resolve(callback());
    }
  } catch (error) {
    task.reject(error);
  } finally {
    _schedulerCurrentState = previousState;
    task.completed = true;
    _schedulerRemoveAbort(task);
  }
}

function _schedulerNormalizeOptions(options) {
  const dictionary = options == null ? {} : Object(options);

  let delay = 0;
  const rawDelay = dictionary.delay;
  if (rawDelay !== undefined) {
    if (typeof rawDelay === "bigint") {
      throw new TypeError("Failed to read the 'delay' property from 'SchedulerPostTaskOptions': Value is not of type 'unsigned long long'.");
    }
    delay = Number(rawDelay);
    if (!Number.isFinite(delay) || delay < 0 || delay >= 18446744073709551616) {
      throw new TypeError("Failed to read the 'delay' property from 'SchedulerPostTaskOptions': Value is outside the 'unsigned long long' value range.");
    }
    delay = Math.trunc(delay);
  }

  let priority = "user-visible";
  const rawPriority = dictionary.priority;
  if (rawPriority !== undefined) {
    priority = String(rawPriority);
    if (!Object.prototype.hasOwnProperty.call(_schedulerPriorityRank, priority)) {
      throw new TypeError("The provided value '" + priority + "' is not a valid enum value of type TaskPriority.");
    }
  }

  const signal = dictionary.signal;
  if (signal !== undefined && !(signal instanceof globalThis.AbortSignal)) {
    throw new TypeError("Failed to read the 'signal' property from 'SchedulerPostTaskOptions': Failed to convert value to 'AbortSignal'.");
  }
  return { delay, priority, signal: signal === undefined ? null : signal };
}

function _schedulerCreateTask(callback, state, resolve, reject) {
  const task = {
    callback, state, resolve, reject,
    priority: state.priority,
    signal: state.signal,
    abortHandler: null,
    delayTimerId: null,
    canceled: false,
    started: false,
    completed: false,
  };
  if (task.signal) {
    task.abortHandler = () => {
      if (task.completed || task.canceled) return;
      task.canceled = true;
      if (task.delayTimerId !== null) clearTimeout(task.delayTimerId);
      _schedulerRemoveAbort(task);
      reject(task.signal.reason);
    };
    task.signal.addEventListener("abort", task.abortHandler);
  }
  return task;
}

globalThis.Scheduler = class Scheduler {
  constructor(key) {
    if (key !== _schedulerConstructionKey) {
      throw new TypeError("Failed to construct 'Scheduler': Illegal constructor");
    }
    _schedulerInstances.add(this);
  }

  postTask(callback, options = {}) {
    return new Promise((resolve, reject) => {
      if (!_schedulerInstances.has(this)) throw new TypeError("Illegal invocation");
      if (typeof callback !== "function") {
        throw new TypeError("Failed to execute 'postTask' on 'Scheduler': parameter 1 is not of type 'Function'.");
      }
      const normalized = _schedulerNormalizeOptions(options);
      if (normalized.signal && normalized.signal.aborted) {
        reject(normalized.signal.reason);
        return;
      }
      const state = { priority: normalized.priority, signal: normalized.signal };
      const task = _schedulerCreateTask(callback, state, resolve, reject);
      if (normalized.delay > 0) {
        task.delayTimerId = setTimeout(() => {
          task.delayTimerId = null;
          _schedulerEnqueue(task, false);
        }, normalized.delay);
      } else {
        _schedulerEnqueue(task, false);
      }
    });
  }

  yield() {
    return new Promise((resolve, reject) => {
      if (!_schedulerInstances.has(this)) throw new TypeError("Illegal invocation");
      const inherited = _schedulerCurrentState;
      const state = inherited
        ? { priority: inherited.priority, signal: inherited.signal }
        : { priority: "user-visible", signal: null };
      if (state.signal && state.signal.aborted) {
        reject(state.signal.reason);
        return;
      }
      _schedulerEnqueue(_schedulerCreateTask(null, state, resolve, reject), true);
    });
  }
};
Object.defineProperty(globalThis.Scheduler.prototype, Symbol.toStringTag, {
  value: "Scheduler",
  configurable: true,
});
_markNative(globalThis.Scheduler);
_markNative(globalThis.Scheduler.prototype.postTask);
_markNative(globalThis.Scheduler.prototype.yield);

const _defaultScheduler = new globalThis.Scheduler(_schedulerConstructionKey);
Object.defineProperty(globalThis, "scheduler", {
  get() { return _defaultScheduler; },
  set(value) {
    Object.defineProperty(globalThis, "scheduler", {
      value, writable: true, enumerable: true, configurable: true,
    });
  },
  enumerable: true,
  configurable: true,
});

// MessagePort is a task-backed EventTarget, not a pair of callback slots.
// React currently uses `onmessage`, while Angular/Zone.js and worker-style
// schedulers commonly use addEventListener + start and inspect the prototype.
// Keep stopped-port messages queued, clone payloads synchronously, and deliver
// one message per task so every delivery gets its own microtask checkpoint.
const _messagePortConstructionKey = {};
const _messagePortState = new WeakMap();
function _messagePortStateFor(port) {
  const state = _messagePortState.get(port);
  if (!state) throw new TypeError("Illegal invocation");
  return state;
}
function _messagePortInstallEventHandler(port, type, callback) {
  const state = _messagePortStateFor(port);
  const slot = type === "message" ? "onmessage" : "onmessageerror";
  const wrapperSlot = type === "message" ? "messageHandlerWrapper" : "messageErrorHandlerWrapper";
  const oldCallback = state[slot];
  state[slot] = callback;

  // Event-handler IDL attributes participate in the same listener list as
  // addEventListener. Install their stable wrapper when the slot first becomes
  // non-null so mixed registrations run in registration order. Reassigning a
  // live handler keeps its position; clearing and setting it again appends it.
  if (callback && !oldCallback) {
    const wrapper = (event) => {
      const current = _messagePortState.get(port)?.[slot];
      if (!current) return;
      if (typeof current === "function") current.call(port, event);
      else current.handleEvent.call(current, event);
    };
    state[wrapperSlot] = wrapper;
    _eventTargetAdd(port, type, wrapper);
  } else if (!callback && oldCallback) {
    _eventTargetRemove(port, type, state[wrapperSlot]);
    state[wrapperSlot] = null;
  }
}
function _messagePortScheduleDelivery(port) {
  const state = _messagePortStateFor(port);
  if (state.closed || !state.messageQueueEnabled || state.messageDeliveryPending || !state.messageQueue.length) return;
  state.messageDeliveryPending = true;
  // User-visible ordinary rank. Scheduler continuations at the same priority
  // remain immediately above this task; FIFO holds across all ordinary tasks.
  _browserPostedTaskEnqueue(() => {
    const current = _messagePortState.get(port);
    if (!current) return;
    current.messageDeliveryPending = false;
    if (current.closed || !current.messageQueueEnabled || !current.messageQueue.length) return;
    const data = current.messageQueue.shift();
    const event = new MessageEvent("message", {
      data,
      origin: "",
      lastEventId: "",
      source: null,
      ports: [],
    });
    _eventTargetDispatch(port, event);
    _messagePortScheduleDelivery(port);
  }, _schedulerPriorityRank["user-visible"] * 2);
}
class MessagePort {
  constructor(key) {
    if (key !== _messagePortConstructionKey) throw new TypeError("Illegal constructor");
    _messagePortState.set(this, {
      entangled: null,
      messageQueue: [],
      messageQueueEnabled: false,
      messageDeliveryPending: false,
      closed: false,
      onmessage: null,
      onmessageerror: null,
      messageHandlerWrapper: null,
      messageErrorHandlerWrapper: null,
    });
  }
  postMessage(message, options) {
    // Structured serialization happens before inspecting the entanglement.
    // This preserves the browser-observable DataCloneError on closed ports and
    // prevents mutations after postMessage from changing the delivered value.
    let cloned;
    try {
      cloned = globalThis.structuredClone(message, options);
    } catch (error) {
      throw error;
    }
    const state = _messagePortStateFor(this);
    const target = state.entangled;
    const targetState = target && _messagePortState.get(target);
    if (state.closed || !targetState || targetState.closed) return;
    targetState.messageQueue.push(cloned);
    _messagePortScheduleDelivery(target);
  }
  start() {
    const state = _messagePortStateFor(this);
    if (state.messageQueueEnabled || state.closed) return;
    state.messageQueueEnabled = true;
    _messagePortScheduleDelivery(this);
  }
  close() {
    const state = _messagePortStateFor(this);
    if (state.closed) return;
    state.closed = true;
    state.messageQueue.length = 0;
    state.messageQueueEnabled = false;
    const peer = state.entangled;
    state.entangled = null;
    const peerState = peer && _messagePortState.get(peer);
    if (peerState?.entangled === this) peerState.entangled = null;
    // A previously scheduled task cannot be removed from the shared task
    // source, but it observes `closed` and therefore cannot dispatch.
  }
  addEventListener(type, callback, options) {
    _eventTargetAdd(this, type, callback, options);
  }
  removeEventListener(type, callback, options) {
    _eventTargetRemove(this, type, callback, options);
  }
  dispatchEvent(event) {
    _messagePortStateFor(this);
    return _eventTargetDispatch(this, event);
  }
  get onmessage() { return _messagePortStateFor(this).onmessage; }
  set onmessage(callback) {
    callback = typeof callback === "function"
      || (callback && typeof callback.handleEvent === "function")
      ? callback : null;
    _messagePortInstallEventHandler(this, "message", callback);
    // Setting the event-handler IDL attribute implicitly starts the port,
    // including when the assigned value is null.
    this.start();
  }
  get onmessageerror() { return _messagePortStateFor(this).onmessageerror; }
  set onmessageerror(callback) {
    callback = typeof callback === "function"
      || (callback && typeof callback.handleEvent === "function")
      ? callback : null;
    _messagePortInstallEventHandler(this, "messageerror", callback);
  }
  get [Symbol.toStringTag]() { return "MessagePort"; }
}

class MessageChannel {
  constructor() {
    this.port1 = new MessagePort(_messagePortConstructionKey);
    this.port2 = new MessagePort(_messagePortConstructionKey);
    _messagePortStateFor(this.port1).entangled = this.port2;
    _messagePortStateFor(this.port2).entangled = this.port1;
  }
}
globalThis.MessageChannel = MessageChannel;
globalThis.MessagePort = MessagePort;

const _cssCamelToKebab = (s) => s.replace(/[A-Z]/g, (m) => "-" + m.toLowerCase());
const _cssKebabToCamel = (s) => s.replace(/-([a-z])/g, (_, c) => c.toUpperCase());

// Standard CSS property names (camelCase). Real CSSStyleDeclaration exposes every
// property as an enumerable accessor, so feature-detection code (`'gap' in
// el.style`) and enumeration (`Object.keys(el.style)`) see the whole set, not
// just the ones that happen to be assigned (issue #356).
const _CSS_PROPERTY_NAMES = [
  "accentColor","alignContent","alignItems","alignSelf","all","animation","animationDelay",
  "animationDirection","animationDuration","animationFillMode","animationIterationCount",
  "animationName","animationPlayState","animationTimingFunction","appearance","aspectRatio",
  "backdropFilter","backfaceVisibility","background","backgroundAttachment","backgroundBlendMode",
  "backgroundClip","backgroundColor","backgroundImage","backgroundOrigin","backgroundPosition",
  "backgroundPositionX","backgroundPositionY","backgroundRepeat","backgroundSize","blockSize",
  "border","borderBlock","borderBlockColor","borderBlockEnd","borderBlockEndColor","borderBlockEndStyle",
  "borderBlockEndWidth","borderBlockStart","borderBlockStartColor","borderBlockStartStyle",
  "borderBlockStartWidth","borderBlockStyle","borderBlockWidth","borderBottom","borderBottomColor",
  "borderBottomLeftRadius","borderBottomRightRadius","borderBottomStyle","borderBottomWidth",
  "borderCollapse","borderColor","borderImage","borderImageOutset","borderImageRepeat",
  "borderImageSlice","borderImageSource","borderImageWidth","borderInline","borderInlineColor",
  "borderInlineEnd","borderInlineEndColor","borderInlineEndStyle","borderInlineEndWidth",
  "borderInlineStart","borderInlineStartColor","borderInlineStartStyle","borderInlineStartWidth",
  "borderInlineStyle","borderInlineWidth","borderLeft","borderLeftColor","borderLeftStyle",
  "borderLeftWidth","borderRadius","borderRight","borderRightColor","borderRightStyle",
  "borderRightWidth","borderSpacing","borderStyle","borderTop","borderTopColor","borderTopLeftRadius",
  "borderTopRightRadius","borderTopStyle","borderTopWidth","borderWidth","bottom","boxShadow",
  "boxSizing","breakAfter","breakBefore","breakInside","captionSide","caretColor","clear","clip",
  "clipPath","color","colorScheme","columnCount","columnFill","columnGap","columnRule","columnRuleColor",
  "columnRuleStyle","columnRuleWidth","columnSpan","columnWidth","columns","contain","container",
  "containerName","containerType","content","counterIncrement","counterReset","counterSet","cssFloat",
  "cursor","direction","display","emptyCells","filter","flex","flexBasis","flexDirection","flexFlow",
  "flexGrow","flexShrink","flexWrap","float","font","fontFamily","fontFeatureSettings","fontKerning",
  "fontOpticalSizing","fontSize","fontSizeAdjust","fontStretch","fontStyle","fontVariant",
  "fontVariantCaps","fontVariantLigatures","fontVariantNumeric","fontWeight","gap","grid","gridArea",
  "gridAutoColumns","gridAutoFlow","gridAutoRows","gridColumn","gridColumnEnd","gridColumnGap",
  "gridColumnStart","gridGap","gridRow","gridRowEnd","gridRowGap","gridRowStart","gridTemplate",
  "gridTemplateAreas","gridTemplateColumns","gridTemplateRows","height","hyphens","imageRendering",
  "inlineSize","inset","insetBlock","insetBlockEnd","insetBlockStart","insetInline","insetInlineEnd",
  "insetInlineStart","isolation","justifyContent","justifyItems","justifySelf","left","letterSpacing",
  "lineBreak","lineHeight","listStyle","listStyleImage","listStylePosition","listStyleType","margin",
  "marginBlock","marginBlockEnd","marginBlockStart","marginBottom","marginInline","marginInlineEnd",
  "marginInlineStart","marginLeft","marginRight","marginTop","mask","maxBlockSize","maxHeight",
  "maxInlineSize","maxWidth","minBlockSize","minHeight","minInlineSize","minWidth","mixBlendMode",
  "objectFit","objectPosition","offset","opacity","order","outline","outlineColor","outlineOffset",
  "outlineStyle","outlineWidth","overflow","overflowAnchor","overflowWrap","overflowX","overflowY",
  "overscrollBehavior","overscrollBehaviorBlock","overscrollBehaviorInline","overscrollBehaviorX",
  "overscrollBehaviorY","padding","paddingBlock","paddingBlockEnd","paddingBlockStart","paddingBottom",
  "paddingInline","paddingInlineEnd","paddingInlineStart","paddingLeft","paddingRight","paddingTop",
  "pageBreakAfter","pageBreakBefore","pageBreakInside","perspective","perspectiveOrigin","placeContent",
  "placeItems","placeSelf","pointerEvents","position","quotes","resize","right","rotate","rowGap",
  "scale","scrollBehavior","scrollMargin","scrollPadding","scrollSnapAlign","scrollSnapStop",
  "scrollSnapType","tabSize","tableLayout","textAlign","textAlignLast","textCombineUpright",
  "textDecoration","textDecorationColor","textDecorationLine","textDecorationSkipInk",
  "textDecorationStyle","textDecorationThickness","textEmphasis","textIndent","textJustify",
  "textOrientation","textOverflow","textRendering","textShadow","textTransform","textUnderlineOffset",
  "textUnderlinePosition","top","touchAction","transform","transformBox","transformOrigin",
  "transformStyle","transition","transitionDelay","transitionDuration","transitionProperty",
  "transitionTimingFunction","translate","unicodeBidi","userSelect","verticalAlign","visibility",
  "whiteSpace","width","willChange","wordBreak","wordSpacing","wordWrap","writingMode","zIndex","zoom",
];
const _CSS_PROP_SET = new Set(_CSS_PROPERTY_NAMES);

// Parse a `style` attribute string (`"color: red; margin: 5px"`) into the given
// dashed-key store, replacing its contents in place.
function _parseCssInto(props, text) {
  for (const k in props) delete props[k];
  if (text) _splitCssDeclarations(text).forEach((p) => {
    const i = p.indexOf(":");
    if (i > 0) { const k = p.slice(0, i).trim(); const v = p.slice(i + 1).trim(); if (k && v) props[_cssCamelToKebab(k)] = v; }
  });
}
// Declaration values routinely contain semicolons in quoted `content`, data
// URLs, gradients, and custom-property token streams. Split only at the
// declaration-list level so reflecting a CSSStyleRule through CSSOM does not
// corrupt otherwise valid CSS before the renderer sees it.
function _splitCssDeclarations(value) {
  const text = String(value || "");
  const declarations = [];
  let start = 0, quote = "", escaped = false, comment = false;
  let parens = 0, brackets = 0, braces = 0;
  const push = (end) => {
    const declaration = text.slice(start, end).trim();
    if (declaration) declarations.push(declaration);
  };
  for (let index = 0; index < text.length; index++) {
    const ch = text[index], next = text[index + 1];
    if (comment) {
      if (ch === "*" && next === "/") { comment = false; index++; }
      continue;
    }
    if (escaped) { escaped = false; continue; }
    if (ch === "\\") { escaped = true; continue; }
    if (quote) { if (ch === quote) quote = ""; continue; }
    if (ch === "/" && next === "*") { comment = true; index++; continue; }
    if (ch === '"' || ch === "'") { quote = ch; continue; }
    if (ch === "(") { parens++; continue; }
    if (ch === ")") { parens = Math.max(0, parens - 1); continue; }
    if (ch === "[") { brackets++; continue; }
    if (ch === "]") { brackets = Math.max(0, brackets - 1); continue; }
    if (ch === "{") { braces++; continue; }
    if (ch === "}") { braces = Math.max(0, braces - 1); continue; }
    if (ch === ";" && !parens && !brackets && !braces) {
      push(index);
      start = index + 1;
    }
  }
  push(text.length);
  return declarations;
}
function _serializeCss(props) {
  const e = Object.entries(props);
  return e.length ? e.map(([k, v]) => `${k}: ${v}`).join("; ") + ";" : "";
}

class CSSStyleDeclaration {
  constructor(owner, onChange) {
    // Non-enumerable so they never leak through the proxy's own-key traps.
    Object.defineProperty(this, "_props", { value: {}, writable: true, enumerable: false, configurable: true });
    // The owner Element, if any. A live declaration reflects that element's
    // `style` content attribute in both directions; an owner-less declaration
    // (getComputedStyle fallback, stylesheet rules) is purely in-memory.
    Object.defineProperty(this, "_owner", { value: owner || null, writable: true, enumerable: false, configurable: true });
    Object.defineProperty(this, "_onChange", { value: onChange || null, writable: true, enumerable: false, configurable: true });
    // Load the content attribute only when style is first observed. Keeping
    // this as a primitive avoids allocating a separate sync object for every
    // wrapped element.
    Object.defineProperty(this, "_loaded", { value: !owner, writable: true, enumerable: false, configurable: true });
  }
  // Pull the initial `style` attribute once. Later attribute mutations update
  // the declaration directly from Element.setAttribute/removeAttribute, so
  // repeated style reads do not cross the JS/Rust op boundary.
  _pull() {
    if (this._loaded) return;
    _parseCssInto(this._props, this._owner.getAttribute("style"));
    this._loaded = true;
  }
  _replaceFromAttribute(text) {
    _parseCssInto(this._props, text);
    this._loaded = true;
  }
  // Serialize `_props` back onto the owner's `style` attribute after a mutation,
  // so el.style.x = … and cssText reflect into getAttribute('style') and
  // serialization. No-op when owner-less.
  _push() {
    const o = this._owner;
    if (o) {
      const text = _serializeCss(this._props);
      if (text) o.setAttribute("style", text);
      else o.removeAttribute("style");
    } else if (this._onChange) {
      this._onChange();
    }
  }
  // Storage is keyed by the dashed CSS name, matching CSSOM. The proxy maps the
  // camelCase IDL access (el.style.fontSize) onto the dashed key (font-size), so
  // getPropertyValue('font-size') and el.style.fontSize stay in sync.
  setProperty(name, value) {
    this._pull();
    const k = _cssCamelToKebab(String(name));
    if (value === "" || value == null) delete this._props[k];
    else this._props[k] = String(value);
    this._push();
  }
  removeProperty(name) { this._pull(); const k = _cssCamelToKebab(String(name)); const old = this._props[k]; delete this._props[k]; this._push(); return old || ""; }
  getPropertyValue(name) { this._pull(); return this._props[_cssCamelToKebab(String(name))] || ""; }
  getPropertyPriority() { return ""; }
  get cssText() { this._pull(); return _serializeCss(this._props); }
  set cssText(v) {
    _parseCssInto(this._props, v);
    this._push();
  }
  get length() { this._pull(); return Object.keys(this._props).length; }
  item(i) { this._pull(); return Object.keys(this._props)[i] || ""; }
}

const _styleProxy = (decl) => new Proxy(decl, {
  get(t, p) {
    if (typeof p === "symbol" || p in t) return t[p];
    if (/^\d+$/.test(p)) return t.item(+p);
    return t.getPropertyValue(p);
  },
  set(t, p, v) {
    if (typeof p === "symbol") { t[p] = v; return true; }
    if (p === "_loaded") { t._loaded = v; return true; }
    if (p === "cssText") { t.cssText = v; return true; }
    if (p in t) { Reflect.set(t, p, v); return true; }
    if (/^\d+$/.test(p)) return true;
    t.setProperty(p, v);
    return true;
  },
  has(t, p) {
    if (typeof p !== "string") return Reflect.has(t, p);
    if (p in Object.getPrototypeOf(t)) return true;
    t._pull();
    if (_cssCamelToKebab(p) in t._props) return true;
    if (_CSS_PROP_SET.has(p) || _CSS_PROP_SET.has(_cssKebabToCamel(p))) return true;
    return /^\d+$/.test(p) && +p < t.length;
  },
  ownKeys(t) {
    t._pull();
    const keys = [];
    const n = t.length;
    for (let i = 0; i < n; i++) keys.push(String(i));
    const names = new Set(_CSS_PROPERTY_NAMES);
    for (const k of Object.keys(t._props)) names.add(_cssKebabToCamel(k));
    for (const name of names) keys.push(name);
    return keys;
  },
  getOwnPropertyDescriptor(t, p) {
    if (typeof p !== "string") return Reflect.getOwnPropertyDescriptor(t, p);
    t._pull();
    if (/^\d+$/.test(p) && +p < t.length) return { value: t.item(+p), writable: false, enumerable: true, configurable: true };
    if (_cssCamelToKebab(p) in t._props || _CSS_PROP_SET.has(p) || _CSS_PROP_SET.has(_cssKebabToCamel(p))) {
      return { value: t.getPropertyValue(p), writable: true, enumerable: true, configurable: true };
    }
    return undefined;
  },
});

// Clone a single node (no children), used by Node.cloneNode. Elements are built
// with createElement/createElementNS and their content attributes copied, so no
// HTML parsing context is involved and every attribute (including style) is
// preserved. Text/Comment/DocumentFragment map to their factory; anything else
// yields null.
function _shallowCloneNode(node) {
  const nt = node.nodeType;
  if (nt === 3) return document.createTextNode(node.data != null ? node.data : (node.textContent || ""));
  if (nt === 8) return document.createComment(node.data != null ? node.data : (node.nodeValue || ""));
  if (nt === 11) return document.createDocumentFragment();
  if (nt !== 1) return null;
  const ns = node.namespaceURI;
  const el = (ns && ns !== "http://www.w3.org/1999/xhtml")
    ? document.createElementNS(ns, node.nodeName)
    : document.createElement(node.localName || node.nodeName.toLowerCase());
  const names = node.getAttributeNames ? node.getAttributeNames() : [];
  for (const name of names) {
    const v = node.getAttribute(name);
    if (v !== null) el.setAttribute(name, v);
  }
  // CSS declarations currently live on the JS wrapper independently of the
  // DOM attribute. Copy that state as well so styles assigned through
  // `node.style` survive cloning even before attribute reflection runs.
  if (node.style && node.style.cssText) el.style.cssText = node.style.cssText;
  return el;
}

// EventTarget listener state belongs to the JS wrapper rather than the backing
// DOM node.  This is also what makes `new EventTarget()` and subclasses used by
// framework schedulers work: those targets deliberately have no native node id.
const _eventTargetListeners = new WeakMap();
function _eventCapture(options) {
  return typeof options === "boolean" ? options : !!(options && options.capture);
}
function _eventTargetAdd(target, type, callback, options) {
  if (callback == null) return;
  const isFunction = typeof callback === "function";
  if (!isFunction && typeof callback.handleEvent !== "function") return;
  type = String(type);
  const capture = _eventCapture(options);
  const signal = options && typeof options === "object" ? options.signal : null;
  if (signal && signal.aborted) return;
  let byType = _eventTargetListeners.get(target);
  if (!byType) {
    byType = new Map();
    _eventTargetListeners.set(target, byType);
  }
  let listeners = byType.get(type);
  if (!listeners) {
    listeners = [];
    byType.set(type, listeners);
  }
  if (listeners.some((entry) => entry.callback === callback && entry.capture === capture)) return;
  const entry = {
    callback,
    capture,
    once: !!(options && typeof options === "object" && options.once),
    passive: !!(options && typeof options === "object" && options.passive),
    signal,
    abortHandler: null,
  };
  listeners.push(entry);
  if (signal && typeof signal.addEventListener === "function") {
    entry.abortHandler = () => _eventTargetRemove(target, type, callback, capture);
    signal.addEventListener("abort", entry.abortHandler, { once: true });
  }
}
function _eventTargetRemove(target, type, callback, options) {
  const byType = _eventTargetListeners.get(target);
  if (!byType) return;
  type = String(type);
  const listeners = byType.get(type);
  if (!listeners) return;
  const capture = _eventCapture(options);
  for (let i = 0; i < listeners.length; i++) {
    const entry = listeners[i];
    if (entry.callback !== callback || entry.capture !== capture) continue;
    listeners.splice(i, 1);
    if (entry.signal && entry.abortHandler && typeof entry.signal.removeEventListener === "function") {
      entry.signal.removeEventListener("abort", entry.abortHandler);
    }
    break;
  }
  if (listeners.length === 0) byType.delete(type);
  if (byType.size === 0) _eventTargetListeners.delete(target);
}
function _eventTargetDispatch(target, event) {
  if (!event || typeof event.type === "undefined") {
    throw new TypeError("Failed to execute 'dispatchEvent' on 'EventTarget': parameter 1 is not of type 'Event'.");
  }
  if (String(event.type) === "") {
    throw new DOMException("The event's type was not specified.", "InvalidStateError");
  }
  if (!event.target) event.target = target;
  event.currentTarget = target;
  event.eventPhase = 2;
  const listeners = (_eventTargetListeners.get(target)?.get(String(event.type)) || []).slice();
  for (const entry of listeners) {
    const current = _eventTargetListeners.get(target)?.get(String(event.type));
    if (!current || !current.includes(entry)) continue;
    if (entry.once) _eventTargetRemove(target, event.type, entry.callback, entry.capture);
    const callback = entry.callback;
    try {
      if (typeof callback === "function") callback.call(target, event);
      else callback.handleEvent.call(callback, event);
    } catch (error) {
      console.error(error);
    }
    if (event._immediatePropagationStopped) break;
  }
  event.currentTarget = null;
  event.eventPhase = 0;
  return !event.defaultPrevented;
}

// During custom-element upgrade, HTMLElement's constructor must return the
// already-existing element being upgraded. A class constructor cannot be
// invoked with `.call(existingElement)`, so the registry and Element
// constructor coordinate through the same construction-stack shape used by
// browser custom-element implementations.
const _customElementConstructionStack = [];

function __prepareInsertedScript(script) {
  if (!Deno.core.ops.op_script_try_start(script._nid)) return;
  const scriptType = (script.getAttribute('type') || '').trim().toLowerCase();
  const isModule = scriptType === 'module';
  const isImportMap = scriptType === 'importmap';
  if (isImportMap) {
    const src = script.getAttribute('src');
    let error = '';
    if (src) {
      error = 'External import maps are not supported';
    } else {
      const base = script.baseURI
        || globalThis.location?.href
        || 'about:blank';
      try {
        error = Deno.core.ops.op_add_import_map(script.textContent || '', base) || '';
      } catch (e) {
        error = e && e.message ? e.message : String(e);
      }
    }
    if (error) {
      console.error('Import map error:', error);
      queueMicrotask(() => {
        try { script.dispatchEvent(new Event('error')); } catch (_) {}
      });
    }
    return;
  }
  if (scriptType && !isModule && scriptType !== 'text/javascript' && scriptType !== 'application/javascript') {
    return;
  }
  const src = script.getAttribute('src');
  const code = src ? "" : script.textContent;
  if (!src && !code) return;
  const prevNid = globalThis.__currentScriptNid;
  if (src) {
    let baseHref;
    try {
      const baseEl = globalThis.document?.querySelector('base[href]');
      baseHref = baseEl ? baseEl.getAttribute('href') : null;
    } catch(e) { baseHref = null; }
    const docUrl = globalThis.location?.href || 'http://localhost/';
    let baseUrl;
    try { baseUrl = baseHref ? new URL(baseHref, docUrl).href : docUrl; }
    catch(e) { baseUrl = docUrl; }
    let fullUrl;
    try {
      fullUrl = src.startsWith('http') || src.startsWith('data:')
        ? src
        : new URL(src, baseUrl).href;
    } catch(e) {
      console.error('Dynamic script URL resolve failed (' + src + '):', e.message);
      fullUrl = src;
    }
    const pageOrigin = (function() { try { return new URL(baseUrl).origin; } catch(e) { return ""; } })();
    const task = {
      url: fullUrl,
      isModule,
      nid: script._nid,
      prevNid,
      pageOrigin,
      dispatchEvent: (ev) => { try { script.dispatchEvent(ev); } catch(e) {} },
    };
    // Non-parser-inserted external scripts are async by default, but scripts
    // prepared while the document is still loading still delay window.load.
    // Snapshot the flag at preparation time: changing readyState later must
    // not turn already-prepared work into a post-load enhancement.
    task.delaysLoad = globalThis.document?.readyState !== 'complete';
    if (task.delaysLoad) __dynLoadDelayingPending++;
    // A non-parser-inserted classic script is force-async unless script code
    // explicitly assigned `.async = false`. Keep that opt-out in insertion
    // order; default/async=true scripts fetch concurrently and execute as soon
    // as each response is ready.
    const explicitlyInOrder = !isModule
      && Object.prototype.hasOwnProperty.call(script, 'async')
      && script.async === false;
    if (!isModule) {
      // Fetch all dynamically inserted classics immediately. `async=false`
      // changes only execution order: browsers still overlap their network
      // requests, then hold a ready body behind earlier ordered scripts.
      __startDynClassicFetch(task);
      if (explicitlyInOrder) {
        __dynScriptQueue.push(task);
        __processDynScriptQueue();
      } else {
        __runAsyncClassicScript(task);
      }
    } else {
      __dynScriptQueue.push(task);
      __processDynScriptQueue();
    }
  } else if (isModule) {
    const dataUrl = 'data:text/javascript;base64,' + btoa(unescape(encodeURIComponent(code)));
    const task = {
      url: dataUrl,
      isModule: true,
      nid: script._nid,
      prevNid,
      pageOrigin: "",
      dispatchEvent: (ev) => { try { script.dispatchEvent(ev); } catch(e) {} },
      delaysLoad: globalThis.document?.readyState !== 'complete',
    };
    if (task.delaysLoad) __dynLoadDelayingPending++;
    __dynScriptQueue.push(task);
    __processDynScriptQueue();
  } else {
    globalThis.__currentScriptNid = script._nid;
    try { (0, eval)(code); }
    catch(e) { console.error('Dynamic inline script error:', e.message); }
    finally { globalThis.__currentScriptNid = prevNid || 0; }
  }
}

function __prepareInsertedSubtree(root) {
  // HTML's script preparation algorithm leaves a disconnected script
  // unstarted.  When an ancestor is later connected, insertion steps visit
  // every script in that subtree in tree order.
  if (!root || !root.isConnected) return;
  const scripts = [];
  const seen = new Set();
  if (root.nodeType === 1 && root.tagName === 'SCRIPT') {
    scripts.push(root);
    seen.add(root._nid);
  }
  const ids = _domParse("query_selector_all_scoped", root._nid, "script") || [];
  for (const nid of ids) {
    const script = _wrapEl(+nid);
    if (script && !seen.has(script._nid)) {
      scripts.push(script);
      seen.add(script._nid);
    }
  }
  for (const script of scripts) __prepareInsertedScript(script);
}

function _seedDetachedTreeState(node) {
  node._treeDetachedExact = true;
  node._treeParent = null;
  node._treeParentEpoch = _treeMutationEpoch;
  node._treeConnected = false;
  node._treeConnectedEpoch = _treeMutationEpoch;
}

function _seedInsertedTreeState(node, parent, connected) {
  node._treeDetachedExact = false;
  node._treeParent = parent;
  node._treeParentEpoch = _treeMutationEpoch;
  node._treeConnected = !!connected;
  node._treeConnectedEpoch = _treeMutationEpoch;
}

function _seedUnchangedConnection(node, connected) {
  node._treeConnected = !!connected;
  node._treeConnectedEpoch = _treeMutationEpoch;
}

class Node {
  static ELEMENT_NODE = 1;
  static ATTRIBUTE_NODE = 2;
  static TEXT_NODE = 3;
  static CDATA_SECTION_NODE = 4;
  static ENTITY_REFERENCE_NODE = 5;
  static ENTITY_NODE = 6;
  static PROCESSING_INSTRUCTION_NODE = 7;
  static COMMENT_NODE = 8;
  static DOCUMENT_NODE = 9;
  static DOCUMENT_TYPE_NODE = 10;
  static DOCUMENT_FRAGMENT_NODE = 11;
  static NOTATION_NODE = 12;
  static DOCUMENT_POSITION_DISCONNECTED = 1;
  static DOCUMENT_POSITION_PRECEDING = 2;
  static DOCUMENT_POSITION_FOLLOWING = 4;
  static DOCUMENT_POSITION_CONTAINS = 8;
  static DOCUMENT_POSITION_CONTAINED_BY = 16;
  static DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC = 32;

  constructor(nid) { this._nid = nid; }
  get nodeType() { return +_dom("node_type", this._nid); }
  get nodeName() { return _domParse("node_name", this._nid) || ""; }
  get ownerDocument() { return globalThis.document; }
  // https://dom.spec.whatwg.org/#dom-node-baseuri
  get baseURI() {
    try {
      const doc = globalThis.document;
      const docUrl = (doc && doc.URL) || "";
      const baseEl = (doc && doc.querySelector) ? doc.querySelector("base[href]") : null;
      if (baseEl) {
        const href = baseEl.getAttribute("href");
        if (href) {
          return docUrl ? new URL(href, docUrl).href : href;
        }
      }
      return docUrl;
    } catch (e) {
      return "";
    }
  }
  get textContent() { return _domParse("text_content", this._nid) ?? ""; }
  set textContent(v) {
    const oldChildren = _domParse("child_nodes", this._nid) || [];
    for (const c of oldChildren) {
      const child = _wrap(c);
      if (child) _detachStyleSheetsInSubtree(child);
      _dom("remove_child", c);
    }
    let added = [];
    if (v != null && v !== "") {
      const tn = +_dom("create_text_node", String(v));
      _dom("append_child", this._nid, tn);
      added = [tn];
    }
    // Real MutationObserver fires childList for the children swap.
    // Without this React 18+ hydration mismatch detection and many polling
    // libs (intersection-driven lazy load, content sync) silently stall.
    if (globalThis.__mutationObservers?.length) {
      globalThis.__notifyMutation('childList', this._nid, added, oldChildren);
    }
  }
  get nodeValue() {
    const t = this.nodeType;
    if (t === 3 || t === 8) return _domParse("text_content", this._nid) ?? "";
    return null;
  }
  set nodeValue(v) {
    const t = this.nodeType;
    if (t === 3 || t === 8) _dom("set_text_content", this._nid, String(v ?? ""));
  }
  get parentNode() {
    if (this._shadowParent) return this._shadowParent;
    if (this._treeDetachedExact) return null;
    if (this._treeParentEpoch === _treeMutationEpoch) return this._treeParent;
    const parent = _wrap(+_dom("parent_node", this._nid));
    this._treeParent = parent;
    this._treeParentEpoch = _treeMutationEpoch;
    return parent;
  }
  get parentElement() { const p = this.parentNode; return p && p.nodeType === 1 ? p : null; }
  get childNodes() {
    const ids = _domParse("child_nodes", this._nid) || [];
    return _nodeList(ids.map(_wrap).filter(Boolean));
  }
  get firstChild() { return _wrap(+_dom("first_child", this._nid)); }
  get lastChild() { return _wrap(+_dom("last_child", this._nid)); }
  get nextSibling() {
    if (this._shadowParent) {
      const children = this._shadowParent.childNodes;
      const index = children.indexOf(this);
      return index >= 0 ? (children[index + 1] || null) : null;
    }
    return _wrap(+_dom("next_sibling", this._nid));
  }
  get previousSibling() {
    if (this._shadowParent) {
      const children = this._shadowParent.childNodes;
      const index = children.indexOf(this);
      return index > 0 ? children[index - 1] : null;
    }
    return _wrap(+_dom("prev_sibling", this._nid));
  }
  appendChild(c) {
    if (!c) return c;
    if (c instanceof DocumentFragment) {
      const children = Array.from(c.childNodes);
      for (const child of children) this.appendChild(child);
      return c;
    }
    if (c._shadowParent) c._shadowParent.removeChild(c);
    else if (c.parentNode) _detachStyleSheetsInSubtree(c);
    const parentConnected = this.isConnected;
    const inserted = _dom("append_child", this._nid, c._nid) === "true";
    if (!inserted) {
      throw new DOMException(
        "Failed to execute 'appendChild' on 'Node': The new child would create an invalid tree.",
        "HierarchyRequestError",
      );
    }
    _seedUnchangedConnection(this, parentConnected);
    _seedInsertedTreeState(c, this, parentConnected);
    _registerWindowNamedTree(c);
    if (globalThis.__mutationObservers?.length) globalThis.__notifyMutation('childList', this._nid, [c._nid], []);
    __prepareInsertedSubtree(c);
    if (c instanceof Element && c.tagName === 'LINK') {
      _loadLinkedStylesheet(c);
    }
    return c;
  }
  removeChild(c) {
    if (!c || c.parentNode !== this) {
      throw new DOMException(
        "Failed to execute 'removeChild' on 'Node': The node to be removed is not a child of this node.",
        'NotFoundError'
      );
    }
    const removedWindowNames = _windowNamedNamesInTree(c);
    const linkedStyle = c instanceof Element
      ? _linkedStylesheetNodes.get(c)
      : null;
    if (linkedStyle?.parentNode === this) {
      _dom("remove_child", linkedStyle._nid);
      _linkedStylesheetNodes.delete(c);
    }
    const parentConnected = this.isConnected;
    const removed = _dom("remove_child", c._nid) === "true";
    if (!removed) {
      throw new DOMException(
        "Failed to execute 'removeChild' on 'Node': The node is not a child of this node.",
        "NotFoundError",
      );
    }
    _seedUnchangedConnection(this, parentConnected);
    _seedDetachedTreeState(c);
    _detachStyleSheetsInSubtree(c);
    _reconcileWindowNamedProperties(removedWindowNames);
    if (globalThis.__mutationObservers?.length) globalThis.__notifyMutation('childList', this._nid, [], [c._nid]);
    return c;
  }
  replaceChild(newChild, oldChild) {
    if (!oldChild || !newChild) return oldChild;
    if (oldChild.parentNode !== this) {
      throw new DOMException(
        "Failed to execute 'replaceChild' on 'Node': The node to be replaced is not a child of this node.",
        "NotFoundError",
      );
    }
    if (newChild === oldChild) return oldChild;
    if (newChild instanceof DocumentFragment) {
      const children = Array.from(newChild.childNodes);
      for (const child of children) this.insertBefore(child, oldChild);
      this.removeChild(oldChild);
      return oldChild;
    }
    if (newChild._shadowParent) newChild._shadowParent.removeChild(newChild);
    else if (newChild.parentNode) _detachStyleSheetsInSubtree(newChild);
    const parentConnected = this.isConnected;
    const removedWindowNames = _windowNamedNamesInTree(oldChild);
    const inserted = _dom("insert_before", newChild._nid, oldChild._nid) === "true";
    if (!inserted) {
      throw new DOMException(
        "Failed to execute 'replaceChild' on 'Node': The new child would create an invalid tree.",
        "HierarchyRequestError",
      );
    }
    const removed = _dom("remove_child", oldChild._nid) === "true";
    if (!removed) throw new DOMException("The node could not be replaced.", "NotFoundError");
    _seedUnchangedConnection(this, parentConnected);
    _seedInsertedTreeState(newChild, this, parentConnected);
    _seedDetachedTreeState(oldChild);
    _detachStyleSheetsInSubtree(oldChild);
    _registerWindowNamedTree(newChild);
    _reconcileWindowNamedProperties(removedWindowNames);
    __prepareInsertedSubtree(newChild);
    return oldChild;
  }
  insertBefore(n, ref) {
    if (!n) return n;
    if (!ref) { this.appendChild(n); return n; }
    if (ref.parentNode !== this) {
      throw new DOMException(
        "Failed to execute 'insertBefore' on 'Node': The reference node is not a child of this node.",
        "NotFoundError",
      );
    }
    if (n === ref) return n;
    if (n instanceof DocumentFragment) {
      const children = Array.from(n.childNodes);
      for (const child of children) this.insertBefore(child, ref);
      return n;
    }
    if (n._shadowParent) n._shadowParent.removeChild(n);
    else if (n.parentNode) _detachStyleSheetsInSubtree(n);
    const parentConnected = this.isConnected;
    const inserted = _dom("insert_before", n._nid, ref._nid) === "true";
    if (!inserted) {
      throw new DOMException(
        "Failed to execute 'insertBefore' on 'Node': The new child would create an invalid tree.",
        "HierarchyRequestError",
      );
    }
    _seedUnchangedConnection(this, parentConnected);
    _seedInsertedTreeState(n, this, parentConnected);
    _registerWindowNamedTree(n);
    __prepareInsertedSubtree(n);
    return n;
  }
  contains(o) { return o ? _dom("contains", this._nid, o._nid) === "true" : false; }
  hasChildNodes() { return _dom("has_child_nodes", this._nid) === "true"; }
  cloneNode(deep) {
    const t = this.nodeType;
    if (t === 1) {
      return _wrap(+_dom("clone_node", this._nid, deep ? "true" : "false"));
    }
    // Clone structurally via real DOM nodes rather than round-tripping through a
    // throwaway <div>.innerHTML: the fragment parser discards elements that are
    // not valid children of <div> (<tr>, <td>, <option>, …), so the old path
    // returned null for them and lost JS-set inline styles. Building each node
    // directly with createElement(NS) + attribute copy avoids any parsing
    // context, and an explicit stack keeps a deep subtree from overflowing the
    // JS stack (issue #490).
    const root = _shallowCloneNode(this);
    if (!deep || !root) return root;
    const stack = [[this, root]];
    while (stack.length) {
      const [src, dst] = stack.pop();
      // A <template>'s children hang off its content fragment, not childNodes,
      // so clone them into the clone's fragment. Gated on the tag name because
      // .content means something else on other elements (e.g. <meta>).
      if (src.localName === 'template' && dst.localName === 'template') {
        const sc = src.content, dc = dst.content;
        if (sc && dc && sc.childNodes) {
          const tk = sc.childNodes;
          for (let i = 0; i < tk.length; i++) {
            const c = _shallowCloneNode(tk[i]);
            if (c) { dc.appendChild(c); stack.push([tk[i], c]); }
          }
        }
      }
      const kids = src.childNodes;
      for (let i = 0; i < kids.length; i++) {
        const c = _shallowCloneNode(kids[i]);
        if (c) { dst.appendChild(c); stack.push([kids[i], c]); }
      }
    }
    return root;
  }
  compareDocumentPosition(other) {
    if (!other) return 0;
    if (this._nid === other._nid) return 0;
    // Different roots: DISCONNECTED | IMPLEMENTATION_SPECIFIC plus a stable
    // (consistent across calls) PRECEDING/FOLLOWING bit, chosen by node-id order.
    if (+_dom("node_root", this._nid) !== +_dom("node_root", other._nid)) {
      return 1 | 32 | ((this._nid < other._nid) ? 4 : 2);
    }
    if (this.contains(other)) return 16 | 4;          // CONTAINED_BY | FOLLOWING
    if (other.contains && other.contains(this)) return 8 | 2; // CONTAINS | PRECEDING
    // Same root, neither contains the other: real tree order (compare_order op:
    // -1 => this precedes other => other FOLLOWS this(4); +1 => this PRECEDING(2)).
    return (+_dom("compare_order", this._nid, other._nid) < 0) ? 4 : 2;
  }
  getRootNode(options) {
    const root = _wrap(+_dom("node_root", this._nid));
    if (options?.composed && root instanceof ShadowRoot) {
      return root.host.getRootNode(options);
    }
    return root;
  }
  get isConnected() {
    if (this._treeDetachedExact) return false;
    if (this._treeConnectedEpoch === _treeMutationEpoch) return this._treeConnected;
    const connected = _dom("is_connected", this._nid) === "true";
    this._treeConnected = connected;
    this._treeConnectedEpoch = _treeMutationEpoch;
    return connected;
  }
  normalize() {
    // Merge adjacent exclusive Text nodes, drop empty ones, recurse. Detached
    // removed nodes keep their own data (read from the backing node by nid).
    let child = this.firstChild;
    while (child) {
      const next = child.nextSibling;
      if (child.nodeType === 3) {
        let data = child.data, sib = child.nextSibling;
        while (sib && sib.nodeType === 3) { const after = sib.nextSibling; data += sib.data; this.removeChild(sib); sib = after; }
        if (data.length === 0) { this.removeChild(child); child = sib; continue; }
        if (data !== child.data) child.data = data;
        child = sib; continue;
      } else if (child.nodeType === 1 || child.nodeType === 11) {
        child.normalize();
      }
      child = next;
    }
  }
  isEqualNode(other) {
    if (!other) return false;
    if (this._nid === other._nid) return true;
    if (this.nodeType !== other.nodeType) return false;
    if (this.nodeName !== other.nodeName) return false;
    if (this.nodeValue !== other.nodeValue) return false;
    const a = this.attributes ? this.attributes : null;
    const b = other.attributes ? other.attributes : null;
    if ((a && a.length) || (b && b.length)) {
      if (!a || !b || a.length !== b.length) return false;
      for (let i = 0; i < a.length; i++) {
        if (other.getAttribute(a[i].name) !== a[i].value) return false;
      }
    }
    const cA = this.childNodes || [];
    const cB = other.childNodes || [];
    if (cA.length !== cB.length) return false;
    for (let i = 0; i < cA.length; i++) {
      if (!cA[i].isEqualNode(cB[i])) return false;
    }
    return true;
  }
  isSameNode(other) { return other && this._nid === other._nid; }
  addEventListener(type, callback, options) {
    _eventTargetAdd(this, type, callback, options);
  }
  removeEventListener(type, callback, options) {
    _eventTargetRemove(this, type, callback, options);
  }
  dispatchEvent(event) {
    return _eventTargetDispatch(this, event);
  }
}
class CharacterData extends Node {
  get data() {
    return _domParse("text_content", this._nid) ?? "";
  }
  set data(v) {
    const oldValue = _domParse("text_content", this._nid) ?? "";
    _dom("set_text_content", this._nid, String(v ?? ""));
    if (globalThis.__mutationObservers?.length) {
      globalThis.__notifyMutation('characterData', this._nid, [], [], null, oldValue);
    }
  }
  get length() { return this.data.length; }
  substringData(offset, count) {
    return this.data.substring(offset, offset + count);
  }
  appendData(s) { this.data += s; }
  insertData(offset, s) {
    const d = this.data;
    this.data = d.slice(0, offset) + s + d.slice(offset);
  }
  deleteData(offset, count) {
    const d = this.data;
    this.data = d.slice(0, offset) + d.slice(offset + count);
  }
  replaceData(offset, count, s) {
    const d = this.data;
    this.data = d.slice(0, offset) + s + d.slice(offset + count);
  }
}

class Text extends CharacterData {
  get nodeName() { return "#text"; }
  get nodeType() { return 3; }
  get wholeText() { return this.data; }
  splitText(offset) {
    const d = this.data;
    const tail = d.substring(offset);
    this.data = d.substring(0, offset);
    const newNid = +_dom("create_text_node", tail);
    const parent = this.parentNode;
    if (parent) {
      const ref = this.nextSibling;
      parent.insertBefore(_wrap(newNid), ref);
    }
    return _wrap(newNid);
  }
  cloneNode() { return document.createTextNode(this.data); }
}

class Comment extends CharacterData {
  get nodeName() { return "#comment"; }
  get nodeType() { return 8; }
  cloneNode() { return document.createComment(this.data); }
}

// DOMTokenList backs class/rel/sandbox/etc. attribute reflection. It parses the
// associated content attribute as an ordered set of tokens and writes changes
// straight back, so reads and writes stay live with the element. A Proxy is
// layered on top so numeric indexing (list[0]) hits item().
class DOMTokenList {
  constructor(el, attr, supportedTokens) {
    // Non-enumerable so the element <-> token-list cycle is not visible to
    // enumeration/serialization (JSON.stringify(classList) would otherwise
    // throw "circular structure").
    Object.defineProperty(this, "_el", { value: el, writable: true, enumerable: false });
    Object.defineProperty(this, "_attr", { value: attr, writable: true, enumerable: false });
    Object.defineProperty(this, "_supported", { value: supportedTokens || null, writable: true, enumerable: false });
    return new Proxy(this, {
      get(t, k, r) {
        if (typeof k === "string" && /^\d+$/.test(k)) return t.item(+k);
        return Reflect.get(t, k, r);
      },
      has(t, k) {
        if (typeof k === "string" && /^\d+$/.test(k)) return +k < t.length;
        return Reflect.has(t, k);
      },
    });
  }
  get [Symbol.toStringTag]() { return "DOMTokenList"; }
  _tokens() {
    const v = this._el.getAttribute(this._attr);
    if (!v) return [];
    const seen = new Set();
    const out = [];
    for (const tok of v.split(/[ \t\n\f\r]+/)) {
      if (tok && !seen.has(tok)) { seen.add(tok); out.push(tok); }
    }
    return out;
  }
  _write(tokens) {
    this._el.setAttribute(this._attr, tokens.join(" "));
  }
  get length() { return this._tokens().length; }
  get value() { return this._el.getAttribute(this._attr) || ""; }
  set value(v) { this._el.setAttribute(this._attr, String(v)); }
  item(i) { const t = this._tokens(); return (i >= 0 && i < t.length) ? t[i] : null; }
  contains(token) { return this._tokens().includes(String(token)); }
  add(...tokens) {
    const t = this._tokens();
    for (const raw of tokens) {
      const tok = String(raw);
      if (tok === "") throw new DOMException("The token provided must not be empty.", "SyntaxError");
      if (/[ \t\n\f\r]/.test(tok)) throw new DOMException("The token provided contains HTML space characters, which are not valid in tokens.", "InvalidCharacterError");
      if (!t.includes(tok)) t.push(tok);
    }
    this._write(t);
  }
  remove(...tokens) {
    let t = this._tokens();
    for (const raw of tokens) {
      const tok = String(raw);
      if (tok === "") throw new DOMException("The token provided must not be empty.", "SyntaxError");
      if (/[ \t\n\f\r]/.test(tok)) throw new DOMException("The token provided contains HTML space characters, which are not valid in tokens.", "InvalidCharacterError");
      t = t.filter((x) => x !== tok);
    }
    this._write(t);
  }
  toggle(token, force) {
    const tok = String(token);
    if (tok === "") throw new DOMException("The token provided must not be empty.", "SyntaxError");
    if (/[ \t\n\f\r]/.test(tok)) throw new DOMException("The token provided contains HTML space characters, which are not valid in tokens.", "InvalidCharacterError");
    const t = this._tokens();
    const has = t.includes(tok);
    if (has) {
      if (force === true) return true;
      this._write(t.filter((x) => x !== tok));
      return false;
    }
    if (force === false) return false;
    t.push(tok);
    this._write(t);
    return true;
  }
  replace(token, newToken) {
    const a = String(token), b = String(newToken);
    if (a === "" || b === "") throw new DOMException("The token provided must not be empty.", "SyntaxError");
    if (/[ \t\n\f\r]/.test(a) || /[ \t\n\f\r]/.test(b)) throw new DOMException("The token provided contains HTML space characters, which are not valid in tokens.", "InvalidCharacterError");
    const t = this._tokens();
    const i = t.indexOf(a);
    if (i === -1) return false;
    if (t.includes(b) && b !== a) { t.splice(i, 1); } else { t[i] = b; }
    this._write(t);
    return true;
  }
  supports(token) {
    if (!this._supported) throw new TypeError("DOMTokenList has no supported tokens.");
    return this._supported.includes(String(token).toLowerCase());
  }
  forEach(cb, thisArg) {
    const t = this._tokens();
    for (let i = 0; i < t.length; i++) cb.call(thisArg, t[i], i, this);
  }
  *values() { yield* this._tokens(); }
  *keys() { const t = this._tokens(); for (let i = 0; i < t.length; i++) yield i; }
  *entries() { const t = this._tokens(); for (let i = 0; i < t.length; i++) yield [i, t[i]]; }
  [Symbol.iterator]() { return this._tokens()[Symbol.iterator](); }
  toString() { return this.value; }
}

// CDATASection: a Text-derived node (nodeType 4) used only in XML documents.
// Extends Text so data/length/textContent/childNodes reuse the working text
// node machinery; only the type-identifying getters differ.
class CDATASection extends Text {
  get nodeName() { return "#cdata-section"; }
  get nodeType() { return 4; }
  get nodeValue() { return this.data; }
  set nodeValue(v) { this.data = v; }
  cloneNode() { return new CDATASection(+_dom("create_text_node", this.data)); }
}

// ProcessingInstruction: nodeType 7, nodeName === target. Extends CharacterData
// and carries a separate target. Backed by a text node so data/nodeValue/
// textContent/length work without native PI support.
class ProcessingInstruction extends CharacterData {
  constructor(nid, target) { super(nid); this._target = target; }
  get target() { return this._target; }
  get nodeName() { return this._target; }
  get nodeType() { return 7; }
  get nodeValue() { return this.data; }
  set nodeValue(v) { this.data = v; }
  cloneNode() { return new ProcessingInstruction(+_dom("create_text_node", this.data), this._target); }
}

// Document character encoding (WHATWG canonical name, e.g. "UTF-8", "EUC-JP").
// Cached per runtime: the encoding is fixed for a document's lifetime and this
// is read on every <a>/<area> URL-component access, so the UTF-8 common case
// must reduce to a single cached-boolean read with no op call and no allocation.
let __docEncoding;
let __docIsUtf8;
function _docEncoding() {
  if (__docEncoding === undefined) {
    const e = _domParse("document_encoding");
    __docEncoding = (typeof e === 'string' && e) ? e : 'UTF-8';
    __docIsUtf8 = __docEncoding.toLowerCase() === 'utf-8';
  }
  return __docEncoding;
}
function _docIsUtf8() { if (__docIsUtf8 === undefined) _docEncoding(); return __docIsUtf8; }
// WHATWG "special scheme" check (these get the special-query percent-encode set).
function _isSpecialScheme(protocol) {
  const s = (protocol || '').replace(/:$/, '').toLowerCase();
  return s === 'http' || s === 'https' || s === 'ws' || s === 'wss' || s === 'ftp' || s === 'file';
}
// Apply the WHATWG URL "encoding override": in a legacy (non-UTF-8) document
// the query of an <a>/<area> href is percent-encoded in the document charset,
// not UTF-8. The url op already produced a UTF-8-encoded query; recover the
// original characters (percent-decode + UTF-8) and re-encode them through the
// document charset. Pure-ASCII queries round-trip unchanged.
function _applyDocQueryEncoding(u) {
  if (!u || !u.search || u.search.length < 2) return u;
  let decoded;
  try { decoded = decodeURIComponent(u.search.slice(1)); } catch (e) { return u; }
  let reencoded;
  try { reencoded = Deno.core.ops.op_url_encode_query(decoded, _docEncoding(), _isSpecialScheme(u.protocol)); }
  catch (e) { return u; }
  const newSearch = '?' + reencoded;
  if (newSearch === u.search) return u;
  const hashIdx = u.href.indexOf('#');
  const frag = hashIdx >= 0 ? u.href.slice(hashIdx) : '';
  const beforeHash = hashIdx >= 0 ? u.href.slice(0, hashIdx) : u.href;
  const qIdx = beforeHash.indexOf('?');
  u.href = (qIdx >= 0 ? beforeHash.slice(0, qIdx) : beforeHash) + newSearch + frag;
  u.search = newSearch;
  return u;
}

// HTMLHyperlinkElementUtils helpers (the <a>/<area> URL-decomposition members).
// The element's href attribute is parsed against the document base URL via the
// WHATWG url op; component getters read it, setters rewrite the href attribute.
function _anchorBase() { return _domParse("document_url") || "about:blank"; }
function _elemHrefURL(el) {
  const raw = el.getAttribute('href');
  if (raw === null || raw === undefined) return null;
  const u = _urlParseOp(raw, _anchorBase());
  if (u && !_docIsUtf8()) return _applyDocQueryEncoding(u);
  return u;
}
function _setElemHrefPart(el, part, value) {
  const u = _elemHrefURL(el);
  if (!u) return;
  const c = _urlSetOp(u.href, part, value);
  if (c) el.setAttribute('href', c.href);
}

// --- <input> number/date conversion (valueAsNumber/valueAsDate/stepUp/Down) ---
// Applicable types and their step scale factor + default step (HTML spec).
const _INPUT_NUM_TYPES = { date: 1, month: 1, week: 1, time: 1, 'datetime-local': 1, number: 1, range: 1 };
const _INPUT_DATE_TYPES = { date: 1, month: 1, week: 1, time: 1, 'datetime-local': 1 };
const _INPUT_STEP_SCALE = { date: 86400000, 'datetime-local': 1000, month: 1, number: 1, range: 1, time: 1000, week: 604800000 };
const _INPUT_STEP_DEFAULT = { date: 1, 'datetime-local': 60, month: 1, number: 1, range: 1, time: 60, week: 1 };
function _pad(n, w) { n = String(Math.abs(n | 0)); while (n.length < w) n = '0' + n; return n; }
function _daysInMonth(y, m) { return [31, ((y % 4 === 0 && y % 100 !== 0) || y % 400 === 0) ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][m - 1]; }
function _isoWeek1Monday(y) { const jan4 = Date.UTC(y, 0, 4); const dow = (new Date(jan4).getUTCDay() + 6) % 7; return jan4 - dow * 86400000; }
// Parse an <input> value string to its numeric form per type; NaN if invalid.
function _inputParseNumber(type, v) {
  v = String(v == null ? '' : v);
  let m;
  switch (type) {
    case 'number': case 'range': { if (v === '') return NaN; const n = Number(v); return isFinite(n) ? n : NaN; }
    case 'date': if ((m = /^(\d{4,})-(\d{2})-(\d{2})$/.exec(v))) { const y = +m[1], mo = +m[2], d = +m[3]; if (mo >= 1 && mo <= 12 && d >= 1 && d <= _daysInMonth(y, mo)) return Date.UTC(y, mo - 1, d); } return NaN;
    case 'month': if ((m = /^(\d{4,})-(\d{2})$/.exec(v))) { const y = +m[1], mo = +m[2]; if (mo >= 1 && mo <= 12) return (y - 1970) * 12 + (mo - 1); } return NaN;
    case 'week': if ((m = /^(\d{4,})-W(\d{2})$/.exec(v))) { const y = +m[1], w = +m[2]; if (w >= 1 && w <= 53) return _isoWeek1Monday(y) + (w - 1) * 604800000; } return NaN;
    case 'time': if ((m = /^(\d{2}):(\d{2})(?::(\d{2})(?:\.(\d{1,3}))?)?$/.exec(v))) { const h = +m[1], mi = +m[2], s = m[3] ? +m[3] : 0, ms = m[4] ? +((m[4] + '00').slice(0, 3)) : 0; if (h <= 23 && mi <= 59 && s <= 59) return ((h * 60 + mi) * 60 + s) * 1000 + ms; } return NaN;
    case 'datetime-local': if ((m = /^(\d{4,})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2})(?:\.(\d{1,3}))?)?$/.exec(v))) { const y = +m[1], mo = +m[2], d = +m[3], h = +m[4], mi = +m[5], s = m[6] ? +m[6] : 0, ms = m[7] ? +((m[7] + '00').slice(0, 3)) : 0; if (mo >= 1 && mo <= 12 && d >= 1 && d <= _daysInMonth(y, mo) && h <= 23 && mi <= 59 && s <= 59) return Date.UTC(y, mo - 1, d, h, mi, s, ms); } return NaN;
  }
  return NaN;
}
// Format a numeric value back to an <input> value string per type.
function _inputFormatNumber(type, n) {
  switch (type) {
    case 'number': case 'range': return String(n);
    case 'date': { const dt = new Date(n); return _pad(dt.getUTCFullYear(), 4) + '-' + _pad(dt.getUTCMonth() + 1, 2) + '-' + _pad(dt.getUTCDate(), 2); }
    case 'month': { const y = 1970 + Math.floor(n / 12); const mo = ((n % 12) + 12) % 12 + 1; return _pad(y, 4) + '-' + _pad(mo, 2); }
    case 'week': { const d = new Date(n); const dow = (d.getUTCDay() + 6) % 7; const thu = n - dow * 86400000 + 3 * 86400000; const ty = new Date(thu).getUTCFullYear(); const w = Math.round((n - dow * 86400000 - _isoWeek1Monday(ty)) / 604800000) + 1; return _pad(ty, 4) + '-W' + _pad(w, 2); }
    case 'time': { n = ((n % 86400000) + 86400000) % 86400000; const ms = n % 1000; n = Math.floor(n / 1000); const s = n % 60; n = Math.floor(n / 60); const mi = n % 60; const h = Math.floor(n / 60); let str = _pad(h, 2) + ':' + _pad(mi, 2); if (s || ms) { str += ':' + _pad(s, 2); if (ms) str += '.' + _pad(ms, 3); } return str; }
    case 'datetime-local': { const dt = new Date(n); let str = _pad(dt.getUTCFullYear(), 4) + '-' + _pad(dt.getUTCMonth() + 1, 2) + '-' + _pad(dt.getUTCDate(), 2) + 'T' + _pad(dt.getUTCHours(), 2) + ':' + _pad(dt.getUTCMinutes(), 2); const s = dt.getUTCSeconds(), ms = dt.getUTCMilliseconds(); if (s || ms) { str += ':' + _pad(s, 2); if (ms) str += '.' + _pad(ms, 3); } return str; }
  }
  return String(n);
}

// WebIDL interface constants live on both the interface object and the interface
// prototype object (instances inherit; idlharness checks Node.prototype).
Object.assign(Node.prototype, {
  ELEMENT_NODE: 1, ATTRIBUTE_NODE: 2, TEXT_NODE: 3, CDATA_SECTION_NODE: 4,
  ENTITY_REFERENCE_NODE: 5, ENTITY_NODE: 6, PROCESSING_INSTRUCTION_NODE: 7,
  COMMENT_NODE: 8, DOCUMENT_NODE: 9, DOCUMENT_TYPE_NODE: 10, DOCUMENT_FRAGMENT_NODE: 11,
  NOTATION_NODE: 12, DOCUMENT_POSITION_DISCONNECTED: 1, DOCUMENT_POSITION_PRECEDING: 2,
  DOCUMENT_POSITION_FOLLOWING: 4, DOCUMENT_POSITION_CONTAINS: 8,
  DOCUMENT_POSITION_CONTAINED_BY: 16, DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC: 32,
});

// HTML elements ASCII-lowercase attribute names (setAttribute('accessKey') is
// stored as 'accesskey'). The toLowerCase is gated behind a cheap uppercase
// charCode scan so the all-lowercase common case (href, class, id, data-*)
// allocates nothing and never consults the namespace; only when an uppercase
// ASCII letter is present do we check the element is HTML before folding.
function _htmlAttrName(el, n) {
  n = typeof n === "string" ? n : String(n);
  for (let i = 0; i < n.length; i++) {
    const c = n.charCodeAt(i);
    if (c >= 65 && c <= 90) {
      return el.namespaceURI === "http://www.w3.org/1999/xhtml" ? n.toLowerCase() : n;
    }
  }
  return n;
}

// A submit button per the HTML spec: a <button> whose type is submit — the
// default, including when the type attribute is missing or invalid — or an
// <input> of type submit/image. Used to validate requestSubmit's submitter.
function _isSubmitButton(el) {
  if (!el || typeof el.localName !== "string") return false;
  const type = ((el.getAttribute && el.getAttribute("type")) || "").toLowerCase();
  if (el.localName === "button") return type !== "reset" && type !== "button";
  if (el.localName === "input") return type === "submit" || type === "image";
  return false;
}

// Carry the context element's full qualified name into html5ever. Fragment
// parsing depends on both the local name and namespace (SVG/MathML included).
function _fragmentContextPayload(context, html) {
  let namespace = 'http://www.w3.org/1999/xhtml';
  let qualified = 'body';
  if (typeof context === 'string') {
    qualified = context || 'body';
  } else if (context && context.nodeType === 1) {
    namespace = context.namespaceURI || '';
    qualified = context.nodeName || context.localName || 'body';
  }
  return namespace + "\0" + qualified + "\0" + String(html == null ? '' : html);
}

// Parse an HTML string into detached nodes using the actual insertion element
// as html5ever's fragment context. This preserves table/select parsing rules,
// comments, text-node order, and foreign-content namespaces without a wrap map.
function _parseHTMLFragment(html, context) {
  html = String(html == null ? '' : html);
  const ns = context && context.nodeType === 1 ? context.namespaceURI : null;
  const tag = context && context.nodeType === 1 ? context.localName : 'body';
  const tmp = ns && ns !== 'http://www.w3.org/1999/xhtml'
    ? document.createElementNS(ns, tag)
    : document.createElement(tag);
  tmp.innerHTML = html;
  const out = [];
  let child;
  while ((child = tmp.firstChild)) out.push(tmp.removeChild(child));
  return out;
}

class NamedNodeMap {
  constructor(element) {
    Object.defineProperty(this, "_element", {
      value: element,
      configurable: false,
      enumerable: false,
      writable: false,
    });
    return new Proxy(this, {
      get(target, prop, receiver) {
        if (typeof prop === "string" && /^(?:0|[1-9]\d*)$/.test(prop)) {
          return target.item(+prop);
        }
        if (Reflect.has(target, prop)) return Reflect.get(target, prop, receiver);
        if (typeof prop === "string") return target.getNamedItem(prop);
        return undefined;
      },
      ownKeys(target) {
        const names = target._names();
        return Reflect.ownKeys(target).concat(
          names.map((_, i) => String(i)),
          names.filter((name) => !Reflect.has(target, name))
        );
      },
      getOwnPropertyDescriptor(target, prop) {
        if (typeof prop === "string" && (/^(?:0|[1-9]\d*)$/.test(prop) || target._names().includes(prop))) {
          return { configurable: true, enumerable: true, value: target[prop], writable: false };
        }
        return Reflect.getOwnPropertyDescriptor(target, prop);
      },
    });
  }
  _names() {
    return _domParse("attribute_names", this._element._nid) || [];
  }
  _attr(name) {
    const value = this._element.getAttribute(name);
    if (value === null) return null;
    const attr = new Attr(name, value, null, null);
    attr.ownerElement = this._element;
    return attr;
  }
  get length() { return this._names().length; }
  item(index) {
    const name = this._names()[Number(index)];
    return name === undefined ? null : this._attr(name);
  }
  getNamedItem(name) {
    name = String(name);
    return this._names().includes(name) ? this._attr(name) : null;
  }
  getNamedItemNS(namespaceURI, localName) {
    return this.getNamedItem(localName);
  }
  setNamedItem(attr) {
    if (!attr || typeof attr.name !== "string") return null;
    return this._element.setAttributeNode(attr);
  }
  setNamedItemNS(attr) { return this.setNamedItem(attr); }
  removeNamedItem(name) {
    const attr = this.getNamedItem(name);
    if (!attr) throw new DOMException("Attribute not found", "NotFoundError");
    return this._element.removeAttributeNode(attr);
  }
  removeNamedItemNS(namespaceURI, localName) {
    return this.removeNamedItem(localName);
  }
  *[Symbol.iterator]() {
    for (let i = 0; i < this.length; i++) yield this.item(i);
  }
}
globalThis.NamedNodeMap = NamedNodeMap;

let _waapiNextId = 1;
const _waapiAnimations = new Set();

function _normalizeWaapiKeyframes(input) {
  let frames;
  if (Array.isArray(input)) {
    frames = input.map(frame => ({ ...(frame || {}) }));
  } else if (input && typeof input === 'object') {
    const properties = Object.keys(input).filter(name => name !== 'offset' && name !== 'easing' && name !== 'composite');
    const count = Math.max(1, ...properties.map(name => Array.isArray(input[name]) ? input[name].length : 1));
    frames = Array.from({ length: count }, (_, index) => {
      const frame = {};
      for (const name of properties) {
        const values = Array.isArray(input[name]) ? input[name] : [input[name]];
        frame[name] = values[Math.min(index, values.length - 1)];
      }
      if (Array.isArray(input.offset)) frame.offset = input.offset[Math.min(index, input.offset.length - 1)];
      return frame;
    });
  } else {
    throw new TypeError('Keyframes must be an object or an array');
  }
  if (frames.length === 0) return [];
  let previous = -Infinity;
  for (let i = 0; i < frames.length; i++) {
    if (frames[i].offset != null) {
      const offset = Number(frames[i].offset);
      if (!Number.isFinite(offset) || offset < 0 || offset > 1 || offset < previous) {
        throw new TypeError('Invalid keyframe offset');
      }
      frames[i].offset = offset;
      previous = offset;
    }
  }
  if (frames[0].offset == null) frames[0].offset = 0;
  if (frames[frames.length - 1].offset == null) frames[frames.length - 1].offset = 1;
  let anchor = 0;
  while (anchor < frames.length - 1) {
    let next = anchor + 1;
    while (next < frames.length && frames[next].offset == null) next++;
    const from = frames[anchor].offset;
    const to = frames[next].offset;
    for (let i = anchor + 1; i < next; i++) {
      frames[i].offset = from + (to - from) * ((i - anchor) / (next - anchor));
    }
    anchor = next;
  }
  return frames.map(frame => {
    const normalized = { offset: frame.offset };
    if (frame.opacity != null) {
      const value = Number(frame.opacity);
      if (Number.isFinite(value)) normalized.opacity = Math.max(0, Math.min(1, value));
    }
    if (frame.transform != null) normalized.transform = String(frame.transform);
    return normalized;
  }).filter(frame => frame.opacity != null || frame.transform != null);
}

function _normalizeWaapiTiming(options) {
  if (typeof options === 'number') options = { duration: options };
  options = options || {};
  const duration = options.duration === 'auto' || options.duration == null ? 0 : Number(options.duration);
  const delay = options.delay == null ? 0 : Number(options.delay);
  const iterations = options.iterations == null ? 1 : Number(options.iterations);
  if (!Number.isFinite(duration) || duration < 0 || !Number.isFinite(delay)
      || (!Number.isFinite(iterations) && iterations !== Infinity) || iterations < 0) {
    throw new TypeError('Invalid animation timing');
  }
  const easing = options.easing == null ? 'linear' : String(options.easing).trim();
  const namedBezier = {
    'ease': [0.25, 0.1, 0.25, 1],
    'ease-in': [0.42, 0, 1, 1],
    'ease-out': [0, 0, 0.58, 1],
    'ease-in-out': [0.42, 0, 0.58, 1],
  };
  let easingBezier = easing === 'linear' ? null : namedBezier[easing];
  let linearEasing = null;
  if (easing.startsWith('linear(') && easing.endsWith(')')) {
    const values = easing.slice(7, -1).split(',').map(value => Number(value.trim()));
    if (values.length >= 2 && values.every(Number.isFinite)) linearEasing = values;
  }
  if (easingBezier === undefined) {
    const match = /^cubic-bezier\(\s*([-+\d.eE]+)\s*,\s*([-+\d.eE]+)\s*,\s*([-+\d.eE]+)\s*,\s*([-+\d.eE]+)\s*\)$/.exec(easing);
    if (match) {
      easingBezier = match.slice(1).map(Number);
      if (!easingBezier.every(Number.isFinite) || easingBezier[0] < 0 || easingBezier[0] > 1
          || easingBezier[2] < 0 || easingBezier[2] > 1) easingBezier = undefined;
    }
  }
  if (linearEasing) easingBezier = null;
  // steps() and linear() with explicit stop positions remain explicit
  // unsupported surfaces rather than being silently approximated.
  if (easingBezier === undefined) throw new TypeError('Unsupported animation easing: ' + easing);
  const fill = ['none', 'forwards', 'backwards', 'both'].includes(options.fill) ? options.fill : 'none';
  const direction = ['normal', 'reverse', 'alternate', 'alternate-reverse'].includes(options.direction)
    ? options.direction : 'normal';
  return { duration, delay, iterations, fill, direction, easing, easingBezier, linearEasing };
}

class KeyframeEffect {
  constructor(target, keyframes, options) {
    if (!(target instanceof Element)) throw new TypeError('KeyframeEffect target must be an Element');
    this.target = target;
    this._keyframes = _normalizeWaapiKeyframes(keyframes);
    this._timing = _normalizeWaapiTiming(options);
  }
  getKeyframes() { return this._keyframes.map(frame => ({ ...frame, computedOffset: frame.offset, easing: 'linear', composite: 'auto' })); }
  getTiming() {
    const timing = this._timing;
    return {
      delay: timing.delay, endDelay: 0, fill: timing.fill,
      iterationStart: 0, iterations: timing.iterations,
      duration: timing.duration, direction: timing.direction, easing: timing.easing,
    };
  }
  getComputedTiming() {
    const animation = this._animation;
    const local = animation ? animation.currentTime : 0;
    const activeDuration = this._timing.duration * this._timing.iterations;
    const endTime = this._timing.delay + activeDuration;
    const progress = activeDuration > 0 ? Math.max(0, Math.min(1, (local - this._timing.delay) / activeDuration)) : null;
    return {
      ...this.getTiming(), activeDuration, endTime, localTime: local,
      progress, currentIteration: progress == null ? null : Math.min(this._timing.iterations, 1),
    };
  }
}

class Animation {
  constructor(effect = null, timeline = globalThis.document?.timeline || null) {
    this.id = '';
    this.effect = effect;
    this.timeline = timeline;
    this.onfinish = null;
    this.oncancel = null;
    this._nativeId = _waapiNextId++;
    this._registered = false;
    this._playState = 'idle';
    this._holdTime = 0;
    this._startTime = null;
    this._finishTimer = null;
    this.ready = Promise.resolve(this);
    this._resetFinishedPromise();
    if (effect) effect._animation = this;
  }
  _resetFinishedPromise() {
    this.finished = new Promise((resolve, reject) => {
      this._resolveFinished = resolve;
      this._rejectFinished = reject;
    });
    // Browser code commonly ignores the rejected cancel promise.
    this.finished.catch(() => {});
  }
  _native(action, value = 0) {
    try {
      const changed = !!Deno.core.ops.op_waapi_control?.(this._nativeId, action, Number(value) || 0);
      if (changed) _domMutationEpoch++;
      return changed;
    }
    catch (_) { return false; }
  }
  _register() {
    if (this._registered || !this.effect) return this._registered;
    const input = {
      id: this._nativeId,
      node: this.effect.target._nid,
      keyframes: this.effect._keyframes,
      ...this.effect._timing,
      // JSON has no Infinity literal and would silently turn it into null.
      // Preserve the Web Animations unrestricted-double value explicitly.
      iterations: this.effect._timing.iterations === Infinity
        ? 0
        : this.effect._timing.iterations,
      iterationsInfinite: this.effect._timing.iterations === Infinity,
    };
    try { this._registered = !!Deno.core.ops.op_waapi_create?.(JSON.stringify(input)); }
    catch (_) { this._registered = false; }
    if (this._registered) {
      _waapiAnimations.add(this);
      _domMutationEpoch++;
    }
    return this._registered;
  }
  _scheduleFinish() {
    if (this._finishTimer != null) clearTimeout(this._finishTimer);
    if (this._playState !== 'running' || !this.effect) return;
    const timing = this.effect._timing;
    if (timing.iterations === Infinity) {
      this._finishTimer = null;
      return;
    }
    const end = Math.max(0, timing.delay + timing.duration * timing.iterations);
    const remaining = Math.max(0, end - this.currentTime);
    this._finishTimer = setTimeout(() => this.finish(), remaining);
  }
  get playState() { return this._playState; }
  get currentTime() {
    if (this._playState === 'running' && this._startTime != null) return Math.max(0, performance.now() - this._startTime);
    return this._holdTime;
  }
  set currentTime(value) {
    const time = Math.max(0, Number(value) || 0);
    this._holdTime = time;
    if (this._playState === 'running') this._startTime = performance.now() - time;
    this._native('currentTime', time);
    this._scheduleFinish();
  }
  get startTime() { return this._startTime; }
  set startTime(value) {
    if (value == null) { this._startTime = null; return; }
    const start = Number(value);
    if (!Number.isFinite(start)) throw new TypeError('Invalid startTime');
    this._startTime = start;
    this._holdTime = Math.max(0, performance.now() - start);
    this._native('currentTime', this._holdTime);
    this._scheduleFinish();
  }
  play() {
    if (!this.effect) return;
    if (this._playState === 'finished' || this._playState === 'idle') {
      this._holdTime = 0;
      if (this._playState === 'finished') this._resetFinishedPromise();
    }
    this._register();
    this._startTime = performance.now() - this._holdTime;
    this._playState = 'running';
    this._native('play');
    this.ready = Promise.resolve(this);
    this._scheduleFinish();
  }
  pause() {
    if (this._playState === 'idle') this._register();
    this._holdTime = this.currentTime;
    this._playState = 'paused';
    this._native('currentTime', this._holdTime);
    this._native('pause');
    if (this._finishTimer != null) clearTimeout(this._finishTimer);
  }
  finish() {
    if (!this.effect) return;
    this._register();
    const timing = this.effect._timing;
    this._holdTime = Math.max(0, timing.delay + timing.duration * timing.iterations);
    this._playState = 'finished';
    this._native('finish');
    if (this._finishTimer != null) clearTimeout(this._finishTimer);
    this._resolveFinished(this);
    const event = new Event('finish');
    this.dispatchEvent(event);
    if (typeof this.onfinish === 'function') { try { this.onfinish.call(this, event); } catch (e) { console.error(e); } }
  }
  cancel() {
    if (this._finishTimer != null) clearTimeout(this._finishTimer);
    this._native('cancel');
    this._registered = false;
    this._playState = 'idle';
    this._holdTime = 0;
    this._startTime = null;
    _waapiAnimations.delete(this);
    this._rejectFinished(new DOMException('The animation was canceled', 'AbortError'));
    const event = new Event('cancel');
    this.dispatchEvent(event);
    if (typeof this.oncancel === 'function') { try { this.oncancel.call(this, event); } catch (e) { console.error(e); } }
    this._resetFinishedPromise();
  }
  reverse() { throw new DOMException('reverse() is not implemented for this animation', 'NotSupportedError'); }
  addEventListener(type, callback, options) { _eventTargetAdd(this, type, callback, options); }
  removeEventListener(type, callback, options) { _eventTargetRemove(this, type, callback, options); }
  dispatchEvent(event) { return _eventTargetDispatch(this, event); }
}

class DocumentTimeline {
  constructor(options = {}) {
    this.originTime = Number(options.originTime) || 0;
  }
  get currentTime() { return performance.now() - this.originTime; }
}

function _animationsForTarget(target) {
  return Array.from(_waapiAnimations).filter(animation => {
    if (animation.effect?.target !== target || animation.playState === 'idle') return false;
    return animation.playState !== 'finished' || animation.effect._timing.fill === 'forwards' || animation.effect._timing.fill === 'both';
  });
}

class Element extends Node {
  constructor(nid) {
    const entry = _customElementConstructionStack[_customElementConstructionStack.length - 1];
    const matchesUpgrade = entry && new.target === entry.constructor;
    const upgrading = matchesUpgrade && !entry.constructed ? entry.element : null;
    super(upgrading ? upgrading._nid : nid);
    if (matchesUpgrade && entry.constructed) {
      throw new TypeError("Custom element is already being constructed");
    }
    if (upgrading) {
      // Keep an already-constructed marker on the stack until the outer class
      // constructor returns. Recursive `new`/`super` calls for the same
      // definition must not steal the element currently being upgraded.
      entry.constructed = true;
      Object.setPrototypeOf(upgrading, new.target.prototype);
      return upgrading;
    }
    this._style = _styleProxy(new CSSStyleDeclaration(this));
  }
  // Element wrappers always back a nodeType-1 node (_wrap/_wrapEl only build an
  // Element for element nodes, and node ids are never freed-and-reused), so this
  // is constant. Overrides Node's dynamic getter to drop one op per nodeType read.
  get nodeType() { return 1; }
  get tagName() {
    // An element's qualified name is immutable for its lifetime. React reads
    // nodeName/tagName repeatedly while hydrating; crossing the native bridge
    // for every comparison adds thousands of calls on modern component trees.
    if (this._tagName !== undefined) return this._tagName;
    this._tagName = _domParse("tag_name", this._nid) || "";
    return this._tagName;
  }
  get nodeName() { return this.tagName; }
  get localName() {
    // The native tree owns the namespace-aware QualName. Reading its local
    // component directly preserves case-sensitive SVG/MathML names such as
    // `linearGradient`; deriving this from HTML's uppercased tagName loses it.
    if (this._lname !== undefined) return this._lname;
    const ln = _domParse("local_name", this._nid)
      || (this.tagName || "").toLowerCase();
    if (ln) this._lname = ln;
    return ln;
  }
  get id() { return this.getAttribute("id") || ""; }
  set id(v) { this.setAttribute("id", v); }
  get className() {
    // SVG elements reflect class as an SVGAnimatedString (.baseVal/.animVal),
    // not a plain string. Anti-fraud sensors read el.className.animVal.
    if (this.namespaceURI === "http://www.w3.org/2000/svg") {
      if (!this._svgClassName) this._svgClassName = new SVGAnimatedString(this, "class");
      return this._svgClassName;
    }
    return this.getAttribute("class") || "";
  }
  set className(v) { this.setAttribute("class", v); }
  get namespaceURI() {
    // createElementNS records the requested namespace on _ns; an empty string
    // maps to the null namespace per spec.
    if (this._ns !== undefined) return this._ns === "" ? null : this._ns;
    // Otherwise use the namespace the HTML tree builder assigned. Foreign
    // content puts the WHOLE <svg>/<math> subtree in that namespace, not just
    // the root, so deriving it from the tag name (the old `localName === "svg"`
    // check) left every descendant looking like HTML and skipped the SVG-only
    // reflections -- notably `get href()`, which then returned a plain string
    // instead of an SVGAnimatedString. An element's namespace never changes,
    // so cache it like _lname.
    if (this._nsCache !== undefined) return this._nsCache;
    let ns = _domParse("namespace_uri", this._nid) || "";
    // Nodes with no element name recorded fall back to the previous heuristic.
    if (!ns) ns = this.localName === "svg" ? "http://www.w3.org/2000/svg" : "http://www.w3.org/1999/xhtml";
    this._nsCache = ns;
    return ns;
  }
  // `inner_html` resolves a <template> to its contents document on the Rust
  // side (issue #463), so this needs no template special case.
  get innerHTML() { return _domParse("inner_html", this._nid) ?? ""; }
  set innerHTML(v) {
    if (this.localName === 'template') {
      this.content.innerHTML = v;
      return;
    }
    // Capture the children that are about to be replaced so we can deliver
    // them as `removedNodes` in the MutationObserver record. Without this,
    // libraries that mutate via `innerHTML =` (jQuery's `.html(s)`, React
    // `dangerouslySetInnerHTML`, vue-style content swaps) silently bypass
    // every MutationObserver subscriber and downstream hydration / polling
    // logic stalls.
    const previousWindowNames = _windowNamedNamesInTree(this);
    // Native fragment replacement bypasses Node.removeChild. Disassociate
    // descendant style sheets before the backing nodes leave the document so
    // retained CSSStyleSheet wrappers cannot keep stale owner/source nodes.
    for (const style of this.querySelectorAll("style")) _detachStyleSheet(style);
    let oldChildren = [];
    let newChildren = [];
    if (globalThis.__mutationObservers?.length) {
      oldChildren = _domParse("child_nodes", this._nid) || [];
    }
    _dom("set_inner_html", this._nid, String(v ?? ""));
    // HTML fragment parsing can introduce IDs without calling the JS
    // setAttribute path. Register those elements for Window named access
    // before script can synchronously read `window.someId`.
    _registerWindowNamedTree(this);
    _reconcileWindowNamedProperties(previousWindowNames);
    if (globalThis.__mutationObservers?.length) {
      newChildren = _domParse("child_nodes", this._nid) || [];
      globalThis.__notifyMutation('childList', this._nid, newChildren, oldChildren);
    }
  }
  get outerHTML() { return _domParse("outer_html", this._nid) ?? ""; }
  get innerText() { return this.textContent; }
  set innerText(v) { this.textContent = v; }
  get children() {
    const ids = _domParse("element_children", this._nid) || [];
    return HTMLCollection._from(ids.map(_wrapEl).filter(Boolean));
  }
  get content() {
    // <template>.content is a DocumentFragment; <meta>.content reflects
    // the content attribute (read/write per spec). Next.js' next/head
    // iterates <meta> tags and sets .content during hydration, which
    // threw with the previous getter-only stub and put React into an
    // infinite retry loop (issue #210).
    const tag = this.localName;
    if (tag === 'template') {
      // Back the fragment with the node's real template contents (issue #463).
      // The parser stores template children in a separate contents document
      // instead of under the element, so without this the getter handed back a
      // fabricated empty fragment and the parsed markup was unreachable.
      // `template_contents` allocates one on demand for created templates.
      const nid = +_dom("template_contents", this._nid);
      if (nid >= 0) {
        // Cache by node id so `.content` keeps a stable identity across reads —
        // frameworks stash the fragment and compare it later.
        if (!_cache.has(nid)) _cache.set(nid, new DocumentFragment(nid));
        const content = _cache.get(nid);
        content._fragmentContext = 'template';
        return content;
      }
      if (!this._templateContent) {
        this._templateContent = document.createDocumentFragment();
        this._templateContent._fragmentContext = 'template';
      }
      return this._templateContent;
    }
    if (tag === 'meta') return this.getAttribute('content') || '';
    return undefined;
  }
  set content(v) {
    if (this.localName === 'meta') {
      this.setAttribute('content', v == null ? '' : String(v));
    }
  }
  get childElementCount() { return this.children.length; }
  get firstElementChild() { return this.children[0] || null; }
  get lastElementChild() { const ch = this.children; return ch[ch.length-1] || null; }
  get nextElementSibling() { let s = this.nextSibling; while(s && s.nodeType !== 1) s = s.nextSibling; return s; }
  get previousElementSibling() { let s = this.previousSibling; while(s && s.nodeType !== 1) s = s.previousSibling; return s; }
  get classList() {
    if (!this._classList) this._classList = new DOMTokenList(this, "class");
    return this._classList;
  }
  get relList() {
    const ns = this.namespaceURI, ln = this.localName;
    const ok = (ns === "http://www.w3.org/2000/svg" && ln === "a") ||
               (ns === "http://www.w3.org/1999/xhtml" && (ln === "a" || ln === "area" || ln === "link"));
    if (!ok) return undefined;
    // relList has supported tokens, so relList.supports(x) returns a boolean
    // rather than throwing. Vite's modulepreload polyfill runs
    // link.relList.supports('modulepreload') at the top of every bundle; a
    // throw there aborts the whole module and the SPA renders blank.
    if (!this._relList) this._relList = new DOMTokenList(this, "rel", ["alternate","dns-prefetch","icon","manifest","modulepreload","next","pingback","preconnect","prefetch","preload","prev","search","stylesheet"]);
    return this._relList;
  }
  get sandbox() {
    if (this.namespaceURI !== "http://www.w3.org/1999/xhtml" || this.localName !== "iframe") return undefined;
    if (!this._sandboxList) this._sandboxList = new DOMTokenList(this, "sandbox", ["allow-downloads","allow-forms","allow-modals","allow-orientation-lock","allow-pointer-lock","allow-popups","allow-popups-to-escape-sandbox","allow-presentation","allow-same-origin","allow-scripts","allow-top-navigation","allow-top-navigation-by-user-activation","allow-top-navigation-to-custom-protocols"]);
    return this._sandboxList;
  }
  get sizes() {
    if (this.namespaceURI !== "http://www.w3.org/1999/xhtml" || this.localName !== "link") return undefined;
    if (!this._sizesList) this._sizesList = new DOMTokenList(this, "sizes");
    return this._sizesList;
  }
  get htmlFor() {
    if (this.namespaceURI !== "http://www.w3.org/1999/xhtml") return undefined;
    const ln = this.localName;
    if (ln === "output") {
      if (!this._htmlForList) this._htmlForList = new DOMTokenList(this, "for");
      return this._htmlForList;
    }
    if (ln === "label") return this.getAttribute("for") || "";
    return undefined;
  }
  set htmlFor(v) {
    if (this.namespaceURI === "http://www.w3.org/1999/xhtml" && this.localName === "label") {
      this.setAttribute("for", String(v));
    }
  }
  get style() { return this._style; }
  set style(v) { if (typeof v === "string") this._style.cssText = v; }
  getAttribute(n) {
    n = _htmlAttrName(this, n);
    // Script-created elements start with a provably empty attribute set. Keep
    // that small null-namespace map coherent through the ordinary mutation
    // APIs so React's write-then-read reflection does not cross the bridge.
    if (this._nullNamespaceAttrs instanceof Map) {
      return this._nullNamespaceAttrs.has(n)
        ? this._nullNamespaceAttrs.get(n)
        : null;
    }
    return _domParse("get_attribute", this._nid, n);
  }
  setAttribute(n, v) {
    n = _htmlAttrName(this, n);
    const popoverPrev = (n === "popover") ? this.popover : undefined;
    const previousWindowName = (n === "id" || n === "name")
      ? this.getAttribute(n)
      : null;
    const value = String(v);
    _dom("set_attribute", this._nid, n + "\0" + value);
    if (this._nullNamespaceAttrs instanceof Map) {
      this._nullNamespaceAttrs.set(n, value);
    }
    if (n === "id" || (n === "name" && _windowNameEligibleElement(this))) {
      if (this.getRootNode() === globalThis.document) {
        _ensureWindowNamedProperty(value);
      }
      if (previousWindowName && previousWindowName !== value) {
        _reconcileWindowNamedProperty(previousWindowName);
      }
    }
    if (n === "style") this._style._replaceFromAttribute(value);
    if (popoverPrev !== undefined) this._popoverTypeMaybeChanged(popoverPrev);
    if (globalThis.__mutationObservers?.length) globalThis.__notifyMutation('attributes', this._nid, [], [], n);
    if (this.localName === "source"
        && (n === "srcset" || n === "sizes" || n === "media" || n === "type")) {
      const picture = this.parentElement;
      const image = picture && picture.localName === "picture"
        ? picture.querySelector("img")
        : null;
      if (image && typeof image._imageSourceChanged === "function") {
        image._imageSourceChanged();
      }
    }
  }
  setAttributeNS(ns, n, v) {
    ns = ns == null || ns === '' ? '' : String(ns);
    n = String(n);
    const value = String(v);
    _ns_validateQualifiedName(ns, n);
    _dom("set_attribute_ns", this._nid, ns + "\0" + n + "\0" + value);
    // Namespace-aware writes can replace an attribute by namespace/local name
    // while changing its qualified name. Fall back to native reads afterwards
    // instead of maintaining a second, subtly different key space here.
    this._nullNamespaceAttrs = null;
    if (ns === "" && n === "style") this._style._replaceFromAttribute(value);
  }
  removeAttribute(n) {
    n = _htmlAttrName(this, n);
    const popoverPrev = (n === "popover") ? this.popover : undefined;
    const previousWindowName = (n === "id" || n === "name")
      ? this.getAttribute(n)
      : null;
    _dom("remove_attribute", this._nid, n);
    if (this._nullNamespaceAttrs instanceof Map) {
      this._nullNamespaceAttrs.delete(n);
    }
    if (previousWindowName
        && (n === "id" || (n === "name" && _windowNameEligibleElement(this)))) {
      _reconcileWindowNamedProperty(previousWindowName);
    }
    if (n === "style") this._style._replaceFromAttribute("");
    if (popoverPrev !== undefined) this._popoverTypeMaybeChanged(popoverPrev);
    if (this.localName === "source"
        && (n === "srcset" || n === "sizes" || n === "media" || n === "type")) {
      const picture = this.parentElement;
      const image = picture && picture.localName === "picture"
        ? picture.querySelector("img")
        : null;
      if (image && typeof image._imageSourceChanged === "function") {
        image._imageSourceChanged();
      }
    }
  }
  removeAttributeNS(ns, n) {
    ns = String(ns == null ? "" : ns);
    n = String(n);
    _dom("remove_attribute_ns", this._nid, ns + "\0" + n);
    this._nullNamespaceAttrs = null;
    if (ns === "" && n === "style") this._style._replaceFromAttribute("");
  }
  hasAttribute(n) { return this.getAttribute(n) !== null; }
  hasAttributes() { return this.attributes.length > 0; }
  getAttributeNames() { return _domParse("attribute_names", this._nid) || []; }
  get attributes() {
    if (!this._attributes) this._attributes = new NamedNodeMap(this);
    return this._attributes;
  }
  getAttributeNS(ns, n) { return _domParse("get_attribute_ns", this._nid, String(ns == null ? "" : ns) + "\0" + String(n)); }
  querySelector(s) { return _wrapEl(+_dom("query_selector_scoped", this._nid, s)); }
  querySelectorAll(s) {
    const ids = _domParse("query_selector_all_scoped", this._nid, s) || [];
    return _nodeList(ids.map(_wrapEl).filter(Boolean));
  }
  getElementsByTagName(t) { return HTMLCollection._from(this.querySelectorAll(t)); }
  getElementsByClassName(c) { return _getElementsByClassName(this, c); }
  matches(s) {
    // :popover-open is a JS-observable popover state, not understood by the
    // native selector engine. Handle it here (and strip it from compound
    // selectors so the rest can still be matched natively).
    if (typeof s === "string" && s.indexOf(":popover-open") !== -1) {
      if (this._popoverState !== "showing") return false;
      const rest = s.replace(/:popover-open/g, "").trim();
      if (rest === "") return true;
      return this.matches(rest);
    }
    // :modal is a JS-observable dialog state (a dialog opened via showModal()),
    // not understood by the native selector engine; handle it like :popover-open.
    if (typeof s === "string" && s.indexOf(":modal") !== -1) {
      if (this._dialogModal !== true) return false;
      const rest = s.replace(/:modal/g, "").trim();
      if (rest === "") return true;
      return this.matches(rest);
    }
    return _dom("matches_selector", this._nid, String(s)) === "true";
  }
  closest(s) {
    let el = this;
    while (el) {
      if (el.nodeType === 1 && el.matches && el.matches(s)) return el;
      el = el.parentNode;
    }
    return null;
  }
  insertAdjacentHTML(position, html) {
    // Position is matched ASCII-case-insensitively; an unknown value throws
    // SyntaxError (both were silent no-ops before). Sibling insertions parse
    // against the parent's context, child insertions against this element, so
    // table/select fragments keep the right parsing context (_parseHTMLFragment).
    const pos = String(position).toLowerCase();
    const parent = this.parentNode;
    const context = (pos === 'beforebegin' || pos === 'afterend') ? parent : this;
    switch (pos) {
      case 'beforebegin':
        if (parent) for (const n of _parseHTMLFragment(html, context)) parent.insertBefore(n, this);
        break;
      case 'afterbegin': {
        const first = this.firstChild;
        for (const n of _parseHTMLFragment(html, context)) this.insertBefore(n, first);
        break;
      }
      case 'beforeend':
        for (const n of _parseHTMLFragment(html, context)) this.appendChild(n);
        break;
      case 'afterend':
        if (parent) { const next = this.nextSibling; for (const n of _parseHTMLFragment(html, context)) parent.insertBefore(n, next); }
        break;
      default:
        throw new DOMException(
          "Failed to execute 'insertAdjacentHTML' on 'Element': The value provided ('" + position + "') is not one of 'beforeBegin', 'afterBegin', 'beforeEnd', or 'afterEnd'.",
          "SyntaxError"
        );
    }
  }
  // Like insertAdjacentHTML but inserts a Text node instead of parsing markup,
  // so the content stays literal.
  insertAdjacentText(position, text) {
    const parent = this.parentNode;
    const node = document.createTextNode(String(text));
    switch (String(position).toLowerCase()) {
      case 'beforebegin':
        if (parent) parent.insertBefore(node, this);
        break;
      case 'afterbegin':
        this.insertBefore(node, this.firstChild);
        break;
      case 'beforeend':
        this.appendChild(node);
        break;
      case 'afterend':
        if (parent) parent.insertBefore(node, this.nextSibling);
        break;
    }
  }
  // Returns the inserted element, or null for beforebegin/afterend when this
  // element has no parent.
  insertAdjacentElement(position, element) {
    const parent = this.parentNode;
    switch (String(position).toLowerCase()) {
      case 'beforebegin':
        if (!parent) return null;
        parent.insertBefore(element, this);
        return element;
      case 'afterbegin':
        this.insertBefore(element, this.firstChild);
        return element;
      case 'beforeend':
        this.appendChild(element);
        return element;
      case 'afterend':
        if (!parent) return null;
        parent.insertBefore(element, this.nextSibling);
        return element;
    }
    return null;
  }
  addEventListener(type, handler, opts) {
    const key = this._nid;
    if (!_eventRegistry[key]) _eventRegistry[key] = {};
    if (!_eventRegistry[key][type]) _eventRegistry[key][type] = [];
    _eventRegistry[key][type].push(handler);
  }
  removeEventListener(type, handler) {
    const key = this._nid;
    if (_eventRegistry[key] && _eventRegistry[key][type]) {
      _eventRegistry[key][type] = _eventRegistry[key][type].filter(h => h !== handler);
    }
  }
  dispatchEvent(event) {
    if (!event) return true;
    if (!event.target) event.target = this;
    event.currentTarget = this;
    // Spec: inline `onclick="..."` content attributes are event handlers
    // for the matching event type. Fire them alongside any
    // addEventListener handlers. Also honor the IDL property
    // `el.onclick = fn` if set. Without this, b.click() never invokes
    // the inline handler and forms with onsubmit / buttons with onclick
    // are silently dead.
    const handlerName = 'on' + event.type;
    const inlineFn = this[handlerName] || this._resolveInlineHandler(handlerName);
    if (typeof inlineFn === 'function') {
      try {
        const ret = inlineFn.call(this, event);
        if (ret === false) event.preventDefault();
      } catch(e) { console.error(e); }
    }
    const handlers = (_eventRegistry[this._nid] || {})[event.type] || [];
    for (const h of handlers) {
      try { h.call(this, event); } catch(e) { console.error(e); }
      if (event._immediatePropagationStopped) break;
    }
    if (event.bubbles && !event._propagationStopped && this.parentNode) {
      this.parentNode.dispatchEvent(event);
    }
    return !event.defaultPrevented;
  }
  _resolveInlineHandler(name) {
    // name = 'onclick' / 'onsubmit' / etc. Compile the content attribute
    // as a function body on first read and cache it on the instance.
    const cache = this.__inlineHandlerCache || (this.__inlineHandlerCache = {});
    if (Object.prototype.hasOwnProperty.call(cache, name)) return cache[name];
    const src = this.getAttribute && this.getAttribute(name);
    if (!src) { cache[name] = null; return null; }
    try {
      cache[name] = new Function('event', src);
    } catch (e) {
      cache[name] = null;
    }
    return cache[name];
  }
  click() {
    const cancelled = !this.dispatchEvent(new MouseEvent("click", {bubbles: true, cancelable: true}));
    if (!cancelled) {
      const link = this.tagName === 'A' ? this : (this.closest ? this.closest('a[href]') : null);
      if (link) {
        const href = link.getAttribute('href');
        if (href && !href.startsWith('#') && !href.startsWith('javascript:')) {
          location.assign(href);
          return;
        }
      }
      // Same predicate requestSubmit validates against, so an internal click
      // can never hand it a submitter it would reject. Also matches the CDP
      // click path in input.rs, which already treats <input type=image> as a
      // submit button.
      if (_isSubmitButton(this)) {
        const form = this.closest ? this.closest('form') : null;
        // A real submit-button click fires the cancelable submit event, so use
        // requestSubmit() (not the plain submit() method, which now bypasses it).
        if (form && typeof form.requestSubmit === 'function') {
          form.requestSubmit(this);
        } else if (form && typeof form.submit === 'function') {
          form.submit(this);
        }
      }
    }
  }
  focus() { globalThis.__obscura_focused = this; globalThis.__obscura_click_target = this; }
  blur() { if (globalThis.__obscura_focused === this) globalThis.__obscura_focused = null; }

  // --- Popover API (HTML "popover") ---------------------------------------
  // Read the popover content attribute case-insensitively. The HTML parser
  // lowercases attribute names, but runtime setAttribute("PoPoVeR", ...)
  // preserves case, and the IDL reflection matches the name ASCII-case-
  // insensitively. Returns the raw stored string, or null if absent.
  _popoverAttrValue() {
    const v = this.getAttribute("popover");
    if (v !== null) return v;
    const names = _domParse("attribute_names", this._nid) || [];
    for (let i = 0; i < names.length; i++) {
      if (names[i].toLowerCase() === "popover") return this.getAttribute(names[i]);
    }
    return null;
  }
  // The reflected (effective) popover type: null (No Popover), "auto",
  // "hint", or "manual". Empty string maps to "auto"; any non-keyword value
  // (invalid) maps to "manual".
  get popover() {
    const raw = this._popoverAttrValue();
    if (raw === null) return null;
    const v = String(raw).toLowerCase();
    if (v === "auto" || v === "hint" || v === "manual") return v;
    if (v === "") return "auto";
    return "manual";
  }
  set popover(value) {
    if (value === null || value === undefined) { this._popoverRemoveAttr(); return; }
    this.setAttribute("popover", String(value));
  }
  _popoverRemoveAttr() {
    if (this.getAttribute("popover") !== null) { this.removeAttribute("popover"); return; }
    const names = _domParse("attribute_names", this._nid) || [];
    for (let i = 0; i < names.length; i++) {
      if (names[i].toLowerCase() === "popover") { this.removeAttribute(names[i]); return; }
    }
  }
  // "check popover validity". expectedToBeShowing is true for hide, false for
  // show. Throws NotSupportedError when there is no valid popover type, and
  // InvalidStateError when the element is not connected; returns false (no
  // throw) when the current state does not match expectedToBeShowing.
  _checkPopoverValidity(expectedToBeShowing) {
    if (this.popover === null) throw new DOMException("Not supported on elements that don't have a valid value for the popover attribute", "NotSupportedError");
    const showing = this._popoverState === "showing";
    if ((expectedToBeShowing && !showing) || (!expectedToBeShowing && showing)) return false;
    if (!this.isConnected) throw new DOMException("Invalid on popover elements which aren't connected", "InvalidStateError");
    return true;
  }
  showPopover() {
    if (!this._checkPopoverValidity(/*expectedToBeShowing*/false)) return;
    const beforeEvent = new ToggleEvent("beforetoggle", { cancelable: true, oldState: "closed", newState: "open" });
    if (!this.dispatchEvent(beforeEvent)) return;
    // The beforetoggle handler may have changed our type or shown us; re-check.
    if (!this._checkPopoverValidity(/*expectedToBeShowing*/false)) return;
    this._popoverState = "showing";
    const target = this;
    setTimeout(() => { try { target.dispatchEvent(new ToggleEvent("toggle", { oldState: "closed", newState: "open" })); } catch (e) {} }, 0);
  }
  hidePopover() {
    if (!this._checkPopoverValidity(/*expectedToBeShowing*/true)) return;
    this.dispatchEvent(new ToggleEvent("beforetoggle", { oldState: "open", newState: "closed" }));
    this._popoverState = "hidden";
    const target = this;
    setTimeout(() => { try { target.dispatchEvent(new ToggleEvent("toggle", { oldState: "open", newState: "closed" })); } catch (e) {} }, 0);
  }
  togglePopover(force) {
    let options = force;
    if (options && typeof options === "object") force = options.force;
    const showing = this._popoverState === "showing";
    if (showing && (force === undefined || force === null || force === false)) {
      this.hidePopover();
    } else if (force === undefined || force === null || force === true) {
      this.showPopover();
    }
    return this._popoverState === "showing";
  }
  // Called from setAttribute/removeAttribute/IDL setter when the popover
  // attribute may have changed. If the effective type changed while showing,
  // hide the popover (firing the hide events) per the HTML spec.
  _popoverTypeMaybeChanged(prevType) {
    const newType = this.popover;
    if (this._popoverState === "showing" && prevType !== newType) {
      // Hide directly. Do not call hidePopover(): it re-validates against the
      // popover attribute, which may now be removed (No Popover), and would
      // throw NotSupportedError. This mirrors the spec hide with throw=false.
      this.dispatchEvent(new ToggleEvent("beforetoggle", { oldState: "open", newState: "closed" }));
      this._popoverState = "hidden";
      const target = this;
      setTimeout(() => { try { target.dispatchEvent(new ToggleEvent("toggle", { oldState: "open", newState: "closed" })); } catch (e) {} }, 0);
    }
  }
  // HTMLDialogElement members (live on Element.prototype like popover/input;
  // meaningful only when localName === 'dialog'). Modal top-layer/focus/render
  // is layout (out of scope); the open state, returnValue, and beforetoggle/
  // toggle/close/cancel events are JS-observable and implemented here.
  get open() { return this.hasAttribute('open'); }
  set open(v) { if (v) { if (!this.hasAttribute('open')) this.setAttribute('open', ''); } else if (this.hasAttribute('open')) { this.removeAttribute('open'); this._dialogModal = false; } }
  get returnValue() { return this._returnValue != null ? this._returnValue : ''; }
  set returnValue(v) { this._returnValue = String(v); }
  get oncancel() { return this._oncancel || null; }
  set oncancel(f) { this._oncancel = typeof f === 'function' ? f : null; }
  get onclose() { return this._onclose || null; }
  set onclose(f) { this._onclose = typeof f === 'function' ? f : null; }
  get closedBy() { const v = (this.getAttribute('closedby') || '').toLowerCase(); return (v === 'any' || v === 'closerequest' || v === 'none') ? v : 'auto'; }
  set closedBy(v) { this.setAttribute('closedby', String(v)); }
  show() {
    if (this.hasAttribute('open')) { if (this._dialogModal) throw new DOMException("The dialog is already open as a modal dialog.", "InvalidStateError"); return; }
    const before = new ToggleEvent("beforetoggle", { cancelable: true, oldState: "closed", newState: "open" });
    if (!this.dispatchEvent(before)) return;
    if (this.hasAttribute('open')) return;
    this.setAttribute('open', ''); this._dialogModal = false;
    const self = this; setTimeout(() => { try { self.dispatchEvent(new ToggleEvent("toggle", { oldState: "closed", newState: "open" })); } catch (e) {} }, 0);
  }
  showModal() {
    if (this.hasAttribute('open')) throw new DOMException("The dialog is already open.", "InvalidStateError");
    if (!this.isConnected) throw new DOMException("The dialog is not connected to a document.", "InvalidStateError");
    const before = new ToggleEvent("beforetoggle", { cancelable: true, oldState: "closed", newState: "open" });
    if (!this.dispatchEvent(before)) return;
    if (this.hasAttribute('open')) return;
    this.setAttribute('open', ''); this._dialogModal = true;
    const self = this; setTimeout(() => { try { self.dispatchEvent(new ToggleEvent("toggle", { oldState: "closed", newState: "open" })); } catch (e) {} }, 0);
  }
  _dialogClose(result, fireClose) {
    if (!this.hasAttribute('open')) return;
    this.dispatchEvent(new ToggleEvent("beforetoggle", { oldState: "open", newState: "closed" }));
    this.removeAttribute('open'); this._dialogModal = false;
    if (result !== undefined) this._returnValue = String(result);
    const self = this;
    setTimeout(() => { try { self.dispatchEvent(new ToggleEvent("toggle", { oldState: "open", newState: "closed" })); } catch (e) {} }, 0);
    if (fireClose) setTimeout(() => { try { self.dispatchEvent(new Event('close', { bubbles: false, cancelable: false })); } catch (e) {} }, 0);
  }
  close(result) { this._dialogClose(result, true); }
  requestClose(result) {
    if (!this.hasAttribute('open')) return;
    if (this._dialogCancelFiring) return; // no re-entrant cancel
    this._dialogCancelFiring = true;
    let canceled = false;
    try { const ev = new Event('cancel', { bubbles: false, cancelable: true }); this.dispatchEvent(ev); canceled = ev.defaultPrevented; }
    finally { this._dialogCancelFiring = false; }
    if (canceled) return;
    this._dialogClose(result, true);
  }
  attachInternals() {
    const reg = (typeof customElements !== 'undefined' && customElements._registry) ? customElements._registry : null;
    if (!reg || !reg.get(this.localName)) throw new DOMException("Failed to execute 'attachInternals' on 'HTMLElement': Unable to attach ElementInternals to non-custom elements.", "NotSupportedError");
    if (this.getAttribute('is')) throw new DOMException("Failed to execute 'attachInternals' on 'HTMLElement': Unable to attach ElementInternals to a customized built-in element.", "NotSupportedError");
    if (this._internalsAttached) throw new DOMException("Failed to execute 'attachInternals' on 'HTMLElement': ElementInternals for the specified element was already attached.", "NotSupportedError");
    this._internalsAttached = true;
    return new ElementInternals(this);
  }
  get value() {
    const tag = this.localName;
    if (tag === 'select') {
      // Selected option wins; otherwise first option (HTML default).
      const opts = this.querySelectorAll('option');
      for (let i = 0; i < opts.length; i++) {
        if (opts[i].selected) {
          return opts[i].getAttribute('value') !== null ? opts[i].getAttribute('value') : opts[i].textContent;
        }
      }
      if (opts.length) return opts[0].getAttribute('value') !== null ? opts[0].getAttribute('value') : opts[0].textContent;
      return '';
    }
    if (_formValues[this._nid] !== undefined) return _formValues[this._nid];
    if (tag === 'textarea') return this.textContent;
    if (tag === 'option') {
      const attr = this.getAttribute('value');
      return attr !== null ? attr : this.textContent;
    }
    if (tag === 'input') {
      const itype = (this.getAttribute('type') || '').toLowerCase();
      if (itype === 'checkbox' || itype === 'radio') {
        // A checkbox/radio with no value attribute defaults to "on" in a real
        // browser, not the empty string.
        const attr = this.getAttribute('value');
        return attr !== null ? attr : 'on';
      }
      if (itype === 'file') {
        // Chrome exposes a file input's value as C:\fakepath\<first filename>.
        return (this._files && this._files.length) ? ('C:\\fakepath\\' + this._files[0].name) : '';
      }
    }
    return this.getAttribute("value") || "";
  }
  // FileList for <input type=file>, populated by DOM.setFileInputFiles (Puppeteer
  // uploadFile / Playwright setInputFiles). null for non-file inputs, matching
  // the DOM. See __obscura_setInputFiles (issue #359).
  get files() {
    if (this.localName !== 'input') return undefined;
    if ((this.getAttribute('type') || '').toLowerCase() !== 'file') return null;
    return this._files || _emptyFileList();
  }
  set value(v) {
    const tag = this.localName;
    if (tag === 'option') {
      this.setAttribute('value', String(v));
      return;
    }
    if (tag === 'select') {
      // Set selected on matching option, clear on others. Puppeteer's
      // page.select(selector, value) round-trips through this setter.
      const wanted = String(v);
      const opts = this.querySelectorAll('option');
      let matched = false;
      for (let i = 0; i < opts.length; i++) {
        const attrV = opts[i].getAttribute('value');
        const optVal = attrV !== null ? attrV : opts[i].textContent;
        if (optVal === wanted) { opts[i].selected = true; matched = true; }
        else { opts[i].selected = false; }
      }
      if (matched) try { this.dispatchEvent(new Event('change', { bubbles: true })); } catch (e) {}
      return;
    }
    _formValues[this._nid] = String(v);
    if (tag === 'textarea') {
      this.textContent = String(v);
    }
  }
  get min() { return this.getAttribute('min') || ''; }
  set min(v) { this.setAttribute('min', v); }
  get max() { return this.getAttribute('max') || ''; }
  set max(v) { this.setAttribute('max', v); }
  get step() { return this.getAttribute('step') || ''; }
  set step(v) { this.setAttribute('step', v); }
  _inputType() { return this.localName === 'input' ? (this.getAttribute('type') || 'text').toLowerCase() : ''; }
  get valueAsNumber() {
    const t = this._inputType();
    if (!_INPUT_NUM_TYPES[t]) return NaN;
    if (t === 'range') {
      let minN = _inputParseNumber('range', this.getAttribute('min')); if (isNaN(minN)) minN = 0;
      let maxN = _inputParseNumber('range', this.getAttribute('max')); if (isNaN(maxN)) maxN = 100;
      if (maxN < minN) maxN = minN;
      const v = _inputParseNumber('range', this.value);
      let n = isNaN(v) ? (minN + (maxN - minN) / 2) : v;
      if (n < minN) n = minN; if (n > maxN) n = maxN;
      return n;
    }
    return _inputParseNumber(t, this.value);
  }
  set valueAsNumber(n) {
    const t = this._inputType();
    if (!_INPUT_NUM_TYPES[t]) throw new DOMException("Failed to set the 'valueAsNumber' property on 'HTMLInputElement': This input element does not support Number values.", 'InvalidStateError');
    n = Number(n);
    if (isNaN(n)) { this.value = ''; return; }
    if (!isFinite(n)) throw new TypeError("Failed to set the 'valueAsNumber' property on 'HTMLInputElement': The value provided is infinite.");
    this.value = _inputFormatNumber(t, n);
  }
  get valueAsDate() {
    const t = this._inputType();
    if (!_INPUT_DATE_TYPES[t]) return null;
    const n = _inputParseNumber(t, this.value);
    if (isNaN(n)) return null;
    if (t === 'month') { const y = 1970 + Math.floor(n / 12); const mo = ((n % 12) + 12) % 12; return new Date(Date.UTC(y, mo, 1)); }
    return new Date(n);
  }
  set valueAsDate(d) {
    const t = this._inputType();
    if (!_INPUT_DATE_TYPES[t]) throw new DOMException("Failed to set the 'valueAsDate' property on 'HTMLInputElement': This input element does not support Date values.", 'InvalidStateError');
    if (d === null) { this.value = ''; return; }
    if (!(d instanceof Date)) throw new TypeError("Failed to set the 'valueAsDate' property on 'HTMLInputElement': The provided value is not a Date.");
    const ms = d.getTime();
    if (isNaN(ms)) { this.value = ''; return; }
    if (t === 'month') { this.value = _inputFormatNumber('month', (d.getUTCFullYear() - 1970) * 12 + d.getUTCMonth()); return; }
    this.value = _inputFormatNumber(t, ms);
  }
  stepUp(n) { this._stepBy(n === undefined ? 1 : (n | 0)); }
  stepDown(n) { this._stepBy(-(n === undefined ? 1 : (n | 0))); }
  _stepBy(delta) {
    const t = this._inputType();
    const stepAttr = this.getAttribute('step');
    if (!_INPUT_STEP_SCALE[t] || (stepAttr && stepAttr.trim().toLowerCase() === 'any')) {
      throw new DOMException("Failed to execute 'stepUp' on 'HTMLInputElement': This form element does not have allowed value steps.", 'InvalidStateError');
    }
    const scale = _INPUT_STEP_SCALE[t];
    let stepN = _INPUT_STEP_DEFAULT[t];
    if (stepAttr) { const s = Number(stepAttr); if (isFinite(s) && s > 0) stepN = s; }
    const allowed = stepN * scale;
    const minN = _inputParseNumber(t, this.getAttribute('min'));
    const maxN = _inputParseNumber(t, this.getAttribute('max'));
    const stepBase = isNaN(minN) ? 0 : minN;
    let value = this.valueAsNumber;
    if (isNaN(value)) value = isNaN(minN) ? 0 : minN;
    value += delta * allowed;
    value = stepBase + Math.round((value - stepBase) / allowed) * allowed;
    const effMin = (t === 'range' && isNaN(minN)) ? 0 : minN;
    const effMax = (t === 'range' && isNaN(maxN)) ? 100 : maxN;
    if (!isNaN(effMin) && value < effMin) value = effMin;
    if (!isNaN(effMax) && value > effMax) value = effMax;
    this.value = _inputFormatNumber(t, value);
  }
  get checked() {
    if (_formChecked[this._nid] !== undefined) return _formChecked[this._nid];
    return this.hasAttribute("checked");
  }
  set checked(v) { _formChecked[this._nid] = !!v; }
  get selected() {
    if (this._selected !== undefined) return this._selected;
    return this.hasAttribute("selected");
  }
  set selected(v) {
    this._selected = !!v;
    // Keep the native DOM tree in sync so layout/paint observes live form
    // state after scripts construct or change an option.
    if (this.localName === 'option') {
      if (this._selected) this.setAttribute('selected', '');
      else this.removeAttribute('selected');
    }
  }
  get text() {
    if (['option', 'script', 'title', 'a'].includes(this.localName)) {
      return this.textContent;
    }
    return undefined;
  }
  set text(v) {
    if (['option', 'script', 'title', 'a'].includes(this.localName)) {
      this.textContent = String(v);
      return;
    }
    // Most elements have no platform `text` reflector. Preserve ordinary
    // expando semantics for them even though all HTML element interfaces
    // currently share this prototype.
    Object.defineProperty(this, 'text', {
      value: v,
      writable: true,
      enumerable: true,
      configurable: true
    });
  }
  get disabled() { return this.hasAttribute("disabled"); }
  set disabled(v) { if (v) this.setAttribute("disabled", ""); else this.removeAttribute("disabled"); }
  get type() { return this.getAttribute("type") || (this.localName === "input" ? "text" : ""); }
  set type(v) { this.setAttribute("type", v); }
  get name() { return this.getAttribute("name") || ""; }
  set name(v) { this.setAttribute("name", v); }
  get placeholder() { return this.getAttribute("placeholder") || ""; }
  set placeholder(v) { this.setAttribute("placeholder", v); }
  // For <a>/<area>, href returns the resolved absolute URL (the spec behavior,
  // and what scrapers want). It uses op_url_resolve, which returns just the
  // resolved string, rather than the full-component op the decomposition
  // members use. Other elements reflect the raw attribute.
  get href() {
    const ln = this.localName;
    // SVG href-bearing elements reflect href as an SVGAnimatedString (with the
    // legacy xlink:href as a fallback), not a resolved URL string. Checked
    // before the HTML <a> path because an SVG <a> also has localName 'a'.
    if (this.namespaceURI === "http://www.w3.org/2000/svg" &&
        (ln === 'a' || ln === 'image' || ln === 'use' || ln === 'script' ||
         ln === 'pattern' || ln === 'filter' || ln === 'textPath' || ln === 'mpath' ||
         ln === 'linearGradient' || ln === 'radialGradient' || ln === 'feImage' || ln === 'tref')) {
      if (!this._svgHref) this._svgHref = new SVGAnimatedString(this, "href", "xlink:href");
      return this._svgHref;
    }
    if (ln === 'a' || ln === 'area') {
      const raw = this.getAttribute('href');
      if (raw === null) return '';
      // Legacy-charset document: href must reflect the encoding-override query.
      if (!_docIsUtf8()) { const u = _elemHrefURL(this); return u ? u.href : raw; }
      const r = _urlResolveOp(raw, _anchorBase());
      return r !== null ? r : raw;
    }
    return this.getAttribute("href") || "";
  }
  set href(v) { this.setAttribute("href", v); }
  // HTMLHyperlinkElementUtils / HTMLAnchorElement reflected content
  // attributes. Real-world locale, routing, and analytics code commonly
  // enumerates `[hreflang]` links and reads the IDL property rather than
  // getAttribute(); leaving it undefined aborts the entire component even
  // though the attribute is present in the DOM.
  get hreflang() { return this.getAttribute("hreflang") || ""; }
  set hreflang(v) { this.setAttribute("hreflang", v); }
  get rel() { return this.getAttribute("rel") || ""; }
  set rel(v) { this.setAttribute("rel", v); }
  get target() { return this.getAttribute("target") || ""; }
  set target(v) { this.setAttribute("target", v); }
  get download() { return this.getAttribute("download") || ""; }
  set download(v) { this.setAttribute("download", v); }
  get ping() { return this.getAttribute("ping") || ""; }
  set ping(v) { this.setAttribute("ping", v); }
  get referrerPolicy() { return this.getAttribute("referrerpolicy") || ""; }
  set referrerPolicy(v) { this.setAttribute("referrerpolicy", v); }
  // HTMLHyperlinkElementUtils URL-decomposition members, live on <a>/<area>.
  get protocol() { const u = (this.localName === 'a' || this.localName === 'area') ? _elemHrefURL(this) : null; return u ? u.protocol : ''; }
  set protocol(v) { if (this.localName === 'a' || this.localName === 'area') _setElemHrefPart(this, 'protocol', v); }
  get username() { const u = (this.localName === 'a' || this.localName === 'area') ? _elemHrefURL(this) : null; return u ? u.username : ''; }
  set username(v) { if (this.localName === 'a' || this.localName === 'area') _setElemHrefPart(this, 'username', v); }
  get password() { const u = (this.localName === 'a' || this.localName === 'area') ? _elemHrefURL(this) : null; return u ? u.password : ''; }
  set password(v) { if (this.localName === 'a' || this.localName === 'area') _setElemHrefPart(this, 'password', v); }
  get host() { const u = (this.localName === 'a' || this.localName === 'area') ? _elemHrefURL(this) : null; return u ? u.host : ''; }
  set host(v) { if (this.localName === 'a' || this.localName === 'area') _setElemHrefPart(this, 'host', v); }
  get hostname() { const u = (this.localName === 'a' || this.localName === 'area') ? _elemHrefURL(this) : null; return u ? u.hostname : ''; }
  set hostname(v) { if (this.localName === 'a' || this.localName === 'area') _setElemHrefPart(this, 'hostname', v); }
  get port() { const u = (this.localName === 'a' || this.localName === 'area') ? _elemHrefURL(this) : null; return u ? u.port : ''; }
  set port(v) { if (this.localName === 'a' || this.localName === 'area') _setElemHrefPart(this, 'port', v); }
  get pathname() { const u = (this.localName === 'a' || this.localName === 'area') ? _elemHrefURL(this) : null; return u ? u.pathname : ''; }
  set pathname(v) { if (this.localName === 'a' || this.localName === 'area') _setElemHrefPart(this, 'pathname', v); }
  get search() { const u = (this.localName === 'a' || this.localName === 'area') ? _elemHrefURL(this) : null; return u ? u.search : ''; }
  set search(v) { if (this.localName === 'a' || this.localName === 'area') _setElemHrefPart(this, 'search', v); }
  get hash() { const u = (this.localName === 'a' || this.localName === 'area') ? _elemHrefURL(this) : null; return u ? u.hash : ''; }
  set hash(v) { if (this.localName === 'a' || this.localName === 'area') _setElemHrefPart(this, 'hash', v); }
  get origin() { const u = (this.localName === 'a' || this.localName === 'area') ? _elemHrefURL(this) : null; return u ? u.origin : ''; }
  get src() {
    // IDL reflection: HTMLScriptElement/HTMLImageElement/etc. `.src` returns the
    // resolved absolute URL, not the literal attribute. Loaders that compute their
    // base via `new URL(document.currentScript.src).origin` break on a relative
    // value (issue #255). getAttribute("src") still returns the literal.
    const v = this.getAttribute("src");
    if (!v) return "";
    try { return new URL(v, globalThis.location?.href || "about:blank").href; }
    catch (e) { return v; }
  }
  set src(v) {
    this.setAttribute("src", v);
    if (this.localName === 'iframe' && v && v !== 'about:blank') {
      this._loadIframeSrc(v);
    }
  }
  _loadIframeSrc(url) {
    let fullUrl = url;
    if (!url.includes('://')) {
      try { fullUrl = new URL(url, _domParse("document_url") || "about:blank").href; } catch(e) {}
    }
    // Both the src setter and the parser sweep in __obscura_init reach here, so
    // a frame the page assigned before init must not be fetched a second time.
    if (this._iframeLoadingUrl === fullUrl) return;
    this._iframeLoadingUrl = fullUrl;
    const el = this;
    fetch(fullUrl, {mode: 'no-cors'}).then(async resp => {
      if (resp.ok || resp.type === 'opaque') {
        const html = await resp.text();
        // Hand the document to the host, which gives this frame a realm of its
        // own and runs the scripts that came with it (issue #600). The shim
        // document below stays: it is what the parent reads through
        // contentDocument.
        const box = el.getBoundingClientRect();
        el._frameId = Deno.core.ops.op_frame_document_ready(
          fullUrl, html, Math.round(box.width) || 300, Math.round(box.height) || 150);
        el._iframeDoc = new _IframeDocument(html, fullUrl, el);
        el._iframeWin = new _IframeWindow(el._iframeDoc, fullUrl);
        // Bind the window to the realm the host just queued. This is what makes
        // posting into the frame reach the frame's own listeners, and makes a
        // message coming back out arrive with this window as its `source`.
        el._iframeWin._frameId = el._frameId;
        globalThis.__obscura_frameWindows[el._frameId] = el._iframeWin;
        // Remember the element, so "is this frame still in the page?" can be
        // asked of the element itself. See __obscura_liveFrameIds.
        globalThis.__obscura_frameElements[el._frameId] = el;
      } else {
        el._iframeDoc = new _IframeDocument('<!DOCTYPE html><html><head></head><body></body></html>', fullUrl, el);
        el._iframeWin = new _IframeWindow(el._iframeDoc, fullUrl);
      }

      // Dispatch through the element so the onload property/attribute and any
      // addEventListener('load', ...) listeners all run. Calling el.onload()
      // directly bypasses listeners registered via addEventListener.
      el.dispatchEvent(new Event('load'));
    }).catch(() => {
      el._iframeDoc = new _IframeDocument('<!DOCTYPE html><html><head></head><body></body></html>', fullUrl, el);
      el._iframeWin = new _IframeWindow(el._iframeDoc, fullUrl);

      el.dispatchEvent(new Event('load'));
    });
  }
  get contentDocument() {
    if (this.localName !== 'iframe') return undefined;
    const real = _frameObjectsFor(this);
    if (real?.document) return real.document;
    if (this._iframeDoc) {
      const pageOrigin = (function(){ try { return new URL(_domParse("document_url")).origin; } catch(e) { return ''; } })();
      const iframeOrigin = (function(url){ try { return new URL(url).origin; } catch(e) { return ''; } })(this.src);
      if (pageOrigin === iframeOrigin || this.src === '' || this.src === 'about:blank' || !this.src.includes('://')) {
        return this._iframeDoc;
      }
      return null; // Cross-origin: blocked
    }
    if (!this._iframeDoc) {
      this._iframeDoc = new _IframeDocument('<!DOCTYPE html><html><head></head><body></body></html>', 'about:blank', this);
      this._iframeWin = new _IframeWindow(this._iframeDoc, 'about:blank');
    }
    return this._iframeDoc;
  }
  get contentWindow() {
    if (this.localName !== 'iframe') return undefined;
    if (_frameObjectsFor(this)) {
      const win = _frameWindowFor(this._frameId);
      if (win) return win;
    }
    if (!this._iframeWin) {
      if (this.parentNode === null) return null;
      this.contentDocument;
    }
    return this._iframeWin;
  }
  get action() {
    const action = this.getAttribute("action") || _domParse("document_url") || "";
    try { return new URL(action, _domParse("document_url") || "about:blank").href; } catch(e) { return action; }
  }
  set action(v) { this.setAttribute("action", v); }
  get method() { return this.getAttribute("method") || "get"; }
  set method(v) { this.setAttribute("method", v); }
  get form() {
    let p = this.parentNode;
    while (p && p.localName !== 'form') p = p.parentNode;
    return p;
  }
  get options() {
    if (this.localName !== 'select') return [];
    return HTMLCollection._from(this.querySelectorAll('option'));
  }
  add(item, before = null) {
    if (this.localName !== 'select') {
      throw new TypeError("Illegal invocation");
    }
    if (!item || item.nodeType !== 1
        || (item.localName !== 'option' && item.localName !== 'optgroup')) {
      throw new TypeError("Failed to execute 'add' on 'HTMLSelectElement': parameter 1 is not of type 'HTMLOptionElement' or 'HTMLOptGroupElement'.");
    }
    if (typeof before === 'number') {
      const reference = this.options[before] || null;
      this.insertBefore(item, reference);
    } else if (before == null) {
      this.appendChild(item);
    } else {
      this.insertBefore(item, before);
    }
  }
  get selectedIndex() {
    const opts = this.options;
    for (let i = 0; i < opts.length; i++) {
      if (opts[i].selected || opts[i].hasAttribute('selected')) return i;
    }
    return opts.length ? 0 : -1;
  }
  set selectedIndex(v) {
    const opts = this.options;
    for (let i = 0; i < opts.length; i++) {
      opts[i]._selected = (i === v);
    }
  }
  // Per the HTML spec, the submit() METHOD submits the form WITHOUT firing a
  // cancelable `submit` event — a page's submit listener cannot veto it. Only
  // requestSubmit() and user-initiated submits fire the cancelable event.
  // Conflating the two broke sites whose submit listener preventDefault()s the
  // native submit and then calls form.submit() from a callback (e.g. an
  // invisible-reCAPTCHA data-callback) to actually send the form.
  submit(submitter) {
    this._navigateSubmit(submitter);
  }
  requestSubmit(submitter) {
    // Per spec, a given submitter must be a submit button owned by this form;
    // both checks run before the submit event fires. A missing/null submitter
    // means "submit from the form itself".
    if (submitter !== undefined && submitter !== null) {
      if (!_isSubmitButton(submitter)) {
        throw new TypeError(
          "Failed to execute 'requestSubmit' on 'HTMLFormElement': The specified element is not a submit button."
        );
      }
      if (submitter.form !== this) {
        throw new DOMException(
          "Failed to execute 'requestSubmit' on 'HTMLFormElement': The specified element is not owned by this form element.",
          'NotFoundError'
        );
      }
    }
    const cancelled = !this.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    if (cancelled) return;
    this._navigateSubmit(submitter);
  }
  _navigateSubmit(submitter) {
    const pairs = [];
    const fields = this.querySelectorAll('input, select, textarea');
    for (let i = 0; i < fields.length; i++) {
      const f = fields[i];
      const name = f.getAttribute('name');
      if (!name) continue;
      if (f.getAttribute('disabled') !== null) continue;
      const tag = f.localName;
      const type = (f.getAttribute('type') || '').toLowerCase();
      if ((type === 'checkbox' || type === 'radio') && !f.checked) continue;
      if (type === 'file' || type === 'reset') continue;
      if (type === 'button') continue;
      if (type === 'submit' || tag === 'button') {
        if (submitter && f !== submitter) continue;
        if (!submitter) continue; // default submit: don't include submit button value
      }

      let val;
      if (tag === 'select') {
        const opt = f.querySelector('option[selected]') || f.querySelector('option');
        val = opt ? (opt.getAttribute('value') !== null ? opt.getAttribute('value') : opt.textContent) : '';
      } else if (tag === 'textarea') {
        val = f.value || f.textContent || '';
      } else {
        val = f.value !== undefined ? f.value : (f.getAttribute('value') || '');
      }
      const enc = (s) => encodeURIComponent(s).replace(/%20/g, '+').replace(/!/g, '%21');
      pairs.push(enc(name) + '=' + enc(val));
    }

    const action = this.getAttribute('action') || '';
    const method = (this.getAttribute('method') || 'GET').toUpperCase();
    const baseUrl = globalThis.location?.href || 'about:blank';
    let targetUrl;
    try { targetUrl = new URL(action, baseUrl).href; } catch(e) { targetUrl = action; }

    const encoded = pairs.join('&');
    if (method === 'POST') {
      Deno.core.ops.op_navigate(targetUrl, 'POST', encoded);
    } else {
      const sep = targetUrl.includes('?') ? '&' : '?';
      Deno.core.ops.op_navigate(targetUrl + (encoded ? sep + encoded : ''), 'GET', '');
    }
  }
  reset() {
    this.dispatchEvent(new Event('reset', { bubbles: true }));
  }
  get dataset() {
    if (this._dataset) return this._dataset;
    const el = this;
    const attrFor = (k) => "data-" + _cssCamelToKebab(k);
    // camelCase the part after the `data-` prefix, e.g. data-foo-bar -> fooBar.
    const dataKeys = () => el.getAttributeNames()
      .filter((n) => n.startsWith("data-"))
      .map((n) => _cssKebabToCamel(n.slice(5)));
    this._dataset = new Proxy({}, {
      get(_, k) { if (typeof k !== "string") return undefined; return el.hasAttribute(attrFor(k)) ? el.getAttribute(attrFor(k)) : undefined; },
      set(_, k, v) { el.setAttribute(attrFor(k), String(v)); return true; },
      has(_, k) { return typeof k === "string" && el.hasAttribute(attrFor(k)); },
      deleteProperty(_, k) { if (typeof k === "string") el.removeAttribute(attrFor(k)); return true; },
      ownKeys() { return dataKeys(); },
      getOwnPropertyDescriptor(_, k) {
        if (typeof k === "string" && el.hasAttribute(attrFor(k))) {
          return { value: el.getAttribute(attrFor(k)), writable: true, enumerable: true, configurable: true };
        }
        return undefined;
      },
    });
    return this._dataset;
  }
  get offsetWidth() {
    if (this._isViewportRoot()) return globalThis.innerWidth || 1280;
    return this.getBoundingClientRect().width;
  }
  get offsetHeight() {
    if (this._isViewportRoot()) return globalThis.innerHeight || 720;
    return this.getBoundingClientRect().height;
  }
  get offsetTop() { return this.getBoundingClientRect().top; }
  get offsetLeft() { return this.getBoundingClientRect().left; }
  // In standards mode documentElement exposes viewport client geometry.
  // Puppeteer's #clickableBox clips boxes to those dimensions; returning the
  // non-render fallback 100x20 there makes every element appear off-screen.
  get clientWidth() {
    // In standards mode only the root element exposes the viewport. Body is
    // an ordinary box; treating it as another viewport breaks libraries that
    // measure the page's body or a full-viewport sizing sentinel.
    if (this.tagName === 'HTML') return globalThis.innerWidth || 1280;
    const metrics = this._renderClientMetrics();
    return metrics ? metrics.width : 100;
  }
  get clientHeight() {
    if (this.tagName === 'HTML') return globalThis.innerHeight || 720;
    const metrics = this._renderClientMetrics();
    return metrics ? metrics.height : 20;
  }
  _renderClientMetrics() {
    if (typeof Deno.core.ops.op_layout_geometry !== 'function') return null;
    try {
      const raw = Deno.core.ops.op_layout_geometry(String(this._nid | 0));
      if (!raw) return { width: 0, height: 0 };
      const geometry = JSON.parse(raw);
      if (geometry
          && Number.isFinite(geometry.clientWidth)
          && Number.isFinite(geometry.clientHeight)) {
        // CSSOM View exposes Web IDL longs. The native layout retains
        // subpixel precision for getBoundingClientRect(); client metrics round
        // to whole CSS pixels like Chromium.
        return {
          width: Math.round(Math.max(0, geometry.clientWidth)),
          height: Math.round(Math.max(0, geometry.clientHeight)),
        };
      }
    } catch (_error) {}
    return { width: 0, height: 0 };
  }
  // `undefined` means this is a non-render build. `null` means the render
  // engine is present but this element has no associated CSS box (for
  // example, display:none or a detached element). Keep those states distinct:
  // CSSOM View returns an empty rect list for the latter, while the former
  // deliberately retains Obscura's compatibility geometry.
  _renderBoxGeometry() {
    if (typeof Deno.core.ops.op_layout_geometry !== 'function') return undefined;
    try {
      const raw = Deno.core.ops.op_layout_geometry(String(this._nid | 0));
      if (!raw) return null;
      const geometry = JSON.parse(raw);
      if (geometry
          && Number.isFinite(geometry.x)
          && Number.isFinite(geometry.y)
          && Number.isFinite(geometry.width)
          && Number.isFinite(geometry.height)) {
        return geometry;
      }
    } catch (_error) {}
    return null;
  }
  _rectFromRenderGeometry(geometry) {
    const x = geometry.x, y = geometry.y;
    const width = geometry.width, height = geometry.height;
    const rect = {
      x, y, width, height,
      top: y, right: x + width, bottom: y + height, left: x,
      toJSON() { return this; },
    };
    Object.defineProperty(rect, "__obscuraViewportFixed", {
      value: !!geometry.viewportFixed,
      enumerable: false,
    });
    return rect;
  }
  get scrollWidth() {
    if (this._isViewportRoot()) {
      const metrics = this._renderScrollMetrics();
      return metrics
        ? Math.round(Math.max(0, metrics.scrollWidth || 0))
        : (globalThis.innerWidth || 1280);
    }
    const metrics = this._renderElementScrollMetrics();
    if (metrics !== undefined) {
      return metrics ? Math.round(Math.max(0, metrics.scrollWidth || 0)) : 0;
    }
    return 100;
  }
  get scrollHeight() {
    if (this._isViewportRoot()) {
      const metrics = this._renderScrollMetrics();
      return metrics
        ? Math.round(Math.max(0, metrics.scrollHeight || 0))
        : (globalThis.innerHeight || 720);
    }
    const metrics = this._renderElementScrollMetrics();
    if (metrics !== undefined) {
      return metrics ? Math.round(Math.max(0, metrics.scrollHeight || 0)) : 0;
    }
    return 20;
  }
  _isViewportRoot() {
    const t = this.tagName;
    return t === 'HTML' || t === 'BODY';
  }
  _renderScrollMetrics() {
    if (typeof Deno.core.ops.op_layout_metrics !== 'function') return null;
    try {
      const raw = Deno.core.ops.op_layout_metrics();
      return raw ? JSON.parse(raw) : null;
    } catch (_e) {
      return null;
    }
  }
  _renderElementScrollMetrics() {
    if (typeof Deno.core.ops.op_element_scroll_metrics !== 'function') return undefined;
    try {
      const raw = Deno.core.ops.op_element_scroll_metrics(String(this._nid | 0));
      if (!raw) return null;
      const metrics = JSON.parse(raw);
      return metrics && metrics.hasBox !== false ? metrics : null;
    } catch (_e) {
      return null;
    }
  }
  _renderScrollOffset() {
    if (typeof Deno.core.ops.op_scroll_offset !== 'function') return null;
    try {
      const raw = Deno.core.ops.op_scroll_offset();
      return raw ? JSON.parse(raw) : null;
    } catch (_e) {
      return null;
    }
  }
  _setRenderScroll(x, y) {
    if (typeof Deno.core.ops.op_scroll_to !== 'function') return null;
    try {
      const raw = Deno.core.ops.op_scroll_to(+x || 0, +y || 0);
      return raw ? JSON.parse(raw) : null;
    } catch (_e) {
      return null;
    }
  }
  _setRenderElementScroll(x, y) {
    if (typeof Deno.core.ops.op_element_scroll_to !== 'function') return null;
    try {
      const raw = Deno.core.ops.op_element_scroll_to(String(this._nid | 0), +x || 0, +y || 0);
      return raw ? JSON.parse(raw) : null;
    } catch (_e) {
      return null;
    }
  }
  // Render builds clamp both viewport and element scroll areas against the
  // exact overflow used by geometry and paint. Non-render builds retain the
  // synthetic compatibility state.
  get scrollTop() {
    if (this._isViewportRoot()) {
      const offset = this._renderScrollOffset();
      if (offset) return offset.y || 0;
    } else {
      const metrics = this._renderElementScrollMetrics();
      if (metrics !== undefined) return metrics ? (metrics.y || 0) : 0;
    }
    return this._scrollTop || 0;
  }
  set scrollTop(v) {
    v = +v;
    const nv = Number.isFinite(v) && v > 0 ? v : 0;
    const old = this.scrollTop;
    let actual = nv;
    if (this._isViewportRoot()) {
      const offset = this._renderScrollOffset();
      const updated = offset && this._setRenderScroll(offset.x, nv);
      if (updated) actual = updated.y || 0;
    } else {
      const metrics = this._renderElementScrollMetrics();
      if (metrics !== undefined) {
        actual = metrics ? (metrics.y || 0) : 0;
        const updated = metrics && this._setRenderElementScroll(metrics.x, nv);
        if (updated) actual = updated.y || 0;
      }
    }
    const changed = actual !== old;
    this._scrollTop = actual;
    if (changed && !this._scrollSuppress) this._fireScroll();
    if (changed &&
        typeof globalThis.__obscura_recompute_intersections === "function") {
      // Scrolling changes target positions, not ResizeObserver box sizes.
      globalThis.__obscura_recompute_intersections();
    }
  }
  get scrollLeft() {
    if (this._isViewportRoot()) {
      const offset = this._renderScrollOffset();
      if (offset) return offset.x || 0;
    } else {
      const metrics = this._renderElementScrollMetrics();
      if (metrics !== undefined) return metrics ? (metrics.x || 0) : 0;
    }
    return this._scrollLeft || 0;
  }
  set scrollLeft(v) {
    v = +v;
    const nv = Number.isFinite(v) && v > 0 ? v : 0;
    const old = this.scrollLeft;
    let actual = nv;
    if (this._isViewportRoot()) {
      const offset = this._renderScrollOffset();
      const updated = offset && this._setRenderScroll(nv, offset.y);
      if (updated) actual = updated.x || 0;
    } else {
      const metrics = this._renderElementScrollMetrics();
      if (metrics !== undefined) {
        actual = metrics ? (metrics.x || 0) : 0;
        const updated = metrics && this._setRenderElementScroll(nv, metrics.y);
        if (updated) actual = updated.x || 0;
      }
    }
    const changed = actual !== old;
    this._scrollLeft = actual;
    if (changed && !this._scrollSuppress) this._fireScroll();
    if (changed &&
        typeof globalThis.__obscura_recompute_intersections === "function") {
      globalThis.__obscura_recompute_intersections();
    }
  }
  getBoundingClientRect() {
    globalThis.__obscura_click_target = this;
    // Real layout when the render feature is compiled in: ask the Rust layout
    // cache for this element's border box. The op is absent in the default
    // build, so probe with typeof and fall through to the synthetic rect below.
    const geometry = this._renderBoxGeometry();
    if (geometry !== undefined) {
      if (geometry) return this._rectFromRenderGeometry(geometry);
      // CSSOM View: an element without an associated box has an all-zero
      // bounding rect. Do not leak the non-render 100x20 compatibility cell.
      return {
        x: 0, y: 0, width: 0, height: 0,
        top: 0, right: 0, bottom: 0, left: 0,
        toJSON() { return this; },
      };
    }
    // Default (non-render) builds keep viewport-sized roots. Without this
    // synthetic fallback every hit test against them clips down to a 100x20
    // cell and Document.elementFromPoint cannot recurse into their children.
    if (this._isViewportRoot()) {
      const vw = globalThis.innerWidth || 1280;
      const vh = globalThis.innerHeight || 720;
      return {
        x: 0, y: 0, width: vw, height: vh,
        top: 0, right: vw, bottom: vh, left: 0,
        toJSON() { return this; },
      };
    }
    // No layout engine (default build): synthesize a deterministic position
    // from the node id so Playwright's actionability polling still gets a
    // stable, distinct rect for hit-testing (issue #45).
    // Every nid maps to a unique cell in a 12-column grid for a 1280x720 viewport.
    const VW = 1280, VH = 720, COLS = 12, CW = 100, CH = 20, GX = 110, GY = 30;
    const rowsPerScreen = Math.max(1, Math.floor((VH - 10) / GY));
    const cell = this._nid | 0;
    const col = ((cell * 7) | 0) % COLS;
    const row = (((cell * 13) | 0) >> 0) % rowsPerScreen;
    const x = 10 + col * GX;
    const y = 10 + row * GY;
    return {
      x, y, width: CW, height: CH,
      top: y, right: x + CW, bottom: y + CH, left: x,
      toJSON() { return this; },
    };
  }
  getClientRects() {
    const geometry = this._renderBoxGeometry();
    if (geometry === null) return new DOMRectList([]);
    if (geometry !== undefined) {
      if (Array.isArray(geometry.clientRects)) {
        return new DOMRectList(geometry.clientRects.map(
          rect => this._rectFromRenderGeometry({
            ...rect,
            viewportFixed: geometry.viewportFixed,
          })
        ));
      }
      return new DOMRectList([this._rectFromRenderGeometry(geometry)]);
    }
    return new DOMRectList([this.getBoundingClientRect()]);
  }
  // No layout engine: a stub that always returns true unblocks Playwright's
  // actionability polling. With a real layout we'd check display, visibility,
  // opacity and rect dimensions per spec.
  checkVisibility(opts) { return true; }
  // ARIA reflection properties. Without an accessibility tree we expose the
  // raw aria-* attributes so Playwright's getByRole / getByLabel locators can
  // at least find elements that author them explicitly.
  get role() { return this.getAttribute('role'); }
  set role(v) { if (v == null) this.removeAttribute('role'); else this.setAttribute('role', String(v)); }
  get ariaLabel() { return this.getAttribute('aria-label'); }
  set ariaLabel(v) { if (v == null) this.removeAttribute('aria-label'); else this.setAttribute('aria-label', String(v)); }
  get ariaRoleDescription() { return this.getAttribute('aria-roledescription'); }
  set ariaRoleDescription(v) { if (v == null) this.removeAttribute('aria-roledescription'); else this.setAttribute('aria-roledescription', String(v)); }
  get ariaChecked() { return this.getAttribute('aria-checked'); }
  set ariaChecked(v) { if (v == null) this.removeAttribute('aria-checked'); else this.setAttribute('aria-checked', String(v)); }
  get ariaDisabled() { return this.getAttribute('aria-disabled'); }
  set ariaDisabled(v) { if (v == null) this.removeAttribute('aria-disabled'); else this.setAttribute('aria-disabled', String(v)); }
  get ariaExpanded() { return this.getAttribute('aria-expanded'); }
  set ariaExpanded(v) { if (v == null) this.removeAttribute('aria-expanded'); else this.setAttribute('aria-expanded', String(v)); }
  get ariaHidden() { return this.getAttribute('aria-hidden'); }
  set ariaHidden(v) { if (v == null) this.removeAttribute('aria-hidden'); else this.setAttribute('aria-hidden', String(v)); }
  get ariaSelected() { return this.getAttribute('aria-selected'); }
  set ariaSelected(v) { if (v == null) this.removeAttribute('aria-selected'); else this.setAttribute('aria-selected', String(v)); }
  scrollIntoView(arg) {
    globalThis.__obscura_click_target = this;
    const rect = this.getBoundingClientRect();
    // A viewport-fixed subtree is already expressed in the viewport's
    // coordinate space and cannot be brought closer by moving the document.
    if (rect.__obscuraViewportFixed) return;

    let block = "start", inline = "nearest";
    if (arg === false) block = "end";
    else if (arg && typeof arg === "object") {
      if (["start", "center", "end", "nearest"].includes(arg.block)) block = arg.block;
      if (["start", "center", "end", "nearest"].includes(arg.inline)) inline = arg.inline;
    }
    const currentX = globalThis.scrollX || 0;
    const currentY = globalThis.scrollY || 0;
    const vw = globalThis.innerWidth || 1280;
    const vh = globalThis.innerHeight || 720;
    const align = (mode, start, end, size, viewportSize, current) => {
      if (mode === "start") return current + start;
      if (mode === "center") return current + start - (viewportSize - size) / 2;
      if (mode === "end") return current + end - viewportSize;
      // CSSOM View's nearest alignment: do nothing when fully visible or when
      // the box spans both viewport edges; otherwise move the closer edge in.
      if ((start >= 0 && end <= viewportSize) || (start < 0 && end > viewportSize)) {
        return current;
      }
      if (start < 0) return current + start;
      if (end > viewportSize) return current + end - viewportSize;
      return current;
    };
    const left = align(inline, rect.left, rect.right, rect.width, vw, currentX);
    const top = align(block, rect.top, rect.bottom, rect.height, vh, currentY);
    globalThis.scrollTo({ left, top, behavior: arg && arg.behavior });
  }
  // scrollTo/scrollBy/scroll accept either (x, y) or a ScrollToOptions object.
  // The setters fire a scroll event of their own, so suppress the per-axis ones
  // here and emit a single event for the whole movement, the way a real browser
  // coalesces one scroll per scroll operation rather than one per axis.
  scrollTo(x, y) {
    let left, top;
    if (x !== null && typeof x === 'object') { left = x.left; top = x.top; }
    else { left = x; top = y; }
    const oldLeft = this.scrollLeft, oldTop = this.scrollTop;
    let native = false, updated = null;
    if (this._isViewportRoot()) {
      const offset = this._renderScrollOffset();
      if (offset) {
        native = true;
        updated = this._setRenderScroll(
          left === undefined ? offset.x : (+left || 0),
          top === undefined ? offset.y : (+top || 0),
        );
      }
    } else {
      const metrics = this._renderElementScrollMetrics();
      if (metrics !== undefined) {
        native = true;
        updated = metrics
          ? this._setRenderElementScroll(
              left === undefined ? metrics.x : (+left || 0),
              top === undefined ? metrics.y : (+top || 0),
            )
          : { x: 0, y: 0 };
      }
    }
    if (native) {
      const actualLeft = updated ? (updated.x || 0) : oldLeft;
      const actualTop = updated ? (updated.y || 0) : oldTop;
      this._scrollLeft = actualLeft;
      this._scrollTop = actualTop;
      if (actualLeft !== oldLeft || actualTop !== oldTop) {
        if (typeof globalThis.__obscura_recompute_intersections === "function") {
          globalThis.__obscura_recompute_intersections();
        }
        this._fireScroll();
      }
      return;
    }
    this._scrollSuppress = true;
    if (left !== undefined) this.scrollLeft = +left || 0;
    if (top !== undefined) this.scrollTop = +top || 0;
    this._scrollSuppress = false;
    if (this.scrollLeft !== oldLeft || this.scrollTop !== oldTop) this._fireScroll();
  }
  scroll(x, y) { this.scrollTo(x, y); }
  scrollBy(x, y) {
    let dl, dt;
    if (x !== null && typeof x === 'object') { dl = x.left; dt = x.top; }
    else { dl = x; dt = y; }
    this.scrollTo({
      left: (this.scrollLeft || 0) + (+dl || 0),
      top: (this.scrollTop || 0) + (+dt || 0),
    });
  }
  _fireScroll() {
    if (this._scrollEventPending) return;
    this._scrollEventPending = true;
    const self = this;
    setTimeout(() => {
      self._scrollEventPending = false;
      try { self.dispatchEvent(new Event('scroll', { bubbles: false })); } catch (e) {}
    }, 0);
  }
  animate(keyframes, options) {
    const animation = new Animation(new KeyframeEffect(this, keyframes, options), document.timeline);
    animation.play();
    return animation;
  }
  getAnimations() { return _animationsForTarget(this); }
  remove() { if (this.parentNode) this.parentNode.removeChild(this); }
  append(...nodes) { for (const n of _convertNodes(nodes)) this.appendChild(n); }
  prepend(...nodes) {
    const ref = this.firstChild;
    for (const n of _convertNodes(nodes)) {
      if (ref) this.insertBefore(n, ref); else this.appendChild(n);
    }
  }
  replaceChildren(...nodes) {
    const converted = _convertNodes(nodes);
    let c;
    while ((c = this.firstChild)) this.removeChild(c);
    for (const n of converted) this.appendChild(n);
  }
}

// WHATWG "convert nodes into a node": a Node argument passes through, anything
// else is stringified into a Text node, so e.g. append(null) inserts the text
// "null" and append(undefined) inserts "undefined" per the (Node or DOMString)
// union, rather than throwing.
function _convertNodes(nodes) {
  const out = [];
  for (let i = 0; i < nodes.length; i++) {
    const n = nodes[i];
    if (n && typeof n._nid === "number") out.push(n);
    else out.push(document.createTextNode(String(n)));
  }
  return out;
}

// ---- Reflected IDL attributes (WHATWG) ---------------------------------------
// Installed ONCE on Element.prototype as shared getter/setter pairs. This is
// data-driven so there is no per-element defineProperty: element creation and
// the querySelector/mutation hot paths are unaffected (each access is a normal
// prototype getter that reads the backing attribute). Covers the global content
// attributes reflected on every element plus the ARIAMixin (aria-* + ariaXxx).
(function installElementReflectors() {
  const P = Element.prototype;
  const def = (name, get, set) => {
    if (Object.prototype.hasOwnProperty.call(P, name)) return; // never clobber an existing member
    Object.defineProperty(P, name, { get, set, enumerable: true, configurable: true });
  };
  // WHATWG "rules for parsing integers"; returns a JS number or null on failure.
  const parseIntAttr = (s) => {
    if (s === null || s === undefined) return null;
    const m = /^[ \t\n\f\r]*([+-]?[0-9]+)/.exec(String(s));
    if (!m) return null;
    const n = parseInt(m[1], 10);
    return Number.isFinite(n) ? n : null;
  };
  // IDL `long` conversion (ToInt32): finite, truncated, wrapped to 32-bit signed.
  const toLong = (v) => {
    let n = Number(v);
    if (!Number.isFinite(n)) n = 0;
    n = Math.trunc(n) % 4294967296;
    if (n >= 2147483648) n -= 4294967296;
    else if (n < -2147483648) n += 4294967296;
    return n;
  };
  // DOMString reflect: get -> attribute or ""; set -> setAttribute(String(v)).
  const reflectStr = (name, attr) => def(name,
    function () { const v = this.getAttribute(attr); return v === null ? "" : v; },
    function (v) { this.setAttribute(attr, String(v)); });
  // boolean reflect: get -> hasAttribute; set -> truthy ? add("") : remove.
  const reflectBool = (name, attr) => def(name,
    function () { return this.hasAttribute(attr); },
    function (v) { if (v) this.setAttribute(attr, ""); else this.removeAttribute(attr); });
  // long reflect: get -> parse else default (static value or per-element fn);
  // set -> setAttribute(String(ToInt32(v))).
  const reflectLong = (name, attr, dflt) => def(name,
    function () {
      const r = parseIntAttr(this.getAttribute(attr));
      if (r !== null && r >= -2147483648 && r <= 2147483647) return r;
      return typeof dflt === "function" ? dflt.call(this) : dflt;
    },
    function (v) { this.setAttribute(attr, String(toLong(v))); });
  // enumerated reflect: get -> canonical (lowercased) keyword, else missing/
  // invalid default; set -> setAttribute(String(v)) (canonicalization on get).
  const reflectEnum = (name, attr, keywords, missingDefault, invalidDefault) => def(name,
    function () {
      const v = this.getAttribute(attr);
      if (v === null) return missingDefault;
      const lc = String(v).toLowerCase();
      return keywords.indexOf(lc) !== -1 ? lc : invalidDefault;
    },
    function (v) { this.setAttribute(attr, String(v)); });
  // nullable DOMString reflect (ARIA): get -> attribute or null; set -> null/
  // undefined removes, else setAttribute(String(v)).
  const reflectNullable = (name, attr) => def(name,
    function () { return this.getAttribute(attr); },
    function (v) { if (v === null || v === undefined) this.removeAttribute(attr); else this.setAttribute(attr, String(v)); });

  // Global content attributes reflected on every element (HTML "global attributes").
  reflectStr("title", "title");
  reflectStr("lang", "lang");
  reflectStr("accessKey", "accesskey");
  reflectStr("slot", "slot");
  reflectEnum("dir", "dir", ["ltr", "rtl", "auto"], "", "");
  reflectBool("autofocus", "autofocus");
  reflectBool("hidden", "hidden");
  // tabIndex default is element-dependent (0 for natively-focusable, else -1);
  // reflection.js does not assert it, but match the common case anyway.
  reflectLong("tabIndex", "tabindex", function () {
    const ln = this.localName;
    if (ln === "a" || ln === "area" || ln === "link") return this.hasAttribute("href") ? 0 : -1;
    return (ln === "button" || ln === "input" || ln === "select" || ln === "textarea" || ln === "iframe") ? 0 : -1;
  });

  // ARIAMixin: aria-* content attributes reflected as nullable DOMString IDL
  // properties (ariaAtomic <-> aria-atomic, ...).
  const ARIA = {
    ariaAtomic: "aria-atomic", ariaAutoComplete: "aria-autocomplete", ariaBrailleLabel: "aria-braillelabel",
    ariaBrailleRoleDescription: "aria-brailleroledescription", ariaBusy: "aria-busy", ariaChecked: "aria-checked",
    ariaColCount: "aria-colcount", ariaColIndex: "aria-colindex", ariaColIndexText: "aria-colindextext",
    ariaColSpan: "aria-colspan", ariaCurrent: "aria-current", ariaDescription: "aria-description",
    ariaDisabled: "aria-disabled", ariaExpanded: "aria-expanded", ariaHasPopup: "aria-haspopup",
    ariaHidden: "aria-hidden", ariaInvalid: "aria-invalid", ariaKeyShortcuts: "aria-keyshortcuts",
    ariaLabel: "aria-label", ariaLevel: "aria-level", ariaLive: "aria-live", ariaModal: "aria-modal",
    ariaMultiLine: "aria-multiline", ariaMultiSelectable: "aria-multiselectable", ariaOrientation: "aria-orientation",
    ariaPlaceholder: "aria-placeholder", ariaPosInSet: "aria-posinset", ariaPressed: "aria-pressed",
    ariaReadOnly: "aria-readonly", ariaRelevant: "aria-relevant", ariaRequired: "aria-required",
    ariaRoleDescription: "aria-roledescription", ariaRowCount: "aria-rowcount", ariaRowIndex: "aria-rowindex",
    ariaRowIndexText: "aria-rowindextext", ariaRowSpan: "aria-rowspan", ariaSelected: "aria-selected",
    ariaSetSize: "aria-setsize", ariaSort: "aria-sort", ariaValueMax: "aria-valuemax",
    ariaValueMin: "aria-valuemin", ariaValueNow: "aria-valuenow", ariaValueText: "aria-valuetext",
  };
  for (const prop in ARIA) reflectNullable(prop, ARIA[prop]);
})();

function _parseXPathPredicate(part) {
  part = String(part || "").trim();
  let m = part.match(/^@([A-Za-z_][\w:.-]*)(?:\s*=\s*(["'])(.*?)\2)?$/);
  if (m) return { kind: "attr", name: m[1], value: m[3] };
  m = part.match(/^contains\(\s*@([A-Za-z_][\w:.-]*)\s*,\s*(["'])(.*?)\2\s*\)$/);
  if (m) return { kind: "contains", name: m[1], value: m[3] };
  m = part.match(/^starts-with\(\s*@([A-Za-z_][\w:.-]*)\s*,\s*(["'])(.*?)\2\s*\)$/);
  if (m) return { kind: "startsWith", name: m[1], value: m[3] };
  return null;
}

function _xpathPredicateParts(body) {
  const out = [];
  let quote = null, start = 0;
  for (let i = 0; i < body.length; i++) {
    const ch = body[i];
    if (quote) {
      if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      continue;
    }
    if (body.slice(i, i + 5).toLowerCase() === " and " || body.slice(i, i + 4).toLowerCase() === "and ") {
      const before = body.slice(start, i).trim();
      if (before) out.push(before);
      i += body[i] === " " ? 4 : 3;
      start = i + 1;
    }
  }
  const last = body.slice(start).trim();
  if (last) out.push(last);
  return out.length ? out : [body];
}

function _xpathFindNodes(expression, contextNode) {
  expression = String(expression || "").trim();
  contextNode = contextNode || document;
  const m = expression.match(/^(?:\.?\/\/)([A-Za-z*][\w:.-]*|\*)?((?:\[[^\]]+\])*)$/);
  if (!m) return [];
  const tag = !m[1] || m[1] === "*" ? "*" : m[1];
  const predicates = [];
  const predText = m[2] || "";
  for (const match of predText.matchAll(/\[([^\]]+)\]/g)) {
    for (const part of _xpathPredicateParts(match[1])) {
      const pred = _parseXPathPredicate(part);
      if (pred) predicates.push(pred);
    }
  }
  const source = typeof contextNode.querySelectorAll === "function"
    ? contextNode.querySelectorAll(tag)
    : [];
  return Array.prototype.filter.call(source, (node) => {
    for (const pred of predicates) {
      const value = node.getAttribute?.(pred.name);
      if (pred.kind === "attr") {
        if (value === null) return false;
        if (pred.value !== undefined && value !== pred.value) return false;
      } else if (pred.kind === "contains") {
        if (value === null || !String(value).includes(pred.value)) return false;
      } else if (pred.kind === "startsWith") {
        if (value === null || !String(value).startsWith(pred.value)) return false;
      }
    }
    return true;
  });
}

function _makeXPathResult(type, nodes) {
  nodes = Array.from(nodes || []);
  const requested = type || XPathResult.ANY_TYPE;
  const resultType = requested === XPathResult.ANY_TYPE
    ? XPathResult.UNORDERED_NODE_ITERATOR_TYPE
    : requested;
  let iter = 0;
  return {
    resultType,
    singleNodeValue: nodes[0] || null,
    snapshotLength: nodes.length,
    snapshotItem(i) { return nodes[i] || null; },
    iterateNext() { return nodes[iter++] || null; },
    invalidIteratorState: false,
    numberValue: nodes.length,
    stringValue: nodes[0]?.textContent || "",
    booleanValue: nodes.length > 0,
  };
}

// `document.domain` exposes the document's effective host.  Keep the relaxed
// value on the live Document object so a navigation (which installs a new
// Document in __obscura_init) naturally restores the URL host.  Detached
// documents inherit the incumbent realm's principal for reads, which is why a
// `new Document().domain` read reflects the live document rather than its own
// about:blank URL.
function _documentUrlHost() {
  try { return new URL(_domParse("document_url") || "about:blank").hostname; }
  catch (_) { return ""; }
}
function _incumbentDocumentDomain() {
  const live = globalThis.document;
  return live && typeof live._effectiveDomain === "string"
    ? live._effectiveDomain
    : _documentUrlHost();
}
function _throwDocumentDomainSecurityError() {
  throw new DOMException("Failed to set the 'domain' property on 'Document'", "SecurityError");
}

class Document extends Node {
  get timeline() {
    if (!this._timeline) {
      this._timeline = new DocumentTimeline();
    }
    return this._timeline;
  }
  getAnimations() {
    return Array.from(_waapiAnimations).filter(animation => animation.playState !== 'idle'
      && (animation.playState !== 'finished' || animation.effect?._timing.fill === 'forwards' || animation.effect?._timing.fill === 'both'));
  }
  get documentElement() { return _wrapEl(+_dom("document_element")); }
  get children() {
    const root = this.documentElement;
    return HTMLCollection._from(root ? [root] : []);
  }
  get childElementCount() { return this.documentElement ? 1 : 0; }
  get firstElementChild() { return this.documentElement; }
  get lastElementChild() { return this.documentElement; }
  get head() { return this.querySelector("head"); }
  get body() { return this.querySelector("body"); }
  get doctype() {
    if (this._doctype !== undefined) return this._doctype;
    const info = _domParse("document_doctype");
    if (info && info.name) {
      this._doctype = new DocumentType(info.nodeId, info.name, info.publicId || "", info.systemId || "");
    } else {
      this._doctype = null;
    }
    return this._doctype;
  }
  get title() { return _domParse("document_title") ?? ""; }
  set title(v) {
    const value = String(v);
    let title = this.querySelector("title");
    if (!title) {
      let head = this.head;
      const root = this.documentElement;
      if (!head && root) {
        head = this.createElement("head");
        root.insertBefore(head, this.body);
      }
      if (!head) return;
      title = this.createElement("title");
      head.appendChild(title);
    }
    title.textContent = value;
  }
  get URL() { return _domParse("document_url") ?? ""; }
  get documentURI() { return this.URL; }
  get domain() {
    return this === globalThis.document
      ? (typeof this._effectiveDomain === "string" ? this._effectiveDomain : _documentUrlHost())
      : _incumbentDocumentDomain();
  }
  set domain(value) {
    // Web IDL performs DOMString conversion before the setter algorithm checks
    // whether the Document has a browsing context.
    const input = String(value);
    if (this !== globalThis.document) _throwDocumentDomainSecurityError();
    const current = this.domain;
    if (!current) _throwDocumentDomainSecurityError();
    const candidate = Deno.core.ops.op_document_domain_candidate(current, input);
    if (!candidate) _throwDocumentDomainSecurityError();
    // This runtime currently has one top-level browsing context and no
    // principal-backed same-origin-domain comparison.  Persisting the
    // validated effective domain supplies the standards-shaped API without
    // weakening iframe/fetch/storage origin checks; those must be wired to a
    // future browsing-context principal model before domain relaxation can
    // grant cross-document access.
    this._effectiveDomain = candidate;
  }
  get referrer() { return _domParse("document_referrer") ?? ""; }
  get location() { return globalThis.location; }
  set location(url) { Deno.core.ops.op_navigate(_resolveUrl(String(url)), 'GET', ''); }
  get defaultView() { return globalThis; }
  get nodeType() { return 9; }
  get nodeName() { return "#document"; }
  get ownerDocument() { return null; } // Document has no ownerDocument
  get compatMode() { return "CSS1Compat"; }
  // The document's character encoding, detected from the response charset
  // (HTTP Content-Type -> <meta charset>). characterSet/charset/inputEncoding
  // are WHATWG aliases. A node-less document (DOMParser/createDocument) has no
  // backing encoding and reports UTF-8.
  get characterSet() { return (this._nid === undefined || this._nid === null) ? "UTF-8" : _docEncoding(); }
  get charset() { return this.characterSet; }
  get inputEncoding() { return this.characterSet; }
  get contentType() {
    // An explicit type set by DOMParser/createDocument wins.
    if (this._contentType) return this._contentType;
    // `new Document()` (the WHATWG constructor, no backing node id) creates an
    // XML document, so createCDATASection/etc. must not throw. Live documents
    // wrapped from the tree carry a real nid and fall through to URL-derived.
    if (this._nid === undefined || this._nid === null) return "application/xml";
    const url = this.URL || "";
    // data: URLs carry their MIME type explicitly.
    const dm = /^data:([^,;]+)/i.exec(url);
    if (dm) {
      const mime = dm[1].toLowerCase();
      if (mime === "application/xhtml+xml") return "application/xhtml+xml";
      if (mime === "text/xml") return "text/xml";
      if (mime === "application/xml" || mime.endsWith("+xml")) return "application/xml";
    }
    if (/\.xhtml(?:[?#]|$)/i.test(url)) return "application/xhtml+xml";
    if (/\.(?:xml|svg)(?:[?#]|$)/i.test(url)) return "application/xml";
    return "text/html";
  }
  get readyState() { return globalThis.__documentReadyState__ || 'complete'; }
  get currentScript() {
    // Next.js / Turbopack chunk loader reads document.currentScript.src to
    // derive its base path. page.rs sets __currentScriptNid before each
    // <script> body runs and clears it after, mirroring real Chrome.
    const nid = globalThis.__currentScriptNid;
    return nid ? _wrapEl(+nid) : null;
  }
  get hidden() { return false; }
  get visibilityState() { return "visible"; }
  getElementById(id) { return _wrapEl(+_dom("get_element_by_id", id)); }
  querySelector(s) { return _wrapEl(+_dom("query_selector", s)); }
  querySelectorAll(s) {
    const ids = _domParse("query_selector_all", s) || [];
    return _nodeList(ids.map(_wrapEl).filter(Boolean));
  }
  getElementsByTagName(t) { return HTMLCollection._from(this.querySelectorAll(t)); }
  getElementsByClassName(c) { return _getElementsByClassName(this, c); }
  getElementsByName(name) { return this.querySelectorAll('[name="' + String(name).replace(/\\/g, '\\\\').replace(/"/g, '\\"') + '"]'); }
  evaluate(expression, contextNode, namespaceResolver, type, result) {
    return _makeXPathResult(type, _xpathFindNodes(expression, contextNode || this));
  }
  createElement(t) {
    const localName = String(t).toLowerCase();
    const nid = +_dom("create_element", localName);
    const C = _elementClassForKnownName(
      "http://www.w3.org/1999/xhtml",
      localName,
    );
    const el = new C(nid);
    // This node was just created from values already known to JS. Seed its
    // immutable metadata instead of rediscovering it through native calls in
    // hydration's tag/local-name checks.
    el._tagName = localName.toUpperCase();
    el._lname = localName;
    el._ns = "http://www.w3.org/1999/xhtml";
    el._nullNamespaceAttrs = new Map();
    _seedDetachedTreeState(el);
    _cache.set(nid, el);
    if (el && localName === 'template') {
      el._templateContent = this.createDocumentFragment();
      el._templateContent._fragmentContext = 'template';
    }
    const definition = globalThis.customElements?._registry?.get(localName);
    if (el && definition) globalThis.customElements._upgradeElement(el, definition);
    return el;
  }
  createElementNS(ns, t) {
    const namespace = ns == null ? null : String(ns);
    const qualified = String(t);
    _ns_validateQualifiedName(namespace == null ? "" : namespace, qualified);
    if (namespace === "http://www.w3.org/1999/xhtml") {
      const el = this.createElement(qualified);
      if (el) el._ns = namespace;
      return el;
    }
    const nid = +_dom(
      "create_element_ns",
      (namespace == null ? "" : namespace) + "\0" + qualified,
    );
    const effectiveNamespace = namespace == null ? "" : namespace;
    const C = _elementClassForKnownName(effectiveNamespace, qualified);
    const el = new C(nid);
    const localName = qualified.includes(":")
      ? qualified.slice(qualified.indexOf(":") + 1)
      : qualified;
    el._tagName = qualified;
    el._lname = localName;
    el._ns = effectiveNamespace;
    el._nullNamespaceAttrs = new Map();
    _seedDetachedTreeState(el);
    _cache.set(nid, el);
    return el;
  }
  createTextNode(t) {
    const nid = +_dom("create_text_node", String(t));
    const n = new Text(nid);
    _seedDetachedTreeState(n);
    _cache.set(nid, n);
    return n;
  }
  createComment(t) {
    const nid = +_dom("create_comment_node", String(t ?? ""));
    const n = new Comment(nid);
    _seedDetachedTreeState(n);
    _cache.set(nid, n);
    return n;
  }
  createCDATASection(data) {
    // Spec: throw NotSupportedError on an HTML document, reject data
    // containing "]]>", then return a CDATASection node.
    if (!_isXMLDocument(this)) {
      throw new DOMException("createCDATASection is not supported in HTML documents", "NotSupportedError");
    }
    const str = String(data);
    if (str.indexOf("]]>") !== -1) {
      throw new DOMException("CDATA section data must not contain ']]>'", "InvalidCharacterError");
    }
    const nid = +_dom("create_text_node", str);
    const n = new CDATASection(nid);
    _seedDetachedTreeState(n);
    _cache.set(nid, n);
    return n;
  }
  createProcessingInstruction(target, data) {
    // Spec: not gated on document type. Reject targets that are not an XML
    // Name, then reject data containing "?>", then return a PI node.
    const tgt = String(target);
    const str = String(data);
    if (!_isValidPITarget(tgt)) {
      throw new DOMException("Invalid processing instruction target", "InvalidCharacterError");
    }
    if (str.indexOf("?>") !== -1) {
      throw new DOMException("Processing instruction data must not contain '?>'", "InvalidCharacterError");
    }
    const nid = +_dom("create_text_node", str);
    const n = new ProcessingInstruction(nid, tgt);
    _seedDetachedTreeState(n);
    _cache.set(nid, n);
    return n;
  }
  createDocumentFragment() {
    const nid = +_dom("create_document_fragment");
    const frag = new DocumentFragment(nid);
    _seedDetachedTreeState(frag);
    _cache.set(nid, frag);
    return frag;
  }
  // Legacy DOM Level 2 event factory. Spec returns an event of the requested
  // class with an empty type until init*Event() is called. We previously
  // returned a generic Event for every type, which broke libraries that call
  // createEvent('CustomEvent').initCustomEvent(...) — see issue #41.
  createEvent(type) {
    const normalized = String(type || '').toLowerCase();
    if (normalized === 'promiserejectionevent') {
      throw new DOMException(
        "The provided event type ('PromiseRejectionEvent') is invalid",
        'NotSupportedError'
      );
    }
    const map = {
      'customevent': CustomEvent, 'customevents': CustomEvent,
      'mouseevent': MouseEvent,   'mouseevents': MouseEvent,
      'keyboardevent': KeyboardEvent, 'keyboardevents': KeyboardEvent,
      'focusevent': FocusEvent,
      'inputevent': InputEvent,
      'uievent': UIEvent, 'uievents': UIEvent,
      'compositionevent': CompositionEvent,
      'wheelevent': WheelEvent,
      'pointerevent': PointerEvent,
      'errorevent': ErrorEvent,
      'popstateevent': PopStateEvent,
      'animationevent': AnimationEvent,
      'transitionevent': TransitionEvent,
      'storageevent': StorageEvent,
    };
    const Cls = map[normalized] || Event;
    return new Cls('');
  }
  createRange() { return new Range(); }
  addEventListener(type, fn, opts) {
    if (typeof fn !== 'function') return;
    if (!this._listeners) this._listeners = {};
    if (!this._listeners[type]) this._listeners[type] = [];
    if (!this._listeners[type].includes(fn)) this._listeners[type].push(fn);
  }
  removeEventListener(type, fn) {
    if (this._listeners?.[type]) {
      this._listeners[type] = this._listeners[type].filter(h => h !== fn);
    }
  }
  dispatchEvent(event) {
    if (!event) return true;
    const handlers = (this._listeners?.[event.type] || []).slice();
    for (const h of handlers) { try { h.call(this, event); } catch(e) { console.error('document event error:', e); } }
    return !event.defaultPrevented;
  }
  createTreeWalker(root, whatToShow, filter) {
    // whatToShow is unsigned long; default SHOW_ALL only when the arg is omitted.
    // An explicit 0 (show nothing) must stay 0, not become SHOW_ALL.
    whatToShow = (whatToShow === undefined) ? 0xFFFFFFFF : (whatToShow >>> 0);
    const walker = {
      root: root,
      currentNode: root,
      whatToShow: whatToShow,
      filter: filter || null,
      // Three-valued per NodeFilter: 1 ACCEPT, 2 REJECT, 3 SKIP. REJECT and
      // SKIP both mean "don't return this node", but only REJECT prunes its
      // descendants, so nextNode() needs to tell them apart (issue #461).
      // A node filtered out by whatToShow is a SKIP: the spec never consults
      // the filter for it, and its descendants stay eligible.
      _filter(node) {
        const nodeType = node.nodeType;
        if (!((whatToShow >> (nodeType - 1)) & 1)) return 3;
        if (this.filter) {
          if (typeof this.filter === 'function') return this.filter(node);
          if (this.filter.acceptNode) return this.filter.acceptNode(node);
        }
        return 1;
      },
      _accept(node) { return this._filter(node) === 1; },
      nextNode() {
        let node = _wrap(+_dom("next_in_subtree", this.root._nid, this.currentNode._nid));
        while (node) {
          const verdict = this._filter(node);
          if (verdict === 1) { this.currentNode = node; return node; }
          // FILTER_REJECT skips the node AND its subtree; FILTER_SKIP (and any
          // other non-accept value) skips only the node.
          const step = verdict === 2 ? "next_after_subtree" : "next_in_subtree";
          node = _wrap(+_dom(step, this.root._nid, node._nid));
        }
        return null;
      },
      // DOM 6.1 "previousNode", implemented as specified (issue #462). The old
      // version looked at exactly one candidate — the previous sibling's
      // deepest last child — and returned null the moment it was filtered out,
      // so a backward walk died mid-tree the way nextNode used to before #432.
      //
      // Unlike nextNode this stays in JS rather than using a DOM traversal op:
      // the descent into last children has to stop on FILTER_REJECT, so the
      // filter is consulted at every step anyway and there is no run of
      // crossings for a native helper to collapse.
      previousNode() {
        let node = this.currentNode;
        while (node !== this.root) {
          let sibling = node.previousSibling;
          while (sibling) {
            node = sibling;
            let verdict = this._filter(node);
            // Descend to the deepest last descendant, but never into a rejected
            // subtree — that is what makes REJECT prune backwards as well.
            while (verdict !== 2 && node.lastChild) {
              node = node.lastChild;
              verdict = this._filter(node);
            }
            if (verdict === 1) { this.currentNode = node; return node; }
            sibling = node.previousSibling;
          }
          const parent = node.parentNode;
          // Reaching root (or a detached node) ends the walk: root is never
          // returned by a backward traversal.
          if (!parent || node === this.root) return null;
          node = parent;
          if (node === this.root) return null;
          if (this._filter(node) === 1) { this.currentNode = node; return node; }
        }
        return null;
      },
      // DOM 6.1 "traverse children" (issue #469). The movers used to step
      // straight to the next sibling when a node was not accepted, so a
      // FILTER_SKIP node hid its children instead of exposing them. `edge` and
      // `step` pick the direction: first/next for forward, last/previous for
      // backward.
      _traverseChildren(edge, step) {
        let node = this.currentNode[edge];
        while (node) {
          const verdict = this._filter(node);
          if (verdict === 1) { this.currentNode = node; return node; }
          // Only SKIP leaves the children eligible; REJECT prunes the subtree.
          if (verdict === 3) {
            const child = node[edge];
            if (child) { node = child; continue; }
          }
          // Subtree exhausted: step sideways, climbing out without passing
          // root or the node the walk started from.
          while (node) {
            const sibling = node[step];
            if (sibling) { node = sibling; break; }
            const parent = node.parentNode;
            if (!parent || parent === this.root || parent === this.currentNode) return null;
            node = parent;
          }
        }
        return null;
      },
      // DOM 6.1 "traverse siblings" (issue #469).
      _traverseSiblings(edge, step) {
        let node = this.currentNode;
        if (node === this.root) return null;
        for (;;) {
          let sibling = node[step];
          while (sibling) {
            node = sibling;
            const verdict = this._filter(node);
            if (verdict === 1) { this.currentNode = node; return node; }
            // Descend into a skipped sibling's subtree; a rejected one is
            // off-limits, and a childless one has nothing to descend into.
            sibling = node[edge];
            if (verdict === 2 || !sibling) sibling = node[step];
          }
          node = node.parentNode;
          if (!node || node === this.root) return null;
          // An accepted parent is where the walk would go next, so there is no
          // sibling to return.
          if (this._filter(node) === 1) return null;
        }
      },
      firstChild() { return this._traverseChildren('firstChild', 'nextSibling'); },
      lastChild() { return this._traverseChildren('lastChild', 'previousSibling'); },
      nextSibling() { return this._traverseSiblings('firstChild', 'nextSibling'); },
      previousSibling() { return this._traverseSiblings('lastChild', 'previousSibling'); },
      // DOM 6.1 "parentNode" (issue #475). The old version looked only at the
      // immediate parent, so it couldn't climb past a skipped ancestor; it also
      // excluded `root` as a result yet stepped to root's own parent when
      // currentNode was root, returning a node OUTSIDE the walker's subtree.
      // The loop's `node !== this.root` guard is what keeps the walk inside
      // root while still allowing root itself to be returned.
      parentNode() {
        let node = this.currentNode;
        while (node && node !== this.root) {
          node = node.parentNode;
          if (node && this._accept(node)) { this.currentNode = node; return node; }
        }
        return null;
      },
    };
    return walker;
  }
  // A real NodeIterator (DOM 6.2), not a TreeWalker in disguise (issue #467).
  // The two differ in more than naming: an iterator's pointer starts *before*
  // its root, so the first nextNode() returns the root itself, and it exposes
  // referenceNode/pointerBeforeReferenceNode/detach rather than a TreeWalker's
  // currentNode and child/sibling movers.
  createNodeIterator(root, whatToShow, filter) {
    // whatToShow is unsigned long; default SHOW_ALL only when the arg is
    // omitted. An explicit 0 (show nothing) must stay 0, not become SHOW_ALL.
    whatToShow = (whatToShow === undefined) ? 0xFFFFFFFF : (whatToShow >>> 0);
    return {
      root: root,
      referenceNode: root,
      pointerBeforeReferenceNode: true,
      whatToShow: whatToShow,
      filter: filter || null,
      // NodeIterator prunes nothing: FILTER_REJECT behaves as FILTER_SKIP, so
      // unlike the TreeWalker only "accepted or not" matters here.
      _accept(node) {
        if (!((whatToShow >> (node.nodeType - 1)) & 1)) return false;
        if (this.filter) {
          if (typeof this.filter === 'function') return this.filter(node) === 1;
          if (this.filter.acceptNode) return this.filter.acceptNode(node) === 1;
        }
        return true;
      },
      // DOM 6.2 "traverse". The pointer sits either before or after
      // referenceNode, which is why reversing direction re-yields the current
      // node instead of stepping over it.
      _traverse(forward) {
        let node = this.referenceNode;
        let before = this.pointerBeforeReferenceNode;
        for (;;) {
          if (forward === before) {
            // Consume the pointer's side without moving: it flips to the other
            // side of the node it already references.
            before = !before;
          } else {
            const step = forward ? "next_in_subtree" : "prev_in_subtree";
            const next = _wrap(+_dom(step, this.root._nid, node._nid));
            // A failed traversal leaves referenceNode and the pointer
            // untouched, so the iterator can be resumed in either direction.
            if (!next) return null;
            node = next;
          }
          if (this._accept(node)) break;
        }
        this.referenceNode = node;
        this.pointerBeforeReferenceNode = before;
        return node;
      },
      nextNode() { return this._traverse(true); },
      previousNode() { return this._traverse(false); },
      // Legacy no-op since DOM4, but older library code still calls it and
      // used to hit "detach is not a function".
      detach() {},
    };
  }
  getSelection() { return this.defaultView ? _selectionFor(this) : null; }
  get activeElement() { return globalThis.__obscura_focused || this.body; }
  // The element that scrolls the viewport, and where the page offset lives
  // (issue #468). Standards mode, so documentElement — quirks mode would be
  // body, but we never parse in quirks mode.
  get scrollingElement() { return this.documentElement; }
  get implementation() {
    const ownerDoc = this;
    return {
      // Spec: createHTMLDocument returns a NEW detached Document. jQuery
      // 3.x's selector feature-detect calls `body.innerHTML = '<form>'` on
      // the result — when we returned `globalThis.document`, the real
      // `<body>` was wiped, taking every page on the open web that ships
      // jQuery 3.x with it. Reuse the DOMParser path to build a detached
      // document, then optionally set the title.
      createHTMLDocument(title) {
        // Build head>title and body explicitly. Parsing a full skeleton string
        // as innerHTML of <html> collapses through the fragment parser (it
        // dropped head/body and kept only <title>), leaving doc.body null.
        const doc = new DOMParser().parseFromString("", "text/html");
        const root = doc.documentElement;
        const head = document.createElement("head");
        const titleEl = document.createElement("title");
        if (title != null) titleEl.textContent = String(title);
        head.appendChild(titleEl);
        const body = document.createElement("body");
        root.appendChild(head);
        root.appendChild(body);
        return doc;
      },
      // Real spec: createDocument(namespaceURI, qualifiedName, doctype) →
      // an XML document with a root element of the given name. We don't
      // have a separate XML stack, so return a minimal detached document
      // with an element of the requested local name as documentElement.
      createDocument(_ns, qualifiedName, _doctype) {
        const name = (qualifiedName && String(qualifiedName)) || "root";
        const safe = name.replace(/[^a-zA-Z0-9-]/g, "");
        const html = qualifiedName ? `<${safe}></${safe}>` : "";
        const doc = new DOMParser().parseFromString(html, "application/xml");
        if (_doctype) doc._docType = _doctype;
        return doc;
      },
      // createDocumentType(qualifiedName, publicId, systemId): build a detached
      // DocumentType node. Browsers validate leniently here (only a name with
      // ASCII whitespace or ">" is rejected, matching the WPT cases); the node's
      // owner document is the document whose implementation was used.
      createDocumentType(qualifiedName, publicId, systemId) {
        const name = String(qualifiedName);
        if (name === "" || /[\t\n\f\r >]/.test(name)) {
          throw new DOMException("The qualified name '" + name + "' contains an invalid character", "InvalidCharacterError");
        }
        const dt = new DocumentType(
          +_dom("create_comment_node", ""),
          name,
          publicId === undefined ? "" : String(publicId),
          systemId === undefined ? "" : String(systemId)
        );
        dt._ownerDocument = ownerDoc;
        return dt;
      },
      hasFeature() { return true; },
    };
  }
  get styleSheets() {
    if (!this._styleSheetList) this._styleSheetList = new StyleSheetList(this);
    return this._styleSheetList;
  }
  get forms() { return this.querySelectorAll("form"); }
  get images() { return this.querySelectorAll("img"); }
  get links() { return this.querySelectorAll("a[href], area[href]"); }
  get scripts() { return this.querySelectorAll("script"); }
  get cookie() {
    return Deno.core.ops.op_get_cookies();
  }
  set cookie(v) {
    if (!v) return;
    Deno.core.ops.op_set_cookie(v);
  }
  write(...args) {
    var html = args.join('');
    if (!html) return;
    var body = this.body;
    if (!body) return;
    var temp = this.createDocumentFragment();
    _dom("set_fragment_html_executable", temp._nid, _fragmentContextPayload('body', html));
    var children = Array.from(temp.childNodes);
    for (var i = 0; i < children.length; i++) {
      body.appendChild(children[i]);
    }
  }
  writeln(...args) {
    this.write(args.join('') + '\n');
  }
  open() {
    var body = this.body;
    if (body) body.innerHTML = '';
    return this;
  }
  close() {
    return;
  }
  hasFocus() { return true; }
  execCommand() { return false; }
}

class DocumentFragment extends Node {
  constructor(nid) {
    const created = nid === undefined;
    super(created ? +_dom("create_document_fragment") : nid);
    if (created) _seedDetachedTreeState(this);
  }
  get nodeType() { return 11; }
  get nodeName() { return "#document-fragment"; }
  get innerHTML() { return _domParse("inner_html", this._nid) ?? ""; }
  set innerHTML(v) {
    const html = String(v ?? "");
    if (this._fragmentContext) {
      _dom("set_inner_html_context", this._nid, _fragmentContextPayload(this._fragmentContext, html));
    } else {
      _dom("set_inner_html", this._nid, html);
    }
  }
  querySelector(s) { return _wrapEl(+_dom("query_selector_scoped", this._nid, s)); }
  querySelectorAll(s) {
    const ids = _domParse("query_selector_all_scoped", this._nid, s) || [];
    return _nodeList(ids.map(_wrapEl).filter(Boolean));
  }
  get children() {
    const ids = _domParse("element_children", this._nid) || [];
    return HTMLCollection._from(ids.map(_wrapEl).filter(Boolean));
  }
  get firstElementChild() { return this.children[0] || null; }
  get lastElementChild() { const ch = this.children; return ch[ch.length - 1] || null; }
  getElementById(id) {
    const needle = String(id);
    const stack = Array.from(this.childNodes || []).reverse();
    while (stack.length) {
      const node = stack.pop();
      if (!node) continue;
      if (node.nodeType === 1 && node.id === needle) return node;
      const children = node.childNodes || [];
      for (let i = children.length - 1; i >= 0; i--) stack.push(children[i]);
    }
    return null;
  }
  cloneNode(deep) {
    const nid = +_dom("clone_node", this._nid, deep ? "true" : "false");
    const frag = new DocumentFragment(nid);
    _cache.set(nid, frag);
    return frag;
  }
}

class DocumentType extends Node {
  constructor(nid, name, publicId, systemId) {
    super(nid);
    this._name = name;
    this._publicId = publicId;
    this._systemId = systemId;
  }
  get nodeType() { return 10; }
  get nodeName() { return this._name; }
  get name() { return this._name; }
  get publicId() { return this._publicId; }
  get systemId() { return this._systemId; }
  get nodeValue() { return null; }
  set nodeValue(v) {}
  get ownerDocument() { return this._ownerDocument || globalThis.document; }
}

const _cache = new Map();

class TextTrackCue {
  constructor(startTime, endTime, text) {
    this.id = "";
    this.startTime = Number(startTime);
    this.endTime = Number(endTime);
    this.text = String(text ?? "");
    this.pauseOnExit = false;
    this.vertical = "";
    this.snapToLines = true;
    this.line = "auto";
    this.lineAlign = "start";
    this.position = "auto";
    this.positionAlign = "auto";
    this.size = 100;
    this.align = "center";
    this.region = null;
    this.onenter = null;
    this.onexit = null;
  }
  getCueAsHTML() {
    const fragment = document.createDocumentFragment();
    fragment.appendChild(document.createTextNode(this.text));
    return fragment;
  }
}
class VTTCue extends TextTrackCue {}
class TextTrackCueList extends Array {
  getCueById(id) {
    return this.find((cue) => cue && cue.id === String(id)) || null;
  }
  item(index) { return this[index] || null; }
}
class TextTrack extends Node {
  constructor(element, kind, label, language) {
    super();
    this._element = element || null;
    this.kind = kind || "subtitles";
    this.label = label || "";
    this.language = language || "";
    this.id = element?.id || "";
    this.mode = element?.hasAttribute?.("default") ? "showing" : "disabled";
    this.inBandMetadataTrackDispatchType = "";
    this._parsedSrc = null;
    this._cues = new TextTrackCueList();
    this.activeCues = new TextTrackCueList();
    this.oncuechange = null;
  }
  get cues() {
    const src = this._element?.getAttribute?.("src") || "";
    if (src !== this._parsedSrc) {
      this._parsedSrc = src;
      this._cues = _parseWebVttCues(src);
    }
    return this._cues;
  }
}
class TextTrackList extends Array {
  item(index) { return this[index] || null; }
  getTrackById(id) {
    return this.find((track) => track && track.id === String(id)) || null;
  }
}
function _vttTime(value) {
  const parts = String(value).trim().split(":").map(Number);
  if (parts.some((part) => !Number.isFinite(part))) return 0;
  if (parts.length === 3) return parts[0] * 3600 + parts[1] * 60 + parts[2];
  if (parts.length === 2) return parts[0] * 60 + parts[1];
  return parts[0] || 0;
}
function _parseWebVttCues(src) {
  const cues = new TextTrackCueList();
  if (!src || !src.startsWith("data:text/vtt")) return cues;
  let text = "";
  try {
    const comma = src.indexOf(",");
    if (comma < 0) return cues;
    const meta = src.slice(0, comma);
    const body = src.slice(comma + 1);
    text = /;base64(?:;|$)/i.test(meta) ? atob(body) : decodeURIComponent(body);
  } catch (_error) {
    return cues;
  }
  const blocks = text.replace(/\r\n?/g, "\n").split(/\n{2,}/);
  for (const block of blocks) {
    const lines = block.split("\n").filter((line) => line.length > 0);
    if (!lines.length || lines[0].trim() === "WEBVTT" || lines[0].trim().startsWith("NOTE")) continue;
    let timingIndex = lines.findIndex((line) => line.includes("-->"));
    if (timingIndex < 0) continue;
    const timing = lines[timingIndex].split("-->");
    const endToken = (timing[1] || "").trim().split(/\s+/)[0];
    const cue = new VTTCue(_vttTime(timing[0]), _vttTime(endToken), lines.slice(timingIndex + 1).join("\n"));
    if (timingIndex > 0) cue.id = lines[timingIndex - 1].trim();
    cues.push(cue);
  }
  return cues;
}

function _imageEncodingError() {
  return new DOMException("The source image cannot be decoded.", "EncodingError");
}

// HTMLImageElement is backed by the same retained resource cache used by
// layout/paint. The render-only native op owns responsive candidate selection,
// fetching, and metadata sniffing; bootstrap owns only the observable request
// state and event timing.
class HTMLImageElement extends Element {
  constructor(nid) {
    super(nid);
    this._imageRequest = 0;
    this._imageQueued = false;
    this._imageInitialized = false;
    this._imageCompletionDeferred = false;
    this._imageComplete = typeof Deno.core.ops.op_image_metadata === "function"
      ? true
      : !this.getAttribute("src");
    this._imageDecoded = false;
    this._imageNaturalWidth = 0;
    this._imageNaturalHeight = 0;
    this._imageCurrentSrc = "";
    this._imageDecodeWaiters = [];
    this._refreshImageFromCache();
    this._imageInitialized = true;
    // Parser images stay lazy until script observes their lifecycle or paint
    // asks for the same cache entry. Inline handlers are observers too.
    if (!this._imageComplete
        && (this.hasAttribute("onload") || this.hasAttribute("onerror"))) {
      this._queueImageRequest();
    }
  }

  get src() {
    const raw = this.getAttribute("src");
    if (!raw) return "";
    try { return new URL(raw, this.baseURI || globalThis.location?.href || "about:blank").href; }
    catch (_error) { return raw; }
  }
  set src(value) { this.setAttribute("src", value); }

  get currentSrc() {
    this._refreshImageFromCache();
    this._queueImageRequest();
    return this._imageCurrentSrc;
  }
  get complete() {
    this._refreshImageFromCache();
    this._queueImageRequest();
    return this._imageComplete;
  }
  get naturalWidth() {
    this._refreshImageFromCache();
    this._queueImageRequest();
    return this._imageNaturalWidth;
  }
  get naturalHeight() {
    this._refreshImageFromCache();
    this._queueImageRequest();
    return this._imageNaturalHeight;
  }
  get onload() { return this._imageOnload || null; }
  set onload(value) {
    this._imageOnload = typeof value === "function" ? value : null;
    if (this._imageOnload) {
      this._refreshImageFromCache();
      this._queueImageRequest();
    }
  }
  get onerror() { return this._imageOnerror || null; }
  set onerror(value) {
    this._imageOnerror = typeof value === "function" ? value : null;
    if (this._imageOnerror) {
      this._refreshImageFromCache();
      this._queueImageRequest();
    }
  }

  get width() {
    const value = Number.parseInt(this.getAttribute("width") || "", 10);
    return Number.isFinite(value) && value >= 0 ? value : this._imageNaturalWidth;
  }
  set width(value) { this.setAttribute("width", Math.max(0, Number(value) || 0)); }
  get height() {
    const value = Number.parseInt(this.getAttribute("height") || "", 10);
    return Number.isFinite(value) && value >= 0 ? value : this._imageNaturalHeight;
  }
  set height(value) { this.setAttribute("height", Math.max(0, Number(value) || 0)); }

  get srcset() { return this.getAttribute("srcset") || ""; }
  set srcset(value) { this.setAttribute("srcset", value); }
  get sizes() { return this.getAttribute("sizes") || ""; }
  set sizes(value) { this.setAttribute("sizes", value); }
  get loading() { return this.getAttribute("loading") || "eager"; }
  set loading(value) { this.setAttribute("loading", value); }
  get decoding() { return this.getAttribute("decoding") || "auto"; }
  set decoding(value) { this.setAttribute("decoding", value); }
  get fetchPriority() { return this.getAttribute("fetchpriority") || "auto"; }
  set fetchPriority(value) { this.setAttribute("fetchpriority", value); }
  get crossOrigin() { return this.getAttribute("crossorigin"); }
  set crossOrigin(value) {
    if (value === null) this.removeAttribute("crossorigin");
    else this.setAttribute("crossorigin", value);
  }

  setAttribute(name, value) {
    const normalized = String(name).toLowerCase();
    super.setAttribute(name, value);
    if (normalized === "src" || normalized === "srcset" || normalized === "sizes"
        || normalized === "crossorigin") {
      this._imageSourceChanged();
    }
    else if ((normalized === "onload" || normalized === "onerror")
        && !this._imageComplete) this._queueImageRequest();
  }

  removeAttribute(name) {
    const normalized = String(name).toLowerCase();
    super.removeAttribute(name);
    if (normalized === "src" || normalized === "srcset" || normalized === "sizes"
        || normalized === "crossorigin") {
      this._imageSourceChanged();
    }
  }

  decode() {
    this._refreshImageFromCache();
    if (this._imageComplete) {
      return this._imageDecoded
        ? Promise.resolve()
        : Promise.reject(_imageEncodingError());
    }
    this._queueImageRequest();
    return new Promise((resolve, reject) => {
      this._imageDecodeWaiters.push({ resolve, reject, request: this._imageRequest });
    });
  }

  _imageSourceChanged() {
    // The lightweight build has no retained render-resource cache. It still
    // preserves the historical non-blocking Image lifecycle so preloaders do
    // not hang while rendering is disabled.
    const hasMetadataLoader = typeof Deno.core.ops.op_load_image_metadata === "function";
    this._adoptImageCandidate(hasMetadataLoader ? "" : this.src);
    this._imageCompletionDeferred = true;
    this._refreshImageFromCache(true);
    if (!this._imageComplete) this._queueImageRequest();
  }

  _adoptImageCandidate(currentSrc) {
    this._rejectImageDecodes();
    this._imageRequest++;
    this._imageQueued = false;
    this._imageNaturalWidth = 0;
    this._imageNaturalHeight = 0;
    this._imageDecoded = false;
    this._imageCurrentSrc = currentSrc ? String(currentSrc) : "";
    this._imageComplete = !this._imageCurrentSrc;
  }

  _queueImageRequest() {
    if (this._imageQueued || this._imageComplete) return;
    this._imageQueued = true;
    const request = this._imageRequest;
    setTimeout(() => {
      if (request === this._imageRequest && !this._imageComplete) {
        this._runImageRequest(request);
      } else if (request === this._imageRequest) {
        this._imageQueued = false;
      }
    }, 1);
  }

  _runImageRequest(request) {
    const finish = (metadata) => {
      if (request !== this._imageRequest) return;
      this._imageQueued = false;
      if (metadata && metadata.state === "stale") {
        this._refreshImageFromCache(true);
        this._queueImageRequest();
        return;
      }
      this._applyImageMetadata(metadata, request, true);
    };
    try {
      const op = Deno.core.ops.op_load_image_metadata;
      if (typeof op === "function") {
        Promise.resolve(op(this._nid >>> 0)).then(
          raw => {
            let metadata = null;
            try { metadata = JSON.parse(raw); }
            catch (_error) { metadata = { ok: false, currentSrc: this.src }; }
            finish(metadata);
          },
          () => finish({ ok: false, currentSrc: this.src }),
        );
      } else {
        // Non-render builds have no authoritative resource cache. Preserve the
        // old non-blocking compatibility behavior without issuing a duplicate
        // network fetch: the request succeeds with unknown intrinsic size.
        finish({ ok: true, currentSrc: this.src, width: 0, height: 0 });
      }
    } catch (_error) {
      finish({ ok: false, currentSrc: this.src });
    }
  }

  _refreshImageFromCache(deferCompletion) {
    try {
      const op = Deno.core.ops.op_image_metadata;
      if (typeof op !== "function") return;
      const metadata = JSON.parse(op(this._nid >>> 0, true));
      if (!metadata) return;
      const selected = metadata.currentSrc ? String(metadata.currentSrc) : "";
      if (selected !== this._imageCurrentSrc) {
        this._adoptImageCandidate(selected);
        // A live candidate switch is a new request even when paint retained
        // the candidate bytes. A cache-only getter must not synchronously
        // complete it and swallow the later load/error event.
        if (this._imageInitialized && selected) {
          this._imageCompletionDeferred = true;
        }
      }
      if (metadata.state === "pending") {
        if (selected && this._imageComplete) {
          this._adoptImageCandidate(selected);
        }
        return;
      }
      if ((deferCompletion || this._imageCompletionDeferred) && selected) {
        this._imageComplete = false;
        this._imageDecoded = false;
        this._imageNaturalWidth = 0;
        this._imageNaturalHeight = 0;
        return;
      }
      this._applyImageMetadata(metadata, this._imageRequest, false);
    } catch (_error) {}
  }

  _applyImageMetadata(metadata, request, dispatchEvent) {
    if (request !== this._imageRequest) return;
    const previousLifecycle = [
      this._imageComplete,
      this._imageDecoded,
      this._imageCurrentSrc,
      this._imageNaturalWidth,
      this._imageNaturalHeight,
    ];
    const selected = metadata && metadata.currentSrc
      ? String(metadata.currentSrc)
      : "";
    if (selected !== this._imageCurrentSrc) {
      this._adoptImageCandidate(selected);
      request = this._imageRequest;
    }
    this._imageCompletionDeferred = false;
    this._imageComplete = true;
    this._imageCurrentSrc = selected || this.src;
    const width = Number(metadata && metadata.width);
    const height = Number(metadata && metadata.height);
    const loaded = !!(metadata && metadata.ok)
      && (typeof Deno.core.ops.op_image_metadata !== "function"
        || (Number.isFinite(width) && width > 0 && Number.isFinite(height) && height > 0));
    if (loaded) {
      this._imageDecoded = true;
      this._imageNaturalWidth = Number.isFinite(width) && width > 0 ? Math.round(width) : 0;
      this._imageNaturalHeight = Number.isFinite(height) && height > 0 ? Math.round(height) : 0;
      this._resolveImageDecodes(request);
      if (dispatchEvent) {
        try { this.dispatchEvent(new Event("load")); } catch (_error) {}
      }
    } else {
      this._imageDecoded = false;
      this._imageNaturalWidth = 0;
      this._imageNaturalHeight = 0;
      this._rejectImageDecodes(request);
      if (dispatchEvent) {
        try { this.dispatchEvent(new Event("error")); } catch (_error) {}
      }
    }
    const lifecycleChanged =
      previousLifecycle[0] !== this._imageComplete ||
      previousLifecycle[1] !== this._imageDecoded ||
      previousLifecycle[2] !== this._imageCurrentSrc ||
      previousLifecycle[3] !== this._imageNaturalWidth ||
      previousLifecycle[4] !== this._imageNaturalHeight;
    if (lifecycleChanged) {
      // Intrinsic dimensions can become layout input at request completion
      // even though no DOM attribute changed. Stable cache-only getters must
      // not manufacture rendering updates on every read.
      _scheduleResizeRenderCheckpoint();
    }
  }

  _resolveImageDecodes(request) {
    const remaining = [];
    for (const waiter of this._imageDecodeWaiters) {
      if (waiter.request === request) waiter.resolve();
      else remaining.push(waiter);
    }
    this._imageDecodeWaiters = remaining;
  }

  _rejectImageDecodes(request) {
    const remaining = [];
    for (const waiter of this._imageDecodeWaiters) {
      if (request === undefined || waiter.request === request) {
        waiter.reject(_imageEncodingError());
      } else {
        remaining.push(waiter);
      }
    }
    this._imageDecodeWaiters = remaining;
  }

  addEventListener(type, callback, options) {
    super.addEventListener(type, callback, options);
    if ((String(type) === "load" || String(type) === "error") && callback) {
      this._refreshImageFromCache();
      this._queueImageRequest();
    }
  }
}
globalThis.HTMLImageElement = HTMLImageElement;
_markNative(HTMLImageElement);
_markNative(HTMLImageElement.prototype.decode);

// Report only capabilities backed by a real decoder. Poster rendering is an
// image operation and does not make any audio/video container playable.
class HTMLMediaElement extends Element {
  static NETWORK_EMPTY = 0;
  static NETWORK_IDLE = 1;
  static NETWORK_LOADING = 2;
  static NETWORK_NO_SOURCE = 3;
  static HAVE_NOTHING = 0;
  static HAVE_METADATA = 1;
  static HAVE_CURRENT_DATA = 2;
  static HAVE_FUTURE_DATA = 3;
  static HAVE_ENOUGH_DATA = 4;
  canPlayType(_type) { return ''; }
  load() {}
  play() {
    return Promise.reject(new DOMException(
      "The element has no supported sources.",
      "NotSupportedError",
    ));
  }
  pause() {}
  get NETWORK_EMPTY() { return HTMLMediaElement.NETWORK_EMPTY; }
  get NETWORK_IDLE() { return HTMLMediaElement.NETWORK_IDLE; }
  get NETWORK_LOADING() { return HTMLMediaElement.NETWORK_LOADING; }
  get NETWORK_NO_SOURCE() { return HTMLMediaElement.NETWORK_NO_SOURCE; }
  get HAVE_NOTHING() { return HTMLMediaElement.HAVE_NOTHING; }
  get HAVE_METADATA() { return HTMLMediaElement.HAVE_METADATA; }
  get HAVE_CURRENT_DATA() { return HTMLMediaElement.HAVE_CURRENT_DATA; }
  get HAVE_FUTURE_DATA() { return HTMLMediaElement.HAVE_FUTURE_DATA; }
  get HAVE_ENOUGH_DATA() { return HTMLMediaElement.HAVE_ENOUGH_DATA; }
  get paused() { return true; }
  get ended() { return false; }
  get networkState() { return HTMLMediaElement.NETWORK_EMPTY; }
  get readyState() { return HTMLMediaElement.HAVE_NOTHING; }
  get error() { return null; }
  get seeking() { return false; }
  get currentTime() { return 0; }
  set currentTime(v) {}
  get duration() { return NaN; }
  get volume() { return 1; }
  set volume(v) {}
  get muted() { return false; }
  set muted(v) {}
  get src() {
    const raw = this.getAttribute("src");
    if (!raw) return "";
    try { return new URL(raw, this.baseURI || globalThis.location?.href || "about:blank").href; }
    catch (_error) { return raw; }
  }
  set src(v) { this.setAttribute('src', v); }
  get currentSrc() { return ""; }
  get textTracks() {
    return TextTrackList.from(
      Array.from(this.querySelectorAll("track")).map((element) => element.track)
    );
  }
  addTextTrack(kind, label = "", language = "") {
    return new TextTrack(null, String(kind), String(label), String(language));
  }
}
_markNative(HTMLMediaElement.prototype.canPlayType);
_markNative(HTMLMediaElement.prototype.play);
_markNative(HTMLMediaElement.prototype.load);
_markNative(HTMLMediaElement.prototype.pause);
class HTMLVideoElement extends HTMLMediaElement {
  get poster() {
    const raw = this.getAttribute("poster");
    if (!raw) return "";
    try { return new URL(raw, this.baseURI || globalThis.location?.href || "about:blank").href; }
    catch (_error) { return raw; }
  }
  set poster(value) { this.setAttribute("poster", value); }
  get videoWidth() { return 0; }
  get videoHeight() { return 0; }
}
class HTMLAudioElement extends HTMLMediaElement {}
class HTMLTrackElement extends Element {
  static NONE = 0;
  static LOADING = 1;
  static LOADED = 2;
  static ERROR = 3;
  get kind() { return this.getAttribute("kind") || "subtitles"; }
  set kind(value) { this.setAttribute("kind", value); }
  get src() { return this.getAttribute("src") || ""; }
  set src(value) { this.setAttribute("src", value); }
  get srclang() { return this.getAttribute("srclang") || ""; }
  set srclang(value) { this.setAttribute("srclang", value); }
  get label() { return this.getAttribute("label") || ""; }
  set label(value) { this.setAttribute("label", value); }
  get default() { return this.hasAttribute("default"); }
  set default(value) { value ? this.setAttribute("default", "") : this.removeAttribute("default"); }
  get readyState() { return HTMLTrackElement.LOADED; }
  get track() {
    if (!this._textTrack) {
      this._textTrack = new TextTrack(this, this.kind, this.label, this.srclang);
    }
    return this._textTrack;
  }
}
globalThis.HTMLMediaElement = HTMLMediaElement;
globalThis.HTMLVideoElement = HTMLVideoElement;
globalThis.HTMLAudioElement = HTMLAudioElement;
globalThis.HTMLTrackElement = HTMLTrackElement;
globalThis.TextTrack = TextTrack;
globalThis.TextTrackList = TextTrackList;
globalThis.TextTrackCue = TextTrackCue;
globalThis.TextTrackCueList = TextTrackCueList;
globalThis.VTTCue = VTTCue;

function _elementClassFor(nid) {
  const tag = _domParse("tag_name", nid);
  // HTML tagName values are ASCII-uppercase. Foreign SVG names retain their
  // case, so keep the common HTML path fast and only inspect the native
  // namespace for possible SVG wrappers.
  if (tag && tag !== tag.toUpperCase()
      && _domParse("namespace_uri", nid) === "http://www.w3.org/2000/svg") {
    if (tag === "path" && globalThis.SVGPathElement) return globalThis.SVGPathElement;
    if (tag === "svg" && globalThis.SVGSVGElement) return globalThis.SVGSVGElement;
    if (globalThis.SVGElement) return globalThis.SVGElement;
  }
  if (tag === "FORM" && globalThis.HTMLFormElement) return globalThis.HTMLFormElement;
  if (tag === "IMG") return HTMLImageElement;
  if (tag === "CANVAS" && globalThis.HTMLCanvasElement) return globalThis.HTMLCanvasElement;
  if (tag === "AUDIO") return HTMLAudioElement;
  if (tag === "VIDEO") return HTMLVideoElement;
  if (tag === "TRACK") return HTMLTrackElement;
  return Element;
}
function _elementClassForKnownName(namespace, qualifiedName) {
  const localName = qualifiedName.includes(":")
    ? qualifiedName.slice(qualifiedName.indexOf(":") + 1)
    : qualifiedName;
  if (namespace === "http://www.w3.org/2000/svg") {
    if (localName === "path" && globalThis.SVGPathElement) return globalThis.SVGPathElement;
    if (localName === "svg" && globalThis.SVGSVGElement) return globalThis.SVGSVGElement;
    if (globalThis.SVGElement) return globalThis.SVGElement;
  }
  if (namespace === "http://www.w3.org/1999/xhtml") {
    const tag = localName.toUpperCase();
    if (tag === "FORM" && globalThis.HTMLFormElement) return globalThis.HTMLFormElement;
    if (tag === "IMG") return HTMLImageElement;
    if (tag === "CANVAS" && globalThis.HTMLCanvasElement) return globalThis.HTMLCanvasElement;
    if (tag === "AUDIO") return HTMLAudioElement;
    if (tag === "VIDEO") return HTMLVideoElement;
    if (tag === "TRACK") return HTMLTrackElement;
  }
  return Element;
}
function _wrap(nid) {
  if (nid < 0 || nid === null || nid === undefined || isNaN(nid)) return null;
  if (_cache.has(nid)) return _cache.get(nid);
  const t = +_dom("node_type", nid);
  let n;
  if (t === 1) { const C = _elementClassFor(nid); n = new C(nid); }
  else if (t === 3) n = new Text(nid);
  else if (t === 8) n = new Comment(nid);
  else if (t === 9) n = new Document(nid);
  else n = new Node(nid);
  _cache.set(nid, n);
  return n;
}
function _wrapEl(nid) {
  if (nid < 0 || nid === null || nid === undefined || isNaN(nid)) return null;
  if (_cache.has(nid)) return _cache.get(nid);
  const C = _elementClassFor(nid);
  const n = new C(nid);
  _cache.set(nid, n);
  return n;
}

globalThis._wrap = _wrap;
globalThis.self = globalThis;

globalThis.document = null;
function _resolveUrl(url) {
  if (!url) return url;
  if (url.startsWith('http://') || url.startsWith('https://') || url.startsWith('about:')) return url;
  try { return new URL(url, _domParse("document_url") || "about:blank").href; } catch(e) { return url; }
}
// `__virtualUrl` is set by `history.pushState`/`replaceState` (and cleared by
// any real navigation). When set, `location.href` and friends read it instead
// of the underlying `document_url`. Without this, client-side routers
// (Next.js, React Router, vue-router) call `pushState` but the URL never
// changes, so their `useLocation` hooks return the wrong path and the UI
// freezes on the original route.
globalThis.__virtualUrl = null;
function __currentUrl() {
  return globalThis.__virtualUrl || _domParse("document_url") || "about:blank";
}
globalThis.location = {
  get href() { return __currentUrl(); },
  set href(url) { var r = _resolveUrl(url); globalThis.__virtualUrl = r; Deno.core.ops.op_navigate(r, 'GET', ''); },
  get origin() { try { return new URL(this.href).origin; } catch { return ""; } },
  get protocol() { try { return new URL(this.href).protocol; } catch { return ""; } },
  get host() { try { return new URL(this.href).host; } catch { return ""; } },
  get hostname() { try { return new URL(this.href).hostname; } catch { return ""; } },
  get pathname() { try { return new URL(this.href).pathname; } catch { return "/"; } },
  get search() { try { return new URL(this.href).search; } catch { return ""; } },
  get hash() { try { return new URL(this.href).hash; } catch { return ""; } },
  get port() { try { return new URL(this.href).port; } catch { return ""; } },
  toString() { return this.href; },
  assign(url) { var r = _resolveUrl(url); globalThis.__virtualUrl = r; Deno.core.ops.op_navigate(r, 'GET', ''); },
  reload() { var r = _resolveUrl(this.href); globalThis.__virtualUrl = r; Deno.core.ops.op_navigate(r, 'GET', ''); },
  replace(url) { var r = _resolveUrl(url); globalThis.__virtualUrl = r; Deno.core.ops.op_navigate(r, 'GET', ''); },
};
const _locationObj = globalThis.location;
Object.defineProperty(globalThis, 'location', {
  get() { return _locationObj; },
  set(url) { var r = _resolveUrl(String(url)); globalThis.__virtualUrl = r; Deno.core.ops.op_navigate(r, 'GET', ''); },
  configurable: false,
  enumerable: true,
});

globalThis.window = globalThis;
globalThis.self = globalThis;
globalThis.top = globalThis;
globalThis.parent = globalThis;
globalThis.frames = globalThis;
globalThis.frameElement = null;
globalThis.length = 0;

// HTML spec exposes on* event handler IDL attributes via the GlobalEventHandlers
// mixin on Window, Document, and HTMLElement. Libraries feature-detect the modern
// event path through these: jQuery checks `("on" + ev) in window`, and React
// decides whether the `input` event is supported via `("oninput" in document)`.
// When that check fails React falls back to a legacy change-detection path that
// never fires onChange for controlled inputs (issue #324). Initialising these to
// null on all three targets makes the checks match real browsers. On Document and
// Element they are non-enumerable so they don't surface in `for..in` over nodes.
for (const _ev of [
  "abort","beforeprint","beforeunload","blur","cancel","canplay","canplaythrough",
  "change","click","close","contextmenu","cuechange","dblclick","drag","dragend",
  "dragenter","dragleave","dragover","dragstart","drop","durationchange","emptied",
  "ended","error","focus","focusin","focusout","formdata","gotpointercapture",
  "hashchange","input","invalid","keydown","keypress","keyup","languagechange",
  "load","loadeddata","loadedmetadata","loadstart","lostpointercapture","message",
  "mousedown","mouseenter","mouseleave","mousemove","mouseout","mouseover","mouseup",
  "offline","online","pagehide","pageshow","paste","pause","play","playing",
  "pointercancel","pointerdown","pointerenter","pointerleave","pointermove",
  "pointerout","pointerover","pointerup","popstate","progress","ratechange",
  "rejectionhandled","reset","resize","scroll","seeked","seeking","select",
  "stalled","storage","submit","suspend","timeupdate","toggle","unhandledrejection",
  "unload","volumechange","waiting","wheel",
]) {
  const _on = "on" + _ev;
  if (!(_on in globalThis)) globalThis[_on] = null;
  for (const _proto of [Document.prototype, Element.prototype]) {
    if (!(_on in _proto)) {
      Object.defineProperty(_proto, _on, { value: null, writable: true, configurable: true, enumerable: false });
    }
  }
}

globalThis.Window = globalThis.Window || function Window() {};
Object.defineProperty(globalThis.Window, Symbol.hasInstance, {
  value(obj) { return obj === globalThis || (obj && obj.window === obj); },
  configurable: true,
});
// A browser global is a Window object, not merely an object accepted by
// `Window[Symbol.hasInstance]`. Framework environment gates (including Ember's)
// also require the direct identity `self.constructor === Window`; leaving the
// inherited Object constructor makes them enter their server-rendering path
// and hand string selectors to DOM render operations.
Object.defineProperty(globalThis, 'constructor', {
  value: globalThis.Window,
  writable: true,
  configurable: true,
  enumerable: false,
});


// Remove the static _iframeRegistry and replace with dynamic getters.
Object.defineProperty(globalThis, 'length', {
  get() {
    return document.querySelectorAll('iframe').length;
  },
  configurable: true,
  enumerable: true
});

// Since we cannot define a Proxy on globalThis easily, we'll define a reasonable number of indexed getters.
for (let i = 0; i < 50; i++) {
  Object.defineProperty(globalThis, i, {
    get() {
      const iframes = document.querySelectorAll('iframe');
      if (i < iframes.length) {
        return iframes[i].contentWindow;
      }
      return undefined;
    },
    configurable: true,
    enumerable: false
  });
}

// Navigator constructor so that typeof Navigator !== 'undefined' and
// navigatorPrototype checks don't throw a ReferenceError.
function Navigator() {}
_markNative(Navigator);

// PluginArray must exist before navigator is built so the plugins getter can use it.
function PluginArray(items) {
  for (var _pi = 0; _pi < items.length; _pi++) this[_pi] = items[_pi];
  this.length = items.length;
}
PluginArray.prototype = Object.create(Array.prototype);
PluginArray.prototype.constructor = PluginArray;
PluginArray.prototype.item = function(i) { return this[i] || null; };
PluginArray.prototype.namedItem = function(name) {
  for (var _pi = 0; _pi < this.length; _pi++) {
    if (this[_pi].name === name) return this[_pi];
  }
  return null;
};
PluginArray.prototype.refresh = function() {};
PluginArray.prototype[Symbol.iterator] = Array.prototype[Symbol.iterator];
Object.defineProperty(PluginArray.prototype, Symbol.toStringTag, {value: 'PluginArray', configurable: true});
_markNative(PluginArray);
_markNative(PluginArray.prototype.item);
_markNative(PluginArray.prototype.namedItem);
_markNative(PluginArray.prototype.refresh);

// Plugin / MimeType / MimeTypeArray global interfaces. Chrome exposes these as
// global constructors; their absence threw "ReferenceError: Plugin is not
// defined" in site bundles that reference them (issue #305). Plain function
// declarations (no globalThis assignment) so they survive the V8 snapshot, the
// same pattern PluginArray uses.
function Plugin(name, filename, description, mimeTypes) {
  this.name = name;
  this.filename = filename;
  this.description = description;
  var mt = mimeTypes || [];
  for (var _i = 0; _i < mt.length; _i++) this[_i] = mt[_i];
  this.length = mt.length;
}
Plugin.prototype.item = function(i) { return this[i] || null; };
Plugin.prototype.namedItem = function(name) {
  for (var _i = 0; _i < this.length; _i++) if (this[_i] && this[_i].type === name) return this[_i];
  return null;
};
Plugin.prototype[Symbol.iterator] = Array.prototype[Symbol.iterator];
Object.defineProperty(Plugin.prototype, Symbol.toStringTag, {value: 'Plugin', configurable: true});
_markNative(Plugin);
_markNative(Plugin.prototype.item);
_markNative(Plugin.prototype.namedItem);

function MimeType(type, description, suffixes, plugin) {
  this.type = type;
  this.description = description;
  this.suffixes = suffixes;
  this.enabledPlugin = plugin || null;
}
Object.defineProperty(MimeType.prototype, Symbol.toStringTag, {value: 'MimeType', configurable: true});
_markNative(MimeType);

function MimeTypeArray(items) {
  for (var _i = 0; _i < items.length; _i++) this[_i] = items[_i];
  this.length = items.length;
}
MimeTypeArray.prototype.item = function(i) { return this[i] || null; };
MimeTypeArray.prototype.namedItem = function(name) {
  for (var _i = 0; _i < this.length; _i++) if (this[_i] && this[_i].type === name) return this[_i];
  return null;
};
MimeTypeArray.prototype[Symbol.iterator] = Array.prototype[Symbol.iterator];
Object.defineProperty(MimeTypeArray.prototype, Symbol.toStringTag, {value: 'MimeTypeArray', configurable: true});
_markNative(MimeTypeArray);
_markNative(MimeTypeArray.prototype.item);
_markNative(MimeTypeArray.prototype.namedItem);

class NetworkInformation {
  constructor() { this._listeners = Object.create(null); }
  get downlink() { return 10; }
  get downlinkMax() { return Infinity; }
  get effectiveType() { return '4g'; }
  get rtt() { return 50; }
  get saveData() { return false; }
  get type() { return 'wifi'; }
  get onchange() { return this._onchange || null; }
  set onchange(v) { this._onchange = typeof v === "function" ? v : null; }
  get ontypechange() { return this._ontypechange || null; }
  set ontypechange(v) { this._ontypechange = typeof v === "function" ? v : null; }
  addEventListener(type, listener) {
    if (typeof listener !== "function") return;
    (this._listeners[type] || (this._listeners[type] = [])).push(listener);
  }
  removeEventListener(type, listener) {
    const listeners = this._listeners[type];
    if (listeners) this._listeners[type] = listeners.filter((item) => item !== listener);
  }
  dispatchEvent(event) {
    if (!event || !event.type) return true;
    for (const listener of this._listeners[event.type] || []) {
      try { listener.call(this, event); } catch (error) { console.error(error); }
    }
    const handler = this["on" + event.type];
    if (typeof handler === "function") {
      try { handler.call(this, event); } catch (error) { console.error(error); }
    }
    return !event.defaultPrevented;
  }
}
_markNative(NetworkInformation);
globalThis.NetworkInformation = NetworkInformation;

globalThis.ContentIndex = class ContentIndex {};

function _chromeMajor() {
  var m = (globalThis.__obscura_ua || '').match(/Chrome\/(\d+)/);
  return m ? (m[1] | 0) : 145;
}
// Chromium derives the sec-ch-ua GREASE brand, version, and brand order
// deterministically from the Chrome major version
// (components/embedder_support/user_agent_utils.cc). Replicating it keeps
// sec-ch-ua and userAgentData exact for every profile version rather than
// hardcoding one static token.
var _GREASE_CHARS = [' ', '(', ':', '-', '.', '/', ')', ';', '=', '?', '_'];
var _GREASE_VER = ['8', '99', '24'];
var _BRAND_PERMS = [[0,1,2],[0,2,1],[1,0,2],[1,2,0],[2,0,1],[2,1,0]];
function _uaBrands() {
  var seed = _chromeMajor();
  var grease = {
    brand: 'Not' + _GREASE_CHARS[seed % 11] + 'A' + _GREASE_CHARS[(seed + 1) % 11] + 'Brand',
    version: _GREASE_VER[seed % 3],
  };
  var ordered = [
    grease,
    {brand: 'Chromium', version: String(seed)},
    {brand: 'Google Chrome', version: String(seed)},
  ];
  var p = _BRAND_PERMS[seed % 6];
  return [ordered[p[0]], ordered[p[1]], ordered[p[2]]];
}

// Fingerprint surfaces (UA, plugins, webdriver, etc.) live on the prototype
// hop below, not as own props here: own accessors are a bot tell.
globalThis.navigator = {
  onLine: true, cookieEnabled: true,
  maxTouchPoints: 0,
  vendor: "Google Inc.", product: "Gecko", productSub: "20030107",
  doNotTrack: null,
  connection: new NetworkInformation(),
  pdfViewerEnabled: true,
  userAgentData: {
    mobile: false,
    get brands() { return _uaBrands(); },
    get platform() { return globalThis.__obscura_ua_platform || "Windows"; },
    getHighEntropyValues(hints) {
      var brands = _uaBrands();
      return Promise.resolve({
        architecture: "x86",
        bitness: "64",
        brands: brands,
        fullVersionList: brands.map(function(b) { return {brand: b.brand, version: b.version + ".0.0.0"}; }),
        mobile: false,
        model: "",
        platform: globalThis.__obscura_ua_platform || "Windows",
        platformVersion: globalThis.__obscura_ua_platform_version || "15.0.0",
        uaFullVersion: _chromeMajor() + ".0.0.0",
        wow64: false,
      });
    },
    toJSON() { return {brands:this.brands,mobile:this.mobile,platform:this.platform}; },
  },
  serviceWorker: { ready: Promise.resolve(), register(){return Promise.resolve();}, getRegistrations(){return Promise.resolve([]);}, controller: null, oncontrollerchange: null, onmessage: null, addEventListener(){}, removeEventListener(){}, dispatchEvent(){return true;} },
  mediaDevices: {
    enumerateDevices() {
      return Promise.resolve([
        {deviceId:"default",kind:"audioinput",label:"",groupId:"default"},
        {deviceId:"comms",kind:"audioinput",label:"",groupId:"comms"},
        {deviceId:"default",kind:"audiooutput",label:"",groupId:"default"},
        {deviceId:"",kind:"videoinput",label:"",groupId:""},
      ]);
    },
    getUserMedia() { return Promise.reject(new DOMException("NotAllowedError")); },
    getDisplayMedia() { return Promise.reject(new DOMException("NotAllowedError")); },
    addEventListener(){}, removeEventListener(){},
  },
  clipboard: { writeText(){return Promise.resolve();}, readText(){return Promise.resolve("");} },
  permissions: { query(params){
    var n = params && params.name;
    // Chrome defaults privacy-sensitive permissions to "prompt", not "granted";
    // returning "granted" for camera/microphone is a bot tell.
    if (n === 'notifications') return Promise.resolve({state: (globalThis.Notification && Notification.permission === 'granted') ? 'granted' : 'prompt', onchange: null});
    if (n === 'geolocation' || n === 'camera' || n === 'microphone' || n === 'midi') return Promise.resolve({state: 'prompt', onchange: null});
    return Promise.resolve({state: 'granted', onchange: null});
  } },
  getBattery() { return Promise.resolve({ charging: _fp('batteryCharging'), chargingTime: _fp('batteryCharging') ? 0 : Infinity, dischargingTime: _fp('batteryCharging') ? Infinity : Math.floor(3600 + _fpRand(250) * 7200), level: _fp('batteryLevel'), addEventListener(){} }); },
  getGamepads() { return []; },
  sendBeacon() { return true; },
  javaEnabled() { return false; },
  geolocation: {
    getCurrentPosition(success, error) {
      const coords = {
        latitude: (globalThis.__obscura_geo_lat ?? 50.1109) + (_fpRand(500) - 0.5) * 0.1,
        longitude: (globalThis.__obscura_geo_lon ?? 8.6821) + (_fpRand(501) - 0.5) * 0.1,
        accuracy: 10 + _fpRand(502) * 40,
        altitude: null,
        altitudeAccuracy: null,
        heading: null,
        speed: null,
      };
      const pos = { coords, timestamp: Date.now() };
      if (typeof success === 'function') success(pos);
    },
    watchPosition(success, error) {
      if (typeof success === 'function') {
        const coords = {
          latitude: (globalThis.__obscura_geo_lat ?? 50.1109) + (_fpRand(503) - 0.5) * 0.1,
          longitude: (globalThis.__obscura_geo_lon ?? 8.6821) + (_fpRand(504) - 0.5) * 0.1,
          accuracy: 10 + _fpRand(505) * 40,
          altitude: null,
          altitudeAccuracy: null,
          heading: null,
          speed: null,
        };
        success({ coords, timestamp: Date.now() });
      }
      return 0;
    },
    clearWatch() {},
  },
  storage: {
    estimate() { return Promise.resolve({ quota: 5000000000, usage: Math.floor(_fpRand(640) * 100000000) }); },
    persist() { return Promise.resolve(false); },
    persisted() { return Promise.resolve(false); },
  },
};

// Put spoofed navigator props on a thin prototype above Navigator.prototype
// so hasOwnProperty/getOwnPropertyDescriptor on the instance match Chrome.
// Getters read __obscura_* lazily (snapshot vs per-page) and are _markNative'd.
(function() {
  var _navProto = Object.create(Navigator.prototype);

  function defGetter(key, fn) {
    _markNative(fn);
    Object.defineProperty(_navProto, key, {
      get: fn, set: undefined, enumerable: true, configurable: true,
    });
  }

  defGetter('webdriver', function() { return false; });
  defGetter('userAgent', function() {
    return globalThis.__obscura_ua ||
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 " +
      "(KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";
  });
  defGetter('appVersion', function() {
    return (globalThis.__obscura_ua ||
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 " +
      "(KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36").replace('Mozilla/', '');
  });
  defGetter('platform', function() {
    return globalThis.__obscura_platform || "Win32";
  });
  defGetter('language', function() { return "en-US"; });
  defGetter('languages', function() { return ["en-US", "en"]; });

  // Cache plugins/mimeTypes so navigator.plugins === navigator.plugins.
  var _plugins = new PluginArray([
    new Plugin("PDF Viewer", "internal-pdf-viewer", "Portable Document Format", []),
    new Plugin("Chrome PDF Viewer", "internal-pdf-viewer", "Portable Document Format", []),
    new Plugin("Chromium PDF Viewer", "internal-pdf-viewer", "Portable Document Format", []),
    new Plugin("Microsoft Edge PDF Viewer", "internal-pdf-viewer", "Portable Document Format", []),
    new Plugin("WebKit built-in PDF", "internal-pdf-viewer", "Portable Document Format", []),
  ]);
  var _mimeTypes = new MimeTypeArray([
    new MimeType("application/pdf", "Portable Document Format", "pdf", null),
    new MimeType("text/pdf", "Portable Document Format", "pdf", null),
  ]);
  defGetter('plugins', function() { return _plugins; });
  defGetter('mimeTypes', function() { return _mimeTypes; });

  // Values set per-page by __obscura_init (avoids own data props on navigator).
  defGetter('hardwareConcurrency', function() { return globalThis.__obscura_hw || 8; });
  defGetter('deviceMemory', function() { return globalThis.__obscura_mem || 8; });

  _navProto.share = _markNative(function share(data) {
    return Promise.reject(new DOMException('Not allowed', 'NotAllowedError'));
  });
  _navProto.canShare = _markNative(function canShare() { return false; });

  Object.setPrototypeOf(globalThis.navigator, _navProto);
})();

globalThis.chrome = {
  app: { isInstalled: false, InstallState: { DISABLED: "disabled", INSTALLED: "installed", NOT_INSTALLED: "not_installed" }, RunningState: { CANNOT_RUN: "cannot_run", READY_TO_RUN: "ready_to_run", RUNNING: "running" } },
  runtime: { OnInstalledReason: {}, OnRestartRequiredReason: {}, PlatformArch: {}, PlatformNaclArch: {}, PlatformOs: {}, RequestUpdateCheckStatus: {}, connect() { throw new Error("Could not establish connection. Receiving end does not exist."); }, sendMessage() { throw new Error("Could not establish connection. Receiving end does not exist."); } },
  csi() {
    const t = Date.now();
    return { onloadT: t, startE: t - Math.floor(100 + _fpRand(610) * 200), pageT: 0, tran: 5, flashVersion: "" };
  },
  loadTimes() {
    const t = Date.now() / 1000;
    const request = t - 0.5 - _fpRand(611) * 0.5;
    const startLoad = request + 0.05 + _fpRand(612) * 0.02;
    const commit = request + 0.3 + _fpRand(613) * 0.4;
    const finishDoc = commit + 0.1 + _fpRand(614) * 0.2;
    const finish = finishDoc + 0.05 + _fpRand(615) * 0.1;
    const firstPaint = commit + 0.03 + _fpRand(616) * 0.1;
    const navTypes = ["BackForward","Reload","Link","Other"];
    return {
      requestTime: request, startLoadTime: startLoad * 1000, commitLoadTime: commit * 1000,
      finishDocumentLoadTime: finishDoc * 1000, finishLoadTime: finish * 1000,
      firstPaintTime: firstPaint * 1000, firstPaintAfterLoadTime: 0,
      navigationType: navTypes[Math.floor(_fpRand(617) * 4)],
      wasFetchedViaSpdy: false, wasNpnNegotiated: false,
      npnNegotiatedProtocol: "http/1.1",
      wasAlternateProtocolAvailable: false, connectionInfo: "http/1.1",
    };
  },
};

globalThis.Notification = class Notification {
  static permission = "default";
  static requestPermission() { return Promise.resolve(Notification.permission); }
  constructor() {}
};

globalThis.WebGLRenderingContext = class WebGLRenderingContext {};
globalThis.WebGL2RenderingContext = class WebGL2RenderingContext {};

class Screen {
  constructor(w, h, availW, availH) {
    this._w = w; this._h = h;
    this._availW = availW === undefined ? w : availW;
    this._availH = availH === undefined ? h - 40 : availH;
    this.colorDepth = 24; this.pixelDepth = 24; this.availTop = 0; this.availLeft = 0;
    this.orientation = {type:'landscape-primary',angle:0,addEventListener(){},removeEventListener(){},dispatchEvent(){return true;}};
  }
  get width() { return this._w; }
  get height() { return this._h; }
  get availWidth() { return this._availW; }
  get availHeight() { return this._availH; }
}
['width','height','availWidth','availHeight'].forEach(function(k) {
  var d = Object.getOwnPropertyDescriptor(Screen.prototype, k);
  if (d && d.get) _markNative(d.get);
});
globalThis.Screen = Screen;
globalThis.screen = new Screen(1920, 1080);
function _applyScreenSize(w, h, emulated) {
  if (globalThis.screen instanceof Screen) {
    globalThis.screen._w = w;
    globalThis.screen._h = h;
    globalThis.screen._availW = w;
    globalThis.screen._availH = emulated ? h : h - 40;
  } else {
    globalThis.screen = new Screen(w, h, w, emulated ? h : h - 40);
  }
}
globalThis.__obscura_set_screen_override = function(w, h, emulated) {
  globalThis.__obscura_screen_emulated = !!emulated;
  if (Number.isFinite(w) && Number.isFinite(h) && w > 0 && h > 0) {
    globalThis.__obscura_screen_w = w;
    globalThis.__obscura_screen_h = h;
    _applyScreenSize(w, h, !!emulated);
    return;
  }
  delete globalThis.__obscura_screen_w;
  delete globalThis.__obscura_screen_h;
  const fallback = _fp('screen');
  _applyScreenSize(fallback[0], fallback[1], !!emulated);
};
globalThis.visualViewport = { width:1920, height:1000, offsetLeft:0, offsetTop:0, scale:1, addEventListener(){}, removeEventListener(){} };
globalThis.devicePixelRatio = 1;
globalThis.innerWidth = 1920; globalThis.innerHeight = 1000;
globalThis.outerWidth = 1920; globalThis.outerHeight = 1080;
globalThis.scrollX = 0; globalThis.scrollY = 0;
globalThis.pageXOffset = 0; globalThis.pageYOffset = 0;

globalThis.__fetchInterceptEnabled = false;
globalThis.__fetchInterceptCallback = null; // Set by CDP to handle paused requests

// charCode -> 6-bit value reverse table for base64 decode. -1 for any byte not
// in the standard alphabet, which mirrors String.indexOf's miss exactly, so the
// bitmath below stays byte-identical to the old indexOf path including on
// malformed input. Built once at module load.
const _B64_DECODE_TABLE = (function () {
  const t = new Int16Array(128).fill(-1);
  const a = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  for (let i = 0; i < 64; i++) t[a.charCodeAt(i)] = i;
  return t;
})();

function _base64ToUint8Array(b64) {
  const clean = String(b64 || '').replace(/[\r\n\s]/g, '');
  if (!clean) return new Uint8Array();
  const T = _B64_DECODE_TABLE;
  const padding = clean.endsWith('==') ? 2 : (clean.endsWith('=') ? 1 : 0);
  const bytes = new Uint8Array((clean.length * 3 >> 2) - padding);
  let out = 0;
  for (let i = 0; i < clean.length; i += 4) {
    // charCodeAt avoids the per-char substring alloc; T[code] replaces the
    // O(64) indexOf scan. Out-of-range (NaN or code >= 128) folds to -1, and
    // `=== 61` is `=== '='`, so results match the old code exactly.
    const ca = clean.charCodeAt(i);     const a = ca < 128 ? T[ca] : -1;
    const cb = clean.charCodeAt(i + 1); const b = cb < 128 ? T[cb] : -1;
    const cc = clean.charCodeAt(i + 2); const c = cc === 61 ? 0 : (cc < 128 ? T[cc] : -1);
    const cd = clean.charCodeAt(i + 3); const d = cd === 61 ? 0 : (cd < 128 ? T[cd] : -1);
    const n = (a << 18) | (b << 12) | (c << 6) | d;
    if (out < bytes.length) bytes[out++] = (n >> 16) & 0xff;
    if (out < bytes.length) bytes[out++] = (n >> 8) & 0xff;
    if (out < bytes.length) bytes[out++] = n & 0xff;
  }
  return bytes;
}

function _bodyToUint8Array(body) {
  if (body == null) return new Uint8Array();
  if (body instanceof Uint8Array) return body;
  if (body instanceof ArrayBuffer) return new Uint8Array(body);
  if (ArrayBuffer.isView(body)) return new Uint8Array(body.buffer, body.byteOffset, body.byteLength);
  // obscura's Blob materializes its data into _bytes in the constructor.
  if (body._bytes instanceof Uint8Array) return body._bytes;
  return new TextEncoder().encode(String(body));
}

function _arrayBufferFromBytes(bytes) {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
}

function _installWasmStreamingFallback() {
  if (typeof WebAssembly === 'undefined') return;
  if (WebAssembly.instantiateStreaming && WebAssembly.instantiateStreaming.__obscuraFallback) return;
  const nativeInstantiateStreaming = WebAssembly.instantiateStreaming;
  const fallback = async function instantiateStreaming(source, imports) {
    const response = await source;
    if (response && typeof response.arrayBuffer === 'function') {
      return WebAssembly.instantiate(await response.arrayBuffer(), imports);
    }
    if (typeof nativeInstantiateStreaming === 'function') {
      return nativeInstantiateStreaming.call(WebAssembly, response, imports);
    }
    return WebAssembly.instantiate(response, imports);
  };
  fallback.__obscuraFallback = true;
  WebAssembly.instantiateStreaming = fallback;
}
_installWasmStreamingFallback();

// Serialize a FormData into a multipart/form-data body the way a browser does
// when it is passed as fetch()/XHR body. The previous shim did String(body),
// so a FormData became the literal "[object Object]" and the multipart payload
// (with its boundary) was lost; servers replied "Invalid boundary for
// multipart/form-data" (e.g. the AWS WAF challenge /mp_verify POST).
function _formDataToMultipart(fd) {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let bnd = '----WebKitFormBoundary';
  for (let i = 0; i < 16; i++) bnd += chars[Math.floor(Math.random() * chars.length)];
  let out = '';
  const entries = fd._d || [];
  for (let i = 0; i < entries.length; i++) {
    const k = entries[i][0], v = entries[i][1];
    out += '--' + bnd + '\r\n';
    if (v != null && typeof v === 'object' && v._bytes != null) {
      out += 'Content-Disposition: form-data; name="' + k + '"; filename="' + (v.name || 'blob') + '"\r\n';
      out += 'Content-Type: ' + (v.type || 'application/octet-stream') + '\r\n\r\n';
      try { out += new TextDecoder().decode(v._bytes); } catch (e) {}
      out += '\r\n';
    } else {
      out += 'Content-Disposition: form-data; name="' + k + '"\r\n\r\n' + String(v) + '\r\n';
    }
  }
  out += '--' + bnd + '--\r\n';
  return { boundary: bnd, body: out };
}

// Coerce a fetch()/XHR body into the string op_fetch_url expects, attaching a
// Content-Type header for body types that need one (FormData, URLSearchParams).
function _serializeBody(initBody, headers) {
  if (initBody == null || initBody === '') return '';
  if (initBody instanceof FormData) {
    const mp = _formDataToMultipart(initBody);
    headers['Content-Type'] = 'multipart/form-data; boundary=' + mp.boundary;
    return mp.body;
  }
  if (initBody instanceof URLSearchParams) {
    if (!Object.keys(headers).some(k => k.toLowerCase() === 'content-type')) {
      headers['Content-Type'] = 'application/x-www-form-urlencoded;charset=UTF-8';
    }
    return initBody.toString();
  }
  if (typeof Blob !== 'undefined' && initBody instanceof Blob) {
    if (initBody.type && !Object.keys(headers).some(k => k.toLowerCase() === 'content-type')) {
      headers['Content-Type'] = initBody.type;
    }
    return _bytesToBinaryString(_bodyToUint8Array(initBody));
  }
  if (typeof ArrayBuffer !== 'undefined' && initBody instanceof ArrayBuffer) {
    const bytes = new Uint8Array(initBody);
    let s = ''; for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    return s;
  }
  if (typeof ArrayBuffer !== 'undefined' && ArrayBuffer.isView(initBody) && initBody.buffer instanceof ArrayBuffer) {
    const bytes = new Uint8Array(initBody.buffer, initBody.byteOffset, initBody.byteLength);
    let s = ''; for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    return s;
  }
  return typeof initBody === 'string' ? initBody : String(initBody);
}

globalThis.fetch = async (input, init = {}) => {
  init = init || {};
  let url = typeof input === "string"
    ? input
    : (input instanceof Request
      ? input.url
      : ((typeof URL === 'function' && input instanceof URL) ? input.href : (input?.url || input?.href || String(input || ""))));
  if (url && !url.includes('://')) {
    try {
      const base = _domParse("document_url") || "about:blank";
      url = new URL(url, base).href;
    } catch(e) { /* keep as-is if URL resolution fails */ }
  }
  const method = init.method || (input instanceof Request ? input.method : "GET");
  let _h = init.headers instanceof Headers ? Object.fromEntries(init.headers.entries()) : (init.headers || {});
  const body = _serializeBody(init.body, _h);
  const hdrs = JSON.stringify(_h);
  const fetchMode = init.mode || (input instanceof Request ? input.mode : "cors");
  const fetchCredentials = init.credentials !== undefined
    ? String(init.credentials)
    : (input instanceof Request ? input.credentials : "same-origin");
  if (fetchCredentials !== "omit" && fetchCredentials !== "same-origin" && fetchCredentials !== "include") {
    throw new TypeError("Failed to execute 'fetch': '" + fetchCredentials + "' is not a valid RequestCredentials value");
  }
  const pageOrigin = (function() { try { const u = new URL(_domParse("document_url") || "about:blank"); return u.origin; } catch(e) { return ""; } })();
  const raw = await Deno.core.ops.op_fetch_url(url, method, hdrs, body, pageOrigin, fetchMode, fetchCredentials);
  const parsed = JSON.parse(raw);
  if (parsed.blocked) {
    const err = new TypeError('net::ERR_FAILED');
    err.name = 'AbortError';
    err.__aborted = true;
    throw err;
  }
  if (parsed.corsBlocked) {
    throw new TypeError('Failed to fetch: ' + (parsed.corsError || 'CORS error'));
  }
  const respType = parsed.status === 0 ? "opaque" : (fetchMode === "no-cors" ? "opaque" : "basic");
  const responseBody = parsed.bodyBase64 ? _base64ToUint8Array(parsed.bodyBase64) : (parsed.body || "");
  const response = new Response(responseBody, {
    status: parsed.status,
    statusText: "",
    headers: parsed.headers || {},
    type: respType,
    url: parsed.url || url,
    redirected: false,
  });
  if (parsed.requestId) {
    Object.defineProperty(response, "__obscuraRequestId", {
      value: parsed.requestId,
      configurable: true,
    });
  }
  return response;
};

if (typeof Headers === "undefined") {
  globalThis.Headers = class Headers {
    constructor(init={}) { this._h={}; if(init) { if(init instanceof Headers) { init.forEach((v,k)=>{this._h[k]=v;}); } else if(typeof init==="object") { for(const[k,v]of Object.entries(init)) this._h[k.toLowerCase()]=String(v); } } }
    get(n) { return this._h[n.toLowerCase()]??null; } set(n,v) { this._h[n.toLowerCase()]=String(v); }
    has(n) { return n.toLowerCase() in this._h; } delete(n) { delete this._h[n.toLowerCase()]; }
    append(n,v) { this._h[n.toLowerCase()]=String(v); }
    forEach(cb) { for(const[k,v] of Object.entries(this._h)) cb(v,k,this); }
    entries() { return Object.entries(this._h)[Symbol.iterator](); }
    keys() { return Object.keys(this._h)[Symbol.iterator](); }
    values() { return Object.values(this._h)[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
  };
}

// XMLHttpRequestEventTarget — spec-required ancestor for XHR EventTarget methods.
// zone.js prefers to walk XMLHttpRequestEventTarget.prototype for addEventListener/
// removeEventListener/dispatchEvent descriptors before falling back to XHR.prototype.
class XMLHttpRequestEventTarget {
  addEventListener(type, handler) {
    if (!this._listeners) this._listeners = {};
    if (!this._listeners[type]) this._listeners[type] = [];
    this._listeners[type].push(handler);
  }
  removeEventListener(type, handler) {
    if (this._listeners && this._listeners[type]) {
      this._listeners[type] = this._listeners[type].filter(h => h !== handler);
    }
  }
  dispatchEvent(event) {
    if (!event || !event.type) return false;
    const ev = (typeof event === 'object') ? event : { type: event };
    ev.target = ev.target || this;
    ev.currentTarget = ev.currentTarget || this;
    const type = ev.type;
    const handlers = (this._listeners && this._listeners[type]) || [];
    for (const h of handlers) { try { h.call(this, ev); } catch (e) {} }
    const prop = 'on' + type;
    if (typeof this[prop] === 'function') {
      try { this[prop](ev); } catch (e) {}
    }
    return true;
  }
}
globalThis.XMLHttpRequestEventTarget = XMLHttpRequestEventTarget;
_markNative(XMLHttpRequestEventTarget);
_markNative(XMLHttpRequestEventTarget.prototype.addEventListener);
_markNative(XMLHttpRequestEventTarget.prototype.removeEventListener);
_markNative(XMLHttpRequestEventTarget.prototype.dispatchEvent);

globalThis.XMLHttpRequest = class XMLHttpRequest extends XMLHttpRequestEventTarget {
  static UNSENT = 0;
  static OPENED = 1;
  static HEADERS_RECEIVED = 2;
  static LOADING = 3;
  static DONE = 4;
  UNSENT = 0; OPENED = 1; HEADERS_RECEIVED = 2; LOADING = 3; DONE = 4;

  constructor() {
    super();
    this.readyState = 0;
    this.status = 0;
    this.statusText = "";
    this.responseText = "";
    this.responseXML = null;
    this.responseURL = "";
    this.responseType = "";
    this.response = null;
    this.timeout = 0;
    this.withCredentials = false;
    this.upload = { addEventListener(){}, removeEventListener(){} };
    this._method = "GET";
    this._url = "";
    this._headers = {};
    this._responseHeaders = {};
    this._aborted = false;
    this._listeners = {};
    this.onreadystatechange = null;
    this.onload = null;
    this.onerror = null;
    this.onabort = null;
    this.onprogress = null;
    this.ontimeout = null;
    this.onloadstart = null;
    this.onloadend = null;
  }

  open(method, url, async_) {
    this._method = method;
    this._url = url;
    this._headers = {};
    this._responseHeaders = {};
    this._aborted = false;
    this.status = 0;
    this.statusText = "";
    this.responseText = "";
    this.response = null;
    this._setReadyState(1);
  }

  setRequestHeader(name, value) {
    this._headers[name] = value;
  }

  getResponseHeader(name) {
    const lower = name.toLowerCase();
    for (const [k, v] of Object.entries(this._responseHeaders)) {
      if (k.toLowerCase() === lower) return v;
    }
    return null;
  }

  getAllResponseHeaders() {
    return Object.entries(this._responseHeaders)
      .map(([k, v]) => k + ': ' + v)
      .join('\r\n');
  }

  overrideMimeType(mime) { this._overrideMime = mime; }

  send(body) {
    if (this.readyState !== 1) return;
    if (this._aborted) return;

    const xhr = this;
    this._fireEvent('loadstart');

    let url = this._url;
    if (url && !url.includes('://')) {
      try {
        const base = _domParse("document_url") || "about:blank";
        url = new URL(url, base).href;
      } catch(e) {}
    }

    fetch(url, {
      method: this._method,
      headers: this._headers,
      body: body || undefined,
      mode: 'cors',
      credentials: this.withCredentials ? 'include' : 'same-origin',
    }).then(async (resp) => {
      if (xhr._aborted) return;

      xhr.status = resp.status;
      xhr.statusText = resp.statusText || '';
      xhr.responseURL = resp.url || url;

      if (resp.headers) {
        resp.headers.forEach((v, k) => { xhr._responseHeaders[k] = v; });
      }

      xhr._setReadyState(2); // HEADERS_RECEIVED

      const text = await resp.text();
      if (xhr._aborted) return;

      xhr.responseText = text;
      xhr._setReadyState(3); // LOADING

      switch (xhr.responseType) {
        case 'json':
          try { xhr.response = JSON.parse(text); } catch(e) { xhr.response = null; }
          break;
        case 'text':
        case '':
          xhr.response = text;
          break;
        case 'arraybuffer':
          xhr.response = new TextEncoder().encode(text).buffer;
          break;
        case 'blob':
          xhr.response = new Blob([text]);
          break;
        case 'document':
          xhr.response = text; // simplified
          break;
        default:
          xhr.response = text;
      }

      xhr._setReadyState(4); // DONE
      xhr._fireEvent('load');
      xhr._fireEvent('loadend');
    }).catch((err) => {
      if (xhr._aborted) return;
      xhr.status = 0;
      xhr.readyState = 4;
      xhr._fireEvent('readystatechange');
      if (err && err.__aborted) {
        xhr._aborted = true;
        xhr._fireEvent('abort');
        xhr._fireEvent('loadend');
        if (xhr.onabort) xhr.onabort(err);
      } else {
        xhr._fireEvent('error');
        xhr._fireEvent('loadend');
        if (xhr.onerror) xhr.onerror(err);
      }
    });
  }

  abort() {
    this._aborted = true;
    if (this.readyState > 0 && this.readyState < 4) {
      this._setReadyState(4);
      this._fireEvent('abort');
      this._fireEvent('loadend');
    }
    this.readyState = 0;
  }

  addEventListener(type, handler) {
    if (!this._listeners[type]) this._listeners[type] = [];
    this._listeners[type].push(handler);
  }

  removeEventListener(type, handler) {
    if (this._listeners[type]) {
      this._listeners[type] = this._listeners[type].filter(h => h !== handler);
    }
  }

  // Per WHATWG DOM spec — required by zone.js which patches XHR via
  // Object.getOwnPropertyDescriptor on XMLHttpRequestEventTarget.prototype.
  dispatchEvent(event) {
    if (!event || !event.type) return false;
    const ev = (typeof event === 'object') ? event : { type: event };
    ev.target = ev.target || this;
    ev.currentTarget = ev.currentTarget || this;
    const type = ev.type;
    const handlers = (this._listeners && this._listeners[type]) || [];
    for (const h of handlers) { try { h.call(this, ev); } catch (e) {} }
    const prop = 'on' + type;
    if (typeof this[prop] === 'function') {
      try { this[prop](ev); } catch (e) {}
    }
    return true;
  }

  _setReadyState(state) {
    this.readyState = state;
    this._fireEvent('readystatechange');
    if (this.onreadystatechange) {
      try { this.onreadystatechange(); } catch(e) {}
    }
  }

  _fireEvent(type) {
    const event = { type, target: this, currentTarget: this, bubbles: false };
    const handlers = this._listeners[type] || [];
    for (const h of handlers) { try { h.call(this, event); } catch(e) {} }
    const prop = 'on' + type;
    if (type !== 'readystatechange' && typeof this[prop] === 'function') {
      try { this[prop](event); } catch(e) {}
    }
  }
};
_markNative(XMLHttpRequest);
_markNative(XMLHttpRequest.prototype.open);
_markNative(XMLHttpRequest.prototype.send);
_markNative(XMLHttpRequest.prototype.abort);
_markNative(XMLHttpRequest.prototype.setRequestHeader);
_markNative(XMLHttpRequest.prototype.addEventListener);
_markNative(XMLHttpRequest.prototype.removeEventListener);
_markNative(XMLHttpRequest.prototype.dispatchEvent);
_markNative(XMLHttpRequest.prototype.getResponseHeader);
_markNative(XMLHttpRequest.prototype.getAllResponseHeaders);

// WHATWG URL parsing/serialization is delegated to the Rust `url` crate via
// op_url_parse / op_url_set. The op returns the full component set as JSON; the
// constructor caches it so getters are plain field reads (no per-access op) and
// the hot paths (navigation, fetch, _resolveUrl) stay cheap. Returns null when
// the input is not a valid URL.
function _urlParseOp(url, base) {
  try {
    const s = Deno.core.ops.op_url_parse(String(url), (base === undefined || base === null) ? "" : String(base));
    const c = JSON.parse(s);
    return (c && c.ok) ? c : null;
  } catch (e) { return null; }
}
function _urlSetOp(href, part, value) {
  try {
    const s = Deno.core.ops.op_url_set(String(href), part, String(value));
    const c = JSON.parse(s);
    return (c && c.ok) ? c : null;
  } catch (e) { return null; }
}
// Returns just the resolved absolute URL string (no component JSON), or null on
// failure. Cheaper than _urlParseOp for callers that only need the href.
function _urlResolveOp(href, base) {
  try {
    const r = Deno.core.ops.op_url_resolve(String(href), (base === undefined || base === null) ? "" : String(base));
    return r ? r : null;
  } catch (e) { return null; }
}
if (typeof URL === 'undefined' || !URL.prototype || !URL.__obscura) {
  const _URL = class URL {
    constructor(url, base) {
      const c = _urlParseOp(url, base);
      if (!c) throw new TypeError("Failed to construct 'URL': Invalid URL");
      this._c = c;
      this._sp = null;
    }
    get href() { return this._c.href; }
    set href(v) { const c = _urlParseOp(v, undefined); if (!c) throw new TypeError("Failed to set the 'href' property on 'URL': Invalid URL"); this._c = c; this._refreshSP(); }
    get protocol() { return this._c.protocol; }
    set protocol(v) { this._set('protocol', v); }
    get username() { return this._c.username; }
    set username(v) { this._set('username', v); }
    get password() { return this._c.password; }
    set password(v) { this._set('password', v); }
    get host() { return this._c.host; }
    set host(v) { this._set('host', v); }
    get hostname() { return this._c.hostname; }
    set hostname(v) { this._set('hostname', v); }
    get port() { return this._c.port; }
    set port(v) { this._set('port', v); }
    get pathname() { return this._c.pathname; }
    set pathname(v) { this._set('pathname', v); }
    get search() { return this._c.search; }
    set search(v) { this._set('search', v); this._refreshSP(); }
    get hash() { return this._c.hash; }
    set hash(v) { this._set('hash', v); }
    get origin() { return this._c.origin; }
    get searchParams() {
      if (!this._sp) { this._sp = new URLSearchParams(this._c.search); this._sp._url = this; }
      return this._sp;
    }
    _set(part, value) { const c = _urlSetOp(this._c.href, part, value); if (c) this._c = c; }
    // search changed on the URL side: refresh the bound searchParams contents.
    _refreshSP() { if (this._sp && this._sp._setFromString) this._sp._setFromString(this._c.search); }
    // searchParams mutated: write the serialized query back without re-refreshing.
    _updateSearch(qs) { this._set('search', qs ? ('?' + qs) : ''); }
    toString() { return this._c.href; }
    toJSON() { return this._c.href; }
    static createObjectURL() { return 'blob:null/fake-' + Math.random().toString(36).slice(2); }
    static revokeObjectURL() {}
    // WHATWG URL.parse: like the constructor but returns null instead of throwing.
    static parse(url, base) { const c = _urlParseOp(url, base); if (!c) return null; const u = Object.create(_URL.prototype); u._c = c; u._sp = null; return u; }
    static canParse(url, base) { return _urlParseOp(url, base) !== null; }
  };
  _URL.__obscura = true;
  globalThis.URL = _URL;
}

globalThis.requestIdleCallback = globalThis.requestIdleCallback || function requestIdleCallback(cb, opts) {
  const start = Date.now();
  return setTimeout(() => {
    cb({
      didTimeout: false,
      timeRemaining() { return Math.max(0, 50 - (Date.now() - start)); },
    });
  }, 1);
};
globalThis.cancelIdleCallback = globalThis.cancelIdleCallback || function cancelIdleCallback(id) { clearTimeout(id); };
_markNative(globalThis.requestIdleCallback);
_markNative(globalThis.cancelIdleCallback);

if (typeof Request === 'undefined') {
  globalThis.Request = class Request {
    constructor(input, init = {}) {
      const inputRequest = input instanceof Request ? input : null;
      if (typeof input === 'string') { this.url = input; }
      else if (inputRequest) { this.url = inputRequest.url; init = { ...inputRequest, ...init }; }
      else if (typeof URL === 'function' && input instanceof URL) { this.url = input.href; }
      else { this.url = input?.url || input?.href || String(input); }
      this.method = (init.method || 'GET').toUpperCase();
      this.headers = new Headers(init.headers);
      this.body = init.body || null;
      this.mode = init.mode || 'cors';
      this.credentials = init.credentials !== undefined
        ? String(init.credentials)
        : (inputRequest ? inputRequest.credentials : 'same-origin');
      if (this.credentials !== 'omit' && this.credentials !== 'same-origin' && this.credentials !== 'include') {
        throw new TypeError("Failed to construct 'Request': '" + this.credentials + "' is not a valid RequestCredentials value");
      }
      this.redirect = init.redirect || 'follow';
      this.referrer = init.referrer || '';
      this.signal = init.signal || { aborted: false, addEventListener(){}, removeEventListener(){} };
      this.cache = init.cache || 'default';
    }
    clone() {
      return new Request(this.url, {
        method: this.method,
        headers: this.headers,
        body: this.body,
        mode: this.mode,
        credentials: this.credentials,
        redirect: this.redirect,
        referrer: this.referrer,
        signal: this.signal,
        cache: this.cache,
      });
    }
    async text() { return this.body ? String(this.body) : ''; }
    async json() { return JSON.parse(await this.text()); }
    async arrayBuffer() { return new TextEncoder().encode(await this.text()).buffer; }
    async blob() {
      const ct = this.headers && this.headers.get ? (this.headers.get('content-type') || '') : '';
      return new Blob(this.body != null ? [this.body] : [], { type: ct });
    }
  };
}

// Decode a response body honoring the Content-Type charset, so fetch()/XHR
// over non-UTF-8 resources (GBK, Shift_JIS, ISO-8859-x, ...) return correctly
// decoded text instead of mojibake. The UTF-8 case (the overwhelming majority)
// takes the plain TextDecoder fast path; only an explicit non-UTF-8 charset
// routes through TextDecoder(label), which falls back to UTF-8 on a bad label.
function _decodeBodyWithCharset(bytes, headers) {
  let label = '';
  try {
    const ct = headers && typeof headers.get === 'function' ? (headers.get('content-type') || '') : '';
    const m = /charset\s*=\s*"?([^";]+)"?/i.exec(ct);
    if (m) label = m[1].trim();
  } catch (e) {}
  if (!label || /^utf-?8$/i.test(label)) return new TextDecoder().decode(bytes);
  try { return new TextDecoder(label).decode(bytes); }
  catch (e) { return new TextDecoder().decode(bytes); }
}

if (typeof Response === 'undefined') {
  globalThis.Response = class Response {
    constructor(body, init = {}) {
      this._bodyBytes = _bodyToUint8Array(body); this.status = init.status || 200; this.statusText = init.statusText || '';
      this.ok = this.status >= 200 && this.status < 300;
      this.headers = new Headers(init.headers);
      this.type = init.type || 'basic'; this.url = init.url || ''; this.redirected = !!init.redirected;
    }
    async text() { return _decodeBodyWithCharset(this._bodyBytes, this.headers); }
    async json() { return JSON.parse(await this.text()); }
    async arrayBuffer() { return _arrayBufferFromBytes(this._bodyBytes); }
    async blob() { return new Blob([this._bodyBytes]); }
    clone() { return new Response(this._bodyBytes, { status: this.status, statusText: this.statusText, headers: this.headers, type: this.type, url: this.url, redirected: this.redirected }); }
    static error() { return new Response(null, { status: 0 }); }
    static redirect(url, status) { return new Response(null, { status: status || 302, headers: { Location: url } }); }
    static json(data, init) { return new Response(JSON.stringify(data), { ...init, headers: { 'content-type': 'application/json', ...(init?.headers || {}) } }); }
  };
}

if (!Element.prototype.replaceWith) {
  // _convertNodes turns any non-node argument (numbers, booleans, null, …) into
  // a Text node via String(n), matching the spec and append()/prepend(); the
  // old `typeof n === 'string'` check corrupted insert_before for other types.
  Element.prototype.replaceWith = function(...nodes) {
    const parent = this.parentNode;
    if (!parent) return;
    for (const n of _convertNodes(nodes)) parent.insertBefore(n, this);
    parent.removeChild(this);
  };
  _markNative(Element.prototype.replaceWith);
}
if (!Element.prototype.before) {
  Element.prototype.before = function(...nodes) {
    const parent = this.parentNode;
    if (!parent) return;
    for (const n of _convertNodes(nodes)) parent.insertBefore(n, this);
  };
  _markNative(Element.prototype.before);
}
if (!Element.prototype.after) {
  Element.prototype.after = function(...nodes) {
    const parent = this.parentNode;
    if (!parent) return;
    const ref = this.nextSibling;
    for (const n of _convertNodes(nodes)) parent.insertBefore(n, ref);
  };
  _markNative(Element.prototype.after);
}

// ChildNode mixin: also mix before/after/replaceWith/remove into
// CharacterData.prototype (covers Text, Comment, ProcessingInstruction).
// These are the same implementations as Element.prototype — frameworks
// (Svelte 5, Vue, Lit) anchor on Comment/Text nodes and call these methods.
if (!CharacterData.prototype.before) CharacterData.prototype.before = Element.prototype.before;
if (!CharacterData.prototype.after) CharacterData.prototype.after = Element.prototype.after;
if (!CharacterData.prototype.replaceWith) CharacterData.prototype.replaceWith = Element.prototype.replaceWith;
if (!CharacterData.prototype.remove) CharacterData.prototype.remove = Element.prototype.remove;

if (!('isConnected' in Node.prototype)) {
  Object.defineProperty(Node.prototype, 'isConnected', {
    get() {
      const root = this.getRootNode({ composed: true });
      return !!root && root.nodeType === 9;
    }
  });
}

// Resize observation is part of the rendering update, not a timer. Keep the
// last delivered size for each observed box and perform one coalesced geometry
// checkpoint after DOM/viewport work. This follows the browser lifecycle and,
// importantly, does not keep the event loop alive with speculative re-fires.
globalThis.__resizeObservers = [];
let _resizeRenderCheckpointPending = false;
let _resizeRenderCheckpointRunning = false;
let _resizeRenderCheckpointRerun = false;
function _registerResizeObserver(observer) {
  if (!globalThis.__resizeObservers.includes(observer)) {
    globalThis.__resizeObservers.push(observer);
  }
}
function _unregisterResizeObserver(observer) {
  const index = globalThis.__resizeObservers.indexOf(observer);
  if (index >= 0) globalThis.__resizeObservers.splice(index, 1);
}
function _scheduleResizeRenderCheckpoint() {
  if (!globalThis.__resizeObservers.length) return;
  if (_resizeRenderCheckpointRunning) {
    _resizeRenderCheckpointRerun = true;
    return;
  }
  if (_resizeRenderCheckpointPending) return;
  _resizeRenderCheckpointPending = true;
  _scheduleRenderingOpportunity();
}
function _runResizeRenderCheckpoint() {
  _resizeRenderCheckpointPending = false;
  _resizeRenderCheckpointRunning = true;
  let depth = 0;
  let skipped = false;
  // Depth strictly increases after each broadcast, so this is naturally
  // bounded by tree depth. Keep a hard ceiling for adversarial callbacks
  // that manufacture an ever-deeper subtree during one delivery cycle.
  for (let iteration = 0; iteration < 64; iteration++) {
    _resizeRenderCheckpointRerun = false;
    const observers = [...globalThis.__resizeObservers];
    const targets = [];
    const seenTargets = new Set();
    for (const observer of observers) {
      for (const target of observer._targets.keys()) {
        if (seenTargets.has(target)) continue;
        seenTargets.add(target);
        targets.push(target);
      }
    }
    const measurements = _roMeasurements(targets);
    let shallowest = Infinity;
    let active = false;
    skipped = false;
    // Gather every observer before invoking any callback. A callback from an
    // earlier observer must not change the geometry gathered for a later one.
    for (const observer of observers) {
      const gathered = observer._gather(measurements, depth);
      active = active || gathered.active;
      skipped = skipped || gathered.skipped;
      shallowest = Math.min(shallowest, gathered.shallowest);
    }
    if (!active) break;
    for (const observer of observers) observer._broadcast();
    depth = shallowest;
    if (!_resizeRenderCheckpointRerun) break;
    if (iteration === 63) skipped = true;
  }
  _resizeRenderCheckpointRunning = false;
  _resizeRenderCheckpointRerun = false;
  if (skipped) {
    // Match the standardized loop-limit signal without queuing another
    // internal task that could keep a pathological page permanently busy.
    try {
      globalThis.dispatchEvent(new ErrorEvent("error", {
        message: "ResizeObserver loop completed with undelivered notifications."
      }));
    } catch (_error) {}
  }
}
globalThis.__obscura_recompute_resizes = _scheduleResizeRenderCheckpoint;
function _roNumber(value) {
  const number = Number.parseFloat(value);
  return Number.isFinite(number) ? number : 0;
}
function _roPhysicalSize(inlineSize, blockSize, vertical) {
  return vertical
    ? new ResizeObserverSize(_roConstructionKey, blockSize, inlineSize)
    : new ResizeObserverSize(_roConstructionKey, inlineSize, blockSize);
}
function _roNodeDepth(target) {
  let depth = 1;
  let node = target;
  while (node && (node = node.parentNode || node.host || null)) depth++;
  return depth;
}
function _roMeasurement(target, suppliedGeometry, suppliedByBatch = false) {
  let geometry = suppliedGeometry ?? null;
  const hasRenderer = typeof Deno.core.ops.op_layout_geometry === "function";
  if (!suppliedByBatch && hasRenderer && target?._nid != null) {
    try {
      const raw = Deno.core.ops.op_layout_geometry(String(target._nid | 0));
      geometry = raw ? JSON.parse(raw) : null;
    } catch (_error) {}
  }

  // Preserve deterministic geometry in non-render builds. This path has no
  // native layout cache, but lifecycle behavior (initial delivery and
  // change-only rechecks) should remain useful to automation consumers.
  if (!suppliedByBatch && !hasRenderer && target?.getBoundingClientRect) {
    const rect = target.getBoundingClientRect();
    geometry = {
      x: rect.x, y: rect.y,
      clientWidth: rect.width, clientHeight: rect.height,
    };
  }

  // No renderer box (detached, display:none) has zero sizes. The initial zero
  // is still delivered because an observation starts without a reported size.
  if (!geometry) {
    const zero = _roPhysicalSize(0, 0, false);
    return {
      contentRect: _ioRect(0, 0, 0, 0),
      contentBoxSize: [zero],
      borderBoxSize: [_roPhysicalSize(0, 0, false)],
      devicePixelContentBoxSize: [_roPhysicalSize(0, 0, false)],
      selected: { "content-box": [0, 0], "border-box": [0, 0], "device-pixel-content-box": [0, 0] },
    };
  }

  // The bulk native measurement includes this small style subset from the
  // same PreparedRender as geometry. Non-render builds retain the CSSOM
  // fallback, and a missing/invalid bulk result falls back above.
  const style = suppliedByBatch
    ? {
        ...geometry,
        // `writing-mode` is not yet part of the renderer's compact computed
        // snapshot. Preserve the existing CSSOM fallback for an authored
        // inline value so batching does not silently swap inline/block axes.
        writingMode: geometry.writingMode || target?.style?.writingMode || "",
      }
    : getComputedStyle(target);
  const paddingTop = _roNumber(style.paddingTop);
  const paddingRight = _roNumber(style.paddingRight);
  const paddingBottom = _roNumber(style.paddingBottom);
  const paddingLeft = _roNumber(style.paddingLeft);
  const borderTop = _roNumber(style.borderTopWidth);
  const borderRight = _roNumber(style.borderRightWidth);
  const borderBottom = _roNumber(style.borderBottomWidth);
  const borderLeft = _roNumber(style.borderLeftWidth);
  const clientWidth = Math.max(0, Number(geometry.clientWidth) || 0);
  const clientHeight = Math.max(0, Number(geometry.clientHeight) || 0);
  const contentWidth = Math.max(0, clientWidth - paddingLeft - paddingRight);
  const contentHeight = Math.max(0, clientHeight - paddingTop - paddingBottom);
  const borderWidth = Math.max(0, clientWidth + borderLeft + borderRight);
  const borderHeight = Math.max(0, clientHeight + borderTop + borderBottom);
  const vertical = /^(?:vertical|sideways)/.test(style.writingMode || "");
  // Per Resize Observer, ordinary non-replaced inline elements have an empty
  // observed box even though getBoundingClientRect() encloses their glyphs.
  const replaced = /^(?:IMG|VIDEO|AUDIO|IFRAME|EMBED|OBJECT|INPUT|TEXTAREA|SELECT|CANVAS|SVG)$/.test(
    target.tagName || ""
  );
  const emptyInline = style.display === "inline" && !replaced;
  const observedContentWidth = emptyInline ? 0 : contentWidth;
  const observedContentHeight = emptyInline ? 0 : contentHeight;
  const observedBorderWidth = emptyInline ? 0 : borderWidth;
  const observedBorderHeight = emptyInline ? 0 : borderHeight;
  const contentSize = _roPhysicalSize(observedContentWidth, observedContentHeight, vertical);
  const borderSize = _roPhysicalSize(observedBorderWidth, observedBorderHeight, vertical);

  // Device-pixel content sizes snap the content edges, rather than merely
  // rounding a CSS size multiplied by DPR. Preserve that distinction for
  // fractional positions and dimensions.
  const dpr = Math.max(0, Number(globalThis.devicePixelRatio) || 1);
  const contentLeft = (Number(geometry.x) + borderLeft + paddingLeft) * dpr;
  const contentTop = (Number(geometry.y) + borderTop + paddingTop) * dpr;
  const deviceWidth = emptyInline ? 0 : Math.max(0,
    Math.round(contentLeft + contentWidth * dpr) - Math.round(contentLeft));
  const deviceHeight = emptyInline ? 0 : Math.max(0,
    Math.round(contentTop + contentHeight * dpr) - Math.round(contentTop));
  const deviceSize = _roPhysicalSize(deviceWidth, deviceHeight, vertical);
  return {
    contentRect: emptyInline
      ? _ioRect(0, 0, 0, 0)
      : _ioRect(paddingLeft, paddingTop, contentWidth, contentHeight),
    contentBoxSize: [contentSize],
    borderBoxSize: [borderSize],
    devicePixelContentBoxSize: [deviceSize],
    selected: {
      "content-box": [contentSize.inlineSize, contentSize.blockSize],
      "border-box": [borderSize.inlineSize, borderSize.blockSize],
      "device-pixel-content-box": [deviceSize.inlineSize, deviceSize.blockSize],
    },
  };
}

function _roMeasurements(targets) {
  const measurements = new Map();
  if (!targets.length) return measurements;
  const bulk = Deno.core.ops.op_resize_observer_measurements;
  if (typeof bulk === "function"
      && targets.every(target => target?._nid != null)) {
    try {
      const raw = bulk(JSON.stringify(targets.map(target => target._nid | 0)));
      const geometries = raw ? JSON.parse(raw) : null;
      if (Array.isArray(geometries) && geometries.length === targets.length) {
        for (let index = 0; index < targets.length; index++) {
          measurements.set(
            targets[index],
            _roMeasurement(targets[index], geometries[index], true),
          );
        }
        return measurements;
      }
    } catch (_error) {}
  }
  for (const target of targets) {
    measurements.set(target, _roMeasurement(target));
  }
  return measurements;
}

const _roConstructionKey = {};
const _roSizeValues = new WeakMap();
globalThis.ResizeObserverSize = class ResizeObserverSize {
  constructor(key, inlineSize, blockSize) {
    if (key !== _roConstructionKey) throw new TypeError("Illegal constructor");
    _roSizeValues.set(this, { inlineSize, blockSize });
  }
  get inlineSize() { return _roSizeValues.get(this)?.inlineSize; }
  get blockSize() { return _roSizeValues.get(this)?.blockSize; }
};
const _roEntryValues = new WeakMap();
globalThis.ResizeObserverEntry = class ResizeObserverEntry {
  constructor(key, target, measurement) {
    if (key !== _roConstructionKey) throw new TypeError("Illegal constructor");
    _roEntryValues.set(this, { target, measurement });
  }
  get target() { return _roEntryValues.get(this)?.target; }
  get contentRect() { return _roEntryValues.get(this)?.measurement.contentRect; }
  get borderBoxSize() { return _roEntryValues.get(this)?.measurement.borderBoxSize; }
  get contentBoxSize() { return _roEntryValues.get(this)?.measurement.contentBoxSize; }
  get devicePixelContentBoxSize() {
    return _roEntryValues.get(this)?.measurement.devicePixelContentBoxSize;
  }
};
globalThis.ResizeObserver = class ResizeObserver {
  constructor(callback) {
    if (typeof callback !== "function") {
      throw new TypeError("ResizeObserver callback must be a function");
    }
    this._callback = callback;
    this._targets = new Map();
    this._active = [];
    this._skipped = false;
  }
  _gather(measurements, depth) {
    this._active = [];
    this._skipped = false;
    let shallowest = Infinity;
    for (const [target, observation] of this._targets) {
      let measurement = measurements.get(target);
      if (!measurement) {
        measurement = _roMeasurement(target);
        measurements.set(target, measurement);
      }
      const size = measurement.selected[observation.box];
      const last = observation.last;
      if (last && last[0] === size[0] && last[1] === size[1]) continue;
      const targetDepth = _roNodeDepth(target);
      // A callback may disconnect and begin observing a different target.
      // Browsers deliver that initial observation on the next rendering
      // opportunity. We fold that opportunity into this bounded cycle so it
      // does not require a persistent frame timer; already-reported targets
      // still obey the loop-depth guard.
      if (targetDepth <= depth && last) {
        this._skipped = true;
        continue;
      }
      shallowest = Math.min(shallowest, targetDepth);
      this._active.push({ target, observation, measurement, size });
    }
    return {
      active: this._active.length > 0,
      skipped: this._skipped,
      shallowest,
    };
  }
  _broadcast() {
    if (!this._active.length) return;
    const entries = this._active.map(({ target, observation, measurement, size }) => {
      // Update before invoking callbacks. Callback-driven mutations are
      // compared against this delivery in the same bounded delivery cycle.
      observation.last = size.slice();
      return new ResizeObserverEntry(_roConstructionKey, target, measurement);
    });
    this._active = [];
    try { this._callback(entries, this); } catch (_error) {}
  }
  observe(target, options = {}) {
    if (!(target instanceof Element)) {
      throw new TypeError("ResizeObserver.observe requires an Element");
    }
    const box = options && options.box != null ? String(options.box) : "content-box";
    if (box !== "content-box" && box !== "border-box" &&
        box !== "device-pixel-content-box") {
      throw new TypeError(`Invalid ResizeObserver box option: ${box}`);
    }
    const current = this._targets.get(target);
    if (current && current.box === box) return;
    this._targets.set(target, { box, last: null });
    _registerResizeObserver(this);
    _scheduleResizeRenderCheckpoint();
  }
  unobserve(target) {
    this._targets.delete(target);
    if (!this._targets.size) _unregisterResizeObserver(this);
  }
  disconnect() {
    this._targets.clear();
    this._active = [];
    this._skipped = false;
    _unregisterResizeObserver(this);
  }
};

if (typeof TextEncoder === 'undefined') {
  globalThis.TextEncoder = class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(str) {
      str = String(str);
      const buf = [];
      for (let i = 0; i < str.length; i++) {
        let c = str.charCodeAt(i);
        if (c < 0x80) buf.push(c);
        else if (c < 0x800) { buf.push(0xC0|(c>>6), 0x80|(c&0x3F)); }
        else if (c < 0xD800 || c >= 0xE000) { buf.push(0xE0|(c>>12), 0x80|((c>>6)&0x3F), 0x80|(c&0x3F)); }
        else { c = 0x10000 + (((c & 0x3FF) << 10) | (str.charCodeAt(++i) & 0x3FF)); buf.push(0xF0|(c>>18), 0x80|((c>>12)&0x3F), 0x80|((c>>6)&0x3F), 0x80|(c&0x3F)); }
      }
      return new Uint8Array(buf);
    }
    encodeInto(str, dest) { const enc = this.encode(str); dest.set(enc.slice(0, dest.length)); return { read: str.length, written: Math.min(enc.length, dest.length) }; }
  };
}
// Fast pure-JS UTF-8 decode (the common case: Response/Blob .text(), most
// pages). Avoids the op + JSON round trip for plain UTF-8.
function _utf8DecodeBytes(bytes, start) {
  let str = '', i = start | 0;
  const n = bytes.length;
  while (i < n) {
    let c = bytes[i++];
    if (c < 0x80) str += String.fromCharCode(c);
    else if (c < 0xE0) str += String.fromCharCode(((c & 0x1F) << 6) | (bytes[i++] & 0x3F));
    else if (c < 0xF0) { const b1 = bytes[i++], b2 = bytes[i++]; str += String.fromCharCode(((c & 0x0F) << 12) | ((b1 & 0x3F) << 6) | (b2 & 0x3F)); }
    else { const b1 = bytes[i++], b2 = bytes[i++], b3 = bytes[i++]; const cp = ((c & 0x07) << 18) | ((b1 & 0x3F) << 12) | ((b2 & 0x3F) << 6) | (b3 & 0x3F); if (cp > 0xFFFF) { const s = cp - 0x10000; str += String.fromCharCode(0xD800 + (s >> 10), 0xDC00 + (s & 0x3FF)); } else str += String.fromCharCode(cp); }
  }
  return str;
}
if (typeof TextDecoder === 'undefined') {
  globalThis.TextDecoder = class TextDecoder {
    constructor(label, options) {
      // No-arg construction (Response.text()/Blob.text() and most pages) is
      // UTF-8; skip the label-validation op on that hot path.
      let name;
      if (label === undefined) {
        name = 'utf-8';
      } else {
        name = Deno.core.ops.op_encoding_for_label(String(label));
        if (!name) throw new RangeError("Failed to construct 'TextDecoder': The encoding label provided ('" + label + "') is invalid.");
      }
      const o = options || {};
      Object.defineProperty(this, 'encoding', { value: name, enumerable: true });
      Object.defineProperty(this, 'fatal', { value: !!o.fatal, enumerable: true });
      Object.defineProperty(this, 'ignoreBOM', { value: !!o.ignoreBOM, enumerable: true });
    }
    decode(input, options) {
      if (input === undefined) return '';
      const bytes = ArrayBuffer.isView(input)
        ? new Uint8Array(input.buffer, input.byteOffset, input.byteLength)
        : new Uint8Array(input);
      // Fast path: plain UTF-8, non-fatal (Response/Blob text, most pages).
      if (this.encoding === 'utf-8' && !this.fatal) {
        let off = 0;
        if (!this.ignoreBOM && bytes.length >= 3 && bytes[0] === 0xEF && bytes[1] === 0xBB && bytes[2] === 0xBF) off = 3;
        return _utf8DecodeBytes(bytes, off);
      }
      // Legacy encodings / fatal mode: encoding_rs via the op.
      const r = JSON.parse(Deno.core.ops.op_text_decode(this.encoding, bytes, this.fatal, this.ignoreBOM));
      if (!r.ok) throw new TypeError("Failed to execute 'decode' on 'TextDecoder': The encoded data was not valid.");
      return r.v;
    }
  };
}

function _splitMediaQueryList(input) {
  const result = [];
  let start = 0, depth = 0, quote = '';
  for (let i = 0; i < input.length; i++) {
    const ch = input[i];
    if (quote) {
      if (ch === '\\') i++;
      else if (ch === quote) quote = '';
    } else if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === '(') {
      depth++;
    } else if (ch === ')') {
      depth--;
      if (depth < 0) return null;
    } else if (ch === ',' && depth === 0) {
      result.push(input.slice(start, i));
      start = i + 1;
    }
  }
  if (depth !== 0 || quote) return null;
  result.push(input.slice(start));
  return result;
}

function _splitMediaAnd(input) {
  const result = [];
  let start = 0, depth = 0, quote = '';
  for (let i = 0; i < input.length; i++) {
    const ch = input[i];
    if (quote) {
      if (ch === '\\') i++;
      else if (ch === quote) quote = '';
      continue;
    }
    if (ch === '"' || ch === "'") { quote = ch; continue; }
    if (ch === '(') { depth++; continue; }
    if (ch === ')') { depth--; continue; }
    if (depth === 0 && input.slice(i, i + 3).toLowerCase() === 'and'
        && (i === 0 || /\s/.test(input[i - 1]))
        && (i + 3 === input.length || /\s/.test(input[i + 3]))) {
      result.push(input.slice(start, i));
      start = i + 3;
      i += 2;
    }
  }
  result.push(input.slice(start));
  return result;
}

function _mediaViewportDimension(name) {
  const value = name === 'width' ? Number(globalThis.innerWidth) : Number(globalThis.innerHeight);
  if (Number.isFinite(value)) return value;
  return name === 'width' ? 1440 : 900;
}

function _parseMediaPx(value) {
  const match = String(value).trim().match(/^([+-]?(?:\d+(?:\.\d*)?|\.\d+))(px)?$/i);
  if (!match || (!match[2] && Number(match[1]) !== 0)) return null;
  const result = Number(match[1]);
  return Number.isFinite(result) ? result : null;
}

function _compareMediaValues(left, operator, right) {
  if (operator === '<') return left < right;
  if (operator === '<=') return left <= right;
  if (operator === '>') return left > right;
  if (operator === '>=') return left >= right;
  return left === right;
}

function _evaluateMediaDimension(feature) {
  let match = feature.match(/^(min|max)-(width|height)\s*:\s*(.+)$/);
  if (match) {
    const expected = _parseMediaPx(match[3]);
    if (expected === null) return false;
    const actual = _mediaViewportDimension(match[2]);
    return match[1] === 'min' ? actual >= expected : actual <= expected;
  }

  match = feature.match(/^(width|height)\s*:\s*(.+)$/);
  if (match) {
    const expected = _parseMediaPx(match[2]);
    return expected !== null && _mediaViewportDimension(match[1]) === expected;
  }

  match = feature.match(/^(width|height)\s*(<=|>=|=|<|>)\s*(.+)$/);
  if (match) {
    const expected = _parseMediaPx(match[3]);
    return expected !== null
      && _compareMediaValues(_mediaViewportDimension(match[1]), match[2], expected);
  }

  match = feature.match(/^(.+?)\s*(<=|>=|=|<|>)\s*(width|height)$/);
  if (match) {
    const expected = _parseMediaPx(match[1]);
    return expected !== null
      && _compareMediaValues(expected, match[2], _mediaViewportDimension(match[3]));
  }

  match = feature.match(/^(.+?)\s*(<=|>=|<|>)\s*(width|height)\s*(<=|>=|<|>)\s*(.+)$/);
  if (match) {
    const lower = _parseMediaPx(match[1]);
    const upper = _parseMediaPx(match[5]);
    if (lower === null || upper === null) return false;
    const actual = _mediaViewportDimension(match[3]);
    return _compareMediaValues(lower, match[2], actual)
      && _compareMediaValues(actual, match[4], upper);
  }

  if (feature === 'width' || feature === 'height')
    return _mediaViewportDimension(feature) !== 0;
  return null;
}

function _evaluateMediaFeature(raw) {
  let feature = raw.trim().toLowerCase();
  if (feature[0] !== '(' || feature[feature.length - 1] !== ')') return false;
  feature = feature.slice(1, -1).trim();

  const dimension = _evaluateMediaDimension(feature);
  if (dimension !== null) return dimension;

  let match = feature.match(/^orientation\s*:\s*(portrait|landscape)$/);
  if (match) {
    const width = _mediaViewportDimension('width');
    const height = _mediaViewportDimension('height');
    return match[1] === 'portrait' ? height >= width : width > height;
  }

  match = feature.match(/^prefers-color-scheme\s*:\s*(dark|light|no-preference)$/);
  if (match) return match[1] === 'light';
  match = feature.match(/^prefers-reduced-motion\s*:\s*(reduce|no-preference)$/);
  if (match) return match[1] === 'no-preference';

  match = feature.match(/^(pointer|any-pointer)\s*:\s*(none|coarse|fine)$/);
  if (match) return match[2] === 'fine';
  match = feature.match(/^(hover|any-hover)\s*:\s*(none|hover)$/);
  if (match) return match[2] === 'hover';

  if (feature === 'color') return true;
  match = feature.match(/^color\s*:\s*(\d+)$/);
  if (match) return Number(match[1]) === 8;
  return false;
}

function _evaluateOneMediaQuery(raw) {
  let query = raw.trim().toLowerCase();
  if (!query) return false;

  let negate = false;
  let modifier = query.match(/^(not|only)\b\s*/);
  if (modifier) {
    negate = modifier[1] === 'not';
    query = query.slice(modifier[0].length).trim();
  }

  let typeMatches = true;
  if (query[0] !== '(') {
    const type = query.match(/^([a-z][a-z0-9-]*)\b/i);
    if (!type) return false;
    typeMatches = type[1] === 'all' || type[1] === 'screen';
    if (type[1] !== 'all' && type[1] !== 'screen' && type[1] !== 'print')
      typeMatches = false;
    query = query.slice(type[0].length).trim();
    if (query) {
      const conjunction = query.match(/^and\b\s*/);
      if (!conjunction) return false;
      query = query.slice(conjunction[0].length).trim();
    }
  }

  let matches = typeMatches;
  if (query) {
    const conditions = _splitMediaAnd(query);
    if (!conditions.length || conditions.some(condition => !condition.trim())) return false;
    matches = matches && conditions.every(_evaluateMediaFeature);
  }
  return negate ? !matches : matches;
}

function _evaluateMediaQueryList(query) {
  const list = _splitMediaQueryList(String(query));
  return !!list && list.some(_evaluateOneMediaQuery);
}

globalThis.matchMedia = _markNative(function matchMedia(q) {
  const media = q == null ? '' : String(q);
  return {
    get matches() { return _evaluateMediaQueryList(media); },
    media,
    onchange: null,
    addListener(){},
    removeListener(){},
    addEventListener(){},
    removeEventListener(){},
    dispatchEvent(){return true;}
  };
});
// getComputedStyle() returns a fresh declaration object, but those objects all
// observe the same computed style for an element until the document mutates.
// Share the immutable native snapshot behind them. Frameworks routinely call
// getComputedStyle() repeatedly on the same few roots; rebuilding and parsing
// several hundred properties for every wrapper dominated real-page startup.
const _computedStyleSnapshotCache = new WeakMap();
globalThis.getComputedStyle = (el) => {
  if (!el) el = document.body || {};
  const style = el?.style || el?._style || new CSSStyleDeclaration();
  // Render builds expose one immutable snapshot from the retained final
  // cascade/layout. The native snapshot is shared per element and epoch while
  // each call still returns a distinct, live CSSStyleDeclaration proxy.
  const cacheable = (typeof el === 'object' && el !== null) || typeof el === 'function';
  let snapshot = cacheable ? _computedStyleSnapshotCache.get(el) : null;
  if (!snapshot) {
    snapshot = { rendered: null, epoch: -1, names: [] };
    if (cacheable) _computedStyleSnapshotCache.set(el, snapshot);
  }
  const refreshRendered = () => {
    const hasRunningAnimation = typeof _animationsForTarget === 'function'
      && _animationsForTarget(el).some(animation => animation.playState === 'running');
    if (snapshot.epoch === _domMutationEpoch && !hasRunningAnimation) return;
    snapshot.epoch = _domMutationEpoch;
    snapshot.rendered = null;
    if (typeof Deno.core.ops.op_computed_style === 'function' && el?._nid != null) {
      try {
        const raw = Deno.core.ops.op_computed_style(String(el._nid | 0));
        snapshot.rendered = raw ? JSON.parse(raw) : null;
      } catch (e) {}
    }
    snapshot.names = snapshot.rendered ? Object.keys(snapshot.rendered) : [];
  };
  // React virtualization libraries (react-window, tanstack-virtual,
  // react-virtuoso) all compute container dimensions via getComputedStyle.
  // The defaults table previously returned `auto` for width/height and
  // `'static'` for position, which made every list render 0 items. Pulling
  // width/height from the synthesized bounding rect makes those libraries
  // actually render content.
  const dimensionFor = (name) => {
    try {
      const r = el.getBoundingClientRect && el.getBoundingClientRect();
      if (!r) return null;
      switch (name) {
        case 'width': case 'inline-size':
          return r.width != null ? `${r.width}px` : null;
        case 'height': case 'block-size':
          return r.height != null ? `${r.height}px` : null;
        case 'left': return r.left != null ? `${r.left}px` : null;
        case 'top': return r.top != null ? `${r.top}px` : null;
        case 'right': return r.right != null ? `${r.right}px` : null;
        case 'bottom': return r.bottom != null ? `${r.bottom}px` : null;
        case 'client-width': case 'offset-width':
          return r.width != null ? `${r.width}px` : null;
        case 'client-height': case 'offset-height':
          return r.height != null ? `${r.height}px` : null;
      }
    } catch (e) {}
    return null;
  };

  const defaultsKebab = {
    display: 'block', visibility: 'visible', opacity: '1',
    position: 'static', overflow: 'visible',
    transform: 'none', 'transform-origin': '0px 0px',
    transition: 'none', animation: 'none',
    float: 'none', clear: 'none',
    margin: '0px', padding: '0px',
    'margin-top': '0px', 'margin-right': '0px', 'margin-bottom': '0px', 'margin-left': '0px',
    'padding-top': '0px', 'padding-right': '0px', 'padding-bottom': '0px', 'padding-left': '0px',
    'font-size': '16px', 'line-height': 'normal', 'font-weight': '400',
    'letter-spacing': 'normal',
    'font-family': 'Times',
    color: 'rgb(0, 0, 0)', 'background-color': 'rgba(0, 0, 0, 0)',
    'border-width': '0px', 'border-style': 'none', 'border-color': 'rgb(0, 0, 0)',
    'border-top-width': '0px', 'border-right-width': '0px',
    'border-bottom-width': '0px', 'border-left-width': '0px',
    'border-radius': '0px',
    'z-index': 'auto', 'pointer-events': 'auto',
    'box-sizing': 'content-box', cursor: 'auto',
    'white-space': 'normal', 'text-align': 'start',
    'flex-flow': 'row nowrap', 'flex-direction': 'row', 'flex-wrap': 'nowrap', 'align-items': 'normal',
    'justify-content': 'normal', gap: 'normal',
    'grid-template-columns': 'none', 'grid-template-rows': 'none',
    'will-change': 'auto', 'backface-visibility': 'visible',
  };

  const lookup = (rawProp) => {
    if (typeof rawProp !== 'string') return '';
    refreshRendered();
    let kebab = rawProp.replace(/([A-Z])/g, '-$1').toLowerCase();
    // CSSOM camelCase vendor properties omit the punctuation from their JS
    // spelling (`webkitLineClamp`) but computed-property names retain it
    // (`-webkit-line-clamp`). Normalize the prefix once for every WebKit
    // property instead of adding per-property aliases to the native snapshot.
    if (kebab.startsWith('webkit-')) kebab = '-' + kebab;
    if (snapshot.rendered && Object.prototype.hasOwnProperty.call(snapshot.rendered, kebab))
      return snapshot.rendered[kebab];
    // Non-render builds and properties outside the renderer snapshot retain
    // the lightweight inline CSSOM behavior.
    const inlineVal = target.getPropertyValue ? target.getPropertyValue(rawProp) : '';
    if (inlineVal) {
      if (kebab === 'opacity') {
        const value = Number(inlineVal);
        if (Number.isFinite(value)) return String(Math.min(1, Math.max(0, value)));
      }
      return inlineVal;
    }
    const dim = dimensionFor(kebab);
    if (dim != null) return dim;
    if (defaultsKebab[rawProp]) return defaultsKebab[rawProp];
    if (defaultsKebab[kebab]) return defaultsKebab[kebab];
    return '';
  };

  const target = style;
  return new Proxy(style, {
    get(_, prop) {
      if (prop === Symbol.toPrimitive || prop === Symbol.toStringTag) return undefined;
      if (prop === 'getPropertyValue') return (name) => lookup(name);
      if (prop === 'getPropertyPriority') return () => '';
      if (prop === 'item') return (i) => {
        refreshRendered();
        return snapshot.names[i | 0] || '';
      };
      if (prop === 'length') {
        refreshRendered();
        return snapshot.names.length;
      }
      if (prop === 'cssText') return '';
      if (prop === 'parentRule') return null;
      // CSSStyleDeclaration's `has` trap intentionally reports every known
      // CSS IDL property. Checking `prop in target` before this lookup therefore
      // returned the empty inline declaration for e.g. computed.display and
      // prevented every computed/default fallback below from running.
      if (typeof prop === 'string'
          && (_CSS_PROP_SET.has(prop)
              || _CSS_PROP_SET.has(_cssKebabToCamel(prop))
              || prop.includes('-'))) {
        return lookup(prop);
      }
      if (prop in target) return target[prop];
      if (typeof prop === 'string') return lookup(prop);
      return undefined;
    },
  });
};
// Returns the one Selection instance for a document (cached on the document),
// so window.getSelection() === document.getSelection(). The real Selection
// class is defined below, after Range. _selectionFor is hoisted.
function _selectionFor(doc) {
  if (!doc) return null;
  if (!doc._selection) doc._selection = new Selection(doc);
  return doc._selection;
}
globalThis.getSelection = _markNative(function getSelection() {
  return _selectionFor(globalThis.document);
});

class CSSRule {
  static STYLE_RULE = 1;
  static CHARSET_RULE = 2;
  static IMPORT_RULE = 3;
  static MEDIA_RULE = 4;
  static FONT_FACE_RULE = 5;
  static PAGE_RULE = 6;
  static KEYFRAMES_RULE = 7;
  static KEYFRAME_RULE = 8;
  static NAMESPACE_RULE = 10;
  static COUNTER_STYLE_RULE = 11;
  static SUPPORTS_RULE = 12;

  constructor(cssText, type = 0) {
    this._cssText = String(cssText || "").trim();
    this._type = type;
    this._parentStyleSheet = null;
    this._parentRule = null;
  }
  get type() { return this._type; }
  get cssText() { return this._cssText; }
  set cssText(_value) {}
  get parentStyleSheet() { return this._parentStyleSheet; }
  get parentRule() { return this._parentRule; }
}
for (const name of [
  "STYLE_RULE", "CHARSET_RULE", "IMPORT_RULE", "MEDIA_RULE", "FONT_FACE_RULE",
  "PAGE_RULE", "KEYFRAMES_RULE", "KEYFRAME_RULE", "NAMESPACE_RULE",
  "COUNTER_STYLE_RULE", "SUPPORTS_RULE",
]) {
  Object.defineProperty(CSSRule.prototype, name, { value: CSSRule[name] });
}

class CSSStyleRule extends CSSRule {
  constructor(selectorText, declarations) {
    super("", CSSRule.STYLE_RULE);
    this._selectorText = String(selectorText || "").trim();
    const declaration = new CSSStyleDeclaration(null, () => this._changed());
    _parseCssInto(declaration._props, declarations);
    declaration._loaded = true;
    this._style = _styleProxy(declaration);
  }
  get selectorText() { return this._selectorText; }
  set selectorText(value) {
    const selector = String(value || "").trim();
    if (!selector || /[{}]/.test(selector)) return;
    this._selectorText = selector;
    this._changed();
  }
  get style() { return this._style; }
  get cssText() {
    const declarations = this._style.cssText;
    return `${this._selectorText} {${declarations ? " " + declarations : ""} }`;
  }
  set cssText(_value) {}
  _changed() {
    if (this._parentStyleSheet) this._parentStyleSheet._ruleChanged();
  }
}

// Split only the stylesheet's top-level rules. The renderer remains the CSS
// parser of record; this scanner exists to expose the live CSSOM rule list and
// deliberately preserves unfamiliar at-rules as opaque CSSRule objects.
function _splitTopLevelCssRules(value) {
  const css = String(value || "");
  const rules = [];
  let position = 0;
  const skipTrivia = () => {
    for (;;) {
      while (position < css.length && /\s/.test(css[position])) position++;
      if (css.startsWith("/*", position)) {
        const end = css.indexOf("*/", position + 2);
        if (end < 0) { position = css.length; return false; }
        position = end + 2;
        continue;
      }
      return true;
    }
  };
  let valid = skipTrivia();
  while (valid && position < css.length) {
    const start = position;
    let quote = "", comment = false, escaped = false;
    let parens = 0, braces = 0, complete = false;
    for (; position < css.length; position++) {
      const ch = css[position], next = css[position + 1];
      if (comment) {
        if (ch === "*" && next === "/") { comment = false; position++; }
        continue;
      }
      if (escaped) { escaped = false; continue; }
      if (ch === "\\") { escaped = true; continue; }
      if (quote) { if (ch === quote) quote = ""; continue; }
      if (ch === "/" && next === "*") { comment = true; position++; continue; }
      if (ch === '"' || ch === "'") { quote = ch; continue; }
      if (ch === "(") { parens++; continue; }
      if (ch === ")") { parens = Math.max(0, parens - 1); continue; }
      if (parens) continue;
      if (ch === "{") { braces++; continue; }
      if (ch === "}") {
        if (!braces) break;
        braces--;
        if (!braces) { position++; complete = true; break; }
        continue;
      }
      if (ch === ";" && !braces) { position++; complete = true; break; }
    }
    if (!complete || quote || comment || braces || parens) {
      valid = false;
      break;
    }
    const text = css.slice(start, position).trim();
    if (text) rules.push(text);
    valid = skipTrivia();
  }
  return { rules, valid: valid && position >= css.length };
}

function _cssRuleFromText(text) {
  const trimmed = String(text || "").trim();
  if (!trimmed) return null;
  if (trimmed[0] === "@") return new CSSRule(trimmed, 0);
  const open = trimmed.indexOf("{");
  if (open <= 0 || !trimmed.endsWith("}")) return null;
  const selector = trimmed.slice(0, open).trim();
  if (!selector) return null;
  return new CSSStyleRule(selector, trimmed.slice(open + 1, -1));
}

class CSSRuleList {
  constructor(sheet) {
    this._sheet = sheet;
    return new Proxy(this, {
      get(target, property, receiver) {
        if (typeof property === "string" && /^(?:0|[1-9]\d*)$/.test(property)) {
          return target.item(+property) || undefined;
        }
        return Reflect.get(target, property, receiver);
      },
      has(target, property) {
        if (typeof property === "string" && /^(?:0|[1-9]\d*)$/.test(property)) {
          return +property < target.length;
        }
        return Reflect.has(target, property);
      },
      getOwnPropertyDescriptor(target, property) {
        if (typeof property === "string" && /^(?:0|[1-9]\d*)$/.test(property)) {
          const value = target.item(+property);
          return value ? { value, writable: false, enumerable: true, configurable: true } : undefined;
        }
        return Reflect.getOwnPropertyDescriptor(target, property);
      },
    });
  }
  get length() { this._sheet._refreshFromOwner(); return this._sheet._rules.length; }
  item(index) {
    this._sheet._refreshFromOwner();
    return this._sheet._rules[index >>> 0] || null;
  }
  forEach(callback, thisArg) {
    for (let i = 0; i < this.length; i++) callback.call(thisArg, this.item(i), i, this);
  }
  *[Symbol.iterator]() { for (let i = 0; i < this.length; i++) yield this.item(i); }
}

class CSSStyleSheet {
  constructor(_options) {
    this.ownerRule = null;
    this.disabled = false;
    this._ownerNode = null;
    this._sourceNode = null;
    this._sourceText = "";
    this._href = null;
    this._originClean = true;
    this._rules = [];
    this._cssRules = new CSSRuleList(this);
    this._adopters = new Set();
  }
  get type() { return "text/css"; }
  get ownerNode() { return this._ownerNode; }
  get parentStyleSheet() { return null; }
  get href() { return this._href; }
  get title() { return this._ownerNode?.getAttribute?.("title") || ""; }
  get cssRules() {
    this._assertOriginClean();
    this._refreshFromOwner();
    return this._cssRules;
  }
  get rules() { return this.cssRules; }
  _bindOwner(ownerNode, sourceNode = ownerNode) {
    this._ownerNode = ownerNode;
    this._sourceNode = sourceNode;
    this._sourceText = null;
    this._refreshFromOwner();
  }
  _bindLinkedOwner(ownerNode, sourceNode, href, originClean) {
    this._ownerNode = ownerNode;
    this._sourceNode = sourceNode;
    this._sourceText = null;
    this._href = href || null;
    this._originClean = originClean !== false;
    if (this._originClean) this._refreshFromOwner();
    else {
      this._setRules([]);
      this._sourceText = sourceNode?.textContent || "";
    }
  }
  _assertOriginClean() {
    if (!this._originClean) {
      throw new DOMException("Cannot access rules in a cross-origin stylesheet", "SecurityError");
    }
  }
  _refreshFromOwner() {
    if (!this._sourceNode || !this._originClean) return;
    const text = this._sourceNode.textContent || "";
    if (text === this._sourceText) return;
    const parsed = _splitTopLevelCssRules(text);
    const rules = parsed.rules.map(_cssRuleFromText).filter(Boolean);
    this._setRules(rules);
    this._sourceText = text;
  }
  _setRules(rules) {
    for (const rule of this._rules) rule._parentStyleSheet = null;
    this._rules.splice(0, this._rules.length, ...rules);
    for (const rule of this._rules) rule._parentStyleSheet = this;
  }
  _serializeText() { return this._rules.map(rule => rule.cssText).join("\n"); }
  _ruleChanged() {
    const text = this._serializeText();
    this._sourceText = text;
    // DOM text is the renderer bridge for this bounded CSSOM implementation:
    // its ordinary style-element mutation path invalidates cascade/layout.
    // Avoiding the observable text rewrite requires a future native effective-
    // source channel shared by CSSOM and the renderer.
    if (this._sourceNode && this._sourceNode.textContent !== text) this._sourceNode.textContent = text;
    _syncAdoptedStyleSheet(this);
  }
  insertRule(rule, index = 0) {
    if (arguments.length < 1) throw new TypeError("CSSStyleSheet.insertRule requires a rule");
    this._assertOriginClean();
    this._refreshFromOwner();
    const idx = Number(index) >>> 0;
    if (idx > this._rules.length) throw new DOMException("Rule index is out of range", "IndexSizeError");
    const parsed = _splitTopLevelCssRules(String(rule));
    if (!parsed.valid || parsed.rules.length !== 1) {
      throw new DOMException("The rule could not be parsed", "SyntaxError");
    }
    const cssRule = _cssRuleFromText(parsed.rules[0]);
    if (!cssRule) throw new DOMException("The rule could not be parsed", "SyntaxError");
    cssRule._parentStyleSheet = this;
    this._rules.splice(idx, 0, cssRule);
    this._ruleChanged();
    return idx;
  }
  deleteRule(index) {
    if (arguments.length < 1) throw new TypeError("CSSStyleSheet.deleteRule requires an index");
    this._assertOriginClean();
    this._refreshFromOwner();
    const idx = Number(index) >>> 0;
    if (idx >= this._rules.length) throw new DOMException("Rule index is out of range", "IndexSizeError");
    const [removed] = this._rules.splice(idx, 1);
    if (removed) removed._parentStyleSheet = null;
    this._ruleChanged();
  }
  addRule(selector, style, index) {
    this.insertRule(String(selector) + "{" + String(style) + "}", index ?? this._rules.length);
    return -1;
  }
  removeRule(index = 0) { this.deleteRule(index); }
  replace(text) { this.replaceSync(text); return Promise.resolve(this); }
  replaceSync(text) {
    this._assertOriginClean();
    const parsed = _splitTopLevelCssRules(String(text));
    this._setRules(parsed.rules.map(_cssRuleFromText).filter(Boolean));
    this._ruleChanged();
  }
}

const _styleElementSheets = new WeakMap();
function _styleElementIsCssomBridge(style) {
  return style.hasAttribute("data-obscura-adopted")
    || style.hasAttribute("data-obscura-linked")
    || style.hasAttribute("data-obscura-external-stylesheets")
    || style.hasAttribute("data-obscura-inline-import");
}
function _styleElementHasCssSheet(style) {
  if (!style || style.localName !== "style" || !style.isConnected) return false;
  // These nodes carry renderer input for another stylesheet owner. Exposing a
  // second style-owned sheet would duplicate entries and, for remote links,
  // bypass the link sheet's origin-clean cssRules check.
  if (_styleElementIsCssomBridge(style)) return false;
  const type = (style.getAttribute("type") || "").trim().toLowerCase();
  return !type || type === "text/css";
}
function _sheetForStyleElement(style) {
  if (!_styleElementHasCssSheet(style)) {
    _detachStyleSheet(style);
    return null;
  }
  let sheet = _styleElementSheets.get(style);
  if (!sheet) {
    sheet = new CSSStyleSheet();
    sheet._bindOwner(style);
    _styleElementSheets.set(style, sheet);
  }
  return sheet;
}
function _detachStyleSheet(style) {
  const sheet = _styleElementSheets.get(style);
  if (!sheet) return;
  sheet._ownerNode = null;
  sheet._sourceNode = null;
  _styleElementSheets.delete(style);
}
function _linkElementHasCssSheet(link) {
  if (!link || link.localName !== "link" || !link.isConnected) return false;
  const rel = (link.getAttribute("rel") || link.rel || "").toLowerCase().split(/\s+/);
  const type = (link.getAttribute("type") || "").trim().toLowerCase();
  return rel.includes("stylesheet") && (!type || type === "text/css")
    && _linkedStylesheetNodes.has(link);
}
function _sheetForLinkElement(link) {
  if (!_linkElementHasCssSheet(link)) {
    _detachLinkedStyleSheet(link);
    return null;
  }
  let sheet = _linkElementSheets.get(link);
  if (!sheet) {
    sheet = _registerLinkedStylesheet(link, _linkedStylesheetNodes.get(link));
  }
  return sheet;
}
function _detachLinkedStyleSheet(link) {
  const sheet = _linkElementSheets.get(link);
  if (!sheet) return;
  sheet._ownerNode = null;
  sheet._sourceNode = null;
  _linkElementSheets.delete(link);
}
function _detachStyleSheetsInSubtree(root) {
  if (!root) return;
  if (root.nodeType === 1 && root.localName === "style") _detachStyleSheet(root);
  if (root.nodeType === 1 && root.localName === "link") _detachLinkedStyleSheet(root);
  if (!root.querySelectorAll) return;
  for (const style of root.querySelectorAll("style")) _detachStyleSheet(style);
  for (const link of root.querySelectorAll('link[rel~="stylesheet"]')) {
    _detachLinkedStyleSheet(link);
  }
}

class StyleSheetList {
  constructor(root) {
    this._root = root;
    return new Proxy(this, {
      get(target, property, receiver) {
        if (typeof property === "string" && /^(?:0|[1-9]\d*)$/.test(property)) {
          return target.item(+property) || undefined;
        }
        return Reflect.get(target, property, receiver);
      },
      has(target, property) {
        if (typeof property === "string" && /^(?:0|[1-9]\d*)$/.test(property)) {
          return +property < target.length;
        }
        return Reflect.has(target, property);
      },
    });
  }
  _sheets() {
    const nodes = this._root.querySelectorAll
      ? this._root.querySelectorAll('style, link[rel~="stylesheet"]')
      : [];
    const out = [];
    for (const style of nodes) {
      if (style.localName === "link") {
        const sheet = _sheetForLinkElement(style);
        if (sheet) out.push(sheet);
        continue;
      }
      if (_styleElementIsCssomBridge(style)) continue;
      const sheet = _sheetForStyleElement(style);
      if (sheet) out.push(sheet);
    }
    return out;
  }
  get length() { return this._sheets().length; }
  item(index) { return this._sheets()[index >>> 0] || null; }
  forEach(callback, thisArg) {
    const sheets = this._sheets();
    sheets.forEach((sheet, index) => callback.call(thisArg, sheet, index, this));
  }
  *[Symbol.iterator]() { yield* this._sheets(); }
}

Object.defineProperty(Element.prototype, "sheet", {
  get() {
    if (this.localName === "style") return _sheetForStyleElement(this);
    if (this.localName === "link") return _sheetForLinkElement(this);
    return null;
  },
  configurable: true,
});
globalThis.CSSRule = CSSRule;
globalThis.CSSStyleRule = CSSStyleRule;
globalThis.CSSRuleList = CSSRuleList;
globalThis.CSSStyleSheet = CSSStyleSheet;
globalThis.StyleSheetList = StyleSheetList;

function _syncAdoptedStyleSheet(sheet) {
  for (const root of Array.from(sheet._adopters || [])) {
    _syncAdoptedStyles(root);
  }
}

function _reconcileAdoptedStyleSheetAdopters(root, sheets) {
  const previous = root._registeredAdoptedStyleSheets
    || (root._registeredAdoptedStyleSheets = new Set());
  const current = new Set(Array.from(sheets || []).filter(sheet => sheet instanceof CSSStyleSheet));
  for (const sheet of previous) {
    if (!current.has(sheet)) sheet._adopters?.delete(root);
  }
  for (const sheet of current) {
    if (!previous.has(sheet)) sheet._adopters.add(root);
  }
  root._registeredAdoptedStyleSheets = current;
}

function _adoptedStyleTarget(root) {
  if (!root) return null;
  if (root.nodeType === 9) return root.head || root.documentElement;
  return root instanceof globalThis.ShadowRoot ? root : null;
}

function _syncAdoptedStyles(root) {
  const sheets = root._adoptedStyleSheets || [];
  _reconcileAdoptedStyleSheetAdopters(root, sheets);
  const nodes = root._adoptedStyleNodes || (root._adoptedStyleNodes = new Map());
  for (const [sheet, node] of Array.from(nodes.entries())) {
    if (!sheets.includes(sheet)) {
      node.remove();
      nodes.delete(sheet);
    }
  }
  const target = _adoptedStyleTarget(root);
  if (!target) return;
  for (const sheet of sheets) {
    if (!(sheet instanceof CSSStyleSheet)) continue;
    let node = nodes.get(sheet);
    if (!node || node.parentNode !== target) {
      node = (root.ownerDocument || globalThis.document).createElement("style");
      node.setAttribute("data-obscura-adopted", "");
      target.appendChild(node);
      nodes.set(sheet, node);
    }
    const css = Array.from(sheet.cssRules || [], rule => rule.cssText || "").join("\n");
    if (node.textContent !== css) node.textContent = css;
  }
}

// Keep the [SameObject] array identity stable even when the IDL setter replaces
// its contents. Mutating the backing target directly avoids intermediate
// materializations while assignment is in progress; ordinary array mutations
// still pass through the proxy and synchronize immediately.
const _adoptedSheetListTargets = new WeakMap();
function _makeAdoptedSheetList(root, values) {
  const target = Array.from(values || []);
  const list = new Proxy(target, {
    set(array, property, value) {
      Reflect.set(array, property, value);
      _syncAdoptedStyles(root);
      return true;
    },
    deleteProperty(array, property) {
      Reflect.deleteProperty(array, property);
      _syncAdoptedStyles(root);
      return true;
    },
  });
  _adoptedSheetListTargets.set(root, target);
  return list;
}

function _adoptedStyleSheetsFor(root) {
  if (!root._adoptedStyleSheets) {
    root._adoptedStyleSheets = _makeAdoptedSheetList(root, []);
  }
  return root._adoptedStyleSheets;
}

function _replaceAdoptedStyleSheets(root, sheets) {
  const list = _adoptedStyleSheetsFor(root);
  const values = Array.from(sheets || []);
  const target = _adoptedSheetListTargets.get(root);
  target.splice(0, target.length, ...values);
  _syncAdoptedStyles(root);
  return list;
}

Object.defineProperty(Document.prototype, 'adoptedStyleSheets', {
  get() { return _adoptedStyleSheetsFor(this); },
  set(sheets) {
    _replaceAdoptedStyleSheets(this, sheets);
  },
});

globalThis.__mutationObservers = [];
globalThis.MutationObserver = class MutationObserver {
  constructor(callback) {
    this._callback = callback;
    this._targets = [];
    this._records = [];
  }
  observe(target, options) {
    this._targets.push({ target, options: options || {} });
    globalThis.__mutationObservers.push(this);
  }
  disconnect() {
    this._targets = [];
    const idx = globalThis.__mutationObservers.indexOf(this);
    if (idx >= 0) globalThis.__mutationObservers.splice(idx, 1);
  }
  takeRecords() {
    const r = this._records.slice();
    this._records = [];
    return r;
  }
  _notify(records) {
    this._records.push(...records);
    Promise.resolve().then(() => {
      if (this._records.length > 0) {
        const batch = this._records.splice(0);
        try { this._callback(batch, this); } catch(e) { /* observer errors shouldn't propagate */ }
      }
    });
  }
};
globalThis.__notifyMutation = function(type, target_nid, addedNodes, removedNodes, attributeName, oldValue) {
  if (!globalThis.__mutationObservers.length) return;
  // Use `_wrap` (the canonical node-id → wrapper resolver) instead of a
  // direct cache poke. The previous code referenced `globalThis._cache`,
  // but `_cache` is a module-local Map — the lookup always returned
  // undefined, so the function silently bailed every time. Result: no
  // MutationObserver fired in obscura, ever, despite the call sites being
  // wired up at appendChild / setAttribute. _wrap also lazily creates a
  // wrapper for nodes that didn't have one yet (e.g. children parsed from
  // `set innerHTML`), which we need for record.target/added/removed.
  const target = _wrap(target_nid);
  if (!target) return;
  const record = {
    type: type, // 'childList', 'attributes', 'characterData'
    target: target,
    addedNodes: (addedNodes || []).map(nid => _wrap(nid)).filter(Boolean),
    removedNodes: (removedNodes || []).map(nid => _wrap(nid)).filter(Boolean),
    attributeName: attributeName || null,
    oldValue: oldValue ?? null,
    previousSibling: null,
    nextSibling: null,
  };
  // Walk target → ancestors so a subtree-mode observer rooted at any
  // ancestor matches. The previous implementation just checked that
  // `target.contains` and `target.closest` were defined (always true on
  // any Element), so subtree=true silently behaved like subtree=false and
  // every nested mutation missed its subscriber.
  for (const obs of globalThis.__mutationObservers) {
    let matched = false;
    for (const t of obs._targets) {
      const root = t.target;
      if (!root) continue;
      // Filter by type per the observer options. Default behaviour matches
      // real MutationObserver: attribute mutations need options.attributes,
      // characterData mutations need options.characterData, childList
      // needs options.childList.
      const wantsType =
        (type === 'attributes' && t.options.attributes) ||
        (type === 'characterData' && t.options.characterData) ||
        (type === 'childList' && t.options.childList);
      if (!wantsType) continue;
      if (root._nid === target_nid) { matched = true; break; }
      if (t.options.subtree) {
        // Walk parents until we hit the observed root or run off the tree.
        let cur = target.parentNode;
        while (cur) {
          if (cur._nid === root._nid) { matched = true; break; }
          cur = cur.parentNode;
        }
        if (matched) break;
      }
    }
    if (matched) obs._notify([record]);
  }
};

globalThis.ShadowRoot = class ShadowRoot extends DocumentFragment {
  constructor(nid, host, options) {
    super(nid);
    this._host = host;
    this._mode = options.mode;
    this._delegatesFocus = !!options.delegatesFocus;
    this._slotAssignment = options.slotAssignment === 'manual' ? 'manual' : 'named';
    this._clonable = !!options.clonable;
    this._serializable = !!options.serializable;
  }
  get host() { return this._host; }
  get mode() { return this._mode; }
  get delegatesFocus() { return this._delegatesFocus; }
  get slotAssignment() { return this._slotAssignment; }
  get clonable() { return this._clonable; }
  get serializable() { return this._serializable; }
  _assertInsertable(node, operation) {
    const createsComposedCycle = node instanceof ShadowRoot
      || node === this._host
      || !!(node?.contains && node.contains(this._host));
    if (createsComposedCycle) {
      throw new DOMException(
        `Failed to execute '${operation}' on 'Node': The new child would contain the parent.`,
        'HierarchyRequestError'
      );
    }
  }
  appendChild(child) {
    this._assertInsertable(child, 'appendChild');
    return super.appendChild(child);
  }
  insertBefore(node, reference) {
    if (reference && reference.parentNode !== this) {
      throw new DOMException(
        "Failed to execute 'insertBefore' on 'Node': The reference node is not a child of this node.",
        'NotFoundError'
      );
    }
    if (node === reference) return node;
    this._assertInsertable(node, 'insertBefore');
    return super.insertBefore(node, reference);
  }
  removeChild(child) {
    if (!child || child.parentNode !== this) {
      throw new DOMException(
        "Failed to execute 'removeChild' on 'Node': The node to be removed is not a child of this node.",
        'NotFoundError'
      );
    }
    return super.removeChild(child);
  }
  replaceChild(node, oldChild) {
    if (!oldChild || oldChild.parentNode !== this) {
      throw new DOMException(
        "Failed to execute 'replaceChild' on 'Node': The node to be replaced is not a child of this node.",
        'NotFoundError'
      );
    }
    if (node === oldChild) return oldChild;
    this._assertInsertable(node, 'replaceChild');
    return super.replaceChild(node, oldChild);
  }
  getRootNode(options) {
    return options?.composed ? this._host.getRootNode(options) : this;
  }
  get activeElement() { return null; }
  get styleSheets() {
    if (!this._styleSheetList) this._styleSheetList = new StyleSheetList(this);
    return this._styleSheetList;
  }
  cloneNode() {
    throw new DOMException(
      'Failed to execute cloneNode on Node: ShadowRoot nodes are not clonable.',
      'NotSupportedError'
    );
  }
  setHTMLUnsafe(value) { this.innerHTML = String(value == null ? '' : value); }
  getHTML() { return this.innerHTML; }
};
// Constructible-stylesheet adoption, mirroring Document.adoptedStyleSheets.
Object.defineProperty(globalThis.ShadowRoot.prototype, 'adoptedStyleSheets', {
  get() { return _adoptedStyleSheetsFor(this); },
  set(sheets) { _replaceAdoptedStyleSheets(this, sheets); },
  configurable: true,
});
globalThis.__obscura_shadowHostNames = new Set(['article','aside','blockquote','body','div','footer','h1','h2','h3','h4','h5','h6','header','main','nav','p','section','span']);
function _isConstructorCE(v) {
  if (typeof v !== 'function') return false;
  try { Reflect.construct(function () {}, [], v); return true; } catch (e) { return false; }
}
const _CE_RESERVED = new Set(['annotation-xml', 'color-profile', 'font-face', 'font-face-src', 'font-face-uri', 'font-face-format', 'font-face-name', 'missing-glyph']);
function _isValidCustomElementName(name) {
  if (typeof name !== 'string' || _CE_RESERVED.has(name)) return false;
  // PotentialCustomElementName (approx): lowercase start, a hyphen, no uppercase.
  return /^[a-z][a-z0-9._·À-￿-]*-[a-z0-9._·À-￿-]*$/.test(name);
}
class CustomElementRegistry {
  constructor() { this._registry = new Map(); this._byCtor = new Map(); this._whenDefinedResolvers = new Map(); this._defining = false; }
  define(name, cls, opts) {
    if (!_isConstructorCE(cls)) throw new TypeError("Failed to execute 'define' on 'CustomElementRegistry': parameter 2 is not a constructor.");
    if (!_isValidCustomElementName(name)) throw new DOMException("Failed to execute 'define' on 'CustomElementRegistry': \"" + name + "\" is not a valid custom element name", "SyntaxError");
    if (this._defining) throw new DOMException("Failed to execute 'define' on 'CustomElementRegistry': operation is not supported while a definition is in progress", "NotSupportedError");
    if (this._registry.has(name)) throw new DOMException("Failed to execute 'define' on 'CustomElementRegistry': the name \"" + name + "\" has already been used with this registry", "NotSupportedError");
    if (this._byCtor.has(cls)) throw new DOMException("Failed to execute 'define' on 'CustomElementRegistry': the constructor has already been used with this registry", "NotSupportedError");
    this._defining = true;
    try { this._byCtor.set(cls, name); this._defineInner(name, cls, opts); } finally { this._defining = false; }
  }
  _defineInner(name, cls, opts) {
    this._registry.set(name, cls);
    // Upgrade existing matching elements: instantiate the class on each,
    // fire connectedCallback if the element is in the document. Without
    // this, lit / MusicKit / Polymer components never wire up their
    // shadow DOM or render, leaving heavy chunks of YouTube,
    // music.apple.com, and any web-component site as empty shells.
    try {
      const matches = globalThis.document?.querySelectorAll(name) || [];
      for (const el of matches) this._upgradeElement(el, cls);
    } catch (e) {}
    const resolvers = this._whenDefinedResolvers.get(name);
    if (resolvers) {
      for (const r of resolvers) r(cls);
      this._whenDefinedResolvers.delete(name);
    }
  }
  _upgradeElement(el, cls) {
    if (el.__customUpgraded) return;
    el.__customUpgraded = true;
    try {
      // Upgrade preserves object identity but installs the definition's
      // prototype before running its class constructor. HTMLElement's
      // constructor consumes this entry and returns `el`, so derived class
      // fields and constructor-side state initialize on the real DOM wrapper.
      const constructionEntry = { element: el, constructor: cls, constructed: false };
      _customElementConstructionStack.push(constructionEntry);
      let constructed;
      try {
        constructed = Reflect.construct(cls, []);
      } finally {
        const pending = _customElementConstructionStack.lastIndexOf(constructionEntry);
        if (pending !== -1) _customElementConstructionStack.splice(pending, 1);
      }
      if (constructed !== el) {
        throw new TypeError("Custom element constructor did not produce the element being upgraded");
      }
      if (typeof el.connectedCallback === 'function' && globalThis.document?.contains?.(el)) {
        try { el.connectedCallback(); } catch (e) {}
      }
    } catch (e) {
      el.__customUpgradeFailed = true;
    }
  }
  get(name) { return this._registry.get(name); }
  getName(cls) {
    if (!_isConstructorCE(cls)) throw new TypeError("Failed to execute 'getName' on 'CustomElementRegistry': parameter 1 is not a constructor.");
    return this._byCtor.has(cls) ? this._byCtor.get(cls) : null;
  }
  whenDefined(name) {
    if (!_isValidCustomElementName(name)) return Promise.reject(new DOMException("Failed to execute 'whenDefined' on 'CustomElementRegistry': \"" + name + "\" is not a valid custom element name", "SyntaxError"));
    const cls = this._registry.get(name);
    if (cls) return Promise.resolve(cls);
    return new Promise((resolve) => {
      const list = this._whenDefinedResolvers.get(name) || [];
      list.push(resolve);
      this._whenDefinedResolvers.set(name, list);
    });
  }
  upgrade(root) {
    if (!root || !root.querySelectorAll) return;
    for (const [name, cls] of this._registry.entries()) {
      const matches = root.querySelectorAll(name);
      for (const el of matches) this._upgradeElement(el, cls);
    }
  }
}
globalThis.CustomElementRegistry = CustomElementRegistry;
globalThis.customElements = new CustomElementRegistry();
globalThis.HTMLUnknownElement = Element;
// ElementInternals: form-associated custom element internals. Validity/state
// are JS-observable; ARIA reflection that needs the accessibility tree is not.
globalThis.ElementInternals = class ElementInternals {
  constructor(el) { this._el = el; this._valid = true; this._flags = {}; this._message = ''; this._value = null; this._states = new Set(); }
  setFormValue(value, state) { this._value = value; }
  setValidity(flags, message, anchor) {
    flags = flags || {};
    const bad = Object.keys(flags).some((k) => k !== 'valid' && flags[k]);
    if (bad && (message == null || message === '')) throw new TypeError("Failed to execute 'setValidity' on 'ElementInternals': The second argument should not be empty if one or more flags in the first argument are true.");
    this._flags = flags; this._valid = !bad; this._message = bad ? String(message) : '';
  }
  checkValidity() { return this._valid; }
  reportValidity() { return this._valid; }
  get validity() {
    const f = this._flags || {};
    return { valid: this._valid, valueMissing: !!f.valueMissing, typeMismatch: !!f.typeMismatch, patternMismatch: !!f.patternMismatch, tooLong: !!f.tooLong, tooShort: !!f.tooShort, rangeUnderflow: !!f.rangeUnderflow, rangeOverflow: !!f.rangeOverflow, stepMismatch: !!f.stepMismatch, badInput: !!f.badInput, customError: !!f.customError };
  }
  get validationMessage() { return this._message || ''; }
  get willValidate() { return true; }
  get form() { return this._el && this._el.closest ? this._el.closest('form') : null; }
  get labels() { return _nodeList([]); }
  get shadowRoot() { return this._el ? _shadowRootForHost(this._el, true) : null; }
  get states() { return this._states; }
};
// Full standard constant set (issue #439). The partial version here lacked
// FILTER_ACCEPT/REJECT/SKIP and most SHOW_* values, so the canonical
// `acceptNode() { return NodeFilter.FILTER_ACCEPT; }` filter idiom returned
// undefined and TreeWalker/NodeIterator rejected every node.
globalThis.NodeFilter = {
  SHOW_ALL: 0xFFFFFFFF,
  SHOW_ELEMENT: 0x1,
  SHOW_ATTRIBUTE: 0x2,
  SHOW_TEXT: 0x4,
  SHOW_CDATA_SECTION: 0x8,
  SHOW_ENTITY_REFERENCE: 0x10,
  SHOW_ENTITY: 0x20,
  SHOW_PROCESSING_INSTRUCTION: 0x40,
  SHOW_COMMENT: 0x80,
  SHOW_DOCUMENT: 0x100,
  SHOW_DOCUMENT_TYPE: 0x200,
  SHOW_DOCUMENT_FRAGMENT: 0x400,
  SHOW_NOTATION: 0x800,
  FILTER_ACCEPT: 1,
  FILTER_REJECT: 2,
  FILTER_SKIP: 3,
};
// ResizeObserver is defined earlier with real per-target firing; the stub
// that previously lived here was a no-op that clobbered the real class.
//
// IntersectionObserver. Render builds provide real, scroll-relative target,
// element-root, and overflow-ancestor boxes from one prepared layout snapshot.
globalThis.__intersectionObservers = [];
let _intersectionRenderCheckpointPending = false;
const _intersectionDeliveryObservers = new Set();
let _intersectionDeliveryTaskPending = false;

function _scheduleIntersectionObserverDelivery(observer) {
  if (!observer._connected || !observer._records.length) return;
  _intersectionDeliveryObservers.add(observer);
  if (_intersectionDeliveryTaskPending) return;
  _intersectionDeliveryTaskPending = true;

  // IntersectionObserver has one task source per document. Deliver every
  // observer which became pending during the rendering update from that task;
  // posting one task per observer lets unrelated scheduler work split a single
  // document notification into seconds of staggered framework updates.
  _browserPostedTaskEnqueue(() => {
    _intersectionDeliveryTaskPending = false;
    const pending = [..._intersectionDeliveryObservers];
    _intersectionDeliveryObservers.clear();
    for (const current of pending) {
      if (!current._connected || !current._records.length) continue;
      const records = current.takeRecords();
      try { current._callback(records, current); } catch (e) {}
    }
  }, _schedulerPriorityRank["user-visible"] * 2);
}

function _scheduleIntersectionRenderCheckpoint() {
  if (!globalThis.__intersectionObservers.some(
    observer => observer._connected && observer._targets.size,
  )) return;
  if (_intersectionRenderCheckpointPending) return;
  _intersectionRenderCheckpointPending = true;
  _scheduleRenderingOpportunity();
}
function _runIntersectionRenderCheckpoint() {
  _intersectionRenderCheckpointPending = false;
  const observers = globalThis.__intersectionObservers.filter(
    observer => observer._connected && observer._targets.size,
  );
  const elements = [];
  const seen = new Set();
  const addElement = element => {
    if (!(element instanceof Element) || seen.has(element)) return;
    seen.add(element);
    elements.push(element);
  };

  // Gather the complete clip graph before entering native code. DOM/shadow
  // ancestry stays in JS, while every geometry/style value comes from the
  // same animation sample and PreparedRender snapshot.
  for (const observer of observers) {
    for (const target of observer._targets) addElement(target);
  }
  for (const observer of observers) {
    if (observer._root instanceof Element) addElement(observer._root);
    for (const target of observer._targets) {
      let ancestor = target.parentNode || target.host || null;
      while (ancestor && ancestor !== observer._root && ancestor.nodeType !== 9) {
        addElement(ancestor);
        ancestor = ancestor.parentNode || ancestor.host || null;
      }
    }
  }
  const measurements = _ioMeasurements(elements);
  for (const observer of observers) {
    if (observer._connected && observer._targets.size) {
      observer._check([...observer._targets], false, measurements);
    }
  }
}
function _ioRect(x, y, width, height) {
  return {
    x, y, width, height,
    top: y, left: x, right: x + width, bottom: y + height,
    toJSON() { return this; },
  };
}
function _ioMargins(value) {
  const parts = String(value || "0px").trim().split(/\s+/);
  if (parts.length < 1 || parts.length > 4) return null;
  const parsed = parts.map((part) => {
    const match = /^([-+]?(?:\d+(?:\.\d*)?|\.\d+))(px|%)$/.exec(part);
    return match ? { value: Number(match[1]), unit: match[2] } : null;
  });
  if (parsed.some((part) => !part)) return null;
  if (parsed.length === 1) return [parsed[0], parsed[0], parsed[0], parsed[0]];
  if (parsed.length === 2) return [parsed[0], parsed[1], parsed[0], parsed[1]];
  if (parsed.length === 3) return [parsed[0], parsed[1], parsed[2], parsed[1]];
  return parsed;
}
function _ioClipsOverflow(value) {
  return /^(?:auto|clip|hidden|overlay|scroll)$/.test(String(value || ""));
}
function _ioMeasurements(elements) {
  const measurements = new Map();
  if (!elements.length) return measurements;
  const bulk = Deno.core.ops.op_intersection_observer_measurements;
  const nativeElements = elements.filter(element => element?._nid != null);
  if (typeof bulk !== "function" || !nativeElements.length) return measurements;
  try {
    const raw = bulk(JSON.stringify(nativeElements.map(element => element._nid | 0)));
    const geometries = raw ? JSON.parse(raw) : null;
    if (Array.isArray(geometries) && geometries.length === nativeElements.length) {
      for (let index = 0; index < nativeElements.length; index++) {
        measurements.set(nativeElements[index], geometries[index]);
      }
    }
  } catch (_error) {}
  return measurements;
}
function _ioElementRect(element, measurements) {
  if (measurements.has(element)) {
    const geometry = measurements.get(element);
    return geometry
      ? _ioRect(
          _roNumber(geometry.x), _roNumber(geometry.y),
          _roNumber(geometry.width), _roNumber(geometry.height),
        )
      : _ioRect(0, 0, 0, 0);
  }
  const rect = element.getBoundingClientRect();
  return _ioRect(rect.x, rect.y, rect.width, rect.height);
}
function _ioElementStyle(element, measurements) {
  return measurements.has(element)
    ? (measurements.get(element) || {})
    : getComputedStyle(element);
}
function _ioElementPaddingBox(element, style, measurements) {
  const hasMeasurement = measurements.has(element);
  const geometry = measurements.get(element);
  const rect = _ioElementRect(element, measurements);
  const borderLeft = _roNumber(style.borderLeftWidth);
  const borderTop = _roNumber(style.borderTopWidth);
  const width = hasMeasurement
    ? (geometry ? _roNumber(geometry.clientWidth) : 0)
    : element.clientWidth;
  const height = hasMeasurement
    ? (geometry ? _roNumber(geometry.clientHeight) : 0)
    : element.clientHeight;
  return _ioRect(rect.left + borderLeft, rect.top + borderTop, width, height);
}
globalThis.IntersectionObserver = class IntersectionObserver {
  constructor(callback, options) {
    if (typeof callback !== "function") {
      throw new TypeError("IntersectionObserver callback must be a function");
    }
    this._callback = callback;
    this._options = options || {};
    this._root = this._options.root == null ? null : this._options.root;
    if (this._root !== null && !(this._root instanceof Element) &&
        this._root?.nodeType !== 9) {
      throw new TypeError("IntersectionObserver root must be an Element or Document");
    }
    this._margins = _ioMargins(this._options.rootMargin || "0px");
    if (!this._margins) throw new SyntaxError("Invalid IntersectionObserver rootMargin");
    const raw = this._options.threshold == null
      ? [0]
      : (Array.isArray(this._options.threshold) ? this._options.threshold : [this._options.threshold]);
    this._thresholds = [...new Set(raw.map(Number))].sort((a, b) => a - b);
    if (!this._thresholds.length) this._thresholds = [0];
    if (this._thresholds.some((value) => !Number.isFinite(value) || value < 0 || value > 1)) {
      throw new RangeError("IntersectionObserver threshold must be between 0 and 1");
    }
    this._targets = new Set();
    this._previous = new Map();
    this._records = [];
    this._connected = true;
    globalThis.__intersectionObservers.push(this);
  }
  _rootBounds(measurements) {
    let x = 0, y = 0;
    let width = globalThis.innerWidth || 1280;
    let height = globalThis.innerHeight || 720;
    if (this._root instanceof Element) {
      const style = _ioElementStyle(this._root, measurements);
      const clips = _ioClipsOverflow(style.overflowX) ||
        _ioClipsOverflow(style.overflowY);
      if (clips) {
        const paddingBox = _ioElementPaddingBox(this._root, style, measurements);
        x = paddingBox.left;
        y = paddingBox.top;
        // The intersection root for a content-clipping element is its padding
        // box (the CSSOM client box), independent of its current scroll offset.
        width = paddingBox.width;
        height = paddingBox.height;
      } else {
        const rect = _ioElementRect(this._root, measurements);
        x = rect.left;
        y = rect.top;
        width = rect.width;
        height = rect.height;
      }
    }
    const resolve = (margin, basis) =>
      margin.unit === "%" ? margin.value * basis / 100 : margin.value;
    // IntersectionObserver resolves every rootMargin percentage against the
    // root rectangle's width, including the block-axis sides.
    const top = resolve(this._margins[0], width);
    const right = resolve(this._margins[1], width);
    const bottom = resolve(this._margins[2], width);
    const left = resolve(this._margins[3], width);
    return _ioRect(x - left, y - top, width + left + right, height + top + bottom);
  }
  _entry(target, root, measurements) {
    const rect = _ioElementRect(target, measurements);
    // A connected zero-area box may intersect when its edges touch the root,
    // but a detached or non-generated box must never become intersecting just
    // because its synthetic zero rectangle happens to sit at the origin.
    const hasGeneratedBox = !measurements.has(target) ||
      measurements.get(target) !== null;
    let inRootTree = hasGeneratedBox && target.isConnected &&
      (!(this._root instanceof Element) || this._root.contains(target));
    let left = Math.max(rect.left, root.left);
    let top = Math.max(rect.top, root.top);
    let right = Math.min(rect.right, root.right);
    let bottom = Math.min(rect.bottom, root.bottom);

    // Mapping a target to its intersection root clips it at every intervening
    // overflow container. Intersecting only with the final root incorrectly
    // exposes offscreen children of nested carousels, virtual lists, and lazy
    // loading viewports. Use each ancestor's padding box, independently by
    // axis, matching Chromium's rectangular overflow clip chain.
    let ancestor = target.parentNode || target.host || null;
    while (inRootTree && ancestor && ancestor !== this._root && ancestor.nodeType !== 9) {
      if (ancestor instanceof Element) {
        const style = _ioElementStyle(ancestor, measurements);
        const clipX = _ioClipsOverflow(style.overflowX);
        const clipY = _ioClipsOverflow(style.overflowY);
        if (clipX || clipY) {
          const clip = _ioElementPaddingBox(ancestor, style, measurements);
          if (clipX) {
            left = Math.max(left, clip.left);
            right = Math.min(right, clip.right);
          }
          if (clipY) {
            top = Math.max(top, clip.top);
            bottom = Math.min(bottom, clip.bottom);
          }
        }
      }
      ancestor = ancestor.parentNode || ancestor.host || null;
    }
    if (this._root instanceof Element && ancestor !== this._root) inRootTree = false;

    const edgesTouch = inRootTree && right >= left && bottom >= top;
    const width = Math.max(0, right - left);
    const height = Math.max(0, bottom - top);
    const targetArea = Math.max(0, rect.width) * Math.max(0, rect.height);
    const isIntersecting = edgesTouch;
    const area = isIntersecting ? width * height : 0;
    return {
      target,
      isIntersecting,
      intersectionRatio: targetArea > 0 ? area / targetArea : (isIntersecting ? 1 : 0),
      boundingClientRect: _ioRect(rect.x, rect.y, rect.width, rect.height),
      intersectionRect: isIntersecting ? _ioRect(left, top, width, height) : _ioRect(0, 0, 0, 0),
      rootBounds: root,
      time: performance.now(),
    };
  }
  _thresholdIndex(ratio) {
    let index = 0;
    while (index < this._thresholds.length && this._thresholds[index] <= ratio) index++;
    return index;
  }
  _queueChanged(target, forceInitial, root, measurements) {
    const entry = this._entry(target, root, measurements);
    const previous = this._previous.get(target);
    const changed = forceInitial || !previous ||
      previous.isIntersecting !== entry.isIntersecting ||
      this._thresholdIndex(previous.intersectionRatio) !==
        this._thresholdIndex(entry.intersectionRatio);
    this._previous.set(target, {
      isIntersecting: entry.isIntersecting,
      intersectionRatio: entry.intersectionRatio,
    });
    if (changed) this._records.push(entry);
  }
  _check(targets, forceInitial, measurements = new Map()) {
    if (!this._connected) return;
    const root = this._rootBounds(measurements);
    for (const target of targets) {
      if (this._targets.has(target)) {
        this._queueChanged(target, !!forceInitial, root, measurements);
      }
    }
    // Delivery remains a task after the rendering update and its microtask
    // checkpoint. The document-level queue batches all pending observers.
    _scheduleIntersectionObserverDelivery(this);
  }
  observe(el) {
    if (!el || this._targets.has(el)) return;
    // `disconnect()` removes every current observation; it does not destroy
    // the observer. Browsers allow the same object to observe targets again.
    // Re-register lazily so dormant observers do not stay in the global
    // geometry recomputation list forever.
    if (!this._connected) {
      this._connected = true;
      if (!globalThis.__intersectionObservers.includes(this)) {
        globalThis.__intersectionObservers.push(this);
      }
    }
    this._targets.add(el);
    this._previous.delete(el);
    _scheduleIntersectionRenderCheckpoint();
  }
  unobserve(el) {
    this._targets.delete(el);
    this._previous.delete(el);
  }
  disconnect() {
    this._connected = false;
    this._targets.clear();
    this._previous.clear();
    this._records.length = 0;
    _intersectionDeliveryObservers.delete(this);
    const index = globalThis.__intersectionObservers.indexOf(this);
    if (index >= 0) globalThis.__intersectionObservers.splice(index, 1);
  }
  takeRecords() { return this._records.splice(0); }
  get root() { return this._root; }
  get rootMargin() {
    return this._margins.map((margin) => `${margin.value}${margin.unit}`).join(" ");
  }
  get thresholds() { return this._thresholds.slice(); }
};
(function() {
  const renderingUpdate = () => {
    _scheduleIntersectionRenderCheckpoint();
    _scheduleResizeRenderCheckpoint();
  };
  // Scrolling calls the IO-only hook. Actual viewport resizing remains a full
  // rendering update and schedules both observer families.
  globalThis.__obscura_recompute_intersections = _scheduleIntersectionRenderCheckpoint;
  globalThis.addEventListener("resize", renderingUpdate);
  const wireUp = () => {
    if (!globalThis.document) return;
    // DOM writes synchronously mark ResizeObserver dirty through `_dom`; this
    // MutationObserver is only needed for intersection geometry. Scheduling RO
    // again here would escape its depth-bounded delivery cycle and allow a
    // self-resizing callback to create an infinite chain of zero-delay tasks.
    const observer = new MutationObserver(_scheduleIntersectionRenderCheckpoint);
    try {
      observer.observe(globalThis.document, {
        childList: true,
        subtree: true,
        attributes: true,
        characterData: true,
      });
    } catch {}
  };
  if (globalThis.document) wireUp();
  else Promise.resolve().then(wireUp);
})();
globalThis.IntersectionObserverEntry = class IntersectionObserverEntry {};
globalThis.PerformanceObserver = class { constructor(){} observe(){} disconnect(){} };

globalThis.DOMException = (function () {
  const NAME_TO_CODE = {
    IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4,
    InvalidCharacterError: 5, NoModificationAllowedError: 7, NotFoundError: 8,
    NotSupportedError: 9, InUseAttributeError: 10, InvalidStateError: 11,
    SyntaxError: 12, InvalidModificationError: 13, NamespaceError: 14,
    InvalidAccessError: 15, TypeMismatchError: 17, SecurityError: 18,
    NetworkError: 19, AbortError: 20, URLMismatchError: 21,
    QuotaExceededError: 22, TimeoutError: 23, InvalidNodeTypeError: 24,
    DataCloneError: 25,
  };
  class DOMException extends Error {
    constructor(message = "", name = "Error") {
      super(message);
      this.name = name;
      this.message = String(message);
    }
    get code() { return NAME_TO_CODE[this.name] || 0; }
  }
  const CONSTS = {
    INDEX_SIZE_ERR: 1, DOMSTRING_SIZE_ERR: 2, HIERARCHY_REQUEST_ERR: 3,
    WRONG_DOCUMENT_ERR: 4, INVALID_CHARACTER_ERR: 5, NO_DATA_ALLOWED_ERR: 6,
    NO_MODIFICATION_ALLOWED_ERR: 7, NOT_FOUND_ERR: 8, NOT_SUPPORTED_ERR: 9,
    INUSE_ATTRIBUTE_ERR: 10, INVALID_STATE_ERR: 11, SYNTAX_ERR: 12,
    INVALID_MODIFICATION_ERR: 13, NAMESPACE_ERR: 14, INVALID_ACCESS_ERR: 15,
    VALIDATION_ERR: 16, TYPE_MISMATCH_ERR: 17, SECURITY_ERR: 18,
    NETWORK_ERR: 19, ABORT_ERR: 20, URL_MISMATCH_ERR: 21,
    QUOTA_EXCEEDED_ERR: 22, TIMEOUT_ERR: 23, INVALID_NODE_TYPE_ERR: 24,
    DATA_CLONE_ERR: 25,
  };
  for (const k in CONSTS) {
    Object.defineProperty(DOMException, k, { value: CONSTS[k], enumerable: true });
    Object.defineProperty(DOMException.prototype, k, { value: CONSTS[k], enumerable: true });
  }
  return DOMException;
})();
// Per the UI Events spec, only events the user agent dispatches (real or
// automation-synthesized input) are trusted; events page script builds with
// `new Event(...)` must report isTrusted === false (issue #303). Returning true
// for everything is a trivial bot-detection tell. Trusted events are tracked in
// a closure-private WeakSet so page JS can neither read nor forge the flag.
// obscura's CDP input pipeline marks its synthetic events via the
// non-enumerable __obscura_markTrusted helper.
const _trustedEvents = new WeakSet();
globalThis.__obscura_markTrusted = function(ev) { try { if (ev) _trustedEvents.add(ev); } catch (_e) {} return ev; };

// Write value/checked through the element's *prototype* accessor, skipping any
// per-instance property a framework layered on top. React (and Preact/Vue)
// install a value tracker by redefining `value`/`checked` on the element to
// record the last value they wrote; a plain `el.value = x` runs that wrapper,
// so their tracker updates in lockstep and the next input/change event looks
// unchanged, so onChange never fires (issue #324). Writing through the
// prototype setter leaves the tracker stale, so the edit is seen as a real
// user change. When no framework wrapper is present this is identical to a
// direct assignment.
globalThis.__obscura_setFieldValue = function(el, field, value) {
  try {
    let proto = Object.getPrototypeOf(el);
    let desc;
    while (proto && !((desc = Object.getOwnPropertyDescriptor(proto, field)) && desc.set)) {
      proto = Object.getPrototypeOf(proto);
    }
    if (desc && desc.set) { desc.set.call(el, value); return; }
  } catch (_e) {}
  el[field] = value;
};

// Build a FileList-like object: an array with the DOM's `item(i)` accessor.
function _makeFileList(files) {
  const list = files.slice();
  Object.defineProperty(list, "item", { value: (i) => list[i] || null, enumerable: false });
  return list;
}
function _emptyFileList() { return _makeFileList([]); }

// Populate an <input type=file>'s FileList from the CDP DOM.setFileInputFiles
// call (Puppeteer uploadFile / Playwright setInputFiles). `specs` is an array of
// { name, type, b64 } where b64 is the base64-encoded file bytes read on the
// Rust side. Real File objects (backed by the bytes) are created so page code can
// read them via FileReader or upload them via fetch/FormData, then input+change
// fire as a genuine selection would (issue #359).
globalThis.__obscura_setInputFiles = function(el, specs) {
  const files = (specs || []).map((s) => {
    let bytes;
    try {
      const bin = atob(s.b64 || "");
      bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    } catch (_e) { bytes = new Uint8Array(0); }
    return new File([bytes], s.name || "", { type: s.type || "" });
  });
  el._files = _makeFileList(files);
  // Mark the events trusted (isTrusted === true), like the Input domain does
  // for synthesized clicks/keys. A real <input type=file> selection fires
  // trusted events; upload flows that gate their change handler on
  // event.isTrusted (common in frameworks and anti-bot code) ignore untrusted
  // ones, which would silently break the exact case this feature targets.
  try { el.dispatchEvent(globalThis.__obscura_markTrusted(new Event("input", { bubbles: true }))); } catch (_e) {}
  try { el.dispatchEvent(globalThis.__obscura_markTrusted(new Event("change", { bubbles: true }))); } catch (_e) {}
};
globalThis.Event = class Event {
  constructor(t,o={}) { if (arguments.length < 1) throw new TypeError("Failed to construct 'Event': 1 argument required, but only 0 present."); this.type=String(t);this.bubbles=!!o.bubbles;this.cancelable=!!o.cancelable;this.composed=!!o.composed;this.defaultPrevented=false;this.target=null;this.currentTarget=null;this.eventPhase=0;this.timeStamp=Date.now();this._propagationStopped=false;this._immediatePropagationStopped=false; }
  get isTrusted() { return _trustedEvents.has(this); }
  preventDefault() { if (this.cancelable) this.defaultPrevented=true; } stopPropagation(){ this._propagationStopped=true; } stopImmediatePropagation(){ this._propagationStopped=true; this._immediatePropagationStopped=true; }
  initEvent(type,bubbles,cancelable) { if (arguments.length < 1) throw new TypeError("Failed to execute 'initEvent' on 'Event': 1 argument required, but only 0 present."); this.type=String(type);this.bubbles=!!bubbles;this.cancelable=!!cancelable;this.defaultPrevented=false;this._propagationStopped=false;this._immediatePropagationStopped=false; }
  composedPath() {
    if (!this.target) return [];
    const path = [];
    let n = this.target;
    while (n) { path.push(n); n = n.parentNode || null; }
    if (typeof window !== "undefined" && window && path[path.length - 1] !== window) path.push(window);
    return path;
  }
};
_markNative(Event);
globalThis.CustomEvent = class extends Event {
  constructor(t,o={}) { if (arguments.length < 1) throw new TypeError("Failed to construct 'CustomEvent': 1 argument required, but only 0 present."); super(t,o);this.detail=o.detail!==undefined?o.detail:null; }
  // Legacy DOM Level 2 init; some libraries (Starbucks China bundle, older
  // analytics shims) still call createEvent('CustomEvent') + initCustomEvent
  // instead of new CustomEvent(...). See issue #41.
  initCustomEvent(type,bubbles,cancelable,detail) {
    this.type = type;
    this.bubbles = !!bubbles;
    this.cancelable = !!cancelable;
    this.detail = detail;
  }
};
globalThis.MouseEvent = class extends Event {
  constructor(t,o={}) { super(t,o);this.view=o.view||null;this.detail=o.detail||0;this.screenX=o.screenX||0;this.screenY=o.screenY||0;this.clientX=o.clientX||0;this.clientY=o.clientY||0;this.ctrlKey=!!o.ctrlKey;this.altKey=!!o.altKey;this.shiftKey=!!o.shiftKey;this.metaKey=!!o.metaKey;this.button=o.button||0;this.buttons=o.buttons||0;this.relatedTarget=o.relatedTarget||null; }
  // Legacy DOM Level 2 initializer. Positional signature per UI Events spec.
  initMouseEvent(type,canBubble,cancelable,view,detail,screenX,screenY,clientX,clientY,ctrlKey,altKey,shiftKey,metaKey,button,relatedTarget) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'initMouseEvent' on 'MouseEvent': 1 argument required, but only 0 present.");
    this.initEvent(type,canBubble,cancelable);
    this.view=view===undefined?null:view;
    this.detail=detail||0;
    this.screenX=screenX||0;
    this.screenY=screenY||0;
    this.clientX=clientX||0;
    this.clientY=clientY||0;
    this.ctrlKey=!!ctrlKey;
    this.altKey=!!altKey;
    this.shiftKey=!!shiftKey;
    this.metaKey=!!metaKey;
    this.button=button||0;
    this.relatedTarget=relatedTarget===undefined?null:relatedTarget;
  }
};
globalThis.KeyboardEvent = class extends Event {
  constructor(t,o={}) { super(t,o);this.view=o.view||null;this.detail=o.detail||0;this.key=o.key||"";this.code=o.code||"";this.location=o.location||0;this.ctrlKey=!!o.ctrlKey;this.altKey=!!o.altKey;this.shiftKey=!!o.shiftKey;this.metaKey=!!o.metaKey;this.repeat=!!o.repeat; }
  // Legacy DOM Level 3 initializer. Positional signature per the WebKit/Gecko form.
  initKeyboardEvent(type,canBubble,cancelable,view,key,location,ctrlKey,altKey,shiftKey,metaKey) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'initKeyboardEvent' on 'KeyboardEvent': 1 argument required, but only 0 present.");
    this.initEvent(type,canBubble,cancelable);
    this.view=view===undefined?null:view;
    this.key=key===undefined?"":String(key);
    this.location=location||0;
    this.ctrlKey=!!ctrlKey;
    this.altKey=!!altKey;
    this.shiftKey=!!shiftKey;
    this.metaKey=!!metaKey;
  }
};
globalThis.FocusEvent = class extends Event { constructor(t,o={}) { super(t,o);this.relatedTarget=o.relatedTarget||null; } };
globalThis.InputEvent = class extends Event { constructor(t,o={}) { super(t,o);this.data=o.data||null;this.inputType=o.inputType||""; } };
globalThis.ErrorEvent = class extends Event { constructor(t,o={}) { super(t,o);this.message=o.message||"";this.error=o.error||null; } };
globalThis.PointerEvent = class extends Event { constructor(t,o={}) { super(t,o); } };
globalThis.AnimationEvent = class extends Event {};
globalThis.TransitionEvent = class extends Event {};
globalThis.UIEvent = class extends Event {
  constructor(t,o={}) { super(t,o);this.view=o.view||null;this.detail=o.detail||0; }
  // Legacy DOM Level 2 initializer. Positional signature per UI Events spec.
  initUIEvent(type,canBubble,cancelable,view,detail) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'initUIEvent' on 'UIEvent': 1 argument required, but only 0 present.");
    this.initEvent(type,canBubble,cancelable);
    this.view=view===undefined?null:view;
    this.detail=detail||0;
  }
};
// WheelEvent inherits all MouseEvent coordinates and modifier state. CDP
// Input.dispatchMouseEvent supplies those fields and automation libraries use
// them to distinguish wheel gestures over nested panes.
globalThis.WheelEvent = class extends MouseEvent {
  constructor(t,o={}) { super(t,o);this.deltaX=o.deltaX||0;this.deltaY=o.deltaY||0;this.deltaZ=o.deltaZ||0;this.deltaMode=o.deltaMode||0; }
};

globalThis.CompositionEvent = class extends Event {
  constructor(t,o={}) { super(t,o);this.view=o.view||null;this.detail=o.detail||0;this.data=o.data||""; }
  // Legacy DOM Level 3 initializer. Positional signature per UI Events spec.
  initCompositionEvent(type,canBubble,cancelable,view,data) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'initCompositionEvent' on 'CompositionEvent': 1 argument required, but only 0 present.");
    this.initEvent(type,canBubble,cancelable);
    this.view=view===undefined?null:view;
    this.data=data===undefined?"":String(data);
  }
};
globalThis.PopStateEvent = class extends Event {
  constructor(type, init) {
    super(type, init || {});
    // Real PopStateEvent exposes `state` from the entry being navigated to.
    // The earlier stub inherited Event but never stored state, so
    // `popstate.state` was always undefined and SPA routers reading
    // `event.state` to restore route info would mis-render.
    this.state = init && 'state' in init ? init.state : null;
  }
};
globalThis.HashChangeEvent = class extends Event {};
globalThis.MessageEvent = class extends Event {
  constructor(t,o={}) {
    super(t,o);
    this.data = Object.prototype.hasOwnProperty.call(o, "data") ? o.data : null;
    this.origin = o.origin == null ? "" : String(o.origin);
    this.lastEventId = o.lastEventId == null ? "" : String(o.lastEventId);
    this.source = o.source == null ? null : o.source;
    this.ports = Array.isArray(o.ports) ? o.ports.slice() : [];
  }
};
globalThis.ProgressEvent = class ProgressEvent extends Event {
  constructor(type, init) {
    super(type, init || {});
    const i = init || {};
    this.lengthComputable = !!i.lengthComputable;
    this.loaded = i.loaded != null ? Number(i.loaded) : 0;
    this.total = i.total != null ? Number(i.total) : 0;
  }
};
globalThis.ClipboardEvent = class extends Event {};
globalThis.SubmitEvent = class extends Event {};

// ToggleEvent backs the popover beforetoggle/toggle events. oldState and
// newState are "open"/"closed". These events do not bubble; beforetoggle is
// cancelable only for the closed -> open (show) transition, toggle is never
// cancelable. See HTML "popover" and html/semantics/popovers WPT.
globalThis.ToggleEvent = class ToggleEvent extends Event {
  constructor(type, init = {}) {
    super(type, init);
    this.oldState = init.oldState !== undefined ? String(init.oldState) : "";
    this.newState = init.newState !== undefined ? String(init.newState) : "";
  }
};
_markNative(globalThis.ToggleEvent);

globalThis.PromiseRejectionEvent = class PromiseRejectionEvent extends Event {
  constructor(type, init) {
    if (arguments.length < 2 || init == null || !('promise' in Object(init))) {
      throw new TypeError(
        "Failed to construct 'PromiseRejectionEvent': required member promise is undefined."
      );
    }
    super(type, init);
    this.promise = init.promise;
    this.reason = init.reason;
  }
};
_markNative(globalThis.PromiseRejectionEvent);

globalThis.StorageEvent = class StorageEvent extends Event {
  constructor(type, init = {}) {
    super(type, init);
    this.key = init.key !== undefined ? init.key : null;
    this.oldValue = init.oldValue !== undefined ? init.oldValue : null;
    this.newValue = init.newValue !== undefined ? init.newValue : null;
    this.url = init.url || "";
    this.storageArea = init.storageArea || null;
  }
  initStorageEvent(type, bubbles, cancelable, key, oldValue, newValue, url, storageArea) {
    this.initEvent(type, bubbles, cancelable);
    this.key = key !== undefined ? key : null;
    this.oldValue = oldValue !== undefined ? oldValue : null;
    this.newValue = newValue !== undefined ? newValue : null;
    this.url = url || "";
    this.storageArea = storageArea || null;
  }
};
_markNative(globalThis.StorageEvent);

// AbortController / AbortSignal. AbortSignal is a real constructor with a
// prototype, so feature-detection and `AbortSignal.prototype` access work. It
// carries aborted/reason, supports throwIfAborted(), and fires "abort" to
// onabort and addEventListener listeners when the controller aborts.
(function () {
  const BRAND = Symbol("AbortSignal");
  function emit(signal, evt) {
    if (typeof signal.onabort === "function") {
      try { signal.onabort.call(signal, evt); } catch (_) {}
    }
    for (const cb of signal._listeners.slice()) {
      const fn = typeof cb === "function" ? cb : cb && cb.handleEvent;
      if (typeof fn === "function") { try { fn.call(signal, evt); } catch (_) {} }
    }
  }
  function fire(signal, reason) {
    if (signal._aborted) return;
    signal._aborted = true;
    signal._reason = reason !== undefined
      ? reason
      : new DOMException("signal is aborted without reason", "AbortError");
    const evt = typeof Event === "function" ? new Event("abort") : { type: "abort" };
    try { evt.target = signal; evt.currentTarget = signal; } catch (_) {}
    emit(signal, evt);
  }
  globalThis.AbortSignal = class AbortSignal {
    constructor(brand) {
      if (brand !== BRAND) {
        throw new TypeError("Failed to construct 'AbortSignal': Illegal constructor");
      }
      this._aborted = false;
      this._reason = undefined;
      this._listeners = [];
      this.onabort = null;
    }
    get aborted() { return this._aborted; }
    get reason() { return this._reason; }
    throwIfAborted() { if (this._aborted) throw this._reason; }
    addEventListener(type, cb) {
      if (type === "abort" && cb != null) this._listeners.push(cb);
    }
    removeEventListener(type, cb) {
      if (type !== "abort") return;
      const i = this._listeners.indexOf(cb);
      if (i >= 0) this._listeners.splice(i, 1);
    }
    dispatchEvent(evt) {
      if (evt && evt.type === "abort") emit(this, evt);
      return true;
    }
    static abort(reason) {
      const s = new AbortSignal(BRAND);
      s._aborted = true;
      s._reason = reason !== undefined
        ? reason
        : new DOMException("signal is aborted without reason", "AbortError");
      return s;
    }
    static timeout(ms) {
      const s = new AbortSignal(BRAND);
      setTimeout(() => fire(s, new DOMException("signal timed out", "TimeoutError")), ms);
      return s;
    }
    static any(signals) {
      const s = new AbortSignal(BRAND);
      const list = Array.from(signals || []);
      for (const sig of list) {
        if (sig && sig.aborted) { s._aborted = true; s._reason = sig.reason; return s; }
      }
      for (const sig of list) {
        if (sig && typeof sig.addEventListener === "function") {
          sig.addEventListener("abort", () => fire(s, sig.reason));
        }
      }
      return s;
    }
  };
  globalThis.AbortController = class AbortController {
    constructor() { this.signal = new globalThis.AbortSignal(BRAND); }
    abort(reason) { fire(this.signal, reason); }
  };
  _markNative(globalThis.AbortSignal);
  _markNative(globalThis.AbortController);
})();
// Normalize one Blob part to bytes. `native` newline normalization applies to
// string parts when the Blob/File `endings` option is "native".
function _blobPartToBytes(p, native) {
  if (p == null) return new Uint8Array(0);
  if (typeof Blob === "function" && p instanceof Blob) return p._bytes || new Uint8Array(0);
  if (p instanceof ArrayBuffer) return new Uint8Array(p.slice(0));
  if (ArrayBuffer.isView(p)) return new Uint8Array(p.buffer.slice(p.byteOffset, p.byteOffset + p.byteLength));
  let s = String(p);
  if (native) s = s.replace(/\r\n|\r|\n/g, "\n");
  return new TextEncoder().encode(s);
}
function _bytesToBinaryString(bytes) { let s = ""; for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]); return s; }
if (typeof Blob === "undefined") globalThis.Blob = class Blob {
  constructor(parts, opts) {
    opts = opts || {};
    const endings = opts.endings != null ? String(opts.endings) : "transparent";
    if (endings !== "transparent" && endings !== "native") throw new TypeError("Failed to construct 'Blob': The provided value '" + endings + "' is not a valid enum value of type EndingType.");
    const native = endings === "native";
    const chunks = []; let total = 0;
    if (parts != null) {
      if (typeof parts === "string" || typeof parts[Symbol.iterator] !== "function") throw new TypeError("Failed to construct 'Blob': The provided value cannot be converted to a sequence.");
      for (const p of parts) { const b = _blobPartToBytes(p, native); chunks.push(b); total += b.length; }
    }
    const data = new Uint8Array(total); let off = 0;
    for (const c of chunks) { data.set(c, off); off += c.length; }
    this._bytes = data;
    this.size = total;
    const t = opts.type != null ? String(opts.type) : "";
    this.type = /^[\x20-\x7e]*$/.test(t) ? t.toLowerCase() : "";
  }
  get [Symbol.toStringTag]() { return "Blob"; }
  slice(start, end, contentType) {
    const len = this.size;
    const s = start === undefined ? 0 : (start < 0 ? Math.max(len + start, 0) : Math.min(start, len));
    let e = end === undefined ? len : (end < 0 ? Math.max(len + end, 0) : Math.min(end, len));
    if (e < s) e = s;
    const out = new Blob([], contentType != null ? { type: contentType } : {});
    out._bytes = this._bytes.slice(s, e);
    out.size = out._bytes.length;
    return out;
  }
  text() { return Promise.resolve(new TextDecoder().decode(this._bytes)); }
  arrayBuffer() { return Promise.resolve(_arrayBufferFromBytes(this._bytes)); }
  bytes() { return Promise.resolve(this._bytes.slice()); }
};
if (typeof File === "undefined") globalThis.File = class File extends Blob {
  constructor(parts, name, opts) {
    if (arguments.length < 2) throw new TypeError("Failed to construct 'File': 2 arguments required, but only " + arguments.length + " present.");
    opts = opts || {};
    super(parts, opts);
    this.name = String(name);
    this.lastModified = opts.lastModified != null ? Number(opts.lastModified) : Date.now();
  }
  get [Symbol.toStringTag]() { return "File"; }
};
if (typeof FormData === "undefined") globalThis.FormData = class FormData { constructor(){this._d=[];} append(k,v){this._d.push([k,v]);} get(k){const e=this._d.find(([a])=>a===k);return e?e[1]:null;} getAll(k){return this._d.filter(([a])=>a===k).map(([,v])=>v);} has(k){return this._d.some(([a])=>a===k);} entries(){return this._d[Symbol.iterator]();} forEach(cb){this._d.forEach(([k,v])=>cb(v,k));} };
// application/x-www-form-urlencoded serializer: like encodeURIComponent but
// space -> '+' and also percent-encoding the chars encodeURIComponent leaves
// bare ( ! ~ ' ( ) ), keeping the form-urlencoded safe set ( * - . _ ).
function _formEncode(s){
  return encodeURIComponent(String(s)).replace(/%20/g,'+').replace(/[!'()~]/g, c => '%' + c.charCodeAt(0).toString(16).toUpperCase());
}
function _hexv(c){ if(c>=48&&c<=57)return c-48; if(c>=65&&c<=70)return c-55; if(c>=97&&c<=102)return c-87; return -1; }
if (typeof URLSearchParams === "undefined") globalThis.URLSearchParams = class URLSearchParams {
  constructor(init=""){
    this._p=[];
    this._url=null; // set by URL.searchParams so mutations write back to the URL
    if (typeof URLSearchParams === 'function' && init instanceof URLSearchParams) {
      this._p = init._p.map(pair => [pair[0], pair[1]]);
    } else if(typeof init==="string"){
      this._parseString(init);
    } else if (init && typeof init[Symbol.iterator] === 'function') {
      for (const pair of init) {
        const a = Array.from(pair);
        if (a.length !== 2) throw new TypeError("Failed to construct 'URLSearchParams': Each query pair must be an iterable [name, value] tuple");
        this._p.push([String(a[0]), String(a[1])]);
      }
    } else if (init && typeof init === 'object') {
      Object.keys(init).forEach(k => this._p.push([String(k), String(init[k])]));
    }
  }
  _decode(s){
    // application/x-www-form-urlencoded percent-decoding: decode each valid %XX
    // byte, leave invalid escapes literal (decodeURIComponent throws on the whole
    // string instead), '+' -> space, then UTF-8 decode the resulting bytes.
    s = String(s);
    const out = [];
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c === 0x2B) { out.push(0x20); }
      else if (c === 0x25 && i + 2 < s.length) {
        const a = _hexv(s.charCodeAt(i + 1)), b = _hexv(s.charCodeAt(i + 2));
        if (a >= 0 && b >= 0) { out.push(a * 16 + b); i += 2; } else { out.push(c); }
      } else if (c < 0x80) { out.push(c); }
      else { const e = new TextEncoder().encode(s[i]); for (let j = 0; j < e.length; j++) out.push(e[j]); }
    }
    try { return new TextDecoder().decode(new Uint8Array(out)); } catch (e) { return s; }
  }
  _parseString(s){
    s = String(s).replace(/^\?/, "");
    if (s === "") return;
    for (const pair of s.split("&")) {
      if (pair === "") continue;
      const i = pair.indexOf("=");
      const k = i === -1 ? pair : pair.slice(0, i);
      const v = i === -1 ? "" : pair.slice(i + 1);
      this._p.push([this._decode(k), this._decode(v)]);
    }
  }
  _setFromString(s){ this._p = []; this._parseString(s); }
  _notify(){ if (this._url) this._url._updateSearch(this.toString()); }
  append(k,v){ this._p.push([String(k),String(v)]); this._notify(); }
  get(k){k=String(k); const p=this._p.find(([key])=>key===k); return p?p[1]:null;}
  getAll(k){k=String(k); return this._p.filter(([key])=>key===k).map(pair=>pair[1]);}
  set(k,v){k=String(k); v=String(v); let done=false; const out=[]; for (const pair of this._p){ if(pair[0]===k){ if(!done){ out.push([k,v]); done=true; } } else out.push(pair); } if(!done) out.push([k,v]); this._p=out; this._notify(); }
  delete(k,v){k=String(k); const hv=(v!==undefined); v=String(v); this._p=this._p.filter(([key,val])=> hv ? !(key===k&&val===v) : key!==k); this._notify();}
  has(k,v){k=String(k); const hv=(v!==undefined); v=String(v); return this._p.some(([key,val])=> hv ? (key===k&&val===v) : key===k);}
  sort(){ this._p.sort((a,b)=> a[0]<b[0]?-1:(a[0]>b[0]?1:0)); this._notify(); }
  get size(){ return this._p.length; }
  toString(){return this._p.map(pair=>_formEncode(pair[0])+"="+_formEncode(pair[1])).join("&");}
  forEach(cb,thisArg){this._p.slice().forEach(pair=>cb.call(thisArg,pair[1],pair[0],this));}
  *entries(){ for (const pair of this._p) yield [pair[0],pair[1]]; }
  *keys(){ for (const pair of this._p) yield pair[0]; }
  *values(){ for (const pair of this._p) yield pair[1]; }
  [Symbol.iterator](){ return this.entries(); }
};

// Real-enough DOMParser. The previous one-liner returned `globalThis.document`,
// so anything that did `new DOMParser().parseFromString(s, 'text/html')` and
// then read `.body.innerHTML` mutated the LIVE page (jQuery 3.x's selector
// feature-detect writes `<form></form>` and wiped real bodies). We parse the
// input into a detached `<html>` element and wrap it so the common Document
// API surface (body / head / documentElement / querySelector* / getElementById /
// getElementsByTagName / getElementsByClassName / title / cloneNode) works.
// Conservative XML well-formedness check. obscura has no XML parser, so this
// only decides whether to surface a <parsererror> (it does not build an XML
// tree). It flags clear structural errors — mismatched or unclosed tags,
// multiple/no root elements, unterminated comment/CDATA/PI — and defaults to
// "well-formed" whenever the scan is ambiguous, so valid XML is never falsely
// flagged. Quoted attribute regions, comments, CDATA, PIs and the doctype are
// skipped; a literal '<' in text (invalid in XML) reads as a bad tag.
function _xmlWellFormed(src) {
  const s = String(src);
  const stack = [];
  let rootsClosed = 0; // top-level elements fully closed (or self-closed)
  let i = 0;
  const n = s.length;
  while (i < n) {
    const lt = s.indexOf('<', i);
    if (lt === -1) break;
    i = lt;
    if (s.startsWith('<!--', i)) { const e = s.indexOf('-->', i + 4); if (e === -1) return false; i = e + 3; continue; }
    if (s.startsWith('<![CDATA[', i)) { const e = s.indexOf(']]>', i + 9); if (e === -1) return false; i = e + 3; continue; }
    if (s.startsWith('<?', i)) { const e = s.indexOf('?>', i + 2); if (e === -1) return false; i = e + 2; continue; }
    if (s.startsWith('<!', i)) { const e = s.indexOf('>', i + 2); if (e === -1) return false; i = e + 1; continue; }
    // A start/end/self-closing tag: find its '>' while skipping quoted regions.
    let j = i + 1, quote = null;
    while (j < n) {
      const c = s[j];
      if (quote) { if (c === quote) quote = null; }
      else if (c === '"' || c === "'") quote = c;
      else if (c === '>') break;
      j++;
    }
    if (j >= n) return false; // unterminated tag
    const inner = s.slice(i + 1, j).trim();
    i = j + 1;
    if (!inner) return false;
    if (inner[0] === '/') {
      const name = inner.slice(1).trim().split(/\s/)[0];
      if (stack.length === 0 || stack[stack.length - 1] !== name) return false;
      stack.pop();
      if (stack.length === 0) rootsClosed++;
    } else if (inner[inner.length - 1] === '/') {
      if (stack.length === 0) rootsClosed++;
    } else {
      const name = inner.split(/\s/)[0];
      if (!name) return false;
      stack.push(name);
    }
  }
  return stack.length === 0 && rootsClosed === 1;
}

globalThis.DOMParser = class DOMParser {
  parseFromString(source, mimeType) {
    const html = String(source ?? "");
    const isXml = typeof mimeType === "string" && /xml/i.test(mimeType);
    const root = document.createElement("html");
    // innerHTML parses children via html5ever fragment-parsing rules. Most
    // HTML inputs start with `<!DOCTYPE>` / `<html>` / `<head>` etc.; the
    // fragment parser strips the outer `<html>` and emits its head+body
    // children, which is what callers want.
    try { root.innerHTML = html; } catch (e) { /* leave empty on parse error */ }

    // For XML mime types, surface a <parsererror> on clearly-malformed input so
    // error-detection code (doc.querySelector('parsererror')) works, matching
    // Chrome. obscura has no XML parser, so the tree stays HTML-parsed.
    if (isXml && !_xmlWellFormed(html)) {
      try {
        root.innerHTML = '<parsererror xmlns="http://www.w3.org/1999/xhtml">This page contains the following errors:<div>error while parsing XML</div></parsererror>';
      } catch (e) { /* ignore */ }
    }

    // Helper: depth-first walk to find an element by predicate.
    const walk = (node, pred) => {
      if (!node) return null;
      if (node.nodeType === 1 && pred(node)) return node;
      const children = node.children || [];
      for (let i = 0; i < children.length; i++) {
        const r = walk(children[i], pred);
        if (r) return r;
      }
      return null;
    };

    const findByTagName = (name) => walk(root, n => n.tagName === name);

    const docNode = {
      _root: root,
      nodeName: "#document",
      nodeType: 9,
      contentType: isXml ? (mimeType || "application/xml") : "text/html",
      get documentElement() { return root; },
      get body() { return findByTagName("BODY"); },
      get head() { return findByTagName("HEAD"); },
      get title() {
        const t = findByTagName("TITLE");
        return t ? (t.textContent || "").replace(/[\t\n\f\r ]+/g, " ").trim() : "";
      },
      set title(value) {
        let t = findByTagName("TITLE");
        if (!t) {
          let head = findByTagName("HEAD");
          if (!head) {
            head = document.createElement("head");
            root.insertBefore(head, findByTagName("BODY"));
          }
          t = document.createElement("title");
          head.appendChild(t);
        }
        t.textContent = String(value);
      },
      get firstChild() { return root; },
      get lastChild() { return root; },
      get children() { return [root]; },
      get childNodes() { return [root]; },
      // Document metadata the WHATWG interface exposes; DOMParser documents have
      // URL about:blank, are already fully parsed, and carry no stylesheets.
      get URL() { return "about:blank"; },
      get documentURI() { return "about:blank"; },
      get domain() { return _incumbentDocumentDomain(); },
      set domain(value) { String(value); _throwDocumentDomainSecurityError(); },
      get referrer() { return ""; },
      get baseURI() { return "about:blank"; },
      get compatMode() { return "CSS1Compat"; },
      get characterSet() { return "UTF-8"; },
      get charset() { return "UTF-8"; },
      get inputEncoding() { return "UTF-8"; },
      get readyState() { return "complete"; },
      get styleSheets() { return { length: 0, item() { return null; }, [Symbol.iterator]: function* () {} }; },
      get defaultView() { return null; },
      get ownerDocument() { return null; },
      createTreeWalker(r, ws, f) { return document.createTreeWalker(r || root, ws, f); },
      createNodeIterator(r, ws, f) { return document.createNodeIterator(r || root, ws, f); },
      querySelector(s) { return root.querySelector(s); },
      querySelectorAll(s) { return root.querySelectorAll(s); },
      getElementById(id) {
        return walk(root, n => n.getAttribute && n.getAttribute("id") === id);
      },
      getElementsByTagName(t) {
        return root.querySelectorAll(t);
      },
      getElementsByClassName(c) {
        return _getElementsByClassName(root, c);
      },
      getElementsByName(n) {
        return root.querySelectorAll(`[name="${n}"]`);
      },
      createElement: (t) => document.createElement(t),
      createElementNS: (ns, t) => document.createElement(t),
      createTextNode: (t) => document.createTextNode(t),
      createComment: (t) => document.createComment(t),
      createDocumentFragment: () => document.createDocumentFragment(),
      createRange: () => new Range(),
      createEvent: (type) => document.createEvent(type),
      createCDATASection: (data) => {
        if (mimeType === "text/html") throw new DOMException("createCDATASection is not supported in HTML documents", "NotSupportedError");
        const s = String(data);
        if (s.indexOf("]]>") !== -1) throw new DOMException("CDATA section data must not contain ']]>'", "InvalidCharacterError");
        return new CDATASection(+_dom("create_text_node", s));
      },
      createProcessingInstruction: (target, data) => {
        const t = String(target), s = String(data);
        if (!_isValidPITarget(t)) throw new DOMException("Invalid processing instruction target", "InvalidCharacterError");
        if (s.indexOf("?>") !== -1) throw new DOMException("Processing instruction data must not contain '?>'", "InvalidCharacterError");
        return new ProcessingInstruction(+_dom("create_text_node", s), t);
      },
      adoptNode: (n) => n,
      importNode: (n) => n,
      // Document-level node insertion. Detached docs from createHTMLDocument /
      // createDocument back onto the same tree, so appending lands under the
      // documentElement; enough for dom/common.js to build its Range fixtures.
      appendChild: function (n) { try { root.appendChild(n); } catch (e) {} return n; },
      removeChild: function (n) { try { root.removeChild(n); } catch (e) {} return n; },
      insertBefore: function (n, ref) { try { root.insertBefore(n, ref); } catch (e) {} return n; },
      _docType: null,
      get doctype() { return this._docType; },
      cloneNode: function (deep) {
        return new DOMParser().parseFromString(root.outerHTML, mimeType);
      },
      contains(n) { return root.contains ? root.contains(n) : false; },
      addEventListener() {}, removeEventListener() {}, dispatchEvent() { return true; },
    };
    return docNode;
  }
};
globalThis.XMLSerializer = class XMLSerializer {
  serializeToString(node) {
    if (!node) return "";
    if (node.nodeType === 10) {
      let s = "<!DOCTYPE " + (node.name || "html");
      if (node.publicId) s += ' PUBLIC "' + node.publicId + '"';
      if (node.systemId) {
        if (!node.publicId) s += " SYSTEM";
        s += ' "' + node.systemId + '"';
      }
      s += ">";
      return s;
    }
    if (node.outerHTML !== undefined) return node.outerHTML;
    if (node.nodeType === 9) {
      let s = "";
      if (node.doctype) s += this.serializeToString(node.doctype);
      if (node.documentElement) s += node.documentElement.outerHTML;
      return s;
    }
    if (node.nodeType === 3) return node.textContent || "";
    if (node.nodeType === 8) return "<!--" + (node.textContent || "") + "-->";
    return "";
  }
};
globalThis.performance = globalThis.performance || {
  now: (function() {
    // Monotonically non-decreasing: return the wall-clock offset, but never a
    // value below the last one. Equal readings are allowed, and avoiding a
    // synthetic per-call increment keeps tight loops from advancing the clock
    // faster than real elapsed time.
    var _last = -Infinity;
    return function() {
      var ms = Date.now() - (globalThis.performance.timeOrigin || 0);
      if (ms < _last) return _last;
      _last = ms;
      return _last;
    };
  })(),
  mark(){}, measure(){},
  clearMarks(){}, clearMeasures(){}, clearResourceTimings(){},
  getEntries(){return [];}, getEntriesByName(){return [];}, getEntriesByType(){return [];},
  setResourceTimingBufferSize(){},
  timeOrigin: 0,
  timing: { navigationStart: 0, domContentLoadedEventEnd: 0, loadEventEnd: 0 },
  navigation: { type: 0, redirectCount: 0 },
  memory: {
    jsHeapSizeLimit: 4294705152,
    totalJSHeapSize: 19321856,
    usedJSHeapSize: 16781520,
  },
};

var _commonFonts = [
  'Arial', 'Arial Black', 'Arial Narrow',
  'Baskerville', 'Book Antiqua',
  'Calibri', 'Cambria', 'Candara', 'Consolas', 'Courier New',
  'DejaVu Sans', 'DejaVu Sans Mono', 'DejaVu Serif',
  'Futura',
  'Garamond', 'Georgia', 'Gill Sans',
  'Helvetica',
  'Impact',
  'Liberation Sans', 'Liberation Sans Mono', 'Liberation Serif',
  'Lucida Console', 'Lucida Handwriting',
  'Microsoft Sans Serif', 'Monaco',
  'Noto Sans', 'Noto Serif',
  'Palatino Linotype',
  'Segoe UI',
  'Tahoma', 'Times New Roman', 'Trebuchet MS',
  'Verdana',
  'Webdings', 'Wingdings',
];
Object.defineProperty(Document.prototype, 'fonts', {
  get() {
    const _set = _commonFonts.map((name, i) => ({
      family: name, style: 'normal', weight: '400', stretch: 'normal',
      status: 'loaded', loaded: Promise.resolve(this),
      [Symbol.toStringTag]: 'FontFace',
    }));
    _set.forEach = (fn) => { _set.forEach(fn); };
    _set.has = (f) => typeof f === 'string'
      ? _commonFonts.some(n => n.toLowerCase() === f.toLowerCase())
      : _set.some(ff => ff.family === f?.family);
    _set.delete = (f) => false;
    _set.clear = () => {};
    _set.add = () => {};
    _set.load = () => Promise.resolve(_set);
    _set.check = (font) => {
      const m = typeof font === 'string' ? font.match(/["']([^"']+)["']/) : null;
      return m ? _commonFonts.some(n => n.toLowerCase() === m[1].toLowerCase()) : true;
    };
    _set.ready = Promise.resolve(_set);
    _set.status = 'loaded';
    _set.addEventListener = () => {};
    _set.removeEventListener = () => {};
    _set.dispatchEvent = () => true;
    return _set;
  },
  configurable: true,
});
globalThis.Crypto = class Crypto {
  // Fill an integer TypedArray from the OS CSPRNG. Filling the underlying bytes
  // (not per-element Math.random) keeps the distribution uniform across every
  // typed-array width and is actually cryptographically random.
  getRandomValues(arr) {
    if (!ArrayBuffer.isView(arr) || arr instanceof DataView ||
        arr instanceof Float32Array || arr instanceof Float64Array ||
        (typeof Float16Array !== 'undefined' && arr instanceof Float16Array)) {
      throw new DOMException("The provided ArrayBufferView is not an integer-typed array", "TypeMismatchError");
    }
    if (arr.byteLength > 65536) {
      throw new DOMException("The requested length exceeds 65536 bytes", "QuotaExceededError");
    }
    const bytes = Deno.core.ops.op_random_bytes(arr.byteLength);
    new Uint8Array(arr.buffer, arr.byteOffset, arr.byteLength).set(bytes);
    return arr;
  }
  randomUUID() {
    const b = Deno.core.ops.op_random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10xx
    let s = "";
    for (let i = 0; i < 16; i++) {
      s += (b[i] + 0x100).toString(16).slice(1);
      if (i === 3 || i === 5 || i === 7 || i === 9) s += "-";
    }
    return s;
  }
};
globalThis.crypto = globalThis.crypto || new globalThis.Crypto();
// Real structured clone (not JSON). JSON.parse(JSON.stringify) silently drops
// ArrayBuffer/TypedArray (they serialize to {}), so Cloudflare's turnstile
// orchestrate loses every byte it tries to round-trip through postMessage and
// the challenge never completes (issue #389). Clone buffers, typed arrays,
// maps/sets, dates, errors, and plain objects recursively; CryptoKey and other
// types that register a clone hook (see crypto.subtle below) are routed there.
function _structuredClone(value, seen) {
  // Functions and symbols are not structured-cloneable (HTML structured clone,
  // DataCloneError). This must run before the primitive early-return below,
  // which would otherwise pass them through by reference.
  if (typeof value === "function" || typeof value === "symbol") {
    throw new DOMException("Failed to execute 'structuredClone': value could not be cloned.", "DataCloneError");
  }
  if (value === null || typeof value !== "object") return value;
  if (seen.has(value)) return seen.get(value);
  // Typed arrays: copy the underlying buffer slice. DataView has no .slice(),
  // so slice its buffer over the view's range and wrap a fresh view.
  if (ArrayBuffer.isView(value)) {
    if (value instanceof DataView) {
      const buf = value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength);
      const copy = new DataView(buf);
      seen.set(value, copy);
      return copy;
    }
    const Ctor = value.constructor;
    const copy = new Ctor(value.slice());
    seen.set(value, copy);
    return copy;
  }
  if (value instanceof ArrayBuffer) {
    const copy = value.slice(0);
    seen.set(value, copy);
    return copy;
  }
  if (value instanceof SharedArrayBuffer) {
    return value; // transferable, not copyable
  }
  if (value instanceof Date) return new Date(value.getTime());
  if (value instanceof RegExp) return new RegExp(value.source, value.flags);
  if (value instanceof Map) {
    const m = new Map();
    seen.set(value, m);
    for (const [k, v] of value) m.set(_structuredClone(k, seen), _structuredClone(v, seen));
    return m;
  }
  if (value instanceof Set) {
    const s = new Set();
    seen.set(value, s);
    for (const v of value) s.add(_structuredClone(v, seen));
    return s;
  }
  if (value instanceof Error) {
    const Ctor = value.constructor || Error;
    const e = new Ctor(value.message);
    // Record the clone before recursing into `cause`, otherwise a cycle
    // through the error (e.cause === e) recurses until the stack overflows.
    seen.set(value, e);
    if (value.name) e.name = value.name;
    if (value.stack) e.stack = value.stack;
    if (value.cause !== undefined) e.cause = _structuredClone(value.cause, seen);
    return e;
  }
  // Platform objects that carry internal slots opt into cloning via a hook
  // (CryptoKey re-registers its key material so the clone stays usable by
  // crypto.subtle). Anything else with a registered hook takes that path.
  if (typeof value[Symbol.toStringTag] === "string" && globalThis.__obscura_clone_hooks) {
    const hook = globalThis.__obscura_clone_hooks[value[Symbol.toStringTag]];
    if (typeof hook === "function") return hook(value, seen);
  }
  // Plain objects clone onto Object.prototype (like Chrome), not the source's
  // prototype. Define each property instead of assigning it: a source with an
  // own enumerable `__proto__` data prop (what JSON.parse('{"__proto__":…}')
  // yields) would otherwise hit the inherited __proto__ setter and reparent
  // the clone instead of copying the property.
  const out = Array.isArray(value) ? [] : {};
  seen.set(value, out);
  for (const k in value) {
    if (Object.prototype.hasOwnProperty.call(value, k)) {
      const cloned = _structuredClone(value[k], seen);
      // Only `__proto__` needs defineProperty: plain assignment would hit the
      // inherited prototype setter and reparent the clone instead of adding an
      // own data property. Every other key takes the fast assignment path.
      if (k === "__proto__") {
        Object.defineProperty(out, k, {
          value: cloned,
          writable: true,
          enumerable: true,
          configurable: true,
        });
      } else {
        out[k] = cloned;
      }
    }
  }
  // Symbols are not enumerable via for-in; copy own symbol-keyed properties.
  const syms = Object.getOwnPropertySymbols(value);
  for (const s of syms) {
    const d = Object.getOwnPropertyDescriptor(value, s);
    if (d && "value" in d) out[s] = _structuredClone(d.value, seen);
  }
  return out;
}
globalThis.structuredClone = globalThis.structuredClone || ((v) => _structuredClone(v, new Map()));
globalThis.reportError = globalThis.reportError || ((e) => console.error(e));

// WHATWG Storage as a legacy platform object: a Proxy routes property access
// (localStorage.foo, localStorage["foo"], delete, `in`, Object.keys) through
// the named getter/setter so length/key()/iteration stay in sync with the
// backing map. Plain prototype methods alone could not intercept direct
// property access, so `localStorage.foo = x` never updated length before.
globalThis.Storage = function Storage() {};
Storage.prototype.getItem = function(k) { k = String(k); return Object.prototype.hasOwnProperty.call(this._data, k) ? this._data[k] : null; };
Storage.prototype.setItem = function(k, v) { this._data[String(k)] = String(v); };
Storage.prototype.removeItem = function(k) { delete this._data[String(k)]; };
Storage.prototype.clear = function() { const d = this._data; for (const k in d) delete d[k]; };
Storage.prototype.key = function(i) { const ks = Object.keys(this._data); i = i >>> 0; return i < ks.length ? ks[i] : null; };
Object.defineProperty(Storage.prototype, 'length', { get: function() { return Object.keys(this._data).length; }, configurable: true });

const _mkStore = () => {
  const target = Object.create(Storage.prototype);
  Object.defineProperty(target, '_data', { value: Object.create(null), writable: true, enumerable: false, configurable: true });
  const isReal = (p) => p === '_data' || p === 'constructor' || (p in Storage.prototype);
  return new Proxy(target, {
    get(t, p, recv) { if (typeof p === 'symbol' || isReal(p)) return Reflect.get(t, p, recv); const v = t.getItem(p); return v === null ? undefined : v; },
    set(t, p, v, recv) { if (typeof p === 'symbol' || isReal(p)) return Reflect.set(t, p, v, recv); t.setItem(p, v); return true; },
    has(t, p) { if (typeof p === 'symbol' || isReal(p)) return true; return Object.prototype.hasOwnProperty.call(t._data, p); },
    deleteProperty(t, p) { if (typeof p === 'symbol' || isReal(p)) return Reflect.deleteProperty(t, p); t.removeItem(p); return true; },
    ownKeys(t) { return Object.keys(t._data); },
    getOwnPropertyDescriptor(t, p) {
      if (typeof p !== 'symbol' && Object.prototype.hasOwnProperty.call(t._data, p))
        return { value: t._data[p], writable: true, enumerable: true, configurable: true };
      return Reflect.getOwnPropertyDescriptor(t, p);
    },
  });
};
globalThis.localStorage = _mkStore();
globalThis.sessionStorage = _mkStore();

globalThis.btoa = globalThis.btoa || ((s) => { const b = new TextEncoder().encode(s); const c="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; let r=""; for(let i=0;i<b.length;i+=3){const a=b[i],bb=b[i+1]??0,cc=b[i+2]??0; r+=c[a>>2]+c[((a&3)<<4)|(bb>>4)]+(i+1<b.length?c[((bb&15)<<2)|(cc>>6)]:"=")+(i+2<b.length?c[cc&63]:"=");} return r; });
globalThis.atob = globalThis.atob || ((s) => {
  const c="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const r=[];
  s=String(s).replace(/[\t\n\f\r ]/g,"");
  for(let i=0;i<s.length;i+=4){
    const a=c.indexOf(s[i]),b=c.indexOf(s[i+1]),cc=c.indexOf(s[i+2]),d=c.indexOf(s[i+3]);
    r.push((a<<2)|(b>>4));
    if(cc>=0)r.push(((b&15)<<4)|(cc>>2));
    if(d>=0)r.push(((cc&3)<<6)|d);
  }
  // Spreading a large decoded payload into one call overflows V8's argument
  // stack. Angular and other SSR frameworks routinely decode blobs large
  // enough to hit that ceiling.
  let out="";
  const chunk=0x8000;
  for(let i=0;i<r.length;i+=chunk) out+=String.fromCharCode(...r.slice(i,i+chunk));
  return out;
});

// Functional History API. The earlier stub returned constant state and was a
// no-op on push/replace, so any SPA that tried to update its URL (Next.js
// client router, React Router, vue-router, hash-based routers) silently
// failed: location.href stayed pinned to the initial page, useLocation hooks
// never updated, and popstate-driven UI froze.
//
// Internally we keep a tiny in-memory stack of {state, url} entries. push/
// replace mutate the stack and set globalThis.__virtualUrl so location.href
// reads the new URL. Real Chrome doesn't fire popstate on push/replace,
// only on user-driven back/forward — we match that exactly.
(() => {
  const stack = [{state: null, url: undefined}]; // initial entry; url=undefined means "use document URL"
  let idx = 0;
  const historyToken = Symbol("History");
  const resolveOrFallback = (url) => {
    // A missing url (pushState/replaceState called with < 3 args) keeps the
    // current document URL per the HTML spec — capture it so the entry does not
    // reset location back to the original document URL.
    if (url === null || url === undefined) return __currentUrl();
    try { return new URL(String(url), __currentUrl()).href; } catch (e) { return String(url); }
  };
  const applyVirtual = () => {
    const entry = stack[idx];
    globalThis.__virtualUrl = entry.url ?? null;
  };
  const fireHashChangeIfNeeded = (prevUrl) => {
    try {
      const next = __currentUrl();
      if (!prevUrl || !next) return;
      const a = new URL(prevUrl), b = new URL(next);
      if (a.origin === b.origin && a.pathname === b.pathname && a.search === b.search && a.hash !== b.hash) {
        const ev = new Event('hashchange');
        ev.oldURL = prevUrl; ev.newURL = next;
        try { globalThis.dispatchEvent(ev); } catch {}
      }
    } catch {}
  };
  class History {
    constructor(token) {
      if (token !== historyToken) throw new TypeError("Illegal constructor");
    }
    get length() { return stack.length; }
    get state() { return stack[idx].state; }
    get scrollRestoration() { return this._scrollRestoration || "auto"; }
    set scrollRestoration(value) {
      const normalized = String(value);
      if (normalized === "auto" || normalized === "manual") {
        this._scrollRestoration = normalized;
      }
    }
    pushState(state, _title, url) {
      const prevUrl = __currentUrl();
      const resolved = resolveOrFallback(url);
      // Truncate forward entries (real Chrome drops the forward stack on a
      // new push) then append + advance.
      stack.length = idx + 1;
      stack.push({state: state ?? null, url: resolved});
      idx = stack.length - 1;
      applyVirtual();
      fireHashChangeIfNeeded(prevUrl);
    }
    replaceState(state, _title, url) {
      const prevUrl = __currentUrl();
      const resolved = resolveOrFallback(url);
      stack[idx] = {state: state ?? null, url: resolved};
      applyVirtual();
      fireHashChangeIfNeeded(prevUrl);
    }
    go(n) {
      n = (n | 0);
      if (n === 0) return; // real spec: go(0) reloads. We don't reload SPAs.
      const next = Math.max(0, Math.min(stack.length - 1, idx + n));
      if (next === idx) return;
      const prevUrl = __currentUrl();
      idx = next;
      applyVirtual();
      // Real Chrome fires popstate on back/forward with the destination entry's state.
      try {
        const ev = new PopStateEvent('popstate', {state: stack[idx].state});
        globalThis.dispatchEvent(ev);
      } catch {}
      fireHashChangeIfNeeded(prevUrl);
    }
    back() { this.go(-1); }
    forward() { this.go(1); }
  }
  Object.defineProperty(History.prototype, Symbol.toStringTag, {value: "History"});
  Object.defineProperty(globalThis, "History", {
    value: History, writable: true, configurable: true,
  });
  Object.defineProperty(globalThis, "history", {
    value: new History(historyToken), writable: true, configurable: true,
  });
})();

// Navigation API. New framework routers increasingly prefer `navigation`
// over popstate/history. Keep it backed by the functional History API above
// so both surfaces agree about the current URL and state.
(() => {
  const listeners = Object.create(null);
  const nav = {
    addEventListener(type, callback) {
      if (typeof callback !== "function") return;
      (listeners[String(type)] ||= []).push(callback);
    },
    removeEventListener(type, callback) {
      const list = listeners[String(type)];
      if (!list) return;
      const index = list.indexOf(callback);
      if (index >= 0) list.splice(index, 1);
    },
    dispatchEvent(event) {
      if (!event || !event.type) return true;
      const list = (listeners[String(event.type)] || []).slice();
      for (const callback of list) {
        try { callback.call(nav, event); } catch (error) { console.error(error); }
      }
      return !event.defaultPrevented;
    },
  };
  let serial = 0;
  const makeEntry = () => {
    const key = "obscura-" + serial;
    const state = history.state;
    return {
      id: key,
      key,
      index: Math.max(0, history.length - 1),
      sameDocument: true,
      url: __currentUrl(),
      getState() { return state; },
      addEventListener() {},
      removeEventListener() {},
    };
  };
  let entry = makeEntry();
  const changed = (from) => {
    const old = from || entry;
    serial++;
    entry = makeEntry();
    try {
      const ev = new Event("currententrychange");
      ev.from = old;
      nav.dispatchEvent(ev);
    } catch {}
    return entry;
  };
  Object.defineProperties(nav, {
    currentEntry: { configurable: true, enumerable: true, get: () => entry },
    canGoBack: { configurable: true, enumerable: true, get: () => history.length > 1 },
    canGoForward: { configurable: true, enumerable: true, get: () => false },
    transition: { configurable: true, enumerable: true, get: () => null },
    activation: { configurable: true, enumerable: true, get: () => null },
  });
  nav.entries = () => [entry];
  nav.updateCurrentEntry = (options) => {
    const old = entry;
    const state = options && Object.prototype.hasOwnProperty.call(options, "state")
      ? options.state : history.state;
    history.replaceState(state, "", __currentUrl());
    return changed(old);
  };
  nav.navigate = (url, options) => {
    const old = entry;
    const state = options && Object.prototype.hasOwnProperty.call(options, "state")
      ? options.state : null;
    if (options && options.history === "replace") history.replaceState(state, "", url);
    else history.pushState(state, "", url);
    const next = changed(old);
    const done = Promise.resolve(next);
    return { committed: done, finished: done };
  };
  nav.reload = () => {
    const done = Promise.resolve(entry);
    return { committed: done, finished: done };
  };
  nav.traverseTo = () => {
    const done = Promise.resolve(entry);
    return { committed: done, finished: done };
  };
  nav.back = () => {
    history.back();
    const done = Promise.resolve(changed());
    return { committed: done, finished: done };
  };
  nav.forward = () => {
    history.forward();
    const done = Promise.resolve(changed());
    return { committed: done, finished: done };
  };
  globalThis.navigation = nav;
})();

globalThis.screenX = 0; globalThis.screenY = 0;
globalThis.screenLeft = 0; globalThis.screenTop = 0;
globalThis.pageXOffset = 0; globalThis.pageYOffset = 0;
globalThis.scrollX = 0; globalThis.scrollY = 0;

// Keep the JavaScript capability surface aligned with the declarations the
// renderer actually implements. Reporting an unknown declaration as supported
// is not harmless: Tailwind and other framework sheets use negative probes to
// select legacy-browser fallbacks, which can replace their modern cascade.
const _CSS_SUPPORTED_DECLARATIONS = new Set((
  "display width height min-width min-height max-width max-height box-sizing aspect-ratio content " +
  "margin margin-top margin-right margin-bottom margin-left margin-inline margin-inline-start " +
  "margin-inline-end margin-block margin-block-start margin-block-end padding padding-top " +
  "padding-right padding-bottom padding-left padding-inline padding-inline-start padding-inline-end " +
  "padding-block padding-block-start padding-block-end border-radius border border-width " +
  "border-top-width border-right-width border-bottom-width border-left-width border-top border-right " +
  "border-bottom border-left background background-color background-image background-size " +
  "background-position background-clip -webkit-background-clip mask-image -webkit-mask-image " +
  "mask-size -webkit-mask-size mask-repeat -webkit-mask-repeat color -webkit-text-fill-color fill " +
  "stroke stroke-width border-color font-size font font-weight font-family font-style text-align " +
  "text-transform text-decoration text-decoration-line line-height white-space overflow-wrap word-wrap word-break text-wrap text-wrap-style align-items justify-items " +
  "place-items align-self justify-self place-self align-content justify-content place-content " +
  "flex-flow flex-direction flex-wrap flex-grow flex-shrink flex-basis flex order position float object-fit " +
  "top right bottom left inset overflow overflow-x overflow-y scrollbar-gutter visibility opacity animation " +
  "animation-name animation-fill-mode animation-iteration-count z-index clear vertical-align " +
  "list-style list-style-type gap grid-gap row-gap grid-row-gap column-gap grid-column-gap " +
  "border-spacing border-collapse grid-template-columns grid-template-rows grid-template-areas " +
  "grid-template grid grid-auto-flow grid-area grid-column grid-row grid-column-start " +
  "grid-column-end grid-row-start grid-row-end transform filter backdrop-filter " +
  "-webkit-backdrop-filter perspective contain will-change content-visibility box-shadow " +
  "-webkit-box-shadow"
).split(/\s+/));

const _CSS_SUPPORTED_COLOR_NAMES = new Set((
  "transparent white black gray grey silver lightgray lightgrey darkgray darkgrey whitesmoke " +
  "gainsboro red green lime blue navy yellow orange purple maroon teal aqua cyan fuchsia magenta " +
  "olive darkblue mediumblue royalblue dodgerblue cornflowerblue steelblue deepskyblue skyblue " +
  "lightskyblue lightblue powderblue cadetblue slateblue darkslateblue midnightblue indigo " +
  "darkgreen forestgreen seagreen mediumseagreen limegreen yellowgreen olivedrab darkolivegreen " +
  "greenyellow lightgreen palegreen springgreen mediumaquamarine aquamarine turquoise " +
  "mediumturquoise darkcyan crimson firebrick darkred indianred tomato orangered coral salmon " +
  "lightsalmon darksalmon hotpink deeppink pink lightpink palevioletred mediumvioletred violet " +
  "orchid plum mediumpurple blueviolet darkviolet darkorchid darkmagenta lavender thistle gold " +
  "goldenrod darkgoldenrod khaki darkkhaki peachpuff moccasin papayawhip wheat tan burlywood " +
  "sandybrown peru chocolate sienna saddlebrown brown rosybrown darkorange lightyellow " +
  "lightgoldenrodyellow lemonchiffon beige ivory azure mintcream honeydew snow seashell linen " +
  "oldlace floralwhite ghostwhite aliceblue lavenderblush mistyrose cornsilk antiquewhite bisque " +
  "blanchedalmond navajowhite dimgray dimgrey slategray slategrey lightslategray lightslategrey " +
  "darkslategray darkslategrey"
).split(/\s+/));

function _cssSupportsColor(value) {
  const raw = value.trim();
  const lower = raw.toLowerCase();
  if (_CSS_SUPPORTED_COLOR_NAMES.has(lower)) return true;
  if (/^#[0-9a-f]{3,4}(?:[0-9a-f]{2}){0,2}$/i.test(lower)) {
    return [4, 5, 7, 9].includes(lower.length);
  }
  if (lower.startsWith("var(") && lower.endsWith(")")) {
    const comma = _cssTopLevelComma(raw.slice(4, -1));
    return comma >= 0 && _cssSupportsColor(raw.slice(4 + comma + 1, -1));
  }
  if (/^rgba?\(/.test(lower) && lower.endsWith(")")) {
    // Keep the non-render build aligned with the renderer's capability
    // evaluator: relative colors are valid CSS, but are not implemented by
    // Obscura yet and therefore must not select an unsupported @supports arm.
    if (/\bfrom\b/.test(lower)) {
      return false;
    }
    const parts = lower.slice(lower.indexOf("(") + 1, -1)
      .split(/[,\s/]+/).filter(Boolean);
    return parts.length >= 3 && parts.slice(0, 3)
      .every((part) => /^[-+]?(?:\d+(?:\.\d*)?|\.\d+)%?$/.test(part));
  }
  if (/^hsla?\(/.test(lower) && lower.endsWith(")")) {
    const parts = lower.slice(lower.indexOf("(") + 1, -1)
      .split(/[,\s/]+/).filter(Boolean);
    return parts.length >= 3 && parts.slice(0, 3).every((part) =>
      /^[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:deg|%)?$/.test(part));
  }
  if (/^okl(?:ab|ch)\(/.test(lower) && lower.endsWith(")")) {
    const parts = lower.slice(lower.indexOf("(") + 1, -1)
      .split(/[,\s/]+/).filter(Boolean);
    return parts.length >= 3 && parts.slice(0, 3).every((part) =>
      /^[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:deg|%)?$/.test(part));
  }
  if (lower.startsWith("light-dark(") && lower.endsWith(")")) {
    const parts = _cssSplitTopLevel(
      raw.slice("light-dark(".length, -1),
      ","
    );
    return !!parts && parts.length === 2 &&
      parts.every((part) => part.trim() && _cssSupportsColor(part));
  }
  if (lower.startsWith("color-mix(") && lower.endsWith(")")) {
    const parts = _cssSplitTopLevel(lower.slice("color-mix(".length, -1), ",");
    if (!parts || parts.length < 3 || !/^in\s+\S+$/i.test(parts[0].trim())) return false;
    const color = (part) => _cssSupportsColor(part.trim().replace(/\s+[-+]?(?:\d+(?:\.\d*)?|\.\d+)%\s*$/, ""));
    return color(parts[1]) && color(parts[2]);
  }
  return false;
}

function _cssTopLevelComma(text) {
  let depth = 0, quote = "";
  for (let i = 0; i < text.length; i++) {
    const character = text[i];
    if (quote) {
      if (character === "\\") i++;
      else if (character === quote) quote = "";
      continue;
    }
    if (character === "'" || character === '"') quote = character;
    else if (character === "(") depth++;
    else if (character === ")") depth--;
    else if (character === "," && depth === 0) return i;
    if (depth < 0) return -1;
  }
  return -1;
}

function _cssSupportsDeclaration(name, value) {
  name = name.trim().toLowerCase();
  value = value.trim();
  if (typeof Deno.core.ops.op_css_supports === "function") {
    try { return !!Deno.core.ops.op_css_supports(name, value); }
    catch (_) { return false; }
  }
  if (!value || _cssHasInvalidSupportsValueSyntax(value)) return false;
  if (name.startsWith("--")) return name.length > 2;
  if (!_CSS_SUPPORTED_DECLARATIONS.has(name)) return false;
  const lower = value.toLowerCase();
  if (["initial", "inherit", "unset", "revert", "revert-layer"].includes(lower)) return true;
  if (name === "display") {
    return ["none", "flex", "inline-flex", "inline", "inline-block", "grid",
      "inline-grid", "block", "flow-root", "table", "inline-table", "contents"].includes(lower);
  }
  if (name === "position") {
    return ["static", "relative", "absolute", "fixed", "sticky"].includes(lower);
  }
  if (name === "box-sizing") return ["content-box", "border-box"].includes(lower);
  if (name === "float") return ["none", "left", "right"].includes(lower);
  if (name === "object-fit") {
    return ["fill", "contain", "cover", "none", "scale-down"].includes(lower);
  }
  if (name === "visibility") return ["visible", "hidden", "collapse"].includes(lower);
  if (name === "scrollbar-gutter") {
    return lower === "auto" || lower === "stable" || lower === "stable both-edges";
  }
  if (name === "white-space") {
    return ["normal", "nowrap", "pre", "pre-wrap", "pre-line", "break-spaces"].includes(lower);
  }
  if (name === "overflow-wrap" || name === "word-wrap") {
    return ["normal", "break-word", "anywhere"].includes(lower);
  }
  if (name === "word-break") {
    return ["normal", "break-all", "keep-all", "break-word"].includes(lower);
  }
  if (name === "text-wrap") {
    return ["auto", "wrap", "balance", "wrap balance", "balance wrap"].includes(lower);
  }
  if (name === "text-wrap-style") return lower === "auto" || lower === "balance";
  if (["filter", "backdrop-filter", "-webkit-backdrop-filter", "perspective"].includes(name)) {
    return lower === "none";
  }
  if (name === "contain") return lower === "none";
  if (name === "content-visibility") return lower === "visible";
  if (name === "content") return _cssSupportsContent(value);
  if (["border", "border-top", "border-right", "border-bottom", "border-left"].includes(name)) {
    if (lower === "none") return true;
    const parts = _cssSplitWhitespace(value);
    if (!parts.length || parts.length > 3) return false;
    let widths = 0, styles = 0, colors = 0;
    for (const part of parts) {
      const token = part.toLowerCase();
      if (["thin", "medium", "thick"].includes(token) ||
          (_cssSupportsDimension(part, false) && !token.includes("%"))) widths++;
      else if (["none", "hidden", "dotted", "dashed", "solid", "double", "groove", "ridge", "inset", "outset"].includes(token)) styles++;
      else if (_cssSupportsColor(part) || token === "currentcolor") colors++;
      else return false;
    }
    return widths <= 1 && styles <= 1 && colors <= 1;
  }
  if (["width", "height", "min-width", "min-height", "max-width", "max-height", "flex-basis"].includes(name)) {
    return _cssSupportsDimension(value, true) || (name === "width" && lower === "fit-content");
  }
  if (/^(?:margin(?:-(?:top|right|bottom|left|inline|inline-start|inline-end|block|block-start|block-end))?|padding(?:-(?:top|right|bottom|left|inline|inline-start|inline-end|block|block-start|block-end))?|inset(?:-(?:inline|inline-start|inline-end|block|block-start|block-end))?|top|right|bottom|left)$/.test(name)) {
    const allowAuto = name.startsWith("margin") || name === "top" || name === "right" || name === "bottom" || name === "left" || name.startsWith("inset");
    const parts = _cssSplitWhitespace(value);
    const max = /^(?:margin|padding|inset)$/.test(name) ? 4 : (/(?:inline|block)$/.test(name) ? 2 : 1);
    return parts.length > 0 && parts.length <= max && parts.every((part) => _cssSupportsDimension(part, allowAuto));
  }
  if (["align-items", "justify-items", "align-self", "justify-self"].includes(name)) {
    return _cssSupportsSelfAlignment(lower);
  }
  if (name === "align-content" || name === "justify-content") {
    return _cssSupportsContentAlignment(lower) || (name === "justify-content" && ["left", "right"].includes(lower));
  }
  if (name === "flex-flow") {
    const tokens = _cssSplitWhitespace(lower);
    if (tokens.length < 1 || tokens.length > 2) return false;
    let direction = false, wrap = false;
    for (const token of tokens) {
      if (["row", "row-reverse", "column", "column-reverse"].includes(token)) {
        if (direction) return false;
        direction = true;
      } else if (["nowrap", "wrap", "wrap-reverse"].includes(token)) {
        if (wrap) return false;
        wrap = true;
      } else {
        return false;
      }
    }
    return true;
  }
  if (name === "flex-direction") return ["row", "row-reverse", "column", "column-reverse"].includes(lower);
  if (name === "flex-wrap") return ["nowrap", "wrap", "wrap-reverse"].includes(lower);
  if (name === "flex-grow" || name === "flex-shrink") {
    return /^(?:\d+(?:\.\d*)?|\.\d+)$/.test(lower);
  }
  if (name === "order") return /^[-+]?\d+$/.test(lower);
  if (name === "opacity") return /^[-+]?(?:\d+(?:\.\d*)?|\.\d+)$/.test(lower);
  if (name === "z-index") return lower === "auto" || /^[-+]?\d+$/.test(lower);
  if (["color", "-webkit-text-fill-color", "background-color", "border-color"].includes(name)) {
    return _cssSupportsColor(value);
  }
  return false;
}

function _cssHasInvalidSupportsValueSyntax(value) {
  let depth = 0, quote = "";
  for (let i = 0; i < value.length; i++) {
    const character = value[i];
    if (quote) {
      if (character === "\\") i++;
      else if (character === quote) quote = "";
      continue;
    }
    if (character === "\\") { i++; continue; }
    if (character === "'" || character === '"') quote = character;
    else if (character === "(" || character === "[") depth++;
    else if (character === ")" || character === "]") {
      if (--depth < 0) return true;
    } else if (depth === 0 && /[;{}]/.test(character)) return true;
    else if (depth === 0 && character === "!" && /^\s*important\b/i.test(value.slice(i + 1))) return true;
  }
  return depth !== 0 || !!quote;
}

function _cssSplitWhitespace(value) {
  const values = [], split = _cssSplitTopLevel(value, " ");
  if (split) return split;
  let depth = 0, quote = "", start = -1;
  for (let i = 0; i <= value.length; i++) {
    const character = value[i] || " ";
    if (quote) {
      if (character === "\\") i++;
      else if (character === quote) quote = "";
    } else if (character === "'" || character === '"') quote = character;
    else if (character === "(") depth++;
    else if (character === ")") depth--;
    if (/\s/.test(character) && depth === 0 && !quote) {
      if (start >= 0) values.push(value.slice(start, i));
      start = -1;
    } else if (start < 0) start = i;
  }
  return values;
}

function _cssSupportsDimension(value, allowAuto) {
  const lower = value.trim().toLowerCase();
  if (allowAuto && lower === "auto") return true;
  if (/^[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:px|pt|em|ex|rem|vw|vh|dvw|dvh|svw|svh|lvw|lvh|vmin|vmax|%)$/i.test(lower)) return true;
  if (/^[-+]?0(?:\.0*)?$/.test(lower)) return true;
  return /^(?:calc|min|max|clamp|var)\(.+\)$/i.test(lower);
}

function _cssSupportsSelfAlignment(value) {
  return /^(?:auto|normal|stretch|baseline|first baseline|center|(?:safe |unsafe )?(?:start|end|self-start|self-end|flex-start|flex-end))$/.test(value);
}

function _cssSupportsContentAlignment(value) {
  return /^(?:normal|stretch|baseline|first baseline|space-between|space-around|space-evenly|(?:safe |unsafe )?(?:start|end|flex-start|flex-end|center))$/.test(value);
}

function _cssSupportsContent(value) {
  const lower = value.trim().toLowerCase();
  if (lower === "none" || lower === "normal" || _cssSupportsSingleUrl(value)) return true;
  let rest = value.trim(), found = false;
  while (rest) {
    rest = rest.trimStart();
    if (rest[0] === "'" || rest[0] === '"') {
      const quote = rest[0];
      let end = 1;
      for (; end < rest.length; end++) {
        if (rest[end] === "\\") end++;
        else if (rest[end] === quote) break;
      }
      if (end >= rest.length) return false;
      rest = rest.slice(end + 1);
      found = true;
      continue;
    }
    const keyword = /^(?:open-quote|close-quote|no-open-quote|no-close-quote)\b/i.exec(rest);
    if (keyword) {
      rest = rest.slice(keyword[0].length);
      found = true;
      continue;
    }
    const fn = /^(attr|counter|counters)\(/i.exec(rest);
    if (!fn) return false;
    let depth = 0, quote = "", end = -1;
    for (let i = fn[1].length; i < rest.length; i++) {
      const character = rest[i];
      if (quote) {
        if (character === "\\") i++;
        else if (character === quote) quote = "";
      } else if (character === "'" || character === '"') quote = character;
      else if (character === "(") depth++;
      else if (character === ")" && --depth === 0) { end = i; break; }
    }
    const argumentsText = rest.slice(fn[0].length, end).trim();
    if (end < 0 || !_cssSupportsContentFunction(fn[1].toLowerCase(), argumentsText)) return false;
    rest = rest.slice(end + 1);
    found = true;
  }
  return found;
}

function _cssSupportsSingleUrl(value) {
  value = value.trim();
  if (!/^url\(/i.test(value) || !value.endsWith(")")) return false;
  let depth = 0, quote = "";
  for (let i = 0; i < value.length; i++) {
    const character = value[i];
    if (quote) {
      if (character === "\\") i++;
      else if (character === quote) quote = "";
      continue;
    }
    if (character === "\\") { i++; continue; }
    if (character === "'" || character === '"') quote = character;
    else if (character === "(") depth++;
    else if (character === ")" && --depth === 0) {
      return i === value.length - 1 && value.slice(4, i).trim().length > 0;
    }
  }
  return false;
}

function _cssSupportsContentFunction(name, argumentsText) {
  const argumentsList = _cssSplitTopLevel(argumentsText, ",") || [argumentsText];
  const ident = (value) => /^[a-z0-9_\\-]+$/i.test(value.trim());
  const counterStyle = (value) => /^(?:decimal|decimal-leading-zero|lower-alpha|lower-latin|upper-alpha|upper-latin|lower-roman|upper-roman)$/i.test(value.trim());
  if (name === "attr") return argumentsList.length === 1 && ident(argumentsList[0].trim().split(/\s+/)[0]);
  if (name === "counter") {
    return argumentsList.length >= 1 && argumentsList.length <= 2 && ident(argumentsList[0]) &&
      (argumentsList.length === 1 || counterStyle(argumentsList[1]));
  }
  if (name === "counters") {
    return argumentsList.length >= 2 && argumentsList.length <= 3 && ident(argumentsList[0]) &&
      /^(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')$/.test(argumentsList[1].trim()) &&
      (argumentsList.length === 2 || counterStyle(argumentsList[2]));
  }
  return false;
}

// Return the contents only when one pair of parentheses encloses the complete
// expression. Declaration leaves such as `(display:grid)` are then evaluated
// by the same path as the two-argument overload.
function _cssEnclosingGroup(text) {
  if (!text.startsWith("(")) return null;
  let depth = 0, quote = "";
  for (let i = 0; i < text.length; i++) {
    const character = text[i];
    if (quote) {
      if (character === "\\") i++;
      else if (character === quote) quote = "";
      continue;
    }
    if (character === "'" || character === '"') quote = character;
    else if (character === "(") depth++;
    else if (character === ")") {
      depth--;
      if (depth < 0) return null;
      if (depth === 0) return i === text.length - 1 ? text.slice(1, i) : null;
    }
  }
  return null;
}

function _cssSplitTopLevel(text, operator) {
  const parts = [];
  const isWord = /^[a-z]+$/i.test(operator);
  let start = 0, depth = 0, quote = "";
  for (let i = 0; i < text.length; i++) {
    const character = text[i];
    if (quote) {
      if (character === "\\") i++;
      else if (character === quote) quote = "";
      continue;
    }
    if (character === "'" || character === '"') {
      quote = character;
      continue;
    }
    if (character === "(" || character === "[") depth++;
    else if (character === ")" || character === "]") {
      depth--;
      if (depth < 0) return null;
    } else if (depth === 0 &&
        text.slice(i, i + operator.length).toLowerCase() === operator.toLowerCase() &&
        (!isWord || (i > 0 && /\s/.test(text[i - 1]) &&
          i + operator.length < text.length && /\s/.test(text[i + operator.length])))) {
      const part = text.slice(start, i).trim();
      if (!part) return null;
      parts.push(part);
      i += operator.length - 1;
      start = i + 1;
    }
  }
  if (depth !== 0 || quote || !parts.length) return null;
  const tail = text.slice(start).trim();
  if (!tail) return null;
  parts.push(tail);
  return parts;
}

function _cssHasTopLevelComma(text) {
  let depth = 0, quote = "";
  for (let i = 0; i < text.length; i++) {
    const character = text[i];
    if (quote) {
      if (character === "\\") i++;
      else if (character === quote) quote = "";
      continue;
    }
    if (character === "\\") { i++; continue; }
    if (character === "'" || character === '"') quote = character;
    else if (character === "(" || character === "[") depth++;
    else if (character === ")" || character === "]") depth--;
    else if (character === "," && depth === 0) return true;
    if (depth < 0) return false;
  }
  return false;
}

const _CSS_SUPPORTED_SIMPLE_PSEUDOS = new Set((
  "hover active focus focus-visible focus-within enabled disabled checked link any-link visited " +
  "first-child last-child only-child root empty scope first-of-type last-of-type only-of-type " +
  "before after"
).split(/\s+/));
const _CSS_SUPPORTED_FUNCTIONAL_PSEUDOS = new Set((
  "nth-child nth-of-type nth-last-child nth-last-of-type is where has host not"
).split(/\s+/));

function _cssSupportsSelector(selector) {
  selector = selector.trim();
  if (!selector || /[{};]/.test(selector)) return false;
  const split = _cssSplitTopLevel(selector, ",");
  if (!split && _cssHasTopLevelComma(selector)) return false;
  const selectors = split || [selector];
  return selectors.every((part) => {
    part = part.trim();
    if (!part || /^[>+~]/.test(part) || /[>+~]\s*$/.test(part)) return false;
    let parens = 0, brackets = 0, quote = "";
    for (let i = 0; i < part.length; i++) {
      const character = part[i];
      if (quote) {
        if (character === "\\") i++;
        else if (character === quote) quote = "";
        continue;
      }
      if (character === "\\") { i++; continue; }
      if (character === "'" || character === '"') quote = character;
      else if (character === "(") parens++;
      else if (character === ")") parens--;
      else if (character === "[") brackets++;
      else if (character === "]") brackets--;
      else if (character === ":" && brackets === 0) {
        const doubleColon = part[i + 1] === ":";
        let end = i + (doubleColon ? 2 : 1);
        const start = end;
        while (end < part.length && /[a-z0-9_-]/i.test(part[end])) end++;
        if (end === start) return false;
        const name = part.slice(start, end).toLowerCase();
        const functional = part[end] === "(";
        if (doubleColon) {
          if (functional || !["before", "after"].includes(name)) return false;
        } else if (functional) {
          if (!_CSS_SUPPORTED_FUNCTIONAL_PSEUDOS.has(name)) return false;
        } else if (!_CSS_SUPPORTED_SIMPLE_PSEUDOS.has(name)) {
          return false;
        }
        i = end - 1;
      }
      if (parens < 0 || brackets < 0) return false;
    }
    return !quote && parens === 0 && brackets === 0;
  });
}

function _cssBalancedSupportsSyntax(condition) {
  const stack = [];
  let quote = "";
  for (let i = 0; i < condition.length; i++) {
    const character = condition[i];
    if (quote) {
      if (character === "\\") i++;
      else if (character === quote) quote = "";
      continue;
    }
    if (character === "\\") { i++; continue; }
    if (character === "'" || character === '"') quote = character;
    else if (character === "(") stack.push(")");
    else if (character === "[") stack.push("]");
    else if ((character === ")" || character === "]") && stack.pop() !== character) return false;
  }
  return !quote && stack.length === 0;
}

// `null` means invalid syntax, which is distinct from a valid false leaf.
// In particular `not <invalid>` must remain false rather than flipping true.
function _cssSupportsConditionResult(condition) {
  condition = condition.trim();
  if (!condition || !_cssBalancedSupportsSyntax(condition)) return null;
  const grouped = _cssEnclosingGroup(condition);
  if (grouped !== null) return _cssSupportsConditionResult(grouped);
  if (/^not\s/i.test(condition)) {
    const result = _cssSupportsConditionResult(condition.slice(3).trim());
    return result === null ? null : !result;
  }
  const orParts = _cssSplitTopLevel(condition, "or");
  const andParts = _cssSplitTopLevel(condition, "and");
  if (orParts && andParts) return null;
  if (orParts) {
    const results = orParts.map(_cssSupportsConditionResult);
    return results.includes(null) ? null : results.some(Boolean);
  }
  if (andParts) {
    const results = andParts.map(_cssSupportsConditionResult);
    return results.includes(null) ? null : results.every(Boolean);
  }
  if (/^selector\(/i.test(condition) && condition.endsWith(")")) {
    return _cssSupportsSelector(condition.slice(condition.indexOf("(") + 1, -1));
  }
  const colon = condition.indexOf(":");
  if (colon < 0) {
    return /^[a-z_-][a-z0-9_-]*\([\s\S]*\)$/i.test(condition) ? false : null;
  }
  return _cssSupportsDeclaration(condition.slice(0, colon), condition.slice(colon + 1));
}

function _cssSupportsCondition(condition) {
  return _cssSupportsConditionResult(condition) === true;
}

globalThis.CSS = {
  supports(prop, value){
    try {
      if (arguments.length >= 2) {
        return _cssSupportsDeclaration(String(prop), String(value));
      }
      return _cssSupportsCondition(String(prop));
    } catch (e) { return false; }
  },
  escape(s){ return s; }
};

globalThis.HTMLElement = Element;
globalThis.HTMLDivElement = Element;
globalThis.HTMLSpanElement = Element;
globalThis.HTMLParagraphElement = Element;
globalThis.HTMLAnchorElement = Element;
globalThis.HTMLImageElement = HTMLImageElement;
globalThis.HTMLInputElement = Element;
globalThis.HTMLButtonElement = Element;
globalThis.HTMLFormElement = class HTMLFormElement extends Element {
  get elements() { return HTMLCollection._from(this.querySelectorAll("input, select, textarea, button, fieldset, output, object")); }
  get length() { return this.elements.length; }
  // Inherit submit() from Element.prototype: it dispatches the cancelable
  // 'submit' event and (if not prevented) builds form data and navigates.
  reset() { for (const f of this.elements) { if ('value' in f) f.value = ''; } }
};
globalThis.HTMLSelectElement = Element;
globalThis.HTMLTextAreaElement = Element;
globalThis.HTMLLabelElement = Element;
globalThis.HTMLTableElement = Element;
globalThis.HTMLIFrameElement = Element;
globalThis.HTMLCanvasElement = Element;
// HTMLVideoElement and HTMLAudioElement are defined above with canPlayType support.
globalThis.HTMLScriptElement = Element;
globalThis.HTMLStyleElement = Element;
globalThis.HTMLLinkElement = Element;
globalThis.HTMLMetaElement = Element;
globalThis.HTMLHeadElement = Element;
globalThis.HTMLBodyElement = Element;
globalThis.HTMLHtmlElement = Element;
globalThis.HTMLBRElement = Element;
globalThis.HTMLHRElement = Element;
globalThis.HTMLUListElement = Element;
globalThis.HTMLOListElement = Element;
globalThis.HTMLLIElement = Element;
globalThis.HTMLPreElement = Element;
globalThis.HTMLHeadingElement = Element;
globalThis.HTMLTemplateElement = Element;
globalThis.HTMLSlotElement = Element;
globalThis.HTMLOptionElement = Element;
globalThis.HTMLDataListElement = Element;
globalThis.HTMLFieldSetElement = Element;
globalThis.HTMLLegendElement = Element;
globalThis.HTMLProgressElement = Element;
globalThis.HTMLDetailsElement = Element;
globalThis.HTMLDialogElement = Element;
// SVGAnimatedString backs the className and href reflections on SVG elements.
// baseVal and animVal both read the live attribute (no SMIL animation), and
// baseVal is writable. Used by the SVG-aware get className()/get href() above.
function SVGAnimatedString(el, attr, fallbackAttr) {
  this._el = el;
  this._attr = attr;
  this._fallback = fallbackAttr || null;
}
SVGAnimatedString.prototype._read = function() {
  let v = this._el.getAttribute(this._attr);
  if (v === null && this._fallback) v = this._el.getAttribute(this._fallback);
  return v == null ? '' : v;
};
Object.defineProperty(SVGAnimatedString.prototype, 'baseVal', {
  get() { return this._read(); },
  set(v) { this._el.setAttribute(this._attr, String(v)); },
  configurable: true, enumerable: true,
});
Object.defineProperty(SVGAnimatedString.prototype, 'animVal', {
  get() { return this._read(); },
  configurable: true, enumerable: true,
});
Object.defineProperty(SVGAnimatedString.prototype, Symbol.toStringTag, { value: 'SVGAnimatedString', configurable: true });
_markNative(SVGAnimatedString);

class SVGElement extends Element {}
class SVGGraphicsElement extends SVGElement {}
class SVGGeometryElement extends SVGGraphicsElement {}
class SVGPathElement extends SVGGeometryElement {}
class SVGSVGElement extends SVGGraphicsElement {}
globalThis.SVGElement = SVGElement;
globalThis.SVGGraphicsElement = SVGGraphicsElement;
globalThis.SVGGeometryElement = SVGGeometryElement;
globalThis.SVGPathElement = SVGPathElement;
globalThis.SVGSVGElement = SVGSVGElement;
globalThis.CharacterData = CharacterData;
globalThis.Text = Text;
globalThis.Comment = Comment;

globalThis.CDATASection = CDATASection;
globalThis.ProcessingInstruction = ProcessingInstruction;
// True when the document was loaded from an XML/XHTML source. Obscura has no
// native XML tree, so this is inferred from contentType (derived from the URL).
function _isXMLDocument(doc) {
  const ct = (doc && doc.contentType) || "text/html";
  return ct !== "text/html";
}
// XML Name production, sufficient for createProcessingInstruction targets.
const _piNameStart = "A-Za-z_:\\u00C0-\\u00D6\\u00D8-\\u00F6\\u00F8-\\u02FF\\u0370-\\u037D\\u037F-\\u1FFF\\u200C-\\u200D\\u2070-\\u218F\\u2C00-\\u2FEF\\u3001-\\uD7FF\\uF900-\\uFDCF\\uFDF0-\\uFFFD";
const _piNameChar = _piNameStart + "0-9.\\u00B7\\u0300-\\u036F\\u203F-\\u2040\\-";
const _piNameRe = new RegExp("^[" + _piNameStart + "][" + _piNameChar + "]*$");
function _isValidPITarget(target) {
  return typeof target === "string" && target.length > 0 && _piNameRe.test(target);
}
globalThis.DocumentFragment = DocumentFragment;
globalThis.DocumentType = DocumentType;
globalThis.Node = Node;
globalThis.Element = Element;
globalThis.Document = Document;
// CSSStyleDeclaration is the type of element.style and getComputedStyle(); it is
// pre-declared non-enumerable in _preHideInternals, but unlike the other WebIDL
// interfaces it had no value assignment, leaving `window.CSSStyleDeclaration`
// undefined (so `el.style instanceof CSSStyleDeclaration` threw). Assigning here
// only fills the value; the property stays enumerable:false, matching Chrome.
globalThis.CSSStyleDeclaration = CSSStyleDeclaration;
globalThis.Animation = Animation;
globalThis.KeyframeEffect = KeyframeEffect;
globalThis.DocumentTimeline = DocumentTimeline;
globalThis.XPathResult = globalThis.XPathResult || class XPathResult {};
Object.assign(globalThis.XPathResult, {
  ANY_TYPE: 0,
  NUMBER_TYPE: 1,
  STRING_TYPE: 2,
  BOOLEAN_TYPE: 3,
  UNORDERED_NODE_ITERATOR_TYPE: 4,
  ORDERED_NODE_ITERATOR_TYPE: 5,
  UNORDERED_NODE_SNAPSHOT_TYPE: 6,
  ORDERED_NODE_SNAPSHOT_TYPE: 7,
  ANY_UNORDERED_NODE_TYPE: 8,
  FIRST_ORDERED_NODE_TYPE: 9,
});
// XMLDocument is a subclass of Document (DOMParser of an XML type and
// implementation.createDocument produce one). The interface must exist globally.
if (typeof XMLDocument === "undefined") globalThis.XMLDocument = class XMLDocument extends Document {};
// ParentNode mixin: Document and DocumentFragment are ParentNodes too, so they
// share Element's append / prepend / replaceChildren.
for (const _proto of [Document.prototype, DocumentFragment.prototype]) {
  _proto.append = Element.prototype.append;
  _proto.prepend = Element.prototype.prepend;
  _proto.replaceChildren = Element.prototype.replaceChildren;
}
globalThis.EventTarget = Node;
globalThis.HTMLCollection = class HTMLCollection extends Array {
  item(i) {
    i = i >>> 0;
    return this[i] != null ? this[i] : null;
  }
  namedItem(name) {
    if (name === undefined || name === null || name === "") return null;
    name = String(name);
    for (let i = 0; i < this.length; i++) {
      const el = this[i];
      if (!el) continue;
      // id always contributes; name only for HTML elements in HTML documents.
      if (el.id === name) return el;
      if (_isHTMLEl(el) && typeof el.getAttribute === "function" && el.getAttribute("name") === name) return el;
    }
    return null;
  }
  // Factory: build an HTMLCollection from an array of elements. Named access
  // (collection[name]) is served lazily by a Proxy so there is NO per-element
  // work at build time (eager defineProperty per id was an O(n) build cost that
  // made querySelectorAll on large result sets ~26x slower). The Proxy only
  // resolves a name when an unknown string key is actually read.
  static _from(arr) {
    const c = new HTMLCollection();
    if (arr) for (let i = 0; i < arr.length; i++) { if (arr[i]) c[c.length] = arr[i]; }
    return new Proxy(c, _htmlCollectionProxy);
  }
};
_markNative(HTMLCollection.prototype.item);
_markNative(HTMLCollection.prototype.namedItem);
// Shared (allocated once) Proxy traps for HTMLCollection named access. Indices,
// length, and inherited methods resolve normally via Reflect; only an unknown
// non-numeric string key falls back to namedItem(), so item/namedItem and the
// Array methods are never shadowed and id="namedItem" cannot recurse.
const _htmlCollectionProxy = {
  get(t, k, r) {
    const v = Reflect.get(t, k, r);
    if (v !== undefined || typeof k !== "string") return v;
    return t.namedItem ? (t.namedItem(k) || undefined) : undefined;
  },
  has(t, k) {
    if (Reflect.has(t, k)) return true;
    return typeof k === "string" && !!(t.namedItem && t.namedItem(k));
  },
};
// True for elements in the HTML namespace (the only ones whose name attribute
// contributes to an HTMLCollection's supported property names).
function _isHTMLEl(el) {
  return !!el && (el.namespaceURI === undefined || el.namespaceURI === "http://www.w3.org/1999/xhtml");
}
// Build a NodeList (no named access, per spec) for querySelectorAll and
// childNodes. Kept light on purpose: querySelectorAll is the hottest query API.
function _nodeList(els) {
  const nl = new NodeList();
  for (let i = 0; i < els.length; i++) nl[i] = els[i];
  nl.length = els.length;
  return nl;
}

// Window named access. HTML exposes every element id, plus the name of a
// small legacy set of HTML elements, as properties of the WindowProxy. V8's
// global object cannot be replaced with a WindowProxy after snapshot startup,
// so install lazy accessors for the supported names present in this document.
// The accessor resolves against the live tree: one match returns that element
// (or an iframe's Window), while duplicates return a live-shaped
// HTMLCollection in tree order.
const _windowNamedPropertyNames = new Set();
const _windowNamedNameTags = new Set(["embed", "form", "iframe", "img", "object"]);

function _windowNameEligibleElement(element) {
  return !!element
    && element.namespaceURI === "http://www.w3.org/1999/xhtml"
    && _windowNamedNameTags.has(element.localName);
}

function _windowNamedSupportedNames(element) {
  const names = [];
  if (!element || element.nodeType !== 1) return names;
  const id = element.getAttribute("id");
  if (id) names.push(id);
  if (_windowNameEligibleElement(element)) {
    const name = element.getAttribute("name");
    if (name && name !== id) names.push(name);
  }
  return names;
}

function _windowNamedCandidates(name) {
  const doc = globalThis.document;
  if (!doc || !name) return [];
  const elements = doc.querySelectorAll(
    "[id],embed[name],form[name],iframe[name],img[name],object[name]"
  );
  const matches = [];
  for (let i = 0; i < elements.length; i++) {
    const element = elements[i];
    if (element.getAttribute("id") === name
        || (_windowNameEligibleElement(element)
          && element.getAttribute("name") === name)) {
      matches.push(element);
    }
  }
  return matches;
}

function _windowNamedValue(name) {
  const matches = _windowNamedCandidates(name);
  if (matches.length === 0) return undefined;
  if (matches.length > 1) return HTMLCollection._from(matches);
  const element = matches[0];
  return element.localName === "iframe" && element.contentWindow
    ? element.contentWindow
    : element;
}

function _ensureWindowNamedProperty(name) {
  name = String(name || "");
  if (!name || _windowNamedPropertyNames.has(name)) return;
  // Existing own Window properties win over named elements.
  if (Object.prototype.hasOwnProperty.call(globalThis, name)) return;
  try {
    Object.defineProperty(globalThis, name, {
      get() { return _windowNamedValue(name); },
      configurable: true,
      enumerable: true,
    });
    _windowNamedPropertyNames.add(name);
  } catch (_error) {}
}

function _reconcileWindowNamedProperty(name) {
  if (!_windowNamedPropertyNames.has(name)) return;
  if (_windowNamedCandidates(name).length !== 0) return;
  try { delete globalThis[name]; } catch (_error) {}
  _windowNamedPropertyNames.delete(name);
}

function _windowNamedNamesInTree(root) {
  const names = new Set();
  if (!root) return names;
  if (root.nodeType === 1) {
    for (const name of _windowNamedSupportedNames(root)) names.add(name);
  }
  if (typeof root.querySelectorAll === "function") {
    const elements = root.querySelectorAll(
      "[id],embed[name],form[name],iframe[name],img[name],object[name]"
    );
    for (let i = 0; i < elements.length; i++) {
      for (const name of _windowNamedSupportedNames(elements[i])) names.add(name);
    }
  }
  return names;
}

function _registerWindowNamedTree(root) {
  // Window named access only considers the document tree. Detached nodes and
  // attached shadow trees must not manufacture own Window properties. Check
  // connectivity first: getRootNode() walks every ancestor, which made the
  // common framework pattern of building a deep detached subtree quadratic.
  if (!root || !root.isConnected || root.getRootNode() !== globalThis.document) return;
  const names = _windowNamedNamesInTree(root);
  for (const name of names) _ensureWindowNamedProperty(name);
}

function _reconcileWindowNamedProperties(names) {
  if (!names || names.size === 0) return;
  const doc = globalThis.document;
  if (!doc) return;
  const present = new Set();
  const elements = doc.querySelectorAll(
    "[id],embed[name],form[name],iframe[name],img[name],object[name]"
  );
  for (let i = 0; i < elements.length; i++) {
    for (const name of _windowNamedSupportedNames(elements[i])) {
      if (names.has(name)) present.add(name);
    }
  }
  for (const name of names) {
    if (_windowNamedPropertyNames.has(name) && !present.has(name)) {
      try { delete globalThis[name]; } catch (_error) {}
      _windowNamedPropertyNames.delete(name);
    }
  }
}

globalThis.DOMTokenList = DOMTokenList;
// NodeList is its own type, not an Array subclass: in a real browser
// Array.isArray(nodeList) is false and Object.prototype.toString reports
// "[object NodeList]". Fingerprinting and feature-detection scripts check both.
// It keeps the array-like surface scripts actually use: indexed access, length,
// item(), forEach(), entries/keys/values, and iteration (so spread and for..of
// work).
globalThis.NodeList = class NodeList {
  constructor() { this.length = 0; }
  item(i) { i = i >>> 0; return this[i] != null ? this[i] : null; }
  forEach(cb, thisArg) {
    for (let i = 0; i < this.length; i++) cb.call(thisArg, this[i], i, this);
  }
  *[Symbol.iterator]() { for (let i = 0; i < this.length; i++) yield this[i]; }
  *entries() { for (let i = 0; i < this.length; i++) yield [i, this[i]]; }
  *keys() { for (let i = 0; i < this.length; i++) yield i; }
  *values() { for (let i = 0; i < this.length; i++) yield this[i]; }
  get [Symbol.toStringTag]() { return 'NodeList'; }
};
_markNative(NodeList);
_markNative(NodeList.prototype.item);
_markNative(NodeList.prototype.forEach);
// Live Range over the real DOM tree. dom/ranges/* tests are pure boundary-point
// algorithms (no layout, no editing engine), so a property-storing Range with
// correct tree-order comparison passes them. Mutating ops (extract/delete/
// insert/surround) are kept minimal: they do not throw, but do not rewrite the
// tree (that is the editing mega-bucket, out of scope).
function _rngNodeLength(n) {
  const t = n.nodeType;
  if (t === 3 || t === 4 || t === 8 || t === 7) return (n.data || n.nodeValue || "").length;
  return n.childNodes.length;
}
// Index among siblings, computed in Rust (one op) instead of serializing the
// whole childNodes list per call: the Range matrices call this heavily.
function _rngNodeIndex(n) {
  if (!n.parentNode) return 0;
  return +_dom("node_index", n._nid);
}
function _rngSame(a, b) { return a === b || (!!a && !!b && a._nid === b._nid); }
// Root nid in one op (callers only read ._nid), instead of an O(depth) walk.
function _rngRoot(n) { return { _nid: +_dom("node_root", n._nid) }; }
function _rngAncestors(n) { const a = []; let c = n; while (c) { a.push(c); c = c.parentNode; } return a; }
// document (preorder) tree order: -1 if a precedes b, 1 if a follows b, 0 same.
// Computed in Rust (one op) rather than walking ancestor chains over per-step
// DOM ops, which made the large dom/ranges matrices time out.
function _rngOrder(a, b) {
  if (_rngSame(a, b)) return 0;
  return +_dom("compare_order", a._nid, b._nid) || 0;
}
// Position of (nA,oA) relative to (nB,oB): -1 before, 0 equal, 1 after.
function _rngCmp(nA, oA, nB, oB) {
  if (_rngSame(nA, nB)) return oA < oB ? -1 : (oA > oB ? 1 : 0);
  if (_rngOrder(nA, nB) > 0) return -_rngCmp(nB, oB, nA, oA);
  if (nA.contains && nA.contains(nB)) { // nA is a strict ancestor of nB
    let child = nB;
    while (child && child.parentNode && child.parentNode._nid !== nA._nid) child = child.parentNode;
    if (child && child.parentNode && child.parentNode._nid === nA._nid && _rngNodeIndex(child) < oA) return 1;
    return -1;
  }
  return -1;
}
function _rngCheckOffset(n, o) {
  if (n && n.nodeType === 10) throw new DOMException("Range boundary cannot be a DocumentType", "InvalidNodeTypeError");
  if (o < 0 || o > _rngNodeLength(n)) throw new DOMException("Range offset out of bounds", "IndexSizeError");
}
globalThis.Range = class Range {
  constructor() {
    const d = globalThis.document || null;
    this._sc = d; this._so = 0; this._ec = d; this._eo = 0;
  }
  get startContainer() { return this._sc; }
  get startOffset() { return this._so; }
  get endContainer() { return this._ec; }
  get endOffset() { return this._eo; }
  get collapsed() { return _rngSame(this._sc, this._ec) && this._so === this._eo; }
  get commonAncestorContainer() {
    if (!this._sc || !this._ec) return null;
    const setA = new Set(_rngAncestors(this._sc).map(n => n._nid));
    let c = this._ec;
    while (c) { if (setA.has(c._nid)) return c; c = c.parentNode; }
    return null;
  }
  setStart(n, o) { _rngCheckOffset(n, o); this._sc = n; this._so = o; if (_rngRoot(n)._nid !== _rngRoot(this._ec)._nid || _rngCmp(this._sc, this._so, this._ec, this._eo) > 0) { this._ec = n; this._eo = o; } }
  setEnd(n, o) { _rngCheckOffset(n, o); this._ec = n; this._eo = o; if (_rngRoot(n)._nid !== _rngRoot(this._sc)._nid || _rngCmp(this._sc, this._so, this._ec, this._eo) > 0) { this._sc = n; this._so = o; } }
  setStartBefore(n) { const p = n.parentNode; if (!p) throw new DOMException("node has no parent", "InvalidNodeTypeError"); this.setStart(p, _rngNodeIndex(n)); }
  setStartAfter(n) { const p = n.parentNode; if (!p) throw new DOMException("node has no parent", "InvalidNodeTypeError"); this.setStart(p, _rngNodeIndex(n) + 1); }
  setEndBefore(n) { const p = n.parentNode; if (!p) throw new DOMException("node has no parent", "InvalidNodeTypeError"); this.setEnd(p, _rngNodeIndex(n)); }
  setEndAfter(n) { const p = n.parentNode; if (!p) throw new DOMException("node has no parent", "InvalidNodeTypeError"); this.setEnd(p, _rngNodeIndex(n) + 1); }
  collapse(toStart) { if (toStart) { this._ec = this._sc; this._eo = this._so; } else { this._sc = this._ec; this._so = this._eo; } }
  selectNode(n) { const p = n.parentNode; if (!p) throw new DOMException("node has no parent", "InvalidNodeTypeError"); const i = _rngNodeIndex(n); this._sc = p; this._so = i; this._ec = p; this._eo = i + 1; }
  selectNodeContents(n) { if (n && n.nodeType === 10) throw new DOMException("cannot select a DocumentType", "InvalidNodeTypeError"); const len = _rngNodeLength(n); this._sc = n; this._so = 0; this._ec = n; this._eo = len; }
  comparePoint(n, o) {
    o = o >>> 0; // offset is a WebIDL unsigned long: -1 -> 4294967295 -> IndexSizeError
    if (_rngRoot(n)._nid !== _rngRoot(this._sc)._nid) throw new DOMException("nodes are in different trees", "WrongDocumentError");
    if (n.nodeType === 10) throw new DOMException("node is a DocumentType", "InvalidNodeTypeError");
    if (o > _rngNodeLength(n)) throw new DOMException("offset out of bounds", "IndexSizeError");
    if (_rngCmp(n, o, this._sc, this._so) < 0) return -1;
    if (_rngCmp(n, o, this._ec, this._eo) > 0) return 1;
    return 0;
  }
  isPointInRange(n, o) {
    o = o >>> 0;
    if (!this._sc || _rngRoot(n)._nid !== _rngRoot(this._sc)._nid) return false;
    if (n.nodeType === 10) throw new DOMException("node is a DocumentType", "InvalidNodeTypeError");
    if (o > _rngNodeLength(n)) throw new DOMException("offset out of bounds", "IndexSizeError");
    return _rngCmp(n, o, this._sc, this._so) >= 0 && _rngCmp(n, o, this._ec, this._eo) <= 0;
  }
  compareBoundaryPoints(how, other) {
    // `how` is a WebIDL `unsigned short`: ToUint16-convert before validating,
    // so NaN/Infinity become 0 (START_TO_START) rather than throwing.
    let h = Math.trunc(Number(how));
    if (!Number.isFinite(h)) h = 0;
    h = ((h % 65536) + 65536) % 65536;
    let a, b;
    switch (h) {
      case 0: a = [this._sc, this._so]; b = [other._sc, other._so]; break; // START_TO_START
      case 1: a = [this._ec, this._eo]; b = [other._sc, other._so]; break; // START_TO_END
      case 2: a = [this._ec, this._eo]; b = [other._ec, other._eo]; break; // END_TO_END
      case 3: a = [this._sc, this._so]; b = [other._ec, other._eo]; break; // END_TO_START
      default: throw new DOMException("invalid comparison type", "NotSupportedError");
    }
    // Different roots -> WrongDocumentError. Guard so a null/foreign container
    // raises that DOMException rather than a raw TypeError from _rngRoot.
    let differ;
    try { differ = _rngRoot(a[0])._nid !== _rngRoot(b[0])._nid; }
    catch (e) { differ = true; }
    if (differ) throw new DOMException("The two Ranges are not in the same tree.", "WrongDocumentError");
    return _rngCmp(a[0], a[1], b[0], b[1]);
  }
  intersectsNode(n) {
    if (_rngRoot(n)._nid !== _rngRoot(this._sc)._nid) return false;
    const p = n.parentNode;
    if (!p) return true;
    const o = _rngNodeIndex(n);
    return _rngCmp(p, o, this._ec, this._eo) < 0 && _rngCmp(p, o + 1, this._sc, this._so) > 0;
  }
  cloneRange() { const r = new Range(); r._sc = this._sc; r._so = this._so; r._ec = this._ec; r._eo = this._eo; return r; }
  createContextualFragment(html) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'createContextualFragment' on 'Range': 1 argument required, but only 0 present.");
    const node = this._sc;
    const ownerDoc = (node && node.ownerDocument) || globalThis.document;
    const frag = ownerDoc.createDocumentFragment();
    let context = node;
    if (context && context.nodeType !== 1) context = context.parentElement;
    if (context && context.localName === 'html') context = null;
    _dom(
      "set_fragment_html_executable",
      frag._nid,
      _fragmentContextPayload(context || 'body', html),
    );
    return frag;
  }
  toString() {
    const sc = this._sc, ec = this._ec;
    if (!sc) return "";
    if (_rngSame(sc, ec) && (sc.nodeType === 3 || sc.nodeType === 4)) return (sc.data || "").slice(this._so, this._eo);
    let s = "";
    if (sc.nodeType === 3 || sc.nodeType === 4) s += (sc.data || "").slice(this._so);
    const cac = this.commonAncestorContainer;
    if (cac) {
      const walk = (node) => {
        if (node.nodeType === 3 || node.nodeType === 4) {
          if (!_rngSame(node, sc) && !_rngSame(node, ec) &&
              _rngCmp(node, 0, this._sc, this._so) >= 0 && _rngCmp(node, _rngNodeLength(node), this._ec, this._eo) <= 0) {
            s += (node.data || "");
          }
        }
        const kids = node.childNodes;
        for (let i = 0; i < kids.length; i++) if (kids[i]) walk(kids[i]);
      };
      walk(cac);
    }
    if (!_rngSame(sc, ec) && (ec.nodeType === 3 || ec.nodeType === 4)) s += (ec.data || "").slice(0, this._eo);
    return s;
  }
  cloneContents() { return (globalThis.document || document).createDocumentFragment(); }
  extractContents() { return (globalThis.document || document).createDocumentFragment(); }
  deleteContents() {}
  insertNode(node) { if (node && this._sc && this._sc.insertBefore) { const kids = this._sc.childNodes; this._sc.insertBefore(node, kids[this._so] || null); } }
  surroundContents(node) { this.insertNode(node); }
  detach() {}
  getBoundingClientRect() {
    if (this.collapsed) return new DOMRect();
    let cac = this.commonAncestorContainer;
    while (cac && cac.nodeType !== 1 && cac.nodeType !== 9) cac = cac.parentNode;
    if (cac && cac.getBoundingClientRect) {
      const r = cac.getBoundingClientRect();
      return new DOMRect(r.x, r.y, r.width, r.height);
    }
    return new DOMRect();
  }
  getClientRects() {
    if (this.collapsed) return new DOMRectList([]);
    return new DOMRectList([this.getBoundingClientRect()]);
  }
  static get START_TO_START() { return 0; }
  static get START_TO_END() { return 1; }
  static get END_TO_END() { return 2; }
  static get END_TO_START() { return 3; }
};
Object.assign(globalThis.Range.prototype, { START_TO_START: 0, START_TO_END: 1, END_TO_END: 2, END_TO_START: 3 });
globalThis.StaticRange = class StaticRange {
  constructor(init) {
    if (!init || init.startContainer == null || init.endContainer == null)
      throw new TypeError("Failed to construct 'StaticRange': required members are undefined");
    const sc = init.startContainer, ec = init.endContainer;
    if (sc.nodeType === 10 || ec.nodeType === 10 || sc.nodeType === 7 || ec.nodeType === 7)
      throw new DOMException("StaticRange endpoints cannot be DocumentType or ProcessingInstruction", "InvalidNodeTypeError");
    this._sc = sc; this._so = init.startOffset >>> 0; this._ec = ec; this._eo = init.endOffset >>> 0;
  }
  get startContainer() { return this._sc; }
  get startOffset() { return this._so; }
  get endContainer() { return this._ec; }
  get endOffset() { return this._eo; }
  get collapsed() { return _rngSame(this._sc, this._ec) && this._so === this._eo; }
};
// Live Selection over the real Range: at most one range + a direction, one
// instance per document. Everything except modify() (needs visual line/word
// layout) is layout-free, built on the Range boundary-point helpers above.
globalThis.Selection = class Selection {
  constructor(doc) { this._doc = doc; this._range = null; this._direction = 'none'; }
  _setRange(r, dir) { this._range = r; this._direction = dir; }
  _inDoc(node) { return !!(node && this._doc && this._doc.contains && this._doc.contains(node)); }
  get rangeCount() { return this._range ? 1 : 0; }
  get isCollapsed() { return !this._range || this._range.collapsed; }
  get type() { return !this._range ? 'None' : (this._range.collapsed ? 'Caret' : 'Range'); }
  get _anchor() { const r = this._range; if (!r) return null; return this._direction === 'backwards' ? [r.endContainer, r.endOffset] : [r.startContainer, r.startOffset]; }
  get _focus() { const r = this._range; if (!r) return null; return this._direction === 'backwards' ? [r.startContainer, r.startOffset] : [r.endContainer, r.endOffset]; }
  get anchorNode() { return this._anchor ? this._anchor[0] : null; }
  get anchorOffset() { return this._anchor ? this._anchor[1] : 0; }
  get focusNode() { return this._focus ? this._focus[0] : null; }
  get focusOffset() { return this._focus ? this._focus[1] : 0; }
  getRangeAt(i) { i = +i; if (!this._range || i < 0 || i > 0) throw new DOMException('The index provided is out of range.', 'IndexSizeError'); return this._range; }
  addRange(range) { if (this._range) return; if (!(range instanceof Range)) return; if (!this._inDoc(range.startContainer) || !this._inDoc(range.endContainer)) return; this._setRange(range, 'forwards'); }
  removeRange(range) { if (!(range instanceof Range)) throw new TypeError("Failed to execute 'removeRange' on 'Selection': parameter 1 is not a Range."); if (this._range === range) this._setRange(null, 'none'); else throw new DOMException('The range was not found.', 'NotFoundError'); }
  removeAllRanges() { this._setRange(null, 'none'); }
  empty() { this.removeAllRanges(); }
  collapse(node, offset) { if (node == null) { this.removeAllRanges(); return; } offset = offset >>> 0; _rngCheckOffset(node, offset); if (!this._inDoc(node)) return; const r = new Range(); r.setStart(node, offset); r.setEnd(node, offset); this._setRange(r, 'forwards'); }
  setPosition(node, offset) { this.collapse(node, offset); }
  collapseToStart() { if (!this._range) throw new DOMException('There is no selection to collapse.', 'InvalidStateError'); const r = new Range(); r.setStart(this._range.startContainer, this._range.startOffset); r.setEnd(this._range.startContainer, this._range.startOffset); this._setRange(r, 'forwards'); }
  collapseToEnd() { if (!this._range) throw new DOMException('There is no selection to collapse.', 'InvalidStateError'); const r = new Range(); r.setStart(this._range.endContainer, this._range.endOffset); r.setEnd(this._range.endContainer, this._range.endOffset); this._setRange(r, 'forwards'); }
  extend(node, offset) { if (!this._range) throw new DOMException('There is no selection to extend.', 'InvalidStateError'); if (!this._inDoc(node)) return; offset = offset >>> 0; _rngCheckOffset(node, offset); const a = this._anchor; const r = new Range(); if (_rngRoot(node)._nid !== _rngRoot(a[0])._nid) { r.setStart(node, offset); r.setEnd(node, offset); this._setRange(r, 'forwards'); return; } if (_rngCmp(a[0], a[1], node, offset) <= 0) { r.setStart(a[0], a[1]); r.setEnd(node, offset); this._setRange(r, 'forwards'); } else { r.setStart(node, offset); r.setEnd(a[0], a[1]); this._setRange(r, 'backwards'); } }
  setBaseAndExtent(aN, aO, fN, fO) { if (arguments.length < 4) throw new TypeError("Failed to execute 'setBaseAndExtent' on 'Selection': 4 arguments required."); if (aN == null || fN == null) throw new TypeError("Failed to execute 'setBaseAndExtent' on 'Selection': nodes must not be null."); aO = +aO; fO = +fO; if (aO < 0 || aO > _rngNodeLength(aN)) throw new DOMException('anchor offset out of range', 'IndexSizeError'); if (fO < 0 || fO > _rngNodeLength(fN)) throw new DOMException('focus offset out of range', 'IndexSizeError'); if (!this._inDoc(aN) || !this._inDoc(fN)) { this.removeAllRanges(); return; } const r = new Range(); if (_rngCmp(aN, aO, fN, fO) <= 0) { r.setStart(aN, aO); r.setEnd(fN, fO); this._setRange(r, 'forwards'); } else { r.setStart(fN, fO); r.setEnd(aN, aO); this._setRange(r, 'backwards'); } }
  selectAllChildren(node) { if (node && node.nodeType === 10) throw new DOMException('cannot selectAllChildren of a DocumentType', 'InvalidNodeTypeError'); if (!this._inDoc(node)) return; const len = _rngNodeLength(node); const r = new Range(); r.setStart(node, 0); r.setEnd(node, len); this._setRange(r, 'forwards'); }
  containsNode(node, allowPartial) { const r = this._range; if (!r || !node) return false; if (_rngRoot(node)._nid !== _rngRoot(r.startContainer)._nid) return false; const len = _rngNodeLength(node); if (allowPartial) return _rngCmp(node, len, r.startContainer, r.startOffset) > 0 && _rngCmp(node, 0, r.endContainer, r.endOffset) < 0; return _rngCmp(node, 0, r.startContainer, r.startOffset) >= 0 && _rngCmp(node, len, r.endContainer, r.endOffset) <= 0; }
  deleteFromDocument() { if (this._range) this._range.deleteContents(); }
  toString() { return this._range ? this._range.toString() : ''; }
  modify() {}
};
_markNative(globalThis.Selection);

[
  navigator.getBattery, navigator.getGamepads, navigator.sendBeacon,
  navigator.javaEnabled, navigator.geolocation?.getCurrentPosition,
  navigator.geolocation?.watchPosition,
  navigator.serviceWorker?.register,
  navigator.permissions?.query, navigator.credentials?.get,
  navigator.storage?.estimate, navigator.storage?.persist, navigator.storage?.persisted,
  globalThis.fetch, globalThis.matchMedia, globalThis.getComputedStyle,
  globalThis.getSelection, globalThis.requestAnimationFrame,
  globalThis.cancelAnimationFrame, globalThis.setTimeout, globalThis.clearTimeout,
  globalThis.setInterval, globalThis.clearInterval, globalThis.queueMicrotask,
  globalThis.structuredClone, globalThis.reportError,
  globalThis.btoa, globalThis.atob,
  console.log, console.warn, console.error, console.info, console.debug,
  console.dir, console.assert,
  Element.prototype.getAttribute, Element.prototype.setAttribute,
  Element.prototype.removeAttribute, Element.prototype.hasAttribute,
  Element.prototype.querySelector, Element.prototype.querySelectorAll,
  Element.prototype.getElementsByTagName, Element.prototype.getElementsByClassName,
  Element.prototype.matches, Element.prototype.closest,
  Element.prototype.getBoundingClientRect, Element.prototype.getClientRects,
  Element.prototype.checkVisibility,
  Element.prototype.addEventListener, Element.prototype.removeEventListener,
  Element.prototype.dispatchEvent, Element.prototype.click,
  Element.prototype.focus, Element.prototype.blur,
  Element.prototype.showPopover, Element.prototype.hidePopover, Element.prototype.togglePopover,
  Element.prototype.cloneNode, Element.prototype.attachShadow,
  Element.prototype.insertAdjacentHTML, Element.prototype.insertAdjacentText,
  Element.prototype.insertAdjacentElement, Element.prototype.scrollIntoView,
  Element.prototype.scrollTo, Element.prototype.scrollBy, Element.prototype.scroll,
  Element.prototype.append, Element.prototype.prepend, Element.prototype.remove,
  Element.prototype.before, Element.prototype.after, Element.prototype.replaceWith,
  HTMLFormElement.prototype.reset,
  Element.prototype.getContext, Element.prototype.toDataURL, Element.prototype.toBlob,
  Element.prototype.getBBox,
  Node.prototype.appendChild, Node.prototype.removeChild,
  Node.prototype.replaceChild, Node.prototype.insertBefore,
  Node.prototype.contains, Node.prototype.hasChildNodes, Node.prototype.cloneNode,
  CharacterData.prototype.before, CharacterData.prototype.after,
  CharacterData.prototype.replaceWith, CharacterData.prototype.remove,
  Document.prototype.getElementById, Document.prototype.querySelector,
  Document.prototype.querySelectorAll, Document.prototype.getElementsByTagName,
  Document.prototype.createElement, Document.prototype.createElementNS,
  Document.prototype.createTextNode, Document.prototype.createComment,
  Document.prototype.createCDATASection, Document.prototype.createProcessingInstruction,
  Document.prototype.createDocumentFragment, Document.prototype.createEvent,
  Document.prototype.hasFocus,
  Storage, Storage.prototype.getItem, Storage.prototype.setItem,
  Storage.prototype.removeItem, Storage.prototype.clear, Storage.prototype.key,
  Notification, Notification.requestPermission,
  window.chrome?.csi, window.chrome?.loadTimes,
  MutationObserver, ResizeObserver, IntersectionObserver, PerformanceObserver,
  XMLSerializer, XMLSerializer.prototype.serializeToString,
].forEach(fn => { if (typeof fn === 'function') _markNative(fn); });

class _IframeDocument {
  constructor(html, url, iframeEl) {
    this._url = url;
    this._iframeEl = iframeEl;
    this.nodeType = 9;
    this.nodeName = '#document';
    this.readyState = 'complete';
    this.characterSet = 'UTF-8';
    this.contentType = 'text/html';
    this.visibilityState = 'visible';
    this.hidden = false;

    this._root = document.createElement('html');
    this._head = document.createElement('head');
    this._body = document.createElement('body');
    this._root.appendChild(this._head);
    this._root.appendChild(this._body);
    var bodyContent = html
      .replace(/^<!DOCTYPE[^>]*>/i, '')
      .replace(/<\/?html[^>]*>/gi, '')
      .replace(/<head[^>]*>[\s\S]*?<\/head>/gi, '')
      .replace(/<\/?body[^>]*>/gi, '')
      .replace(/^\s+/, ''); // trim leading whitespace (before <body> content)
    if (bodyContent) {
      this._body.innerHTML = bodyContent;
    }

    this._title = '';
    if (this._head) {
      const titleEl = this._head.querySelector('title');
      if (titleEl) this._title = titleEl.textContent;
    }
  }

  get documentElement() { return this._root; }
  get head() { return this._head; }
  get body() { return this._body; }
  get title() { return this._title; }
  set title(v) { this._title = v; }
  get URL() { return this._url; }
  get documentURI() { return this._url; }
  get location() { return this._iframeEl?.contentWindow?.location; }
  get defaultView() { return this._iframeEl?.contentWindow; }
  get ownerDocument() { return null; }
  get compatMode() { return 'CSS1Compat'; }
  get activeElement() { return this._body; }

  getElementById(id) {
    return this._root.querySelector('#' + id);
  }
  querySelector(sel) {
    return this._root.querySelector(sel);
  }
  querySelectorAll(sel) {
    return this._root.querySelectorAll(sel);
  }
  getElementsByTagName(tag) {
    return this._root.querySelectorAll(tag);
  }
  getElementsByClassName(cls) {
    return _getElementsByClassName(this._root, cls);
  }
  createElement(tag) { return document.createElement(tag); }
  createElementNS(ns, tag) { return document.createElementNS(ns, tag); }
  createTextNode(text) { return document.createTextNode(text); }
  createComment(text) { return document.createComment(text); }
  createDocumentFragment() { return document.createDocumentFragment(); }
  createEvent(type) { return document.createEvent(type); }
  createRange() { return new Range(); }
  hasFocus() { return false; }

  get cookie() { return ''; }
  set cookie(v) {}
  get implementation() { return document.implementation; }
  get styleSheets() { return []; }

  addEventListener(type, listener) {
    if (typeof listener !== 'function') return;
    if (!this._listeners) this._listeners = Object.create(null);
    const list = this._listeners[type] || (this._listeners[type] = []);
    if (!list.includes(listener)) list.push(listener);
  }
  removeEventListener(type, listener) {
    const list = this._listeners && this._listeners[type];
    if (!list) return;
    const index = list.indexOf(listener);
    if (index !== -1) list.splice(index, 1);
  }
  dispatchEvent(event) {
    const type = event && event.type;
    if (!type) return true;
    const list = this._listeners && this._listeners[type];
    if (list) {
      for (const listener of list.slice()) {
        try { listener.call(this, event); } catch (error) { console.error(error); }
      }
    }
    const handler = this['on' + type];
    if (typeof handler === 'function') {
      try { handler.call(this, event); } catch (error) { console.error(error); }
    }
    return !event.defaultPrevented;
  }

  write(html) {
    if (this._body) this._body.innerHTML += html;
  }
  writeln(html) { this.write(html + '\n'); }
  open() { if (this._body) this._body.innerHTML = ''; }
  close() {}
}

const _iframeRealmGlobalCache = new WeakMap();
let _iframeRealmGlobalNames = [];
let _iframeRealmGlobalNameSet = new Set();

function _iframeSourceIsConstructor(value) {
  try {
    Reflect.construct(Object, [], value);
    return true;
  } catch (e) {
    return false;
  }
}

function _iframeRealmFunction(target, name, source) {
  let wrapped;
  if (_iframeSourceIsConstructor(source)) {
    wrapped = function (...args) {
      if (new.target) return Reflect.construct(source, args, new.target);
      return Reflect.apply(source, this === target ? globalThis : this, args);
    };
    if (source.prototype && (typeof source.prototype === 'object' || typeof source.prototype === 'function')) {
      const prototype = Object.create(source.prototype);
      Object.defineProperty(prototype, 'constructor', {
        value: wrapped,
        writable: true,
        configurable: true,
      });
      wrapped.prototype = prototype;
    }
  } else {
    wrapped = (...args) => Reflect.apply(source, globalThis, args);
  }
  // Inherit static members such as Promise.resolve, Object.keys, and
  // Array.isArray while keeping the constructor identity realm-local.
  try { Object.setPrototypeOf(wrapped, source); } catch (e) {}
  try { Object.defineProperty(wrapped, 'name', { value: name, configurable: true }); } catch (e) {}
  try { Object.defineProperty(wrapped, 'length', { value: source.length, configurable: true }); } catch (e) {}
  return _markNative(wrapped);
}

function _iframeRealmGlobal(target, name) {
  let cache = _iframeRealmGlobalCache.get(target);
  if (!cache) {
    cache = new Map();
    _iframeRealmGlobalCache.set(target, cache);
  }
  if (cache.has(name)) return cache.get(name);

  const source = globalThis[name];
  let value = source;
  if (typeof source === 'function') {
    value = _iframeRealmFunction(target, name, source);
  } else if (source && typeof source === 'object') {
    // Namespace objects such as Math, JSON, Reflect, and Intl belong to the
    // child global too. A lightweight facade gives each iframe a stable,
    // distinct object without copying large immutable tables.
    value = Object.create(source);
  }
  cache.set(name, value);
  return value;
}

const _iframeWindowProxyHandler = {
  get(target, key, receiver) {
    if (key === 'globalThis') return receiver;
    if (Reflect.has(target, key)) return Reflect.get(target, key, receiver);
    if (typeof key === 'string' && _iframeRealmGlobalNameSet.has(key)) {
      return _iframeRealmGlobal(target, key);
    }
    return undefined;
  },
  has(target, key) {
    return key === 'globalThis'
      || Reflect.has(target, key)
      || (typeof key === 'string' && _iframeRealmGlobalNameSet.has(key));
  },
  ownKeys(target) {
    const keys = Reflect.ownKeys(target);
    const seen = new Set(keys);
    for (const name of _iframeRealmGlobalNames) {
      if (!seen.has(name)) keys.push(name);
    }
    if (!seen.has('globalThis')) keys.push('globalThis');
    return keys;
  },
  getOwnPropertyDescriptor(target, key) {
    const own = Reflect.getOwnPropertyDescriptor(target, key);
    if (own) return own;
    if (key === 'globalThis') {
      return { value: target.self, writable: true, enumerable: false, configurable: true };
    }
    if (typeof key === 'string' && _iframeRealmGlobalNameSet.has(key)) {
      return {
        value: _iframeRealmGlobal(target, key),
        writable: true,
        enumerable: false,
        configurable: true,
      };
    }
    return undefined;
  },
};

// Cross-realm messaging.
//
// A realm cannot reach another realm's context on its own, so postMessage is
// handed to the host, which delivers it into the target realm. These are
// declared rather than assigned by the host so the snapshot-time hide list
// picks them up; a global added later would stay enumerable on `window`.
globalThis.__obscura_frameId = 0;        // 0 is the page's own realm
globalThis.__obscura_parentFrameId = 0;
globalThis.__obscura_frameWindows = Object.create(null); // frame id -> its window
// frame id -> that frame's real window and document, filled by the host.
// Declared here rather than created by the host at runtime: the hide list is
// computed from this global at snapshot time, so a property the host adds later
// would stay enumerable on `window` and be visible to any script that walks it.
globalThis.__obscura_frameObjects = Object.create(null);
// frame id -> the iframe element that owns it. Holds the element until the
// frame is forgotten, which is bounded by the host's live-frame cap.
globalThis.__obscura_frameElements = Object.create(null);

// The frames of this realm whose element is still in the document.
//
// Liveness is asked of the element, not of a document query: an iframe inside
// a shadow root is absent from `document.querySelectorAll('iframe')` — the
// shape a challenge widget uses — while `isConnected` reports it correctly.
// Treating it as gone would tear down a frame that is still in the page.
globalThis.__obscura_liveFrameIds = function () {
  const live = [];
  for (const id in globalThis.__obscura_frameElements) {
    const element = globalThis.__obscura_frameElements[id];
    if (element && element.isConnected) live.push(id >>> 0);
  }
  return live;
};

// Drop everything this realm holds for a frame the host has discarded. One
// place, so a registry added later cannot be missed by the discard path: any
// surviving reference keeps the frame's context and DOM tree alive.
globalThis.__obscura_forgetFrame = function (frameId) {
  delete globalThis.__obscura_frameElements[frameId];
  delete globalThis.__obscura_frameObjects[frameId];
  delete globalThis.__obscura_frameWindows[frameId];
};

function _realmOrigin() {
  try { return new URL(_domParse('document_url')).origin; } catch (_) { return 'null'; }
}

function _sendRealmMessage(targetFrameId, data) {
  let json;
  // Structured clone cannot cross realms here. JSON carries what postMessage is
  // actually used for; anything else throws the same DataCloneError a browser
  // throws for an unclonable value, rather than arriving silently as null.
  try {
    json = JSON.stringify({ v: data === undefined ? null : data });
  } catch (_) {
    throw new DOMException('The object could not be cloned.', 'DataCloneError');
  }
  if (json === undefined) json = '{"v":null}';
  Deno.core.ops.op_post_frame_message(
    targetFrameId >>> 0, globalThis.__obscura_frameId >>> 0, _realmOrigin(), json);
}

// The frame's own window and document, when this page is allowed to touch
// them. Same isolate, so these are the frame's real objects rather than a copy:
// `contentWindow.someGlobal` reads the frame's global and `contentDocument` is
// the document the frame's own scripts mutated.
//
// A free function, not a getter on Element.prototype: every own property of a
// public interface is visible to anything that walks it, and real Chrome has no
// such member.
function _frameObjectsFor(element) {
  const frameId = element._frameId;
  if (!frameId) return null;
  const entry = globalThis.__obscura_frameObjects[frameId];
  return entry || null;
}

// The window object this realm uses to stand for frame `frameId`, built once
// and reused so `event.source === iframe.contentWindow` holds.
//
// Once the host has published the frame's real global, that is the object,
// wrapped only to keep `postMessage` meaning "send *into* the frame from
// here". Calling the frame's own postMessage would make the frame both sender
// and receiver, losing the sender's origin and source.
function _frameWindowFor(frameId) {
  if (!frameId) return null;
  const real = globalThis.__obscura_frameObjects?.[frameId]?.window;
  const existing = globalThis.__obscura_frameWindows[frameId];
  if (!real) return existing || null;
  if (existing && existing.__obscura_wrapsRealm) return existing;

  const post = _markNative(function (data, _targetOrigin, _transfer) {
    _sendRealmMessage(frameId, data);
  });
  const win = new Proxy(real, {
    get(target, prop) {
      if (prop === 'postMessage') return post;
      if (prop === '__obscura_wrapsRealm') return true;
      // Not `receiver`: an accessor on a real global must run with the global
      // itself as `this`, not with this proxy.
      return Reflect.get(target, prop);
    },
    has(target, prop) {
      return prop === '__obscura_wrapsRealm' || Reflect.has(target, prop);
    },
  });
  globalThis.__obscura_frameWindows[frameId] = win;
  return win;
}

// The host calls this inside the target realm.
globalThis.__obscura_deliverMessage = function(dataJson, origin, sourceFrameId) {
  let data = null;
  try { data = JSON.parse(dataJson).v; } catch (_) {}
  // Who to reply to: the frame above, or one of the frames below.
  const source = (globalThis.__obscura_frameId !== 0
                  && sourceFrameId === globalThis.__obscura_parentFrameId)
    ? globalThis.parent
    : _frameWindowFor(sourceFrameId);
  try {
    // Trusted, because the user agent delivers this event: the sender called
    // postMessage, it did not dispatch this. Real embedders check the flag and
    // drop anything untrusted, so an untrusted event is not merely suspicious,
    // it is silently discarded and the widget waits forever.
    globalThis.dispatchEvent(globalThis.__obscura_markTrusted(
      new MessageEvent('message', { data, origin, source })));
  } catch (error) {
    console.error('message listener failed:', error && error.message || error);
  }
};

// A window in another browsing context, as seen from this one.
//
// Only the cross-origin surface is exposed: reaching synchronously into another
// realm's DOM is not something this engine does, and a browser forbids it
// across origins anyway. Widgets use postMessage regardless, which is what it
// is for.
class _RemoteWindow {
  constructor(frameId) {
    Object.defineProperty(this, '_frameId', { value: frameId, enumerable: false });
  }
  postMessage(data, _targetOrigin, _transfer) { _sendRealmMessage(this._frameId, data); }
  get self() { return this; }
  get window() { return this; }
  get frames() { return this; }
  get parent() { return this; }
  get top() { return this; }
  get opener() { return null; }
  get closed() { return false; }
  get length() { return 0; }
  focus() {}
  blur() {}
  close() {}
}
_markNative(_RemoteWindow.prototype.postMessage);

const _remoteWindows = new Map();
function _remoteWindow(frameId) {
  let win = _remoteWindows.get(frameId);
  if (!win) {
    win = new _RemoteWindow(frameId);
    _remoteWindows.set(frameId, win);
  }
  return win;
}

// Installs `parent` and `top` for a framed document. Called from
// __obscura_init, before any of the document's own scripts run: `parent ===
// window` is how a document decides it is top-level, and one script taking
// that branch wrongly is enough to change everything after it.
function _installFramingRelationships() {
  if (!globalThis.__obscura_frameId) return; // the page really is the top
  for (const [name, frameId] of [
    ['parent', globalThis.__obscura_parentFrameId],
    ['top', 0], // the top browsing context is always the page's realm
  ]) {
    try {
      Object.defineProperty(globalThis, name, {
        value: _remoteWindow(frameId),
        writable: false,
        enumerable: true,
        configurable: true,
      });
    } catch (_) {}
  }
}

class _IframeWindow {
  constructor(doc, url) {
    this.document = doc;
    this._url = url;
    this.top = globalThis;
    this.parent = globalThis;
    this.frameElement = null;
    this.length = 0;
    this.name = '';
    this.closed = false;
    this.navigator = globalThis.navigator;
    this.screen = globalThis.screen;
    this.innerWidth = 300;
    this.innerHeight = 150;
    this.outerWidth = 300;
    this.outerHeight = 150;
    this.devicePixelRatio = globalThis.devicePixelRatio;
    this.localStorage = globalThis.localStorage;
    this.sessionStorage = globalThis.sessionStorage;
    this.performance = globalThis.performance;
    this.crypto = globalThis.crypto;
    this.console = globalThis.console;
    this.chrome = globalThis.chrome;

    try {
      const u = new URL(url);
      this.location = {
        href: url, origin: u.origin, protocol: u.protocol,
        host: u.host, hostname: u.hostname, port: u.port,
        pathname: u.pathname, search: u.search, hash: u.hash,
        toString() { return url; }, assign(){}, reload(){}, replace(){},
      };
    } catch(e) {
      this.location = { href: url, origin: '', protocol: '', host: '', hostname: '', port: '', pathname: '/', search: '', hash: '', toString() { return url; }, assign(){}, reload(){}, replace(){} };
    }

    const proxy = new Proxy(this, _iframeWindowProxyHandler);
    this.self = proxy;
    this.window = proxy;
    this.frames = proxy;
    return proxy;
  }

  postMessage(data, _targetOrigin, _transfer) {
    // Into the frame's own realm, through the host. This used to dispatch the
    // event on the *parent's* window, so a page could never actually talk to
    // the document inside its iframe. A frame that has not loaded yet has no
    // browsing context to receive anything.
    if (!this._frameId) return;
    _sendRealmMessage(this._frameId, data);
  }

  setTimeout(fn, ms) { return globalThis.setTimeout(fn, ms); }
  clearTimeout(id) { globalThis.clearTimeout(id); }
  setInterval(fn, ms) { return globalThis.setInterval(fn, ms); }
  clearInterval(id) { globalThis.clearInterval(id); }
  requestAnimationFrame(fn) { return globalThis.requestAnimationFrame(fn); }

  addEventListener(type, fn) {
    if (!this._listeners) this._listeners = {};
    if (!this._listeners[type]) this._listeners[type] = [];
    this._listeners[type].push(fn);
  }
  removeEventListener(type, fn) {
    if (this._listeners?.[type]) {
      this._listeners[type] = this._listeners[type].filter(h => h !== fn);
    }
  }
  dispatchEvent(event) {
    const handlers = this._listeners?.[event?.type] || [];
    for (const h of handlers) { try { h.call(this, event); } catch(e) {} }
    return true;
  }

  getComputedStyle(el) { return globalThis.getComputedStyle(el); }
  matchMedia(q) { return globalThis.matchMedia(q); }
  getSelection() { return globalThis.getSelection(); }
  fetch(input, init) { return globalThis.fetch(input, init); }
  close() { this.closed = true; }
  focus() {}
  blur() {}
}

// Encode an RGBA pixel buffer into a valid PNG data URL.
// Uses stored-block DEFLATE (no compression) wrapped in zlib.
// This produces a larger file than a real browser but the hash is unique
// per session (from _fpNoise) and valid, so it does not match the known
// headless stub.
function _encodePNG(w, h, rgba) {
  // RGBA scanlines: filter byte (0) + 4 bytes per pixel.
  var rowLen = 1 + w * 4;
  var raw = new Uint8Array(h * rowLen);
  for (var y = 0; y < h; y++) {
    var base = y * rowLen;
    raw[base] = 0;
    for (var x = 0; x < w; x++) {
      var s = (y * w + x) << 2, d = base + 1 + x * 4;
      raw[d] = rgba[s]; raw[d+1] = rgba[s+1]; raw[d+2] = rgba[s+2]; raw[d+3] = rgba[s+3];
    }
  }
  // Adler32 of raw
  var s1 = 1, s2 = 0, M = 65521;
  for (var i = 0; i < raw.length; i++) { s1 = (s1 + raw[i]) % M; s2 = (s2 + s1) % M; }
  var adler = ((s2 << 16) | s1) >>> 0;
  // Stored DEFLATE blocks (zlib level 0)
  var MAXB = 65535, nb = Math.ceil(raw.length / MAXB) || 1;
  var dlen = 2 + nb * 5 + raw.length + 4;
  var def = new Uint8Array(dlen), dp = 0;
  def[dp++] = 0x78; def[dp++] = 0x01;
  for (var bi = 0; bi < nb; bi++) {
    var bs = bi * MAXB, be = Math.min(raw.length, bs + MAXB), bl = be - bs;
    def[dp++] = bi === nb-1 ? 1 : 0;
    def[dp++] = bl&0xff; def[dp++] = (bl>>8)&0xff;
    def[dp++] = (~bl)&0xff; def[dp++] = (~bl>>8)&0xff;
    def.set(raw.subarray(bs, be), dp); dp += bl;
  }
  def[dp++]=(adler>>24)&0xff; def[dp++]=(adler>>16)&0xff; def[dp++]=(adler>>8)&0xff; def[dp]=adler&0xff;
  // CRC32 (lazy table)
  if (!_encodePNG._t) {
    var t = new Uint32Array(256);
    for (var n = 0; n < 256; n++) { var c = n; for (var k=0;k<8;k++) c=c&1?0xEDB88320^(c>>>1):(c>>>1); t[n]=c; }
    _encodePNG._t = t;
  }
  var T = _encodePNG._t;
  function crc32(a, st, ln) { var c=0xFFFFFFFF; for(var i=st,e=st+ln;i<e;i++) c=T[(c^a[i])&0xff]^(c>>>8); return (c^0xFFFFFFFF)>>>0; }
  function putChunk(out, off, type, data) {
    var dl = data.length;
    out[off]=(dl>>24)&0xff; out[off+1]=(dl>>16)&0xff; out[off+2]=(dl>>8)&0xff; out[off+3]=dl&0xff;
    out[off+4]=type.charCodeAt(0); out[off+5]=type.charCodeAt(1); out[off+6]=type.charCodeAt(2); out[off+7]=type.charCodeAt(3);
    out.set(data, off+8);
    var cr = crc32(out, off+4, 4+dl);
    out[off+8+dl]=(cr>>24)&0xff; out[off+9+dl]=(cr>>16)&0xff; out[off+10+dl]=(cr>>8)&0xff; out[off+11+dl]=cr&0xff;
    return off+12+dl;
  }
  var ihd = new Uint8Array(13);
  ihd[0]=(w>>24)&0xff; ihd[1]=(w>>16)&0xff; ihd[2]=(w>>8)&0xff; ihd[3]=w&0xff;
  ihd[4]=(h>>24)&0xff; ihd[5]=(h>>16)&0xff; ihd[6]=(h>>8)&0xff; ihd[7]=h&0xff;
  ihd[8]=8; ihd[9]=6; // 8-bit RGBA
  var png = new Uint8Array(8 + 25 + (12+dlen) + 12);
  png.set([0x89,0x50,0x4E,0x47,0x0D,0x0A,0x1A,0x0A]);
  var p = 8;
  p = putChunk(png, p, 'IHDR', ihd);
  p = putChunk(png, p, 'IDAT', def);
  putChunk(png, p, 'IEND', new Uint8Array(0));
  // Base64 encode
  var C = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  var b64 = 'data:image/png;base64,';
  for (var i = 0; i < png.length; i += 3) {
    var a=png[i], b=i+1<png.length?png[i+1]:0, c=i+2<png.length?png[i+2]:0;
    b64 += C[a>>2] + C[((a&3)<<4)|(b>>4)] + (i+1<png.length?C[((b&15)<<2)|(c>>6)]:'=') + (i+2<png.length?C[c&63]:'=');
  }
  return b64;
}

globalThis.__ariaQuerySelector = function(root, selector) { return null; };
globalThis.__ariaQuerySelectorAll = async function*(root, selector) { /* yields nothing */ };
const _MAX_CANVAS_DIMENSION = 32767;
const _MAX_CANVAS_PIXELS = 67108864;
class _Canvas2D {
  constructor(canvas) {
    this.canvas = canvas;
    this._damageQueued = false;
    this._resizeFromCanvas();
  }
  _canvasDimension(name, fallback) {
    const raw = this.canvas.getAttribute(name);
    if (raw === null || raw === '') return fallback;
    const parsed = Number.parseInt(raw, 10);
    return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
  }
  _resetDrawingState() {
    this.fillStyle = '#000000';
    this.strokeStyle = '#000000';
    this.lineWidth = 1;
    this.font = '10px sans-serif';
    this.textAlign = 'start';
    this.textBaseline = 'alphabetic';
    this.globalAlpha = 1;
    this.globalCompositeOperation = 'source-over';
    this._stateStack = [];
  }
  _resizeFromCanvas() {
    const requestedWidth = this._canvasDimension('width', 300);
    const requestedHeight = this._canvasDimension('height', 150);
    const valid = requestedWidth <= _MAX_CANVAS_DIMENSION
      && requestedHeight <= _MAX_CANVAS_DIMENSION
      && requestedWidth * requestedHeight <= _MAX_CANVAS_PIXELS;
    this._w = valid ? requestedWidth : 0;
    this._h = valid ? requestedHeight : 0;
    this._buf = new Uint8ClampedArray(this._w * this._h * 4);
    this._resetDrawingState();
    const register = Deno.core.ops.op_canvas_register_surface;
    if (typeof register === 'function') {
      // op2 accepts Uint8Array, while Canvas exposes Uint8ClampedArray. This
      // second view shares the exact backing store; no pixel copy is made.
      const bytes = new Uint8Array(
        this._buf.buffer,
        this._buf.byteOffset,
        this._buf.byteLength,
      );
      if (!register(this.canvas._nid, this._w, this._h, bytes)) {
        throw new RangeError('Canvas backing store allocation failed');
      }
    }
  }
  _markPaintDamage() {
    if (this._damageQueued) return;
    this._damageQueued = true;
    queueMicrotask(() => {
      this._damageQueued = false;
      const damage = Deno.core.ops.op_canvas_paint_damage;
      if (typeof damage === 'function') damage(this.canvas._nid);
    });
  }
  _parseColor(css) {
    if (!css || typeof css !== 'string' || css === 'none') return [0,0,0,0];
    if (css.startsWith('#')) {
      const hex = css.slice(1);
      if (hex.length === 3) return [parseInt(hex[0]+hex[0],16),parseInt(hex[1]+hex[1],16),parseInt(hex[2]+hex[2],16),255];
      if (hex.length === 6) return [parseInt(hex.slice(0,2),16),parseInt(hex.slice(2,4),16),parseInt(hex.slice(4,6),16),255];
      if (hex.length === 8) return [parseInt(hex.slice(0,2),16),parseInt(hex.slice(2,4),16),parseInt(hex.slice(4,6),16),parseInt(hex.slice(6,8),16)];
    }
    const m = css.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*([\d.]+))?\)/);
    if (m) return [+m[1],+m[2],+m[3],m[4]!==undefined?Math.round(+m[4]*255):255];
    const named = {red:[255,0,0,255],green:[0,128,0,255],blue:[0,0,255,255],white:[255,255,255,255],black:[0,0,0,255],yellow:[255,255,0,255],orange:[255,165,0,255],gray:[128,128,128,255],transparent:[0,0,0,0]};
    return named[css] || [0,0,0,255];
  }
  _setPixel(x, y, r, g, b, a) {
    x = Math.round(x); y = Math.round(y);
    if (x < 0 || x >= this._w || y < 0 || y >= this._h) return;
    const idx = (y * this._w + x) * 4;
    const alpha = (a / 255) * this.globalAlpha;
    if (this.globalCompositeOperation === 'multiply') {
      this._buf[idx+0] = Math.round((r/255) * (this._buf[idx+0]/255) * 255);
      this._buf[idx+1] = Math.round((g/255) * (this._buf[idx+1]/255) * 255);
      this._buf[idx+2] = Math.round((b/255) * (this._buf[idx+2]/255) * 255);
      this._buf[idx+3] = Math.min(255, this._buf[idx+3] + Math.round(a * alpha));
    } else {
      this._buf[idx+0] = Math.round(r * alpha + this._buf[idx+0] * (1 - alpha));
      this._buf[idx+1] = Math.round(g * alpha + this._buf[idx+1] * (1 - alpha));
      this._buf[idx+2] = Math.round(b * alpha + this._buf[idx+2] * (1 - alpha));
      this._buf[idx+3] = Math.min(255, Math.round(a * alpha + this._buf[idx+3] * (1 - alpha)));
    }
  }
  fillRect(x, y, w, h) {
    const [r,g,b,a] = this._parseColor(this.fillStyle);
    x=Math.round(x); y=Math.round(y); w=Math.round(w); h=Math.round(h);
    for (let py = Math.max(0,y); py < Math.min(this._h, y+h); py++) {
      for (let px = Math.max(0,x); px < Math.min(this._w, x+w); px++) {
        this._setPixel(px, py, r, g, b, a);
      }
    }
    this._markPaintDamage();
  }
  clearRect(x, y, w, h) {
    x=Math.round(x); y=Math.round(y); w=Math.round(w); h=Math.round(h);
    for (let py = Math.max(0,y); py < Math.min(this._h, y+h); py++) {
      for (let px = Math.max(0,x); px < Math.min(this._w, x+w); px++) {
        const idx = (py * this._w + px) * 4;
        this._buf[idx] = this._buf[idx+1] = this._buf[idx+2] = this._buf[idx+3] = 0;
      }
    }
    this._markPaintDamage();
  }
  strokeRect(x, y, w, h) {
    const [r,g,b,a] = this._parseColor(this.strokeStyle);
    const lw = this.lineWidth;
    for (let px = Math.round(x); px < Math.round(x+w); px++) {
      for (let l = 0; l < lw; l++) { this._setPixel(px, Math.round(y)+l, r,g,b,a); this._setPixel(px, Math.round(y+h)-1-l, r,g,b,a); }
    }
    for (let py = Math.round(y); py < Math.round(y+h); py++) {
      for (let l = 0; l < lw; l++) { this._setPixel(Math.round(x)+l, py, r,g,b,a); this._setPixel(Math.round(x+w)-1-l, py, r,g,b,a); }
    }
    this._markPaintDamage();
  }
  fillText(text, x, y) {
    const [r,g,b,a] = this._parseColor(this.fillStyle);
    const fontSize = parseInt(this.font) || 10;
    const scale = Math.max(1, Math.round(fontSize / 10));
    const str = String(text);
    let cx = Math.round(x);
    for (let i = 0; i < str.length; i++) {
      const code = str.charCodeAt(i);
      for (let row = 0; row < 7; row++) {
        for (let col = 0; col < 5; col++) {
          const on = ((_fpRand(code * 100 + row * 10 + col) > 0.45) &&
                      (row > 0 && row < 6 && col > 0 && col < 4)) ||
                     (_fpRand(code * 200 + row * 7 + col) > 0.7);
          if (on) {
            for (let sy = 0; sy < scale; sy++) {
              for (let sx = 0; sx < scale; sx++) {
                this._setPixel(cx + col*scale + sx, Math.round(y) - 7*scale + row*scale + sy, r, g, b, a);
              }
            }
          }
        }
      }
      cx += 6 * scale;
    }
    this._markPaintDamage();
  }
  strokeText(text, x, y) { this.fillText(text, x, y); }
  measureText(t) {
    const fontSize = parseInt(this.font) || 10;
    const scale = Math.max(1, Math.round(fontSize / 10));
    return { width: String(t).length * 6 * scale, actualBoundingBoxAscent: 7*scale, actualBoundingBoxDescent: 2*scale };
  }
  getImageData(x, y, w, h) {
    x=Math.round(x); y=Math.round(y); w=Math.round(w); h=Math.round(h);
    const data = new Uint8ClampedArray(w * h * 4);
    for (let py = 0; py < h; py++) {
      for (let px = 0; px < w; px++) {
        const srcX = x + px, srcY = y + py;
        const dstIdx = (py * w + px) * 4;
        if (srcX >= 0 && srcX < this._w && srcY >= 0 && srcY < this._h) {
          const srcIdx = (srcY * this._w + srcX) * 4;
          data[dstIdx] = this._buf[srcIdx];
          data[dstIdx+1] = this._buf[srcIdx+1];
          data[dstIdx+2] = this._buf[srcIdx+2];
          data[dstIdx+3] = this._buf[srcIdx+3];
        }
      }
    }
    return { data, width: w, height: h };
  }
  putImageData(imageData, dx, dy) {
    dx=Math.round(dx); dy=Math.round(dy);
    const {data, width: w, height: h} = imageData;
    for (let py = 0; py < h; py++) {
      for (let px = 0; px < w; px++) {
        const srcIdx = (py * w + px) * 4;
        const x = dx + px, y = dy + py;
        if (x >= 0 && x < this._w && y >= 0 && y < this._h) {
          const dstIdx = (y * this._w + x) * 4;
          this._buf[dstIdx] = data[srcIdx];
          this._buf[dstIdx+1] = data[srcIdx+1];
          this._buf[dstIdx+2] = data[srcIdx+2];
          this._buf[dstIdx+3] = data[srcIdx+3];
        }
      }
    }
    this._markPaintDamage();
  }
  createImageData(w, h) { return { data: new Uint8ClampedArray(w*h*4), width: w, height: h }; }
  drawImage(img, sx, sy, sw, sh, dx, dy, dw, dh) {
    if (img && img._ctx && img._ctx._buf) {
      const src = img._ctx;
      dx = dx ?? sx; dy = dy ?? sy; dw = dw ?? (sw ?? src._w); dh = dh ?? (sh ?? src._h);
      for (let py = 0; py < dh; py++) {
        for (let px = 0; px < dw; px++) {
          const srcX = Math.floor((sx||0) + px * (sw||src._w) / dw);
          const srcY = Math.floor((sy||0) + py * (sh||src._h) / dh);
          if (srcX >= 0 && srcX < src._w && srcY >= 0 && srcY < src._h) {
            const srcIdx = (srcY * src._w + srcX) * 4;
            this._setPixel(dx+px, dy+py, src._buf[srcIdx], src._buf[srcIdx+1], src._buf[srcIdx+2], src._buf[srcIdx+3]);
          }
        }
      }
    }
    this._markPaintDamage();
  }
  beginPath() { this._path = []; }
  closePath() {}
  moveTo(x, y) { if (this._path) this._path.push({t:'M',x,y}); }
  lineTo(x, y) { if (this._path) this._path.push({t:'L',x,y}); }
  bezierCurveTo() {} quadraticCurveTo() {}
  arc(x, y, r, s, e) { if (this._path) this._path.push({t:'A',x,y,r}); }
  arcTo() {}
  rect(x, y, w, h) { this.fillRect(x, y, w, h); }
  fill() {
    if (!this._path) return;
    const [r,g,b,a] = this._parseColor(this.fillStyle);
    for (const seg of this._path) {
      if (seg.t === 'A') {
        const cx = Math.round(seg.x), cy = Math.round(seg.y), rad = seg.r;
        const r2 = rad * rad;
        for (let py = Math.max(0, cy - rad); py <= Math.min(this._h - 1, cy + rad); py++) {
          for (let px = Math.max(0, cx - rad); px <= Math.min(this._w - 1, cx + rad); px++) {
            if ((px-cx)*(px-cx) + (py-cy)*(py-cy) <= r2) this._setPixel(px, py, r, g, b, a);
          }
        }
      }
    }
    this._path = [];
    this._markPaintDamage();
  }
  stroke() {}
  clip() {}
  save() { this._stateStack.push({fillStyle: this.fillStyle, strokeStyle: this.strokeStyle, globalAlpha: this.globalAlpha, font: this.font, lineWidth: this.lineWidth}); }
  restore() { const s = this._stateStack.pop(); if (s) Object.assign(this, s); }
  translate() {} rotate() {} scale() {}
  setTransform() {} resetTransform() {} transform() {}
  createLinearGradient(x0,y0,x1,y1) { return { addColorStop(){}, _x0:x0,_y0:y0,_x1:x1,_y1:y1 }; }
  createRadialGradient() { return { addColorStop(){} }; }
  createPattern() { return {}; }
  isPointInPath() { return false; }
  isPointInStroke() { return false; }
  // Line-dash plus a few path/style methods that charting libraries (Highcharts,
  // ECharts) call on every animation frame. A missing setLineDash threw
  // "is not a function" from a timer each tick, spamming errors (#258).
  setLineDash() {}
  getLineDash() { return []; }
  ellipse() {}
  roundRect() {}
  createConicGradient() { return { addColorStop(){} }; }
  getContextAttributes() { return { alpha: true, desynchronized: false, colorSpace: "srgb", willReadFrequently: false }; }
}

class HTMLCanvasElement extends Element {
  get width() {
    const raw = this.getAttribute('width');
    const parsed = raw === null ? 300 : Number.parseInt(raw, 10);
    return Number.isFinite(parsed) && parsed >= 0 ? parsed : 300;
  }
  set width(value) { this.setAttribute('width', Math.max(0, Number(value) || 0)); }
  get height() {
    const raw = this.getAttribute('height');
    const parsed = raw === null ? 150 : Number.parseInt(raw, 10);
    return Number.isFinite(parsed) && parsed >= 0 ? parsed : 150;
  }
  set height(value) { this.setAttribute('height', Math.max(0, Number(value) || 0)); }
  setAttribute(name, value) {
    super.setAttribute(name, value);
    const normalized = String(name).toLowerCase();
    if (this._ctx && (normalized === 'width' || normalized === 'height')) {
      this._ctx._resizeFromCanvas();
    }
  }
  removeAttribute(name) {
    super.removeAttribute(name);
    const normalized = String(name).toLowerCase();
    if (this._ctx && (normalized === 'width' || normalized === 'height')) {
      this._ctx._resizeFromCanvas();
    }
  }
}
globalThis.HTMLCanvasElement = HTMLCanvasElement;

HTMLCanvasElement.prototype.getContext = function getContext(type) {
  if (type === '2d') {
    if (!this._ctx) {
      try { this._ctx = new _Canvas2D(this); }
      catch (_error) { return null; }
    }
    return this._ctx;
  }
  if (type === 'webgl' || type === 'experimental-webgl' || type === 'webgl2') {
    // Context creation is allowed to fail, and that is the only truthful
    // behavior until the renderer has a real WebGL backend. The former shim
    // reported successful shader/program creation while every draw call was a
    // no-op. Feature-detecting applications consequently selected their WebGL
    // path, hid their HTML/image fallback, and produced a blank canvas.
    return null;
  }
  return null;
};
HTMLCanvasElement.prototype.toDataURL = function(type) {
  const ctx = this._ctx || this.getContext('2d');
  if (ctx && ctx._buf) {
    if (ctx._w === 0 || ctx._h === 0) return 'data:,';
    return _encodePNG(ctx._w, ctx._h, ctx._buf);
  }
  return 'data:,';
};
HTMLCanvasElement.prototype.toBlob = function(cb, type, q) {
  const url = this.toDataURL(type, q);
  const comma = url.indexOf(',');
  if (comma < 0 || !url.startsWith('data:image/')) { cb(null); return; }
  const binary = atob(url.slice(comma + 1));
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  cb(new Blob([bytes], {type: String(type || 'image/png')}));
};
Element.prototype.getBBox = function() { return { x: 0, y: 0, width: 0, height: 0 }; };
Element.prototype.getComputedTextLength = function() { return 0; };
Element.prototype.getExtentOfChar = function(ch) { return { x: 0, y: 0, width: 0, height: 0 }; };
Element.prototype.getSubStringLength = function(ch, len) { return 0; };

_markNative(HTMLCanvasElement.prototype.getContext);
_markNative(HTMLCanvasElement.prototype.toDataURL);
_markNative(HTMLCanvasElement.prototype.toBlob);

Element.prototype.attachShadow = function attachShadow(opts) {
  var _mode = opts == null ? undefined : opts.mode;
  if (_mode !== 'open' && _mode !== 'closed') {
    throw new TypeError('Failed to execute attachShadow on Element: the mode value is not a valid ShadowRootMode.');
  }
  var _ln = (this.localName || '').toLowerCase();
  if (!globalThis.__obscura_shadowHostNames.has(_ln) && _ln.indexOf('-') === -1) {
    throw new DOMException('Failed to execute attachShadow on Element: this element does not support attachShadow', 'NotSupportedError');
  }
  if (Deno.core.ops.op_shadow_root_info(this._nid)) {
    throw new DOMException('Failed to execute attachShadow on Element: the element already hosts a shadow tree.', 'NotSupportedError');
  }
  const rootNid = Deno.core.ops.op_shadow_attach(this._nid, _mode);
  if (rootNid < 0) {
    throw new DOMException('Failed to execute attachShadow on Element: this element does not support attachShadow', 'NotSupportedError');
  }
  const shadow = new ShadowRoot(rootNid, this, opts);
  _treeMutationEpoch++;
  shadow._treeDetachedExact = false;
  shadow._treeParent = null;
  shadow._treeParentEpoch = _treeMutationEpoch;
  shadow._treeConnected = this.isConnected;
  shadow._treeConnectedEpoch = _treeMutationEpoch;
  _cache.set(rootNid, shadow);
  return shadow;
};

_markNative(Element.prototype.attachShadow);

function _shadowRootForHost(host, includeClosed) {
  if (!host) return null;
  const info = Deno.core.ops.op_shadow_root_info(host._nid);
  if (!info) return null;
  const parts = info.split('\0');
  if (!includeClosed && parts[1] !== 'open') return null;
  const rootNid = +parts[0];
  let root = _cache.get(rootNid);
  if (!(root instanceof ShadowRoot)) {
    root = new ShadowRoot(rootNid, host, { mode: parts[1] });
    _cache.set(rootNid, root);
  }
  return root;
}

Object.defineProperty(Element.prototype, 'shadowRoot', {
  configurable: true,
  enumerable: true,
  get: function () {
    return _shadowRootForHost(this, false);
  },
});

// setHTMLUnsafe / getHTML: shims over innerHTML. setHTMLUnsafe parses markup
// like innerHTML (declarative shadow roots inside are not expanded yet, but the
// call no longer throws so the rest of a test file can run); getHTML serializes
// like innerHTML.
Element.prototype.setHTMLUnsafe = function setHTMLUnsafe(html) { this.innerHTML = String(html == null ? "" : html); };
Element.prototype.getHTML = function getHTML() { return this.innerHTML; };
_markNative(Element.prototype.setHTMLUnsafe);
_markNative(Element.prototype.getHTML);
// Document.parseHTMLUnsafe(html): static that parses into a new HTML document.
if (typeof Document !== 'undefined' && typeof Document.parseHTMLUnsafe !== 'function') {
  Document.parseHTMLUnsafe = function parseHTMLUnsafe(html) {
    return new DOMParser().parseFromString(String(html == null ? "" : html), "text/html");
  };
  _markNative(Document.parseHTMLUnsafe);
}

globalThis.AudioBuffer = class AudioBuffer {
  constructor(opts) {
    var o = (typeof opts === 'object' && opts !== null) ? opts : {};
    this.numberOfChannels = o.numberOfChannels || 1;
    this.length = o.length || 0;
    this.sampleRate = o.sampleRate || 44100;
    this.duration = this.length / (this.sampleRate || 44100);
    this._chs = [];
    for (var c = 0; c < this.numberOfChannels; c++) this._chs.push(new Float32Array(this.length));
  }
  getChannelData(c) { return this._chs[c] || this._chs[0] || new Float32Array(0); }
  copyFromChannel(dst, ch, start) { var s=this._chs[ch]||this._chs[0]; start=start||0; for(var i=0;i<dst.length;i++) dst[i]=(s&&s[start+i])||0; }
  copyToChannel(src, ch, start) { var d=this._chs[ch]||this._chs[0]; start=start||0; if(d) for(var i=0;i<src.length;i++) d[start+i]=src[i]; }
};
globalThis.AudioContext = class AudioContext {
  constructor() { this.sampleRate=_fp('audioSampleRate'); this.state='running'; this.currentTime=0; this.baseLatency=_fp('audioBaseLatency'); this.destination={maxChannelCount:2,numberOfInputs:1,numberOfOutputs:0,channelCount:2}; this._listeners={}; }
  addEventListener(type, fn) { if (!this._listeners[type]) this._listeners[type]=[]; this._listeners[type].push(fn); }
  removeEventListener(type, fn) { if (this._listeners[type]) this._listeners[type]=this._listeners[type].filter(h=>h!==fn); }
  _ap(v, min=-3.4028235e38, max=3.4028235e38) { return { value: v, defaultValue: v, minValue: min, maxValue: max, setValueAtTime(){} }; }
  createOscillator() { return {context:this,type:'sine',frequency:this._ap(440, -22050, 22050),detune:this._ap(0, -153600, 153600),connect(){},start(){},stop(){},disconnect(){},addEventListener(){},removeEventListener(){}}; }
  createDynamicsCompressor() { return {context:this,threshold:this._ap(_fp('compThreshold'), -100, 0),knee:this._ap(_fp('compKnee'), 0, 40),ratio:this._ap(_fp('compRatio'), 1, 20),attack:this._ap(0.003, 0, 1),release:this._ap(0.25, 0, 1),reduction:0,connect(){},disconnect(){}}; }
  createAnalyser() {
    return {context:this,fftSize:2048,frequencyBinCount:1024,channelCount:2,channelCountMode:'max',channelInterpretation:'speakers',maxDecibels:-30,minDecibels:-100,numberOfInputs:1,numberOfOutputs:1,smoothingTimeConstant:0.8,connect(){},disconnect(){},
      getByteFrequencyData(a){for(let i=0;i<a.length;i++)a[i]=Math.floor(_fpRand(600+i)*10);},
      getFloatFrequencyData(a){for(let i=0;i<a.length;i++)a[i]=-100+_fpRand(700+i)*5;}
    };
  }
  createGain() { return {context:this,gain:this._ap(1),connect(){},disconnect(){}}; }
  createBiquadFilter() { return {context:this,type:'lowpass',frequency:this._ap(350, 0, 22050),Q:this._ap(1, 0.0001, 1000),gain:this._ap(0, -40, 40),connect(){},disconnect(){}}; }
  createBufferSource() { return {context:this,buffer:null,connect(){},start(){},stop(){},disconnect(){},loop:false}; }
  createBuffer(ch,len,rate) { return new globalThis.AudioBuffer({numberOfChannels:ch||1,length:len||0,sampleRate:rate||44100}); }
  createScriptProcessor() { return {connect(){},disconnect(){},onaudioprocess:null}; }
  decodeAudioData(buf) { return Promise.resolve(this.createBuffer(2,44100,44100)); }
  resume() { this.state='running'; return Promise.resolve(); }
  suspend() { this.state='suspended'; return Promise.resolve(); }
  close() { this.state='closed'; return Promise.resolve(); }
};
globalThis.OfflineAudioContext = class OfflineAudioContext extends AudioContext {
  constructor(ch,len,rate) {
    super();
    if (typeof ch === 'object' && ch !== null) {
      this.length = ch.length || 44100;
      this.sampleRate = ch.sampleRate || 44100;
    } else {
      this.length = len || 44100;
      this.sampleRate = rate || 44100;
    }
    this.oncomplete = null;
  }
  startRendering() {
    var self = this;
    var buf = this.createBuffer(1, self.length, 44100);
    var data = buf.getChannelData(0);
    // Simulate compressed triangle wave at 10kHz.
    // Target: sum(|data[4500..5000]|) matches Chrome Linux (~124.04347527516074).
    var target = 124.04347527516074 + (_fpRand(9991) - 0.5) * 0.002;
    var freq = 10000, sr = 44100;
    for (var i = 0; i < self.length; i++) {
      var phase = ((i * freq / sr) % 1 + 1) % 1;
      data[i] = phase < 0.5 ? 4*phase - 1 : 3 - 4*phase;
    }
    var s = 0;
    for (var i = 4500; i < 5000; i++) s += Math.abs(data[i]);
    var scale = s > 0 ? target / s : 0;
    for (var i = 0; i < self.length; i++) data[i] *= scale;
    // Fire oncomplete + 'complete' listeners on next microtask so callers
    // can register handlers synchronously after startRendering().
    var p = Promise.resolve().then(function() {
      var evt = {renderedBuffer: buf, target: self, type: 'complete'};
      if (typeof self.oncomplete === 'function') {
        try { self.oncomplete(evt); } catch(e) {}
      }
      var listeners = (self._listeners && self._listeners['complete']) || [];
      for (var i = 0; i < listeners.length; i++) {
        try { listeners[i](evt); } catch(e) {}
      }
      return buf;
    });
    return p;
  }
};
globalThis.webkitAudioContext = globalThis.AudioContext;

globalThis.speechSynthesis = {
  speaking: false, pending: false, paused: false,
  getVoices() { return [{ name:'Google US English', lang:'en-US', default:true, localService:true, voiceURI:'Google US English' }]; },
  speak() {}, cancel() {}, pause() {}, resume() {},
  addEventListener() {}, removeEventListener() {},
  onvoiceschanged: null,
};
globalThis.SpeechSynthesisUtterance = class SpeechSynthesisUtterance { constructor(t){this.text=t;this.lang='en-US';this.rate=1;this.pitch=1;this.volume=1;} };

globalThis.MediaStream = class MediaStream { constructor(){this.id='';this.active=true;} getTracks(){return [];} getAudioTracks(){return [];} getVideoTracks(){return [];} addTrack(){} removeTrack(){} clone(){return new MediaStream();} };
globalThis.MediaStreamTrack = class MediaStreamTrack { constructor(){this.kind='';this.enabled=true;this.readyState='live';} stop(){} clone(){return new MediaStreamTrack();} };
globalThis.RTCPeerConnection = class RTCPeerConnection {
  constructor(){this.localDescription=null;this.remoteDescription=null;this.iceConnectionState='new';this.iceGatheringState='new';this.signalingState='stable';this.connectionState='new';}
  createOffer(){return Promise.resolve({type:'offer',sdp:''});}
  createAnswer(){return Promise.resolve({type:'answer',sdp:''});}
  setLocalDescription(){return Promise.resolve();}
  setRemoteDescription(){return Promise.resolve();}
  addIceCandidate(){return Promise.resolve();}
  close(){}
  createDataChannel(){return {close(){},send(){},addEventListener(){},removeEventListener(){}};}
  addEventListener(){} removeEventListener(){}
  getStats(){return Promise.resolve(new Map());}
};
globalThis.RTCSessionDescription = class RTCSessionDescription { constructor(d){this.type=d?.type;this.sdp=d?.sdp;} };
globalThis.RTCIceCandidate = class RTCIceCandidate { constructor(d){this.candidate=d?.candidate||'';} };

// Minimal but spec-shape-correct IndexedDB shim. We don't persist anything,
// but authentication libraries (Firebase, Supabase, dexie) hang forever on
// the first `get` because their request's `onsuccess` is never called. Fire
// `onsuccess` asynchronously with `null` so reads complete-but-empty, which
// most libraries treat as a cache miss and fall back to the network.
function _idbRequest(produceResult) {
  const req = {
    result: undefined,
    error: null,
    source: null,
    transaction: null,
    readyState: 'pending',
    onsuccess: null,
    onerror: null,
    addEventListener(type, fn) { req['on' + type] = fn; },
    removeEventListener(type, fn) { if (req['on' + type] === fn) req['on' + type] = null; },
  };
  Promise.resolve().then(() => {
    try {
      req.result = produceResult();
      req.readyState = 'done';
      if (typeof req.onsuccess === 'function') {
        try { req.onsuccess({ target: req, type: 'success' }); } catch (e) {}
      }
    } catch (e) {
      req.error = e; req.readyState = 'done';
      if (typeof req.onerror === 'function') {
        try { req.onerror({ target: req, type: 'error' }); } catch (e2) {}
      }
    }
  });
  return req;
}

function _idbObjectStore(name) {
  const data = new Map();
  return {
    name,
    keyPath: null,
    autoIncrement: false,
    indexNames: { contains() { return false; }, length: 0, item() { return null; } },
    transaction: null,
    add(value, key) { const k = key ?? Date.now(); data.set(k, value); return _idbRequest(() => k); },
    put(value, key) { const k = key ?? Date.now(); data.set(k, value); return _idbRequest(() => k); },
    get(key) { return _idbRequest(() => data.get(key) ?? undefined); },
    getAll() { return _idbRequest(() => Array.from(data.values())); },
    getAllKeys() { return _idbRequest(() => Array.from(data.keys())); },
    getKey(key) { return _idbRequest(() => (data.has(key) ? key : undefined)); },
    delete(key) { return _idbRequest(() => { data.delete(key); return undefined; }); },
    clear() { return _idbRequest(() => { data.clear(); return undefined; }); },
    count() { return _idbRequest(() => data.size); },
    openCursor() { return _idbRequest(() => null); },
    openKeyCursor() { return _idbRequest(() => null); },
    createIndex() { return { name: '', keyPath: '', unique: false, multiEntry: false, get() { return _idbRequest(() => undefined); } }; },
    index() { return { get() { return _idbRequest(() => undefined); }, getAll() { return _idbRequest(() => []); }, count() { return _idbRequest(() => 0); }, openCursor() { return _idbRequest(() => null); } }; },
    deleteIndex() {},
  };
}

function _idbTransaction(storeNames) {
  const stores = new Map();
  const names = Array.isArray(storeNames) ? storeNames : [storeNames];
  for (const n of names) stores.set(String(n), _idbObjectStore(String(n)));
  const tx = {
    db: null,
    mode: 'readonly',
    objectStoreNames: { contains: (n) => stores.has(String(n)), length: stores.size },
    onabort: null, oncomplete: null, onerror: null,
    error: null,
    objectStore(name) {
      let s = stores.get(name);
      if (!s) { s = _idbObjectStore(name); stores.set(name, s); }
      s.transaction = tx;
      return s;
    },
    abort() {},
    commit() {},
    addEventListener(type, fn) { tx['on' + type] = fn; },
    removeEventListener(type, fn) { if (tx['on' + type] === fn) tx['on' + type] = null; },
  };
  Promise.resolve().then(() => {
    if (typeof tx.oncomplete === 'function') {
      try { tx.oncomplete({ target: tx, type: 'complete' }); } catch (e) {}
    }
  });
  return tx;
}

function _idbDatabase(name, version) {
  return {
    name,
    version,
    objectStoreNames: { contains() { return false; }, length: 0, item() { return null; } },
    createObjectStore(n) { return _idbObjectStore(n); },
    deleteObjectStore() {},
    transaction(storeNames, mode) {
      const tx = _idbTransaction(storeNames);
      tx.mode = mode || 'readonly';
      return tx;
    },
    close() {},
    onversionchange: null, onabort: null, onerror: null, onclose: null,
    addEventListener() {}, removeEventListener() {},
  };
}

globalThis.indexedDB = {
  open(name, version) {
    return _idbRequest(() => _idbDatabase(name, version || 1));
  },
  deleteDatabase(_name) { return _idbRequest(() => undefined); },
  databases() { return Promise.resolve([]); },
  cmp(a, b) { return a < b ? -1 : a > b ? 1 : 0; },
};
globalThis.IDBKeyRange = {
  only(v) { return { lower: v, upper: v, lowerOpen: false, upperOpen: false, includes(x) { return x === v; } }; },
  lowerBound(v, open) { return { lower: v, upper: null, lowerOpen: !!open, upperOpen: false, includes(x) { return open ? x > v : x >= v; } }; },
  upperBound(v, open) { return { lower: null, upper: v, lowerOpen: false, upperOpen: !!open, includes(x) { return open ? x < v : x <= v; } }; },
  bound(l, u, lo, uo) { return { lower: l, upper: u, lowerOpen: !!lo, upperOpen: !!uo, includes(x) { return (lo ? x > l : x >= l) && (uo ? x < u : x <= u); } }; },
};

globalThis.caches = {
  open() { return Promise.resolve({ match(){return Promise.resolve(undefined);}, put(){return Promise.resolve();}, delete(){return Promise.resolve(false);}, keys(){return Promise.resolve([]);} }); },
  match() { return Promise.resolve(undefined); },
  has() { return Promise.resolve(false); },
  delete() { return Promise.resolve(false); },
  keys() { return Promise.resolve([]); },
};

_markNative(AudioContext); _markNative(OfflineAudioContext);
_markNative(SpeechSynthesisUtterance);
_markNative(MediaStream); _markNative(MediaStreamTrack);
_markNative(RTCPeerConnection); _markNative(RTCSessionDescription); _markNative(RTCIceCandidate);

// Timezone is driven by the process TZ (set by the CLI, default Europe/Berlin),
// so native Intl.DateTimeFormat and Date report the same zone. No JS override:
// forcing a fixed zone here only on Intl left Date on UTC, which is the exact
// cross-surface mismatch a fingerprinting script looks for.

if (typeof PointerEvent === 'undefined') {
  globalThis.PointerEvent = class PointerEvent extends MouseEvent {
    constructor(type, opts={}) { super(type, opts); this.pointerId = opts.pointerId || 0; this.width = opts.width || 1; this.height = opts.height || 1; this.pressure = opts.pressure || 0; this.pointerType = opts.pointerType || 'mouse'; }
  };
}

if (typeof navigator.credentials === 'undefined') {
  navigator.credentials = { get(){return Promise.resolve(null);}, create(){return Promise.resolve(null);}, store(){return Promise.resolve();}, preventSilentAccess(){return Promise.resolve();} };
}

navigator.mediaCapabilities = {
  decodingInfo(cfg) {
    return Promise.resolve({ supported: true, smooth: true, powerEfficient: true, keySystemAccess: null, configuration: cfg });
  },
  encodingInfo(cfg) {
    return Promise.resolve({ supported: true, smooth: true, powerEfficient: true, configuration: cfg });
  },
};
navigator.locks = {
  request(name, opts, cb) {
    if (typeof opts === 'function') { cb = opts; opts = {}; }
    if (typeof cb === 'function') return Promise.resolve(cb({ name, mode: (opts && opts.mode) || 'exclusive' }));
    return Promise.resolve(null);
  },
  query() { return Promise.resolve({ held: [], pending: [] }); },
};
navigator.keyboard = {
  getLayoutMap() { return Promise.resolve(new Map()); },
  lock() { return Promise.resolve(); },
  unlock() {},
};
navigator.gpu = { requestAdapter() { return Promise.resolve(null); } };
navigator.wakeLock = { request() { return Promise.reject(new DOMException('Not allowed', 'NotAllowedError')); } };

globalThis.opener = null;

globalThis.Worker = class Worker {
  constructor(url) {
    this.onmessage = null;
    this.onerror = null;
    this._terminated = false;
    this._listeners = {};
    const worker = this;

    let resolvedUrl = url;
    if (typeof url === 'string') {
      const blob = globalThis.__blobStore?.[url];
      if (blob) {
        worker._code = blob;
        // Auto-start on next tick so caller can set onmessage first.
        setTimeout(() => worker._autoRun(), 0);
        return;
      }
      // Resolve relative URLs against the current page.
      if (!url.startsWith('http') && !url.startsWith('blob:') && !url.startsWith('data:')) {
        try { resolvedUrl = new URL(url, globalThis.location?.href || '').href; } catch(e) {}
      }
      (async () => {
        try {
          const resp = await fetch(resolvedUrl);
          worker._code = await resp.text();
          if (!worker._terminated) worker._autoRun();
        } catch(e) { if (worker.onerror) worker.onerror(e); }
      })();
    }
  }
  _makeScope() {
    const worker = this;
    // WorkerGlobalScope defined + no document property → IS_WORKER_SCOPE = true in creepjs
    const scope = {
      WorkerGlobalScope: function WorkerGlobalScope() {},
      DedicatedWorkerGlobalScope: function DedicatedWorkerGlobalScope() {},
      postMessage: (msg) => {
        if (worker._terminated) return;
        const evt = { data: msg };
        if (worker.onmessage) worker.onmessage(evt);
        const ls = worker._listeners['message'] || [];
        for (const h of ls) h(evt);
      },
      addEventListener: (type, fn) => {
        if (!scope._ev) scope._ev = {};
        if (!scope._ev[type]) scope._ev[type] = [];
        scope._ev[type].push(fn);
      },
      close: () => { worker._terminated = true; },
      crypto: globalThis.crypto,
      Crypto: globalThis.Crypto,
      TextEncoder: globalThis.TextEncoder,
      TextDecoder: globalThis.TextDecoder,
      atob: globalThis.atob,
      btoa: globalThis.btoa,
      setTimeout: globalThis.setTimeout,
      setInterval: globalThis.setInterval,
      clearTimeout: globalThis.clearTimeout,
      clearInterval: globalThis.clearInterval,
      scheduler: globalThis.scheduler,
      Scheduler: globalThis.Scheduler,
      fetch: globalThis.fetch,
      console: globalThis.console,
      performance: globalThis.performance,
      location: globalThis.location,
    };
    scope.self = scope;
    return scope;
  }
  _autoRun() {
    if (this._terminated || !this._code) return;
    const worker = this;
    const scope = worker._makeScope();
    try {
      const fn = new Function('self', 'postMessage', 'addEventListener', 'close', worker._code);
      fn(scope, scope.postMessage, scope.addEventListener, scope.close);
    } catch(e) {
      console.error('Worker error:', e.message);
      if (worker.onerror) worker.onerror(e);
    }
  }
  postMessage(data) {
    if (this._terminated) return;
    const worker = this;
    setTimeout(() => {
      if (worker._terminated || !worker._code) return;
      const scope = worker._makeScope();
      try {
        const fn = new Function('self', 'postMessage', 'addEventListener', 'close', worker._code);
        fn(scope, scope.postMessage, scope.addEventListener, scope.close);
        const evs = (scope._ev && scope._ev['message']) || [];
        if (evs.length) { for (const h of evs) h({ data }); }
        else if (scope.onmessage) scope.onmessage({ data });
      } catch(e) {
        console.error('Worker error:', e.message);
        if (worker.onerror) worker.onerror(e);
      }
    }, 0);
  }
  terminate() { this._terminated = true; }
  addEventListener(type, fn) {
    if (!this._listeners[type]) this._listeners[type] = [];
    this._listeners[type].push(fn);
  }
  removeEventListener(type, fn) {
    if (this._listeners[type]) this._listeners[type] = this._listeners[type].filter(h => h !== fn);
  }
};

globalThis.__blobStore = globalThis.__blobStore || {};
URL.createObjectURL = function(blob) {
  if (blob) {
    const id = 'blob:obscura/' + Math.random().toString(36).substring(2);
    // Store synchronously so a Worker built from the blob URL in the same
    // tick sees its source. Blob-URL Worker construction is synchronous in
    // real browsers; the previous async blob.text().then() store raced the
    // Worker constructor, so new Worker(blobURL) fell through to fetch() and
    // failed (net::ERR_FAILED), which broke AWS WAF's proof-of-work worker.
    // The obscura Blob materializes _bytes in its constructor; fall back to
    // the async text() store only for foreign Blob shims without _bytes.
    if (blob._bytes) {
      let text = '';
      try { text = new TextDecoder().decode(blob._bytes); } catch (e) {}
      globalThis.__blobStore[id] = text;
    } else if (typeof blob.text === 'function') {
      blob.text().then(text => { globalThis.__blobStore[id] = text; });
    } else {
      globalThis.__blobStore[id] = '';
    }
    return id;
  }
  return 'blob:obscura/fallback';
};
URL.revokeObjectURL = function(url) {
  delete globalThis.__blobStore[url];
};

// Window-level scrolling (issue #468). #431 gave elements functional
// scrollTop/scrollLeft plus scroll methods, but left these three as no-ops, so
// the dominant infinite-scroll idiom -- window.scrollTo(0, body.scrollHeight),
// window.scrollBy(0, 500), then a window 'scroll' listener -- did nothing at
// all: the offset never moved and no event ever fired.
//
// The page offset is stored on the scrolling element rather than in separate
// window state, so window.scrollY and document.scrollingElement.scrollTop are
// two views of one value, which is what pages assume. Render builds clamp that
// shared root offset against measured document overflow; non-render builds keep
// the legacy synthetic offset used by automation-only consumers.
function _scrollRoot() {
  const doc = globalThis.document;
  return (doc && doc.scrollingElement) || null;
}
function _windowScroll(x, y, relative) {
  const root = _scrollRoot();
  if (!root) return;
  const beforeLeft = root.scrollLeft || 0;
  const beforeTop = root.scrollTop || 0;
  let left, top;
  if (x !== null && typeof x === 'object') { left = x.left; top = x.top; }
  else { left = x; top = y; }
  if (left !== undefined) {
    root.scrollLeft = (relative ? (root.scrollLeft || 0) : 0) + (+left || 0);
  }
  if (top !== undefined) {
    root.scrollTop = (relative ? (root.scrollTop || 0) : 0) + (+top || 0);
  }
  if ((root.scrollLeft || 0) === beforeLeft && (root.scrollTop || 0) === beforeTop) {
    return;
  }
  // Async, matching the element path #431 added. Dispatched at the document
  // AND the window: a page scroll event reaches both in Chrome, but
  // Document.dispatchEvent here runs only its own listeners and does not
  // propagate, so firing once would strand half the listeners.
  setTimeout(() => {
    try {
      const doc = globalThis.document;
      if (doc) doc.dispatchEvent(new Event('scroll', { bubbles: false }));
      globalThis.dispatchEvent(new Event('scroll', { bubbles: false }));
    } catch (e) {}
  }, 0);
}
globalThis.scrollTo = function(x, y) { _windowScroll(x, y, false); };
globalThis.scrollBy = function(x, y) { _windowScroll(x, y, true); };
globalThis.scroll = function(x, y) { _windowScroll(x, y, false); };
_markNative(globalThis.scrollTo);
_markNative(globalThis.scrollBy);
_markNative(globalThis.scroll);
// Read-only accessors, as on a real Window: assigning window.scrollY does not
// scroll the page. These replace the hard-coded 0 data properties defined
// earlier, so they must stay after them.
for (const [name, offset] of [
  ['scrollX', 'scrollLeft'], ['pageXOffset', 'scrollLeft'],
  ['scrollY', 'scrollTop'], ['pageYOffset', 'scrollTop'],
]) {
  Object.defineProperty(globalThis, name, {
    configurable: true,
    enumerable: true,
    get() { const root = _scrollRoot(); return root ? (root[offset] || 0) : 0; },
  });
}
globalThis.focus = function() {}; _markNative(globalThis.focus);
globalThis.blur = function() {}; _markNative(globalThis.blur);
globalThis.print = function() {}; _markNative(globalThis.print);
globalThis.alert = function() {}; _markNative(globalThis.alert);
globalThis.confirm = function() { return true; }; _markNative(globalThis.confirm);
globalThis.prompt = function() { return null; }; _markNative(globalThis.prompt);
globalThis.open = function() { return null; }; _markNative(globalThis.open);
globalThis.close = function() {}; _markNative(globalThis.close);
globalThis.stop = function() {}; _markNative(globalThis.stop);
// `window.postMessage` targets this same window. It was a no-op, so a page
// that posted to itself and waited for the `message` event waited forever.
// Same realm, so this needs no host round trip; it is queued as a task because
// postMessage never delivers synchronously.
globalThis.postMessage = function(data, _targetOrigin, _transfer) {
  let clone = data;
  // Match the cross-realm path: a value postMessage cannot carry is rejected
  // at the call, not delivered as something else.
  try {
    clone = JSON.parse(JSON.stringify({ v: data === undefined ? null : data })).v;
  } catch (_) {
    throw new DOMException('The object could not be cloned.', 'DataCloneError');
  }
  const origin = _realmOrigin();
  setTimeout(() => {
    try {
      globalThis.dispatchEvent(globalThis.__obscura_markTrusted(
        new MessageEvent('message', { data: clone, origin, source: globalThis })));
    } catch (error) {
      console.error('message listener failed:', error && error.message || error);
    }
  }, 0);
};
_markNative(globalThis.postMessage);
globalThis.requestIdleCallback = globalThis.requestIdleCallback || function(cb) { return setTimeout(cb, 0); };
globalThis.cancelIdleCallback = globalThis.cancelIdleCallback || function(id) { clearTimeout(id); };
if (typeof ReadableStream === 'undefined') {
  globalThis.ReadableStream = class ReadableStream {
    constructor(source = {}, strategy = {}) {
      this._source = source;
      this._queue = [];
      this._reads = [];
      this._state = "readable";
      this._error = null;
      this.locked = false;
      const stream = this;
      this._controller = {
        enqueue(chunk) {
          if (stream._state !== "readable") return;
          const pending = stream._reads.shift();
          if (pending) pending.resolve({value: chunk, done: false});
          else stream._queue.push(chunk);
        },
        close() {
          if (stream._state !== "readable") return;
          stream._state = "closed";
          while (stream._reads.length) {
            stream._reads.shift().resolve({value: undefined, done: true});
          }
        },
        error(error) {
          if (stream._state !== "readable") return;
          stream._state = "errored";
          stream._error = error;
          while (stream._reads.length) stream._reads.shift().reject(error);
        },
        get desiredSize() { return Math.max(0, 1 - stream._queue.length); },
      };
      try {
        const started = source.start?.(this._controller);
        if (started && typeof started.then === "function") {
          started.catch((error) => this._controller.error(error));
        }
      } catch (error) {
        this._controller.error(error);
      }
    }
    getReader() {
      if (this.locked) throw new TypeError("ReadableStream is locked");
      this.locked = true;
      const stream = this;
      return {
        read() {
          if (stream._queue.length > 0) return Promise.resolve({ value: stream._queue.shift(), done: false });
          if (stream._state === "closed") return Promise.resolve({ value: undefined, done: true });
          if (stream._state === "errored") return Promise.reject(stream._error);
          return new Promise((resolve, reject) => stream._reads.push({resolve, reject}));
        },
        releaseLock() { stream.locked = false; },
        cancel(reason) { return stream.cancel(reason); },
        get closed() {
          if (stream._state === "closed") return Promise.resolve();
          if (stream._state === "errored") return Promise.reject(stream._error);
          return new Promise((resolve, reject) => {
            const poll = () => {
              if (stream._state === "closed") resolve();
              else if (stream._state === "errored") reject(stream._error);
              else setTimeout(poll, 0);
            };
            poll();
          });
        },
      };
    }
    cancel(reason) {
      this._queue.length = 0;
      this._controller.close();
      try { return Promise.resolve(this._source.cancel?.(reason)); }
      catch (error) { return Promise.reject(error); }
    }
    async pipeTo(destination) {
      const reader = this.getReader();
      const writer = destination.getWriter();
      try {
        while (true) {
          const {value, done} = await reader.read();
          if (done) break;
          await writer.write(value);
        }
        await writer.close();
      } catch (error) {
        try { await writer.abort(error); } catch {}
        throw error;
      } finally {
        reader.releaseLock();
        writer.releaseLock();
      }
    }
    pipeThrough(transform) {
      this.pipeTo(transform.writable).catch((error) => {
        try { transform.readable._controller?.error(error); } catch {}
      });
      return transform.readable;
    }
    tee() {
      let leftController;
      let rightController;
      const left = new ReadableStream({start(controller) { leftController = controller; }});
      const right = new ReadableStream({start(controller) { rightController = controller; }});
      (async () => {
        try {
          const reader = this.getReader();
          while (true) {
            const {value, done} = await reader.read();
            if (done) break;
            leftController.enqueue(value);
            rightController.enqueue(value);
          }
          leftController.close();
          rightController.close();
        } catch (error) {
          leftController.error(error);
          rightController.error(error);
        }
      })();
      return [left, right];
    }
    [Symbol.asyncIterator]() {
      const reader = this.getReader();
      return { next: () => reader.read(), return: () => { reader.releaseLock(); return Promise.resolve({done:true}); } };
    }
  };
}
if (typeof WritableStream === 'undefined') {
  globalThis.WritableStream = class WritableStream {
    constructor(sink = {}) {
      this._sink = sink;
      this._state = "writable";
      this._error = null;
      this._chain = Promise.resolve();
      this.locked = false;
      try {
        const started = sink.start?.({});
        if (started && typeof started.then === "function") this._chain = Promise.resolve(started);
      } catch (error) {
        this._state = "errored";
        this._error = error;
        this._chain = Promise.reject(error);
      }
    }
    getWriter() {
      if (this.locked) throw new TypeError("WritableStream is locked");
      this.locked = true;
      const stream = this;
      return {
        write(chunk) {
          if (stream._state !== "writable") return Promise.reject(stream._error || new TypeError("WritableStream is closed"));
          stream._chain = stream._chain.then(() => stream._sink.write?.(chunk));
          return stream._chain;
        },
        close() {
          if (stream._state !== "writable") return stream._chain;
          stream._state = "closed";
          stream._chain = stream._chain.then(() => stream._sink.close?.());
          return stream._chain;
        },
        abort(reason) {
          stream._state = "errored";
          stream._error = reason;
          stream._chain = stream._chain.then(() => stream._sink.abort?.(reason));
          return stream._chain;
        },
        releaseLock() { stream.locked = false; },
        get ready() { return stream._chain.then(() => undefined); },
        get closed() { return stream._chain.then(() => undefined); },
        get desiredSize() { return 1; },
      };
    }
    close() { const writer = this.getWriter(); return writer.close().finally(() => writer.releaseLock()); }
    abort(reason) { const writer = this.getWriter(); return writer.abort(reason).finally(() => writer.releaseLock()); }
  };
}
if (typeof TransformStream === 'undefined') {
  globalThis.TransformStream = class TransformStream {
    constructor(transformer = {}) {
      let controller;
      this.readable = new ReadableStream({
        start(readableController) { controller = readableController; },
      });
      this.writable = new WritableStream({
        async write(chunk) {
          if (transformer.transform) await transformer.transform(chunk, controller);
          else controller.enqueue(chunk);
        },
        async close() {
          if (transformer.flush) await transformer.flush(controller);
          controller.close();
        },
        abort(reason) { controller.error(reason); },
      });
      try { transformer.start?.(controller); }
      catch (error) { controller.error(error); }
    }
  };
}
if (typeof TextEncoderStream === 'undefined') {
  globalThis.TextEncoderStream = class TextEncoderStream {
    constructor() {
      const encoder = new TextEncoder();
      const transform = new TransformStream({
        transform(chunk, controller) {
          controller.enqueue(encoder.encode(String(chunk)));
        },
      });
      this.readable = transform.readable;
      this.writable = transform.writable;
    }
    get encoding() { return "utf-8"; }
  };
}
if (typeof TextDecoderStream === 'undefined') {
  globalThis.TextDecoderStream = class TextDecoderStream {
    constructor(label = "utf-8", options = {}) {
      const decoder = new TextDecoder(label, options);
      const transform = new TransformStream({
        transform(chunk, controller) {
          controller.enqueue(decoder.decode(chunk, {stream: true}));
        },
        flush(controller) {
          const tail = decoder.decode();
          if (tail) controller.enqueue(tail);
        },
      });
      this.readable = transform.readable;
      this.writable = transform.writable;
      this._decoder = decoder;
    }
    get encoding() { return this._decoder.encoding; }
    get fatal() { return this._decoder.fatal; }
    get ignoreBOM() { return this._decoder.ignoreBOM; }
  };
}

if (!globalThis.crypto) globalThis.crypto = {};
if (!globalThis.crypto.subtle) {
  // Real WebCrypto for the secret-key algorithms sites actually use: HMAC,
  // AES-GCM/CBC/CTR, PBKDF2 and HKDF, plus raw/JWK-oct key handling. The crypto
  // itself runs in Rust ops (RustCrypto); this shim only marshals bytes and
  // normalizes algorithm parameters. Public-key algorithms (RSA*, ECDSA, ECDH)
  // and non-symmetric key formats (pkcs8/spki) are not implemented and throw
  // NotSupportedError rather than returning fake data.
  const keyMaterial = new WeakMap();

  class CryptoKey {
    constructor() { throw new TypeError("Illegal constructor"); }
    get [Symbol.toStringTag]() { return "CryptoKey"; }
  }
  function makeKey(type, extractable, algorithm, usages, bytes) {
    const k = Object.create(CryptoKey.prototype);
    Object.defineProperty(k, "type", { value: type, enumerable: true });
    Object.defineProperty(k, "extractable", { value: !!extractable, enumerable: true });
    Object.defineProperty(k, "algorithm", { value: algorithm, enumerable: true });
    Object.defineProperty(k, "usages", { value: Object.freeze((usages || []).slice()), enumerable: true });
    keyMaterial.set(k, bytes);
    return k;
  }
  function keyBytes(key) {
    if (!(key instanceof CryptoKey) || !keyMaterial.has(key)) {
      throw new DOMException("Argument is not a valid CryptoKey", "InvalidAccessError");
    }
    return keyMaterial.get(key);
  }
  // A CryptoKey cloned via structuredClone or postMessage is a different
  // object, so the WeakMap lookup above misses and crypto.subtle throws
  // "Argument is not a valid CryptoKey". Re-register the (cloned) key's
  // material so the clone stays usable. The clone hook is dispatched by
  // _structuredClone via Symbol.toStringTag ("CryptoKey"); registered lazily
  // because structuredClone is defined before this block (issue #389).
  globalThis.__obscura_clone_hooks = globalThis.__obscura_clone_hooks || {};
  // `seen` is the clone memo _structuredClone hands every hook. Populate it so
  // one key reached twice in a graph clones to one shared object (and its key
  // material is registered once), matching structuredClone's identity rules.
  globalThis.__obscura_clone_hooks["CryptoKey"] = function (src, seen) {
    if (seen && seen.has(src)) return seen.get(src);
    const copy = makeKey(src.type, src.extractable, src.algorithm, src.usages, keyBytes(src));
    if (seen) seen.set(src, copy);
    return copy;
  };

  const toBytes = (data) => {
    if (data instanceof ArrayBuffer) return new Uint8Array(data);
    if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    return new Uint8Array(data || []);
  };
  const bufferOf = (u8) => new Uint8Array(u8).buffer;

  const ALGO_CANON = {
    "AES-CTR": "AES-CTR", "AES-CBC": "AES-CBC", "AES-GCM": "AES-GCM", "AES-KW": "AES-KW",
    "HMAC": "HMAC", "PBKDF2": "PBKDF2", "HKDF": "HKDF",
    "RSASSA-PKCS1-V1_5": "RSASSA-PKCS1-v1_5", "RSA-PSS": "RSA-PSS", "RSA-OAEP": "RSA-OAEP",
    "ECDSA": "ECDSA", "ECDH": "ECDH",
  };
  function normalizeAlgo(algorithm) {
    const a = typeof algorithm === "string" ? { name: algorithm } : (algorithm || {});
    const upper = String(a.name || "").toUpperCase();
    const name = ALGO_CANON[upper] || upper;
    return Object.assign({}, a, { name });
  }
  // SubtleCrypto hashes for HMAC/PBKDF2/HKDF and digest (SHA-1/256/384/512).
  function normalizeHash(h) {
    const n = (typeof h === "string" ? h : (h && h.name) || "").toUpperCase().replace("_", "-");
    if (n === "SHA-1" || n === "SHA-256" || n === "SHA-384" || n === "SHA-512") return n;
    throw new DOMException("Unsupported hash algorithm: " + (h && (h.name || h)), "NotSupportedError");
  }
  const hashBlockSize = (hash) => (hash === "SHA-384" || hash === "SHA-512" ? 128 : 64);

  function b64urlToBytes(s) {
    s = String(s).replace(/-/g, "+").replace(/_/g, "/");
    while (s.length % 4) s += "=";
    const bin = atob(s);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  function bytesToB64url(bytes) {
    let bin = "";
    for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
    return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  // Run an op, converting a Rust-side failure (bad GCM tag, bad CBC padding)
  // into the OperationError the WebCrypto spec requires. DOMExceptions we raise
  // ourselves pass through unchanged.
  function runOp(fn) {
    try { return fn(); }
    catch (e) {
      if (e instanceof DOMException) throw e;
      throw new DOMException(String((e && e.message) || e), "OperationError");
    }
  }

  function keyAlgorithmFor(alg, bytes) {
    switch (alg.name) {
      case "HMAC":
        return { name: "HMAC", hash: { name: normalizeHash(alg.hash) }, length: bytes.length * 8 };
      case "AES-CTR": case "AES-CBC": case "AES-GCM": case "AES-KW":
        if (bytes.length !== 16 && bytes.length !== 24 && bytes.length !== 32) {
          throw new DOMException("AES key data must be 128, 192, or 256 bits", "DataError");
        }
        return { name: alg.name, length: bytes.length * 8 };
      case "PBKDF2": return { name: "PBKDF2" };
      case "HKDF": return { name: "HKDF" };
      default:
        throw new DOMException("Unsupported key algorithm: " + alg.name, "NotSupportedError");
    }
  }

  const subtle = {
    async digest(algorithm, data) {
      const name = (typeof algorithm === "string" ? algorithm : algorithm && algorithm.name || "").toUpperCase().replace("_", "-");
      if (name !== "SHA-1" && name !== "SHA-256" && name !== "SHA-384" && name !== "SHA-512" &&
          name !== "SHA-512/224" && name !== "SHA-512/256") {
        throw new DOMException("Unrecognized algorithm name", "NotSupportedError");
      }
      return bufferOf(Deno.core.ops.op_subtle_digest(name, toBytes(data)));
    },

    async importKey(format, keyData, algorithm, extractable, keyUsages) {
      const alg = normalizeAlgo(algorithm);
      let bytes;
      if (format === "raw") {
        bytes = toBytes(keyData);
      } else if (format === "jwk") {
        if (!keyData || keyData.kty !== "oct" || typeof keyData.k !== "string") {
          throw new DOMException("Only symmetric 'oct' JWK keys are supported", "NotSupportedError");
        }
        bytes = b64urlToBytes(keyData.k);
      } else {
        throw new DOMException("Only 'raw' and symmetric 'jwk' key formats are supported", "NotSupportedError");
      }
      return makeKey("secret", extractable, keyAlgorithmFor(alg, bytes), keyUsages, bytes);
    },

    async exportKey(format, key) {
      const bytes = keyBytes(key);
      if (!key.extractable) throw new DOMException("Key is not extractable", "InvalidAccessError");
      if (format === "raw") return bufferOf(bytes);
      if (format === "jwk") {
        const jwk = { kty: "oct", k: bytesToB64url(bytes), ext: key.extractable, key_ops: key.usages.slice() };
        if (key.algorithm.name && key.algorithm.name.indexOf("AES-") === 0) {
          jwk.alg = "A" + (bytes.length * 8) + key.algorithm.name.slice(4);
        } else if (key.algorithm.name === "HMAC") {
          jwk.alg = "HS" + key.algorithm.hash.name.slice(4);
        }
        return jwk;
      }
      throw new DOMException("Only 'raw' and 'jwk' export is supported", "NotSupportedError");
    },

    async generateKey(algorithm, extractable, keyUsages) {
      const alg = normalizeAlgo(algorithm);
      if (alg.name === "HMAC") {
        const hash = normalizeHash(alg.hash);
        const len = alg.length ? Math.ceil(alg.length / 8) : hashBlockSize(hash);
        const bytes = Deno.core.ops.op_random_bytes(len);
        return makeKey("secret", extractable, { name: "HMAC", hash: { name: hash }, length: len * 8 }, keyUsages, bytes);
      }
      if (alg.name === "AES-CTR" || alg.name === "AES-CBC" || alg.name === "AES-GCM" || alg.name === "AES-KW") {
        if (alg.length !== 128 && alg.length !== 192 && alg.length !== 256) {
          throw new DOMException("AES key length must be 128, 192, or 256 bits", "OperationError");
        }
        const bytes = Deno.core.ops.op_random_bytes(alg.length / 8);
        return makeKey("secret", extractable, { name: alg.name, length: alg.length }, keyUsages, bytes);
      }
      throw new DOMException("generateKey does not support " + alg.name, "NotSupportedError");
    },

    async sign(algorithm, key, data) {
      const alg = normalizeAlgo(algorithm);
      const bytes = keyBytes(key);
      if (alg.name === "HMAC") {
        const hash = key.algorithm && key.algorithm.hash ? key.algorithm.hash.name : normalizeHash(alg.hash);
        return bufferOf(runOp(() => Deno.core.ops.op_subtle_hmac(hash, bytes, toBytes(data))));
      }
      throw new DOMException("sign does not support " + alg.name, "NotSupportedError");
    },

    async verify(algorithm, key, signature, data) {
      const alg = normalizeAlgo(algorithm);
      const bytes = keyBytes(key);
      if (alg.name === "HMAC") {
        const hash = key.algorithm && key.algorithm.hash ? key.algorithm.hash.name : normalizeHash(alg.hash);
        const mac = runOp(() => Deno.core.ops.op_subtle_hmac(hash, bytes, toBytes(data)));
        const sig = toBytes(signature);
        if (sig.length !== mac.length) return false;
        let diff = 0;
        for (let i = 0; i < mac.length; i++) diff |= mac[i] ^ sig[i];
        return diff === 0;
      }
      throw new DOMException("verify does not support " + alg.name, "NotSupportedError");
    },

    async encrypt(algorithm, key, data) { return aesCipher(true, algorithm, key, data); },
    async decrypt(algorithm, key, data) { return aesCipher(false, algorithm, key, data); },

    async deriveBits(algorithm, baseKey, length) {
      const alg = normalizeAlgo(algorithm);
      const bytes = keyBytes(baseKey);
      const lenBytes = Math.ceil((length || 0) / 8);
      if (alg.name === "PBKDF2") {
        const hash = normalizeHash(alg.hash);
        const salt = toBytes(alg.salt);
        const iterations = alg.iterations >>> 0;
        return bufferOf(runOp(() => Deno.core.ops.op_subtle_pbkdf2(hash, bytes, salt, iterations, lenBytes)));
      }
      if (alg.name === "HKDF") {
        const hash = normalizeHash(alg.hash);
        const salt = alg.salt != null ? toBytes(alg.salt) : new Uint8Array(0);
        const info = alg.info != null ? toBytes(alg.info) : new Uint8Array(0);
        return bufferOf(runOp(() => Deno.core.ops.op_subtle_hkdf(hash, bytes, salt, info, lenBytes)));
      }
      throw new DOMException("deriveBits does not support " + alg.name, "NotSupportedError");
    },

    async deriveKey(algorithm, baseKey, derivedKeyAlgorithm, extractable, keyUsages) {
      const dAlg = normalizeAlgo(derivedKeyAlgorithm);
      let bits;
      if (dAlg.name === "HMAC") {
        bits = dAlg.length || hashBlockSize(normalizeHash(dAlg.hash)) * 8;
      } else if (dAlg.name === "AES-CTR" || dAlg.name === "AES-CBC" || dAlg.name === "AES-GCM" || dAlg.name === "AES-KW") {
        bits = dAlg.length;
        if (bits !== 128 && bits !== 192 && bits !== 256) {
          throw new DOMException("AES key length must be 128, 192, or 256 bits", "OperationError");
        }
      } else {
        throw new DOMException("deriveKey does not support deriving " + dAlg.name, "NotSupportedError");
      }
      const derivedBits = await this.deriveBits(algorithm, baseKey, bits);
      return this.importKey("raw", derivedBits, derivedKeyAlgorithm, extractable, keyUsages);
    },

    async wrapKey(format, key, wrappingKey, wrapAlgorithm) {
      const exported = await this.exportKey(format, key);
      const bytes = format === "jwk"
        ? new TextEncoder().encode(JSON.stringify(exported))
        : new Uint8Array(exported);
      return this.encrypt(wrapAlgorithm, wrappingKey, bytes);
    },

    async unwrapKey(format, wrappedKey, unwrappingKey, unwrapAlgorithm, unwrappedKeyAlgorithm, extractable, keyUsages) {
      const decrypted = await this.decrypt(unwrapAlgorithm, unwrappingKey, wrappedKey);
      const keyData = format === "jwk"
        ? JSON.parse(new TextDecoder().decode(new Uint8Array(decrypted)))
        : decrypted;
      return this.importKey(format, keyData, unwrappedKeyAlgorithm, extractable, keyUsages);
    },
  };

  function aesCipher(encrypt, algorithm, key, data) {
    const alg = normalizeAlgo(algorithm);
    const bytes = keyBytes(key);
    const input = toBytes(data);
    if (alg.name === "AES-GCM") {
      const iv = toBytes(alg.iv);
      const aad = alg.additionalData != null ? toBytes(alg.additionalData) : new Uint8Array(0);
      const tagLength = alg.tagLength == null ? 128 : alg.tagLength;
      if (tagLength !== 128) {
        throw new DOMException("Only a 128-bit AES-GCM tag length is supported", "NotSupportedError");
      }
      return bufferOf(runOp(() => Deno.core.ops.op_subtle_aes_gcm(encrypt, bytes, iv, aad, input)));
    }
    if (alg.name === "AES-CBC") {
      const iv = toBytes(alg.iv);
      return bufferOf(runOp(() => Deno.core.ops.op_subtle_aes_cbc(encrypt, bytes, iv, input)));
    }
    if (alg.name === "AES-CTR") {
      const counter = toBytes(alg.counter);
      const length = alg.length >>> 0;
      return bufferOf(runOp(() => Deno.core.ops.op_subtle_aes_ctr(bytes, counter, length, input)));
    }
    throw new DOMException((encrypt ? "encrypt" : "decrypt") + " does not support " + alg.name, "NotSupportedError");
  }

  globalThis.CryptoKey = CryptoKey;
  globalThis.SubtleCrypto = function SubtleCrypto() { throw new TypeError("Illegal constructor"); };
  Object.setPrototypeOf(subtle, globalThis.SubtleCrypto.prototype);
  globalThis.crypto.subtle = subtle;
}

if (typeof DOMRect === 'undefined') {
  globalThis.DOMRect = class DOMRect {
    constructor(x=0,y=0,w=0,h=0) { this.x=x;this.y=y;this.width=w;this.height=h;this.top=y;this.right=x+w;this.bottom=y+h;this.left=x; }
    toJSON() { return {x:this.x,y:this.y,width:this.width,height:this.height,top:this.top,right:this.right,bottom:this.bottom,left:this.left}; }
    static fromRect(r={}) { return new DOMRect(r.x,r.y,r.width,r.height); }
  };
}

if (typeof DOMRectList === 'undefined') {
  globalThis.DOMRectList = class DOMRectList {
    constructor(arr=[]) {
      this.length = arr.length;
      for (let i = 0; i < arr.length; i++) this[i] = arr[i];
    }
    item(i) { return this[i] || null; }
    [Symbol.iterator]() {
      let i = 0, self = this;
      return { next() { const done = i >= self.length; return { value: done ? undefined : self[i++], done }; } };
    }
  };
}
if (typeof DOMPoint === 'undefined') {
  globalThis.DOMPoint = class DOMPoint {
    constructor(x=0,y=0,z=0,w=1) { this.x=x;this.y=y;this.z=z;this.w=w; }
    static fromPoint(p={}) { return new DOMPoint(p.x,p.y,p.z,p.w); }
  };
}
if (typeof DOMMatrix === 'undefined') {
  globalThis.DOMMatrix = class DOMMatrix {
    constructor() { this.a=1;this.b=0;this.c=0;this.d=1;this.e=0;this.f=0;this.is2D=true;this.isIdentity=true; }
    static fromMatrix() { return new DOMMatrix(); }
    static fromFloat32Array() { return new DOMMatrix(); }
    static fromFloat64Array() { return new DOMMatrix(); }
    multiply() { return new DOMMatrix(); }
    inverse() { return new DOMMatrix(); }
    translate() { return new DOMMatrix(); }
    scale() { return new DOMMatrix(); }
    rotate() { return new DOMMatrix(); }
    transformPoint(p) { return new DOMPoint(p?.x||0,p?.y||0); }
  };
}

if (typeof Image === 'undefined') {
  // In a real browser `new Image()` is `document.createElement('img')`, i.e. a
  // full HTMLImageElement. The old plain-class shim had no `.style`, so
  // `new Image().style` was `undefined` and libraries that touch it on a
  // detached image threw (issue #350). Build a real element so `.style`,
  // attribute reflection, and event dispatch all come for free.
  globalThis.Image = function Image(width, height) {
    const img = document.createElement('img');
    if (width !== undefined) img.width = width >>> 0;
    if (height !== undefined) img.height = height >>> 0;
    return img;
  };
  globalThis.Image.prototype = globalThis.HTMLImageElement.prototype;
}

if (typeof Audio === 'undefined') {
  globalThis.Audio = class Audio {
    constructor(src) { this.src = src || ''; this.paused = true; this.volume = 1; this.currentTime = 0; this.duration = 0; }
    play() { return Promise.resolve(); } pause() { this.paused = true; } load() {}
    addEventListener() {} removeEventListener() {}
  };
}

if (typeof FileReader === 'undefined') {
  globalThis.FileReader = class FileReader {
    constructor() {
      this.result = null; this.error = null; this.readyState = 0; // EMPTY
      this.onloadstart = null; this.onprogress = null; this.onload = null;
      this.onabort = null; this.onerror = null; this.onloadend = null;
      this._listeners = {};
    }
    get [Symbol.toStringTag]() { return "FileReader"; }
    _read(blob, kind, encoding) {
      // Spec: reading while LOADING throws InvalidStateError.
      if (this.readyState === 1) throw new DOMException("The object is already busy reading Blobs.", "InvalidStateError");
      this.readyState = 1; // LOADING
      this.result = null; this.error = null;
      this._fire("loadstart");
      const self = this;
      Promise.resolve().then(function () {
        if (self.readyState !== 1) return; // aborted before completion
        const bytes = (blob && blob._bytes) ? blob._bytes : new Uint8Array(0);
        try {
          if (kind === "text") self.result = new TextDecoder(encoding || "utf-8").decode(bytes);
          else if (kind === "binary") self.result = _bytesToBinaryString(bytes);
          else if (kind === "dataurl") self.result = "data:" + ((blob && blob.type) || "application/octet-stream") + ";base64," + btoa(_bytesToBinaryString(bytes));
          else self.result = _arrayBufferFromBytes(bytes);
        } catch (e) { self.error = e; }
        self.readyState = 2; // DONE
        self._fire("progress"); self._fire("load"); self._fire("loadend");
      });
    }
    readAsText(blob, encoding) { this._read(blob, "text", encoding); }
    readAsDataURL(blob) { this._read(blob, "dataurl"); }
    readAsArrayBuffer(blob) { this._read(blob, "arraybuffer"); }
    readAsBinaryString(blob) { this._read(blob, "binary"); }
    abort() {
      const wasReading = this.readyState === 1;
      this.readyState = 0; this.result = null;
      if (wasReading) { this._fire("abort"); this._fire("loadend"); }
    }
    _fire(type) {
      const ev = { type: type, target: this, currentTarget: this, lengthComputable: false, loaded: 0, total: 0 };
      const h = this["on" + type]; if (typeof h === "function") { try { h.call(this, ev); } catch (e) {} }
      const ls = this._listeners[type]; if (ls) for (const fn of ls.slice()) { try { fn.call(this, ev); } catch (e) {} }
    }
    addEventListener(t, fn) { if (typeof fn === "function") (this._listeners[t] = this._listeners[t] || []).push(fn); }
    removeEventListener(t, fn) { const ls = this._listeners[t]; if (ls) { const i = ls.indexOf(fn); if (i >= 0) ls.splice(i, 1); } }
    dispatchEvent() { return true; }
  };
  globalThis.FileReader.EMPTY = 0; globalThis.FileReader.LOADING = 1; globalThis.FileReader.DONE = 2;
  Object.assign(globalThis.FileReader.prototype, { EMPTY: 0, LOADING: 1, DONE: 2 });
}

// Real network sockets aren't implemented; we don't have a runtime WS / SSE
// client in V8. But pages that wait for an `open` event (Vite HMR clients
// embedded on docs sites, live-dashboards, anything calling
// `await new Promise(r => ws.addEventListener('open', r))`) silently hang
// forever otherwise. Fire `open` after a microtask so the consumer at least
// proceeds; subsequent messages never arrive, which is no worse than the
// current "no signal whatsoever" behaviour.
// Minimal EventTarget shared by socket-like classes. Real `EventTarget` is
// currently aliased to `Node`, which would drag DOM-tree assumptions into a
// `WebSocket`. Defining a private shim avoids that.
function _makeListenerBox(self) {
  const map = new Map();
  self.addEventListener = function (type, fn) {
    if (typeof fn !== 'function') return;
    let bucket = map.get(type);
    if (!bucket) { bucket = []; map.set(type, bucket); }
    bucket.push(fn);
  };
  self.removeEventListener = function (type, fn) {
    const bucket = map.get(type);
    if (!bucket) return;
    const i = bucket.indexOf(fn);
    if (i >= 0) bucket.splice(i, 1);
  };
  self.dispatchEvent = function (event) {
    const bucket = map.get(event.type);
    if (!bucket) return true;
    for (const fn of bucket.slice()) {
      try { fn.call(self, event); } catch (e) { /* swallow */ }
    }
    return true;
  };
}

if (typeof EventSource === 'undefined') {
  globalThis.EventSource = class EventSource {
    constructor(url, init) {
      this.url = url;
      this.readyState = 0; // CONNECTING
      this.withCredentials = !!(init && init.withCredentials);
      this.onopen = null; this.onmessage = null; this.onerror = null;
      _makeListenerBox(this);
      Promise.resolve().then(() => {
        if (this.readyState !== 0) return;
        this.readyState = 1; // OPEN
        const ev = new Event('open');
        if (typeof this.onopen === 'function') { try { this.onopen(ev); } catch (e) {} }
        try { this.dispatchEvent(ev); } catch (e) {}
      });
    }
    close() { this.readyState = 2; }
    static CONNECTING = 0; static OPEN = 1; static CLOSED = 2;
  };
}

if (typeof WebSocket === 'undefined') {
  globalThis.WebSocket = class WebSocket {
    constructor(url, protocols) {
      // Validate URL scheme per spec — Chrome throws SyntaxError for non-ws/wss URLs
      if (typeof url !== 'string' || !/^wss?:\/\//i.test(url)) {
        throw new DOMException(
          "Failed to construct 'WebSocket': The URL '" + url + "' is invalid.",
          'SyntaxError'
        );
      }
      this.url = url;
      this.readyState = 0; // CONNECTING
      this.bufferedAmount = 0;
      this.binaryType = 'blob';
      this.extensions = '';
      this.protocol = Array.isArray(protocols) ? (protocols[0] || '') : (protocols || '');
      this.onopen = null; this.onmessage = null; this.onerror = null; this.onclose = null;
      _makeListenerBox(this);
      Promise.resolve().then(() => {
        if (this.readyState !== 0) return;
        this.readyState = 1; // OPEN
        const ev = new Event('open');
        if (typeof this.onopen === 'function') { try { this.onopen(ev); } catch (e) {} }
        try { this.dispatchEvent(ev); } catch (e) {}
      });
    }
    send(data) { /* drop; no real socket */ }
    close(code, reason) {
      if (this.readyState >= 2) return;
      this.readyState = 3; // CLOSED
      const ev = new Event('close');
      ev.code = code || 1000; ev.reason = reason || ''; ev.wasClean = true;
      if (typeof this.onclose === 'function') { try { this.onclose(ev); } catch (e) {} }
      try { this.dispatchEvent(ev); } catch (e) {}
    }
    static CONNECTING = 0; static OPEN = 1; static CLOSING = 2; static CLOSED = 3;
  };
}

if (typeof BroadcastChannel === 'undefined') {
  // BroadcastChannel is used by authentication/session coordinators and by
  // modern framework dev/runtime clients. Keep the registry realm-local: one
  // Obscura page is one origin-bound browsing context today, so every channel
  // in this registry has the same storage key and origin by construction.
  const channelsByName = new Map();
  const channelState = new WeakMap();
  const stateFor = (channel) => {
    const state = channelState.get(channel);
    if (!state) throw new TypeError('Illegal invocation');
    return state;
  };
  const installHandler = (channel, type, callback) => {
    const state = stateFor(channel);
    const slot = type === 'message' ? 'onmessage' : 'onmessageerror';
    const wrapperSlot = type === 'message' ? 'messageWrapper' : 'messageErrorWrapper';
    const oldCallback = state[slot];
    state[slot] = callback;
    if (callback && !oldCallback) {
      const wrapper = (event) => {
        const current = channelState.get(channel)?.[slot];
        if (!current) return;
        if (typeof current === 'function') current.call(channel, event);
        else current.handleEvent.call(current, event);
      };
      state[wrapperSlot] = wrapper;
      _eventTargetAdd(channel, type, wrapper);
    } else if (!callback && oldCallback) {
      _eventTargetRemove(channel, type, state[wrapperSlot]);
      state[wrapperSlot] = null;
    }
  };

  globalThis.BroadcastChannel = class BroadcastChannel {
    constructor(name) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to construct 'BroadcastChannel': 1 argument required.");
      }
      const normalizedName = String(name);
      const state = {
        name: normalizedName,
        closed: false,
        onmessage: null,
        onmessageerror: null,
        messageWrapper: null,
        messageErrorWrapper: null,
      };
      channelState.set(this, state);
      let channels = channelsByName.get(normalizedName);
      if (!channels) channelsByName.set(normalizedName, channels = new Set());
      channels.add(this);
    }
    get name() { return stateFor(this).name; }
    get onmessage() { return stateFor(this).onmessage; }
    set onmessage(callback) {
      callback = typeof callback === 'function'
        || (callback && typeof callback.handleEvent === 'function')
        ? callback : null;
      installHandler(this, 'message', callback);
    }
    get onmessageerror() { return stateFor(this).onmessageerror; }
    set onmessageerror(callback) {
      callback = typeof callback === 'function'
        || (callback && typeof callback.handleEvent === 'function')
        ? callback : null;
      installHandler(this, 'messageerror', callback);
    }
    addEventListener(type, callback, options) {
      stateFor(this);
      _eventTargetAdd(this, type, callback, options);
    }
    removeEventListener(type, callback, options) {
      stateFor(this);
      _eventTargetRemove(this, type, callback, options);
    }
    dispatchEvent(event) {
      stateFor(this);
      return _eventTargetDispatch(this, event);
    }
    postMessage(message) {
      const state = stateFor(this);
      if (state.closed) {
        throw new DOMException("BroadcastChannel is closed.", "InvalidStateError");
      }

      // Serialization is synchronous and precedes recipient selection. This
      // preserves DataCloneError even when no peer is listening and freezes
      // the posted graph before the caller can mutate it.
      const snapshot = globalThis.structuredClone(message);
      const recipients = Array.from(channelsByName.get(state.name) || [])
        .filter((channel) => channel !== this && !channelState.get(channel)?.closed);
      const origin = globalThis.location?.origin || '';
      for (const recipient of recipients) {
        // Each destination gets an independent deserialization, not a shared
        // JS object. Clone now so all serialization remains part of postMessage.
        const data = globalThis.structuredClone(snapshot);
        _scheduleAfter(0, () => {
          const recipientState = channelState.get(recipient);
          if (!recipientState || recipientState.closed) return;
          _eventTargetDispatch(recipient, new MessageEvent('message', {
            data,
            origin,
            source: null,
            ports: [],
          }));
        });
      }
    }
    close() {
      const state = stateFor(this);
      if (state.closed) return;
      state.closed = true;
      const channels = channelsByName.get(state.name);
      if (!channels) return;
      channels.delete(this);
      if (!channels.size) channelsByName.delete(state.name);
    }
    get [Symbol.toStringTag]() { return 'BroadcastChannel'; }
  };
  // EventTarget is currently Node-backed in this runtime; link the prototype
  // without invoking Node's DOM-node constructor or exposing a fake `_nid`.
  Object.setPrototypeOf(globalThis.BroadcastChannel.prototype, globalThis.EventTarget.prototype);
}

if (typeof MediaQueryList === 'undefined') {
  globalThis.MediaQueryList = class MediaQueryList {
    constructor(q) { this.media = q || ''; this.matches = false; }
    addListener() {} removeListener() {} addEventListener() {} removeEventListener() {}
  };
}

if (typeof ImageData === 'undefined') {
  globalThis.ImageData = class ImageData {
    constructor(w, h) {
      if (w instanceof Uint8ClampedArray) { this.data = w; this.width = h; this.height = w.length / (4 * h); }
      else { this.width = w; this.height = h; this.data = new Uint8ClampedArray(w * h * 4); }
    }
  };
}

if (typeof CanvasRenderingContext2D === 'undefined') {
  globalThis.CanvasRenderingContext2D = class CanvasRenderingContext2D {};
}

if (typeof OffscreenCanvas === 'undefined') {
  globalThis.OffscreenCanvas = class OffscreenCanvas {
    constructor(w, h) { this.width = w; this.height = h; }
    getContext(type) { return globalThis.document?.createElement('canvas')?.getContext(type) || null; }
    convertToBlob() { return Promise.resolve(new Blob([''])); }
    transferToImageBitmap() { return {}; }
  };
}

if (typeof Path2D === 'undefined') {
  globalThis.Path2D = class Path2D { constructor(){} moveTo(){} lineTo(){} arc(){} rect(){} closePath(){} addPath(){} };
}

if (typeof ImageBitmap === 'undefined') {
  globalThis.ImageBitmap = class ImageBitmap { constructor(){this.width=0;this.height=0;} close(){} };
  globalThis.createImageBitmap = function() { return Promise.resolve(new ImageBitmap()); };
}

if (typeof Selection === 'undefined') {
  globalThis.Selection = class Selection {
    constructor(){this.anchorNode=null;this.focusNode=null;this.rangeCount=0;this.isCollapsed=true;this.type='None';}
    getRangeAt(){return null;} collapse(){} extend(){} selectAllChildren(){} deleteFromDocument(){}
    addRange(){} removeRange(){} removeAllRanges(){} toString(){return '';}
  };
}

if (typeof TreeWalker === 'undefined') {
  globalThis.TreeWalker = class TreeWalker {
    constructor(root){this.root=root;this.currentNode=root;this.whatToShow=0xFFFFFFFF;this.filter=null;}
    parentNode(){return this.currentNode?.parentNode||null;}
    firstChild(){return this.currentNode?.firstChild||null;}
    lastChild(){return this.currentNode?.lastChild||null;}
    previousSibling(){return this.currentNode?.previousSibling||null;}
    nextSibling(){return this.currentNode?.nextSibling||null;}
    nextNode(){return null;} previousNode(){return null;}
  };
}

if (typeof Range === 'undefined') {
  globalThis.Range = class Range {
    constructor(){this.startContainer=null;this.startOffset=0;this.endContainer=null;this.endOffset=0;this.collapsed=true;this.commonAncestorContainer=null;}
    setStart(n,o){this.startContainer=n;this.startOffset=o;} setEnd(n,o){this.endContainer=n;this.endOffset=o;}
    collapse(){} selectNode(){} selectNodeContents(){} cloneContents(){return document?.createDocumentFragment();}
    deleteContents(){} insertNode(){} getBoundingClientRect(){return new DOMRect();}
    getClientRects(){return new DOMRectList([]);} cloneRange(){return new Range();} toString(){return '';}
  };
}

if (typeof FontFace === 'undefined') {
  const _fontFaceString = value => String(value ?? '');
  const _fontFaceBytesBase64 = bytes => {
    const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
    let out = '';
    for (let i = 0; i < bytes.length; i += 3) {
      const a = bytes[i], b = bytes[i + 1] || 0, c = bytes[i + 2] || 0;
      out += alphabet[a >> 2];
      out += alphabet[((a & 3) << 4) | (b >> 4)];
      out += i + 1 < bytes.length ? alphabet[((b & 15) << 2) | (c >> 6)] : '=';
      out += i + 2 < bytes.length ? alphabet[c & 63] : '=';
    }
    return out;
  };
  const _fontFaceSource = source => {
    if (typeof source === 'string') {
      if (!source.trim()) throw new DOMException('The font source is empty', 'SyntaxError');
      return { css: source, binary: false };
    }
    let bytes;
    if (source instanceof ArrayBuffer) {
      bytes = new Uint8Array(source.slice(0));
    } else if (ArrayBuffer.isView(source)) {
      bytes = new Uint8Array(source.buffer.slice(source.byteOffset, source.byteOffset + source.byteLength));
    } else {
      throw new TypeError('FontFace source must be a CSS source string or BufferSource');
    }
    return {
      css: 'url("data:font/ttf;base64,' + _fontFaceBytesBase64(bytes) + '")',
      binary: true
    };
  };
  const _fontFaceDescriptor = (descriptors, name, fallback) =>
    descriptors && descriptors[name] !== undefined ? String(descriptors[name]) : fallback;
  const _fontFaceDeclarations = block => {
    const declarations = Object.create(null);
    let start = 0, depth = 0, quote = '', escaped = false;
    const commit = end => {
      const declaration = block.slice(start, end);
      const colon = declaration.indexOf(':');
      if (colon > 0) declarations[declaration.slice(0, colon).trim().toLowerCase()] =
        declaration.slice(colon + 1).trim();
    };
    for (let i = 0; i <= block.length; i++) {
      const ch = block[i];
      if (escaped) { escaped = false; continue; }
      if (ch === '\\') { escaped = true; continue; }
      if (quote) { if (ch === quote) quote = ''; continue; }
      if (ch === '"' || ch === "'") { quote = ch; continue; }
      if (ch === '(') depth++;
      else if (ch === ')') depth = Math.max(0, depth - 1);
      else if ((ch === ';' && depth === 0) || i === block.length) {
        commit(i);
        start = i + 1;
      }
    }
    return declarations;
  };
  const _fontFaceAuthoredRules = doc => {
    const out = [];
    for (const style of doc.querySelectorAll('style')) {
      const css = style.textContent || '';
      const pattern = /@font-face\s*\{([\s\S]*?)\}/gi;
      let match;
      while ((match = pattern.exec(css))) {
        const declarations = _fontFaceDeclarations(match[1]);
        const family = (declarations['font-family'] || '').trim().replace(/^(['"])(.*)\1$/, '$2');
        const source = declarations.src || '';
        if (!family || !source) continue;
        out.push({
          family,
          source,
          descriptors: {
            style: declarations['font-style'] || 'normal',
            weight: declarations['font-weight'] || 'normal',
            stretch: declarations['font-stretch'] || 'normal',
            unicodeRange: declarations['unicode-range'] || 'U+0-10FFFF',
            variant: declarations['font-variant'] || 'normal',
            featureSettings: declarations['font-feature-settings'] || 'normal',
            variationSettings: declarations['font-variation-settings'] || 'normal',
            display: declarations['font-display'] || 'auto',
            ascentOverride: declarations['ascent-override'] || 'normal',
            descentOverride: declarations['descent-override'] || 'normal',
            lineGapOverride: declarations['line-gap-override'] || 'normal'
          }
        });
      }
    }
    return out;
  };

  globalThis.FontFace = class FontFace {
    constructor(family, source, descriptors={}) {
      if (arguments.length < 2) throw new TypeError('FontFace requires family and source');
      this._sets = new Set();
      this._family = _fontFaceString(family);
      if (!this._family.trim()) throw new DOMException('The font family is empty', 'SyntaxError');
      const normalizedSource = _fontFaceSource(source);
      this._source = normalizedSource.css;
      this._style = _fontFaceDescriptor(descriptors, 'style', 'normal');
      this._weight = _fontFaceDescriptor(descriptors, 'weight', 'normal');
      this._stretch = _fontFaceDescriptor(descriptors, 'stretch', 'normal');
      this._unicodeRange = _fontFaceDescriptor(descriptors, 'unicodeRange', 'U+0-10FFFF');
      this._variant = _fontFaceDescriptor(descriptors, 'variant', 'normal');
      this._featureSettings = _fontFaceDescriptor(descriptors, 'featureSettings', 'normal');
      this._variationSettings = _fontFaceDescriptor(descriptors, 'variationSettings', 'normal');
      this._display = _fontFaceDescriptor(descriptors, 'display', 'auto');
      this._ascentOverride = _fontFaceDescriptor(descriptors, 'ascentOverride', 'normal');
      this._descentOverride = _fontFaceDescriptor(descriptors, 'descentOverride', 'normal');
      this._lineGapOverride = _fontFaceDescriptor(descriptors, 'lineGapOverride', 'normal');
      this._status = normalizedSource.binary ? 'loaded' : 'unloaded';
      this._loadedPromise = normalizedSource.binary ? Promise.resolve(this) : null;
    }
    _changed() { for (const set of this._sets) set._faceChanged(this); }
    _setDescriptor(slot, value) {
      this[slot] = String(value);
      this._changed();
    }
    get family() { return this._family; }
    set family(value) { this._setDescriptor('_family', value); }
    get style() { return this._style; }
    set style(value) { this._setDescriptor('_style', value); }
    get weight() { return this._weight; }
    set weight(value) { this._setDescriptor('_weight', value); }
    get stretch() { return this._stretch; }
    set stretch(value) { this._setDescriptor('_stretch', value); }
    get unicodeRange() { return this._unicodeRange; }
    set unicodeRange(value) { this._setDescriptor('_unicodeRange', value); }
    get variant() { return this._variant; }
    set variant(value) { this._setDescriptor('_variant', value); }
    get featureSettings() { return this._featureSettings; }
    set featureSettings(value) { this._setDescriptor('_featureSettings', value); }
    get variationSettings() { return this._variationSettings; }
    set variationSettings(value) { this._setDescriptor('_variationSettings', value); }
    get display() { return this._display; }
    set display(value) { this._setDescriptor('_display', value); }
    get ascentOverride() { return this._ascentOverride; }
    set ascentOverride(value) { this._setDescriptor('_ascentOverride', value); }
    get descentOverride() { return this._descentOverride; }
    set descentOverride(value) { this._setDescriptor('_descentOverride', value); }
    get lineGapOverride() { return this._lineGapOverride; }
    set lineGapOverride(value) { this._setDescriptor('_lineGapOverride', value); }
    get status() { return this._status; }
    get loaded() {
      if (!this._loadedPromise) {
        this._loadedPromise = new Promise((resolve, reject) => {
          this._resolveLoaded = resolve;
          this._rejectLoaded = reject;
        });
      }
      return this._loadedPromise;
    }
    load() {
      if (this._status === 'loaded') return this.loaded;
      if (this._status === 'loading') return this.loaded;
      this._status = 'loading';
      this._changed();
      const loaded = this.loaded;
      Promise.resolve().then(() => {
        if (this._status !== 'loading') return;
        this._status = 'loaded';
        this._resolveLoaded?.(this);
        this._changed();
      });
      return loaded;
    }
  };

  const _fontFaceSelection = font => {
    const value = String(font);
    const size = /(?:^|\s)(?:\d*\.?\d+)(?:px|pt|pc|in|cm|mm|q|em|rem|ex|ch|vw|vh|vmin|vmax|%)(?:\s*\/\s*[^\s]+)?\s+(.+)$/i.exec(value);
    if (!size) throw new DOMException('Invalid font shorthand', 'SyntaxError');
    const family = size[1].split(',')[0].trim().replace(/^(['"])(.*)\1$/, '$2').toLowerCase();
    const prefix = value.slice(0, size.index + size[0].length - size[1].length).toLowerCase();
    const weight = /\b(?:[1-9]00|bold)\b/.exec(prefix)?.[0] || 'normal';
    const style = /\b(?:italic|oblique)\b/.exec(prefix)?.[0] || 'normal';
    return { family, weight: weight === 'bold' ? 700 : weight === 'normal' ? 400 : +weight, style };
  };
  const _fontFaceMatches = (face, selection) => {
    if (face.family.trim().replace(/^(['"])(.*)\1$/, '$2').toLowerCase() !== selection.family) return false;
    const faceWeight = face.weight.toLowerCase() === 'bold' ? 700 :
      face.weight.toLowerCase() === 'normal' ? 400 : +(face.weight.split(/\s+/)[0]) || 400;
    const italic = /^(?:italic|oblique)/i.test(face.style);
    return Math.abs(faceWeight - selection.weight) < 350 && italic === (selection.style !== 'normal');
  };

  globalThis.FontFaceSet = class FontFaceSet extends EventTarget {
    constructor(initialFaces=[], ownerDocument=null) {
      super();
      this._faces = new Set();
      this._ownerDocument = ownerDocument;
      this._cssFaces = new Map();
      this._status = 'loaded';
      this._readyPromise = Promise.resolve(this);
      this.onloading = null;
      this.onloadingdone = null;
      this.onloadingerror = null;
      if (initialFaces != null) for (const face of initialFaces) this.add(face);
    }
    get status() { return this._status; }
    get ready() { return this._readyPromise; }
    get size() { this._discoverCssFaces(); return this._faces.size; }
    _discoverCssFaces() {
      if (!this._ownerDocument) return;
      const retained = new Set();
      for (const rule of _fontFaceAuthoredRules(this._ownerDocument)) {
        const key = JSON.stringify([rule.family, rule.source, rule.descriptors]);
        retained.add(key);
        if (this._cssFaces.has(key)) continue;
        try {
          const face = new FontFace(rule.family, rule.source, rule.descriptors);
          face._cssConnected = true;
          face._sets.add(this);
          this._cssFaces.set(key, face);
          this._faces.add(face);
        } catch (_) {}
      }
      for (const [key, face] of this._cssFaces) {
        if (retained.has(key)) continue;
        face._sets.delete(this);
        this._faces.delete(face);
        this._cssFaces.delete(key);
      }
    }
    _dispatch(type, faces) {
      const event = new Event(type);
      event.fontfaces = faces;
      this.dispatchEvent(event);
      const handler = this['on' + type];
      if (typeof handler === 'function') {
        try { handler.call(this, event); } catch (error) { console.error(error); }
      }
    }
    _syncNative() {
      if (!this._ownerDocument || typeof Deno.core.ops.op_set_dynamic_fonts !== 'function') return;
      const registrations = [];
      for (const face of this._faces) registrations.push({
        ...(face._cssConnected ? { skip: true } : {}),
        family: face.family,
        source: face._source,
        style: face.style,
        weight: face.weight,
        unicodeRange: face.unicodeRange
      });
      Deno.core.ops.op_set_dynamic_fonts(JSON.stringify(registrations.filter(face => !face.skip)));
      _scheduleResizeRenderCheckpoint();
    }
    _faceChanged(face) {
      this._syncNative();
      if (face.status === 'loading' && this._status !== 'loading') {
        this._status = 'loading';
        const pending = Array.from(this._faces).filter(candidate => candidate.status === 'loading');
        this._readyPromise = Promise.all(pending.map(candidate => candidate.loaded)).then(() => {
          this._status = 'loaded';
          this._dispatch('loadingdone', Array.from(this._faces));
          return this;
        });
        this._dispatch('loading', [face]);
      }
    }
    add(face) {
      if (!(face instanceof FontFace)) throw new TypeError('FontFaceSet.add requires a FontFace');
      this._discoverCssFaces();
      if (!this._faces.has(face)) {
        this._faces.add(face);
        face._sets.add(this);
        this._syncNative();
      }
      return this;
    }
    check(font, text=' ') {
      void String(text);
      this._discoverCssFaces();
      const selection = _fontFaceSelection(font);
      const matches = Array.from(this._faces).filter(face => _fontFaceMatches(face, selection));
      return matches.length === 0 || matches.every(face => face.status === 'loaded');
    }
    clear() {
      for (const face of Array.from(this._faces)) {
        if (face._cssConnected) continue;
        face._sets.delete(this);
        this._faces.delete(face);
      }
      this._syncNative();
    }
    delete(face) {
      this._discoverCssFaces();
      if (!(face instanceof FontFace) || face._cssConnected || !this._faces.delete(face)) return false;
      face._sets.delete(this);
      this._syncNative();
      return true;
    }
    load(font, text=' ') {
      void String(text);
      this._discoverCssFaces();
      const selection = _fontFaceSelection(font);
      const matches = Array.from(this._faces).filter(face => _fontFaceMatches(face, selection));
      return Promise.all(matches.map(face => face.load())).then(() => matches);
    }
    forEach(callback, thisArg=undefined) {
      if (typeof callback !== 'function') throw new TypeError('FontFaceSet.forEach callback must be callable');
      this._discoverCssFaces();
      for (const face of this._faces) callback.call(thisArg, face, face, this);
    }
    has(face) { this._discoverCssFaces(); return this._faces.has(face); }
    entries() { this._discoverCssFaces(); return Array.from(this._faces, face => [face, face])[Symbol.iterator](); }
    keys() { this._discoverCssFaces(); return Array.from(this._faces).values(); }
    values() { this._discoverCssFaces(); return Array.from(this._faces).values(); }
    [Symbol.iterator]() { return this.values(); }
  };
  Object.defineProperty(Document.prototype, 'fonts', {
    get() {
      if (!this._fonts) this._fonts = new FontFaceSet([], this);
      return this._fonts;
    },
    configurable: true
  });
}

if (typeof SharedWorker === 'undefined') {
  globalThis.SharedWorker = class SharedWorker {
    constructor() { this.port = { postMessage(){}, onmessage:null, start(){}, close(){}, addEventListener(){}, removeEventListener(){} }; this.onerror = null; }
  };
}
if (typeof ServiceWorkerContainer === 'undefined') {
  globalThis.ServiceWorkerContainer = class { register(){return Promise.resolve();} getRegistrations(){return Promise.resolve([]);} };
}

if (typeof URLPattern === 'undefined') {
  globalThis.URLPattern = class URLPattern {
    constructor(pattern){this._pattern=pattern||{};} test(){return false;} exec(){return null;}
  };
}

if (typeof Document !== 'undefined' && !Document.prototype.importNode) {
  Document.prototype.importNode = function(node, deep) { return node?.cloneNode(!!deep) || null; };
}

// Document.adoptNode: standard DOM (HTML living spec). Frameworks that move
// nodes between documents (portals, iframe hand-off) call it; the missing
// method throws "adoptNode is not a function". With no second document to
// transfer ownership from, the node is already ours, so return it as-is,
// matching the observable effect of adoption into this document.
if (typeof Document !== 'undefined' && !Document.prototype.adoptNode) {
  Document.prototype.adoptNode = function(node) { return node || null; };
}

// Element.toggleAttribute: standard DOM. Lit/Stencil and several ad SDKs call
// it; the missing method throws. Spec semantics: no force arg toggles, force
// true adds, force false removes; returns the new presence.
if (typeof Element !== 'undefined' && !Element.prototype.toggleAttribute) {
  Element.prototype.toggleAttribute = function(name, force) {
    const n = String(name);
    const present = this.hasAttribute(n);
    const want = arguments.length < 2 ? !present : !!force;
    if (want && !present) { this.setAttribute(n, ''); return true; }
    if (!want && present) { this.removeAttribute(n); return false; }
    return want;
  };
}

// Document.elementFromPoint / elementsFromPoint — no layout engine, so this is a stub:
// in-viewport coords return <body> (or <html> as fallback), out-of-viewport returns null.
// Wrong-but-non-throwing beats "undefined", which traps ad/analytics bootstraps in retry loops
// (see issue #63).
if (typeof Document !== 'undefined' && !Document.prototype.elementFromPoint) {
  // Real hit testing against the synthetic bboxes from getBoundingClientRect.
  // Flat iteration over every element, NOT a tree walk: our synthetic rects
  // don't form a proper containment hierarchy (a child's rect can lie far
  // outside its parent's), so a tree walk that only descends into ancestors
  // containing (x,y) would never reach a deep <input> inside <label><p>.
  // Returns the deepest matching element (highest nid wins as a proxy for
  // tree depth) so descendants beat ancestors.
  Document.prototype.elementFromPoint = function(x, y) {
    if (typeof x !== 'number' || typeof y !== 'number' || !isFinite(x) || !isFinite(y)) {
      return null;
    }
    var w = (typeof window !== 'undefined' && window.innerWidth) || 1280;
    var h = (typeof window !== 'undefined' && window.innerHeight) || 720;
    if (x < 0 || y < 0 || x > w || y > h) return null;
    var all = this.querySelectorAll('*');
    var best = null;
    var bestNid = -1;
    for (var i = 0; i < all.length; i++) {
      var el = all[i];
      if (!el || !el.getBoundingClientRect) continue;
      // documentElement / body span the viewport; skip them so we pick a
      // real descendant instead of falling back to <html>/<body>.
      if (el === this.documentElement || el === this.body) continue;
      var r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) continue;
      if (x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) {
        // A descendant's layout rect can extend beyond an overflow clip. It
        // must not win hit testing where its scrolling ancestor hides it —
        // otherwise a wheel well outside a small pane scrolls that pane
        // instead of the page behind it.
        var visible = true;
        var ancestor = el.parentElement;
        while (ancestor && ancestor !== this.documentElement && ancestor !== this.body) {
          var style = null;
          try { style = getComputedStyle(ancestor); } catch (_e) {}
          var ox = style ? (style.overflowX || style.overflow || '') : '';
          var oy = style ? (style.overflowY || style.overflow || '') : '';
          var clipsX = ox === 'auto' || ox === 'scroll' || ox === 'hidden' || ox === 'clip';
          var clipsY = oy === 'auto' || oy === 'scroll' || oy === 'hidden' || oy === 'clip';
          if (clipsX || clipsY) {
            var ar = ancestor.getBoundingClientRect();
            // Overflow clips at the padding box, inside the border. Renderer
            // client metrics expose that box's size; computed border widths
            // locate it within the border-box rect.
            var borderLeft = parseFloat(style && style.borderLeftWidth) || 0;
            var borderTop = parseFloat(style && style.borderTopWidth) || 0;
            var clipLeft = ar.left + borderLeft;
            var clipTop = ar.top + borderTop;
            var clipRight = clipLeft + ancestor.clientWidth;
            var clipBottom = clipTop + ancestor.clientHeight;
            if ((clipsX && (x < clipLeft || x > clipRight)) ||
                (clipsY && (y < clipTop || y > clipBottom))) {
              visible = false;
              break;
            }
          }
          ancestor = ancestor.parentElement;
        }
        if (!visible) continue;
        var nid = el._nid | 0;
        if (nid > bestNid) { best = el; bestNid = nid; }
      }
    }
    return best || this.body || this.documentElement || null;
  };
  Document.prototype.elementsFromPoint = function(x, y) {
    var el = this.elementFromPoint(x, y);
    return el ? [el] : [];
  };
}
if (typeof ShadowRoot !== 'undefined' && !ShadowRoot.prototype.elementFromPoint) {
  ShadowRoot.prototype.elementFromPoint = function(x, y) {
    return Document.prototype.elementFromPoint.call(globalThis.document || this, x, y);
  };
  ShadowRoot.prototype.elementsFromPoint = function(x, y) {
    return Document.prototype.elementsFromPoint.call(globalThis.document || this, x, y);
  };
}

globalThis.__obscura_init = function() {
  // The host sets __obscura_frameId on a frame realm before calling this.
  _realmFrameId = globalThis.__obscura_frameId >>> 0;
  _fpSeed = Date.now() ^ (Math.random() * 0xFFFFFFFF >>> 0);
  _fpCache = null;
  // A real navigation just completed (this runs after set_url), so drop any
  // URL a location setter previewed synchronously and let document_url drive
  // location.href again, including any redirect target.
  globalThis.__virtualUrl = null;
  _installWasmStreamingFallback();

  const documentNid = +_dom("document_node_id");
  globalThis.document = new Document(documentNid);
  // parentNode on <html> reaches the backing document node. Keep that wrapper
  // canonical so getRootNode(), isConnected, and identity comparisons return
  // the same Document object exposed as globalThis.document.
  _cache.set(documentNid, globalThis.document);
  const previousWindowNames = new Set(_windowNamedPropertyNames);
  _registerWindowNamedTree(globalThis.document.documentElement);
  _reconcileWindowNamedProperties(previousWindowNames);

  const scr = _fp('screen');
  const sw = Number.isFinite(globalThis.__obscura_screen_w) && globalThis.__obscura_screen_w > 0
    ? globalThis.__obscura_screen_w : scr[0];
  const sh = Number.isFinite(globalThis.__obscura_screen_h) && globalThis.__obscura_screen_h > 0
    ? globalThis.__obscura_screen_h : scr[1];
  // The OS screen and the page viewport are different browser concepts.
  // Keep the fingerprinted screen, but let the embedding browser provide the
  // actual CSS viewport so responsive JavaScript, layout, and screenshots all
  // observe the same dimensions.
  const vw = Number.isFinite(globalThis.__obscura_viewport_w) && globalThis.__obscura_viewport_w > 0
    ? globalThis.__obscura_viewport_w : sw;
  const vh = Number.isFinite(globalThis.__obscura_viewport_h) && globalThis.__obscura_viewport_h > 0
    ? globalThis.__obscura_viewport_h : sh - 80;
  _applyScreenSize(sw, sh, !!globalThis.__obscura_screen_emulated);
  globalThis.visualViewport = { width:vw, height:vh, offsetLeft:0, offsetTop:0, scale:1, addEventListener(){}, removeEventListener(){} };
  // Screen dimensions do not determine the output device scale. The embedding
  // browser applies an explicit device metric after page initialization; the
  // standalone runtime has the same 1x default as Obscura's render surface.
  globalThis.devicePixelRatio = 1;
  globalThis.innerWidth = vw; globalThis.innerHeight = vh;
  globalThis.outerWidth = sw; globalThis.outerHeight = sh - 40;

  var hwValues = globalThis.__obscura_stealth ? [4, 6, 8, 12, 16] : [2, 4, 6, 8, 12, 16];
  globalThis.__obscura_hw = hwValues[Math.floor(_fpRand(400) * hwValues.length)];
  var memValues = globalThis.__obscura_stealth ? [4, 8] : [0.25, 0.5, 1, 2, 4, 8];
  globalThis.__obscura_mem = memValues[Math.floor(_fpRand(401) * memValues.length)];

  const t0 = Date.now() + Math.floor(_fpRand(641) * 100) - 50;
  globalThis.performance.timeOrigin = t0;
  globalThis.performance.timing = { navigationStart: t0, domContentLoadedEventEnd: t0, loadEventEnd: t0 };
  var _totalHeap = 15000000 + Math.floor(_fpRand(620) * 85000000);
  globalThis.performance.memory = {
    jsHeapSizeLimit: 4294705152,
    totalJSHeapSize: _totalHeap,
    usedJSHeapSize: Math.floor(_totalHeap * (0.3 + _fpRand(621) * 0.5)),
  };
  globalThis.Notification.permission = "default";

  // userAgentData brands and getHighEntropyValues now derive the Chrome
  // version from navigator.userAgent and read the platform from the page
  // globals, so every stealth surface agrees without a per-mode override.

  // Before any of this document's own scripts run: `parent === window` is how
  // a document decides it is top-level, and one script taking that branch
  // wrongly changes everything after it.
  _installFramingRelationships();

  // A parser-created <iframe src> never went through the src setter, so
  // nothing had started its load and the frame stayed empty (issue #600).
  // This also runs inside a frame realm, so a frame nested in a frame loads
  // by the same path, with op_frame_document_ready recording the caller as
  // its parent.
  for (const frame of globalThis.document.querySelectorAll('iframe')) {
    const src = frame.getAttribute('src');
    if (src && src !== 'about:blank') frame._loadIframeSrc(src);
  }

  // Hide internals (_*, obscura, Obscura). The set of keys is static at
  // snapshot-build time, so we precompute it ONCE below (after this
  // function definition) and reuse it on every page init. Was an
  // Object.keys + filter on every navigation, ~5-40ms per page on
  // SPAs that load 1000+ globals.
  const toHide = globalThis.__obscura_hide_list || [];
  for (let i = 0; i < toHide.length; i++) {
    try { Object.defineProperty(globalThis, toHide[i], { enumerable: false }); } catch(e) {}
  }
  delete globalThis.__obscura_init;
};

// Snapshot-time pre-computation of the hide list. Bootstrap.js runs once
// during the V8 snapshot build (build.rs); this line captures the set of
// globals defined by bootstrap that we want to hide and stashes them
// for __obscura_init to consume on every subsequent page. The snapshot
// preserves the array as a regular global.
// Use getOwnPropertyNames, not Object.keys: the internal globals declared by
// _preHideInternals are already non-enumerable, so Object.keys would omit them
// and leave them out of the hide list (and thus visible to the reflection-API
// filter and to fingerprinting scripts). getOwnPropertyNames captures them.
globalThis.__obscura_hide_list = Object.getOwnPropertyNames(globalThis).filter(k =>
  k.startsWith('_') || k.includes('obscura') || k.includes('Obscura')
);

/* ===== WPT conformance shims: batch 2 ===== */

// ---- Node namespace lookup methods ----

Node.prototype.lookupNamespaceURI = function(prefix) {
  let node = this;
  if (node.nodeType === 9) node = node.documentElement;
  if (!node || node.nodeType !== 1) return null;
  const _ns_builtins = { 'xml': 'http://www.w3.org/XML/1998/namespace', 'xmlns': 'http://www.w3.org/2000/xmlns/' };
  if (prefix && _ns_builtins[prefix]) return _ns_builtins[prefix];
  while (node && node.nodeType === 1) {
    if (prefix) {
      if (node.prefix === prefix && node.namespaceURI) return node.namespaceURI;
      const nsAttr = node.getAttribute('xmlns:' + prefix);
      if (nsAttr !== null) return nsAttr || null;
    } else {
      const defaultNs = node.getAttribute('xmlns');
      if (defaultNs !== null) return defaultNs || null;
      if (node.prefix === null && node.namespaceURI) return node.namespaceURI;
    }
    node = node.parentElement;
  }
  return null;
};
_markNative(Node.prototype.lookupNamespaceURI);

Node.prototype.lookupPrefix = function(namespace) {
  namespace = namespace || null;
  let node = this;
  if (node.nodeType === 9) node = node.documentElement;
  if (!node || node.nodeType !== 1) return null;
  const _ns_builtins = { 'http://www.w3.org/XML/1998/namespace': 'xml', 'http://www.w3.org/2000/xmlns/': 'xmlns' };
  if (_ns_builtins[namespace]) return _ns_builtins[namespace];
  while (node && node.nodeType === 1) {
    if (node.namespaceURI === namespace) {
      const p = node.prefix;
      if (p) return p;
    }
    const attrs = node.attributes || [];
    for (let i = 0; i < attrs.length; i++) {
      const attr = attrs[i];
      const attrName = attr.name || attr.nodeName || '';
      const attrValue = attr.value || attr.nodeValue || '';
      if (attrName === 'xmlns' && attrValue === namespace) return '';
      if (attrName.startsWith('xmlns:')) {
        const prefix = attrName.substring(6);
        if (attrValue === namespace) return prefix;
      }
    }
    node = node.parentElement;
  }
  return null;
};
_markNative(Node.prototype.lookupPrefix);

Node.prototype.isDefaultNamespace = function(namespace) {
  return this.lookupNamespaceURI(null) === (namespace || null);
};
_markNative(Node.prototype.isDefaultNamespace);


// ---- getElementsByTagNameNS on Element and Document ----
// getElementsByTagNameNS on Element and Document
if (!Element.prototype.getElementsByTagNameNS) {
  Element.prototype.getElementsByTagNameNS = function(namespaceURI, localName) {
    const all = this.querySelectorAll('*');
    const filtered = [];
    const nsMatch = namespaceURI === '*';
    const tagMatch = localName === '*';
    for (let i = 0; i < all.length; i++) {
      const el = all[i];
      if (!el) continue;
      const elNs = el.namespaceURI;
      const elTag = el.localName;
      const nsOk = nsMatch || (elNs === (namespaceURI || null));
      const tagOk = tagMatch || (elTag === localName);
      if (nsOk && tagOk) filtered.push(el);
    }
    const result = new HTMLCollection(...filtered);
    result.item = (i) => result[i] != null ? result[i] : null;
    return result;
  };
  _markNative(Element.prototype.getElementsByTagNameNS);
}
if (!Document.prototype.getElementsByTagNameNS) {
  Document.prototype.getElementsByTagNameNS = function(namespaceURI, localName) {
    const all = this.querySelectorAll('*');
    const filtered = [];
    const nsMatch = namespaceURI === '*';
    const tagMatch = localName === '*';
    for (let i = 0; i < all.length; i++) {
      const el = all[i];
      if (!el) continue;
      const elNs = el.namespaceURI;
      const elTag = el.localName;
      const nsOk = nsMatch || (elNs === (namespaceURI || null));
      const tagOk = tagMatch || (elTag === localName);
      if (nsOk && tagOk) filtered.push(el);
    }
    const result = new HTMLCollection(...filtered);
    result.item = (i) => result[i] != null ? result[i] : null;
    return result;
  };
  _markNative(Document.prototype.getElementsByTagNameNS);
}

// ---- Attr nodes and createAttribute ----
// Attr class: represents attribute nodes (nodeType 2)
if (!globalThis.Attr) {
  globalThis.Attr = class Attr {
    constructor(name, value = '', namespaceURI = null, prefix = null) {
      this.name = name;
      this.localName = name;
      this.value = value;
      this.namespaceURI = namespaceURI;
      this.prefix = prefix;
      this.ownerElement = null;
      this.specified = true;
    }
    get nodeName() { return this.name; }
    get nodeValue() { return this.value; }
    set nodeValue(v) { this.value = v; }
    get nodeType() { return 2; }
  };
}

// XML Name validation helper for attribute/processing instruction names
const _ns_isValidXmlName = (name) => {
  if (typeof name !== 'string' || !name.length) return false;
  return /^[A-Za-z_:][\w.\-:]*$/.test(name);
};

const _ns_validateQualifiedName = (namespaceURI, qualifiedName) => {
  const parts = qualifiedName.split(':');
  if (parts.length > 2 || parts.some((part) => !_ns_isValidXmlName(part))) {
    throw new DOMException('Invalid attribute name', 'InvalidCharacterError');
  }
  const prefix = parts.length === 2 ? parts[0] : null;
  const XML = 'http://www.w3.org/XML/1998/namespace';
  const XMLNS = 'http://www.w3.org/2000/xmlns/';
  if ((prefix && !namespaceURI)
      || (prefix === 'xml' && namespaceURI !== XML)
      || ((qualifiedName === 'xmlns' || prefix === 'xmlns') && namespaceURI !== XMLNS)
      || (namespaceURI === XMLNS && qualifiedName !== 'xmlns' && prefix !== 'xmlns')) {
    throw new DOMException('The namespace is invalid', 'NamespaceError');
  }
};

// Document.prototype.createAttribute: create a detached Attr node
if (!Document.prototype.createAttribute) {
  Document.prototype.createAttribute = function(localName) {
    const name = String(localName || '');
    if (!_ns_isValidXmlName(name)) {
      throw new DOMException('Invalid attribute name', 'InvalidCharacterError');
    }
    return new Attr(name, '', null, null);
  };
  _markNative(Document.prototype.createAttribute);
}

// Document.prototype.createAttributeNS: create a namespaced Attr node
if (!Document.prototype.createAttributeNS) {
  Document.prototype.createAttributeNS = function(namespaceURI, qualifiedName) {
    const ns = namespaceURI ? String(namespaceURI) : null;
    const qn = String(qualifiedName || '');
    if (!qn.length) {
      throw new DOMException('Invalid attribute name', 'InvalidCharacterError');
    }
    let prefix = null;
    let localName = qn;
    const colonIdx = qn.indexOf(':');
    if (colonIdx !== -1) {
      prefix = qn.substring(0, colonIdx);
      localName = qn.substring(colonIdx + 1);
      if (!_ns_isValidXmlName(prefix) || !_ns_isValidXmlName(localName)) {
        throw new DOMException('Invalid attribute name', 'InvalidCharacterError');
      }
    } else {
      if (!_ns_isValidXmlName(localName)) {
        throw new DOMException('Invalid attribute name', 'InvalidCharacterError');
      }
    }
    return new Attr(qn, '', ns, prefix);
  };
  _markNative(Document.prototype.createAttributeNS);
}

// Element.prototype.getAttributeNode: return an Attr node or null
if (!Element.prototype.getAttributeNode) {
  Element.prototype.getAttributeNode = function(name) {
    const val = this.getAttribute(name);
    if (val === null) return null;
    const attr = new Attr(name, val, null, null);
    attr.ownerElement = this;
    return attr;
  };
  _markNative(Element.prototype.getAttributeNode);
}

// Element.prototype.getAttributeNodeNS: return a namespaced Attr node or null
if (!Element.prototype.getAttributeNodeNS) {
  Element.prototype.getAttributeNodeNS = function(namespaceURI, localName) {
    const val = this.getAttributeNS(namespaceURI, localName);
    if (val === null) return null;
    const name = String(localName || '');
    const attr = new Attr(name, val, namespaceURI ? String(namespaceURI) : null, null);
    attr.ownerElement = this;
    return attr;
  };
  _markNative(Element.prototype.getAttributeNodeNS);
}

// Element.prototype.setAttributeNode: set an Attr and return the previous one
if (!Element.prototype.setAttributeNode) {
  Element.prototype.setAttributeNode = function(attr) {
    if (!attr || typeof attr.name !== 'string') return null;
    const prevVal = this.getAttribute(attr.name);
    const prevAttr = prevVal !== null ? new Attr(attr.name, prevVal, null, null) : null;
    if (prevAttr) prevAttr.ownerElement = this;
    this.setAttribute(attr.name, attr.value);
    attr.ownerElement = this;
    return prevAttr;
  };
  _markNative(Element.prototype.setAttributeNode);
}

// Element.prototype.setAttributeNodeNS: set a namespaced Attr and return the previous one
if (!Element.prototype.setAttributeNodeNS) {
  Element.prototype.setAttributeNodeNS = function(attr) {
    if (!attr || typeof attr.name !== 'string') return null;
    const prevVal = this.getAttribute(attr.name);
    const prevAttr = prevVal !== null 
      ? new Attr(attr.name, prevVal, attr.namespaceURI || null, attr.prefix || null) 
      : null;
    if (prevAttr) prevAttr.ownerElement = this;
    this.setAttributeNS(attr.namespaceURI || null, attr.name, attr.value);
    attr.ownerElement = this;
    return prevAttr;
  };
  _markNative(Element.prototype.setAttributeNodeNS);
}

// Element.prototype.removeAttributeNode: remove and return an Attr
if (!Element.prototype.removeAttributeNode) {
  Element.prototype.removeAttributeNode = function(attr) {
    if (!attr || typeof attr.name !== 'string') return attr;
    const val = this.getAttribute(attr.name);
    if (val !== null) {
      this.removeAttribute(attr.name);
    }
    return attr;
  };
  _markNative(Element.prototype.removeAttributeNode);
}


// ---- form control validity and text selection ----

// ValidityState class for form validation state reporting
if (typeof ValidityState === 'undefined') {
  globalThis.ValidityState = class ValidityState {
    constructor() {
      this.badInput = false;
      this.customError = false;
      this.patternMismatch = false;
      this.rangeOverflow = false;
      this.rangeUnderflow = false;
      this.stepMismatch = false;
      this.tooLong = false;
      this.tooShort = false;
      this.typeMismatch = false;
      this.valueMissing = false;
      this.valid = true;
    }
  };
}

// Validity and validation message storage on elements
const _ns_validityCache = new WeakMap();
const _ns_customValidityMsg = new WeakMap();

// Element.prototype.validity - returns cached ValidityState for the element
if (!Element.prototype.validity) {
  Object.defineProperty(Element.prototype, 'validity', {
    get: function() {
      if (!_ns_validityCache.has(this)) {
        _ns_validityCache.set(this, new ValidityState());
      }
      return _ns_validityCache.get(this);
    },
    enumerable: true,
    configurable: true
  });
}

// Element.prototype.willValidate - whether element is subject to constraint validation
if (!Element.prototype.willValidate) {
  Object.defineProperty(Element.prototype, 'willValidate', {
    get: function() {
      return true;
    },
    enumerable: true,
    configurable: true
  });
}

// Element.prototype.validationMessage - custom validation message if set
if (!Element.prototype.validationMessage) {
  Object.defineProperty(Element.prototype, 'validationMessage', {
    get: function() {
      return _ns_customValidityMsg.get(this) || '';
    },
    enumerable: true,
    configurable: true
  });
}

// Element.prototype.checkValidity - stub returns true
if (!Element.prototype.checkValidity) {
  Element.prototype.checkValidity = function checkValidity() {
    return true;
  };
  _markNative(Element.prototype.checkValidity);
}

// Element.prototype.reportValidity - stub returns true
if (!Element.prototype.reportValidity) {
  Element.prototype.reportValidity = function reportValidity() {
    return true;
  };
  _markNative(Element.prototype.reportValidity);
}

// Element.prototype.setCustomValidity - set custom validation message
if (!Element.prototype.setCustomValidity) {
  Element.prototype.setCustomValidity = function setCustomValidity(msg) {
    const validity = this.validity;
    if (msg && msg.length > 0) {
      _ns_customValidityMsg.set(this, msg);
      validity.customError = true;
      validity.valid = false;
    } else {
      _ns_customValidityMsg.delete(this);
      validity.customError = false;
      validity.valid = true;
    }
  };
  _markNative(Element.prototype.setCustomValidity);
}

// Text selection on Element.prototype
const _ns_selectionStart = new WeakMap();
const _ns_selectionEnd = new WeakMap();
const _ns_selectionDir = new WeakMap();

// Element.prototype.selectionStart - get/set selection start position
if (!Element.prototype.selectionStart) {
  Object.defineProperty(Element.prototype, 'selectionStart', {
    get: function() {
      return _ns_selectionStart.get(this) ?? null;
    },
    set: function(v) {
      _ns_selectionStart.set(this, v == null ? null : Math.max(0, parseInt(v, 10) || 0));
    },
    enumerable: true,
    configurable: true
  });
}

// Element.prototype.selectionEnd - get/set selection end position
if (!Element.prototype.selectionEnd) {
  Object.defineProperty(Element.prototype, 'selectionEnd', {
    get: function() {
      return _ns_selectionEnd.get(this) ?? null;
    },
    set: function(v) {
      _ns_selectionEnd.set(this, v == null ? null : Math.max(0, parseInt(v, 10) || 0));
    },
    enumerable: true,
    configurable: true
  });
}

// Element.prototype.selectionDirection - get/set selection direction
if (!Element.prototype.selectionDirection) {
  Object.defineProperty(Element.prototype, 'selectionDirection', {
    get: function() {
      return _ns_selectionDir.get(this) ?? 'none';
    },
    set: function(v) {
      _ns_selectionDir.set(this, v === 'forward' || v === 'backward' ? v : 'none');
    },
    enumerable: true,
    configurable: true
  });
}

// Element.prototype.setSelectionRange - set text selection range
if (!Element.prototype.setSelectionRange) {
  Element.prototype.setSelectionRange = function setSelectionRange(start, end, direction) {
    start = Math.max(0, parseInt(start, 10) || 0);
    end = Math.max(0, parseInt(end, 10) || 0);
    direction = direction === 'forward' || direction === 'backward' ? direction : 'none';
    _ns_selectionStart.set(this, start);
    _ns_selectionEnd.set(this, end);
    _ns_selectionDir.set(this, direction);
  };
  _markNative(Element.prototype.setSelectionRange);
}

// Element.prototype.setRangeText - replace selection with text
if (!Element.prototype.setRangeText) {
  Element.prototype.setRangeText = function setRangeText(replacement, start, end, selectMode) {
    const val = this.value;
    if (!val) return;
    const strVal = String(val);
    start = start === undefined ? (this.selectionStart ?? 0) : Math.max(0, parseInt(start, 10) || 0);
    end = end === undefined ? (this.selectionEnd ?? 0) : Math.max(0, parseInt(end, 10) || 0);
    const newValue = strVal.slice(0, start) + String(replacement) + strVal.slice(end);
    this.value = newValue;
    selectMode = selectMode || 'preserve';
    if (selectMode === 'select') {
      const replLen = String(replacement).length;
      _ns_selectionStart.set(this, start);
      _ns_selectionEnd.set(this, start + replLen);
      _ns_selectionDir.set(this, 'none');
    } else if (selectMode === 'start') {
      _ns_selectionStart.set(this, start);
      _ns_selectionEnd.set(this, start);
      _ns_selectionDir.set(this, 'none');
    } else if (selectMode === 'end') {
      const replLen = String(replacement).length;
      _ns_selectionStart.set(this, start + replLen);
      _ns_selectionEnd.set(this, start + replLen);
      _ns_selectionDir.set(this, 'none');
    }
  };
  _markNative(Element.prototype.setRangeText);
}

// Element.prototype.select - select all text in the element
if (!Element.prototype.select) {
  Element.prototype.select = function select() {
    const val = this.value;
    if (val === undefined || val === null) return;
    const len = String(val).length;
    _ns_selectionStart.set(this, 0);
    _ns_selectionEnd.set(this, len);
    _ns_selectionDir.set(this, 'none');
  };
  _markNative(Element.prototype.select);
}


// ---- Response.blob() on the real fetch path ----

if (typeof Response !== 'undefined' && Response.prototype && !Response.prototype.blob) {
  Response.prototype.blob = async function() {
    const bytes = await this.arrayBuffer();
    const contentType = this.headers && typeof this.headers.get === 'function' ? this.headers.get('content-type') : '';
    return new Blob([new Uint8Array(bytes)], { type: contentType || '' });
  };
  _markNative(Response.prototype.blob);
}
if (typeof Response !== 'undefined' && Response.prototype && !Response.prototype.text) {
  Response.prototype.text = async function() {
    const buffer = await this.arrayBuffer();
    return new TextDecoder().decode(new Uint8Array(buffer));
  };
  _markNative(Response.prototype.text);
}
if (typeof Response !== 'undefined' && Response.prototype && !Response.prototype.json) {
  Response.prototype.json = async function() {
    return JSON.parse(await this.text());
  };
  _markNative(Response.prototype.json);
}
// arrayBuffer is the body primitive that blob/text/json derive from; the
// engine's Response provides it natively, so it is intentionally not shimmed
// here (a JS fallback could only recurse into itself).

// tamperedFunctions: obscura reimplements much of the DOM/Web platform in JS.
// Real Chrome reports "[native code]" from toString() for every builtin method,
// accessor, and constructor; any JS-backed member that leaks its source is a
// detection tell (pixelscan's tamperedFunctions check flags e.g.
// Element.prototype.nodeType, whose getter returned "get nodeType() {...}").
// Individual _markNative calls throughout this file cover methods but miss the
// property accessors and several constructors. Sweep every builtin constructor
// reachable from the global object and mark its prototype members (methods and
// accessors) plus the constructor itself native. This runs once at snapshot
// build time, so it costs nothing per page, and genuinely-native V8 builtins
// already report native, so only the JS-backed members are affected.
(function _collectIframeRealmGlobals() {
  const standardGlobals = [
    'Infinity', 'NaN', 'undefined',
    'eval', 'isFinite', 'isNaN', 'parseFloat', 'parseInt',
    'decodeURI', 'decodeURIComponent', 'encodeURI', 'encodeURIComponent',
    'escape', 'unescape',
    'Atomics', 'Intl', 'JSON', 'Math', 'Reflect', 'WebAssembly',
    'atob', 'btoa', 'queueMicrotask', 'reportError', 'structuredClone',
  ];
  const constructors = Object.getOwnPropertyNames(globalThis).filter(name => {
    if (!/^[A-Z]/.test(name)) return false;
    try { return typeof globalThis[name] === 'function'; }
    catch (e) { return false; }
  });
  _iframeRealmGlobalNames = Array.from(new Set(constructors.concat(standardGlobals)))
    .filter(name => name in globalThis);
  _iframeRealmGlobalNameSet = new Set(_iframeRealmGlobalNames);
})();

(function _markBuiltinsNative() {
  var seen = new Set();
  function walk(ctor) {
    if (typeof ctor !== 'function') { return; }
    _markNative(ctor);
    var proto = ctor.prototype;
    if (!proto || seen.has(proto)) { return; }
    seen.add(proto);
    var keys = Object.getOwnPropertyNames(proto);
    for (var i = 0; i < keys.length; i++) {
      var key = keys[i];
      var d;
      try { d = Object.getOwnPropertyDescriptor(proto, key); } catch (e) { continue; }
      if (!d) { continue; }
      if (typeof d.value === 'function') { _markNative(d.value); }
      if (typeof d.get === 'function') { _markNativeAs(d.get, 'function get ' + key + '() { [native code] }'); }
      if (typeof d.set === 'function') { _markNativeAs(d.set, 'function set ' + key + '() { [native code] }'); }
    }
  }
  var names = Object.getOwnPropertyNames(globalThis);
  for (var i = 0; i < names.length; i++) {
    var name = names[i];
    if (!/^[A-Z]/.test(name)) { continue; }
    var val;
    try { val = globalThis[name]; } catch (e) { continue; }
    if (typeof val === 'function') { walk(val); }
  }
})();

})();
