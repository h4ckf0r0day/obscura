//! Origin-keyed backing store for Web Storage (`localStorage` /
//! `sessionStorage`) and IndexedDB snapshots.
//!
//! One instance per browser context backs `localStorage` and IndexedDB, so
//! every page and frame of the same origin sees the same data, and the data
//! outlives a navigation. A separate instance per page backs
//! `sessionStorage`. Only the context instance is written to disk (with
//! `--storage-dir`), mirroring how cookies persist.

use std::collections::{BTreeMap, HashMap};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// Chromium's per-origin Web Storage quota: 10 MiB, counted as UTF-16 bytes of
/// every key plus value.
pub const WEB_STORAGE_QUOTA_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct WebStorage {
    // BTreeMap: Chromium's storage area is an ordered map, so key(i) follows
    // key order rather than insertion order.
    areas: RwLock<HashMap<String, Area>>,
    // origin -> database name -> opaque snapshot owned by the JS IndexedDB shim.
    idb: RwLock<HashMap<String, BTreeMap<String, String>>>,
}

/// One origin's entries plus their running UTF-16 byte size, so the quota
/// check on a write is O(1) instead of summing the whole area.
#[derive(Debug, Default)]
struct Area {
    entries: BTreeMap<String, String>,
    used: usize,
}

impl Area {
    fn insert(&mut self, key: String, value: String) {
        let added = utf16_len(&key) + utf16_len(&value);
        if let Some(old) = self.entries.insert(key.clone(), value) {
            self.used -= utf16_len(&key) + utf16_len(&old);
        }
        self.used += added;
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OnDisk {
    #[serde(default, rename = "localStorage")]
    local_storage: HashMap<String, BTreeMap<String, String>>,
    #[serde(default, rename = "indexedDB")]
    indexed_db: HashMap<String, BTreeMap<String, String>>,
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count() * 2
}

/// Origins that must never reach disk: opaque ("null") documents have no
/// stable identity to key persisted data on.
fn persistable(origin: &str) -> bool {
    origin != "null" && !origin.is_empty()
}

impl WebStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, origin: &str, key: &str) -> Option<String> {
        let areas = self.areas.read().ok()?;
        areas.get(origin)?.entries.get(key).cloned()
    }

    /// Store `value` under `key`. Returns false, leaving the area unchanged,
    /// when the write would exceed the per-origin quota.
    pub fn set(&self, origin: &str, key: &str, value: &str) -> bool {
        let Ok(mut areas) = self.areas.write() else {
            return false;
        };
        let area = areas.entry(origin.to_string()).or_default();
        let old = area.entries.get(key).map(|v| utf16_len(key) + utf16_len(v)).unwrap_or(0);
        if area.used - old + utf16_len(key) + utf16_len(value) > WEB_STORAGE_QUOTA_BYTES {
            return false;
        }
        area.insert(key.to_string(), value.to_string());
        true
    }

    pub fn remove(&self, origin: &str, key: &str) {
        if let Ok(mut areas) = self.areas.write() {
            if let Some(area) = areas.get_mut(origin) {
                if let Some(old) = area.entries.remove(key) {
                    area.used -= utf16_len(key) + utf16_len(&old);
                }
            }
        }
    }

    pub fn clear(&self, origin: &str) {
        if let Ok(mut areas) = self.areas.write() {
            areas.remove(origin);
        }
    }

    pub fn keys(&self, origin: &str) -> Vec<String> {
        self.areas
            .read()
            .ok()
            .and_then(|areas| areas.get(origin).map(|a| a.entries.keys().cloned().collect()))
            .unwrap_or_default()
    }

    pub fn len(&self, origin: &str) -> usize {
        self.areas
            .read()
            .ok()
            .and_then(|areas| areas.get(origin).map(|a| a.entries.len()))
            .unwrap_or(0)
    }

