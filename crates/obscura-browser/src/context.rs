use std::path::PathBuf;
use std::sync::Arc;

use obscura_js::ops::{IndexedDbStorage, OriginStorage};
use obscura_net::{CookieJar, ObscuraHttpClient, RobotsCache};

pub struct BrowserContext {
    pub id: String,
    pub cookie_jar: Arc<CookieJar>,
    /// `localStorage` backing store, keyed by origin. Owned by the context
    /// rather than by a V8 realm, so an entry written in one document is still
    /// there in the next (issue #678).
    pub local_storage: Arc<OriginStorage>,
    /// IndexedDB records and schema, keyed by origin. Owned by the context, so
    /// a page reload or a second tab sees the same databases.
    pub indexed_db: Arc<IndexedDbStorage>,
    pub http_client: Arc<ObscuraHttpClient>,
    pub user_agent: String,
    pub platform: String,
    pub ua_platform: String,
    pub ua_platform_version: String,
    pub proxy_url: Option<String>,
    pub robots_cache: Arc<RobotsCache>,
    pub obey_robots: bool,
    pub stealth: bool,
    /// When true, CDP-driven navigation to file:// URLs is permitted.
    /// Default is false: a remote CDP client cannot point the browser
    /// at /etc/shadow even if Obscura is running as a privileged user.
    /// Flip on via `obscura serve --allow-file-access` for legitimate
    /// local-HTML testing workflows. Enforced by `Page` navigation itself,
    /// so every CDP and MCP route is covered; the CLI's own `obscura fetch
    /// file://...` opts its local context in. A page can never drive
    /// itself from a web origin into file:// regardless of this flag.
    pub allow_file_access: bool,
    pub storage_dir: Option<PathBuf>,
    /// When true, the http client allows fetching localhost / RFC1918 /
    /// link-local addresses. Set via `--allow-private-network` (issue #33).
    /// Independent of `allow_file_access` because they cover different threat
    /// models: file:// is a local file-system read, while private-network is
    /// the broader SSRF gate from issue #4.
    pub allow_private_network: bool,
}

/// One origin's persisted `localStorage`. The origin is inside the file, so the
/// file name only has to be unique and stable, not reversible.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredOriginStorage {
    origin: String,
    /// `[key, value]` pairs in insertion order, the order `Storage.key(i)` uses.
    entries: Vec<(String, String)>,
}

/// Filename-safe stem for an origin: a sanitized prefix for readability plus the
/// full FNV-1a digest of the origin, so `https://a-b.example` and
/// `https://a_b.example` cannot share a file.
fn storage_file_stem(origin: &str) -> String {
    let prefix: String = origin
        .chars()
        .take(48)
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' })
        .collect();
    let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in origin.as_bytes() {
        digest ^= u64::from(*byte);
        digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{prefix}-{digest:016x}")
}

/// Write through a temporary file so an interrupted write cannot leave a
/// half-written profile behind.
fn write_file_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Restore `{storage_dir}/localStorage/*.json` into a fresh store.
fn load_web_storage(storage: &OriginStorage, dir: Option<&PathBuf>) {
    let Some(dir) = dir else { return };
    let root = dir.join("localStorage");
    let Ok(read_dir) = std::fs::read_dir(&root) else { return };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            tracing::warn!("Failed to read {}", path.display());
            continue;
        };
        match serde_json::from_str::<StoredOriginStorage>(&contents) {
            Ok(stored) => storage.load(&stored.origin, stored.entries),
            Err(e) => tracing::warn!("Ignoring unreadable {}: {}", path.display(), e),
        }
    }
}

impl BrowserContext {
    pub fn new(id: String) -> Self {
        Self::_new_inner(id, None, false, None, None, false)
    }

    /// Create a BrowserContext with an optional storage directory.
    /// When `storage_dir` is set, cookies are automatically loaded from
    /// `{storage_dir}/cookies.json` on creation.
    pub fn with_storage(
        id: String,
        storage_dir: Option<PathBuf>,
    ) -> Self {
        Self::_new_inner(id, None, false, None, storage_dir, false)
    }

    /// Create a BrowserContext with full options including storage_dir.
    pub fn with_storage_full(
        id: String,
        proxy_url: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
        storage_dir: Option<PathBuf>,
    ) -> Self {
        Self::_new_inner(id, proxy_url, stealth, user_agent, storage_dir, false)
    }

