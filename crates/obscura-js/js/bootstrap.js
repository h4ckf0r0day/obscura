"use strict";

globalThis.__obscura_errors = [];
globalThis.__obscura_console = [];
globalThis.__obscura_fetch_log = [];
globalThis.__obscura_dom_log = [];

function __obscuraRecordFetch(entry) {
  try {
    const log = globalThis.__obscura_fetch_log || (globalThis.__obscura_fetch_log = []);
    log.push(entry);
    if (log.length > 80) log.splice(0, log.length - 80);
  } catch(e) {}
}

function __obscuraRecordDOM(entry) {
  try {
    const log = globalThis.__obscura_dom_log || (globalThis.__obscura_dom_log = []);
    log.push(entry);
    if (log.length > 120) log.splice(0, log.length - 120);
  } catch(e) {}
}

globalThis.__obscura_set_current_script = function(nid) {
  try {
    const id = Number(nid || 0);
    document._currentScript = id > 0 ? _wrapEl(id) : null;
  } catch(e) {
    try { document._currentScript = null; } catch(_) {}
  }
};

function _invokeEventHandler(handler, thisArg, event) {
  if (typeof handler === "function") return handler.call(thisArg, event);
  if (handler && typeof handler.handleEvent === "function") return handler.handleEvent.call(handler, event);
}

globalThis.addEventListener = globalThis.addEventListener || function(){};
globalThis.onunhandledrejection = function(e) {
  globalThis.__obscura_errors.push({
    msg: "unhandledrejection",
    src: "",
    line: 0,
    error: String(e?.reason?.stack || e?.reason?.message || e?.reason || ""),
  });
  if (e?.preventDefault) e.preventDefault();
};

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
  for (const h of handlers) { try { _invokeEventHandler(h, globalThis, event); } catch(e) { console.error(e); } }
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

