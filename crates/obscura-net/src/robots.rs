use std::collections::{HashMap, VecDeque};
use std::sync::RwLock;

/// Origins whose robots.txt rules are kept per context; past this the oldest is
/// evicted and refetched if visited again.
const MAX_ROBOTS_ENTRIES: usize = 4096;
/// Bytes of a robots.txt body that are parsed (Google's documented limit).
const MAX_ROBOTS_BODY_BYTES: usize = 500 * 1024;

pub struct RobotsCache {
    cache: RwLock<RobotsEntries>,
}

#[derive(Default)]
struct RobotsEntries {
    rules: HashMap<String, RobotsRules>,
    /// Origins in insertion order, oldest first, for eviction.
    order: VecDeque<String>,
    /// Set by enable_persistence; writes the shared robots-cache.json.
    persist_path: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RobotsRules {
    disallowed: Vec<String>,
    allowed: Vec<String>,
    /// Wall-clock seconds when the body was fetched, for the shared-file TTL.
    #[serde(default)]
    ts: u64,
}

/// Entries older than this are refetched even when present. The
/// per-process cache used to live forever; the shared file must not.
const ROBOTS_TTL_SECS: u64 = 24 * 3600;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedRobots {
    /// Wall-clock seconds when the entry was fetched.
    entries: HashMap<String, RobotsRules>,
}

impl RobotsCache {
    pub fn new() -> Self {
        RobotsCache {
            cache: RwLock::new(RobotsEntries::default()),
        }
    }

    pub fn parse_and_store(&self, domain: &str, body: &str, our_agent: &str) {
        let rules = parse_robots_txt(truncate_body(body), our_agent);
        let fresh = RobotsRules {
            disallowed: rules.disallowed,
            allowed: rules.allowed,
            ts: now_secs(),
        };
        let mut cache = self.cache.write().unwrap();
        if cache.rules.insert(domain.to_string(), fresh).is_none() {
            cache.order.push_back(domain.to_string());
        }
        while cache.rules.len() > MAX_ROBOTS_ENTRIES {
            let Some(oldest) = cache.order.pop_front() else {
                break;
            };
            cache.rules.remove(&oldest);
        }
        let should_persist = cache.persist_path.is_some();
        drop(cache);
        if should_persist {
            self.persist();
        }
    }

    /// Share this cache through `{robots_cache_dir}/robots-cache.json`:
    /// entries are loaded once and every newly fetched origin is merged
    /// back, so sibling processes and recycled workers stop refetching
    /// robots.txt for the same site. Idempotent; safe to call on every
    /// navigation.
    pub fn enable_persistence(&self, dir: &std::path::Path) {
        {
            let cache = self.cache.read().unwrap();
            if cache.persist_path.is_some() {
                return;
            }
        }
        let path = dir.join("robots-cache.json");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut cache = self.cache.write().unwrap();
        if cache.persist_path.is_some() {
            return;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(persisted) = serde_json::from_str::<PersistedRobots>(&text) {
                for (origin, rules) in persisted.entries {
                    if now_secs().saturating_sub(rules.ts) > ROBOTS_TTL_SECS {
                        continue;
                    }
                    if cache.rules.insert(origin.clone(), rules).is_none() {
                        cache.order.push_back(origin);
                    }
                }
            }
        }
        cache.persist_path = Some(path);
    }

    /// Merge-write the cache to the shared file. Atomic rename keeps
    /// concurrent readers consistent; a racing writer can lose another
    /// process's newest entry, which only costs one extra robots fetch.
    fn persist(&self) {
        let (path, entries) = {
            let cache = self.cache.read().unwrap();
            let Some(path) = cache.persist_path.clone() else { return };
            (path, cache.rules.clone())
        };
        // Reload under the same path so entries another process added since
        // our load survive, then merge ours over them.
        let mut merged: HashMap<String, RobotsRules> = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<PersistedRobots>(&text)
                .map(|p| p.entries)
                .unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        for (origin, rules) in entries {
            merged.insert(origin, rules);
        }
        let body = match serde_json::to_string(&PersistedRobots { entries: merged }) {
            Ok(body) => body,
            Err(_) => return,
        };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.cache.read().unwrap().rules.len()
    }

    /// True only for a fresh entry: a stale one (past the shared-file TTL)
    /// behaves like a miss so the caller refetches the body.
    pub fn contains(&self, origin: &str) -> bool {
        let cache = self.cache.read().unwrap();
        cache.rules.get(origin).is_some_and(|rules| {
            now_secs().saturating_sub(rules.ts) <= ROBOTS_TTL_SECS
        })
    }