    /// Variant that also accepts the `allow_private_network` opt-in. All
    /// pre-existing constructors default it to `false`; callers that want the
    /// CLI's `--allow-private-network` (issue #33) behaviour go through here.
    pub fn with_storage_and_network(
        id: String,
        proxy_url: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
        storage_dir: Option<PathBuf>,
        allow_private_network: bool,
    ) -> Self {
        Self::_new_inner(id, proxy_url, stealth, user_agent, storage_dir, allow_private_network)
    }

    fn _new_inner(
        id: String,
        proxy_url: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
        storage_dir: Option<PathBuf>,
        allow_private_network: bool,
    ) -> Self {
        let cookie_jar = Arc::new(CookieJar::new());

        // Restore cookies from disk if storage_dir is configured
        if let Some(ref dir) = storage_dir {
            let cookie_path = dir.join("cookies.json");
            if cookie_path.exists() {
                match cookie_jar.load_from_file(&cookie_path) {
                    Ok(n) if n > 0 => {
                        tracing::info!("Loaded {} cookies from {}", n, cookie_path.display());
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("Failed to load cookies from {}: {}", cookie_path.display(), e);
                    }
                }
            }
        }

        let mut client = ObscuraHttpClient::with_full_options(
            cookie_jar.clone(),
            proxy_url.as_deref(),
            allow_private_network,
        );
        if stealth {
            client.block_trackers = true;
        }
        let profile = crate::profiles::select_profile();
        let resolved_ua = user_agent.unwrap_or_else(|| profile.user_agent.to_string());
        let platform = profile.platform.to_string();
        let ua_platform = profile.ua_platform.to_string();
        let ua_platform_version = profile.ua_platform_version.to_string();
        // Sync the http client's UA at construction so navigation requests pick it
        // up before any async setup runs. The lock has no other holders here, so
        // try_write always succeeds; we fall back silently if it ever fails.
        if let Ok(mut guard) = client.user_agent.try_write() {
            *guard = resolved_ua.clone();
        }
        let http_client = Arc::new(client);
        let local_storage = Arc::new(OriginStorage::default());
        if let Some(ref dir) = storage_dir {
            load_web_storage(&local_storage, Some(dir));
        }

        BrowserContext {
            id,
            cookie_jar,
            local_storage,
            indexed_db: Arc::new(IndexedDbStorage::default()),
            http_client,
            user_agent: resolved_ua,
            platform,
            ua_platform,
            ua_platform_version,
            proxy_url,
            robots_cache: Arc::new(RobotsCache::new()),
            obey_robots: false,
            stealth,
            allow_file_access: false,
            storage_dir,
            allow_private_network,
        }
    }

    pub fn with_options(id: String, proxy_url: Option<String>, stealth: bool) -> Self {
        Self::with_full_options(id, proxy_url, stealth, None)
    }

    pub fn with_full_options(
        id: String,
        proxy_url: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
    ) -> Self {
        Self::_new_inner(id, proxy_url, stealth, user_agent, None, false)
    }

    pub fn with_proxy(id: String, proxy_url: Option<String>) -> Self {
        Self::with_options(id, proxy_url, false)
    }

    /// Create a context with the same browser configuration but independent
    /// mutable network state. Persistent copies start with the template's
    /// current cookies; incognito copies start empty and never write to the
    /// template's storage directory.
    pub fn isolated_copy(&self, id: String, persistent: bool) -> Self {
        let cookie_jar = Arc::new(CookieJar::new());
        if persistent {
            cookie_jar.copy_from(&self.cookie_jar);
        }

        let mut client = ObscuraHttpClient::with_full_options(
            cookie_jar.clone(),
            self.proxy_url.as_deref(),
            self.allow_private_network,
        );
        if self.stealth {
            client.block_trackers = true;
        }
        if let Ok(mut guard) = client.user_agent.try_write() {
            *guard = self.user_agent.clone();
        }

        BrowserContext {
            id,
            cookie_jar,
            // localStorage belongs to the profile, so a persistent copy shares
            // the template's store instead of snapshotting it: two contexts on
            // one --storage-dir would otherwise overwrite each other's origins.
            local_storage: if persistent {
                self.local_storage.clone()
            } else {
                Arc::new(OriginStorage::default())
            },
            indexed_db: if persistent {
                self.indexed_db.clone()
            } else {
                Arc::new(IndexedDbStorage::default())
            },
            http_client: Arc::new(client),
            user_agent: self.user_agent.clone(),
            platform: self.platform.clone(),
            ua_platform: self.ua_platform.clone(),
            ua_platform_version: self.ua_platform_version.clone(),
            proxy_url: self.proxy_url.clone(),
            robots_cache: Arc::new(RobotsCache::new()),
            obey_robots: self.obey_robots,
            stealth: self.stealth,
            allow_file_access: self.allow_file_access,
            storage_dir: persistent.then(|| self.storage_dir.clone()).flatten(),
            allow_private_network: self.allow_private_network,
        }
    }