class DOMStringMap {}
globalThis.DOMStringMap = DOMStringMap;

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
function _asNodeList(items) {
  const list = Array.from(items || []).filter(Boolean);
  list.item = (i) => list[i] || null;
  list.forEach = Array.prototype.forEach.bind(list);
  return list;
}
function _scopedQuerySelectorAll(root, selector) {
  if (!root || !globalThis.document) return _asNodeList([]);
  const all = Document.prototype.querySelectorAll.call(globalThis.document, selector);
  return _asNodeList(Array.from(all).filter(el => el !== root && root.contains && root.contains(el)));
}
const _consoleFn = (level, args) => {
  const msg = args.map(a => {
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
  }).join(" ");
  try {
    if (level === "error" || level === "warn") {
      globalThis.__obscura_console.push({ level, msg });
      if (globalThis.__obscura_console.length > 200) globalThis.__obscura_console.shift();
    }
  } catch {}
  try { Deno.core.ops.op_console_msg(level, msg); } catch {}
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
const _pendingTimers = new Map();
const _clearedTimers = new Set();
const _normalizeDelay = (delay) => {
  const n = Number(delay);
  if (!Number.isFinite(n) || n <= 0) return 0;
  return Math.min(Math.floor(n), 60000);
};
const _sleep = (delay) => {
  const ms = _normalizeDelay(delay);
  return ms === 0 ? Promise.resolve() : Deno.core.ops.op_sleep(ms);
};

globalThis.setTimeout = (fn, delay = 0, ...args) => {
  if (typeof fn !== "function") return ++_tid;
  const id = ++_tid;
  _pendingTimers.set(id, { fn, args, delay, interval: false });
  _sleep(delay).then(() => {
    if (!_clearedTimers.has(id) && _pendingTimers.has(id)) {
      _pendingTimers.delete(id);
      try { fn(...args); } catch(e) { console.error("Timer error:", e); }
    }
  });
  return id;
};

globalThis.clearTimeout = (id) => { _clearedTimers.add(id); _pendingTimers.delete(id); };
globalThis.setInterval = (fn, delay, ...args) => {
  if (typeof fn !== "function") return ++_tid;
  const id = ++_tid;
  const tick = () => {
    _sleep(delay).then(() => {
      if (_clearedTimers.has(id) || !_pendingTimers.has(id)) return;
      try { fn(...args); } catch(e) { console.error("Interval error:", e); }
      if (!_clearedTimers.has(id) && _pendingTimers.has(id)) tick();
    });
  };
  _pendingTimers.set(id, { fn, args, delay, interval: true });
  tick();
  return id;
};
globalThis.clearInterval = globalThis.clearTimeout;
globalThis.requestAnimationFrame = (fn) => setTimeout(() => fn(globalThis.performance?.now ? globalThis.performance.now() : Date.now()), 16);
globalThis.cancelAnimationFrame = globalThis.clearTimeout;
globalThis.queueMicrotask = globalThis.queueMicrotask || ((fn) => Promise.resolve().then(fn));
globalThis.scheduler = globalThis.scheduler || {
  postTask(callback, options = {}) {
    const run = () => {
      if (typeof callback !== "function") return undefined;
      return callback();
    };
    if (options?.signal?.aborted) return Promise.reject(options.signal.reason || new DOMException("AbortError"));
    return Promise.resolve().then(run);
  },
  yield() {
    return Promise.resolve();
  },
};
globalThis.TaskController = globalThis.TaskController || class TaskController {
  constructor() { this.signal = { aborted: false, reason: undefined, priority: "user-visible", addEventListener(){}, removeEventListener(){} }; }
  abort(reason) { this.signal.aborted = true; this.signal.reason = reason; }
  setPriority(priority) { this.signal.priority = priority; }
};
globalThis.TaskSignal = globalThis.TaskSignal || class TaskSignal {};

class MessagePort {
  constructor() {
    this.onmessage = null;
    this._listeners = {};
    this._entangledPort = null;
    this._closed = false;
  }
  postMessage(data) {
    const target = this._entangledPort;
    if (!target || target._closed) return;
    Promise.resolve().then(() => {
      if (!target._closed) target._dispatchMessage(data);
    });
  }
  start() {}
  close() { this._closed = true; }
  addEventListener(type, handler) {
    if (!handler) return;
    if (!this._listeners[type]) this._listeners[type] = [];
    this._listeners[type].push(handler);
  }
  removeEventListener(type, handler) {
    if (!this._listeners[type]) return;
    this._listeners[type] = this._listeners[type].filter(h => h !== handler);
  }
  dispatchEvent(event) {
    if (!event) return true;
    event.target = this;
    event.currentTarget = this;
    const handlers = this._listeners[event.type] || [];
    for (const h of handlers) { try { _invokeEventHandler(h, this, event); } catch(e) { console.error(e); } }
    const prop = "on" + event.type;
    if (typeof this[prop] === "function") {
      try { this[prop].call(this, event); } catch(e) { console.error(e); }
    }
    return !event.defaultPrevented;
  }
  _dispatchMessage(data) {
    this.dispatchEvent(new MessageEvent("message", { data }));
  }
}
class MessageChannel {
  constructor() {
    this.port1 = new MessagePort();
    this.port2 = new MessagePort();
    this.port1._entangledPort = this.port2;
    this.port2._entangledPort = this.port1;
  }
}
globalThis.MessageChannel = MessageChannel;
globalThis.MessagePort = MessagePort;

function _bootloaderHashFor(el) {
  if (!el || typeof el.getAttribute !== "function") return "";
  return el.getAttribute("data-bootloader-hash-client")
    || el.getAttribute("data-bootloader-hash")
    || "";
}

function _notifyBootloaderResourceDone(el) {
  const hash = _bootloaderHashFor(el);
  if (!hash) return;

  try {
    const loaded = globalThis._btldr || (globalThis._btldr = {});
    loaded[hash] = 1;
  } catch(e) {}
}

function _fireElementLoad(el) {
  if (!el) return;
  try { el.dispatchEvent(new Event('load')); } catch(e) {}
  _notifyBootloaderResourceDone(el);
}

function _fireElementError(el, error) {
  if (!el) return;
  try {
    const event = new Event('error');
    event.error = error;
    el.dispatchEvent(event);
  } catch(e) {}
}

globalThis.__obscura_finish_resource_node = function(nid, failed, message) {
  try {
    const el = _wrapEl(Number(nid || 0));
    if (!el) return;
    if (failed) {
      const error = new Error(message || "Resource load failed");
      _fireElementError(el, error);
    } else {
      _fireElementLoad(el);
    }
  } catch(e) {}
};

function _handleInsertedResourceElement(el) {
  if (!(el instanceof Element) || !el.isConnected) return;

  if (el.tagName === 'SCRIPT') {
    const scriptType = el.getAttribute('type') || '';
    if (scriptType && scriptType !== 'text/javascript' && scriptType !== 'application/javascript' && scriptType !== 'module') {
      return;
    }
    const src = el.getAttribute('src');
    if (src) {
      if (el._resourceLoadingStarted === src) return;
      el._resourceLoadingStarted = src;
      const fullUrl = src.startsWith('http') ? src : new URL(src, globalThis.location?.href || 'http://localhost/').href;
      const pageOrigin = (function() { try { return new URL(globalThis.location?.href || "about:blank").origin; } catch(e) { return ""; } })();
      (async () => {
        try {
          const raw = await Deno.core.ops.op_fetch_url(fullUrl, "GET", "{}", "", pageOrigin, "no-cors");
          const parsed = JSON.parse(raw);
          if (parsed.body) {
            try {
              document._currentScript = el;
              (0, eval)(parsed.body);
            } catch(e) {
              console.error('Dynamic script error (' + fullUrl + '):', e.message);
            } finally {
              document._currentScript = null;
            }
          }
          _fireElementLoad(el);
        } catch(e) {
          console.error('Dynamic script fetch error:', e.message);
          _fireElementError(el, e);
        }
      })();
    } else {
      const code = el.textContent;
      if (code) {
        if (el._resourceLoadingStarted === "inline") return;
        el._resourceLoadingStarted = "inline";
        try {
          document._currentScript = el;
          (0, eval)(code);
        } catch(e) {
          console.error('Dynamic inline script error:', e.message);
        } finally {
          document._currentScript = null;
        }
      }
      setTimeout(() => _fireElementLoad(el), 0);
    }
    return;
  }

  if (_isStylesheetLink(el)) {
    _makeStyleSheetFor(el);
    const href = el.href || el.getAttribute('href');
    if (!href) {
      return;
    }
    if (el._resourceLoadingStarted === href) return;
    el._resourceLoadingStarted = href;
    const fullUrl = _resolveUrl(href);
    fetch(fullUrl, { mode: 'no-cors' })
      .then(() => _fireElementLoad(el))
      .catch((error) => _fireElementError(el, error));
  }
}

function _queueResourceLoadIfConnected(el) {
  if (!(el instanceof Element) || !el.isConnected) return;
  const localName = el.localName;
  if (localName === "script" && (el.getAttribute("src") || el.textContent)) {
    setTimeout(() => { if (el.isConnected) _handleInsertedResourceElement(el); }, 0);
  } else if (localName === "link" && _isStylesheetLink(el) && el.getAttribute("href")) {
    setTimeout(() => { if (el.isConnected) _handleInsertedResourceElement(el); }, 0);
  }
}

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
    const children = _domParse("child_nodes", this._nid) || [];
    for (const c of children) _dom("remove_child", c);
    if (v != null && v !== "") {
      const tn = +_dom("create_text_node", String(v));
      _dom("append_child", this._nid, tn);
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
  get isConnected() {
    let node = this;
    while (node) {
      if (node.nodeType === 9) return true;
      node = node.parentNode;
    }
    return false;
  }
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
    if (c.nodeType === 11) {
      const nodes = Array.from(c.childNodes || []);
      for (const child of nodes) this.appendChild(child);
      return c;
    }
    _dom("append_child", this._nid, c._nid);
    if (globalThis.__mutationObservers?.length) globalThis.__notifyMutation('childList', this._nid, [c._nid], []);
    if (c instanceof Element) _handleInsertedResourceElement(c);
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
    if (newChild.nodeType === 11) {
      const nodes = Array.from(newChild.childNodes || []);
      for (const child of nodes) this.insertBefore(child, oldChild);
      this.removeChild(oldChild);
      return oldChild;
    }
    _dom("insert_before", newChild._nid, oldChild._nid);
    _dom("remove_child", oldChild._nid);
    if (globalThis.__mutationObservers?.length) globalThis.__notifyMutation('childList', this._nid, [newChild._nid], [oldChild._nid]);
    if (newChild instanceof Element) _handleInsertedResourceElement(newChild);
    return oldChild;
  }
  insertBefore(n, ref) {
    if (!n) return n;
    if (!ref) { this.appendChild(n); return n; }
    if (n.nodeType === 11) {
      const nodes = Array.from(n.childNodes || []);
      for (const child of nodes) this.insertBefore(child, ref);
      return n;
    }
    _dom("insert_before", n._nid, ref._nid);
    if (globalThis.__mutationObservers?.length) globalThis.__notifyMutation('childList', this._nid, [n._nid], []);
    if (n instanceof Element) _handleInsertedResourceElement(n);
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
  isEqualNode(other) { return other && this._nid === other._nid; }
  isSameNode(other) { return other && this._nid === other._nid; }
  addEventListener(type, handler, opts) {
    if (!handler) return;
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
    const handlers = (_eventRegistry[this._nid] || {})[event.type] || [];
    for (const h of handlers) { try { _invokeEventHandler(h, this, event); } catch(e) { console.error(e); } }
    const prop = "on" + event.type;
    if (typeof this[prop] === "function") {
      try { _invokeEventHandler(this[prop], this, event); } catch(e) { console.error(e); }
    }
    if (event.bubbles && !event.defaultPrevented && this.parentNode) {
      this.parentNode.dispatchEvent(event);
    }
    return !event.defaultPrevented;
  }
}
class CharacterData extends Node {
  get data() {
    return _domParse("text_content", this._nid) ?? "";
  }
  set data(v) {
    _dom("set_text_content", this._nid, String(v ?? ""));
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
    const tag = this.localName;
    if (tag === "svg" || this._ns === "http://www.w3.org/2000/svg") return "http://www.w3.org/2000/svg";
    return "http://www.w3.org/1999/xhtml";
  }
  get innerHTML() { return _domParse("inner_html", this._nid) ?? ""; }
  set innerHTML(v) {
    __obscuraRecordDOM({
      op: "setInnerHTML",
      tag: this.tagName,
      id: this.id || "",
      className: (this.className || "").slice(0, 80),
      length: String(v ?? "").length,
      prefix: String(v ?? "").slice(0, 160),
    });
    if (this.localName === 'template') {
      this.content.innerHTML = v;
      return;
    }
    _dom("set_inner_html", this._nid, String(v ?? ""));
  }
  get outerHTML() { return _domParse("outer_html", this._nid) ?? ""; }
  get innerText() {
    const tag = this.localName;
    if (tag === "script" || tag === "style" || tag === "template" || tag === "noscript") return "";
    let text = "";
    for (const child of this.childNodes || []) {
      if (!child) continue;
      if (child.nodeType === 3) text += child.textContent || "";
      else if (child.nodeType === 1) text += child.innerText || "";
    }
    return text;
  }
  set innerText(v) { this.textContent = v; }
  get children() {
    const ids = _domParse("element_children", this._nid) || [];
    return ids.map(_wrapEl).filter(Boolean);
  }
  get content() {
    if (this.localName !== 'template') return undefined;
    if (!this._templateContent) this._templateContent = document.createDocumentFragment();
    return this._templateContent;
  }
  get childElementCount() { return this.children.length; }
  get firstElementChild() { return this.children[0] || null; }
  get lastElementChild() { const ch = this.children; return ch[ch.length-1] || null; }
  get nextElementSibling() { let s = this.nextSibling; while(s && s.nodeType !== 1) s = s.nextSibling; return s; }
  get previousElementSibling() { let s = this.previousSibling; while(s && s.nodeType !== 1) s = s.previousSibling; return s; }
  get classList() {
    const el = this;
    const obj = {
      add: (...c) => { const s = new Set((el.className||"").split(/\s+/).filter(Boolean)); c.forEach(x=>s.add(x)); el.className=[...s].join(" "); },
      remove: (...c) => { const s = new Set((el.className||"").split(/\s+/).filter(Boolean)); c.forEach(x=>s.delete(x)); el.className=[...s].join(" "); },
      contains: c => (el.className||"").split(/\s+/).includes(c),
      toggle: (c, force) => { const has = obj.contains(c); if(force===true||(!has&&force!==false)){obj.add(c);return true;} obj.remove(c); return false; },
      get length() { return (el.className||"").split(/\s+/).filter(Boolean).length; },
      item: i => (el.className||"").split(/\s+/).filter(Boolean)[i] || null,
      forEach: (cb) => (el.className||"").split(/\s+/).filter(Boolean).forEach(cb),
      toString: () => el.className || "",
    };
    return obj;
  }
  get style() { return this._style; }
  set style(v) { if (typeof v === "string") this._style.cssText = v; }
  getAttribute(n) { return _domParse("get_attribute", this._nid, n); }
  setAttribute(n, v) {
    _dom("set_attribute", this._nid, n + "\0" + String(v));
    if (globalThis.__mutationObservers?.length) globalThis.__notifyMutation('attributes', this._nid, [], [], n);
    const lowerName = String(n || "").toLowerCase();
    if (
      (this.localName === "script" && (lowerName === "src" || lowerName === "type")) ||
      (this.localName === "link" && (lowerName === "href" || lowerName === "rel" || lowerName === "as"))
    ) {
      _queueResourceLoadIfConnected(this);
    }
  }
  setAttributeNS(ns, n, v) { this.setAttribute(n, v); } // Simplified NS handling
  removeAttribute(n) { _dom("remove_attribute", this._nid, n); }
  removeAttributeNS(ns, n) { this.removeAttribute(n); }
  hasAttribute(n) { return this.getAttribute(n) !== null; }
  hasAttributes() { return true; } // Simplified
  getAttributeNS(ns, n) { return this.getAttribute(n); }
  querySelector(s) {
    const list = this.querySelectorAll(s);
    return list[0] || null;
  }
  querySelectorAll(s) { return _scopedQuerySelectorAll(this, s); }
  getElementsByTagName(t) { return this.querySelectorAll(t); }
  getElementsByClassName(c) { return this.querySelectorAll("." + c); }
  matches(s) {
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
    __obscuraRecordDOM({
      op: "insertAdjacentHTML",
      tag: this.tagName,
      id: this.id || "",
      className: (this.className || "").slice(0, 80),
      position,
      length: String(html ?? "").length,
      prefix: String(html ?? "").slice(0, 160),
    });
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
    if (!handler) return;
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
    const handlers = (_eventRegistry[this._nid] || {})[event.type] || [];
    for (const h of handlers) { try { _invokeEventHandler(h, this, event); } catch(e) { console.error(e); } }
    const prop = "on" + event.type;
    if (typeof this[prop] === "function") {
      try { _invokeEventHandler(this[prop], this, event); } catch(e) { console.error(e); }
    }
    if (event.bubbles && !event.defaultPrevented && this.parentNode) {
      this.parentNode.dispatchEvent(event);
    }
    return !event.defaultPrevented;
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
  get value() {
    if (_formValues[this._nid] !== undefined) return _formValues[this._nid];
    const tag = this.localName;
    if (tag === 'textarea') return this.textContent;
    return this.getAttribute("value") || "";
  }
  set value(v) {
    _formValues[this._nid] = String(v);
    const tag = this.localName;
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
  set href(v) {
    this.setAttribute("href", v);
  }
  get rel() { return this.getAttribute("rel") || ""; }
  set rel(v) {
    this.setAttribute("rel", v);
  }
  get as() { return this.getAttribute("as") || ""; }
  set as(v) {
    this.setAttribute("as", v);
  }
  get media() { return this.getAttribute("media") || ""; }
  set media(v) { this.setAttribute("media", v); }
  get crossOrigin() { return this.getAttribute("crossorigin") || ""; }
  set crossOrigin(v) {
    if (v == null) this.removeAttribute("crossorigin");
    else this.setAttribute("crossorigin", v);
  }
  get integrity() { return this.getAttribute("integrity") || ""; }
  set integrity(v) { this.setAttribute("integrity", v); }
  get nonce() { return this.getAttribute("nonce") || ""; }
  set nonce(v) { this.setAttribute("nonce", v); }
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
      _finishIframeLoad(el, fullUrl);
    }).catch(() => {
      el._iframeDoc = new _IframeDocument('<!DOCTYPE html><html><head></head><body></body></html>', fullUrl, el);
      el._iframeWin = new _IframeWindow(el._iframeDoc, fullUrl);
      _finishIframeLoad(el, fullUrl);
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
  get action() { return this.getAttribute("action") || ""; }
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
    return this.querySelectorAll('option');
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
    return new Proxy(new DOMStringMap(), {
      get(_, k) { if(typeof k!=="string")return undefined; return el.getAttribute("data-"+k.replace(/([A-Z])/g,"-$1").toLowerCase()); },
      set(_, k, v) { el.setAttribute("data-"+k.replace(/([A-Z])/g,"-$1").toLowerCase(), v); return true; },
    });
  }
  _isBlockLikeForLayout() {
    const tag = this.localName;
    return this === document.documentElement || this === document.body ||
      this.id?.startsWith('mount_') ||
      ['div','main','section','article','nav','header','footer','form','ul','ol','li'].includes(tag);
  }
  get offsetWidth() { return this.clientWidth; } get offsetHeight() { return this.clientHeight; }
  get offsetTop() { return 0; } get offsetLeft() { return 0; }
  get clientWidth() {
    if (this === document.documentElement || this === document.body) return globalThis.innerWidth || 1920;
    if (this._isBlockLikeForLayout()) return Math.max(320, globalThis.innerWidth || 1920);
    return 100;
  }
  get clientHeight() {
    if (this === document.documentElement || this === document.body) return globalThis.innerHeight || 1000;
    if (this._isBlockLikeForLayout()) return Math.max(20, Math.min(globalThis.innerHeight || 1000, 1000));
    return 20;
  }
  get scrollWidth() {
    if (this === document.documentElement || this === document.body) return Math.max(this.clientWidth, globalThis.innerWidth || 1920);
    return this.clientWidth;
  }
  get scrollHeight() {
    if (this === document.documentElement || this === document.body) return Math.max(this.clientHeight, 6000);
    return this.clientHeight;
  }
  get scrollTop() {
    if (this === document.documentElement || this === document.body) return globalThis.__scrollY || 0;
    return this._scrollTop || 0;
  }
  set scrollTop(v) {
    if (this === document.documentElement || this === document.body) {
      _setWindowScroll(globalThis.__scrollX || 0, Number(v) || 0);
    } else {
      this._scrollTop = Number(v) || 0;
      this.dispatchEvent(new Event('scroll'));
    }
  }
  get scrollLeft() {
    if (this === document.documentElement || this === document.body) return globalThis.__scrollX || 0;
    return this._scrollLeft || 0;
  }
  set scrollLeft(v) {
    if (this === document.documentElement || this === document.body) {
      _setWindowScroll(Number(v) || 0, globalThis.__scrollY || 0);
    } else {
      this._scrollLeft = Number(v) || 0;
      this.dispatchEvent(new Event('scroll'));
    }
  }
  getBoundingClientRect() {
    globalThis.__obscura_click_target = this;
    const width = this.clientWidth || 100;
    const height = this.clientHeight || 20;
    return {x:0,y:0,width,height,top:0,right:width,bottom:height,left:0,toJSON(){return this;}};
  }
  getClientRects() { return [this.getBoundingClientRect()]; }
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
  after(...nodes) {
    const parent = this.parentNode;
    if (!parent) return;
    const ref = this.nextSibling;
    for (const n of nodes) {
      parent.insertBefore(typeof n === "string" ? document.createTextNode(n) : n, ref);
    }
  }
  before(...nodes) {
    const parent = this.parentNode;
    if (!parent) return;
    for (const n of nodes) {
      parent.insertBefore(typeof n === "string" ? document.createTextNode(n) : n, this);
    }
  }
  remove() { if (this.parentNode) this.parentNode.removeChild(this); }
  append(...nodes) { for (const n of nodes) { if (typeof n === "string") this.appendChild(document.createTextNode(n)); else this.appendChild(n); } }
  prepend(...nodes) {
    const ref = this.firstChild;
    for (const n of nodes) {
      this.insertBefore(typeof n === "string" ? document.createTextNode(n) : n, ref);
    }
  }
}

class HTMLElement extends Element {}
class HTMLDivElement extends HTMLElement {}
class HTMLSpanElement extends HTMLElement {}
class HTMLParagraphElement extends HTMLElement {}
class HTMLAnchorElement extends HTMLElement {}
class HTMLImageElement extends HTMLElement {}
class HTMLInputElement extends HTMLElement {}
class HTMLButtonElement extends HTMLElement {}
class HTMLFormElement extends HTMLElement {}
class HTMLSelectElement extends HTMLElement {}
class HTMLTextAreaElement extends HTMLElement {}
class HTMLLabelElement extends HTMLElement {}
class HTMLTableElement extends HTMLElement {}
class HTMLIFrameElement extends HTMLElement {}
class HTMLCanvasElement extends HTMLElement {}
class HTMLVideoElement extends HTMLElement {}
class HTMLAudioElement extends HTMLElement {}
class HTMLScriptElement extends HTMLElement {
  static supports(type) {
    return ["classic", "module", "importmap", "speculationrules"].includes(String(type || "").toLowerCase());
  }
  get text() { return this.textContent; }
  set text(value) { this.textContent = value; }
}
class HTMLStyleElement extends HTMLElement {}
class HTMLLinkElement extends HTMLElement {}
class HTMLMetaElement extends HTMLElement {}
class HTMLHeadElement extends HTMLElement {}
class HTMLBodyElement extends HTMLElement {}
class HTMLHtmlElement extends HTMLElement {}
class HTMLBRElement extends HTMLElement {}
class HTMLHRElement extends HTMLElement {}
class HTMLUListElement extends HTMLElement {}
class HTMLOListElement extends HTMLElement {}
class HTMLLIElement extends HTMLElement {}
class HTMLPreElement extends HTMLElement {}
class HTMLHeadingElement extends HTMLElement {}
class HTMLTemplateElement extends HTMLElement {}
class HTMLSlotElement extends HTMLElement {}
class HTMLOptionElement extends HTMLElement {}
class HTMLDataListElement extends HTMLElement {}
class HTMLFieldSetElement extends HTMLElement {}
class HTMLLegendElement extends HTMLElement {}
class HTMLProgressElement extends HTMLElement {}
class HTMLDetailsElement extends HTMLElement {}
class HTMLDialogElement extends HTMLElement {}
class SVGElement extends Element {}
class SVGSVGElement extends SVGElement {}

const _htmlElementConstructors = {
  a: HTMLAnchorElement,
  area: HTMLAnchorElement,
  audio: HTMLAudioElement,
  body: HTMLBodyElement,
  br: HTMLBRElement,
  button: HTMLButtonElement,
  canvas: HTMLCanvasElement,
  datalist: HTMLDataListElement,
  details: HTMLDetailsElement,
  dialog: HTMLDialogElement,
  div: HTMLDivElement,
  fieldset: HTMLFieldSetElement,
  form: HTMLFormElement,
  h1: HTMLHeadingElement,
  h2: HTMLHeadingElement,
  h3: HTMLHeadingElement,
  h4: HTMLHeadingElement,
  h5: HTMLHeadingElement,
  h6: HTMLHeadingElement,
  head: HTMLHeadElement,
  hr: HTMLHRElement,
  html: HTMLHtmlElement,
  iframe: HTMLIFrameElement,
  img: HTMLImageElement,
  input: HTMLInputElement,
  label: HTMLLabelElement,
  legend: HTMLLegendElement,
  li: HTMLLIElement,
  link: HTMLLinkElement,
  meta: HTMLMetaElement,
  ol: HTMLOListElement,
  option: HTMLOptionElement,
  p: HTMLParagraphElement,
  pre: HTMLPreElement,
  progress: HTMLProgressElement,
  script: HTMLScriptElement,
  select: HTMLSelectElement,
  slot: HTMLSlotElement,
  span: HTMLSpanElement,
  style: HTMLStyleElement,
  table: HTMLTableElement,
  template: HTMLTemplateElement,
  textarea: HTMLTextAreaElement,
  ul: HTMLUListElement,
  video: HTMLVideoElement,
};

function _elementConstructorForTag(tagName) {
  const tag = String(tagName || "").toLowerCase();
  if (tag === "svg") return SVGSVGElement;
  if (tag === "path" || tag === "g" || tag === "circle" || tag === "rect" || tag === "line" || tag === "polyline" || tag === "polygon" || tag === "use") {
    return SVGElement;
  }
  return _htmlElementConstructors[tag] || HTMLElement;
}

class Document extends Node {
  get documentElement() { return _wrapEl(+_dom("document_element")); }
  get head() { return this.querySelector("head"); }
  get body() { return this.querySelector("body"); }
  get scrollingElement() { return this.documentElement || this.body; }
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
  get URL() { return _currentDocumentUrl(); }
  get documentURI() { return this.URL; }
  get location() { return globalThis.location; }
  set location(url) { Deno.core.ops.op_navigate(_resolveUrl(String(url)), 'GET', ''); }
  get defaultView() { return globalThis; }
  get nodeType() { return 9; }
  get nodeName() { return "#document"; }
  get ownerDocument() { return null; } // Document has no ownerDocument
  get compatMode() { return "CSS1Compat"; }
  get characterSet() { return "UTF-8"; }
  get contentType() { return "text/html"; }
  get readyState() { return this._readyState || "loading"; }
  get currentScript() { return this._currentScript || null; }
  get hidden() { return false; }
  get visibilityState() { return "visible"; }
  getElementById(id) { return _wrapEl(+_dom("get_element_by_id", id)); }
  querySelector(s) { return _wrapEl(+_dom("query_selector", s)); }
  querySelectorAll(s) {
    const ids = _domParse("query_selector_all", s) || [];
    return _asNodeList(ids.map(_wrapEl));
  }
  getElementsByTagName(t) { return this.querySelectorAll(t); }
  getElementsByClassName(c) { return this.querySelectorAll("." + c); }
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
  createRange() { return { setStart(){}, setEnd(){}, collapse(){}, selectNodeContents(){}, cloneContents(){ return document.createDocumentFragment(); } }; }
  addEventListener(type, fn, opts) { return Node.prototype.addEventListener.call(this, type, fn, opts); }
  removeEventListener(type, fn) { return Node.prototype.removeEventListener.call(this, type, fn); }
  dispatchEvent(event) {
    const ok = Node.prototype.dispatchEvent.call(this, event);
    if (event?.bubbles && !event.defaultPrevented && globalThis.dispatchEvent) {
      try { globalThis.dispatchEvent(event); } catch(e) {}
    }
    return ok;
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
      createHTMLDocument(title) { return globalThis.document; },
      createDocument() { return globalThis.document; },
      hasFeature() { return true; },
    };
  }
  get styleSheets() { return _documentStyleSheets(); }
  get forms() { return this.querySelectorAll("form"); }
  get images() { return this.querySelectorAll("img"); }
  get links() { return this.querySelectorAll("a[href],area[href]"); }
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
  set innerHTML(v) {
    __obscuraRecordDOM({
      op: "fragmentSetInnerHTML",
      length: String(v ?? "").length,
      prefix: String(v ?? "").slice(0, 160),
    });
    _dom("set_inner_html", this._nid, String(v ?? ""));
  }
  querySelector(s) { return _wrapEl(+_dom("query_selector", s)); }
  querySelectorAll(s) {
    const ids = _domParse("query_selector_all", s) || [];
    const list = ids.map(_wrapEl).filter(Boolean);
    list.item = (i) => list[i] || null;
    return list;
  }
  get children() {
    const ids = _domParse("element_children", this._nid) || [];
    return ids.map(_wrapEl).filter(Boolean);
  }
  get firstElementChild() { return this.children[0] || null; }
  get lastElementChild() { const ch = this.children; return ch[ch.length - 1] || null; }
  getElementById(id) { return null; }
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
function _wrap(nid) {
  if (nid < 0 || nid === null || nid === undefined || isNaN(nid)) return null;
  if (_cache.has(nid)) return _cache.get(nid);
  const t = +_dom("node_type", nid);
  let n;
  if (t === 1) {
    const tag = _domParse("tag_name", nid) || "";
    const Ctor = _elementConstructorForTag(tag);
    n = new Ctor(nid);
  }
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
  const tag = _domParse("tag_name", nid) || "";
  const Ctor = _elementConstructorForTag(tag);
  const n = new Ctor(nid);
  _cache.set(nid, n);
  return n;
}

globalThis.document = null;
globalThis.__obscura_location_href = null;
globalThis.__obscura_navigation_log = globalThis.__obscura_navigation_log || [];

function _currentDocumentUrl() {
  return globalThis.__obscura_location_href || _domParse("document_url") || "about:blank";
}

function _recordNavigation(entry) {
  try {
    globalThis.__obscura_navigation_log.push({
      at: Date.now(),
      ...entry,
    });
    if (globalThis.__obscura_navigation_log.length > 100) {
      globalThis.__obscura_navigation_log.splice(0, globalThis.__obscura_navigation_log.length - 100);
    }
  } catch (_) {}
}

function _resolveUrl(url) {
  if (!url) return url;
  if (url.startsWith('http://') || url.startsWith('https://') || url.startsWith('about:')) return url;
  try { return new URL(url, _currentDocumentUrl()).href; } catch(e) { return url; }
}
globalThis.location = {
  get href() { return _currentDocumentUrl(); },
  set href(url) {
    const resolved = _resolveUrl(String(url));
    _recordNavigation({ type: "location.href", url: resolved });
    Deno.core.ops.op_navigate(resolved, 'GET', '');
  },
  get origin() { try { return new URL(this.href).origin; } catch { return ""; } },
  get protocol() { try { return new URL(this.href).protocol; } catch { return ""; } },
  get host() { try { return new URL(this.href).host; } catch { return ""; } },
  get hostname() { try { return new URL(this.href).hostname; } catch { return ""; } },
  get pathname() { try { return new URL(this.href).pathname; } catch { return "/"; } },
  set pathname(value) {
    const next = new URL(this.href);
    next.pathname = String(value);
    this.href = next.href;
  },
  get search() { try { return new URL(this.href).search; } catch { return ""; } },
  set search(value) {
    const next = new URL(this.href);
    next.search = String(value);
    this.href = next.href;
  },
  get hash() { try { return new URL(this.href).hash; } catch { return ""; } },
  set hash(value) {
    const oldURL = this.href;
    const next = new URL(this.href);
    next.hash = String(value);
    globalThis.__obscura_location_href = next.href;
    _recordNavigation({ type: "hash", url: next.href });
    try { globalThis.dispatchEvent(new HashChangeEvent("hashchange", { oldURL, newURL: next.href })); } catch (_) {}
  },
  get port() { try { return new URL(this.href).port; } catch { return ""; } },
  toString() { return this.href; },
  assign(url) {
    const resolved = _resolveUrl(String(url));
    _recordNavigation({ type: "location.assign", url: resolved });
    Deno.core.ops.op_navigate(resolved, 'GET', '');
  },
  reload() {},
  replace(url) {
    const resolved = _resolveUrl(String(url));
    _recordNavigation({ type: "location.replace", url: resolved });
    Deno.core.ops.op_navigate(resolved, 'GET', '');
  },
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
function _isFacebookInstagramSyncIframe(url) {
  try {
    const parsed = new URL(url);
    return parsed.origin === "https://www.facebook.com" && parsed.pathname === "/instagram/login_sync/";
  } catch (_) {
    return false;
  }
}
function _finishIframeLoad(iframeEl, url) {
  _registerIframe(iframeEl);
  _fireElementLoad(iframeEl);
  if (_isFacebookInstagramSyncIframe(url) && iframeEl._iframeWin) {
    setTimeout(() => {
      iframeEl._iframeWin?._postMessageToParent?.({ eventName: "ig_iframe_ready" });
    }, 0);
  }
}
const _OBSCURA_DEFAULT_UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const _OBSCURA_CH_BRANDS = [
  {brand: "Google Chrome", version: "124"},
  {brand: "Not.A/Brand", version: "8"},
  {brand: "Chromium", version: "124"},
];
const _OBSCURA_CH_FULL_VERSION_LIST = [
  {brand: "Google Chrome", version: "124.0.0.0"},
  {brand: "Not.A/Brand", version: "8.0.0.0"},
  {brand: "Chromium", version: "124.0.0.0"},
];
globalThis.__obscura_language = globalThis.__obscura_language || "en-US";
globalThis.__obscura_languages = globalThis.__obscura_languages || ["en-US","en"];
globalThis.__obscura_platform = globalThis.__obscura_platform || "MacIntel";
globalThis.__obscura_user_agent_metadata = globalThis.__obscura_user_agent_metadata || null;
function _obscuraLangList(value) {
  if (!value || typeof value !== "string") return ["en-US","en"];
  return value.split(",").map(v => v.split(";")[0].trim()).filter(Boolean);
}
function _obscuraUAMetadata() {
  return globalThis.__obscura_user_agent_metadata || {};
}
function _obscuraMetadataFromUA(ua) {
  ua = String(ua || _OBSCURA_DEFAULT_UA);
  const fullVersion = (ua.match(/(?:Chrome|Chromium)\/([0-9.]+)/) || [])[1] || "124.0.0.0";
  const major = fullVersion.split(".")[0] || "124";
  const isLinux = /Linux/i.test(ua);
  const isMac = /Mac OS X|Macintosh/i.test(ua);
  return {
    architecture: "x86",
    bitness: "64",
    brands: [
      {brand: "Google Chrome", version: major},
      {brand: "Not.A/Brand", version: "8"},
      {brand: "Chromium", version: major},
    ],
    fullVersionList: [
      {brand: "Google Chrome", version: fullVersion},
      {brand: "Not.A/Brand", version: "8.0.0.0"},
      {brand: "Chromium", version: fullVersion},
    ],
    mobile: /Mobile/i.test(ua),
    model: "",
    platform: isLinux ? "Linux" : (isMac ? "macOS" : "Windows"),
    platformVersion: isLinux ? "" : (isMac ? "14.4.1" : "10.0.0"),
    uaFullVersion: fullVersion,
  };
}
globalThis.__obscura_apply_user_agent_string = function(ua) {
  if (typeof ua !== "string") return;
  if (!ua.trim()) ua = _OBSCURA_DEFAULT_UA;
  globalThis.__obscura_ua = ua;
  globalThis.__obscura_user_agent_metadata = _obscuraMetadataFromUA(ua);
  if (/Linux/i.test(ua)) globalThis.__obscura_platform = "Linux x86_64";
  else if (/Mac OS X|Macintosh/i.test(ua)) globalThis.__obscura_platform = "MacIntel";
  else if (/Windows/i.test(ua)) globalThis.__obscura_platform = "Win32";
};
globalThis.__obscura_apply_user_agent_override = function(params) {
  params = params || {};
  if (typeof params.userAgent === "string") globalThis.__obscura_apply_user_agent_string(params.userAgent);
  if (typeof params.acceptLanguage === "string") {
    globalThis.__obscura_language = params.acceptLanguage.split(",")[0].split(";")[0].trim() || "en-US";
    globalThis.__obscura_languages = _obscuraLangList(params.acceptLanguage);
  }
  if (typeof params.platform === "string") globalThis.__obscura_platform = params.platform;
  if (params.userAgentMetadata && typeof params.userAgentMetadata === "object") {
    globalThis.__obscura_user_agent_metadata = params.userAgentMetadata;
  }
};
function __obscura_pw_parse_arg(value, handles, refs) {
  handles = handles || [];
  refs = refs || new Map();
  if (value && typeof value === "object") {
    if ("ref" in value) return refs.get(value.ref);
    if ("h" in value) return handles[value.h];
    if ("v" in value) {
      if (value.v === "undefined") return undefined;
      if (value.v === "null") return null;
      if (value.v === "NaN") return NaN;
      if (value.v === "Infinity") return Infinity;
      if (value.v === "-Infinity") return -Infinity;
      if (value.v === "-0") return -0;
    }
    if ("bi" in value) return BigInt(value.bi);
    if ("a" in value) {
      const result = [];
      refs.set(value.id, result);
      for (const item of value.a) result.push(__obscura_pw_parse_arg(item, handles, refs));
      return result;
    }
    if ("o" in value) {
      const result = {};
      refs.set(value.id, result);
      for (const item of value.o) {
        if (item.k !== "__proto__") result[item.k] = __obscura_pw_parse_arg(item.v, handles, refs);
      }
      return result;
    }
  }
  return value;
}
function __obscura_pw_serialize(value, refs) {
  refs = refs || new Map();
  if (value === undefined) return {v: "undefined"};
  if (value === null) return {v: "null"};
  if (typeof value === "number") {
    if (Number.isNaN(value)) return {v: "NaN"};
    if (value === Infinity) return {v: "Infinity"};
    if (value === -Infinity) return {v: "-Infinity"};
    if (Object.is(value, -0)) return {v: "-0"};
    return value;
  }
  if (typeof value === "boolean" || typeof value === "string") return value;
  if (typeof value === "bigint") return {bi: String(value)};
  if (typeof value === "object") {
    if (refs.has(value)) return {ref: refs.get(value)};
    const id = refs.size + 1;
    refs.set(value, id);
    if (Array.isArray(value)) return {a: value.map(v => __obscura_pw_serialize(v, refs)), id};
    const entries = [];
    for (const key of Object.keys(value)) {
      if (key !== "__proto__") entries.push({k: key, v: __obscura_pw_serialize(value[key], refs)});
    }
    return {o: entries, id};
  }
  return {v: "undefined"};
}
function __obscura_pw_selector_from_parsed(parsed) {
  try {
    const parts = parsed && parsed.parts;
    if (!Array.isArray(parts) || parts.length !== 1) return null;
    const part = parts[0];
    if (!part || part.name !== "css") return null;
    const body = part.body;
    if (!Array.isArray(body) || body.length !== 1) return null;
    const simples = body[0] && body[0].simples;
    if (!Array.isArray(simples) || simples.length !== 1) return null;
    const simple = simples[0];
    if (!simple || simple.combinator !== "") return null;
    const selector = simple.selector;
    if (!selector || !Array.isArray(selector.functions) || selector.functions.length !== 0) return null;
    return typeof selector.css === "string" ? selector.css : null;
  } catch (_) {
    return null;
  }
}
function __obscura_pw_selector_from_info(info) {
  return __obscura_pw_selector_from_parsed(info && info.parsed);
}
globalThis.__obscura_make_playwright_injected_script = function() {
  function queryAll(parsed, root) {
    const selector = __obscura_pw_selector_from_parsed(parsed);
    if (!selector) return [];
    const scope = root || document;
    return Array.from(scope.querySelectorAll(selector));
  }
  return {
    eval: globalThis.eval.bind(globalThis),
    querySelectorAll(parsed, root) {
      return queryAll(parsed, root);
    },
    querySelector(parsed, root, strict) {
      const elements = queryAll(parsed, root);
      if (strict && elements.length > 1) {
        throw new Error("strict mode violation: selector resolved to " + elements.length + " elements");
      }
      return elements[0] || null;
    },
    checkDeprecatedSelectorUsage() {},
    markTargetElements() {},
    previewNode(node) {
      if (!node) return "<null>";
      const name = (node.nodeName || node.tagName || "node").toLowerCase();
      const id = node.id ? "#" + node.id : "";
      return "<" + name + id + ">";
    }
  };
};
globalThis.__obscura_make_playwright_utility_script = function() {
  class UtilityScript {
    evaluate(isFunction, returnByValue, expression, argCount, ...argsAndHandles) {
      const args = argsAndHandles.slice(0, argCount);
      const handles = argsAndHandles.slice(argCount);
      const parsedArgs = args.map(arg => __obscura_pw_parse_arg(arg, handles));
      const fastResult = this._fastLocatorResult(returnByValue, expression, parsedArgs);
      if (fastResult !== undefined) return fastResult;
      let result = globalThis.eval(expression);
      if (isFunction === true) result = result(...parsedArgs);
      else if (isFunction !== false && typeof result === "function") result = result(...parsedArgs);
      if (!returnByValue) return result;
      if (result && typeof result.then === "function") {
        return result.then(value => __obscura_pw_serialize(value)).catch(() => undefined);
      }
      return __obscura_pw_serialize(result);
    }
    jsonValue(_returnByValue, value) {
      return __obscura_pw_serialize(value);
    }
    _fastLocatorResult(returnByValue, expression, parsedArgs) {
      if (!returnByValue || typeof expression !== "string") return undefined;

      if (expression.indexOf("injected.querySelectorAll") !== -1 && expression.indexOf("elements.length") !== -1) {
        const payload = parsedArgs[1] || {};
        const selector = __obscura_pw_selector_from_info(payload.info);
        if (!selector) return undefined;
        return __obscura_pw_serialize(document.querySelectorAll(selector).length);
      }

      if (expression.indexOf("callbackText") === -1 || expression.indexOf("injected.querySelector") === -1) {
        return undefined;
      }

      const payload = parsedArgs[1] || {};
      const selector = __obscura_pw_selector_from_info(payload.info);
      if (!selector || typeof payload.callbackText !== "string") return undefined;

      const element = document.querySelector(selector);
      if (!element) return __obscura_pw_serialize({ success: false });

      let value;
      if (payload.callbackText.indexOf("element.innerText") !== -1) {
        value = element.innerText;
      } else if (payload.callbackText.indexOf("element.textContent") !== -1) {
        value = element.textContent;
      } else {
        return undefined;
      }

      return __obscura_pw_serialize({
        log: "  locator resolved to " + selector,
        success: true,
        value
      });
    }
  }
  return new UtilityScript();
};
class _ObscuraServiceWorker {
  constructor(scriptURL = "") {
    this.scriptURL = scriptURL;
    this.state = "activated";
    this.onstatechange = null;
    this.onerror = null;
  }
  postMessage() {}
  addEventListener() {}
  removeEventListener() {}
  dispatchEvent() { return true; }
}
class _ObscuraPushManager {
  getSubscription() { return Promise.resolve(null); }
  permissionState() { return Promise.resolve("prompt"); }
  subscribe() { return Promise.reject(new DOMException("NotAllowedError")); }
}
class _ObscuraNavigationPreloadManager {
  enable() { return Promise.resolve(); }
  disable() { return Promise.resolve(); }
  getState() { return Promise.resolve({enabled: false, headerValue: "true"}); }
  setHeaderValue() { return Promise.resolve(); }
}
class _ObscuraServiceWorkerRegistration {
  constructor(scope = "/", scriptURL = "") {
    this.scope = scope;
    this.installing = null;
    this.waiting = null;
    this.active = new _ObscuraServiceWorker(scriptURL);
    this.navigationPreload = new _ObscuraNavigationPreloadManager();
    this.pushManager = new _ObscuraPushManager();
    this.updateViaCache = "imports";
    this.onupdatefound = null;
  }
  getNotifications() { return Promise.resolve([]); }
  showNotification() { return Promise.resolve(); }
  unregister() { return Promise.resolve(true); }
  update() { return Promise.resolve(this); }
  addEventListener() {}
  removeEventListener() {}
  dispatchEvent() { return true; }
}
class _ObscuraServiceWorkerContainer {
  constructor() {
    this.controller = null;
    this.oncontrollerchange = null;
    this.onmessage = null;
    this.onmessageerror = null;
    this._registrations = [];
    this.ready = Promise.resolve(new _ObscuraServiceWorkerRegistration("/", ""));
  }
  register(scriptURL = "", options = {}) {
    const registration = new _ObscuraServiceWorkerRegistration(options.scope || "/", String(scriptURL || ""));
    this._registrations.push(registration);
    this.ready = Promise.resolve(registration);
    return Promise.resolve(registration);
  }
  getRegistration() { return Promise.resolve(this._registrations[0] || null); }
  getRegistrations() { return Promise.resolve(this._registrations.slice()); }
  startMessages() {}
  addEventListener() {}
  removeEventListener() {}
  dispatchEvent() { return true; }
}
globalThis.ServiceWorker = globalThis.ServiceWorker || _ObscuraServiceWorker;
globalThis.ServiceWorkerRegistration = globalThis.ServiceWorkerRegistration || _ObscuraServiceWorkerRegistration;
globalThis.ServiceWorkerContainer = globalThis.ServiceWorkerContainer || _ObscuraServiceWorkerContainer;
globalThis.navigator = {
  get userAgent() { return globalThis.__obscura_ua || _OBSCURA_DEFAULT_UA; },
  get appVersion() { return this.userAgent.replace('Mozilla/', ''); },
  get language() { return globalThis.__obscura_language || "en-US"; },
  get languages() { return globalThis.__obscura_languages || ["en-US","en"]; },
  get platform() { return globalThis.__obscura_platform || "MacIntel"; },
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
    get brands() { return Array.isArray(_obscuraUAMetadata().brands) ? _obscuraUAMetadata().brands : _OBSCURA_CH_BRANDS; },
    get mobile() { return Boolean(_obscuraUAMetadata().mobile); },
    get platform() { return typeof _obscuraUAMetadata().platform === "string" ? _obscuraUAMetadata().platform : "macOS"; },
    getHighEntropyValues(hints) {
      const metadata = _obscuraUAMetadata();
      return Promise.resolve({
        architecture: metadata.architecture || "x86",
        bitness: metadata.bitness || "64",
        brands: Array.isArray(metadata.brands) ? metadata.brands : _OBSCURA_CH_BRANDS,
        fullVersionList: Array.isArray(metadata.fullVersionList) ? metadata.fullVersionList : _OBSCURA_CH_FULL_VERSION_LIST,
        mobile: Boolean(metadata.mobile),
        model: metadata.model || "",
        platform: metadata.platform || "macOS",
        platformVersion: metadata.platformVersion || "14.4.1",
        uaFullVersion: metadata.uaFullVersion || "124.0.0.0",
      });
    },
    toJSON() { return {brands:this.brands,mobile:this.mobile,platform:this.platform}; },
  },
  serviceWorker: new globalThis.ServiceWorkerContainer(),
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
};

globalThis.chrome = {
  app: { isInstalled: false, InstallState: { DISABLED: "disabled", INSTALLED: "installed", NOT_INSTALLED: "not_installed" }, RunningState: { CANNOT_RUN: "cannot_run", READY_TO_RUN: "ready_to_run", RUNNING: "running" } },
  runtime: { OnInstalledReason: {}, OnRestartRequiredReason: {}, PlatformArch: {}, PlatformNaclArch: {}, PlatformOs: {}, RequestUpdateCheckStatus: {}, connect() { return {}; }, sendMessage() {} },
  csi() { return {}; },
  loadTimes() { return {}; },
};

globalThis.Notification = class Notification {
  static permission = "default";
  static requestPermission() { return Promise.resolve("default"); }
  constructor() {}
};

globalThis.WebGLRenderingContext = class WebGLRenderingContext {};
globalThis.WebGL2RenderingContext = class WebGL2RenderingContext {};

globalThis.screen = { width:1920, height:1080, availWidth:1920, availHeight:1040, colorDepth:24, pixelDepth:24, availTop:0, availLeft:0, orientation:{type:"landscape-primary",angle:0,addEventListener(){},removeEventListener(){},dispatchEvent(){return true;}} };
globalThis.visualViewport = { width:1920, height:1000, offsetLeft:0, offsetTop:0, scale:1, addEventListener(){}, removeEventListener(){} };
globalThis.devicePixelRatio = 2;
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
  __obscuraRecordFetch({
    kind: 'fetch',
    method: String(method || 'GET').toUpperCase(),
    url: parsed.url || url,
    status: parsed.status,
    blocked: !!parsed.blocked,
    corsBlocked: !!parsed.corsBlocked,
    contentType: (parsed.headers && (parsed.headers['content-type'] || parsed.headers['Content-Type'])) || '',
    bodyPrefix: String(parsed.body || '').slice(0, 240),
    bodyBase64Bytes: parsed.bodyBase64 ? String(parsed.bodyBase64).length : 0,
  });
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
      __obscuraRecordFetch({
        kind: 'xhr',
        method: String(xhr._method || 'GET').toUpperCase(),
        url: xhr.responseURL || url,
        status: xhr.status,
        contentType: xhr.getResponseHeader('content-type') || '',
        bodyPrefix: String(text || '').slice(0, 240),
      });
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
    if (!handler) return;
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
    const event = new Event(type);
    event.target = this;
    event.currentTarget = this;
    const handlers = this._listeners[type] || [];
    for (const h of handlers) { try { _invokeEventHandler(h, this, event); } catch(e) {} }
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
  constructor(callback) { this._callback = callback; this._targets = []; }
  observe(el) {
    this._targets.push(el);
    Promise.resolve().then(() => {
      this._callback([{
        target: el, contentRect: { x:0, y:0, width:100, height:20, top:0, left:0, bottom:20, right:100 },
        borderBoxSize: [{ blockSize: 20, inlineSize: 100 }],
        contentBoxSize: [{ blockSize: 20, inlineSize: 100 }],
      }], this);
    });
  }
  unobserve(el) { this._targets = this._targets.filter(t => t !== el); }
  disconnect() { this._targets = []; }
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

function _mediaQueryMatches(q) {
  q = String(q || '').toLowerCase();
  const width = globalThis.innerWidth || 1920;
  const height = globalThis.innerHeight || 1000;
  const tests = q.split(/\s+and\s+/i).map(s => s.replace(/^\s*only\s+/, '').trim());
  for (let test of tests) {
    test = test.replace(/^\(|\)$/g, '').trim();
    if (!test || test === 'screen' || test === 'all') continue;
    let m;
    if ((m = test.match(/^min-width\s*:\s*(\d+(?:\.\d+)?)px$/))) {
      if (width < Number(m[1])) return false;
      continue;
    }
    if ((m = test.match(/^max-width\s*:\s*(\d+(?:\.\d+)?)px$/))) {
      if (width > Number(m[1])) return false;
      continue;
    }
    if ((m = test.match(/^min-height\s*:\s*(\d+(?:\.\d+)?)px$/))) {
      if (height < Number(m[1])) return false;
      continue;
    }
    if ((m = test.match(/^max-height\s*:\s*(\d+(?:\.\d+)?)px$/))) {
      if (height > Number(m[1])) return false;
      continue;
    }
    if ((m = test.match(/^orientation\s*:\s*(portrait|landscape)$/))) {
      if ((m[1] === 'portrait') !== (height >= width)) return false;
      continue;
    }
    if ((m = test.match(/^prefers-color-scheme\s*:\s*(dark|light)$/))) {
      if (m[1] !== 'dark') return false;
      continue;
    }
    if (test === 'not all') return false;
    return false;
  }
  return true;
}
globalThis.matchMedia = _markNative(function matchMedia(q) {
  const mql = {
    matches: _mediaQueryMatches(q),
    media: String(q || ''),
    onchange: null,
    addListener(fn){ this.addEventListener('change', fn); },
    removeListener(fn){ this.removeEventListener('change', fn); },
    addEventListener(type, fn){
      if (type !== 'change' || typeof fn !== 'function') return;
      this._listeners = this._listeners || [];
      this._listeners.push(fn);
    },
    removeEventListener(type, fn){
      if (type !== 'change' || !this._listeners) return;
      this._listeners = this._listeners.filter(h => h !== fn);
    },
    dispatchEvent(event){
      const handlers = this._listeners || [];
      for (const h of handlers) { try { h.call(this, event); } catch(e) {} }
      if (typeof this.onchange === 'function') this.onchange(event);
      return true;
    },
  };
  return mql;
});
globalThis.getComputedStyle = (el) => {
  if (!el) el = document.body || {};
  const style = el?.style || el?._style || new CSSStyleDeclaration();
  return new Proxy(style, {
    get(target, prop) {
      if (prop === Symbol.toPrimitive || prop === Symbol.toStringTag) return undefined;
      if (prop in target) return target[prop];
      if (typeof prop === 'string') {
        const v = target.getPropertyValue ? target.getPropertyValue(prop) : '';
        if (v) return v;
        const defaults = {
          display: 'block', visibility: 'visible', opacity: '1',
          position: 'static', overflow: 'visible',
          transform: 'none', transition: 'none', animation: 'none',
          float: 'none', clear: 'none',
          width: 'auto', height: 'auto',
          top: 'auto', left: 'auto', right: 'auto', bottom: 'auto',
          margin: '0px', padding: '0px',
          'margin-top': '0px', 'margin-right': '0px', 'margin-bottom': '0px', 'margin-left': '0px',
          'padding-top': '0px', 'padding-right': '0px', 'padding-bottom': '0px', 'padding-left': '0px',
          'font-size': '16px', 'line-height': 'normal', 'font-weight': '400',
          color: 'rgb(0, 0, 0)', 'background-color': 'rgba(0, 0, 0, 0)',
          'border-width': '0px', 'border-style': 'none', 'border-color': 'rgb(0, 0, 0)',
          'z-index': 'auto', 'pointer-events': 'auto',
          'box-sizing': 'content-box', cursor: 'auto',
        };
        const kebabProp = prop.replace(/([A-Z])/g, '-$1').toLowerCase();
        if (defaults[prop]) return defaults[prop];
        if (defaults[kebabProp]) return defaults[kebabProp];
        return '';
      }
      if (prop === 'getPropertyValue') {
        return (name) => {
          const v = target.getPropertyValue ? target.getPropertyValue(name) : '';
          if (v) return v;
          const defaults = {transform:'none',opacity:'1',display:'block',visibility:'visible'};
          return defaults[name] || defaults[name.replace(/-([a-z])/g,(_,c)=>c.toUpperCase())] || '';
        };
      }
      if (prop === 'length') return 0;
      return undefined;
    }
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
    this.rules = this.cssRules;
    this.ownerRule = null;
    this.disabled = false;
    this.href = null;
    this.ownerNode = null;
    this.media = { mediaText: "", length: 0, item(){ return null; }, appendMedium(){}, deleteMedium(){}, toString(){ return this.mediaText; } };
    this.title = null;
    this.type = "text/css";
    this._rules = [];
  }
  insertRule(rule, index) {
    const idx = index ?? this._rules.length;
    this._rules.splice(idx, 0, { cssText: rule, type: 1 });
    this.cssRules = this._rules;
    this.rules = this._rules;
    return idx;
  }
  deleteRule(index) {
    this._rules.splice(index, 1);
    this.cssRules = this._rules;
    this.rules = this._rules;
  }
  addRule(selector, style, index) {
    return this.insertRule(selector + '{' + style + '}', index);
  }
  removeRule(index) { this.deleteRule(index); }
  replace(text) {
    this._rules = [{ cssText: text, type: 1 }];
    this.cssRules = this._rules;
    this.rules = this._rules;
    return Promise.resolve(this);
  }
  replaceSync(text) {
    this._rules = [{ cssText: text, type: 1 }];
    this.cssRules = this._rules;
    this.rules = this._rules;
  }
};

function _makeStyleSheetFor(ownerNode) {
  if (!ownerNode) return null;
  if (ownerNode._sheet) return ownerNode._sheet;
  const sheet = new CSSStyleSheet();
  sheet.ownerNode = ownerNode;
  sheet.href = ownerNode.localName === "link" ? (ownerNode.href || ownerNode.getAttribute("href") || null) : null;
  sheet.title = ownerNode.getAttribute ? ownerNode.getAttribute("title") : null;
  const media = ownerNode.getAttribute ? (ownerNode.getAttribute("media") || "") : "";
  sheet.media = {
    mediaText: media,
    length: media ? 1 : 0,
    item(index){ return index === 0 && media ? media : null; },
    appendMedium(){},
    deleteMedium(){},
    toString(){ return this.mediaText; },
  };
  sheet._rules = [{ cssText: "/* loaded */", type: 1, selectorText: ":root", style: {} }];
  sheet.cssRules = sheet._rules;
  sheet.rules = sheet._rules;
  ownerNode._sheet = sheet;
  return sheet;
}

function _isStylesheetLink(el) {
  if (!el || el.localName !== "link") return false;
  const rel = (el.getAttribute("rel") || "").toLowerCase().split(/\s+/);
  return rel.includes("stylesheet");
}

function _documentStyleSheets() {
  const nodes = [];
  try { nodes.push(...Array.from(document.querySelectorAll("link,style"))); } catch(e) {}
  const sheets = nodes
    .filter(el => el.localName === "style" || _isStylesheetLink(el))
    .map(_makeStyleSheetFor)
    .filter(Boolean);
  sheets.item = (index) => sheets[index] || null;
  return sheets;
}

Object.defineProperty(Element.prototype, "sheet", {
  configurable: true,
  get() {
    if (this.localName === "style" || _isStylesheetLink(this)) return _makeStyleSheetFor(this);
    return null;
  },
});

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
        try { this._callback.call(this, batch, this); } catch(e) { console.error("MutationObserver callback error:", e); }
      }
    });
  }
};
globalThis.__notifyMutation = function(type, target_nid, addedNodes, removedNodes, attributeName) {
  if (!globalThis.__mutationObservers.length) return;
  const target = _wrap(target_nid);
  if (!target) return;
  const record = {
    type: type, // 'childList', 'attributes', 'characterData'
    target: target,
    addedNodes: _asNodeList((addedNodes || []).map(_wrap).filter(Boolean)),
    removedNodes: _asNodeList((removedNodes || []).map(_wrap).filter(Boolean)),
    attributeName: attributeName || null,
    oldValue: null,
    previousSibling: null,
    nextSibling: null,
  };
  for (const obs of globalThis.__mutationObservers) {
    for (const t of obs._targets) {
      if (t.target._nid === target_nid || (t.options.subtree && t.target.contains && t.target.contains(target))) {
        obs._notify([record]);
        break;
      }
    }
  }
};

