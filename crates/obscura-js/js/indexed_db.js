// Included inside bootstrap's private closure when generating the startup snapshot.
const _idbToken = {};
const _idbSlots = new WeakMap();
const _idbConnections = new Map();
const _idbOpenQueues = new Map();
function _idbError(name) { return new DOMException(name, name); }
function _idbOp(action, data) {
  const result = JSON.parse(__obscuraCore.ops.op_indexed_db(action, JSON.stringify(data)));
  if (result && result.error) throw _idbError(result.error);
  return result;
}
function _idbTask(fn) { _browserPostedTaskEnqueue(fn, _schedulerPriorityRank['user-visible'] * 2); }
function _idbSlot(value) {
  const slot = _idbSlots.get(value);
  if (!slot) throw new TypeError('Illegal invocation');
  return slot;
}
function _idbNames(names) {
  const values = names().slice().sort();
  values.item = i => values[Number(i) >>> 0] ?? null;
  values.contains = name => values.includes(String(name));
  return values;
}
function _idbDispatch(target, type, init = {}) {
  const event = globalThis.__obscura_markTrusted(new IDBVersionChangeEvent(type, {
    bubbles: type === 'error' || type === 'abort', cancelable: type === 'error', ...init,
  }));
  event.target = target;
  event._dispatching = true;
  let threw = false, current = target;
  while (current) {
    event.currentTarget = current;
    event.eventPhase = current === target ? 2 : 3;
    const listeners = (_eventTargetListeners.get(current)?.get(type) || []).slice();
    const invoke = callback => {
      try {
        if (typeof callback === 'function') callback.call(current, event);
        else callback?.handleEvent(event);
      } catch (error) { threw = true; globalThis.reportError(error); }
    };
    for (const listener of listeners) {
      if (event._immediatePropagationStopped) break;
      if (!(_eventTargetListeners.get(current)?.get(type) || []).includes(listener)) continue;
      if (listener.once) _eventTargetRemove(current, type, listener.callback, listener.capture);
      invoke(listener.callback);
    }
    if (!event.bubbles || event._propagationStopped) break;
    const slot = _idbSlot(current);
    current = slot.transaction || slot.db || null;
  }
  event.currentTarget = null; event.eventPhase = 0;
  event._dispatching = false;
  return { canceled:event.defaultPrevented, threw };
}
function _idbInitialize(target, token, state) {
  if (token !== _idbToken) throw new TypeError('Illegal constructor');
  _idbSlots.set(target, {handlers:{},handlerListeners:{}, ...state});
}
class IDBVersionChangeEvent extends Event {
  constructor(type, init = {}) {
    super(type, init);
    this.oldVersion = Number(init.oldVersion || 0);
    this.newVersion = init.newVersion == null ? null : Number(init.newVersion);
  }
}
class IDBRequest {
  constructor(token, state = {}) { _idbInitialize(this,token,{result:undefined,error:null,source:null,transaction:null,readyState:'pending',...state}); }
  get result() { const s = _idbSlot(this); if (s.readyState !== 'done') throw _idbError('InvalidStateError'); return s.result; }
  get error() { const s = _idbSlot(this); if (s.readyState !== 'done') throw _idbError('InvalidStateError'); return s.error; }
  get source() { return _idbSlot(this).source; }
  get transaction() { return _idbSlot(this).transaction; }
  get readyState() { return _idbSlot(this).readyState; }
}
class IDBOpenDBRequest extends IDBRequest {}
function _idbSettle(request, result, error = null) {
  Object.assign(_idbSlot(request), {result,error,readyState:'done'});
  return _idbDispatch(request, error ? 'error' : 'success');
}
function _idbKey(value, seen = new Set()) {
  if (typeof value === 'number' && !Number.isNaN(value)) return [0,String(value === 0 ? 0 : value)];
  if (value instanceof Date && !Number.isNaN(value.getTime())) return [1,String(value.getTime())];
  if (typeof value === 'string') return [2,value];
  if (value instanceof ArrayBuffer || ArrayBuffer.isView(value)) {
    const bytes = value instanceof ArrayBuffer ? new Uint8Array(value)
      : new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    return [3,Array.from(bytes)];
  }
  if (Array.isArray(value) && !seen.has(value)) {
    seen.add(value);
    const keys = [];
    for (let i = 0; i < value.length; i++) {
      if (!(i in value)) throw _idbError('DataError');
      keys.push(_idbKey(value[i], seen));
    }
    seen.delete(value);
    return [4,keys];
  }
  throw _idbError('DataError');
}
function _idbCompare(a, b) {
  if (a[0] !== b[0]) return a[0] < b[0] ? -1 : 1;
  if (a[0] <= 1) return Math.sign(Number(a[1]) - Number(b[1])) || (a[1] === b[1] ? 0 : Number(a[1]) < Number(b[1]) ? -1 : 1);
  if (a[0] === 2) return a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0;
  const x = a[1], y = b[1];
  for (let i = 0; i < Math.min(x.length, y.length); i++) {
    const cmp = a[0] === 4 ? _idbCompare(x[i], y[i]) : Math.sign(x[i] - y[i]);
    if (cmp) return cmp;
  }
  return Math.sign(x.length - y.length);
}
function _idbKeyValue(key) {
  switch (key[0]) {
    case 0: return Number(key[1]);
    case 1: return new Date(Number(key[1]));
    case 2: return key[1];
    case 3: return new Uint8Array(key[1]).buffer;
    default: return key[1].map(_idbKeyValue);
  }
}
class IDBKeyRange {
  constructor(token, lower, upper, lowerOpen, upperOpen) {
    if (token !== _idbToken) throw new TypeError('Illegal constructor');
    _idbSlots.set(this, {lower,upper,lowerOpen,upperOpen});
  }
  get lower() { const k = _idbSlot(this).lower; return k ? _idbKeyValue(k) : undefined; }
  get upper() { const k = _idbSlot(this).upper; return k ? _idbKeyValue(k) : undefined; }
  get lowerOpen() { return _idbSlot(this).lowerOpen; }
  get upperOpen() { return _idbSlot(this).upperOpen; }
  includes(value) { return _idbInRange(_idbKey(value), this); }
  static only(value) { const key = _idbKey(value); return new IDBKeyRange(_idbToken,key,key,false,false); }
  static lowerBound(value, open = false) { return new IDBKeyRange(_idbToken,_idbKey(value),null,!!open,true); }
  static upperBound(value, open = false) { return new IDBKeyRange(_idbToken,null,_idbKey(value),true,!!open); }
  static bound(lower, upper, lowerOpen = false, upperOpen = false) {
    const a = _idbKey(lower), b = _idbKey(upper), cmp = _idbCompare(a,b);
    if (cmp > 0 || cmp === 0 && (lowerOpen || upperOpen)) throw _idbError('DataError');
    return new IDBKeyRange(_idbToken,a,b,!!lowerOpen,!!upperOpen);
  }
}
function _idbRange(value, required = false) {
  if (value instanceof IDBKeyRange) return value;
  if (value === undefined || value === null) { if (required) throw _idbError('DataError'); return null; }
  return IDBKeyRange.only(value);
}
function _idbInRange(key, range) {
  if (!range) return true;
  const r = _idbSlot(range);
  const low = r.lower ? _idbCompare(key,r.lower) : 1;
  const high = r.upper ? _idbCompare(key,r.upper) : -1;
  return (low > 0 || low === 0 && !r.lowerOpen) && (high < 0 || high === 0 && !r.upperOpen);
}
// Keep platform payloads private, particularly non-extractable CryptoKey material.
const _idbPlatformCodecs = [{
  name:'Blob', test:value => value instanceof Blob,
  encode:value => ({bytes:value._bytes,type:value.type,file:value instanceof File,name:value.name,lastModified:value.lastModified}),
  decode:data => data.file ? new File([data.bytes],data.name,data) : new Blob([data.bytes],data),
}];
function _idbPrepare(value, seen, platform) {
  if (value === null || typeof value !== 'object') return value;
  if (seen.has(value)) return seen.get(value);
  if (__obscuraCore.isProxy(value)) throw _idbError('DataCloneError');
  for (const codec of _idbPlatformCodecs) {
    if (!codec.test(value)) continue;
    const marker = {};
    seen.set(value,marker);
    platform.push([marker,codec.name,codec.encode(value)]);
    return marker;
  }
  if (value instanceof Node || typeof WeakRef === 'function' && value instanceof WeakRef) throw _idbError('DataCloneError');
  if (__obscuraCore.isAnyArrayBuffer(value) || __obscuraCore.isArrayBufferView(value)
      || __obscuraCore.isDate(value) || __obscuraCore.isRegExp(value) || __obscuraCore.isBoxedPrimitive(value)
      || __obscuraCore.isPromise(value) || __obscuraCore.isWeakMap(value) || __obscuraCore.isWeakSet(value)
      || __obscuraCore.isModuleNamespaceObject(value)) return value;
  let copy;
  if (__obscuraCore.isMap(value)) {
    copy = new Map(); seen.set(value,copy);
    Map.prototype.forEach.call(value,(v,k) => copy.set(_idbPrepare(k,seen,platform),_idbPrepare(v,seen,platform)));
  } else if (__obscuraCore.isSet(value)) {
    copy = new Set(); seen.set(value,copy);
    Set.prototype.forEach.call(value,v => copy.add(_idbPrepare(v,seen,platform)));
  } else if (__obscuraCore.isNativeError(value)) {
    const type = ['EvalError','RangeError','ReferenceError','SyntaxError','TypeError','URIError'].includes(value.name) ? globalThis[value.name] : Error;
    copy = new type(value.message); seen.set(value,copy);
    copy.stack = value.stack;
    if (Object.hasOwn(value,'cause')) copy.cause = _idbPrepare(value.cause,seen,platform);
  } else {
    copy = Array.isArray(value) ? new Array(value.length) : {}; seen.set(value,copy);
    for (const key of Object.keys(value)) Object.defineProperty(copy,key,{
      value:_idbPrepare(value[key],seen,platform),writable:true,enumerable:true,configurable:true,
    });
  }
  return copy;
}
function _idbRevive(value, replacements, seen = new Set()) {
  if (value === null || typeof value !== 'object') return value;
  if (replacements.has(value)) return replacements.get(value);
  if (seen.has(value)) return value;
  seen.add(value);
  if (__obscuraCore.isMap(value)) {
    const entries = Array.from(value); value.clear();
    for (const [k,v] of entries) value.set(_idbRevive(k,replacements,seen),_idbRevive(v,replacements,seen));
  } else if (__obscuraCore.isSet(value)) {
    const entries = Array.from(value); value.clear();
    for (const v of entries) value.add(_idbRevive(v,replacements,seen));
  } else {
    for (const key of Object.keys(value)) value[key] = _idbRevive(value[key],replacements,seen);
    if (__obscuraCore.isNativeError(value) && Object.hasOwn(value,'cause')) value.cause = _idbRevive(value.cause,replacements,seen);
  }
  return value;
}
function _idbSerialize(value) {
  try {
    const platform = [];
    const prepared = _idbPrepare(value,new Map(),platform);
    const bytes = __obscuraCore.serialize({value:prepared,platform}, {forStorage:true}, () => {throw _idbError('DataCloneError');});
    return btoa(_bytesToBinaryString(bytes));
  } catch (_) { throw _idbError('DataCloneError'); }
}
function _idbDeserialize(value) {
  const decoded = __obscuraCore.deserialize(_base64ToUint8Array(value), {forStorage:true});
  if (!decoded.platform.length) return decoded.value;
  const replacements = new Map(decoded.platform.map(([marker,name,data]) =>
    [marker,_idbPlatformCodecs.find(codec => codec.name === name).decode(data)]));
  return _idbRevive(decoded.value,replacements);
}
function _idbPath(value, path) {
  if (Array.isArray(path)) {
    const result = path.map(p => _idbPath(value,p));
    return result.includes(undefined) ? undefined : result;
  }
  if (path === '') return value;
  for (const name of path.split('.')) {
    if (value == null || !(name in Object(value))) return undefined;
    value = value[name];
  }
  return value;
}
function _idbInject(value, path, key) {
  const names = path.split('.');
  for (const name of names.slice(0,-1)) {
    if (value === null || typeof value !== 'object') throw _idbError('DataError');
    if (!Object.hasOwn(value,name)) Object.defineProperty(value,name,{value:{},writable:true,enumerable:true,configurable:true});
    value = value[name];
  }
  if (value === null || typeof value !== 'object') throw _idbError('DataError');
  Object.defineProperty(value,names.at(-1),{value:key,writable:true,enumerable:true,configurable:true});
}
function _idbValidatePath(path) {
  if (Array.isArray(path)) {
    if (!path.length) throw _idbError('SyntaxError');
    return path.map(_idbValidatePath);
  }
  path = String(path);
  if (path && !path.split('.').every(name => /^[$_\p{ID_Start}][$\u200c\u200d\p{ID_Continue}]*$/u.test(name))) throw _idbError('SyntaxError');
  return path;
}