    /// Flush `localStorage` to `{storage_dir}/localStorage`. Only the origins
    /// that changed since the last successful write are touched, and an origin
    /// that lost its last entry has its file removed so a restart cannot
    /// resurrect it. The store is only marked clean once every touched origin
    /// reached the disk, so a failed write is retried instead of lost.
    pub fn save_local_storage(&self) {
        let Some(ref dir) = self.storage_dir else {
            return;
        };
        if !self.local_storage.is_dirty() {
            return;
        }
        let changed = self.local_storage.changed_origins();
        let root = dir.join("localStorage");
        if let Err(e) = std::fs::create_dir_all(&root) {
            tracing::warn!("Failed to create {}: {}", root.display(), e);
            return;
        }
        for origin in &changed {
            let path = root.join(format!("{}.json", storage_file_stem(origin)));
            let entries = self.local_storage.snapshot(origin);
            if entries.is_empty() {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => tracing::warn!("Failed to remove {}: {}", path.display(), e),
                }
                continue;
            }
            let stored = StoredOriginStorage { origin: origin.clone(), entries };
            match serde_json::to_string(&stored) {
                Ok(payload) => {
                    if let Err(e) = write_file_atomically(&path, payload.as_bytes()) {
                        tracing::warn!("Failed to save {}: {}", path.display(), e);
                    }
                }
                Err(e) => tracing::warn!("Failed to encode localStorage: {}", e),
            }
        }
        self.local_storage.take_changed();
    }

    /// Persist cookies and Web Storage if storage_dir is configured.
    pub fn save_storage(&self) {
        self.save_local_storage();
        self.save_cookies();
    }

    /// Persist cookies to disk if storage_dir is configured.
    /// Called during graceful shutdown.
    pub fn save_cookies(&self) {
        if let Some(ref dir) = self.storage_dir {
            let _ = std::fs::create_dir_all(dir);
            let cookie_path = dir.join("cookies.json");
            if let Err(e) = self.cookie_jar.save_to_file(&cookie_path) {
                tracing::warn!("Failed to save cookies to {}: {}", cookie_path.display(), e);
            } else {
                tracing::info!("Saved cookies to {}", cookie_path.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn with_full_options_propagates_user_agent_to_http_client() {
        let ctx = BrowserContext::with_full_options(
            "test".to_string(),
            None,
            false,
            Some("Custom-UA/1.0".to_string()),
        );
        assert_eq!(ctx.user_agent, "Custom-UA/1.0");
        let client_ua = ctx.http_client.user_agent.read().await.clone();
        assert_eq!(client_ua, "Custom-UA/1.0");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn with_full_options_falls_back_to_chrome_default() {
        let ctx = BrowserContext::with_full_options(
            "test".to_string(),
            None,
            false,
            None,
        );
        assert!(ctx.user_agent.contains("Chrome"));
        let client_ua = ctx.http_client.user_agent.read().await.clone();
        assert!(client_ua.contains("Chrome"));
        assert_eq!(ctx.user_agent, client_ua);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn with_options_keeps_default_user_agent() {
        let ctx = BrowserContext::with_options("test".to_string(), None, false);
        assert!(ctx.user_agent.contains("Chrome"));
    }

    fn temp_storage_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "obscura-storage-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// --storage-dir has to survive a restart, which is the whole point of a
    /// profile: a second context over the same directory reads back what the
    /// first one wrote.
    #[test]
    fn local_storage_round_trips_through_the_storage_dir() {
        let dir = temp_storage_dir("roundtrip");
        let first = BrowserContext::with_storage("profile-a".into(), Some(dir.clone()));
        first.local_storage.set("https://app.example", "token".into(), "abc".into());
        first.local_storage.set("https://app.example", "theme".into(), "dark".into());
        first.local_storage.set("https://other.example", "k".into(), "v".into());
        first.save_local_storage();

        let files: Vec<String> = std::fs::read_dir(dir.join("localStorage"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(files.len(), 2, "one file per origin: {files:?}");

        let second = BrowserContext::with_storage("profile-b".into(), Some(dir.clone()));
        assert_eq!(
            second.local_storage.get("https://app.example", "token"),
            Some("abc".to_string())
        );
        assert_eq!(
            second.local_storage.get("https://app.example", "theme"),
            Some("dark".to_string())
        );
        assert_eq!(
            second.local_storage.get("https://other.example", "k"),
            Some("v".to_string())
        );
        // Insertion order is part of the format: Storage.key(i) depends on it.
        let keys: Vec<String> = second
            .local_storage
            .snapshot("https://app.example")
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(keys, vec!["token".to_string(), "theme".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Without --storage-dir nothing is written and nothing is loaded, and the
    /// incognito copy of a persistent context stays empty.
    #[test]
    fn local_storage_stays_in_memory_without_a_storage_dir() {
        let persistent = BrowserContext::with_storage("no-dir".into(), None);
        assert!(persistent.storage_dir.is_none());
        persistent.local_storage.set("https://app.example", "k".into(), "v".into());
        persistent.save_local_storage();
        // No directory is configured, so the write is a no-op and the entry
        // stays in this process only.
        assert!(persistent.local_storage.is_dirty());
        assert_eq!(
            persistent.local_storage.get("https://app.example", "k"),
            Some("v".to_string())
        );

        let incognito = persistent.isolated_copy("incognito".into(), false);
        assert_eq!(incognito.local_storage.get("https://app.example", "k"), None);
        // The template keeps its own entry.
        assert_eq!(
            persistent.local_storage.get("https://app.example", "k"),
            Some("v".to_string())
        );
    }

    /// An unchanged store must not touch the disk, or every navigation on a
    /// large profile would rewrite every origin file.
    #[test]
    fn save_local_storage_skips_untouched_origins() {
        let dir = temp_storage_dir("dirty");
        let context = BrowserContext::with_storage("dirty-check".into(), Some(dir.clone()));
        context.local_storage.set("https://app.example", "k".into(), "v".into());
        context.save_local_storage();
        let first_write = std::fs::metadata(
            dir.join("localStorage").join(format!("{}.json", storage_file_stem("https://app.example"))),
        )
        .and_then(|meta| meta.modified())
        .expect("the first write must land");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        context.save_local_storage();
        let second_write = std::fs::metadata(
            dir.join("localStorage").join(format!("{}.json", storage_file_stem("https://app.example"))),
        )
        .and_then(|meta| meta.modified())
        .expect("the file must still exist");
        assert_eq!(
            first_write, second_write,
            "an unchanged store must not be rewritten"
        );

        // A write to one origin must leave another origin's file alone.
        context.local_storage.set("https://other.example", "x".into(), "y".into());
        context.save_local_storage();
        let other_path = dir.join("localStorage").join(format!(
            "{}.json",
            storage_file_stem("https://other.example")
        ));
        let other_first = std::fs::metadata(&other_path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        context.local_storage.set("https://app.example", "k2".into(), "v2".into());
        context.save_local_storage();
        let other_second = std::fs::metadata(&other_path).unwrap().modified().unwrap();
        assert_eq!(other_first, other_second, "an untouched origin must not be rewritten");

        // Clearing an origin removes its file instead of leaving it behind for
        // the next load to resurrect.
        context.local_storage.clear("https://other.example");
        context.save_local_storage();
        assert!(!other_path.exists(), "a cleared origin must not keep its file");
        let after_clear =
            BrowserContext::with_storage("dirty-check-3".into(), Some(dir.clone()));
        assert_eq!(after_clear.local_storage.get("https://other.example", "x"), None);

        let reloaded = BrowserContext::with_storage("dirty-check-2".into(), Some(dir.clone()));
        assert_eq!(
            reloaded.local_storage.get("https://app.example", "k2"),
            Some("v2".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn isolated_copy_does_not_share_mutable_network_state() {
        let source = BrowserContext::with_full_options(
            "source".to_string(),
            None,
            false,
            Some("Template-UA/1.0".to_string()),
        );
        source.cookie_jar.set_cookie("sid=source", &url::Url::parse("https://example.com").unwrap());

        let persistent = source.isolated_copy("persistent".to_string(), true);
        let incognito = source.isolated_copy("incognito".to_string(), false);

        assert_eq!(persistent.cookie_jar.get_all_cookies().len(), 1);
        assert!(incognito.cookie_jar.get_all_cookies().is_empty());
        assert!(persistent
            .cookie_jar
            .get_cookie_header(&url::Url::parse("https://sub.example.com").unwrap())
            .is_empty());
        persistent.cookie_jar.clear();
        persistent.http_client.set_user_agent("Changed-UA/2.0").await;

        assert_eq!(source.cookie_jar.get_all_cookies().len(), 1);
        assert_eq!(source.http_client.user_agent.read().await.as_str(), "Template-UA/1.0");
    }
}