globalThis.ShadowRoot = class ShadowRoot {};
globalThis.customElements = {
  _registry: new Map(),
  define(name, cls, opts) { this._registry.set(name, cls); },
  get(name) { return this._registry.get(name); },
  whenDefined(name) { return Promise.resolve(this._registry.get(name)); },
  upgrade() {},
};
globalThis.NodeFilter = { SHOW_ELEMENT: 1, SHOW_TEXT: 4, SHOW_ALL: 0xFFFFFFFF };
if (typeof globalThis.ResizeObserver === 'undefined') {
  globalThis.ResizeObserver = class { constructor(){} observe(){} unobserve(){} disconnect(){} };
}
globalThis.IntersectionObserver = class {
  constructor(callback) { this._callback = callback; }
  observe(el) {
    Promise.resolve().then(() => {
      this._callback([{
        target: el,
        isIntersecting: true,
        intersectionRatio: 1,
        boundingClientRect: el.getBoundingClientRect ? el.getBoundingClientRect() : {x:0,y:0,width:100,height:20},
        intersectionRect: el.getBoundingClientRect ? el.getBoundingClientRect() : {x:0,y:0,width:100,height:20},
        rootBounds: {x:0,y:0,width:globalThis.innerWidth||1920,height:globalThis.innerHeight||1000},
      }], this);
    });
  }
  unobserve() {}
  disconnect() {}
};
globalThis.PerformanceObserver = class PerformanceObserver {
  static supportedEntryTypes = [
    "element", "event", "first-input", "largest-contentful-paint", "layout-shift",
    "long-animation-frame", "longtask", "mark", "measure", "navigation", "paint",
    "resource", "visibility-state",
  ];
  constructor(callback){ this._callback = callback; }
  observe() {}
  disconnect() {}
  takeRecords() { return []; }
};

