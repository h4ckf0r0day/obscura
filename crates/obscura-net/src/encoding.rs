//! Charset detection and decoding for HTTP response bodies.
//!
//! Issue #113: obscura used to call `String::from_utf8_lossy` on every
//! response body, which silently corrupts every non-UTF-8 page (GBK, Big5,
//! Shift-JIS, Windows-125x, EUC-KR, ISO-8859-x). Picking the right decoder
//! is required for scraping non-Latin sites at all.
//!
//! Detection order, mirroring real browsers (HTML5 spec § 8.2.2.4):
//!   1. `Content-Type: text/html; charset=...` from the HTTP response header.
//!   2. `<meta charset="...">` or `<meta http-equiv="Content-Type" content="text/html; charset=...">`
//!      sniffed from the first 1024 bytes of the body.
//!   3. Default UTF-8.
//!
//! For non-HTML resources (JS, CSS, JSON), only steps 1 and 3 apply.

use encoding_rs::{Encoding, UTF_8};

/// Decode an HTTP response body. `content_type_header` is the raw header
/// value if present (e.g. `text/html; charset=gbk`). For HTML resources,
/// the parser also sniffs `<meta charset>` in the first 1KB.
pub fn decode_response(bytes: &[u8], content_type_header: Option<&str>) -> String {
    let (encoding, _) = detect_encoding(bytes, content_type_header);
    let (cow, _, _) = encoding.decode(bytes);
    cow.into_owned()
}

/// Same as `decode_response` but skips the `<meta charset>` sniff. Use for
/// non-HTML resources where embedded HTML meta tags are not authoritative
/// (script and style bodies, JSON, plain text).
pub fn decode_non_html(bytes: &[u8], content_type_header: Option<&str>) -> String {
    let encoding = content_type_header
        .and_then(charset_from_content_type)
        .and_then(|name| Encoding::for_label(name.as_bytes()))
        .unwrap_or(UTF_8);
    let (cow, _, _) = encoding.decode(bytes);
    cow.into_owned()
}

/// Resolve the encoding to use for an HTML response, mirroring the HTML5
/// detection order. Returns the encoding and a tag describing where it was
/// picked from (for logging / tests).
pub fn detect_encoding<'a>(
    bytes: &'a [u8],
    content_type_header: Option<&str>,
) -> (&'static Encoding, &'static str) {
    if let Some(charset) = content_type_header.and_then(charset_from_content_type) {
        if let Some(enc) = Encoding::for_label(charset.as_bytes()) {
            return (enc, "content-type");
        }
    }
    if let Some(enc) = sniff_meta_charset(bytes) {
        return (enc, "meta-charset");
    }
    (UTF_8, "default-utf8")
}

/// Pull the `charset=` parameter out of a Content-Type header value.
fn charset_from_content_type(header: &str) -> Option<String> {
    for part in header.split(';') {
        let trimmed = part.trim();
        if let Some(rest) = trimmed.strip_prefix("charset=").or_else(|| trimmed.strip_prefix("Charset=")) {
            // Strip surrounding quotes if present.
            let value = rest.trim_matches(|c: char| c == '"' || c == '\'').trim();
            if !value.is_empty() {
                return Some(value.to_ascii_lowercase());
            }
        }
        // Some servers send `Content-Type: text/html; CHARSET = gbk`.
        let lower = trimmed.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("charset") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let value = rest.trim().trim_matches(|c: char| c == '"' || c == '\'');
                if !value.is_empty() {
                    return Some(value.to_ascii_lowercase());
                }
            }
        }
    }
    None
}