function _idbActive(state) {
  return state.state === 'active' && state.task === __obscuraCore.ops.op_indexed_db_task();
}
class IDBTransaction {
  constructor(token, db, names, mode, id = null, schema = null) {
    const d = _idbSlot(db);
    _idbInitialize(this,token,{db, names, mode, error:null, state:'active', task:__obscuraCore.ops.op_indexed_db_task(), started:!!schema, schema,
      requests:[], stores:new Map(), scheduled:false, autoCommit:false, committing:false,
      id:id ?? _idbOp('begin',{name:d.name,connection:d.connection,stores:names,mode})});
    d.transactions.add(this);
    _idbTask(() => { const s = _idbSlot(this); s.autoCommit = true; _idbRun(this); });
  }
  get db() { return _idbSlot(this).db; }
  get mode() { return _idbSlot(this).mode; }
  get error() { return _idbSlot(this).error; }
  get durability() { return 'default'; }
  get objectStoreNames() { const s = _idbSlot(this); return _idbNames(() => s.mode === 'versionchange' ? s.schema.map(x => x.name) : s.names); }
  objectStore(name) {
    const s = _idbSlot(this); name = String(name);
    if (s.state === 'finished') throw _idbError('InvalidStateError');
    const schemas = s.schema || _idbSlot(s.db).schema;
    if (!(s.mode === 'versionchange' || s.names.includes(name)) || !schemas.some(x => x.name === name)) throw _idbError('NotFoundError');
    if (!s.stores.has(name)) s.stores.set(name, new IDBObjectStore(_idbToken,this,name));
    return s.stores.get(name);
  }
  abort() { const s = _idbSlot(this); if (s.state === 'finished' || s.committing) throw _idbError('InvalidStateError'); _idbAbort(this,null); }
  commit() {
    const s = _idbSlot(this);
    if (!_idbActive(s) || s.committing) throw _idbError('InvalidStateError');
    s.committing = true; _idbSchedule(this);
  }
}
function _idbSchedule(tx) {
  const s = _idbSlot(tx);
  if (!s.scheduled && s.state !== 'finished') { s.scheduled = true; _idbTask(() => {s.scheduled = false; _idbRun(tx);}); }
}
function _idbRequestFor(source, operation) {
  const tx = source instanceof IDBTransaction ? source : _idbSlot(source).transaction;
  const s = _idbSlot(tx);
  if (!_idbActive(s) || s.committing) throw _idbError('TransactionInactiveError');
  const request = new IDBRequest(_idbToken,{source,transaction:tx});
  s.requests.push({request,operation});
  _idbSchedule(tx);
  return request;
}
function _idbRun(tx) {
  const s = _idbSlot(tx);
  if (s.state === 'finished') return;
  s.state = 'inactive';
  try {
    if (!s.started) {
      const info = _idbOp('ready',{id:s.id});
      if (!info) { setTimeout(() => _idbSchedule(tx), 1); return; }
      s.schema = info.schema;
      s.started = true;
    }
    const pending = s.requests.shift();
    if (pending) {
      s.task = __obscuraCore.ops.op_indexed_db_task();
      s.state = s.committing ? 'committing' : 'active';
      let result, error = null;
      try { result = pending.operation(); } catch (e) { error = e; }
      s.state = s.committing ? 'committing' : 'active';
      const event = _idbSettle(pending.request, result, error);
      if (s.state === 'finished') return;
      if (event.threw || error && !event.canceled) { _idbAbort(tx,error || _idbError('AbortError')); return; }
      // Promise reactions from a request are still in its active task.
      queueMicrotask(() => _idbSchedule(tx));
      return;
    }
    if (!s.autoCommit) return;
    _idbOp('commit',{id:s.id,schema:s.schema,version:_idbSlot(s.db).version});
    s.state = 'finished';
    const d = _idbSlot(s.db);
    d.schema = s.schema;
    d.transactions.delete(tx);
    if (d.upgrade === tx) d.upgrade = null;
    _idbDispatch(tx,'complete');
    if (s.onCommit) s.onCommit();
    if (d.closePending && !d.transactions.size) _idbClose(s.db);
  } catch (error) { _idbAbort(tx,error); }
}
function _idbAbort(tx, error) {
  const s = _idbSlot(tx);
  if (s.state === 'finished') return;
  s.state = 'finished'; s.error = error;
  _idbOp('abort',{id:s.id});
  const pending = s.requests.splice(0);
  const d = _idbSlot(s.db);
  d.transactions.delete(tx);
  if (d.upgrade === tx) { d.upgrade = null; d.schema = s.originalSchema || []; d.version = s.oldVersion; }
  _idbTask(() => {
    for (const entry of pending) _idbSettle(entry.request,undefined,_idbError('AbortError'));
    _idbDispatch(tx,'abort');
    if (s.onAbort) s.onAbort();
    if (d.closePending && !d.transactions.size) _idbClose(s.db);
  });
}
function _idbStoreSchema(store, write = false) {
  const h = _idbSlot(store), t = _idbSlot(h.transaction);
  if (!_idbActive(t) || t.committing) throw _idbError('TransactionInactiveError');
  const schema = (t.schema || _idbSlot(t.db).schema).find(x => x.name === h.name);
  if (!schema) throw _idbError('InvalidStateError');
  if (write && t.mode === 'readonly') throw _idbError('ReadOnlyError');
  return schema;
}
function _idbRecords(store, range = null) {
  const h = _idbSlot(store), t = _idbSlot(h.transaction);
  return _idbOp('entries',{id:t.id,store:h.name}).map(([key,value]) => [JSON.parse(key),value])
    .filter(([key]) => _idbInRange(key,range)).sort((a,b) => _idbCompare(a[0],b[0]));
}
class IDBObjectStore {
  constructor(token, transaction, name) {
    if (token !== _idbToken) throw new TypeError('Illegal constructor');
    _idbSlots.set(this,{transaction,name,indexes:new Map()});
  }
  get name() { return _idbSlot(this).name; }
  get transaction() { return _idbSlot(this).transaction; }
  get keyPath() { const t = _idbSlot(this.transaction); return (t.schema || _idbSlot(t.db).schema).find(s => s.name === this.name)?.keyPath; }
  get autoIncrement() { const t = _idbSlot(this.transaction); return !!(t.schema || _idbSlot(t.db).schema).find(s => s.name === this.name)?.autoIncrement; }
  get indexNames() { const t = _idbSlot(this.transaction); return _idbNames(() => ((t.schema || _idbSlot(t.db).schema).find(s => s.name === this.name)?.indexes || []).map(i => i.name)); }
  put(value, key) { return _idbWrite(this,value,key,false); }
  add(value, key) { return _idbWrite(this,value,key,true); }
  get(query) { return _idbRead(this,query,false,false,true); }
  getKey(query) { return _idbRead(this,query,true,false,true); }
  getAll(query, count) { return _idbRead(this,query,false,true,false,count); }
  getAllKeys(query, count) { return _idbRead(this,query,true,true,false,count); }
  count(query) {
    _idbStoreSchema(this);
    const range = _idbRange(query);
    return _idbRequestFor(this,() => _idbRecords(this,range).length);
  }
  delete(query) {
    _idbStoreSchema(this,true);
    const range = _idbRange(query,true);
    return _idbRequestFor(this,() => {
      const t = _idbSlot(this.transaction);
      for (const [key] of _idbRecords(this,range)) _idbOp('remove',{id:t.id,store:this.name,key:JSON.stringify(key)});
    });
  }
  clear() {
    _idbStoreSchema(this,true);
    return _idbRequestFor(this,() => { _idbOp('clear',{id:_idbSlot(this.transaction).id,store:this.name}); });
  }
  index(name) {
    const schema = _idbStoreSchema(this), h = _idbSlot(this); name = String(name);
    if (!schema.indexes.some(i => i.name === name)) throw _idbError('NotFoundError');
    if (!h.indexes.has(name)) h.indexes.set(name,new IDBIndex(_idbToken,this,name));
    return h.indexes.get(name);
  }
  createIndex(name, keyPath, options = {}) {
    const schema = _idbStoreSchema(this,true); name = String(name);
    if (this.transaction.mode !== 'versionchange') throw _idbError('InvalidStateError');
    if (schema.indexes.some(i => i.name === name)) throw _idbError('ConstraintError');
    keyPath = _idbValidatePath(keyPath);
    if (options.multiEntry && Array.isArray(keyPath)) throw _idbError('InvalidAccessError');
    schema.indexes.push({name,keyPath,unique:!!options.unique,multiEntry:!!options.multiEntry});
    const index = this.index(name);
    _idbRequestFor(this,() => { _idbIndexRows(index,null,true); });
    return index;
  }
  deleteIndex(name) {
    const schema = _idbStoreSchema(this,true); name = String(name);
    if (this.transaction.mode !== 'versionchange') throw _idbError('InvalidStateError');
    const at = schema.indexes.findIndex(i => i.name === name);
    if (at < 0) throw _idbError('NotFoundError');
    schema.indexes.splice(at,1);
  }
  openCursor(query, direction = 'next') { return _idbCursor(this,query,direction,false); }
  openKeyCursor(query, direction = 'next') { return _idbCursor(this,query,direction,true); }
}
function _idbWrite(store, value, key, add) {
  const schema = _idbStoreSchema(store,true);
  const serialized = _idbSerialize(value);
  const clone = _idbDeserialize(serialized);
  if (schema.keyPath !== null) {
    if (key !== undefined) throw _idbError('DataError');
    key = _idbPath(clone,schema.keyPath);
    if (key === undefined && !schema.autoIncrement) throw _idbError('DataError');
  }
  let encoded = key === undefined ? null : _idbKey(key);
  if (!encoded && !schema.autoIncrement) throw _idbError('DataError');
  return _idbRequestFor(store,() => {
    const t = _idbSlot(store.transaction), live = t.schema.find(x => x.name === store.name);
    let payload = serialized;
    let nextKey = live.nextKey;
    if (!encoded) {
      if (live.nextKey > Number.MAX_SAFE_INTEGER) throw _idbError('ConstraintError');
      key = live.nextKey; nextKey++; encoded = _idbKey(key);
      if (live.keyPath !== null) { _idbInject(clone,live.keyPath,key); payload = _idbSerialize(clone); }
    } else if (live.autoIncrement && encoded[0] === 0 && Number(encoded[1]) >= live.nextKey) {
      nextKey = Math.floor(Number(encoded[1])) + 1;
    }
    const rawKey = JSON.stringify(encoded);
    if (add && _idbOp('get',{id:t.id,store:store.name,key:rawKey}) !== null) throw _idbError('ConstraintError');
    for (const index of live.indexes.filter(i => i.unique)) {
      const wanted = _idbIndexKeys(index,clone);
      if (!wanted.length) continue;
      for (const [other, data] of _idbRecords(store)) {
        if (_idbCompare(other,encoded) === 0) continue;
        if (_idbIndexKeys(index,_idbDeserialize(data)).some(k => wanted.some(w => _idbCompare(k,w) === 0))) throw _idbError('ConstraintError');
      }
    }
    _idbOp('put',{id:t.id,store:store.name,key:rawKey,value:payload});
    live.nextKey = nextKey;
    return _idbKeyValue(encoded);
  });
}
function _idbCountLimit(count) {
  if (count === undefined) return Infinity;
  count = Number(count);
  if (!Number.isFinite(count) || count < 0 || count > 0xffffffff) throw new TypeError('Invalid count');
  return Math.trunc(count) || Infinity;
}
function _idbRead(source, query, keys, all, required, count) {
  const store = source instanceof IDBIndex ? source.objectStore : source;
  _idbStoreSchema(store);
  const range = _idbRange(query,required), limit = _idbCountLimit(count);
  return _idbRequestFor(source,() => {
    let records;
    if (source instanceof IDBIndex) records = _idbIndexRows(source,range).map(row => [row[1],row[2]]);
    else if (range && _idbSlot(range).lower && _idbSlot(range).upper
        && _idbCompare(_idbSlot(range).lower,_idbSlot(range).upper) === 0) {
      const key = _idbSlot(range).lower;
      const value = _idbOp('get',{id:_idbSlot(store.transaction).id,store:store.name,key:JSON.stringify(key)});
      records = value === null ? [] : [[key,value]];
    } else records = _idbRecords(store,range);
    const values = records.slice(0,all ? limit : 1).map(([key,value]) => keys ? _idbKeyValue(key) : _idbDeserialize(value));
    return all ? values : values[0];
  });
}
function _idbIndexSchema(index) {
  const store = _idbSlot(index.objectStore), tx = _idbSlot(store.transaction);
  const schema = (tx.schema || _idbSlot(tx.db).schema).find(s => s.name === store.name);
  const definition = schema?.indexes.find(i => i.name === _idbSlot(index).name);
  if (!definition) throw _idbError('InvalidStateError');
  return definition;
}
function _idbIndexKeys(definition, value) {
  const key = _idbPath(value,definition.keyPath);
  const values = definition.multiEntry && Array.isArray(key) ? key : [key];
  const result = [];
  for (const value of values) {
    try { const encoded = _idbKey(value); if (!result.some(k => !_idbCompare(k,encoded))) result.push(encoded); } catch (_) {}
  }
  return result;
}
function _idbIndexRows(index, range, validate = false) {
  const definition = _idbIndexSchema(index), rows = [];
  for (const [primary,value] of _idbRecords(index.objectStore)) {
    for (const key of _idbIndexKeys(definition,_idbDeserialize(value))) if (_idbInRange(key,range)) rows.push([key,primary,value]);
  }
  rows.sort((a,b) => _idbCompare(a[0],b[0]) || _idbCompare(a[1],b[1]));
  if (validate && definition.unique && rows.some((r,i) => i && _idbCompare(rows[i-1][0],r[0]) === 0)) throw _idbError('ConstraintError');
  return rows;
}
class IDBIndex {
  constructor(token, objectStore, name) {
    if (token !== _idbToken) throw new TypeError('Illegal constructor');
    _idbSlots.set(this,{objectStore,name,transaction:objectStore.transaction});
  }
  get name() { return _idbSlot(this).name; }
  get objectStore() { return _idbSlot(this).objectStore; }
  get keyPath() { return _idbIndexSchema(this).keyPath; }
  get unique() { return _idbIndexSchema(this).unique; }
  get multiEntry() { return _idbIndexSchema(this).multiEntry; }
  get(query) { return _idbRead(this,query,false,false,true); }
  getKey(query) { return _idbRead(this,query,true,false,true); }
  getAll(query, count) { return _idbRead(this,query,false,true,false,count); }
  getAllKeys(query, count) { return _idbRead(this,query,true,true,false,count); }
  count(query) { _idbIndexSchema(this); const range = _idbRange(query); return _idbRequestFor(this,() => _idbIndexRows(this,range).length); }
  openCursor(query, direction = 'next') { return _idbCursor(this,query,direction,false); }
  openKeyCursor(query, direction = 'next') { return _idbCursor(this,query,direction,true); }
}
class IDBCursor {
  constructor(token, state) { if (token !== _idbToken) throw new TypeError('Illegal constructor'); _idbSlots.set(this,state); }
  get source() { return _idbSlot(this).source; }
  get direction() { return _idbSlot(this).direction; }
  get key() { return _idbKeyValue(_idbSlot(this).row[0]); }
  get primaryKey() { return _idbKeyValue(_idbSlot(this).row[1]); }
  get request() { return _idbSlot(this).request; }
  continue(key) { _idbContinue(this,key); }
  advance(count) {
    count = Number(count);
    if (!Number.isInteger(count) || count <= 0 || count > 0xffffffff) throw new TypeError('Invalid count');
    _idbContinue(this,undefined,count);
  }
  update(value) {
    const s = _idbSlot(this);
    if (!s.positioned || s.keysOnly) throw _idbError('InvalidStateError');
    const store = s.source instanceof IDBIndex ? s.source.objectStore : s.source;
    if (store.keyPath !== null) {
      const key = _idbPath(value,store.keyPath);
      if (key === undefined || _idbCompare(_idbKey(key),s.row[1])) throw _idbError('DataError');
      return store.put(value);
    }
    return store.put(value,this.primaryKey);
  }
  delete() {
    const s = _idbSlot(this);
    if (!s.positioned || s.keysOnly) throw _idbError('InvalidStateError');
    return (s.source instanceof IDBIndex ? s.source.objectStore : s.source).delete(this.primaryKey);
  }
}
class IDBCursorWithValue extends IDBCursor {
  get value() { return _idbSlot(this).value; }
}
function _idbCursor(source, query, direction, keysOnly) {
  const store = source instanceof IDBIndex ? source.objectStore : source;
  _idbStoreSchema(store);
  direction = String(direction);
  if (!['next','nextunique','prev','prevunique'].includes(direction)) throw new TypeError('Invalid cursor direction');
  const state = {source,range:_idbRange(query),direction,keysOnly,positioned:false,row:null,value:undefined,request:null};
  const cursor = new (keysOnly ? IDBCursor : IDBCursorWithValue)(_idbToken,state);
  const request = _idbRequestFor(source,() => _idbCursorNext(cursor,undefined,1));
  state.request = request;
  return request;
}
function _idbCursorNext(cursor, sought, count) {
  const s = _idbSlot(cursor), reverse = s.direction.startsWith('prev');
  let rows = s.source instanceof IDBIndex ? _idbIndexRows(s.source,s.range)
    : _idbRecords(s.source,s.range).map(([key,value]) => [key,key,value]);
  if (reverse) rows.reverse();
  if (s.direction.endsWith('unique')) rows = rows.filter((row,i) => !i || _idbCompare(row[0],rows[i-1][0]));
  rows = rows.filter(row => {
    if (s.row) {
      const cmp = _idbCompare(row[0],s.row[0]) || (s.direction.endsWith('unique') ? 0 : _idbCompare(row[1],s.row[1]));
      if (reverse ? cmp >= 0 : cmp <= 0) return false;
    }
    return !sought || (reverse ? _idbCompare(row[0],sought) <= 0 : _idbCompare(row[0],sought) >= 0);
  });
  const next = rows[count-1];
  s.positioned = !!next;
  if (!next) return null;
  s.row = next;
  if (!s.keysOnly) s.value = _idbDeserialize(next[2]);
  return cursor;
}
function _idbContinue(cursor, key, count = 1) {
  const s = _idbSlot(cursor), tx = _idbSlot(_idbSlot(s.request).transaction);
  if (!_idbActive(tx) || tx.committing) throw _idbError('TransactionInactiveError');
  if (!s.positioned) throw _idbError('InvalidStateError');
  const sought = key === undefined ? null : _idbKey(key);
  if (sought) {
    const cmp = _idbCompare(sought,s.row[0]);
    if (s.direction.startsWith('prev') ? cmp >= 0 : cmp <= 0) throw _idbError('DataError');
  }
  s.positioned = false;
  Object.assign(_idbSlot(s.request),{readyState:'pending',result:undefined,error:null});
  tx.requests.push({request:s.request,operation:() => _idbCursorNext(cursor,sought,count)});
  _idbSchedule(_idbSlot(s.request).transaction);
}

