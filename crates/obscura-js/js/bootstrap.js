"use strict";

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

const _dom = (cmd, a1, a2) => Deno.core.ops.op_dom(cmd, String(a1 ?? ""), String(a2 ?? ""));

const _nativeFns = new Set();
const _origToString = Function.prototype.toString;
Function.prototype.toString = function() {
  if (_nativeFns.has(this)) {
    return `function ${this.name || ''}() { [native code] }`;
  }
  return _origToString.call(this);
};
function _markNative(fn) { if (typeof fn === 'function') _nativeFns.add(fn); return fn; }
_nativeFns.add(Function.prototype.toString);

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
  const gpuPool = [
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
  const gpuVendorPool = [
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
    if (a instanceof Error) return a.stack || a.message || String(a);
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

const _scheduleAfter = (delay, fn) => {
  const d = Math.max(0, Number(delay) || 0);
  if (d === 0) Promise.resolve().then(fn);
  else Deno.core.ops.op_sleep(d).then(fn);
};

globalThis.setTimeout = (fn, delay = 0, ...args) => {
  if (typeof fn !== "function") return ++_tid;
  const id = ++_tid;
  _scheduleAfter(delay, () => {
    if (_clearedTimers.has(id)) return;
    try { fn(...args); } catch(e) { console.error("Timer error:", e); }
  });
  return id;
};

globalThis.clearTimeout = (id) => { _clearedTimers.add(id); };

globalThis.setInterval = (fn, delay = 0, ...args) => {
  if (typeof fn !== "function") return ++_tid;
  const id = ++_tid;
  _intervals.add(id);
  const tick = () => {
    if (!_intervals.has(id)) return;
    try { fn(...args); } catch(e) { console.error("Interval error:", e); }
    if (!_intervals.has(id)) return;
    _scheduleAfter(delay, tick);
  };
  _scheduleAfter(delay, tick);
  return id;
};

globalThis.clearInterval = (id) => { _intervals.delete(id); _clearedTimers.add(id); };
globalThis.requestAnimationFrame = (fn) => setTimeout(fn, 0);
globalThis.cancelAnimationFrame = globalThis.clearTimeout;
globalThis.queueMicrotask = globalThis.queueMicrotask || ((fn) => Promise.resolve().then(fn));

class MessageChannel {
  constructor() {
    this.port1 = { onmessage: null, postMessage: () => {}, close() {}, addEventListener() {}, removeEventListener() {} };
    this.port2 = { onmessage: null, postMessage: () => {}, close() {}, addEventListener() {}, removeEventListener() {} };
    this.port1.postMessage = (data) => {
      Promise.resolve().then(() => { if (this.port2.onmessage) this.port2.onmessage({ data }); });
    };
    this.port2.postMessage = (data) => {
      Promise.resolve().then(() => { if (this.port1.onmessage) this.port1.onmessage({ data }); });
    };
  }
}
globalThis.MessageChannel = MessageChannel;
globalThis.MessagePort = class MessagePort { constructor(){} postMessage(){} close(){} addEventListener(){} removeEventListener(){} };

class CSSStyleDeclaration {
  constructor() { this._props = {}; }
  setProperty(name, value) { this._props[name] = String(value); }
  removeProperty(name) { const old = this._props[name]; delete this._props[name]; return old || ""; }
  getPropertyValue(name) { return this._props[name] || ""; }
  get cssText() { return Object.entries(this._props).map(([k,v]) => `${k}: ${v}`).join("; "); }
  set cssText(v) { this._props = {}; if(v) v.split(";").forEach(p => { const [k,...rest]=p.split(":"); if(k&&rest.length) this._props[k.trim()]=rest.join(":").trim(); }); }
  get length() { return Object.keys(this._props).length; }
  item(i) { return Object.keys(this._props)[i] || ""; }
}

const _styleProxy = (decl) => new Proxy(decl, {
  get(t, p) {
    if (typeof p === "symbol" || p in t) return t[p];
    if (typeof p === "string") return t._props[p] || "";
    return undefined;
  },
  set(t, p, v) {
    if (typeof p === "string") { t._props[p] = String(v); return true; }
    t[p] = v; return true;
  }
});

class Node {
  static ELEMENT_NODE = 1;
  static TEXT_NODE = 3;
  static COMMENT_NODE = 8;
  static DOCUMENT_NODE = 9;
  static DOCUMENT_FRAGMENT_NODE = 11;

  constructor(nid) { this._nid = nid; }
  get nodeType() { return +_dom("node_type", this._nid); }
  get nodeName() { return _domParse("node_name", this._nid) || ""; }
  get ownerDocument() { return globalThis.document; }
  get textContent() { return _domParse("text_content", this._nid) ?? ""; }
  set textContent(v) {
    const oldChildren = _domParse("child_nodes", this._nid) || [];
    for (const c of oldChildren) _dom("remove_child", c);
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
  get parentNode() { return _wrap(+_dom("parent_node", this._nid)); }
  get parentElement() { const p = this.parentNode; return p && p.nodeType === 1 ? p : null; }
  get childNodes() {
    const ids = _domParse("child_nodes", this._nid) || [];
    const list = ids.map(_wrap).filter(Boolean);
    list.item = (i) => list[i] || null;
    return list;
  }
  get firstChild() { return _wrap(+_dom("first_child", this._nid)); }
  get lastChild() { return _wrap(+_dom("last_child", this._nid)); }
  get nextSibling() { return _wrap(+_dom("next_sibling", this._nid)); }
  get previousSibling() { return _wrap(+_dom("prev_sibling", this._nid)); }
  appendChild(c) {
    if (!c) return c;
    _dom("append_child", this._nid, c._nid);
    if (globalThis.__mutationObservers?.length) globalThis.__notifyMutation('childList', this._nid, [c._nid], []);
    if (c instanceof Element && c.tagName === 'SCRIPT') {
      const scriptType = c.getAttribute('type') || '';
      const isModule = scriptType === 'module';
      if (scriptType && !isModule && scriptType !== 'text/javascript' && scriptType !== 'application/javascript') {
        return c;
      }
      const src = c.getAttribute('src');
      const prevNid = globalThis.__currentScriptNid;
      if (src) {
        const fullUrl = src.startsWith('http') || src.startsWith('data:')
          ? src
          : new URL(src, globalThis.location?.href || 'http://localhost/').href;
        const pageOrigin = (function() { try { return new URL(globalThis.location?.href || "about:blank").origin; } catch(e) { return ""; } })();
        (async () => {
          try {
            if (isModule) {
              await import(fullUrl);
            } else {
              const raw = await Deno.core.ops.op_fetch_url(fullUrl, "GET", "{}", "", pageOrigin, "no-cors");
              const parsed = JSON.parse(raw);
              if (parsed.body) {
                globalThis.__currentScriptNid = c._nid;
                try { (0, eval)(parsed.body); }
                catch(e) { console.error('Dynamic script error (' + fullUrl + '):', e.message); }
                finally { globalThis.__currentScriptNid = prevNid || 0; }
              }
            }
            if (typeof c.onload === 'function') try { c.onload(new Event('load')); } catch(e) {}
              try { c.dispatchEvent(new Event('load')); } catch(e) {}
          } catch(e) {
            console.error('Dynamic script fetch error:', e.message);
            if (typeof c.onerror === 'function') try { c.onerror(e); } catch(ex) {}
          }
        })();
      } else {
        const code = c.textContent;
        if (code) {
          if (isModule) {
            const dataUrl = 'data:text/javascript;base64,' + btoa(unescape(encodeURIComponent(code)));
            (async () => {
              try { await import(dataUrl); }
              catch(e) { console.error('Dynamic inline module error:', e.message); }
            })();
          } else {
            globalThis.__currentScriptNid = c._nid;
            try { (0, eval)(code); }
            catch(e) { console.error('Dynamic inline script error:', e.message); }
            finally { globalThis.__currentScriptNid = prevNid || 0; }
          }
        }
      }
    }
    return c;
  }
  removeChild(c) {
    if (!c) return c;
    _dom("remove_child", c._nid);
    if (globalThis.__mutationObservers?.length) globalThis.__notifyMutation('childList', this._nid, [], [c._nid]);
    return c;
  }
  replaceChild(newChild, oldChild) {
    if (!oldChild || !newChild) return oldChild;
    _dom("insert_before", newChild._nid, oldChild._nid);
    _dom("remove_child", oldChild._nid);
    return oldChild;
  }
  insertBefore(n, ref) {
    if (!n) return n;
    if (!ref) { this.appendChild(n); return n; }
    _dom("insert_before", n._nid, ref._nid);
    return n;
  }
  contains(o) { return o ? _dom("contains", this._nid, o._nid) === "true" : false; }
  hasChildNodes() { return _dom("has_child_nodes", this._nid) === "true"; }
  cloneNode(deep) {
    const t = this.nodeType;
    if (t === 1) {
      if (deep) {
        const wrapper = document.createElement('div');
        wrapper.innerHTML = _domParse("outer_html", this._nid) || "";
        const clone = wrapper.firstChild;
        return clone;
      }
      const el = document.createElement(this.nodeName.toLowerCase());
      const html = _domParse("outer_html", this._nid) || "";
      const attrMatch = html.match(/^<[a-zA-Z][^\s>]*([\s\S]*?)>/);
      if (attrMatch && attrMatch[1]) {
        const attrStr = attrMatch[1].trim();
        const re = /([a-zA-Z_:][a-zA-Z0-9_.:-]*)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|(\S+)))?/g;
        let m;
        while ((m = re.exec(attrStr)) !== null) {
          const name = m[1];
          const val = m[2] !== undefined ? m[2] : m[3] !== undefined ? m[3] : m[4] || "";
          if (name !== this.nodeName.toLowerCase()) el.setAttribute(name, val);
        }
      }
      return el;
    }
    if (t === 3) return document.createTextNode(this.textContent);
    if (t === 8) return document.createComment(this.nodeValue || "");
    return null;
  }
  compareDocumentPosition(other) {
    if (!other) return 0;
    if (this._nid === other._nid) return 0;
    if (this.contains(other)) return 16 | 4; // DOCUMENT_POSITION_CONTAINED_BY | FOLLOWING
    if (other.contains && other.contains(this)) return 8 | 2; // DOCUMENT_POSITION_CONTAINS | PRECEDING
    return 4; // DOCUMENT_POSITION_FOLLOWING
  }
  getRootNode() { return globalThis.document; }
  normalize() {} // no-op
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
  addEventListener() {} removeEventListener() {} dispatchEvent() { return true; }
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

class Element extends Node {
  constructor(nid) {
    super(nid);
    this._style = _styleProxy(new CSSStyleDeclaration());
  }
  get tagName() { return _domParse("tag_name", this._nid) || ""; }
  get localName() { return (this.tagName || "").toLowerCase(); }
  get id() { return this.getAttribute("id") || ""; }
  set id(v) { this.setAttribute("id", v); }
  get className() { return this.getAttribute("class") || ""; }
  set className(v) { this.setAttribute("class", v); }
  get namespaceURI() {
    // createElementNS records the requested namespace on _ns; an empty string
    // maps to the null namespace per spec. Elements made via createElement (or
    // parsed) have no _ns: default to XHTML, except <svg> which is SVG.
    if (this._ns !== undefined) return this._ns === "" ? null : this._ns;
    if (this.localName === "svg") return "http://www.w3.org/2000/svg";
    return "http://www.w3.org/1999/xhtml";
  }
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
    let oldChildren = [];
    let newChildren = [];
    if (globalThis.__mutationObservers?.length) {
      oldChildren = _domParse("child_nodes", this._nid) || [];
    }
    _dom("set_inner_html", this._nid, String(v ?? ""));
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
      if (!this._templateContent) this._templateContent = document.createDocumentFragment();
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
    if (!this._relList) this._relList = new DOMTokenList(this, "rel");
    return this._relList;
  }
  get sandbox() {
    if (this.namespaceURI !== "http://www.w3.org/1999/xhtml" || this.localName !== "iframe") return undefined;
    if (!this._sandboxList) this._sandboxList = new DOMTokenList(this, "sandbox");
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
  getAttribute(n) { return _domParse("get_attribute", this._nid, n); }
  setAttribute(n, v) {
    const popoverPrev = (n === "popover" || (typeof n === "string" && n.length === 7 && n.toLowerCase() === "popover")) ? this.popover : undefined;
    _dom("set_attribute", this._nid, n + "\0" + String(v));
    if (popoverPrev !== undefined) this._popoverTypeMaybeChanged(popoverPrev);
    if (globalThis.__mutationObservers?.length) globalThis.__notifyMutation('attributes', this._nid, [], [], n);
  }
  setAttributeNS(ns, n, v) { this.setAttribute(n, v); } // Simplified NS handling
  removeAttribute(n) { const popoverPrev = (n === "popover" || (typeof n === "string" && n.length === 7 && n.toLowerCase() === "popover")) ? this.popover : undefined; _dom("remove_attribute", this._nid, n); if (popoverPrev !== undefined) this._popoverTypeMaybeChanged(popoverPrev); }
  removeAttributeNS(ns, n) { this.removeAttribute(n); }
  hasAttribute(n) { return this.getAttribute(n) !== null; }
  hasAttributes() { return true; } // Simplified
  get attributes() {
    const el = this;
    const names = _domParse("attribute_names", el._nid) || [];
    const list = names.map((name) => ({
      name,
      localName: name,
      value: el.getAttribute(name) ?? "",
      namespaceURI: null,
      prefix: null,
      specified: true,
      ownerElement: el,
      nodeName: name,
      nodeValue: el.getAttribute(name) ?? "",
      nodeType: 2,
    }));
    list.length = names.length;
    list.getNamedItem = (n) => names.includes(n) ? list[names.indexOf(n)] : null;
    list.setNamedItem = (a) => { if (a && a.name) el.setAttribute(a.name, a.value); return a; };
    list.removeNamedItem = (n) => { const a = list.getNamedItem(n); if (a) el.removeAttribute(n); return a; };
    list.item = (i) => list[i] || null;
    for (let i = 0; i < names.length; i++) {
      Object.defineProperty(list, names[i], { value: list[i], configurable: true, enumerable: false });
    }
    return list;
  }
  getAttributeNS(ns, n) { return this.getAttribute(n); }
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
    const parent = this.parentNode;
    if (!parent || !parent.querySelectorAll) return false;
    const matches = parent.querySelectorAll(s);
    for (let i = 0; i < matches.length; i++) {
      if (matches[i]._nid === this._nid) return true;
    }
    return false;
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
    const parent = this.parentNode;
    switch (position) {
      case 'beforebegin':
        if (parent) { const tmp = document.createElement('div'); tmp.innerHTML = html; const children = tmp.childNodes; for (let i = 0; i < children.length; i++) parent.insertBefore(children[i], this); }
        break;
      case 'afterbegin':
        { const tmp = document.createElement('div'); tmp.innerHTML = html; const children = tmp.childNodes; const first = this.firstChild; for (let i = children.length - 1; i >= 0; i--) this.insertBefore(children[i], first); }
        break;
      case 'beforeend':
        { const tmp = document.createElement('div'); tmp.innerHTML = html; const children = tmp.childNodes; for (let i = 0; i < children.length; i++) this.appendChild(children[i]); }
        break;
      case 'afterend':
        if (parent) { const tmp = document.createElement('div'); tmp.innerHTML = html; const children = tmp.childNodes; const next = this.nextSibling; for (let i = 0; i < children.length; i++) parent.insertBefore(children[i], next); }
        break;
    }
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
      const type = (this.getAttribute('type') || '').toLowerCase();
      if (type === 'submit' || (this.localName === 'button' && type !== 'button' && type !== 'reset')) {
        const form = this.closest ? this.closest('form') : null;
        if (form && typeof form.submit === 'function') {
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
    return this.getAttribute("value") || "";
  }
  set value(v) {
    const tag = this.localName;
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
  get checked() {
    if (_formChecked[this._nid] !== undefined) return _formChecked[this._nid];
    return this.hasAttribute("checked");
  }
  set checked(v) { _formChecked[this._nid] = !!v; }
  get selected() {
    if (this._selected !== undefined) return this._selected;
    return this.hasAttribute("selected");
  }
  set selected(v) { this._selected = !!v; }
  get disabled() { return this.hasAttribute("disabled"); }
  set disabled(v) { if (v) this.setAttribute("disabled", ""); else this.removeAttribute("disabled"); }
  get type() { return this.getAttribute("type") || (this.localName === "input" ? "text" : ""); }
  set type(v) { this.setAttribute("type", v); }
  get name() { return this.getAttribute("name") || ""; }
  set name(v) { this.setAttribute("name", v); }
  get placeholder() { return this.getAttribute("placeholder") || ""; }
  set placeholder(v) { this.setAttribute("placeholder", v); }
  get href() { return this.getAttribute("href") || ""; }
  set href(v) { this.setAttribute("href", v); }
  get src() { return this.getAttribute("src") || ""; }
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
    const el = this;
    fetch(fullUrl, {mode: 'no-cors'}).then(async resp => {
      if (resp.ok || resp.type === 'opaque') {
        const html = await resp.text();
        el._iframeDoc = new _IframeDocument(html, fullUrl, el);
        el._iframeWin = new _IframeWindow(el._iframeDoc, fullUrl);
      } else {
        el._iframeDoc = new _IframeDocument('<!DOCTYPE html><html><head></head><body></body></html>', fullUrl, el);
        el._iframeWin = new _IframeWindow(el._iframeDoc, fullUrl);
      }
      _registerIframe(el);
      if (typeof el.onload === 'function') {
        try { el.onload(); } catch(e) {}
      } else {
        var onloadAttr = el.getAttribute('onload');
        if (onloadAttr) try { (0, eval)(onloadAttr); } catch(e) {}
      }
    }).catch(() => {
      el._iframeDoc = new _IframeDocument('<!DOCTYPE html><html><head></head><body></body></html>', fullUrl, el);
      el._iframeWin = new _IframeWindow(el._iframeDoc, fullUrl);
      _registerIframe(el);
      if (typeof el.onload === 'function') try { el.onload(); } catch(e) {}
    });
  }
  get contentDocument() {
    if (this.localName !== 'iframe') return undefined;
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
    if (!this._iframeWin) {
      this.contentDocument; // side effect: creates _iframeDoc + _iframeWin
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
  get selectedIndex() {
    const opts = this.options;
    for (let i = 0; i < opts.length; i++) {
      if (opts[i].selected || opts[i].hasAttribute('selected')) return i;
    }
    return -1;
  }
  set selectedIndex(v) {
    const opts = this.options;
    for (let i = 0; i < opts.length; i++) {
      opts[i]._selected = (i === v);
    }
  }
  submit(submitter) {
    const cancelled = !this.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    if (cancelled) return;

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
    const el = this;
    return new Proxy({}, {
      get(_, k) { if(typeof k!=="string")return undefined; return el.getAttribute("data-"+k.replace(/([A-Z])/g,"-$1").toLowerCase()); },
      set(_, k, v) { el.setAttribute("data-"+k.replace(/([A-Z])/g,"-$1").toLowerCase(), v); return true; },
    });
  }
  get offsetWidth() { return this._isViewportRoot() ? (globalThis.innerWidth || 1280) : 100; }
  get offsetHeight() { return this._isViewportRoot() ? (globalThis.innerHeight || 720) : 20; }
  get offsetTop() { return 0; } get offsetLeft() { return 0; }
  // documentElement / body / window expose VIEWPORT geometry, not their own content box.
  // Puppeteer's #clickableBox clips boxes to document.documentElement.clientWidth/Height;
  // returning 100x20 there made every element appear off-screen and broke .click().
  get clientWidth() { return this._isViewportRoot() ? (globalThis.innerWidth || 1280) : 100; }
  get clientHeight() { return this._isViewportRoot() ? (globalThis.innerHeight || 720) : 20; }
  get scrollWidth() { return this._isViewportRoot() ? (globalThis.innerWidth || 1280) : 100; }
  get scrollHeight() { return this._isViewportRoot() ? (globalThis.innerHeight || 720) : 20; }
  _isViewportRoot() {
    const t = this.tagName;
    return t === 'HTML' || t === 'BODY';
  }
  get scrollTop() { return 0; } set scrollTop(v) {}
  get scrollLeft() { return 0; } set scrollLeft(v) {}
  getBoundingClientRect() {
    globalThis.__obscura_click_target = this;
    // documentElement and body span the full viewport. Without this every
    // hit test against them clips down to a 100x20 synthetic cell and
    // Document.elementFromPoint can never recurse into their children.
    if (this._isViewportRoot()) {
      const vw = globalThis.innerWidth || 1280;
      const vh = globalThis.innerHeight || 720;
      return {
        x: 0, y: 0, width: vw, height: vh,
        top: 0, right: vw, bottom: vh, left: 0,
        toJSON() { return this; },
      };
    }
    // No layout engine, but Playwright's actionability polling needs each
    // element to occupy a stable, distinct rect so hit-testing can pick the
    // right one (issue #45). Synthesize a deterministic position from the
    // node id: every nid maps to a unique cell in a 12-column grid, sized
    // to fit a 1280x720 viewport. Stable across reads, different per node.
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
  getClientRects() { return [this.getBoundingClientRect()]; }
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
  scrollIntoView() { globalThis.__obscura_click_target = this; }
  animate(keyframes, options) {
    const duration = typeof options === 'number' ? options : (options?.duration || 0);
    return {
      finished: Promise.resolve(), currentTime: 0, playState: 'finished',
      effect: { getComputedTiming() { return { duration }; } },
      cancel(){}, finish(){}, play(){}, pause(){}, reverse(){},
      addEventListener(){}, removeEventListener(){},
      onfinish: null, oncancel: null,
    };
  }
  getAnimations() { return []; }
  get isConnected() {
    var node = this;
    while (node) {
      if (node.nodeType === 9) return true;
      node = node.parentNode;
    }
    return false;
  }
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

class Document extends Node {
  get documentElement() { return _wrapEl(+_dom("document_element")); }
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
  set title(v) {}
  get URL() { return _domParse("document_url") ?? ""; }
  get documentURI() { return this.URL; }
  get location() { return globalThis.location; }
  set location(url) { Deno.core.ops.op_navigate(_resolveUrl(String(url)), 'GET', ''); }
  get defaultView() { return globalThis; }
  get nodeType() { return 9; }
  get nodeName() { return "#document"; }
  get ownerDocument() { return null; } // Document has no ownerDocument
  get compatMode() { return "CSS1Compat"; }
  get characterSet() { return "UTF-8"; }
  get contentType() {
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
  createElement(t) {
    const el = _wrapEl(+_dom("create_element", t.toLowerCase()));
    if (el && t.toLowerCase() === 'template') {
      el._templateContent = this.createDocumentFragment();
    }
    return el;
  }
  createElementNS(ns, t) {
    const el = this.createElement(t);
    if (el) el._ns = ns;
    return el;
  }
  createTextNode(t) { return _wrap(+_dom("create_text_node", String(t))); }
  createComment(t) {
    const nid = +_dom("create_comment_node", String(t ?? ""));
    const n = new Comment(nid);
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
    _cache.set(nid, n);
    return n;
  }
  createDocumentFragment() {
    const nid = +_dom("create_document_fragment");
    const frag = new DocumentFragment(nid);
    _cache.set(nid, frag);
    return frag;
  }
  // Legacy DOM Level 2 event factory. Spec returns an event of the requested
  // class with an empty type until init*Event() is called. We previously
  // returned a generic Event for every type, which broke libraries that call
  // createEvent('CustomEvent').initCustomEvent(...) — see issue #41.
  createEvent(type) {
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
    };
    const Cls = map[String(type || '').toLowerCase()] || Event;
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
    whatToShow = whatToShow || 0xFFFFFFFF; // NodeFilter.SHOW_ALL
    const walker = {
      root: root,
      currentNode: root,
      whatToShow: whatToShow,
      filter: filter || null,
      _accept(node) {
        const nodeType = node.nodeType;
        const show = (whatToShow >> (nodeType - 1)) & 1;
        if (!show) return false;
        if (this.filter) {
          if (typeof this.filter === 'function') return this.filter(node) === 1;
          if (this.filter.acceptNode) return this.filter.acceptNode(node) === 1;
        }
        return true;
      },
      nextNode() {
        let node = this.currentNode;
        let child = node.firstChild;
        while (child) {
          if (this._accept(child)) { this.currentNode = child; return child; }
          if (child.firstChild) { child = child.firstChild; continue; }
          if (child.nextSibling) { child = child.nextSibling; continue; }
          let parent = child.parentNode;
          while (parent && parent !== this.root) {
            if (parent.nextSibling) { child = parent.nextSibling; break; }
            parent = parent.parentNode;
          }
          if (!parent || parent === this.root) return null;
        }
        return null;
      },
      previousNode() {
        let node = this.currentNode;
        if (node === this.root) return null;
        let sibling = node.previousSibling;
        if (sibling) {
          while (sibling.lastChild) sibling = sibling.lastChild;
          if (this._accept(sibling)) { this.currentNode = sibling; return sibling; }
        }
        let parent = node.parentNode;
        if (parent && parent !== this.root && this._accept(parent)) {
          this.currentNode = parent;
          return parent;
        }
        return null;
      },
      firstChild() {
        let child = this.currentNode.firstChild;
        while (child) {
          if (this._accept(child)) { this.currentNode = child; return child; }
          child = child.nextSibling;
        }
        return null;
      },
      lastChild() {
        let child = this.currentNode.lastChild;
        while (child) {
          if (this._accept(child)) { this.currentNode = child; return child; }
          child = child.previousSibling;
        }
        return null;
      },
      nextSibling() {
        let sibling = this.currentNode.nextSibling;
        while (sibling) {
          if (this._accept(sibling)) { this.currentNode = sibling; return sibling; }
          sibling = sibling.nextSibling;
        }
        return null;
      },
      previousSibling() {
        let sibling = this.currentNode.previousSibling;
        while (sibling) {
          if (this._accept(sibling)) { this.currentNode = sibling; return sibling; }
          sibling = sibling.previousSibling;
        }
        return null;
      },
      parentNode() {
        let parent = this.currentNode.parentNode;
        if (parent && parent !== this.root && this._accept(parent)) {
          this.currentNode = parent;
          return parent;
        }
        return null;
      },
    };
    return walker;
  }
  createNodeIterator(root, whatToShow, filter) {
    return this.createTreeWalker(root, whatToShow, filter);
  }
  getSelection() { return globalThis.getSelection(); }
  get activeElement() { return globalThis.__obscura_focused || this.body; }
  get implementation() {
    return {
      // Spec: createHTMLDocument returns a NEW detached Document. jQuery
      // 3.x's selector feature-detect calls `body.innerHTML = '<form>'` on
      // the result — when we returned `globalThis.document`, the real
      // `<body>` was wiped, taking every page on the open web that ships
      // jQuery 3.x with it. Reuse the DOMParser path to build a detached
      // document, then optionally set the title.
      createHTMLDocument(title) {
        const skeleton = `<html><head><title></title></head><body></body></html>`;
        const doc = new DOMParser().parseFromString(skeleton, "text/html");
        if (title != null) {
          const t = doc.querySelector("title");
          if (t) t.textContent = String(title);
        }
        return doc;
      },
      // Real spec: createDocument(namespaceURI, qualifiedName, doctype) →
      // an XML document with a root element of the given name. We don't
      // have a separate XML stack, so return a minimal detached document
      // with an element of the requested local name as documentElement.
      createDocument(_ns, qualifiedName, _doctype) {
        const name = (qualifiedName && String(qualifiedName)) || "root";
        const safe = name.replace(/[^a-zA-Z0-9-]/g, "");
        const html = `<${safe}></${safe}>`;
        return new DOMParser().parseFromString(html, "application/xml");
      },
      hasFeature() { return true; },
    };
  }
  get styleSheets() { return []; }
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
    var temp = this.createElement('div');
    temp.innerHTML = html;
    var children = temp.childNodes;
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
  get nodeType() { return 11; }
  get nodeName() { return "#document-fragment"; }
  get innerHTML() { return _domParse("inner_html", this._nid) ?? ""; }
  set innerHTML(v) { _dom("set_inner_html", this._nid, String(v ?? "")); }
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
    const frag = document.createDocumentFragment();
    if (deep) frag.innerHTML = this.innerHTML;
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
  get ownerDocument() { return globalThis.document; }
}

const _cache = new Map();
function _elementClassFor(nid) {
  const tag = _domParse("tag_name", nid);
  if (tag === "FORM" && globalThis.HTMLFormElement) return globalThis.HTMLFormElement;
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
  set href(url) { globalThis.__virtualUrl = null; Deno.core.ops.op_navigate(_resolveUrl(url), 'GET', ''); },
  get origin() { try { return new URL(this.href).origin; } catch { return ""; } },
  get protocol() { try { return new URL(this.href).protocol; } catch { return ""; } },
  get host() { try { return new URL(this.href).host; } catch { return ""; } },
  get hostname() { try { return new URL(this.href).hostname; } catch { return ""; } },
  get pathname() { try { return new URL(this.href).pathname; } catch { return "/"; } },
  get search() { try { return new URL(this.href).search; } catch { return ""; } },
  get hash() { try { return new URL(this.href).hash; } catch { return ""; } },
  get port() { try { return new URL(this.href).port; } catch { return ""; } },
  toString() { return this.href; },
  assign(url) { globalThis.__virtualUrl = null; Deno.core.ops.op_navigate(_resolveUrl(url), 'GET', ''); },
  reload() {},
  replace(url) { globalThis.__virtualUrl = null; Deno.core.ops.op_navigate(_resolveUrl(url), 'GET', ''); },
};
const _locationObj = globalThis.location;
Object.defineProperty(globalThis, 'location', {
  get() { return _locationObj; },
  set(url) { Deno.core.ops.op_navigate(_resolveUrl(String(url)), 'GET', ''); },
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

// HTML spec exposes on* event handler IDL attributes on Window. Libraries like
// jQuery feature-detect bubbling via `("on" + ev) in window` and fall back to
// a legacy IE path that crashes on missing DOM APIs when the check returns
// false. Initialising them to null makes the check match real browsers.
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
  if (!(("on" + _ev) in globalThis)) globalThis["on" + _ev] = null;
}

globalThis.Window = globalThis.Window || function Window() {};
Object.defineProperty(globalThis.Window, Symbol.hasInstance, {
  value(obj) { return obj === globalThis || (obj && obj.window === obj); },
  configurable: true,
});


const _iframeRegistry = [];
function _registerIframe(iframeEl) {
  const idx = _iframeRegistry.length;
  _iframeRegistry.push(iframeEl);
  globalThis.length = _iframeRegistry.length;
  Object.defineProperty(globalThis, idx, {
    get() { return iframeEl._iframeWin || null; },
    configurable: true,
    enumerable: false,
  });
}
globalThis.navigator = {
  get userAgent() { return globalThis.__obscura_ua || "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"; },
  get appVersion() { return this.userAgent.replace('Mozilla/', ''); },
  language: "en-US", languages: ["en-US","en"], platform: "Linux x86_64",
  onLine: true, cookieEnabled: true, hardwareConcurrency: 8,
  maxTouchPoints: 0,
  vendor: "Google Inc.", product: "Gecko", productSub: "20030107",
  doNotTrack: null,
  deviceMemory: 8,
  connection: { effectiveType: "4g", rtt: 50, downlink: 10, saveData: false },
  get webdriver() { return undefined; },
  pdfViewerEnabled: true,
  get plugins() {
    const p = [
      { name: "PDF Viewer", filename: "internal-pdf-viewer", description: "Portable Document Format", length: 1 },
      { name: "Chrome PDF Viewer", filename: "internal-pdf-viewer", description: "Portable Document Format", length: 1 },
      { name: "Chromium PDF Viewer", filename: "internal-pdf-viewer", description: "Portable Document Format", length: 1 },
      { name: "Microsoft Edge PDF Viewer", filename: "internal-pdf-viewer", description: "Portable Document Format", length: 1 },
      { name: "WebKit built-in PDF", filename: "internal-pdf-viewer", description: "Portable Document Format", length: 1 },
    ];
    p.item = (i) => p[i] || null;
    p.namedItem = (name) => p.find(x => x.name === name) || null;
    p.refresh = () => {};
    p[Symbol.iterator] = function() { return this[Symbol.iterator](); };
    return p;
  },
  get mimeTypes() {
    const m = [
      { type: "application/pdf", description: "Portable Document Format", suffixes: "pdf", enabledPlugin: null },
      { type: "text/pdf", description: "Portable Document Format", suffixes: "pdf", enabledPlugin: null },
    ];
    m.item = (i) => m[i] || null;
    m.namedItem = (name) => m.find(x => x.type === name) || null;
    return m;
  },
  userAgentData: {
    brands: [
      {brand: "Google Chrome", version: "145"},
      {brand: "Chromium", version: "145"},
      {brand: "Not=A?Brand", version: "24"},
    ],
    mobile: false,
    platform: "Linux",
    getHighEntropyValues(hints) {
      return Promise.resolve({
        architecture: "x86",
        bitness: "64",
        brands: [{brand:"Google Chrome",version:"145"},{brand:"Chromium",version:"145"},{brand:"Not=A?Brand",version:"24"}],
        fullVersionList: [{brand:"Google Chrome",version:"145.0.0.0"},{brand:"Chromium",version:"145.0.0.0"},{brand:"Not=A?Brand",version:"24.0.0.0"}],
        mobile: false,
        model: "",
        platform: "Linux",
        platformVersion: "6.8.0",
        uaFullVersion: "145.0.0.0",
      });
    },
    toJSON() { return {brands:this.brands,mobile:this.mobile,platform:this.platform}; },
  },
  serviceWorker: { ready: Promise.resolve(), register(){return Promise.resolve();}, getRegistrations(){return Promise.resolve([]);}, controller: null },
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
    if (params?.name === 'notifications') return Promise.resolve({state:"prompt",onchange:null});
    return Promise.resolve({state:"granted"});
  } },
  getBattery() { return Promise.resolve({ charging: _fp('batteryCharging'), chargingTime: _fp('batteryCharging') ? 0 : Infinity, dischargingTime: _fp('batteryCharging') ? Infinity : Math.floor(3600 + _fpRand(250) * 7200), level: _fp('batteryLevel'), addEventListener(){} }); },
  getGamepads() { return []; },
  sendBeacon() { return true; },
  javaEnabled() { return false; },
  geolocation: {
    getCurrentPosition(success, error) {
      const coords = {
        latitude: 50.1109 + (_fpRand(500) - 0.5) * 0.1,
        longitude: 8.6821 + (_fpRand(501) - 0.5) * 0.1,
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
          latitude: 50.1109 + (_fpRand(503) - 0.5) * 0.1,
          longitude: 8.6821 + (_fpRand(504) - 0.5) * 0.1,
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

globalThis.chrome = {
  app: { isInstalled: false, InstallState: { DISABLED: "disabled", INSTALLED: "installed", NOT_INSTALLED: "not_installed" }, RunningState: { CANNOT_RUN: "cannot_run", READY_TO_RUN: "ready_to_run", RUNNING: "running" } },
  runtime: { OnInstalledReason: {}, OnRestartRequiredReason: {}, PlatformArch: {}, PlatformNaclArch: {}, PlatformOs: {}, RequestUpdateCheckStatus: {}, connect() { return {}; }, sendMessage() {} },
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

globalThis.screen = { width:1920, height:1080, availWidth:1920, availHeight:1040, colorDepth:24, pixelDepth:24, availTop:0, availLeft:0, orientation:{type:"landscape-primary",angle:0,addEventListener(){},removeEventListener(){},dispatchEvent(){return true;}} };
globalThis.visualViewport = { width:1920, height:1000, offsetLeft:0, offsetTop:0, scale:1, addEventListener(){}, removeEventListener(){} };
globalThis.devicePixelRatio = 1;
globalThis.innerWidth = 1920; globalThis.innerHeight = 1000;
globalThis.outerWidth = 1920; globalThis.outerHeight = 1080;
globalThis.scrollX = 0; globalThis.scrollY = 0;
globalThis.pageXOffset = 0; globalThis.pageYOffset = 0;

globalThis.__fetchInterceptEnabled = false;
globalThis.__fetchInterceptCallback = null; // Set by CDP to handle paused requests

function _base64ToUint8Array(b64) {
  const clean = String(b64 || '').replace(/[\r\n\s]/g, '');
  if (!clean) return new Uint8Array();
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const padding = clean.endsWith('==') ? 2 : (clean.endsWith('=') ? 1 : 0);
  const bytes = new Uint8Array((clean.length * 3 >> 2) - padding);
  let out = 0;
  for (let i = 0; i < clean.length; i += 4) {
    const a = alphabet.indexOf(clean[i]);
    const b = alphabet.indexOf(clean[i + 1]);
    const c = clean[i + 2] === '=' ? 0 : alphabet.indexOf(clean[i + 2]);
    const d = clean[i + 3] === '=' ? 0 : alphabet.indexOf(clean[i + 3]);
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

globalThis.fetch = async (input, init = {}) => {
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
  const hdrs = JSON.stringify(init.headers instanceof Headers ? Object.fromEntries(init.headers.entries()) : init.headers || {});
  const body = init.body ? String(init.body) : "";
  const fetchMode = init.mode || (input instanceof Request ? input.mode : "cors");
  const pageOrigin = (function() { try { const u = new URL(_domParse("document_url") || "about:blank"); return u.origin; } catch(e) { return ""; } })();
  const raw = await Deno.core.ops.op_fetch_url(url, method, hdrs, body, pageOrigin, fetchMode);
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
  return new Response(responseBody, {
    status: parsed.status,
    statusText: "",
    headers: parsed.headers || {},
    type: respType,
    url: parsed.url || url,
    redirected: false,
  });
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

globalThis.XMLHttpRequest = class XMLHttpRequest {
  static UNSENT = 0;
  static OPENED = 1;
  static HEADERS_RECEIVED = 2;
  static LOADING = 3;
  static DONE = 4;
  UNSENT = 0; OPENED = 1; HEADERS_RECEIVED = 2; LOADING = 3; DONE = 4;

  constructor() {
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
_markNative(XMLHttpRequest.prototype.getResponseHeader);
_markNative(XMLHttpRequest.prototype.getAllResponseHeaders);

if (typeof URL === 'undefined' || !URL.prototype) {
  globalThis.URL = class URL {
    constructor(url, base) {
      // Per WHATWG URL spec, both arguments are stringified — callers
      // routinely pass `window.location` (Location object) or a URL
      // instance as `base`. Coerce explicitly so the regex .match() calls
      // below don't blow up on non-strings.
      url = String(url);
      if (base !== undefined && base !== null) base = String(base);
      let full = url;
      if (base && !url.includes('://')) {
        var bm = base.match(/^(https?:\/\/[^\/\?#]+)(\/[^?#]*)?/);
        if (bm) {
          var bOrigin = bm[1];
          var bPath = bm[2] || '/';
          if (url.startsWith('/')) {
            full = bOrigin + url;
          } else if (url.startsWith('?') || url.startsWith('#')) {
            full = bOrigin + bPath + url;
          } else {
            var dir = bPath.substring(0, bPath.lastIndexOf('/') + 1);
            full = bOrigin + dir + url;
          }
        }
      }
      const m = full.match(/^(https?):\/\/([^\/\?#]+)(\/[^?#]*)?(\?[^#]*)?(#.*)?$/);
      if (m) {
        this.protocol = m[1] + ':';
        this.host = m[2]; this.hostname = m[2].split(':')[0];
        this.port = m[2].includes(':') ? m[2].split(':')[1] : '';
        this.pathname = m[3] || '/';
        this.search = m[4] || ''; this.hash = m[5] || '';
      } else {
        this.protocol = ''; this.host = ''; this.hostname = '';
        this.port = ''; this.pathname = full; this.search = ''; this.hash = '';
      }
      this.href = full; this.origin = this.protocol + '//' + this.host;
      this.searchParams = new URLSearchParams(this.search);
    }
    toString() { return this.href; }
    toJSON() { return this.href; }
    static createObjectURL() { return 'blob:null/fake-' + Math.random().toString(36).slice(2); }
    static revokeObjectURL() {}
  };
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
      if (typeof input === 'string') { this.url = input; }
      else if (input instanceof Request) { this.url = input.url; init = { ...input, ...init }; }
      else if (typeof URL === 'function' && input instanceof URL) { this.url = input.href; }
      else { this.url = input?.url || input?.href || String(input); }
      this.method = (init.method || 'GET').toUpperCase();
      this.headers = new Headers(init.headers);
      this.body = init.body || null;
      this.mode = init.mode || 'cors';
      this.credentials = init.credentials || 'same-origin';
      this.redirect = init.redirect || 'follow';
      this.referrer = init.referrer || '';
      this.signal = init.signal || { aborted: false, addEventListener(){}, removeEventListener(){} };
      this.cache = init.cache || 'default';
    }
    clone() { return new Request(this.url, { method: this.method, headers: this.headers, body: this.body }); }
    async text() { return this.body ? String(this.body) : ''; }
    async json() { return JSON.parse(await this.text()); }
    async arrayBuffer() { return new TextEncoder().encode(await this.text()).buffer; }
  };
}

if (typeof Response === 'undefined') {
  globalThis.Response = class Response {
    constructor(body, init = {}) {
      this._bodyBytes = _bodyToUint8Array(body); this.status = init.status || 200; this.statusText = init.statusText || '';
      this.ok = this.status >= 200 && this.status < 300;
      this.headers = new Headers(init.headers);
      this.type = init.type || 'basic'; this.url = init.url || ''; this.redirected = !!init.redirected;
    }
    async text() { return new TextDecoder().decode(this._bodyBytes); }
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
  Element.prototype.replaceWith = function(...nodes) {
    const parent = this.parentNode;
    if (!parent) return;
    for (const n of nodes) {
      if (typeof n === 'string') parent.insertBefore(document.createTextNode(n), this);
      else parent.insertBefore(n, this);
    }
    parent.removeChild(this);
  };
  _markNative(Element.prototype.replaceWith);
}
if (!Element.prototype.before) {
  Element.prototype.before = function(...nodes) {
    const parent = this.parentNode;
    if (!parent) return;
    for (const n of nodes) {
      if (typeof n === 'string') parent.insertBefore(document.createTextNode(n), this);
      else parent.insertBefore(n, this);
    }
  };
  _markNative(Element.prototype.before);
}
if (!Element.prototype.after) {
  Element.prototype.after = function(...nodes) {
    const parent = this.parentNode;
    if (!parent) return;
    const ref = this.nextSibling;
    for (const n of nodes) {
      if (typeof n === 'string') parent.insertBefore(document.createTextNode(n), ref);
      else parent.insertBefore(n, ref);
    }
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
      let node = this;
      while (node) {
        if (node.nodeType === 9) return true; // Document node
        node = node.parentNode;
      }
      return false;
    }
  });
}

globalThis.ResizeObserver = class ResizeObserver {
  constructor(callback) {
    this._callback = callback;
    this._targets = new Set();
    this._connected = true;
    this._fireCount = 0;
  }
  _fireFor(targets) {
    if (!this._connected || !targets.length) return;
    const records = targets.map(target => {
      const r = target.getBoundingClientRect ? target.getBoundingClientRect() : { x: 0, y: 0, width: 100, height: 20 };
      return {
        target,
        contentRect: { x: r.x || 0, y: r.y || 0, width: r.width || 100, height: r.height || 20, top: r.top || 0, left: r.left || 0, bottom: r.bottom || 20, right: r.right || 100 },
        borderBoxSize: [{ blockSize: r.height || 20, inlineSize: r.width || 100 }],
        contentBoxSize: [{ blockSize: r.height || 20, inlineSize: r.width || 100 }],
        devicePixelContentBoxSize: [{ blockSize: r.height || 20, inlineSize: r.width || 100 }],
      };
    });
    try { this._callback(records, this); } catch (e) { /* RO callbacks must not propagate */ }
  }
  observe(el) {
    if (!el || !this._connected) return;
    if (this._targets.has(el)) return;
    this._targets.add(el);
    Promise.resolve().then(() => this._fireFor([el]));
    [200, 800].forEach(delay => {
      setTimeout(() => {
        if (this._connected && this._targets.has(el) && this._fireCount < 16) {
          this._fireCount++;
          this._fireFor([el]);
        }
      }, delay);
    });
  }
  unobserve(el) { this._targets.delete(el); }
  disconnect() { this._connected = false; this._targets.clear(); }
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
if (typeof TextDecoder === 'undefined') {
  globalThis.TextDecoder = class TextDecoder {
    constructor(label) { this.encoding = label || 'utf-8'; }
    decode(buf) {
      if (!buf) return '';
      const bytes = ArrayBuffer.isView(buf)
        ? new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength)
        : new Uint8Array(buf);
      let str = '', i = 0;
      while (i < bytes.length) {
        let c = bytes[i++];
        if (c < 0x80) str += String.fromCharCode(c);
        else if (c < 0xE0) str += String.fromCharCode(((c&0x1F)<<6)|(bytes[i++]&0x3F));
        else if (c < 0xF0) { const b1=bytes[i++], b2=bytes[i++]; str += String.fromCharCode(((c&0x0F)<<12)|((b1&0x3F)<<6)|(b2&0x3F)); }
        else { const b1=bytes[i++], b2=bytes[i++], b3=bytes[i++]; const cp=((c&0x07)<<18)|((b1&0x3F)<<12)|((b2&0x3F)<<6)|(b3&0x3F); if(cp>0xFFFF){const s=cp-0x10000;str+=String.fromCharCode(0xD800+(s>>10),0xDC00+(s&0x3FF));}else str+=String.fromCharCode(cp); }
      }
      return str;
    }
  };
}

globalThis.matchMedia = _markNative(function matchMedia(q) { return { matches: false, media: q, addListener(){}, removeListener(){}, addEventListener(){}, removeEventListener(){}, dispatchEvent(){return true;} }; });
globalThis.getComputedStyle = (el) => {
  if (!el) el = document.body || {};
  const style = el?.style || el?._style || new CSSStyleDeclaration();
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
    'font-family': 'Times',
    color: 'rgb(0, 0, 0)', 'background-color': 'rgba(0, 0, 0, 0)',
    'border-width': '0px', 'border-style': 'none', 'border-color': 'rgb(0, 0, 0)',
    'border-top-width': '0px', 'border-right-width': '0px',
    'border-bottom-width': '0px', 'border-left-width': '0px',
    'border-radius': '0px',
    'z-index': 'auto', 'pointer-events': 'auto',
    'box-sizing': 'content-box', cursor: 'auto',
    'white-space': 'normal', 'text-align': 'start',
    'flex-direction': 'row', 'flex-wrap': 'nowrap', 'align-items': 'normal',
    'justify-content': 'normal', gap: 'normal',
    'grid-template-columns': 'none', 'grid-template-rows': 'none',
    'will-change': 'auto', 'backface-visibility': 'visible',
  };

  const lookup = (rawProp) => {
    if (typeof rawProp !== 'string') return '';
    // Inline value first.
    const inlineVal = target.getPropertyValue ? target.getPropertyValue(rawProp) : '';
    if (inlineVal) return inlineVal;
    const kebab = rawProp.replace(/([A-Z])/g, '-$1').toLowerCase();
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
      if (prop in target) return target[prop];
      if (prop === 'getPropertyValue') return (name) => lookup(name);
      if (prop === 'getPropertyPriority') return () => '';
      if (prop === 'item') return (i) => '';
      if (prop === 'length') return 0;
      if (prop === 'cssText') return '';
      if (prop === 'parentRule') return null;
      if (typeof prop === 'string') return lookup(prop);
      return undefined;
    },
  });
};
globalThis.getSelection = _markNative(function getSelection() {
  return {
    rangeCount: 0,
    anchorNode: null, anchorOffset: 0,
    focusNode: null, focusOffset: 0,
    isCollapsed: true, type: 'None',
    removeAllRanges() { this.rangeCount = 0; },
    addRange(range) { this.rangeCount = 1; this._range = range; },
    getRangeAt(i) { return this._range || null; },
    collapse(node, offset) { this.anchorNode = node; this.anchorOffset = offset || 0; this.isCollapsed = true; },
    extend(node, offset) { this.focusNode = node; this.focusOffset = offset || 0; },
    selectAllChildren(node) {},
    deleteFromDocument() {},
    containsNode(node) { return false; },
    toString() { return ''; },
  };
});

globalThis.CSSStyleSheet = class CSSStyleSheet {
  constructor(options) {
    this.cssRules = [];
    this.ownerRule = null;
    this.disabled = false;
    this._rules = [];
  }
  insertRule(rule, index) {
    const idx = index ?? this._rules.length;
    this._rules.splice(idx, 0, { cssText: rule, type: 1 });
    this.cssRules = this._rules;
    return idx;
  }
  deleteRule(index) {
    this._rules.splice(index, 1);
    this.cssRules = this._rules;
  }
  addRule(selector, style, index) {
    return this.insertRule(selector + '{' + style + '}', index);
  }
  removeRule(index) { this.deleteRule(index); }
  replace(text) {
    this._rules = [{ cssText: text, type: 1 }];
    this.cssRules = this._rules;
    return Promise.resolve(this);
  }
  replaceSync(text) {
    this._rules = [{ cssText: text, type: 1 }];
    this.cssRules = this._rules;
  }
};

Object.defineProperty(Document.prototype, 'adoptedStyleSheets', {
  get() { return this._adoptedStyleSheets || []; },
  set(sheets) { this._adoptedStyleSheets = sheets; },
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

globalThis.ShadowRoot = class ShadowRoot extends DocumentFragment {};
globalThis.__obscura_shadowHostNames = new Set(['article','aside','blockquote','body','div','footer','h1','h2','h3','h4','h5','h6','header','main','nav','p','section','span']);
globalThis.customElements = {
  _registry: new Map(),
  _whenDefinedResolvers: new Map(),
  define(name, cls, opts) {
    if (this._registry.has(name)) return;
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
  },
  _upgradeElement(el, cls) {
    if (el.__customUpgraded) return;
    el.__customUpgraded = true;
    try {
      // Web Components spec: copy own props from the prototype onto the
      // element. JS-side classes define behavior via methods on the
      // prototype; we don't truly swap prototypes (Element is shared),
      // so attach the prototype methods directly to the instance.
      const proto = cls.prototype;
      for (const key of Object.getOwnPropertyNames(proto)) {
        if (key === 'constructor') continue;
        const desc = Object.getOwnPropertyDescriptor(proto, key);
        if (desc) Object.defineProperty(el, key, desc);
      }
      // Run constructor-side init on the element. Real custom elements
      // run the class constructor, but Element instances aren't a `cls`
      // subclass here; calling `.call(el)` runs whatever init logic the
      // class defines without needing a new allocation.
      try { cls.call(el); } catch (e) {}
      if (typeof el.connectedCallback === 'function' && globalThis.document?.contains?.(el)) {
        try { el.connectedCallback(); } catch (e) {}
      }
    } catch (e) {}
  },
  get(name) { return this._registry.get(name); },
  whenDefined(name) {
    const cls = this._registry.get(name);
    if (cls) return Promise.resolve(cls);
    return new Promise((resolve) => {
      const list = this._whenDefinedResolvers.get(name) || [];
      list.push(resolve);
      this._whenDefinedResolvers.set(name, list);
    });
  },
  upgrade(root) {
    if (!root || !root.querySelectorAll) return;
    for (const [name, cls] of this._registry.entries()) {
      const matches = root.querySelectorAll(name);
      for (const el of matches) this._upgradeElement(el, cls);
    }
  },
};
globalThis.NodeFilter = { SHOW_ELEMENT: 1, SHOW_TEXT: 4, SHOW_ALL: 0xFFFFFFFF };
// ResizeObserver is defined earlier with real per-target firing; the stub
// that previously lived here was a no-op that clobbered the real class.
//
// IntersectionObserver: without a layout engine we can't compute real
// intersection geometry, so every observed target is treated as fully
// in-viewport (`isIntersecting: true`, `intersectionRatio: 1`). Real
// libraries lean on this in three patterns we must support:
//
//   1. Lazy load: observe(img) -> first intersection -> load src -> unobserve.
//      One fire is enough — covered by the initial microtask fire.
//   2. Infinite scroll: observe(sentinel) -> on intersection load more ->
//      new sentinel mounts -> fire again. Needs re-fires as DOM grows.
//   3. Reveal-on-scroll animations: observe(card) -> isIntersecting flips
//      true once and an animation runs. One fire is enough.
//
// To cover (2) without spinning forever, we burst-fire at an exponential
// backoff schedule and ALSO re-fire whenever the DOM mutates (a strong
// signal that the page just rendered something new). Per-observer total
// fire cap stops us from looping on a never-disconnected observer.
globalThis.__intersectionObservers = [];
globalThis.IntersectionObserver = class IntersectionObserver {
  constructor(callback, options) {
    this._callback = callback;
    this._options = options || {};
    this._targets = new Set();
    this._connected = true;
    this._fireCount = 0;
    globalThis.__intersectionObservers.push(this);
  }
  _fireFor(targets) {
    if (!this._connected || !targets.length || this._fireCount >= 256) return;
    this._fireCount++;
    const records = targets.map(target => ({
      target,
      isIntersecting: true,
      intersectionRatio: 1,
      boundingClientRect: target.getBoundingClientRect
        ? target.getBoundingClientRect()
        : { x: 0, y: 0, width: 100, height: 20, top: 0, left: 0, right: 100, bottom: 20 },
      intersectionRect: target.getBoundingClientRect
        ? target.getBoundingClientRect()
        : { x: 0, y: 0, width: 100, height: 20, top: 0, left: 0, right: 100, bottom: 20 },
      rootBounds: { x: 0, y: 0, width: 1280, height: 720, top: 0, left: 0, right: 1280, bottom: 720 },
      time: Date.now(),
    }));
    try { this._callback(records, this); } catch (e) { /* IO callbacks must not propagate */ }
  }
  observe(el) {
    if (!el || !this._connected) return;
    if (this._targets.has(el)) return;
    this._targets.add(el);
    Promise.resolve().then(() => this._fireFor([el]));
    // Exponential burst to cover infinite-scroll sentinels that "re-arm"
    // after content lands. Without a real scroll/layout signal, we fake the
    // re-fire schedule. Beyond ~10s the page has usually settled.
    [120, 500, 1500, 3500, 7000].forEach(delay => {
      setTimeout(() => {
        if (this._connected && this._targets.has(el)) this._fireFor([el]);
      }, delay);
    });
  }
  unobserve(el) { this._targets.delete(el); }
  disconnect() {
    this._connected = false;
    this._targets.clear();
    const idx = globalThis.__intersectionObservers.indexOf(this);
    if (idx >= 0) globalThis.__intersectionObservers.splice(idx, 1);
  }
  takeRecords() { return []; }
  get root() { return this._options.root || null; }
  get rootMargin() { return this._options.rootMargin || "0px 0px 0px 0px"; }
  get thresholds() {
    const t = this._options.threshold;
    if (t == null) return [0];
    return Array.isArray(t) ? t.slice() : [t];
  }
};
// When the DOM mutates (e.g. infinite scroll loads a batch of items), re-fire
// every active IntersectionObserver so libraries observing dynamic content
// see a fresh isIntersecting=true event. Uses the same per-observer fire cap
// to prevent runaway loops if the page is mutating in a tight cycle.
(function() {
  const reFire = () => {
    for (const obs of globalThis.__intersectionObservers) {
      if (!obs._connected) continue;
      const ts = [...obs._targets];
      if (ts.length) obs._fireFor(ts);
    }
  };
  // Lazy-attach a single MutationObserver on document.body once the page is
  // ready, debounced via a microtask so a flurry of mutations only triggers
  // one IO sweep.
  let pending = false;
  const wireUp = () => {
    if (!globalThis.document?.body) return;
    const mo = new MutationObserver(() => {
      if (pending) return;
      pending = true;
      Promise.resolve().then(() => { pending = false; reFire(); });
    });
    try { mo.observe(globalThis.document.body, {childList: true, subtree: true}); } catch {}
  };
  if (globalThis.document?.body) wireUp();
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
globalThis.Event = class Event {
  constructor(t,o={}) { this.type=t;this.bubbles=!!o.bubbles;this.cancelable=!!o.cancelable;this.composed=!!o.composed;this.defaultPrevented=false;this.target=null;this.currentTarget=null;this.eventPhase=0;this.timeStamp=Date.now();this._propagationStopped=false;this._immediatePropagationStopped=false; }
  get isTrusted() { return true; }
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
globalThis.CustomEvent = class extends Event {
  constructor(t,o={}) { super(t,o);this.detail=o.detail; }
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
globalThis.WheelEvent = class extends Event { constructor(t,o={}) { super(t,o);this.deltaX=o.deltaX||0;this.deltaY=o.deltaY||0;this.deltaZ=o.deltaZ||0;this.deltaMode=o.deltaMode||0; } };

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
globalThis.MessageEvent = class extends Event { constructor(t,o={}) { super(t,o);this.data=o.data; } };
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

globalThis.AbortController = class AbortController { constructor(){this.signal={aborted:false,addEventListener(){},removeEventListener(){},onabort:null};} abort(){this.signal.aborted=true;} };
globalThis.AbortSignal = { timeout(ms){return {aborted:false,addEventListener(){},removeEventListener(){}}; } };
if (typeof Blob === "undefined") globalThis.Blob = class Blob { constructor(parts=[],opts={}){this._data=parts.join("");this.size=this._data.length;this.type=opts.type||"";} async text(){return this._data;} };
if (typeof File === "undefined") globalThis.File = class extends Blob { constructor(parts,name,opts){super(parts,opts);this.name=name;} };
if (typeof FormData === "undefined") globalThis.FormData = class FormData { constructor(){this._d=[];} append(k,v){this._d.push([k,v]);} get(k){const e=this._d.find(([a])=>a===k);return e?e[1]:null;} getAll(k){return this._d.filter(([a])=>a===k).map(([,v])=>v);} has(k){return this._d.some(([a])=>a===k);} entries(){return this._d[Symbol.iterator]();} forEach(cb){this._d.forEach(([k,v])=>cb(v,k));} };
if (typeof URLSearchParams === "undefined") globalThis.URLSearchParams = class {
  constructor(init=""){
    this._p=[];
    if(typeof init==="string"){
      init.replace(/^\?/,"").split("&").forEach(p=>{const[k,...v]=p.split("=");if(k)this.append(decodeURIComponent(k),decodeURIComponent(v.join("=")));});
    } else if (init && typeof init[Symbol.iterator] === 'function') {
      for (const pair of init) if (pair && pair.length >= 2) this.append(pair[0], pair[1]);
    } else if (init && typeof init === 'object') {
      Object.keys(init).forEach(k => this.append(k, init[k]));
    }
  }
  append(k,v){this._p.push([String(k),String(v)]);}
  get(k){const p=this._p.find(([key])=>key===String(k)); return p?p[1]:null;}
  set(k,v){this.delete(k); this.append(k,v);}
  delete(k){k=String(k); this._p=this._p.filter(([key])=>key!==k);}
  has(k){k=String(k); return this._p.some(([key])=>key===k);}
  toString(){return this._p.map(([k,v])=>`${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join("&");}
  forEach(cb){this._p.forEach(([k,v])=>cb(v,k,this));}
};

// Real-enough DOMParser. The previous one-liner returned `globalThis.document`,
// so anything that did `new DOMParser().parseFromString(s, 'text/html')` and
// then read `.body.innerHTML` mutated the LIVE page (jQuery 3.x's selector
// feature-detect writes `<form></form>` and wiped real bodies). We parse the
// input into a detached `<html>` element and wrap it so the common Document
// API surface (body / head / documentElement / querySelector* / getElementById /
// getElementsByTagName / getElementsByClassName / title / cloneNode) works.
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
        return t ? (t.textContent || "") : "";
      },
      get firstChild() { return root; },
      get lastChild() { return root; },
      get children() { return [root]; },
      get childNodes() { return [root]; },
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
  now: () => Date.now(),
  mark(){}, measure(){},
  clearMarks(){}, clearMeasures(){}, clearResourceTimings(){},
  getEntries(){return [];}, getEntriesByName(){return [];}, getEntriesByType(){return [];},
  setResourceTimingBufferSize(){},
  timeOrigin: 0,
  timing: { navigationStart: 0, domContentLoadedEventEnd: 0, loadEventEnd: 0 },
  navigation: { type: 0, redirectCount: 0 },
  memory: {
    jsHeapSizeLimit: 2172649472,
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
globalThis.crypto = globalThis.crypto || { getRandomValues(arr) { for(let i=0;i<arr.length;i++) arr[i]=Math.floor(Math.random()*256); return arr; }, randomUUID(){ return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g,c=>{const r=Math.random()*16|0;return(c==="x"?r:(r&3|8)).toString(16);}); } };
globalThis.structuredClone = globalThis.structuredClone || ((v) => JSON.parse(JSON.stringify(v)));
globalThis.reportError = globalThis.reportError || ((e) => console.error(e));

globalThis.Storage = function Storage() {};
Storage.prototype.getItem = function(k) { return (this._data && this._data[k]) ?? null; };
Storage.prototype.setItem = function(k, v) { if (this._data) this._data[k] = String(v); };
Storage.prototype.removeItem = function(k) { if (this._data) delete this._data[k]; };
Storage.prototype.clear = function() { if (this._data) for (var k in this._data) delete this._data[k]; };
Object.defineProperty(Storage.prototype, 'length', { get: function() { return this._data ? Object.keys(this._data).length : 0; } });
Storage.prototype.key = function(i) { return this._data ? Object.keys(this._data)[i] ?? null : null; };

const _mkStore = () => { var s = Object.create(Storage.prototype); s._data = {}; return s; };
globalThis.localStorage = _mkStore();
globalThis.sessionStorage = _mkStore();

globalThis.btoa = globalThis.btoa || ((s) => { const b = new TextEncoder().encode(s); const c="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; let r=""; for(let i=0;i<b.length;i+=3){const a=b[i],bb=b[i+1]??0,cc=b[i+2]??0; r+=c[a>>2]+c[((a&3)<<4)|(bb>>4)]+(i+1<b.length?c[((bb&15)<<2)|(cc>>6)]:"=")+(i+2<b.length?c[cc&63]:"=");} return r; });
globalThis.atob = globalThis.atob || ((s) => { const c="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; let r=[]; for(let i=0;i<s.length;i+=4){const a=c.indexOf(s[i]),b=c.indexOf(s[i+1]),cc=c.indexOf(s[i+2]),d=c.indexOf(s[i+3]); r.push((a<<2)|(b>>4)); if(cc>=0)r.push(((b&15)<<4)|(cc>>2)); if(d>=0)r.push(((cc&3)<<6)|d);} return String.fromCharCode(...r); });

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
  const resolveOrFallback = (url) => {
    if (url === null || url === undefined) return undefined;
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
  globalThis.history = {
    get length() { return stack.length; },
    get state() { return stack[idx].state; },
    scrollRestoration: "auto",
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
    },
    replaceState(state, _title, url) {
      const prevUrl = __currentUrl();
      const resolved = resolveOrFallback(url);
      stack[idx] = {state: state ?? null, url: resolved};
      applyVirtual();
      fireHashChangeIfNeeded(prevUrl);
    },
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
    },
    back() { this.go(-1); },
    forward() { this.go(1); },
  };
})();
globalThis.screenX = 0; globalThis.screenY = 0;
globalThis.screenLeft = 0; globalThis.screenTop = 0;
globalThis.pageXOffset = 0; globalThis.pageYOffset = 0;
globalThis.scrollX = 0; globalThis.scrollY = 0;

globalThis.CSS = { supports(){return false;}, escape(s){return s;} };

globalThis.HTMLElement = Element;
globalThis.HTMLDivElement = Element;
globalThis.HTMLSpanElement = Element;
globalThis.HTMLParagraphElement = Element;
globalThis.HTMLAnchorElement = Element;
globalThis.HTMLImageElement = Element;
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
globalThis.HTMLVideoElement = Element;
globalThis.HTMLAudioElement = Element;
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
globalThis.SVGElement = Element;
globalThis.SVGSVGElement = Element;
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
// Build a NodeList (no named access, per spec) for querySelectorAll. Kept light
// on purpose: querySelectorAll is the hottest query API.
function _nodeList(els) {
  const nl = new NodeList();
  for (let i = 0; i < els.length; i++) nl[nl.length] = els[i];
  return nl;
}
globalThis.DOMTokenList = DOMTokenList;
globalThis.NodeList = class NodeList extends Array {
  item(i) { return this[i] != null ? this[i] : null; }
};
globalThis.Range = class Range { setStart(){} setEnd(){} collapse(){} selectNodeContents(){} deleteContents(){} cloneContents(){ return document.createDocumentFragment(); } insertNode(){} getBoundingClientRect(){return {x:0,y:0,width:0,height:0,top:0,right:0,bottom:0,left:0};} };

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
  Element.prototype.insertAdjacentHTML, Element.prototype.scrollIntoView,
  Element.prototype.append, Element.prototype.prepend, Element.prototype.remove,
  Element.prototype.before, Element.prototype.after, Element.prototype.replaceWith,
  HTMLFormElement.prototype.reset,
  Element.prototype.getContext, Element.prototype.toDataURL, Element.prototype.toBlob,
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

  addEventListener() {}
  removeEventListener() {}
  dispatchEvent() { return true; }

  write(html) {
    if (this._body) this._body.innerHTML += html;
  }
  writeln(html) { this.write(html + '\n'); }
  open() { if (this._body) this._body.innerHTML = ''; }
  close() {}
}

class _IframeWindow {
  constructor(doc, url) {
    this.document = doc;
    this._url = url;
    this.self = this;
    this.top = globalThis;
    this.parent = globalThis;
    this.window = this;
    this.frames = this;
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
  }

  postMessage(data, origin) {
    const event = new MessageEvent('message', {
      data: data,
      origin: this.location.origin,
      source: this,
    });
    Promise.resolve().then(() => {
      globalThis.dispatchEvent?.(event);
    });
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

globalThis.__ariaQuerySelector = function(root, selector) { return null; };
globalThis.__ariaQuerySelectorAll = async function*(root, selector) { /* yields nothing */ };
class _Canvas2D {
  constructor(canvas) {
    this.canvas = canvas;
    this._w = canvas.width || 300;
    this._h = canvas.height || 150;
    this._buf = new Uint8ClampedArray(this._w * this._h * 4);
    for (let i = 0; i < this._w * this._h; i++) {
      this._buf[i*4+0] = 255 + Math.floor(_fpNoise(i % this._w, Math.floor(i / this._w), 0));
      this._buf[i*4+1] = 255 + Math.floor(_fpNoise(i % this._w, Math.floor(i / this._w), 1));
      this._buf[i*4+2] = 255 + Math.floor(_fpNoise(i % this._w, Math.floor(i / this._w), 2));
      this._buf[i*4+3] = 255;
    }
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
  _parseColor(css) {
    if (!css || css === 'none') return [0,0,0,0];
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
    this._buf[idx+0] = Math.round(r * alpha + this._buf[idx+0] * (1 - alpha));
    this._buf[idx+1] = Math.round(g * alpha + this._buf[idx+1] * (1 - alpha));
    this._buf[idx+2] = Math.round(b * alpha + this._buf[idx+2] * (1 - alpha));
    this._buf[idx+3] = Math.min(255, Math.round(a * alpha + this._buf[idx+3] * (1 - alpha)));
  }
  fillRect(x, y, w, h) {
    const [r,g,b,a] = this._parseColor(this.fillStyle);
    x=Math.round(x); y=Math.round(y); w=Math.round(w); h=Math.round(h);
    for (let py = Math.max(0,y); py < Math.min(this._h, y+h); py++) {
      for (let px = Math.max(0,x); px < Math.min(this._w, x+w); px++) {
        this._setPixel(px, py, r, g, b, a);
      }
    }
  }
  clearRect(x, y, w, h) {
    x=Math.round(x); y=Math.round(y); w=Math.round(w); h=Math.round(h);
    for (let py = Math.max(0,y); py < Math.min(this._h, y+h); py++) {
      for (let px = Math.max(0,x); px < Math.min(this._w, x+w); px++) {
        const idx = (py * this._w + px) * 4;
        this._buf[idx] = this._buf[idx+1] = this._buf[idx+2] = this._buf[idx+3] = 0;
      }
    }
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
  }
  beginPath() { this._path = []; }
  closePath() {}
  moveTo(x, y) { if (this._path) this._path.push({t:'M',x,y}); }
  lineTo(x, y) { if (this._path) this._path.push({t:'L',x,y}); }
  bezierCurveTo() {} quadraticCurveTo() {}
  arc(x, y, r, s, e) { if (this._path) this._path.push({t:'A',x,y,r}); }
  arcTo() {}
  rect(x, y, w, h) { this.fillRect(x, y, w, h); }
  fill() {}
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
}

Element.prototype.getContext = function getContext(type) {
  if (type === '2d') {
    if (!this._ctx) {
      this._ctx = new _Canvas2D(this);
    }
    return this._ctx;
  }
  if (type === 'webgl' || type === 'experimental-webgl' || type === 'webgl2') {
    return {
      canvas: this,
      getExtension(name) {
        if (name === 'WEBGL_debug_renderer_info') return { UNMASKED_VENDOR_WEBGL: 0x9245, UNMASKED_RENDERER_WEBGL: 0x9246 };
        return null;
      },
      getParameter(pname) {
        if (pname === 0x9245) return _fp('gpuVendor');
        if (pname === 0x9246) return _fp('gpu');
        if (pname === 0x1F01) return 'WebKit WebGL';  // GL_RENDERER
        if (pname === 0x1F00) return 'WebKit';          // GL_VENDOR
        if (pname === 0x1F02) return 'OpenGL ES 3.0 (ANGLE)'; // GL_VERSION
        if (pname === 0x8B8C) return 'WebGL GLSL ES 3.00 (ANGLE)'; // GL_SHADING_LANGUAGE_VERSION
        return 0;
      },
      getSupportedExtensions() { return ['WEBGL_debug_renderer_info','EXT_texture_filter_anisotropic','WEBGL_compressed_texture_s3tc','WEBGL_lose_context']; },
      getShaderPrecisionFormat() { return { rangeMin: 127, rangeMax: 127, precision: 23 }; },
      createBuffer() { return {}; }, createShader() { return {}; }, createProgram() { return {}; },
      shaderSource() {}, compileShader() {}, attachShader() {}, linkProgram() {},
      getProgramParameter() { return true; }, useProgram() {}, deleteShader() {},
      bindBuffer() {}, bufferData() {}, enableVertexAttribArray() {}, vertexAttribPointer() {},
      drawArrays() {}, drawElements() {}, viewport() {}, clear() {}, clearColor() {},
      enable() {}, disable() {}, blendFunc() {}, depthFunc() {},
      getUniformLocation() { return {}; }, getAttribLocation() { return 0; },
      uniform1f() {}, uniform1i() {}, uniformMatrix4fv() {},
      createTexture() { return {}; }, bindTexture() {}, texImage2D() {}, texParameteri() {},
      activeTexture() {}, pixelStorei() {}, generateMipmap() {},
      createFramebuffer() { return {}; }, bindFramebuffer() {}, framebufferTexture2D() {},
      readPixels(x,y,w,h,f,t,d) { if(d) for(let i=0;i<d.length;i++) d[i]=Math.floor(Math.random()*256); },
      VERTEX_SHADER: 0x8B31, FRAGMENT_SHADER: 0x8B30, LINK_STATUS: 0x8B82,
      ARRAY_BUFFER: 0x8892, STATIC_DRAW: 0x88E4, FLOAT: 0x1406,
      TRIANGLES: 0x0004, COLOR_BUFFER_BIT: 0x4000, DEPTH_BUFFER_BIT: 0x100,
      TEXTURE_2D: 0x0DE1, RGBA: 0x1908, UNSIGNED_BYTE: 0x1401,
    };
  }
  return null;
};
Element.prototype.toDataURL = function(type) {
  if (this._ctx && this._ctx._buf) {
    const ctx = this._ctx;
    const w = ctx._w, h = ctx._h, buf = ctx._buf;
    let hash = _fpSeed;
    for (let i = 0; i < buf.length; i += 37) {
      hash = ((hash << 5) - hash + buf[i]) | 0;
    }
    const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
    let b64 = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUg';
    for (let i = 0; i < 60; i++) {
      hash = ((hash << 5) - hash + i) | 0;
      b64 += chars[(hash >>> 0) % 64];
    }
    return b64 + '==';
  }
  return _fp('canvasFingerprint');
};
Element.prototype.toBlob = function(cb, type, q) { cb(new Blob([''])); };

_markNative(Element.prototype.getContext);
_markNative(Element.prototype.toDataURL);
_markNative(Element.prototype.toBlob);

Element.prototype.attachShadow = function attachShadow(opts) {
  var _mode = opts == null ? undefined : opts.mode;
  if (_mode !== 'open' && _mode !== 'closed') {
    throw new TypeError('Failed to execute attachShadow on Element: the mode value is not a valid ShadowRootMode.');
  }
  var _ln = (this.localName || '').toLowerCase();
  if (!globalThis.__obscura_shadowHostNames.has(_ln) && _ln.indexOf('-') === -1) {
    throw new DOMException('Failed to execute attachShadow on Element: this element does not support attachShadow', 'NotSupportedError');
  }
  if (this._shadowRoot) {
    throw new DOMException('Failed to execute attachShadow on Element: the element already hosts a shadow tree.', 'NotSupportedError');
  }
  const host = this;
  const children = [];
  const shadow = {
    mode: opts.mode,
    host: host,
    get innerHTML() { return children.map(c => c.outerHTML || c.textContent || '').join(''); },
    set innerHTML(v) {
      children.length = 0;
      if (v) {
        const tmp = document.createElement('div');
        tmp.innerHTML = v;
        for (let i = 0; i < tmp.childNodes.length; i++) children.push(tmp.childNodes[i]);
      }
    },
    get childNodes() { return children; },
    get firstChild() { return children[0] || null; },
    get lastChild() { return children[children.length - 1] || null; },
    get firstElementChild() { return children.find(c => c.nodeType === 1) || null; },
    get children() { return children.filter(c => c.nodeType === 1); },
    appendChild(c) {
      if (c) {
        children.push(c);
        try { c.parentNode = shadow; } catch (_) { /* parentNode is getter-only on Node, ignore */ }
      }
      return c;
    },
    insertBefore(n, ref) {
      if (!n) return n;
      if (!ref) { shadow.appendChild(n); return n; }
      const idx = children.indexOf(ref);
      if (idx >= 0) {
        children.splice(idx, 0, n);
        try { n.parentNode = shadow; } catch (_) {}
      }
      else shadow.appendChild(n);
      return n;
    },
    removeChild(c) { const idx = children.indexOf(c); if (idx >= 0) children.splice(idx, 1); return c; },
    replaceChild(n, o) {
      const idx = children.indexOf(o);
      if (idx >= 0) {
        children[idx] = n;
        try { n.parentNode = shadow; } catch (_) {}
      }
      return o;
    },
    querySelector(s) {
      for (const c of children) {
        if (c.matches && c.matches(s)) return c;
        if (c.querySelector) { const r = c.querySelector(s); if (r) return r; }
      }
      return null;
    },
    querySelectorAll(s) {
      const results = [];
      for (const c of children) {
        if (c.matches && c.matches(s)) results.push(c);
        if (c.querySelectorAll) results.push(...c.querySelectorAll(s));
      }
      return results;
    },
    getElementById(id) { return shadow.querySelector('#' + id); },
    contains(n) { return children.includes(n); },
    getRootNode() { return shadow; },
    get ownerDocument() { return document; },
    get nodeType() { return 11; }, // DOCUMENT_FRAGMENT_NODE
    get nodeName() { return '#document-fragment'; },
    addEventListener() {}, removeEventListener() {}, dispatchEvent() { return true; },
    setHTMLUnsafe(v) { this.innerHTML = String(v == null ? "" : v); },
    getHTML() { return this.innerHTML; },
    // Own textContent: ShadowRoot now extends DocumentFragment, so without
    // these the inherited Node accessors run against this._nid. The setter in
    // particular would target the host document and wipe it. Operate on the
    // shadow's own `children` store instead.
    get textContent() { return children.map(c => c.textContent || "").join(""); },
    set textContent(v) {
      children.length = 0;
      if (v != null && v !== "") children.push(document.createTextNode(String(v)));
    },
    hasChildNodes() { return children.length > 0; },
    // A detached fragment id backs any inherited nid-based method we do not
    // override, so they stay non-destructive (operate on an empty fragment)
    // rather than falling through to node 0 / the document.
    _nid: +_dom("create_document_fragment"),
    activeElement: null,
    get styleSheets() { return []; },
    cloneNode() { throw new DOMException('Failed to execute cloneNode on Node: ShadowRoot nodes are not clonable.', 'NotSupportedError'); },
  };
  Object.setPrototypeOf(shadow, ShadowRoot.prototype);
  this._shadowRoot = shadow;
  return shadow;
};

_markNative(Element.prototype.attachShadow);

Object.defineProperty(Element.prototype, 'shadowRoot', {
  configurable: true,
  enumerable: true,
  get: function () {
    var sr = this._shadowRoot;
    return sr && sr.mode === 'open' ? sr : null;
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

globalThis.AudioContext = class AudioContext {
  constructor() { this.sampleRate=_fp('audioSampleRate'); this.state='running'; this.currentTime=0; this.baseLatency=_fp('audioBaseLatency'); this.destination={maxChannelCount:2,numberOfInputs:1,numberOfOutputs:0,channelCount:2}; }
  createOscillator() { return {type:'sine',frequency:{value:440,setValueAtTime(){}},connect(){},start(){},stop(){},disconnect(){},addEventListener(){}}; }
  createDynamicsCompressor() { return {threshold:{value:_fp('compThreshold')},knee:{value:_fp('compKnee')},ratio:{value:_fp('compRatio')},attack:{value:0.003},release:{value:0.25},reduction:0,connect(){},disconnect(){}}; }
  createAnalyser() {
    return {fftSize:2048,frequencyBinCount:1024,connect(){},disconnect(){},
      getByteFrequencyData(a){for(let i=0;i<a.length;i++)a[i]=Math.floor(_fpRand(600+i)*10);},
      getFloatFrequencyData(a){for(let i=0;i<a.length;i++)a[i]=-100+_fpRand(700+i)*5;}
    };
  }
  createGain() { return {gain:{value:1,setValueAtTime(){}},connect(){},disconnect(){}}; }
  createBiquadFilter() { return {type:'lowpass',frequency:{value:350},Q:{value:1},connect(){},disconnect(){}}; }
  createBufferSource() { return {buffer:null,connect(){},start(){},stop(){},disconnect(){},loop:false}; }
  createBuffer(ch,len,rate) { return {length:len,sampleRate:rate,numberOfChannels:ch,getChannelData(c){return new Float32Array(len);},duration:len/rate}; }
  createScriptProcessor() { return {connect(){},disconnect(){},onaudioprocess:null}; }
  decodeAudioData(buf) { return Promise.resolve(this.createBuffer(2,44100,44100)); }
  resume() { this.state='running'; return Promise.resolve(); }
  suspend() { this.state='suspended'; return Promise.resolve(); }
  close() { this.state='closed'; return Promise.resolve(); }
};
globalThis.OfflineAudioContext = class OfflineAudioContext extends AudioContext {
  constructor(ch,len,rate) { super(); this.length=len||44100; }
  startRendering() { return Promise.resolve(this.createBuffer(2,this.length,44100)); }
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

const _OrigDateTimeFormat = Intl.DateTimeFormat;
const _defaultTZ = 'Europe/Germany';
Intl.DateTimeFormat = function(locales, options) {
  if (!options) options = {};
  if (!options.timeZone) options.timeZone = _defaultTZ;
  return new _OrigDateTimeFormat(locales, options);
};
Intl.DateTimeFormat.prototype = _OrigDateTimeFormat.prototype;
Intl.DateTimeFormat.supportedLocalesOf = _OrigDateTimeFormat.supportedLocalesOf;
const _origResolved = _OrigDateTimeFormat.prototype.resolvedOptions;
_OrigDateTimeFormat.prototype.resolvedOptions = function() {
  const r = _origResolved.call(this);
  if (r.timeZone === 'UTC') r.timeZone = _defaultTZ;
  return r;
};

if (typeof PointerEvent === 'undefined') {
  globalThis.PointerEvent = class PointerEvent extends MouseEvent {
    constructor(type, opts={}) { super(type, opts); this.pointerId = opts.pointerId || 0; this.width = opts.width || 1; this.height = opts.height || 1; this.pressure = opts.pressure || 0; this.pointerType = opts.pointerType || 'mouse'; }
  };
}

if (typeof navigator.credentials === 'undefined') {
  navigator.credentials = { get(){return Promise.resolve(null);}, create(){return Promise.resolve(null);}, store(){return Promise.resolve();}, preventSilentAccess(){return Promise.resolve();} };
}

globalThis.opener = null;

globalThis.Worker = class Worker {
  constructor(url) {
    this.onmessage = null;
    this.onerror = null;
    this._terminated = false;
    this._listeners = {};
    const worker = this;

    if (typeof url === 'string' && (url.startsWith('blob:') || url.startsWith('http'))) {
      const blobContent = globalThis.__blobStore?.[url];
      if (blobContent) {
        this._code = blobContent;
      } else {
        (async () => {
          try {
            const resp = await fetch(url);
            worker._code = await resp.text();
          } catch(e) { if (worker.onerror) worker.onerror(e); }
        })();
      }
    }
  }
  postMessage(data) {
    if (this._terminated) return;
    const worker = this;
    setTimeout(() => {
      if (worker._terminated || !worker._code) return;
      try {
        const workerSelf = {
          onmessage: null,
          postMessage: (msg) => {
            const evt = { data: msg };
            if (worker.onmessage) worker.onmessage(evt);
            const handlers = worker._listeners['message'] || [];
            for (const h of handlers) h(evt);
          },
          addEventListener: (type, fn) => { workerSelf['on' + type] = fn; },
          close: () => { worker._terminated = true; },
          crypto: globalThis.crypto,
          TextEncoder: globalThis.TextEncoder,
          TextDecoder: globalThis.TextDecoder,
          atob: globalThis.atob,
          btoa: globalThis.btoa,
          setTimeout: globalThis.setTimeout,
          setInterval: globalThis.setInterval,
          clearTimeout: globalThis.clearTimeout,
          clearInterval: globalThis.clearInterval,
          fetch: globalThis.fetch,
          console: globalThis.console,
        };
        const fn = new Function('self', 'postMessage', 'addEventListener', 'close', worker._code);
        fn(workerSelf, workerSelf.postMessage, workerSelf.addEventListener, workerSelf.close);
        if (workerSelf.onmessage) workerSelf.onmessage({ data });
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
const _origCreateObjectURL = URL.createObjectURL;
URL.createObjectURL = function(blob) {
  if (blob && typeof blob.text === 'function') {
    const id = 'blob:obscura/' + Math.random().toString(36).substring(2);
    blob.text().then(text => { globalThis.__blobStore[id] = text; });
    return id;
  }
  return 'blob:obscura/fallback';
};
URL.revokeObjectURL = function(url) {
  delete globalThis.__blobStore[url];
};

globalThis.scrollTo = function(x, y) {};
globalThis.scrollBy = function(x, y) {};
globalThis.scroll = function(x, y) {};
globalThis.focus = function() {};
globalThis.blur = function() {};
globalThis.print = function() {};
globalThis.alert = function() {};
globalThis.confirm = function() { return true; };
globalThis.prompt = function() { return null; };
globalThis.open = function() { return null; };
globalThis.close = function() {};
globalThis.stop = function() {};
globalThis.postMessage = function() {};
globalThis.requestIdleCallback = globalThis.requestIdleCallback || function(cb) { return setTimeout(cb, 0); };
globalThis.cancelIdleCallback = globalThis.cancelIdleCallback || function(id) { clearTimeout(id); };
if (typeof ReadableStream === 'undefined') {
  globalThis.ReadableStream = class ReadableStream {
    constructor(source = {}, strategy = {}) {
      this._source = source; this._queue = []; this._closed = false;
      this.locked = false;
      if (source.start) source.start({ enqueue: (chunk) => this._queue.push(chunk), close: () => { this._closed = true; }, error: () => {} });
    }
    getReader() {
      this.locked = true;
      const stream = this;
      return {
        read() {
          if (stream._queue.length > 0) return Promise.resolve({ value: stream._queue.shift(), done: false });
          if (stream._closed) return Promise.resolve({ value: undefined, done: true });
          return Promise.resolve({ value: undefined, done: true });
        },
        releaseLock() { stream.locked = false; },
        cancel() { stream._closed = true; return Promise.resolve(); },
        get closed() { return stream._closed ? Promise.resolve() : new Promise(() => {}); },
      };
    }
    cancel() { this._closed = true; return Promise.resolve(); }
    pipeTo(dest) { return Promise.resolve(); }
    pipeThrough(transform) { return transform.readable || new ReadableStream(); }
    tee() { return [new ReadableStream(), new ReadableStream()]; }
    [Symbol.asyncIterator]() {
      const reader = this.getReader();
      return { next: () => reader.read(), return: () => { reader.releaseLock(); return Promise.resolve({done:true}); } };
    }
  };
}
if (typeof WritableStream === 'undefined') {
  globalThis.WritableStream = class WritableStream {
    constructor(sink = {}) { this._sink = sink; this.locked = false; }
    getWriter() {
      this.locked = true;
      const stream = this;
      return {
        write(chunk) { if (stream._sink.write) stream._sink.write(chunk); return Promise.resolve(); },
        close() { if (stream._sink.close) stream._sink.close(); return Promise.resolve(); },
        abort() { return Promise.resolve(); },
        releaseLock() { stream.locked = false; },
        get ready() { return Promise.resolve(); },
        get closed() { return Promise.resolve(); },
        get desiredSize() { return 1; },
      };
    }
    close() { return Promise.resolve(); }
    abort() { return Promise.resolve(); }
  };
}
if (typeof TransformStream === 'undefined') {
  globalThis.TransformStream = class TransformStream {
    constructor(transformer = {}) {
      this.readable = new ReadableStream();
      this.writable = new WritableStream();
    }
  };
}

if (!globalThis.crypto) globalThis.crypto = {};
if (!globalThis.crypto.subtle) {
  globalThis.crypto.subtle = {
    async digest(algorithm, data) {
      // Real WebCrypto digest. Delegates to `op_subtle_digest` which runs
      // the actual SHA-1/256/384/512 via Rust's `sha1` and `sha2` crates.
      // The previous JS implementation was a custom FNV variant that
      // produced bytes shaped like the hash but with wrong contents, so
      // SRI checks, JWS signature verification, and OAuth PKCE silently
      // accepted invalid input.
      const name = (typeof algorithm === 'string' ? algorithm : algorithm?.name) || 'SHA-256';
      let bytes;
      if (data instanceof ArrayBuffer) bytes = new Uint8Array(data);
      else if (ArrayBuffer.isView(data)) bytes = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
      else bytes = new Uint8Array(data || []);
      const out = Deno.core.ops.op_subtle_digest(name, bytes);
      return new Uint8Array(out).buffer;
    },
    async encrypt() { throw new DOMException('NotSupportedError'); },
    async decrypt() { throw new DOMException('NotSupportedError'); },
    async sign() { return new ArrayBuffer(32); },
    async verify() { return true; },
    async generateKey() { return { type: 'secret', algorithm: {}, extractable: false, usages: [] }; },
    async importKey() { return { type: 'secret', algorithm: {}, extractable: false, usages: [] }; },
    async exportKey() { return new ArrayBuffer(32); },
    async deriveBits() { return new ArrayBuffer(32); },
    async deriveKey() { return { type: 'secret', algorithm: {}, extractable: false, usages: [] }; },
    async wrapKey() { return new ArrayBuffer(32); },
    async unwrapKey() { return { type: 'secret', algorithm: {}, extractable: false, usages: [] }; },
  };
}

if (typeof DOMRect === 'undefined') {
  globalThis.DOMRect = class DOMRect {
    constructor(x=0,y=0,w=0,h=0) { this.x=x;this.y=y;this.width=w;this.height=h;this.top=y;this.right=x+w;this.bottom=y+h;this.left=x; }
    toJSON() { return {x:this.x,y:this.y,width:this.width,height:this.height,top:this.top,right:this.right,bottom:this.bottom,left:this.left}; }
    static fromRect(r={}) { return new DOMRect(r.x,r.y,r.width,r.height); }
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
  globalThis.Image = class Image {
    constructor(w, h) { this.width = w || 0; this.height = h || 0; this.src = ''; this.onload = null; this.onerror = null; this.complete = false; this.naturalWidth = 0; this.naturalHeight = 0; }
    addEventListener() {} removeEventListener() {}
    setAttribute(k, v) { this[k] = v; if (k === 'src' && this.onload) setTimeout(() => { this.complete = true; this.onload(); }, 0); }
    getAttribute(k) { return this[k]; }
  };
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
    constructor() { this.result = null; this.readyState = 0; this.onload = null; this.onerror = null; }
    readAsText(blob) { if (blob?.text) blob.text().then(t => { this.result = t; this.readyState = 2; if (this.onload) this.onload({target:this}); }); }
    readAsDataURL(blob) { this.result = 'data:;base64,'; this.readyState = 2; if (this.onload) setTimeout(() => this.onload({target:this}), 0); }
    readAsArrayBuffer(blob) { this.result = new ArrayBuffer(0); this.readyState = 2; if (this.onload) setTimeout(() => this.onload({target:this}), 0); }
    abort() { this.readyState = 0; }
    addEventListener(t, fn) { if (t === 'load') this.onload = fn; }
    removeEventListener() {}
  };
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
  globalThis.BroadcastChannel = class BroadcastChannel {
    constructor(name) {
      this.name = name; this.onmessage = null; this.onmessageerror = null;
      _makeListenerBox(this);
    }
    postMessage(msg) {}
    close() {}
  };
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

if (typeof NodeFilter === 'undefined') {
  globalThis.NodeFilter = { SHOW_ALL:0xFFFFFFFF, SHOW_ELEMENT:1, SHOW_TEXT:4, SHOW_COMMENT:128,
    FILTER_ACCEPT:1, FILTER_REJECT:2, FILTER_SKIP:3 };
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
    getClientRects(){return [];} cloneRange(){return new Range();} toString(){return '';}
  };
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
  _fpSeed = Date.now() ^ (Math.random() * 0xFFFFFFFF >>> 0);
  _fpCache = null;
  _installWasmStreamingFallback();

  globalThis.document = new Document(+_dom("document_node_id"));

  const scr = _fp('screen');
  const sw = scr[0], sh = scr[1];
  globalThis.screen = { width:sw, height:sh, availWidth:sw, availHeight:sh-40, colorDepth:24, pixelDepth:24, availTop:0, availLeft:0, orientation:{type:"landscape-primary",angle:0,addEventListener(){},removeEventListener(){},dispatchEvent(){return true;}} };
  globalThis.visualViewport = { width:sw, height:sh-80, offsetLeft:0, offsetTop:0, scale:1, addEventListener(){}, removeEventListener(){} };
  globalThis.devicePixelRatio = sw >= 2560 ? 2 : 1;
  globalThis.innerWidth = sw; globalThis.innerHeight = sh - 80;
  globalThis.outerWidth = sw; globalThis.outerHeight = sh;

  var hwValues = [2, 4, 6, 8, 12, 16];
  globalThis.navigator.hardwareConcurrency = hwValues[Math.floor(_fpRand(400) * hwValues.length)];
  var memValues = [0.25, 0.5, 1, 2, 4, 8];
  globalThis.navigator.deviceMemory = memValues[Math.floor(_fpRand(401) * memValues.length)];

  const t0 = Date.now() + Math.floor(_fpRand(641) * 100) - 50;
  globalThis.performance.timeOrigin = t0;
  globalThis.performance.timing = { navigationStart: t0, domContentLoadedEventEnd: t0, loadEventEnd: t0 };
  globalThis.performance.memory = {
    jsHeapSizeLimit: 2172649472,
    totalJSHeapSize: 15000000 + Math.floor(_fpRand(620) * 85000000),
    usedJSHeapSize: 8000000 + Math.floor(_fpRand(621) * 42000000),
  };
  globalThis.Notification.permission = _fpRand(630) > 0.5 ? "granted" : "default";

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
globalThis.__obscura_hide_list = Object.keys(globalThis).filter(k =>
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
