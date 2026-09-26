use std::collections::HashMap;
use std::sync::RwLock;

pub struct RobotsCache {
    cache: RwLock<HashMap<String, RobotsRules>>,
}

#[derive(Debug, Clone)]
struct RobotsRules {
    disallowed: Vec<String>,
    allowed: Vec<String>,
}

impl RobotsCache {
    pub fn new() -> Self {
        RobotsCache {
            cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn parse_and_store(&self, domain: &str, body: &str, our_agent: &str) {
        let rules = parse_robots_txt(body, our_agent);
        self.cache.write().unwrap().insert(domain.to_string(), rules);
    }

    pub fn contains(&self, origin: &str) -> bool {
        self.cache.read().unwrap().contains_key(origin)
    }

    pub fn is_allowed(&self, domain: &str, path: &str) -> bool {
        let cache = self.cache.read().unwrap();
        let rules = match cache.get(domain) {
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
}