    /// Every (key, value) pair for `origin`, in key order.
    pub fn entries(&self, origin: &str) -> Vec<(String, String)> {
        self.areas
            .read()
            .ok()
            .and_then(|areas| {
                areas
                    .get(origin)
                    .map(|a| a.entries.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            })
            .unwrap_or_default()
    }

    pub fn idb_get(&self, origin: &str, name: &str) -> Option<String> {
        let idb = self.idb.read().ok()?;
        idb.get(origin)?.get(name).cloned()
    }

    pub fn idb_put(&self, origin: &str, name: &str, snapshot: &str) {
        if let Ok(mut idb) = self.idb.write() {
            idb.entry(origin.to_string())
                .or_default()
                .insert(name.to_string(), snapshot.to_string());
        }
    }

    pub fn idb_delete(&self, origin: &str, name: &str) {
        if let Ok(mut idb) = self.idb.write() {
            if let Some(dbs) = idb.get_mut(origin) {
                dbs.remove(name);
            }
        }
    }

    pub fn idb_names(&self, origin: &str) -> Vec<String> {
        self.idb
            .read()
            .ok()
            .and_then(|idb| idb.get(origin).map(|d| d.keys().cloned().collect()))
            .unwrap_or_default()
    }

    pub fn save_to_file(&self, path: &std::path::Path) -> std::io::Result<()> {
        let keep = |m: &HashMap<String, BTreeMap<String, String>>| {
            m.iter()
                .filter(|(o, a)| persistable(o) && !a.is_empty())
                .map(|(o, a)| (o.clone(), a.clone()))
                .collect::<HashMap<_, _>>()
        };
        let local_storage = self
            .areas
            .read()
            .map(|areas| {
                areas
                    .iter()
                    .filter(|(o, a)| persistable(o) && !a.entries.is_empty())
                    .map(|(o, a)| (o.clone(), a.entries.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let on_disk = OnDisk {
            local_storage,
            indexed_db: self.idb.read().map(|a| keep(&a)).unwrap_or_default(),
        };
        let json = serde_json::to_string(&on_disk)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Write-then-rename so a crash mid-write never truncates saved state.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// Merge the file's contents into this store. Returns the number of
    /// origins loaded.
    pub fn load_from_file(&self, path: &std::path::Path) -> std::io::Result<usize> {
        let raw = std::fs::read_to_string(path)?;
        let on_disk: OnDisk = serde_json::from_str(&raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let n = on_disk.local_storage.len() + on_disk.indexed_db.len();
        if let Ok(mut areas) = self.areas.write() {
            for (origin, area) in on_disk.local_storage {
                if persistable(&origin) {
                    let target = areas.entry(origin).or_default();
                    for (key, value) in area {
                        target.insert(key, value);
                    }
                }
            }
        }
        if let Ok(mut idb) = self.idb.write() {
            for (origin, dbs) in on_disk.indexed_db {
                if persistable(&origin) {
                    idb.entry(origin).or_default().extend(dbs);
                }
            }
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_remove_clear_are_origin_scoped() {
        let s = WebStorage::new();
        assert!(s.set("https://a.test", "k", "1"));
        assert!(s.set("https://b.test", "k", "2"));
        assert_eq!(s.get("https://a.test", "k").as_deref(), Some("1"));
        assert_eq!(s.get("https://b.test", "k").as_deref(), Some("2"));
        s.remove("https://a.test", "k");
        assert_eq!(s.get("https://a.test", "k"), None);
        s.clear("https://b.test");
        assert_eq!(s.len("https://b.test"), 0);
    }

    #[test]
    fn keys_are_ordered() {
        let s = WebStorage::new();
        s.set("o", "b", "");
        s.set("o", "a", "");
        s.set("o", "c", "");
        assert_eq!(s.keys("o"), vec!["a", "b", "c"]);
    }

    #[test]
    fn quota_rejects_oversized_write_and_keeps_old_value() {
        let s = WebStorage::new();
        assert!(s.set("o", "k", "small"));
        let big = "x".repeat(WEB_STORAGE_QUOTA_BYTES / 2 + 1);
        assert!(!s.set("o", "k", &big));
        assert_eq!(s.get("o", "k").as_deref(), Some("small"));
        // Replacing a value counts only the new size, not old + new.
        let fits = "x".repeat(WEB_STORAGE_QUOTA_BYTES / 2 - 8);
        assert!(s.set("o", "k", &fits));
    }

    #[test]
    fn quota_tracks_removals_and_many_small_writes() {
        let s = WebStorage::new();
        let half = "x".repeat(WEB_STORAGE_QUOTA_BYTES / 4 - 8);
        assert!(s.set("o", "a", &half));
        assert!(s.set("o", "b", &half));
        assert!(!s.set("o", "c", &half), "a third half-quota value must not fit");
        s.remove("o", "a");
        assert!(s.set("o", "c", &half), "removing a value frees its bytes");
        s.clear("o");
        for i in 0..20_000 {
            assert!(s.set("o", &format!("k{i}"), "value"));
        }
        assert_eq!(s.len("o"), 20_000);
    }

    #[test]
    fn round_trips_through_disk_and_skips_opaque_origins() {
        let dir = std::env::temp_dir().join(format!("obscura-ws-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("web_storage.json");
        let s = WebStorage::new();
        s.set("https://a.test", "token", "abc");
        s.set("null", "x", "y");
        s.idb_put("https://a.test", "db1", "{\"v\":1}");
        s.save_to_file(&path).unwrap();

        let loaded = WebStorage::new();
        loaded.load_from_file(&path).unwrap();
        assert_eq!(loaded.get("https://a.test", "token").as_deref(), Some("abc"));
        assert_eq!(loaded.get("null", "x"), None);
        assert_eq!(loaded.idb_get("https://a.test", "db1").as_deref(), Some("{\"v\":1}"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
