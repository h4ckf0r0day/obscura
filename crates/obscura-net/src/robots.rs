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

        // RFC 9309 2.2.2: the most specific (longest) match wins; Allow only
        // wins when both rules have the same length.
        let mut best: Option<(usize, bool)> = None;
        for pattern in &rules.allowed {
            if path_matches(path, pattern)
                && best.is_none_or(|(length, _)| pattern.len() >= length)
            {
                best = Some((pattern.len(), true));
            }
        }
        for pattern in &rules.disallowed {
            if path_matches(path, pattern)
                && best.is_none_or(|(length, allow)| pattern.len() > length || (pattern.len() == length && !allow))
            {
                best = Some((pattern.len(), false));
            }
        }
        best.is_none_or(|(_, allow)| allow)
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
    // RFC 9309 2.2.1: a group applies when any of its user-agent lines names
    // us, or when it names "*". Consecutive user-agent lines belong to the
    // same group; the first rule ends the agent list, and a later user-agent
    // line starts a new group.
    let our_tokens: Vec<String> = our_agent
        .split(|c: char| c.is_whitespace() || matches!(c, '/' | '(' | ')' | ';' | ','))
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect();

    struct Group {
        agents: Vec<String>,
        disallowed: Vec<String>,
        allowed: Vec<String>,
        rules_started: bool,
    }

    let mut groups: Vec<Group> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_lowercase();
        let value = value.trim();
        match key.as_str() {
            "user-agent" => {
                if groups.last().is_none_or(|group| group.rules_started) {
                    groups.push(Group {
                        agents: Vec::new(),
                        disallowed: Vec::new(),
                        allowed: Vec::new(),
                        rules_started: false,
                    });
                }
                if let Some(group) = groups.last_mut() {
                    group.agents.push(value.to_lowercase());
                }
            }
            "allow" | "disallow" => {
                let Some(group) = groups.last_mut() else {
                    continue;
                };
                group.rules_started = true;
                if value.is_empty() {
                    continue;
                }
                if key == "allow" {
                    group.allowed.push(value.to_string());
                } else {
                    group.disallowed.push(value.to_string());
                }
            }
            _ => {}
        }
    }

    let matches = |group: &Group| {
        group
            .agents
            .iter()
            .any(|agent| agent != "*" && our_tokens.iter().any(|token| token == agent))
    };
    let specific = groups
        .iter()
        .any(&matches);
    let mut disallowed = Vec::new();
    let mut allowed = Vec::new();
    for group in groups.iter().filter(|group| {
        if specific { matches(group) } else { group.agents.iter().any(|agent| agent == "*") }
    }) {
        disallowed.extend(group.disallowed.iter().cloned());
        allowed.extend(group.allowed.iter().cloned());
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
    fn consecutive_user_agent_lines_share_one_group() {
        // RFC 9309 2.1: a group's user-agent lines are consecutive, and the
        // first rule ends that list. Google's robots.txt relies on this.
        let body = "User-agent: *\nUser-agent: Yandex\nDisallow: /search\nAllow: /search/about\n";
        let cache = RobotsCache::new();
        cache.parse_and_store("google.test", body, "Obscura");
        assert!(!cache.is_allowed("google.test", "/search?q=test"));
        assert!(cache.is_allowed("google.test", "/search/about"));
    }

    #[test]
    fn longest_matching_rule_wins_and_allow_wins_a_tie() {
        // RFC 9309 2.2.2: the most specific match decides. Allow only wins
        // when both patterns are the same length.
        let body = "User-agent: *\nAllow: /p\nDisallow: /p/private\nDisallow: /tie\nAllow: /tie\n";
        let cache = RobotsCache::new();
        cache.parse_and_store("paths.test", body, "Obscura");
        assert!(cache.is_allowed("paths.test", "/p"));
        assert!(!cache.is_allowed("paths.test", "/p/private/x"));
        assert!(cache.is_allowed("paths.test", "/tie"));
    }

    #[test]
    fn a_matching_specific_group_overrides_the_star_group() {
        let body = "User-agent: *\nDisallow: /\n\nUser-agent: Obscura\nDisallow: /only\n";
        let cache = RobotsCache::new();
        cache.parse_and_store("specific.test", body, "Obscura");
        assert!(cache.is_allowed("specific.test", "/elsewhere"));
        assert!(!cache.is_allowed("specific.test", "/only"));
    }

    #[test]
    fn unrelated_agents_in_wildcard_groups_do_not_select_specific_rules() {
        let cache = RobotsCache::new();
        let mixed = "User-agent: *\nUser-agent: OtherBot\nDisallow: /mixed\n\n";
        cache.parse_and_store("wildcard.test", &format!("{mixed}User-agent: *\nDisallow: /fallback\n"), "Obscura");
        assert!(!cache.is_allowed("wildcard.test", "/mixed"));
        assert!(!cache.is_allowed("wildcard.test", "/fallback"));
        cache.parse_and_store("specific.test", &format!("{mixed}User-agent: Obscura\nDisallow: /own\n"), "Obscura");
        assert!(cache.is_allowed("specific.test", "/mixed"));
        assert!(!cache.is_allowed("specific.test", "/own"));
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
