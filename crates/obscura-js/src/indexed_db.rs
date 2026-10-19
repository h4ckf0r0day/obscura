//! Context-owned IndexedDB records. Transactions stage deltas, not copies of a database.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use serde_json::{json, Value};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_HANDLES: usize = 1024;
const RECORD_OVERHEAD: usize = 96;
type DatabaseKey = (String, String);

#[derive(Default)]
struct Database {
    version: u64,
    schema: Vec<Value>,
    records: HashMap<String, HashMap<String, String>>,
    connections: HashMap<u64, u64>,
    queue: VecDeque<u64>,
    openings: VecDeque<(u64, u64)>,
    schema_bytes: usize,
}

struct Transaction {
    owner: u64,
    database: DatabaseKey,
    mode: String,
    scope: HashSet<String>,
    cleared: HashSet<String>,
    writes: HashMap<(String, String), Option<String>>,
    pending_bytes: usize,
}

#[derive(Default)]
struct Inner {
    next: u64,
    bytes: usize,
    pending_bytes: usize,
    databases: HashMap<DatabaseKey, Database>,
    transactions: HashMap<u64, Transaction>,
    notices: HashMap<u64, Vec<Value>>,
}

#[derive(Default)]
pub struct IndexedDbStorage {
    inner: Mutex<Inner>,
    pending_notices: std::sync::atomic::AtomicBool,
}

impl IndexedDbStorage {
    pub fn has_pending_notifications(&self) -> bool {
        self.pending_notices.load(std::sync::atomic::Ordering::Acquire)
    }
}

pub(crate) struct IndexedDbOwner {
    pub(crate) storage: Arc<IndexedDbStorage>,
    id: u64,
}

impl IndexedDbOwner {
    pub(crate) fn new(storage: Arc<IndexedDbStorage>) -> Self {
        let id = {
            let mut inner = storage.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.next += 1;
            inner.next
        };
        Self { storage, id }
    }

    pub(crate) fn command(&self, origin: &str, action: &str, data: Value) -> Value {
        let mut inner = self.storage.inner.lock().unwrap_or_else(|e| e.into_inner());
        let result = inner.command(self.id, origin, action, data);
        self.storage.pending_notices.store(!inner.notices.is_empty(), std::sync::atomic::Ordering::Release);
        match result {
            Ok(value) => value,
            Err(name) => json!({"error":name}),
        }
    }

    pub(crate) fn take_notices(&self) -> Vec<Value> {
        let mut inner = self.storage.inner.lock().unwrap_or_else(|e| e.into_inner());
        let notices = inner.notices.remove(&self.id).unwrap_or_default();
        self.storage.pending_notices.store(!inner.notices.is_empty(), std::sync::atomic::Ordering::Release);
        notices
    }
}

impl Drop for IndexedDbOwner {
    fn drop(&mut self) {
        let mut inner = self.storage.inner.lock().unwrap_or_else(|e| e.into_inner());
        let ids: HashSet<u64> = inner.transactions.iter()
            .filter_map(|(id, tx)| (tx.owner == self.id).then_some(*id)).collect();
        let released = inner.transactions.values().filter(|tx| tx.owner == self.id).map(|tx| tx.pending_bytes).sum::<usize>();
        inner.pending_bytes = inner.pending_bytes.saturating_sub(released);
        inner.transactions.retain(|id, _| !ids.contains(id));
        for db in inner.databases.values_mut() {
            db.connections.retain(|_, owner| *owner != self.id);
            db.queue.retain(|id| !ids.contains(id));
            db.openings.retain(|(_, owner)| *owner != self.id);
        }
        inner.notices.remove(&self.id);
        self.storage.pending_notices.store(!inner.notices.is_empty(), std::sync::atomic::Ordering::Release);
    }
}

fn text<'a>(data: &'a Value, key: &str) -> Result<&'a str, &'static str> {
    data.get(key).and_then(Value::as_str).ok_or("DataError")
}

