/// Helpers shared across CDP domain handlers.
///
/// The file-scheme detector here is reused by every CDP entrypoint that can
/// trigger a navigation, so we don't end up with one domain enforcing the
/// `--allow-file-access` gate and another silently letting `file://` through
/// (see GHSA-q55h-vfv9-qcr5 and its incomplete-fix variant in
/// `Target.createTarget`).

/// Returns true when `raw` parses as a `file:`-scheme URL, or syntactically
/// starts with `file:` after a possible leading-whitespace strip. Matching is
/// case-insensitive on the scheme so neither `FILE://` nor `File://` slips
/// past callers that gate on `file://`.
pub(crate) fn url_is_file_scheme(raw: &str) -> bool {
    url::Url::parse(raw)
        .map(|u| u.scheme().eq_ignore_ascii_case("file"))
        .unwrap_or_else(|_| {
            raw.trim_start().to_ascii_lowercase().starts_with("file:")
        })
}

/// Truncate `s` to at most `max` bytes, never splitting a UTF-8 character.
///
/// `&s[..max]` panics when `max` lands inside a multi-byte character, and the
/// strings we truncate for log previews are attacker-controlled (raw WebSocket
/// frames, intercepted URLs). A single frame whose byte `max` straddles a
/// multi-byte char would otherwise panic the CDP processor task
/// (`str::floor_char_boundary` would do this but is still unstable).
pub(crate) fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_plain_file_url() {
        assert!(url_is_file_scheme("file:///etc/passwd"));
    }

    #[test]
    fn matches_case_insensitively() {
        assert!(url_is_file_scheme("FILE:///etc/passwd"));
        assert!(url_is_file_scheme("File:///etc/passwd"));
        assert!(url_is_file_scheme("fIlE:///etc/passwd"));
    }

    #[test]
    fn matches_with_leading_whitespace_fallback() {
        // url::Url::parse rejects leading whitespace, but the syntactic
        // fallback still catches `   file:...` so callers can't be tricked
        // into letting it through.
        assert!(url_is_file_scheme("   file:///etc/passwd"));
    }

    #[test]
    fn rejects_http_https_about_data() {
        assert!(!url_is_file_scheme("http://example.com"));
        assert!(!url_is_file_scheme("https://example.com"));
        assert!(!url_is_file_scheme("about:blank"));
        assert!(!url_is_file_scheme("data:text/plain,hi"));
        assert!(!url_is_file_scheme(""));
    }

    #[test]
    fn rejects_lookalikes_that_are_not_file_scheme() {
        // The URL parser rejects these (no `://`), so the syntactic fallback
        // kicks in. `file` appearing anywhere except as the leading scheme
        // must not match.
        assert!(!url_is_file_scheme("notfile:///x"));
        assert!(!url_is_file_scheme("http://file/"));
    }

    #[test]
    fn truncate_never_splits_a_multibyte_char() {
        // 199 ASCII bytes + '€' (3 bytes, occupying indices 199..=201): byte 200
        // falls inside the '€'. This is exactly the shape of a malformed CDP
        // frame that would reach the `warn!("Invalid CDP: ...")` preview.
        let s = format!("{}€tail", "a".repeat(199));
        assert!(!s.is_char_boundary(200), "setup: byte 200 splits the € char");

        // The old logging code did `&s[..s.len().min(200)]`, which panics here —
        // a single crafted frame would take down the CDP processor task.
        let naive = std::panic::catch_unwind(|| {
            let _ = &s[..s.len().min(200)];
        });
        assert!(naive.is_err(), "raw byte slice at a non-char-boundary must panic");

        // The helper truncates on the boundary before the € instead.
        let safe = truncate_on_char_boundary(&s, 200);
        assert!(s.starts_with(safe));
        assert!(safe.len() <= 200);
        assert_eq!(safe.len(), 199, "should stop right before the € char");
    }

    #[test]
    fn truncate_returns_whole_string_when_short() {
        assert_eq!(truncate_on_char_boundary("hi", 200), "hi");
        assert_eq!(truncate_on_char_boundary("", 10), "");
        // Exact-length and exact-boundary cases are returned unchanged.
        assert_eq!(truncate_on_char_boundary("abc", 3), "abc");
    }
}