/// Scan the first 1024 bytes for a `<meta charset="...">` or
/// `<meta http-equiv="Content-Type" content="...; charset=...">` declaration.
/// We only look at ASCII bytes; valid meta-charset declarations are always
/// ASCII regardless of the page's actual encoding.
fn sniff_meta_charset(bytes: &[u8]) -> Option<&'static Encoding> {
    let prefix_len = bytes.len().min(1024);
    let prefix = &bytes[..prefix_len];
    // Lossy is fine: any meta charset attribute is ASCII, even on a non-UTF-8 page.
    let s = String::from_utf8_lossy(prefix).to_ascii_lowercase();

    // Look for any `<meta ... charset=...>` pattern in the first 1KB. We
    // intentionally accept both the modern shorthand (`<meta charset=gbk>`)
    // and the legacy http-equiv form (`<meta http-equiv="content-type" content="text/html; charset=gbk">`).
    let mut pos = 0;
    while let Some(meta_start) = s[pos..].find("<meta") {
        let abs = pos + meta_start;
        // Find the closing `>` for this meta tag.
        let end = s[abs..].find('>').map(|e| abs + e).unwrap_or(s.len());
        let tag = &s[abs..end];

        if let Some(charset_pos) = tag.find("charset") {
            let after = &tag[charset_pos + "charset".len()..];
            let after = after.trim_start();
            if let Some(eq_rest) = after.strip_prefix('=') {
                let value = eq_rest
                    .trim_start()
                    .trim_start_matches(|c: char| c == '"' || c == '\'')
                    .split(|c: char| c == '"' || c == '\'' || c == ';' || c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("");
                if !value.is_empty() {
                    if let Some(enc) = Encoding::for_label(value.as_bytes()) {
                        return Some(enc);
                    }
                }
            }
        }

        pos = end + 1;
        if pos >= s.len() {
            break;
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_charset_wins() {
        let bytes = b"<html><head><meta charset=\"utf-8\"></head><body></body></html>";
        let (enc, source) = detect_encoding(bytes, Some("text/html; charset=gbk"));
        assert_eq!(enc.name(), "GBK");
        assert_eq!(source, "content-type");
    }

    #[test]
    fn content_type_quoted_charset_is_parsed() {
        let (enc, _) = detect_encoding(b"", Some("text/html; charset=\"Shift_JIS\""));
        assert_eq!(enc.name(), "Shift_JIS");
    }

    #[test]
    fn meta_charset_used_when_header_missing() {
        let bytes = b"<!doctype html><html><head><meta charset=\"big5\"></head></html>";
        let (enc, source) = detect_encoding(bytes, None);
        assert_eq!(enc.name(), "Big5");
        assert_eq!(source, "meta-charset");
    }

    #[test]
    fn meta_http_equiv_charset_is_recognized() {
        let bytes = b"<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=EUC-KR\"></head></html>";
        let (enc, _) = detect_encoding(bytes, None);
        assert_eq!(enc.name(), "EUC-KR");
    }

    #[test]
    fn no_charset_anywhere_falls_back_to_utf8() {
        let bytes = b"<html><body>hello</body></html>";
        let (enc, source) = detect_encoding(bytes, None);
        assert_eq!(enc.name(), "UTF-8");
        assert_eq!(source, "default-utf8");
    }

    #[test]
    fn decode_response_gbk_bytes_roundtrip() {
        // "你好" (ni hao) encoded as GBK = C4 E3 BA C3
        let bytes: &[u8] = &[0xC4, 0xE3, 0xBA, 0xC3];
        let s = decode_response(bytes, Some("text/html; charset=gbk"));
        assert_eq!(s, "你好");
    }

    #[test]
    fn decode_non_html_skips_meta_sniff() {
        // A JS body that happens to contain a string `<meta charset="gbk">`
        // must NOT be decoded as GBK — non-HTML resources only honor the
        // HTTP header.
        let bytes = br#"var x = '<meta charset="gbk">'; // not the real charset"#;
        let s = decode_non_html(bytes, Some("application/javascript"));
        assert!(s.contains("<meta charset="));
    }

    #[test]
    fn meta_sniff_only_scans_first_1kb() {
        let mut bytes = vec![b' '; 2048];
        bytes.extend_from_slice(b"<meta charset=\"gbk\">");
        let (enc, _) = detect_encoding(&bytes, None);
        // Beyond 1KB: ignored, fall back to UTF-8.
        assert_eq!(enc.name(), "UTF-8");
    }
}
