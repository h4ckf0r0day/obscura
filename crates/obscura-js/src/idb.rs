use std::path::{Path, PathBuf};
use redb::{Database, ReadableTable, TableDefinition};

const IDB_RECORDS: TableDefinition<&str, &str> = TableDefinition::new("idb_records");

fn sanitize_for_path(s: &str) -> String {
    let sanitized: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn get_db_path(storage_dir: &Path, origin: &str, db_name: &str) -> PathBuf {
    let origin_clean = sanitize_for_path(origin);
    let db_clean = sanitize_for_path(db_name);
    let dir = storage_dir.join("indexeddb").join(origin_clean);
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{}.redb", db_clean))
}

fn make_composite_key(store: &str, key: &str) -> String {
    format!("{}\0{}", store, key)
}

pub fn idb_get(db_path: &Path, store: &str, key: &str) -> Option<String> {
    let db = Database::open(db_path).ok()?;
    let read_txn = db.begin_read().ok()?;
    let table = read_txn.open_table(IDB_RECORDS).ok()?;
    let comp_key = make_composite_key(store, key);
    let val = table.get(comp_key.as_str()).ok()??;
    Some(val.value().to_string())
}

pub fn idb_put(db_path: &Path, store: &str, key: &str, val_json: &str) -> Result<(), String> {
    let db = Database::create(db_path).or_else(|_| Database::open(db_path)).map_err(|e| e.to_string())?;
    let write_txn = db.begin_write().map_err(|e| e.to_string())?;
    {
        let mut table = write_txn.open_table(IDB_RECORDS).map_err(|e| e.to_string())?;
        let comp_key = make_composite_key(store, key);
        table.insert(comp_key.as_str(), val_json).map_err(|e| e.to_string())?;
    }
    write_txn.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn idb_delete(db_path: &Path, store: &str, key: &str) -> Result<(), String> {
    if !db_path.exists() {
        return Ok(());
    }
    let db = Database::open(db_path).map_err(|e| e.to_string())?;
    let write_txn = db.begin_write().map_err(|e| e.to_string())?;
    {
        let mut table = write_txn.open_table(IDB_RECORDS).map_err(|e| e.to_string())?;
        let comp_key = make_composite_key(store, key);
        let _ = table.remove(comp_key.as_str());
    }
    write_txn.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn idb_clear(db_path: &Path, store: &str) -> Result<(), String> {
    if !db_path.exists() {
        return Ok(());
    }
    let db = Database::open(db_path).map_err(|e| e.to_string())?;
    let write_txn = db.begin_write().map_err(|e| e.to_string())?;
    {
        let mut table = write_txn.open_table(IDB_RECORDS).map_err(|e| e.to_string())?;
        let prefix = format!("{}\0", store);
        let end_prefix = format!("{}\x01", store);
        
        let keys_to_remove: Vec<String> = table
            .range(prefix.as_str()..end_prefix.as_str())
            .map_err(|e| e.to_string())?
            .filter_map(|res| res.ok().map(|(k, _)| k.value().to_string()))
            .collect();

        for k in keys_to_remove {
            let _ = table.remove(k.as_str());
        }
    }
    write_txn.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn idb_get_all(db_path: &Path, store: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(db) = Database::open(db_path) else { return out; };
    let Ok(read_txn) = db.begin_read() else { return out; };
    let Ok(table) = read_txn.open_table(IDB_RECORDS) else { return out; };
    let prefix = format!("{}\0", store);
    let end_prefix = format!("{}\x01", store);

    if let Ok(iter) = table.range(prefix.as_str()..end_prefix.as_str()) {
        for item in iter.flatten() {
            out.push(item.1.value().to_string());
        }
    }
    out
}

pub fn idb_get_all_keys(db_path: &Path, store: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(db) = Database::open(db_path) else { return out; };
    let Ok(read_txn) = db.begin_read() else { return out; };
    let Ok(table) = read_txn.open_table(IDB_RECORDS) else { return out; };
    let prefix = format!("{}\0", store);
    let end_prefix = format!("{}\x01", store);

    if let Ok(iter) = table.range(prefix.as_str()..end_prefix.as_str()) {
        for item in iter.flatten() {
            let raw_key = item.0.value();
            if let Some(user_key) = raw_key.strip_prefix(&prefix) {
                out.push(user_key.to_string());
            }
        }
    }
    out
}

pub fn idb_count(db_path: &Path, store: &str) -> usize {
    let Ok(db) = Database::open(db_path) else { return 0; };
    let Ok(read_txn) = db.begin_read() else { return 0; };
    let Ok(table) = read_txn.open_table(IDB_RECORDS) else { return 0; };
    let prefix = format!("{}\0", store);
    let end_prefix = format!("{}\x01", store);

    if let Ok(iter) = table.range(prefix.as_str()..end_prefix.as_str()) {
        iter.count()
    } else {
        0
    }
}

const CACHE_RECORDS: TableDefinition<&str, &str> = TableDefinition::new("cache_records");

pub fn get_cache_db_path(storage_dir: &Path, origin: &str) -> PathBuf {
    let origin_clean = sanitize_for_path(origin);
    let dir = storage_dir.join("cache_storage").join(origin_clean);
    let _ = std::fs::create_dir_all(&dir);
    dir.join("caches.redb")
}

pub fn cache_get(db_path: &Path, cache_name: &str, url: &str) -> Option<String> {
    let db = Database::open(db_path).ok()?;
    let read_txn = db.begin_read().ok()?;
    let table = read_txn.open_table(CACHE_RECORDS).ok()?;
    let comp_key = make_composite_key(cache_name, url);
    let val = table.get(comp_key.as_str()).ok()??;
    Some(val.value().to_string())
}

pub fn cache_put(db_path: &Path, cache_name: &str, url: &str, val_json: &str) -> Result<(), String> {
    let db = Database::create(db_path).or_else(|_| Database::open(db_path)).map_err(|e| e.to_string())?;
    let write_txn = db.begin_write().map_err(|e| e.to_string())?;
    {
        let mut table = write_txn.open_table(CACHE_RECORDS).map_err(|e| e.to_string())?;
        let comp_key = make_composite_key(cache_name, url);
        table.insert(comp_key.as_str(), val_json).map_err(|e| e.to_string())?;
    }
    write_txn.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn cache_delete(db_path: &Path, cache_name: &str, url: &str) -> Result<bool, String> {
    if !db_path.exists() {
        return Ok(false);
    }
    let db = Database::open(db_path).map_err(|e| e.to_string())?;
    let write_txn = db.begin_write().map_err(|e| e.to_string())?;
    let removed = {
        let mut table = write_txn.open_table(CACHE_RECORDS).map_err(|e| e.to_string())?;
        let comp_key = make_composite_key(cache_name, url);
        let res = table.remove(comp_key.as_str()).map_err(|e| e.to_string())?.is_some();
        res
    };
    write_txn.commit().map_err(|e| e.to_string())?;
    Ok(removed)
}

pub fn cache_keys(db_path: &Path, cache_name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(db) = Database::open(db_path) else { return out; };
    let Ok(read_txn) = db.begin_read() else { return out; };
    let Ok(table) = read_txn.open_table(CACHE_RECORDS) else { return out; };
    let prefix = format!("{}\0", cache_name);
    let end_prefix = format!("{}\x01", cache_name);

    if let Ok(iter) = table.range(prefix.as_str()..end_prefix.as_str()) {
        for item in iter.flatten() {
            let raw_key = item.0.value();
            if let Some(user_key) = raw_key.strip_prefix(&prefix) {
                out.push(user_key.to_string());
            }
        }
    }
    out
}
