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
}

#[derive(Debug, Clone)]
struct RobotsRules {
    disallowed: Vec<String>,
    allowed: Vec<String>,
}

impl RobotsCache {
    pub fn new() -> Self {
        RobotsCache {
            cache: RwLock::new(RobotsEntries::default()),
        }
    }

    pub fn parse_and_store(&self, domain: &str, body: &str, our_agent: &str) {
        let rules = parse_robots_txt(truncate_body(body), our_agent);
        let mut cache = self.cache.write().unwrap();
        if cache.rules.insert(domain.to_string(), rules).is_none() {
            cache.order.push_back(domain.to_string());
        }
        while cache.rules.len() > MAX_ROBOTS_ENTRIES {
            let Some(oldest) = cache.order.pop_front() else {
                break;
            };
            cache.rules.remove(&oldest);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.cache.read().unwrap().rules.len()
    }

    pub fn contains(&self, origin: &str) -> bool {
        self.cache.read().unwrap().rules.contains_key(origin)
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
    let our_agent_lower = our_agent.to_lowercase();
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
                    in_matching_section = agent == "*"
                        || our_agent_lower.contains(&agent)
                        || agent.contains(&our_agent_lower);
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

    RobotsRules { disallowed, allowed }
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
}
