//! Address bar input: decide between a URL and a search, and format URLs
//! for display.

use url::Url;

// DuckDuckGo Lite is plain server-rendered HTML, which the engine lays out
// well and which does not put automated clients behind a captcha.
pub const SEARCH_URL: &str = "https://lite.duckduckgo.com/lite/?q=";
pub const NEW_TAB_URL: &str = "about:blank";

/// Turn what the user typed into a URL to load.
pub fn resolve(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let has_scheme = ["http://", "https://", "about:", "data:", "file:"]
        .iter()
        .any(|scheme| input.len() >= scheme.len() && input[..scheme.len()].eq_ignore_ascii_case(scheme));
    if has_scheme {
        if let Ok(url) = Url::parse(input) {
            return Some(url.to_string());
        }
    }
    if !input.contains(char::is_whitespace) {
        let host = input.split(['/', '?', '#']).next().unwrap_or("");
        let bare_host = host.rsplit_once(':').map_or(host, |(name, port)| {
            if port.chars().all(|c| c.is_ascii_digit()) { name } else { host }
        });
        let local = bare_host.eq_ignore_ascii_case("localhost")
            || bare_host.parse::<std::net::IpAddr>().is_ok();
        let looks_like_domain = bare_host.contains('.')
            && !bare_host.starts_with('.')
            && !bare_host.ends_with('.')
            && bare_host.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '.');
        if local || looks_like_domain {
            let scheme = if local { "http" } else { "https" };
            if let Ok(url) = Url::parse(&format!("{scheme}://{input}")) {
                return Some(url.to_string());
            }
        }
    }
    Some(format!("{SEARCH_URL}{}", encode_query(input)))
}

fn encode_query(query: &str) -> String {
    url::form_urlencoded::byte_serialize(query.as_bytes()).collect()
}

/// The URL as shown while the address bar is not being edited: scheme,
/// `www.` and a lone trailing slash are dropped, like other modern browsers.
pub fn display(url: &str) -> String {
    if url == NEW_TAB_URL || url.is_empty() {
        return String::new();
    }
    let Ok(parsed) = Url::parse(url) else {
        return url.to_string();
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return url.to_string();
    }
    let rest = &url[parsed.scheme().len() + 3..];
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    rest.strip_suffix('/')
        .filter(|trimmed| !trimmed.contains('/'))
        .unwrap_or(rest)
        .to_string()
}

pub fn is_secure(url: &str) -> bool {
    url.starts_with("https://")
}

/// Host shown as a tab title before the page reports its own.
pub fn fallback_title(url: &str) -> String {
    if url == NEW_TAB_URL || url.is_empty() {
        return "New Tab".into();
    }
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string()))
        .unwrap_or_else(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_become_https_urls() {
        assert_eq!(resolve("example.com").unwrap(), "https://example.com/");
        assert_eq!(resolve("en.wikipedia.org/wiki/Rust").unwrap(), "https://en.wikipedia.org/wiki/Rust");
        assert_eq!(resolve("  HTTP://Example.com/a ").unwrap(), "http://example.com/a");
    }

    #[test]
    fn local_hosts_use_http() {
        assert_eq!(resolve("localhost:8080/x").unwrap(), "http://localhost:8080/x");
        assert_eq!(resolve("127.0.0.1").unwrap(), "http://127.0.0.1/");
    }

    #[test]
    fn everything_else_is_a_search() {
        assert_eq!(resolve("rust borrow checker").unwrap(), format!("{SEARCH_URL}rust+borrow+checker"));
        assert_eq!(resolve("obscura").unwrap(), format!("{SEARCH_URL}obscura"));
        assert_eq!(resolve("a&b").unwrap(), format!("{SEARCH_URL}a%26b"));
        assert!(resolve("   ").is_none());
    }

    #[test]
    fn display_hides_noise() {
        assert_eq!(display("https://www.example.com/"), "example.com");
        assert_eq!(display("https://example.com/docs/"), "example.com/docs/");
        assert_eq!(display("http://localhost:3000/a?b=1"), "localhost:3000/a?b=1");
        assert_eq!(display("about:blank"), "");
    }
}
