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
        self.cache
            .write()
            .unwrap()
            .insert(domain.to_string(), rules);
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
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
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

    RobotsRules {
        disallowed,
        allowed,
    }
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
