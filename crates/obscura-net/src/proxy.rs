//! Proxy pool for per-context IP rotation.
//!
//! `wreq` bakes a single proxy into a client at build time, so rotation happens
//! at the **granularity of a browser context** (each fetch builds a client →
//! picks the next proxy). This widens free-scrape coverage/volume: spread
//! requests across many egress IPs to dodge rate-limits/bans, and pick a
//! country-appropriate exit.
//!
//! Config: `OBSCURA_PROXIES` env (comma-separated URLs, each may carry auth:
//! `http://user:pass@host:port`, `socks5://host:port`). The CLI `--proxy-pool`
//! flag just sets that env. An explicit single `--proxy` always overrides the
//! pool (see `BrowserContext`).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

struct ProxyEntry {
    url: String,
    /// Flipped false by `mark_bad`; rotation skips unhealthy entries until none
    /// remain healthy (then it falls back to trying them anyway).
    healthy: AtomicBool,
}

pub struct ProxyPool {
    proxies: Vec<ProxyEntry>,
    cursor: AtomicUsize,
}

static GLOBAL: OnceLock<ProxyPool> = OnceLock::new();

impl ProxyPool {
    /// Build from a list of proxy URLs (blank/whitespace entries dropped).
    pub fn from_list<I: IntoIterator<Item = String>>(urls: I) -> Self {
        let proxies = urls
            .into_iter()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
            .map(|url| ProxyEntry { url, healthy: AtomicBool::new(true) })
            .collect();
        ProxyPool {
            proxies,
            cursor: AtomicUsize::new(0),
        }
    }

    /// Build from the `OBSCURA_PROXIES` env var (comma-separated).
    pub fn from_env() -> Self {
        let raw = std::env::var("OBSCURA_PROXIES").unwrap_or_default();
        Self::from_list(raw.split(',').map(|s| s.to_string()))
    }

    /// Process-wide pool, lazily initialized from `OBSCURA_PROXIES`. The CLI sets
    /// that env (from `--proxy-pool`) before any context is created, so this
    /// reflects it.
    pub fn global() -> &'static ProxyPool {
        GLOBAL.get_or_init(Self::from_env)
    }

    pub fn is_empty(&self) -> bool {
        self.proxies.is_empty()
    }

    pub fn len(&self) -> usize {
        self.proxies.len()
    }

    /// Round-robin the next **healthy** proxy URL. `None` when the pool is empty
    /// (caller then goes direct). If every entry is marked unhealthy we still
    /// return the round-robin pick — better to retry a flaky proxy than to leak
    /// the real IP by going direct unexpectedly.
    pub fn next(&self) -> Option<String> {
        let n = self.proxies.len();
        if n == 0 {
            return None;
        }
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % n;
        for offset in 0..n {
            let e = &self.proxies[(start + offset) % n];
            if e.healthy.load(Ordering::Relaxed) {
                return Some(e.url.clone());
            }
        }
        Some(self.proxies[start].url.clone())
    }

    /// Sidelines a proxy after a transport failure; rotation skips it until a
    /// later `mark_good` (or until all are unhealthy).
    pub fn mark_bad(&self, url: &str) {
        for e in &self.proxies {
            if e.url == url {
                e.healthy.store(false, Ordering::Relaxed);
            }
        }
    }

    pub fn mark_good(&self, url: &str) {
        for e in &self.proxies {
            if e.url == url {
                e.healthy.store(true, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pool_returns_none() {
        let p = ProxyPool::from_list(Vec::<String>::new());
        assert!(p.is_empty());
        assert_eq!(p.next(), None);
    }

    #[test]
    fn round_robins_and_skips_unhealthy() {
        let p = ProxyPool::from_list(
            ["http://a:1", " ", "http://b:2", "http://c:3"].iter().map(|s| s.to_string()),
        );
        assert_eq!(p.len(), 3); // blank dropped
        // round-robin over 3
        let seq: Vec<String> = (0..3).filter_map(|_| p.next()).collect();
        assert_eq!(seq, vec!["http://a:1", "http://b:2", "http://c:3"]);
        // mark one bad → it's skipped
        p.mark_bad("http://b:2");
        let after: Vec<String> = (0..3).filter_map(|_| p.next()).collect();
        assert!(!after.contains(&"http://b:2".to_string()), "unhealthy skipped: {after:?}");
    }
}