class IDBDatabase {
  constructor(token, name, version, schema, connection) {
    _idbInitialize(this,token,{name,version,schema,connection,transactions:new Set(),upgrade:null,closePending:false,closed:false});
    if (connection) _idbConnections.set(connection,this);
  }
  get name() { return _idbSlot(this).name; }
  get version() { return _idbSlot(this).version; }
  get objectStoreNames() { return _idbNames(() => (_idbSlot(this).upgrade ? _idbSlot(_idbSlot(this).upgrade).schema : _idbSlot(this).schema).map(s => s.name)); }
  createObjectStore(name, options = {}) {
    const d = _idbSlot(this); name = String(name);
    if (!d.upgrade) throw _idbError('InvalidStateError');
    const t = _idbSlot(d.upgrade);
    if (!_idbActive(t)) throw _idbError('TransactionInactiveError');
    if (t.schema.some(s => s.name === name)) throw _idbError('ConstraintError');
    const keyPath = options.keyPath == null ? null : _idbValidatePath(options.keyPath);
    if (options.autoIncrement && (keyPath === '' || Array.isArray(keyPath))) throw _idbError('InvalidAccessError');
    t.schema.push({name,keyPath,autoIncrement:!!options.autoIncrement,nextKey:1,indexes:[]});
    return d.upgrade.objectStore(name);
  }
  deleteObjectStore(name) {
    const d = _idbSlot(this); name = String(name);
    if (!d.upgrade) throw _idbError('InvalidStateError');
    const t = _idbSlot(d.upgrade);
    if (!_idbActive(t)) throw _idbError('TransactionInactiveError');
    const at = t.schema.findIndex(s => s.name === name);
    if (at < 0) throw _idbError('NotFoundError');
    _idbOp('clear',{id:t.id,store:name});
    t.schema.splice(at,1); t.stores.delete(name);
  }
  transaction(storeNames, mode = 'readonly', options = {}) {
    const d = _idbSlot(this);
    if (d.closed || d.closePending || d.upgrade) throw _idbError('InvalidStateError');
    mode = String(mode);
    if (mode !== 'readonly' && mode !== 'readwrite') throw new TypeError('Invalid transaction mode');
    if (options.durability !== undefined && !['default','strict','relaxed'].includes(options.durability)) throw new TypeError('Invalid durability');
    const names = [...new Set(typeof storeNames === 'string' ? [storeNames] : Array.from(storeNames,String))].sort();
    if (!names.length) throw _idbError('InvalidAccessError');
    for (const name of names) if (!d.schema.some(s => s.name === name)) throw _idbError('NotFoundError');
    return new IDBTransaction(_idbToken,this,names,mode);
  }
  close() { const d = _idbSlot(this); d.closePending = true; if (!d.transactions.size) _idbClose(this); }
}
function _idbClose(db) {
  const d = _idbSlot(db);
  if (d.closed) return;
  if (d.connection) _idbOp('close',{name:d.name,connection:d.connection});
  _idbConnections.delete(d.connection);
  d.closed = true;
}
function _idbPollNotices(notices = _idbOp('notices',{})) {
  for (const notice of notices) {
    const db = _idbConnections.get(notice.connection);
    if (db && !_idbSlot(db).closePending) _idbDispatch(db,'versionchange',{oldVersion:db.version,newVersion:notice.version});
  }
}
function _idbOpen(name, requestedVersion, deleting) {
  if (!__obscuraCore.ops.op_storage_access()) throw _idbError('SecurityError');
  const request = new IDBOpenDBRequest(_idbToken);
  let queue = _idbOpenQueues.get(name);
  if (!queue) _idbOpenQueues.set(name,queue = []);
  let blocked = false, notified = false, id = null, opening = null;
  const finish = () => {
    if (opening !== null) _idbOp('release_open',{name,opening});
    queue.shift(); if (queue.length) _idbTask(queue[0]); else _idbOpenQueues.delete(name);
  };
  const attempt = () => {
    try {
      if (opening === null) opening = _idbOp('reserve_open',{name});
      if (!_idbOp('open_ready',{name,opening})) { setTimeout(attempt,1); return; }
      const info = _idbOp('info',{name}), version = deleting ? null : requestedVersion ?? (info.version || 1);
      if (!deleting && version < info.version) throw _idbError('VersionError');
      if (deleting || version > info.version) {
        if (!notified) {
          notified = true;
          _idbOp('notify',{name,version});
          _idbPollNotices();
        }
        if (_idbOp('info',{name}).connections) {
          if (!blocked) { blocked = true; _idbDispatch(request,'blocked',{oldVersion:info.version,newVersion:version}); }
          setTimeout(attempt,5); return;
        }
      }
      if (id === null) id = _idbOp('begin',{name,mode:'versionchange',stores:[]});
      const live = _idbOp('ready',{id});
      if (!live) { setTimeout(attempt,1); return; }
      if (deleting) {
        _idbOp('abort',{id}); id = null;
        if (!_idbOp('delete',{name})) { setTimeout(attempt,1); return; }
        _idbSettle(request,undefined); finish(); return;
      }
      if (version < live.version) throw _idbError('VersionError');
      const db = new IDBDatabase(_idbToken,name,version,live.schema,0);
      const succeed = () => {
        const d = _idbSlot(db);
        d.connection = _idbOp('connect',{name}); _idbConnections.set(d.connection,db);
        _idbSlot(request).transaction = null;
        _idbSettle(request,db); finish();
      };
      if (version > live.version) {
        const tx = new IDBTransaction(_idbToken,db,live.schema.map(s => s.name),'versionchange',id,live.schema);
        const t = _idbSlot(tx), d = _idbSlot(db);
        t.originalSchema = __obscuraCore.deserialize(__obscuraCore.serialize(live.schema));
        t.oldVersion = live.version;
        d.upgrade = tx;
        Object.assign(_idbSlot(request),{result:db,readyState:'done',transaction:tx});
        t.onCommit = succeed;
        t.onAbort = () => {d.closed = true; _idbSlot(request).transaction = null; _idbSettle(request,undefined,_idbError('AbortError')); finish();};
        const outcome = _idbDispatch(request,'upgradeneeded',{oldVersion:live.version,newVersion:version});
        if (outcome.threw) _idbAbort(tx,_idbError('AbortError'));
      } else {
        _idbOp('abort',{id}); succeed();
      }
    } catch (error) {
      if (id !== null) { try { _idbOp('abort',{id}); } catch (_) {} }
      _idbSettle(request,undefined,error); finish();
    }
  };
  queue.push(attempt);
  if (queue.length === 1) _idbTask(attempt);
  return request;
}
class IDBFactory {
  constructor(token) { if (token !== _idbToken) throw new TypeError('Illegal constructor'); }
  open(name, version) {
    if (!arguments.length) throw new TypeError('Database name required');
    if (version !== undefined) {
      version = Number(version);
      if (!Number.isFinite(version) || version < 1 || version > Number.MAX_SAFE_INTEGER) throw new TypeError('Invalid version');
      version = Math.floor(version);
    }
    return _idbOpen(String(name),version,false);
  }
  deleteDatabase(name) { if (!arguments.length) throw new TypeError('Database name required'); return _idbOpen(String(name),null,true); }
  databases() {
    try { return Promise.resolve(_idbOp('databases',{})); } catch (error) { return Promise.reject(error); }
  }
  cmp(a,b) { return _idbCompare(_idbKey(a),_idbKey(b)); }
}
for (const [klass, events] of [[IDBRequest,['success','error']], [IDBOpenDBRequest,['blocked','upgradeneeded']],
  [IDBTransaction,['complete','abort','error']], [IDBDatabase,['versionchange','close','error','abort']]]) {
  for (const type of events) Object.defineProperty(klass.prototype,'on'+type,{
    get(){return _idbSlot(this).handlers[type] || null;},
    set(value){
      const slot = _idbSlot(this);
      slot.handlers[type] = typeof value === 'function' ? value : null;
      if (!slot.handlers[type]) {
        if (slot.handlerListeners[type]) _eventTargetRemove(this,type,slot.handlerListeners[type],false);
        delete slot.handlerListeners[type];
      } else if (!slot.handlerListeners[type]) {
        const callback = event => {if (slot.handlers[type]?.call(this,event) === false) event.preventDefault();};
        slot.handlerListeners[type] = callback;
        _eventTargetAdd(this,type,callback,false);
      }
    },
    configurable:true,enumerable:true,
  });
}
for (const klass of [IDBRequest,IDBTransaction,IDBDatabase]) {
  Object.setPrototypeOf(klass.prototype,EventTarget.prototype);
  Object.setPrototypeOf(klass,EventTarget);
}
for (const klass of [IDBFactory,IDBRequest,IDBOpenDBRequest,IDBTransaction,IDBDatabase,
  IDBObjectStore,IDBIndex,IDBCursor,IDBCursorWithValue,IDBKeyRange,IDBVersionChangeEvent]) {
  Object.defineProperty(klass.prototype,Symbol.toStringTag,{value:klass.name,configurable:true});
  _markNative(klass);
  for (const name of Object.getOwnPropertyNames(klass.prototype)) {
    if (name === 'constructor') continue;
    const descriptor = Object.getOwnPropertyDescriptor(klass.prototype,name);
    _markNative(descriptor.value); _markNative(descriptor.get); _markNative(descriptor.set);
    Object.defineProperty(klass.prototype,name,{...descriptor,enumerable:true});
  }
  Object.defineProperty(globalThis,klass.name,{value:klass,writable:true,configurable:true,enumerable:false});
}
const _idbFactory = new IDBFactory(_idbToken);
Object.defineProperty(globalThis,'indexedDB',{get(){
  if (!__obscuraCore.ops.op_storage_access()) throw _idbError('SecurityError');
  return _idbFactory;
},configurable:true,enumerable:true});
Object.defineProperty(globalThis,'__obscura_poll_indexeddb',{
  value:notices => _idbTask(() => _idbPollNotices(notices)),configurable:true,
});