globalThis.Event = class Event {
  constructor(t,o={}) { this.type=t;this.bubbles=!!o.bubbles;this.cancelable=!!o.cancelable;this.composed=!!o.composed;this.defaultPrevented=false;this.target=null;this.currentTarget=null;this.eventPhase=0;this.timeStamp=Date.now(); }
  get isTrusted() { return true; }
  preventDefault() { this.defaultPrevented=true; } stopPropagation(){} stopImmediatePropagation(){}
  initEvent(type,bubbles,cancelable) { this.type=type;this.bubbles=!!bubbles;this.cancelable=!!cancelable; }
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
globalThis.MouseEvent = class extends Event { constructor(t,o={}) { super(t,o);this.clientX=o.clientX||0;this.clientY=o.clientY||0; } };
globalThis.KeyboardEvent = class extends Event { constructor(t,o={}) { super(t,o);this.key=o.key||"";this.code=o.code||""; } };
globalThis.FocusEvent = class extends Event {};
globalThis.InputEvent = class extends Event { constructor(t,o={}) { super(t,o);this.data=o.data||null;this.inputType=o.inputType||""; } };
globalThis.ErrorEvent = class extends Event { constructor(t,o={}) { super(t,o);this.message=o.message||"";this.error=o.error||null; } };
globalThis.PointerEvent = class extends Event { constructor(t,o={}) { super(t,o); } };
globalThis.AnimationEvent = class extends Event {};
globalThis.TransitionEvent = class extends Event {};
globalThis.UIEvent = class extends Event {};
globalThis.WheelEvent = class extends Event {};
globalThis.PopStateEvent = class extends Event {};
globalThis.HashChangeEvent = class extends Event {};
globalThis.MessageEvent = class extends Event {
  constructor(t,o={}) {
    super(t,o);
    this.data = o.data;
    this.origin = o.origin || "";
    this.lastEventId = o.lastEventId || "";
    this.source = o.source || null;
    this.ports = o.ports || [];
  }
};
globalThis.ClipboardEvent = class extends Event {};
globalThis.SubmitEvent = class extends Event {};

globalThis.AbortController = class AbortController { constructor(){this.signal={aborted:false,addEventListener(){},removeEventListener(){},onabort:null};} abort(){this.signal.aborted=true;} };
globalThis.AbortSignal = {
  timeout(ms){return {aborted:false,reason:undefined,addEventListener(){},removeEventListener(){}}; },
  any(signals){
    const list = Array.from(signals || []);
    const aborted = list.find(signal => signal?.aborted);
    return {aborted: !!aborted, reason: aborted?.reason, addEventListener(){}, removeEventListener(){}};
  },
};
if (typeof Blob === "undefined") globalThis.Blob = class Blob { constructor(parts=[],opts={}){this._data=parts.join("");this.size=this._data.length;this.type=opts.type||"";} async text(){return this._data;} };
if (typeof File === "undefined") globalThis.File = class extends Blob { constructor(parts,name,opts){super(parts,opts);this.name=name;} };
if (typeof FormData === "undefined") globalThis.FormData = class FormData { constructor(){this._d=[];} append(k,v){this._d.push([k,v]);} get(k){const e=this._d.find(([a])=>a===k);return e?e[1]:null;} getAll(k){return this._d.filter(([a])=>a===k).map(([,v])=>v);} has(k){return this._d.some(([a])=>a===k);} entries(){return this._d[Symbol.iterator]();} forEach(cb){this._d.forEach(([k,v])=>cb(v,k));} };
if (typeof URLSearchParams === "undefined" || typeof URLSearchParams.prototype?.[Symbol.iterator] !== "function") {
  globalThis.URLSearchParams = class URLSearchParams {
    constructor(init = "") {
      this._entries = [];
      if (typeof init === "string") {
        const query = init.replace(/^\?/, "");
        if (query) {
          for (const part of query.split("&")) {
            if (!part) continue;
            const [rawKey, ...rawValue] = part.split("=");
            this.append(_decodeParam(rawKey), _decodeParam(rawValue.join("=")));
          }
        }
      } else if (init instanceof URLSearchParams) {
        init.forEach((value, key) => this.append(key, value));
      } else if (init && typeof init[Symbol.iterator] === "function") {
        for (const pair of init) {
          if (!pair) continue;
          this.append(pair[0], pair[1]);
        }
      } else if (init && typeof init === "object") {
        for (const [key, value] of Object.entries(init)) this.append(key, value);
      }
    }
    append(key, value) { this._entries.push([String(key), String(value)]); }
    delete(key) { key = String(key); this._entries = this._entries.filter(([k]) => k !== key); }
    get(key) { key = String(key); const hit = this._entries.find(([k]) => k === key); return hit ? hit[1] : null; }
    getAll(key) { key = String(key); return this._entries.filter(([k]) => k === key).map(([, v]) => v); }
    has(key) { key = String(key); return this._entries.some(([k]) => k === key); }
    set(key, value) {
      key = String(key); value = String(value);
      let found = false;
      const next = [];
      for (const [k, v] of this._entries) {
        if (k === key) {
          if (!found) next.push([key, value]);
          found = true;
        } else {
          next.push([k, v]);
        }
      }
      if (!found) next.push([key, value]);
      this._entries = next;
    }
    sort() { this._entries.sort(([a], [b]) => a < b ? -1 : (a > b ? 1 : 0)); }
    get size() { return this._entries.length; }
    entries() { return this._entries[Symbol.iterator](); }
    keys() { return this._entries.map(([k]) => k)[Symbol.iterator](); }
    values() { return this._entries.map(([, v]) => v)[Symbol.iterator](); }
    forEach(callback, thisArg) {
      for (const [key, value] of this._entries) callback.call(thisArg, value, key, this);
    }
    toString() {
      return this._entries
        .map(([key, value]) => `${_encodeParam(key)}=${_encodeParam(value)}`)
        .join("&");
    }
    [Symbol.iterator]() { return this.entries(); }
  };
}
function _decodeParam(value) {
  try { return decodeURIComponent(String(value).replace(/\+/g, " ")); } catch { return String(value); }
}
function _encodeParam(value) {
  return encodeURIComponent(String(value)).replace(/%20/g, "+");
}

globalThis.DOMParser = class { parseFromString(s,t) { return globalThis.document; } };
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

Object.defineProperty(Document.prototype, 'fonts', {
  get() {
    return {
      ready: Promise.resolve(),
      check() { return true; },
      load() { return Promise.resolve([]); },
      add() {},
      delete() { return false; },
      clear() {},
      has() { return false; },
      forEach() {},
      get size() { return 0; },
      get status() { return 'loaded'; },
      addEventListener() {}, removeEventListener() {}, dispatchEvent() { return true; },
      [Symbol.iterator]() { return [][Symbol.iterator](); },
    };
  },
  configurable: true,
});
globalThis.crypto = globalThis.crypto || { getRandomValues(arr) { for(let i=0;i<arr.length;i++) arr[i]=Math.floor(Math.random()*256); return arr; }, randomUUID(){ return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g,c=>{const r=Math.random()*16|0;return(c==="x"?r:(r&3|8)).toString(16);}); } };
globalThis.structuredClone = globalThis.structuredClone || ((value) => {
  const seen = new WeakMap();
  const clone = (v) => {
    if (v === null || typeof v !== "object") return v;
    if (seen.has(v)) return seen.get(v);
    if (v instanceof Date) return new Date(v.getTime());
    if (v instanceof RegExp) return new RegExp(v.source, v.flags);
    if (v instanceof Map) {
      const out = new Map();
      seen.set(v, out);
      v.forEach((mapValue, mapKey) => out.set(clone(mapKey), clone(mapValue)));
      return out;
    }
    if (v instanceof Set) {
      const out = new Set();
      seen.set(v, out);
      v.forEach((setValue) => out.add(clone(setValue)));
      return out;
    }
    if (ArrayBuffer.isView(v)) return new v.constructor(v);
    if (v instanceof ArrayBuffer) return v.slice(0);
    if (Array.isArray(v)) {
      const out = [];
      seen.set(v, out);
      v.forEach((item, index) => { out[index] = clone(item); });
      return out;
    }
    const out = {};
    seen.set(v, out);
    for (const key of Object.keys(v)) out[key] = clone(v[key]);
    return out;
  };
  return clone(value);
});
globalThis.reportError = globalThis.reportError || ((e) => console.error(e));