    pub fn is_allowed(&self, domain: &str, path: &str) -> bool {
        let cache = self.cache.read().unwrap();
        let rules = match cache.rules.get(domain) {
            Some(r) => r,
            None => return true,
        };

        for pattern in &rules.allowed {
            if path_matches(path, pattern) {
                return true;
            }
        }

        for pattern in &rules.disallowed {
            if path_matches(path, pattern) {
                return false;
            }
        }

        true
    }
}

impl Default for RobotsCache {
    fn default() -> Self {
        Self::new()
    }
}

/// The first `MAX_ROBOTS_BODY_BYTES` of `body`, cut back to the last complete
/// line so no rule is parsed from a truncated line.
fn truncate_body(body: &str) -> &str {
    if body.len() <= MAX_ROBOTS_BODY_BYTES {
        return body;
    }
    let mut end = MAX_ROBOTS_BODY_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let head = &body[..end];
    head.rfind('\n').map_or(head, |newline| &head[..newline])
}

fn parse_robots_txt(body: &str, our_agent: &str) -> RobotsRules {
    // RFC 9309 2.2.1: a group applies when its product token equals one of
    // ours, case-insensitively. Our identification string is a full
    // User-Agent, so split it into its product tokens. Substring matching in
    // either direction let "User-agent: Moz" govern a Mozilla/5.0 UA and
    // "User-agent: ObscuraBot" govern a UA of "Obscura".
    let our_tokens: Vec<String> = our_agent
        .split(|c: char| c.is_whitespace() || matches!(c, '/' | '(' | ')' | ';' | ','))
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut disallowed = Vec::new();
    let mut allowed = Vec::new();
    let mut in_matching_section = false;
    let mut found_specific = false;

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim();

            match key.as_str() {
                "user-agent" => {
                    let agent = value.to_lowercase();
                    in_matching_section =
                        agent == "*" || our_tokens.iter().any(|token| *token == agent);
                    if agent != "*" && in_matching_section {
                        found_specific = true;
                    }
                }
                "disallow" if in_matching_section && !value.is_empty() => {
                    disallowed.push(value.to_string());
                }
                "allow" if in_matching_section && !value.is_empty() => {
                    allowed.push(value.to_string());
                }
                _ => {}
            }
        }
    }

    if !found_specific {
        disallowed.clear();
        allowed.clear();
        in_matching_section = false;

        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_lowercase();
                let value = value.trim();
                match key.as_str() {
                    "user-agent" => {
                        in_matching_section = value.trim() == "*";
                    }
                    "disallow" if in_matching_section && !value.is_empty() => {
                        disallowed.push(value.to_string());
                    }
                    "allow" if in_matching_section && !value.is_empty() => {
                        allowed.push(value.to_string());
                    }
                    _ => {}
                }
            }
        }
    }

    RobotsRules { disallowed, allowed, ts: 0 }
}

