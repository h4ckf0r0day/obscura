use std::collections::HashSet;
use std::sync::OnceLock;

const PGL_LIST: &str = include_str!("pgl_domains.txt");

fn blocklist() -> &'static HashSet<&'static str> {
    static BLOCKLIST: OnceLock<HashSet<&str>> = OnceLock::new();
    BLOCKLIST.get_or_init(|| {
        let mut set = HashSet::with_capacity(4000);
        for line in PGL_LIST.lines() {
            let domain = line.trim();
            if !domain.is_empty() && !domain.starts_with('#') {
                set.insert(domain);
            }
        }
        for domain in EXTRA_DOMAINS {
            set.insert(*domain);
        }
        set
    })
}

pub fn is_blocked(host: &str) -> bool {
    let bl = blocklist();

    if bl.contains(host) {
        return true;
    }

    let mut domain = host;
    while let Some(pos) = domain.find('.') {
        domain = &domain[pos + 1..];
        if bl.contains(domain) {
            return true;
        }
    }

    false
}

static EXTRA_DOMAINS: &[&str] = &[];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        assert!(is_blocked("google-analytics.com"));
        assert!(is_blocked("doubleclick.net"));
    }

    #[test]
    fn test_subdomain_match() {
        assert!(is_blocked("www.google-analytics.com"));
        assert!(is_blocked("ssl.google-analytics.com"));
    }

    #[test]
    fn test_not_blocked() {
        assert!(!is_blocked("google.com"));
        assert!(!is_blocked("example.com"));
        assert!(!is_blocked("github.com"));
    }

    #[test]
    fn test_pgl_domains() {
        assert!(is_blocked("adnxs.com"));
        assert!(is_blocked("criteo.com"));
    }

    #[test]
    fn test_blocklist_size() {
        assert!(blocklist().len() > 3500);
    }
}