const _mkStore = () => { const s={}; return { getItem:k=>s[k]??null, setItem:(k,v)=>{s[k]=String(v);}, removeItem:k=>{delete s[k];}, clear:()=>{for(const k in s)delete s[k];}, get length(){return Object.keys(s).length;}, key:i=>Object.keys(s)[i]??null }; };
globalThis.localStorage = _mkStore();
globalThis.sessionStorage = _mkStore();
globalThis.trustedTypes = globalThis.trustedTypes || {
  createPolicy(name, rules = {}) {
    return {
      name: String(name || ""),
      createHTML(value, ...args) { return rules.createHTML ? rules.createHTML(value, ...args) : String(value); },
      createScript(value, ...args) { return rules.createScript ? rules.createScript(value, ...args) : String(value); },
      createScriptURL(value, ...args) { return rules.createScriptURL ? rules.createScriptURL(value, ...args) : String(value); },
    };
  },
  getAttributeType() { return null; },
  getPropertyType() { return null; },
  isHTML(value) { return typeof value === "string"; },
  isScript(value) { return typeof value === "string"; },
  isScriptURL(value) { return typeof value === "string"; },
};
globalThis.TrustedHTML = globalThis.TrustedHTML || class TrustedHTML {};
globalThis.TrustedScript = globalThis.TrustedScript || class TrustedScript {};
globalThis.TrustedScriptURL = globalThis.TrustedScriptURL || class TrustedScriptURL {};