fn path_matches(path: &str, pattern: &str) -> bool {
    if pattern.ends_with('*') {
        path.starts_with(&pattern[..pattern.len() - 1])
    } else if pattern.ends_with('$') {
        path == &pattern[..pattern.len() - 1]
    } else {
        path.starts_with(pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_robots() {
        let body = "User-agent: *\nDisallow: /private/\nDisallow: /admin\nAllow: /admin/public\n";
        let cache = RobotsCache::new();
        cache.parse_and_store("example.com", body, "Obscura");
        assert!(cache.is_allowed("example.com", "/"));
        assert!(cache.is_allowed("example.com", "/page"));
        assert!(!cache.is_allowed("example.com", "/private/secret"));
        assert!(!cache.is_allowed("example.com", "/admin"));
        assert!(cache.is_allowed("example.com", "/admin/public"));
    }

    #[test]
    fn agent_groups_match_product_tokens_not_substrings() {
        // RFC 9309 2.2.1: a group applies when its product token equals one of
        // ours. Substring matching in either direction let "User-agent: Moz"
        // govern a Mozilla/5.0 UA and "User-agent: ObscuraBot" govern "Obscura".
        let chrome = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";
        let cache = RobotsCache::new();
        cache.parse_and_store("a.test", "User-agent: Moz\nDisallow: /\n", chrome);
        assert!(cache.is_allowed("a.test", "/page"), "a token prefix is not a match");
        cache.parse_and_store("b.test", "User-agent: chrome\nDisallow: /\n", chrome);
        assert!(!cache.is_allowed("b.test", "/page"), "an exact token still matches, case-insensitively");
        cache.parse_and_store("c.test", "User-agent: ObscuraBot\nDisallow: /\n", "Obscura");
        assert!(cache.is_allowed("c.test", "/page"), "our token being a substring of theirs is not a match");
        cache.parse_and_store("d.test", "User-agent: Obscura\nDisallow: /\n", "Obscura/1.0 (+https://example.test)");
        assert!(!cache.is_allowed("d.test", "/page"), "a product token followed by a version matches");
    }

    #[test]
    fn test_no_rules_means_allowed() {
        let cache = RobotsCache::new();
        assert!(cache.is_allowed("unknown.com", "/anything"));
        assert!(!cache.contains("unknown.com"));

        cache.parse_and_store("unknown.com", "", "Obscura");
        assert!(cache.contains("unknown.com"));
        assert!(cache.is_allowed("unknown.com", "/anything"));
    }

    #[test]
    fn test_disallow_all() {
        let body = "User-agent: *\nDisallow: /\n";
        let cache = RobotsCache::new();
        cache.parse_and_store("blocked.com", body, "Obscura");
        assert!(!cache.is_allowed("blocked.com", "/"));
        assert!(!cache.is_allowed("blocked.com", "/page"));
    }

    // One entry per navigated origin must not grow the cache without bound in
    // a long-lived context; the oldest origin is evicted and simply refetched.
    #[test]
    fn cache_evicts_oldest_origin_past_the_entry_cap() {
        let cache = RobotsCache::new();
        for i in 0..MAX_ROBOTS_ENTRIES + 10 {
            cache.parse_and_store(&format!("https://site{i}.test"), "", "Obscura");
        }
        assert_eq!(cache.len(), MAX_ROBOTS_ENTRIES);
        assert!(!cache.contains("https://site0.test"));
        assert!(cache.contains(&format!("https://site{}.test", MAX_ROBOTS_ENTRIES + 9)));

        // Re-storing a cached origin does not duplicate its eviction slot.
        let last = format!("https://site{}.test", MAX_ROBOTS_ENTRIES + 9);
        cache.parse_and_store(&last, "", "Obscura");
        assert_eq!(cache.len(), MAX_ROBOTS_ENTRIES);
    }

    // Like Google, only the first 500 KiB of a robots.txt body is parsed, so one
    // oversized file cannot inflate its cache entry.
    #[test]
    fn rules_past_the_body_cap_are_ignored() {
        let mut body = String::from("User-agent: *\nDisallow: /early\n");
        while body.len() <= MAX_ROBOTS_BODY_BYTES {
            body.push_str("# padding padding padding padding padding\n");
        }
        body.push_str("Disallow: /late\n");
        let cache = RobotsCache::new();
        cache.parse_and_store("big.test", &body, "Obscura");
        assert!(!cache.is_allowed("big.test", "/early"));
        assert!(cache.is_allowed("big.test", "/late"));
    }

    fn temp_robots_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "obscura-robots-test-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // The shared-cache contract: a second cache pointed at the same dir sees
    // the first one's entries without refetching, which is what lets 50 serve
    // workers fetch a site's robots.txt once per TTL instead of 50 times.
    #[test]
    fn persistence_round_trips_between_cache_instances() {
        let dir = temp_robots_dir("round-trip");
        let first = RobotsCache::new();
        first.enable_persistence(&dir);
        first.parse_and_store(
            "shared.test",
            "User-agent: *\nDisallow: /private\n",
            "Obscura",
        );
        assert!(dir.join("robots-cache.json").exists());

        let second = RobotsCache::new();
        second.enable_persistence(&dir);
        assert!(second.contains("shared.test"));
        assert!(!second.is_allowed("shared.test", "/private"));
        assert!(second.is_allowed("shared.test", "/public"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A stale entry is a miss, so a long-lived deployment cannot get stuck on
    // rules whose site changed them days ago.
    #[test]
    fn persisted_entries_past_the_ttl_behave_like_a_miss() {
        let dir = temp_robots_dir("ttl");
        let dir_copy = dir.clone();
        let cache = RobotsCache::new();
        cache.enable_persistence(&dir);
        cache.parse_and_store("stale.test", "User-agent: *\nDisallow: /\n", "Obscura");
        // Age the persisted entry past the TTL, then reload it in a fresh
        // cache like a recycled worker would.
        let path = dir_copy.join("robots-cache.json");
        let mut persisted: PersistedRobots =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for rules in persisted.entries.values_mut() {
            rules.ts = now_secs() - ROBOTS_TTL_SECS - 1;
        }
        std::fs::write(&path, serde_json::to_string(&persisted).unwrap()).unwrap();

        let reloaded = RobotsCache::new();
        reloaded.enable_persistence(&dir_copy);
        assert!(!reloaded.contains("stale.test"));
        let _ = std::fs::remove_dir_all(&dir_copy);
    }
}