impl Inner {
    fn command(&mut self, owner: u64, origin: &str, action: &str, data: Value) -> Result<Value, &'static str> {
        if origin == "null" { return Err("SecurityError"); }
        if action == "databases" {
            return Ok(Value::Array(self.databases.iter().filter_map(|((o, name), db)|
                (o == origin && db.version > 0).then(|| json!({"name":name,"version":db.version}))).collect()));
        }
        if matches!(action, "info" | "connect" | "close" | "notify" | "delete" | "begin"
            | "reserve_open" | "open_ready" | "release_open") {
            let name = text(&data, "name")?;
            if name.len() > 16 * 1024 || self.databases.len() >= MAX_HANDLES
                && !self.databases.contains_key(&(origin.into(), name.into())) { return Err("QuotaExceededError"); }
            let key = (origin.to_string(), name.to_string());
            if action == "delete" {
                let Some(db) = self.databases.get(&key) else { return Ok(json!(true)); };
                if !db.connections.is_empty() || !db.queue.is_empty() { return Ok(json!(false)); }
                if let Some(db) = self.databases.get_mut(&key) {
                    self.bytes = self.bytes.saturating_sub(db.schema_bytes);
                    for store in db.records.values() {
                        for (k, v) in store { self.bytes = self.bytes.saturating_sub(k.len() + v.len() + RECORD_OVERHEAD); }
                    }
                    db.records.clear(); db.schema.clear(); db.schema_bytes = 0; db.version = 0;
                }
                return Ok(json!(true));
            }
            let db = self.databases.entry(key.clone()).or_default();
            match action {
                "reserve_open" => {
                    if db.openings.len() >= MAX_HANDLES { return Err("QuotaExceededError"); }
                    self.next += 1;
                    db.openings.push_back((self.next, owner));
                    return Ok(json!(self.next));
                }
                "open_ready" | "release_open" => {
                    let id = data["opening"].as_u64().ok_or("DataError")?;
                    if action == "open_ready" { return Ok(json!(db.openings.front() == Some(&(id, owner)))); }
                    db.openings.retain(|entry| *entry != (id, owner));
                    if db.openings.is_empty() && db.version == 0 && db.connections.is_empty() && db.queue.is_empty() {
                        self.databases.remove(&key);
                    }
                    return Ok(Value::Null);
                }
                "info" => return Ok(json!({"version":db.version,"schema":db.schema,"connections":db.connections.len()})),
                "connect" => {
                    if db.connections.len() >= MAX_HANDLES { return Err("QuotaExceededError"); }
                    self.next += 1;
                    db.connections.insert(self.next, owner);
                    return Ok(json!(self.next));
                }
                "close" => {
                    let id = data["connection"].as_u64().ok_or("DataError")?;
                    if db.connections.get(&id) == Some(&owner) { db.connections.remove(&id); }
                    return Ok(Value::Null);
                }
                "notify" => {
                    for (&id, &other) in &db.connections {
                        let notice = json!({"connection":id,"version":data["version"]});
                        let pending = self.notices.entry(other).or_default();
                        if !pending.contains(&notice) && pending.len() < MAX_HANDLES { pending.push(notice); }
                    }
                    return Ok(Value::Null);
                }
                "begin" => {
                    if self.transactions.len() >= MAX_HANDLES { return Err("QuotaExceededError"); }
                    let mode = text(&data, "mode")?.to_string();
                    if !matches!(mode.as_str(), "readonly" | "readwrite" | "versionchange") { return Err("TypeError"); }
                    if mode != "versionchange" {
                        let connection = data["connection"].as_u64().ok_or("InvalidStateError")?;
                        if db.connections.get(&connection) != Some(&owner) { return Err("InvalidStateError"); }
                    }
                    let scope = data["stores"].as_array().ok_or("DataError")?.iter()
                        .map(|s| s.as_str().map(str::to_string).ok_or("DataError"))
                        .collect::<Result<HashSet<_>, _>>()?;
                    self.next += 1;
                    let id = self.next;
                    db.queue.push_back(id);
                    self.transactions.insert(id, Transaction { owner, database:key, mode, scope,
                        cleared:HashSet::new(), writes:HashMap::new(), pending_bytes:0 });
                    return Ok(json!(id));
                }
                _ => unreachable!(),
            }
        }
        let id = data["id"].as_u64().ok_or("TransactionInactiveError")?;
        let tx = self.transactions.get(&id).ok_or("TransactionInactiveError")?;
        if tx.owner != owner || tx.database.0 != origin { return Err("InvalidStateError"); }
        let db = self.databases.get(&tx.database).ok_or("InvalidStateError")?;
        if action == "abort" {
            let key = tx.database.clone();
            self.pending_bytes = self.pending_bytes.saturating_sub(tx.pending_bytes);
            self.transactions.remove(&id);
            if let Some(db) = self.databases.get_mut(&key) { db.queue.retain(|queued| *queued != id); }
            return Ok(Value::Null);
        }
        if action == "ready" {
            return Ok(if db.queue.front() == Some(&id) {
                json!({"version":db.version,"schema":db.schema})
            } else { Value::Null });
        }
        if db.queue.front() != Some(&id) { return Err("TransactionInactiveError"); }
        if action == "commit" {
            let schema = data["schema"].as_array().ok_or("DataError")?.clone();
            let version = if tx.mode == "versionchange" { Some(data["version"].as_u64().filter(|v| *v > 0).ok_or("DataError")?) }
                else { None };
            if schema.len() > MAX_HANDLES { return Err("QuotaExceededError"); }
            let names: HashSet<String> = schema.iter()
                .map(|s| text(s, "name").map(str::to_string)).collect::<Result<_, _>>()?;
            let schema_bytes = serde_json::to_vec(&schema).map_err(|_| "DataError")?.len();
            let mut bytes = self.bytes.saturating_sub(db.schema_bytes).saturating_add(schema_bytes);
            for (name, records) in &db.records {
                if !names.contains(name) || tx.cleared.contains(name) {
                    for (k, v) in records { bytes = bytes.saturating_sub(k.len() + v.len() + RECORD_OVERHEAD); }
                }
            }
            for ((store, key), value) in &tx.writes {
                if !names.contains(store) { continue; }
                if !tx.cleared.contains(store) {
                    if let Some(old) = db.records.get(store).and_then(|s| s.get(key)) {
                        bytes = bytes.saturating_sub(key.len() + old.len() + RECORD_OVERHEAD);
                    }
                }
                if let Some(value) = value { bytes = bytes.saturating_add(key.len() + value.len() + RECORD_OVERHEAD); }
            }
            if bytes > MAX_BYTES { return Err("QuotaExceededError"); }
            let tx = self.transactions.remove(&id).ok_or("TransactionInactiveError")?;
            self.pending_bytes = self.pending_bytes.saturating_sub(tx.pending_bytes);
            let db = self.databases.get_mut(&tx.database).ok_or("InvalidStateError")?;
            db.records.retain(|name, _| names.contains(name) && !tx.cleared.contains(name));
            for ((store, key), value) in tx.writes {
                if !names.contains(&store) { continue; }
                let records = db.records.entry(store).or_default();
                if let Some(value) = value { records.insert(key, value); } else { records.remove(&key); }
            }
            db.schema = schema;
            db.schema_bytes = schema_bytes;
            if let Some(version) = version { db.version = version; }
            db.queue.pop_front();
            self.bytes = bytes;
            return Ok(Value::Null);
        }
        let store = text(&data, "store")?.to_string();
        if tx.mode != "versionchange" && !tx.scope.contains(&store) { return Err("NotFoundError"); }
        match action {
            "get" => {
                let key = text(&data, "key")?;
                let value = tx.writes.get(&(store.clone(), key.to_string())).cloned()
                    .unwrap_or_else(|| if tx.cleared.contains(&store) { None }
                        else { db.records.get(&store).and_then(|s| s.get(key)).cloned() });
                Ok(json!(value))
            }
            "entries" => {
                let mut records = if tx.cleared.contains(&store) { HashMap::new() }
                    else { db.records.get(&store).cloned().unwrap_or_default() };
                for ((name, key), value) in &tx.writes {
                    if name != &store { continue; }
                    if let Some(value) = value { records.insert(key.clone(), value.clone()); }
                    else { records.remove(key); }
                }
                Ok(json!(records.into_iter().collect::<Vec<_>>()))
            }
            "put" | "remove" | "clear" => {
                if tx.mode == "readonly" { return Err("ReadOnlyError"); }
                let tx = self.transactions.get_mut(&id).ok_or("TransactionInactiveError")?;
                if action == "clear" {
                    tx.cleared.insert(store.clone());
                    let released: usize = tx.writes.iter().filter(|((name, _), _)| name == &store)
                        .map(|((name, key), value)| name.len() + key.len() + value.as_ref().map_or(0, String::len) + RECORD_OVERHEAD).sum();
                    tx.pending_bytes = tx.pending_bytes.saturating_sub(released);
                    self.pending_bytes = self.pending_bytes.saturating_sub(released);
                    tx.writes.retain(|(name, _), _| name != &store);
                } else {
                    let key = text(&data, "key")?.to_string();
                    let value = if action == "put" { Some(text(&data, "value")?.to_string()) } else { None };
                    if key.len() > 64 * 1024 || value.as_ref().is_some_and(|v| v.len() > 16 * 1024 * 1024) {
                        return Err("QuotaExceededError");
                    }
                    let old = tx.writes.get(&(store.clone(), key.clone())).map_or(0, |old|
                        store.len() + key.len() + old.as_ref().map_or(0, String::len) + RECORD_OVERHEAD);
                    let added = store.len() + key.len() + value.as_ref().map_or(0, String::len) + RECORD_OVERHEAD;
                    let total = self.pending_bytes.saturating_sub(old).saturating_add(added);
                    if self.bytes.saturating_add(total) > MAX_BYTES { return Err("QuotaExceededError"); }
                    let pending = tx.pending_bytes.saturating_sub(old).saturating_add(added);
                    self.pending_bytes = total;
                    tx.pending_bytes = pending;
                    tx.writes.insert((store, key), value);
                }
                Ok(Value::Null)
            }
            _ => Err("NotSupportedError"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn begin(owner: &IndexedDbOwner, name: &str, mode: &str) -> u64 {
        let connection = owner.command("https://idb.test", "connect", json!({"name":name}));
        let id = owner.command("https://idb.test", "begin",
            json!({"name":name,"mode":mode,"stores":["items"],"connection":connection})).as_u64().unwrap();
        assert!(!owner.command("https://idb.test", "ready", json!({"id":id})).is_null());
        id
    }
    #[test]
    fn pending_writes_share_one_context_memory_budget() {
        let store = Arc::new(IndexedDbStorage::default());
        let owner = IndexedDbOwner::new(store);
        let first = begin(&owner,"first","versionchange");
        let second = begin(&owner,"second","versionchange");
        let value = "x".repeat(8 * 1024 * 1024);
        let mut refused = false;
        for i in 0..9 {
            let result = owner.command("https://idb.test","put",
                json!({"id":if i < 4 {first} else {second},"store":"items","key":i.to_string(),"value":value}));
            refused |= result["error"] == "QuotaExceededError";
        }
        assert!(refused,"parallel databases must not multiply the context's pending-write budget");
    }
    #[test]
    fn malformed_commit_cannot_publish_writes_or_consume_the_transaction() {
        let owner = IndexedDbOwner::new(Arc::new(IndexedDbStorage::default()));
        let id = begin(&owner,"atomic","versionchange");
        owner.command("https://idb.test","put",json!({"id":id,"store":"items","key":"key","value":"value"}));
        assert_eq!(owner.command("https://idb.test","commit",json!({"id":id,"schema":[{"name":"items"}]}))["error"],"DataError");
        assert_eq!(owner.command("https://idb.test","info",json!({"name":"atomic"}))["schema"],json!([]));
        assert_eq!(owner.command("https://idb.test","get",json!({"id":id,"store":"items","key":"key"})),json!("value"));
    }
    #[test]
    fn dropping_a_document_aborts_its_writes_and_unblocks_waiting_transactions() {
        let store = Arc::new(IndexedDbStorage::default());
        let owner = IndexedDbOwner::new(store.clone());
        let id = begin(&owner,"dropped","versionchange");
        owner.command("https://idb.test","put",json!({"id":id,"store":"items","key":"key","value":"discard"}));
        let next = IndexedDbOwner::new(store);
        let waiting = next.command("https://idb.test","begin",json!({"name":"dropped","mode":"versionchange","stores":[]})).as_u64().unwrap();
        assert!(next.command("https://idb.test","ready",json!({"id":waiting})).is_null());
        drop(owner);
        assert!(!next.command("https://idb.test","ready",json!({"id":waiting})).is_null());
        assert!(next.command("https://idb.test","get",json!({"id":waiting,"store":"items","key":"key"})).is_null());
    }
    #[test]
    fn opening_requests_are_ordered_across_documents_and_released_on_teardown() {
        let storage = Arc::new(IndexedDbStorage::default());
        let first = IndexedDbOwner::new(storage.clone());
        let second = IndexedDbOwner::new(storage);
        let a = first.command("https://idb.test","reserve_open",json!({"name":"shared"}));
        let b = second.command("https://idb.test","reserve_open",json!({"name":"shared"}));
        assert!(a.is_u64() && b.is_u64());
        assert_eq!(first.command("https://idb.test","open_ready",json!({"name":"shared","opening":a})),json!(true));
        assert_eq!(second.command("https://idb.test","open_ready",json!({"name":"shared","opening":b})),json!(false));
        drop(first);
        assert_eq!(second.command("https://idb.test","open_ready",json!({"name":"shared","opening":b})),json!(true));
        assert_eq!(second.command("https://idb.test","delete",json!({"name":"shared"})),json!(true));
        assert_eq!(second.command("https://idb.test","open_ready",json!({"name":"shared","opening":b})),json!(true));
    }
}