globalThis.btoa = globalThis.btoa || ((s) => { const b = new TextEncoder().encode(s); const c="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; let r=""; for(let i=0;i<b.length;i+=3){const a=b[i],bb=b[i+1]??0,cc=b[i+2]??0; r+=c[a>>2]+c[((a&3)<<4)|(bb>>4)]+(i+1<b.length?c[((bb&15)<<2)|(cc>>6)]:"=")+(i+2<b.length?c[cc&63]:"=");} return r; });
globalThis.atob = globalThis.atob || ((s) => { const c="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; let r=[]; for(let i=0;i<s.length;i+=4){const a=c.indexOf(s[i]),b=c.indexOf(s[i+1]),cc=c.indexOf(s[i+2]),d=c.indexOf(s[i+3]); r.push((a<<2)|(b>>4)); if(cc>=0)r.push(((b&15)<<4)|(cc>>2)); if(d>=0)r.push(((cc&3)<<6)|d);} return String.fromCharCode(...r); });

const _historyEntries = [{ url: globalThis.location?.href || "about:blank", state: null, key: "0" }];
let _historyIndex = 0;
function _historyEntryFor(index) {
  const entry = _historyEntries[index] || _historyEntries[_historyEntries.length - 1] || { url: globalThis.location?.href || "about:blank", state: null, key: "0" };
  return {
    id: entry.key,
    key: entry.key,
    url: entry.url,
    index,
    sameDocument: true,
    getState() { return entry.state; },
  };
}
function _updateNavigationCurrentEntry() {
  if (globalThis.navigation) {
    try { globalThis.navigation.currentEntry = _historyEntryFor(_historyIndex); } catch (_) {}
  }
}
function _sameDocumentNavigate(state, title, url, replace) {
  if (arguments.length < 3 || url === undefined || url === null || url === "") {
    url = globalThis.location.href;
  }
  const resolved = _resolveUrl(String(url));
  let current;
  try { current = new URL(globalThis.location.href); } catch (_) { current = null; }
  let next;
  try { next = new URL(resolved); } catch (_) { next = null; }
  if (current && next && current.origin !== next.origin) {
    throw new DOMException("Failed to execute '" + (replace ? "replaceState" : "pushState") + "' on 'History': A history state object with URL '" + resolved + "' cannot be created in a document with origin '" + current.origin + "'.", "SecurityError");
  }
  globalThis.__obscura_location_href = resolved;
  if (replace) {
    _historyEntries[_historyIndex] = { url: resolved, state, key: _historyEntries[_historyIndex]?.key || String(_historyIndex) };
  } else {
    _historyEntries.splice(_historyIndex + 1);
    _historyEntries.push({ url: resolved, state, key: String(Date.now()) + ":" + _historyEntries.length });
    _historyIndex = _historyEntries.length - 1;
  }
  _recordNavigation({ type: replace ? "history.replaceState" : "history.pushState", url: resolved });
  _updateNavigationCurrentEntry();
}
globalThis.history = {
  get length() { return _historyEntries.length; },
  get state() { return _historyEntries[_historyIndex]?.state ?? null; },
  pushState(state, title, url) { _sameDocumentNavigate(state, title, url, false); },
  replaceState(state, title, url) { _sameDocumentNavigate(state, title, url, true); },
  go(delta = 0) {
    const nextIndex = _historyIndex + Number(delta || 0);
    if (nextIndex < 0 || nextIndex >= _historyEntries.length || nextIndex === _historyIndex) return;
    const oldURL = globalThis.location.href;
    _historyIndex = nextIndex;
    globalThis.__obscura_location_href = _historyEntries[_historyIndex].url;
    _recordNavigation({ type: "history.go", url: globalThis.__obscura_location_href, delta: Number(delta || 0) });
    _updateNavigationCurrentEntry();
    setTimeout(() => {
      try { globalThis.dispatchEvent(new PopStateEvent("popstate", { state: globalThis.history.state })); } catch (_) {}
      if (oldURL.split("#")[1] !== String(globalThis.location.href).split("#")[1]) {
        try { globalThis.dispatchEvent(new HashChangeEvent("hashchange", { oldURL, newURL: globalThis.location.href })); } catch (_) {}
      }
    }, 0);
  },
  back() { this.go(-1); },
  forward() { this.go(1); },
  scrollRestoration: "auto",
};
globalThis.navigation = globalThis.navigation || {
  currentEntry: _historyEntryFor(_historyIndex),
  transition: null,
  canGoBack: false,
  canGoForward: false,
  entries(){ return _historyEntries.map((_, index) => _historyEntryFor(index)); },
  navigate(url){ globalThis.location.assign(String(url)); return { committed: Promise.resolve(this.currentEntry), finished: Promise.resolve(this.currentEntry) }; },
  reload(){ return { committed: Promise.resolve(this.currentEntry), finished: Promise.resolve(this.currentEntry) }; },
  traverseTo(){ return { committed: Promise.resolve(this.currentEntry), finished: Promise.resolve(this.currentEntry) }; },
  back(){ return { committed: Promise.resolve(this.currentEntry), finished: Promise.resolve(this.currentEntry) }; },
  forward(){ return { committed: Promise.resolve(this.currentEntry), finished: Promise.resolve(this.currentEntry) }; },
  updateCurrentEntry() {},
  addEventListener(){}, removeEventListener(){}, dispatchEvent(){ return true; },
};
globalThis.cookieStore = globalThis.cookieStore || {
  async get(name) {
    const wanted = typeof name === "string" ? name : name?.name;
    const cookie = String(document.cookie || "").split(/;\s*/).find(part => part.split("=")[0] === wanted);
    if (!cookie) return null;
    const [key, ...rest] = cookie.split("=");
    return { name: key, value: rest.join("=") };
  },
  async getAll() {
    return String(document.cookie || "").split(/;\s*/).filter(Boolean).map(part => {
      const [key, ...rest] = part.split("=");
      return { name: key, value: rest.join("=") };
    });
  },
  async set(name, value) { document.cookie = String(name) + "=" + String(value ?? ""); },
  async delete(name) { document.cookie = String(typeof name === "string" ? name : name?.name) + "=; Max-Age=0"; },
  addEventListener(){}, removeEventListener(){}, dispatchEvent(){ return true; },
};
globalThis.ReportingObserver = globalThis.ReportingObserver || class ReportingObserver {
  constructor(callback, options) { this._callback = callback; this._options = options; }
  observe() {}
  disconnect() {}
  takeRecords() { return []; }
};
globalThis.screenX = 0; globalThis.screenY = 0;
globalThis.screenLeft = 0; globalThis.screenTop = 0;
globalThis.pageXOffset = 0; globalThis.pageYOffset = 0;
globalThis.scrollX = 0; globalThis.scrollY = 0;

function _cssSupportsCondition(condition) {
  const text = String(condition || '').trim();
  if (!text) return false;
  if (/^not\s+/i.test(text)) return !_cssSupportsCondition(text.replace(/^not\s+/i, ''));
  if (/^selector\s*\(/i.test(text)) return true;
  const withoutParens = text.replace(/^\((.*)\)$/s, '$1').trim();
  const parts = withoutParens.split(/\)\s+and\s+\(/i).map(p => p.replace(/^\(|\)$/g, '').trim()).filter(Boolean);
  if (parts.length > 1) return parts.every(_cssSupportsCondition);
  const orParts = withoutParens.split(/\)\s+or\s+\(/i).map(p => p.replace(/^\(|\)$/g, '').trim()).filter(Boolean);
  if (orParts.length > 1) return orParts.some(_cssSupportsCondition);
  const colon = withoutParens.indexOf(':');
  if (colon > 0) {
    return _cssSupportsDeclaration(withoutParens.slice(0, colon), withoutParens.slice(colon + 1));
  }
  return true;
}
function _cssSupportsDeclaration(property, value) {
  const prop = String(property || '').trim().toLowerCase();
  const val = String(value || '').trim().toLowerCase();
  if (!prop || !val) return false;
  if (prop === 'selector') return true;
  if (prop === '-webkit-touch-callout') return false;
  if (prop.startsWith('--')) return true;
  const knownProperties = new Set([
    'align-content','align-items','align-self','all','animation','animation-delay','animation-direction',
    'animation-duration','animation-fill-mode','animation-iteration-count','animation-name','animation-play-state',
    'animation-timing-function','appearance','aspect-ratio','backdrop-filter','background','background-color',
    'background-image','background-position','background-repeat','background-size','border','border-bottom',
    'border-color','border-left','border-radius','border-right','border-top','bottom','box-shadow','box-sizing',
    'caret-color','clear','clip','clip-path','color','color-scheme','column-gap','contain','content',
    'content-visibility','cursor','display','filter','flex','flex-basis','flex-direction','flex-flow','flex-grow',
    'flex-shrink','flex-wrap','float','font','font-family','font-feature-settings','font-size','font-stretch',
    'font-style','font-variant','font-weight','gap','grid','grid-area','grid-auto-columns','grid-auto-flow',
    'grid-auto-rows','grid-column','grid-column-end','grid-column-start','grid-row','grid-row-end','grid-row-start',
    'grid-template','grid-template-areas','grid-template-columns','grid-template-rows','height','inset',
    'inset-block','inset-block-end','inset-block-start','inset-inline','inset-inline-end','inset-inline-start',
    'justify-content','justify-items','justify-self','left','letter-spacing','line-height','margin','margin-bottom',
    'margin-left','margin-right','margin-top','max-height','max-width','min-height','min-width','object-fit',
    'object-position','opacity','order','outline','overflow','overflow-wrap','overflow-x','overflow-y','padding',
    'padding-bottom','padding-left','padding-right','padding-top','place-content','place-items','pointer-events',
    'position','right','row-gap','scroll-behavior','text-align','text-decoration','text-overflow','text-transform',
    'top','touch-action','transform','transform-origin','transition','transition-delay','transition-duration',
    'transition-property','transition-timing-function','translate','user-select','vertical-align','visibility',
    'white-space','width','will-change','word-break','word-wrap','z-index','zoom',
  ]);
  if (knownProperties.has(prop) || prop.startsWith('-webkit-')) return true;
  return true;
}
globalThis.CSS = {
  supports(property, value) {
    if (arguments.length === 1) return _cssSupportsCondition(property);
    return _cssSupportsDeclaration(property, value);
  },
  escape(s) {
    return String(s).replace(/[\0-\x1f\x7f]|^-?\d|^-$|[^\w-]/g, (ch, offset) => {
      if (ch === '\0') return '\uFFFD';
      const code = ch.charCodeAt(0).toString(16);
      if (/^[\w-]$/.test(ch) && !(offset === 0 && /[\d-]/.test(ch))) return ch;
      return '\\' + code + ' ';
    });
  },
};

globalThis.HTMLElement = HTMLElement;
globalThis.HTMLDivElement = HTMLDivElement;
globalThis.HTMLSpanElement = HTMLSpanElement;
globalThis.HTMLParagraphElement = HTMLParagraphElement;
globalThis.HTMLAnchorElement = HTMLAnchorElement;
globalThis.HTMLImageElement = HTMLImageElement;
globalThis.HTMLInputElement = HTMLInputElement;
globalThis.HTMLButtonElement = HTMLButtonElement;
globalThis.HTMLFormElement = HTMLFormElement;
globalThis.HTMLSelectElement = HTMLSelectElement;
globalThis.HTMLTextAreaElement = HTMLTextAreaElement;
globalThis.HTMLLabelElement = HTMLLabelElement;
globalThis.HTMLTableElement = HTMLTableElement;
globalThis.HTMLIFrameElement = HTMLIFrameElement;
globalThis.HTMLCanvasElement = HTMLCanvasElement;
globalThis.HTMLVideoElement = HTMLVideoElement;
globalThis.HTMLAudioElement = HTMLAudioElement;
globalThis.HTMLScriptElement = HTMLScriptElement;
globalThis.HTMLStyleElement = HTMLStyleElement;
globalThis.HTMLLinkElement = HTMLLinkElement;
globalThis.HTMLMetaElement = HTMLMetaElement;
globalThis.HTMLHeadElement = HTMLHeadElement;
globalThis.HTMLBodyElement = HTMLBodyElement;
globalThis.HTMLHtmlElement = HTMLHtmlElement;
globalThis.HTMLBRElement = HTMLBRElement;
globalThis.HTMLHRElement = HTMLHRElement;
globalThis.HTMLUListElement = HTMLUListElement;
globalThis.HTMLOListElement = HTMLOListElement;
globalThis.HTMLLIElement = HTMLLIElement;
globalThis.HTMLPreElement = HTMLPreElement;
globalThis.HTMLHeadingElement = HTMLHeadingElement;
globalThis.HTMLTemplateElement = HTMLTemplateElement;
globalThis.HTMLSlotElement = HTMLSlotElement;
globalThis.HTMLOptionElement = HTMLOptionElement;
globalThis.HTMLDataListElement = HTMLDataListElement;
globalThis.HTMLFieldSetElement = HTMLFieldSetElement;
globalThis.HTMLLegendElement = HTMLLegendElement;
globalThis.HTMLProgressElement = HTMLProgressElement;
globalThis.HTMLDetailsElement = HTMLDetailsElement;
globalThis.HTMLDialogElement = HTMLDialogElement;
globalThis.SVGElement = SVGElement;
globalThis.SVGSVGElement = SVGSVGElement;
globalThis.CharacterData = CharacterData;
globalThis.Text = Text;
globalThis.Comment = Comment;
globalThis.DocumentFragment = DocumentFragment;
globalThis.DocumentType = DocumentType;
globalThis.Node = Node;
globalThis.Element = Element;
globalThis.Document = Document;
globalThis.EventTarget = Node;
globalThis.Range = class Range { setStart(){} setEnd(){} collapse(){} selectNodeContents(){} deleteContents(){} cloneContents(){ return document.createDocumentFragment(); } insertNode(){} getBoundingClientRect(){return {x:0,y:0,width:0,height:0,top:0,right:0,bottom:0,left:0};} };
Object.defineProperty(Element.prototype, "noModule", {
  configurable: true,
  get() { return this.hasAttribute("nomodule"); },
  set(v) { if (v) this.setAttribute("nomodule", ""); else this.removeAttribute("nomodule"); },
});

[
  navigator.getBattery, navigator.getGamepads, navigator.sendBeacon,
  navigator.javaEnabled, navigator.serviceWorker?.register,
  navigator.permissions?.query, navigator.credentials?.get,
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
  Element.prototype.addEventListener, Element.prototype.removeEventListener,
  Element.prototype.dispatchEvent, Element.prototype.click,
  Element.prototype.focus, Element.prototype.blur,
  Element.prototype.cloneNode, Element.prototype.attachShadow,
  Element.prototype.insertAdjacentHTML, Element.prototype.scrollIntoView,
  Element.prototype.append, Element.prototype.remove,
  Element.prototype.getContext, Element.prototype.toDataURL, Element.prototype.toBlob,
  Node.prototype.appendChild, Node.prototype.removeChild,
  Node.prototype.replaceChild, Node.prototype.insertBefore,
  Node.prototype.contains, Node.prototype.hasChildNodes, Node.prototype.cloneNode,
  Document.prototype.getElementById, Document.prototype.querySelector,
  Document.prototype.querySelectorAll, Document.prototype.getElementsByTagName,
  Document.prototype.createElement, Document.prototype.createElementNS,
  Document.prototype.createTextNode, Document.prototype.createComment,
  Document.prototype.createDocumentFragment, Document.prototype.createEvent,
  Document.prototype.hasFocus,
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
    return this._root.querySelectorAll('.' + cls);
  }
  createElement(tag) { return document.createElement(tag); }
  createElementNS(ns, tag) { return document.createElementNS(ns, tag); }
  createTextNode(text) { return document.createTextNode(text); }
  createComment(text) { return document.createComment(text); }
  createDocumentFragment() { return document.createDocumentFragment(); }
  createEvent(type) { return document.createEvent(type); }
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

  _postMessageToParent(data) {
    const event = new MessageEvent('message', {
      data: data,
      origin: this.location.origin,
      source: this,
    });
    Promise.resolve().then(() => {
      globalThis.dispatchEvent?.(event);
    });
  }

  postMessage(data, origin) {
    if (this.location?.origin === "https://www.facebook.com" && data?.eventName === "ig_iframe_sync") {
      Promise.resolve().then(() => {
        this._postMessageToParent({ eventName: "ig_iframe_success" });
      });
      return;
    }
    this._postMessageToParent(data);
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
  const host = this;
  const children = [];
  const shadow = {
    mode: opts?.mode || 'open',
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
    cloneNode() { return shadow; },
  };
  this.shadowRoot = shadow;
  return shadow;
};

_markNative(Element.prototype.attachShadow);

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

globalThis.indexedDB = {
  open(name, version) {
    const req = { result: null, error: null, onsuccess: null, onerror: null, onupgradeneeded: null };
    Promise.resolve().then(() => {
      req.result = { name, version: version||1, objectStoreNames: { contains(){return false;}, length:0 }, createObjectStore(){return {createIndex(){}}; }, transaction(){return {objectStore(){return {get(){return {onsuccess:null,onerror:null};},put(){return {onsuccess:null};},delete(){return {onsuccess:null};}};}}; }, close(){} };
      if (req.onsuccess) req.onsuccess({ target: req });
    });
    return req;
  },
  deleteDatabase() { return { onsuccess: null, onerror: null }; },
};
globalThis.IDBKeyRange = { only(v){return v;}, lowerBound(v){return v;}, upperBound(v){return v;}, bound(l,u){return [l,u];} };

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
const _defaultTZ = 'America/New_York';
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

globalThis.__scrollX = globalThis.__scrollX || 0;
globalThis.__scrollY = globalThis.__scrollY || 0;
function _setWindowScroll(x, y) {
  globalThis.__scrollX = Math.max(0, Number(x) || 0);
  globalThis.__scrollY = Math.max(0, Number(y) || 0);
  try { globalThis.dispatchEvent(new Event('scroll')); } catch(e) {}
  try { document.dispatchEvent(new Event('scroll')); } catch(e) {}
}
try {
  Object.defineProperties(globalThis, {
    scrollX: { configurable: true, get() { return globalThis.__scrollX || 0; } },
    scrollY: { configurable: true, get() { return globalThis.__scrollY || 0; } },
    pageXOffset: { configurable: true, get() { return globalThis.__scrollX || 0; } },
    pageYOffset: { configurable: true, get() { return globalThis.__scrollY || 0; } },
  });
} catch(e) {}
globalThis.scrollTo = function(x, y) {
  if (x && typeof x === 'object') {
    _setWindowScroll(x.left ?? globalThis.__scrollX ?? 0, x.top ?? globalThis.__scrollY ?? 0);
  } else {
    _setWindowScroll(x, y);
  }
};
globalThis.scrollBy = function(x, y) {
  if (x && typeof x === 'object') {
    _setWindowScroll((globalThis.__scrollX || 0) + (Number(x.left) || 0), (globalThis.__scrollY || 0) + (Number(x.top) || 0));
  } else {
    _setWindowScroll((globalThis.__scrollX || 0) + (Number(x) || 0), (globalThis.__scrollY || 0) + (Number(y) || 0));
  }
};
globalThis.scroll = globalThis.scrollTo;
globalThis.focus = function() {};
globalThis.blur = function() {};
globalThis.print = function() {};
globalThis.alert = function() {};
globalThis.confirm = function() { return true; };
globalThis.prompt = function() { return null; };
globalThis.open = function() { return null; };
globalThis.close = function() {};
globalThis.stop = function() {};
globalThis.postMessage = function(message, targetOrigin) {
  const origin = globalThis.location?.origin || "";
  if (targetOrigin && targetOrigin !== "*" && targetOrigin !== "/" && targetOrigin !== origin) return;
  setTimeout(() => {
    try {
      globalThis.dispatchEvent(new MessageEvent("message", { data: message, origin, source: globalThis }));
    } catch(e) {}
  }, 0);
};
globalThis.requestIdleCallback = globalThis.requestIdleCallback || function(cb, opts = {}) {
  const start = Date.now();
  const timeout = Number(opts && opts.timeout);
  const delay = Number.isFinite(timeout) && timeout >= 0 ? Math.min(timeout, 50) : 1;
  return setTimeout(() => {
    if (typeof cb !== "function") return;
    cb({
      didTimeout: Number.isFinite(timeout) && Date.now() - start >= timeout,
      timeRemaining() { return Math.max(0, 50 - (Date.now() - start)); },
    });
  }, delay);
};
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
      const name = typeof algorithm === 'string' ? algorithm : algorithm?.name || 'SHA-256';
      const bytes = new Uint8Array(data instanceof ArrayBuffer ? data : data.buffer || data);
      let hash = 0x811c9dc5;
      for (let i = 0; i < bytes.length; i++) { hash ^= bytes[i]; hash = Math.imul(hash, 0x01000193); }
      const size = name.includes('512') ? 64 : name.includes('384') ? 48 : 32;
      const result = new Uint8Array(size);
      for (let i = 0; i < size; i++) { hash = Math.imul(hash ^ i, 0x45d9f3b); result[i] = (hash >>> 0) & 0xff; }
      return result.buffer;
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

if (typeof EventSource === 'undefined') {
  globalThis.EventSource = class EventSource {
    constructor(url) { this.url = url; this.readyState = 0; this.onopen = null; this.onmessage = null; this.onerror = null; }
    close() { this.readyState = 2; }
    addEventListener() {} removeEventListener() {}
    static CONNECTING = 0; static OPEN = 1; static CLOSED = 2;
  };
}

if (typeof WebSocket === 'undefined') {
  globalThis.WebSocket = class WebSocket {
    constructor(url, protocols) { this.url = url; this.readyState = 0; this.bufferedAmount = 0; this.onopen = null; this.onmessage = null; this.onerror = null; this.onclose = null; this.protocol = ''; }
    send(data) {} close(code, reason) { this.readyState = 3; if (this.onclose) this.onclose({code:code||1000,reason:reason||'',wasClean:true}); }
    addEventListener() {} removeEventListener() {}
    static CONNECTING = 0; static OPEN = 1; static CLOSING = 2; static CLOSED = 3;
  };
}

if (typeof BroadcastChannel === 'undefined') {
  globalThis.BroadcastChannel = class BroadcastChannel {
    constructor(name) { this.name = name; this.onmessage = null; }
    postMessage(msg) {} close() {}
    addEventListener() {} removeEventListener() {}
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
  Document.prototype.elementFromPoint = function(x, y) {
    if (typeof x !== 'number' || typeof y !== 'number' || !isFinite(x) || !isFinite(y)) {
      return null;
    }
    var w = (typeof window !== 'undefined' && window.innerWidth) || 0;
    var h = (typeof window !== 'undefined' && window.innerHeight) || 0;
    if (x < 0 || y < 0 || x > w || y > h) {
      return null;
    }
    return this.body || this.documentElement || null;
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

globalThis.__obscura_apply_viewport = function(width, height, dpr) {
  const w = Math.max(1, Math.floor(Number(width) || 1920));
  const h = Math.max(1, Math.floor(Number(height) || 1000));
  const scale = Math.max(0.1, Number(dpr) || 2);
  globalThis.screen = {
    width: w,
    height: Math.max(h, 1),
    availWidth: w,
    availHeight: Math.max(h - 40, 1),
    colorDepth: 24,
    pixelDepth: 24,
    availTop: 0,
    availLeft: 0,
    orientation: {type:"landscape-primary",angle:0,addEventListener(){},removeEventListener(){},dispatchEvent(){return true;}},
  };
  globalThis.visualViewport = {
    width: w,
    height: h,
    offsetLeft: 0,
    offsetTop: 0,
    scale: 1,
    addEventListener(){},
    removeEventListener(){},
  };
  globalThis.devicePixelRatio = scale;
  globalThis.innerWidth = w;
  globalThis.innerHeight = h;
  globalThis.outerWidth = w;
  globalThis.outerHeight = h + 80;
  try { globalThis.dispatchEvent(new Event('resize')); } catch(e) {}
};

globalThis.__obscura_init = function() {
  _fpSeed = Date.now() ^ (Math.random() * 0xFFFFFFFF >>> 0);
  _fpCache = null;
  _installWasmStreamingFallback();

  globalThis.document = new Document(+_dom("document_node_id"));

  const scr = _fp('screen');
  const sw = scr[0], sh = scr[1];
  globalThis.__obscura_apply_viewport(sw, sh - 80, 2);

  const t0 = Date.now();
  globalThis.performance.timeOrigin = t0;
  globalThis.performance.timing = { navigationStart: t0, domContentLoadedEventEnd: t0, loadEventEnd: t0 };

  const hide = (obj, props) => {
    for (const p of props) {
      if (p in obj) {
        try { Object.defineProperty(obj, p, { enumerable: false, configurable: true }); } catch(e) {}
      }
    }
  };
  const toHide = Object.keys(globalThis).filter(k =>
    k.startsWith('_') || k.includes('obscura') || k.includes('Obscura')
  );
  for (const p of toHide) {
    try { Object.defineProperty(globalThis, p, { enumerable: false }); } catch(e) {
    }
  }
  delete globalThis.__obscura_init;
};
