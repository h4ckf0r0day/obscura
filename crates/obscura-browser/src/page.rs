use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use obscura_dom::{parse_html, DomTree};
use obscura_js::frame::FrameRealm;
use obscura_js::runtime::ObscuraJsRuntime;
use obscura_net::{
    CallbackRegistry, ObscuraHttpClient, ObscuraNetError, RequestCallback, ResourceRequest,
    ResourceType, Response, ResponseCallback,
};
use url::Url;

use crate::context::BrowserContext;
use crate::lifecycle::LifecycleState;

/// Parse `OBSCURA_GEOLOCATION="lat,lon"` for the navigator.geolocation shim.
/// Returns None when unset or malformed, leaving the built-in default in place.
/// Lets a deployment align the reported coordinates with the region its exit IP
/// resolves to, so timezone and location stay consistent (issue #228).
fn env_geolocation() -> Option<(f64, f64)> {
    let raw = std::env::var("OBSCURA_GEOLOCATION").ok()?;
    let (lat, lon) = raw.split_once(',')?;
    let lat: f64 = lat.trim().parse().ok()?;
    let lon: f64 = lon.trim().parse().ok()?;
    let valid = lat.is_finite()
        && lon.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon);
    valid.then_some((lat, lon))
}

fn decode_data_uri(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("data:")?;
    let comma = rest.find(',')?;
    let meta = &rest[..comma];
    let payload = &rest[comma + 1..];
    if meta.split(';').any(|t| t.eq_ignore_ascii_case("base64")) {
        let cleaned: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
        BASE64.decode(cleaned).ok()
    } else {
        Some(percent_decode(payload))
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = hex_val(b[i + 1]);
            let lo = hex_val(b[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Truncate `s` to at most `max` bytes without splitting a UTF-8 character.
/// `&s[..max]` panics if `max` lands inside a multi-byte char; the evaluated
/// expression logged below is caller-controlled, so slice it safely.
/// (`str::floor_char_boundary` would do this but is still unstable.)
fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(feature = "render")]
fn remaining_settle_resource_warmup_ms(
    max_ms: u64,
    elapsed: std::time::Duration,
    configured_ms: u64,
) -> u64 {
    std::time::Duration::from_millis(max_ms)
        .checked_sub(elapsed)
        .map(|remaining| {
            (remaining.as_millis().min(u128::from(u64::MAX)) as u64).min(configured_ms)
        })
        .unwrap_or(0)
}

#[cfg(feature = "stealth")]
use obscura_net::StealthHttpClient;

/// Returns true when a JS-initiated navigation would step from a
/// non-file scheme into a file: URL. We treat that move as an SOP
/// violation because the existing realm survives the navigation and
/// can read the new document's body.
fn cross_scheme_to_file(from: &str, to: &str) -> bool {
    let to_is_file = Url::parse(to)
        .map(|u| u.scheme().eq_ignore_ascii_case("file"))
        .unwrap_or(false);
    if !to_is_file {
        return false;
    }
    Url::parse(from)
        .map(|u| !u.scheme().eq_ignore_ascii_case("file"))
        .unwrap_or(true)
}

/// Sub-resource fetch policy. http(s) is always fine; data: is allowed
/// because the bytes are inline in the URI (no network fetch, no SSRF);
/// file: is only allowed when the page itself was loaded from file:;
/// everything else (javascript:, chrome:, etc) is blocked.
/// Real Chrome allows data: subresources by default; Instagram and most
/// Meta properties depend on this for their inline bootstrap scripts.
fn subresource_allowed(page_url: Option<&Url>, resource: &str) -> bool {
    let Ok(target) = Url::parse(resource) else {
        return false;
    };
    let scheme = target.scheme().to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" | "data" => true,
        "file" => page_url
            .map(|u| u.scheme().eq_ignore_ascii_case("file"))
            .unwrap_or(false),
        _ => false,
    }
}

/// Compute the default `strict-origin-when-cross-origin` referrer value used
/// for a document-initiated navigation. Direct navigations bypass this helper
/// and use an empty referrer. Referrer-Policy overrides are not yet plumbed
/// through the navigation request.
fn navigation_referrer(source: &Url, target: &Url) -> String {
    if !matches!(source.scheme(), "http" | "https")
        || !matches!(target.scheme(), "http" | "https")
        || (source.scheme() == "https" && target.scheme() == "http")
    {
        return String::new();
    }

    if source.origin() == target.origin() {
        let mut sanitized = source.clone();
        sanitized.set_fragment(None);
        let _ = sanitized.set_username("");
        let _ = sanitized.set_password(None);
        return sanitized.to_string();
    }

    let mut origin = source.origin().ascii_serialization();
    origin.push('/');
    origin
}

/// Escape a value for safe inclusion inside a JavaScript template
/// literal. The previous implementation only escaped `\`, `` ` `` and
/// `${`; that left U+2028 / U+2029 (the JS-specific line terminators)
/// and other control characters as breakout vectors. Done at the
/// callsite means future tweaks come back to one function.
fn escape_for_js_template_literal(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '$' => out.push_str("\\$"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            '\u{0000}' => out.push_str("\\0"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct NetworkEvent {
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub resource_type: String,
    pub status: u16,
    pub headers: std::collections::HashMap<String, String>,
    pub response_headers: Arc<std::collections::HashMap<String, String>>,
    pub body_size: usize,
    pub timestamp: f64,
}

#[derive(Debug, Clone)]
pub struct StoredResponseBody {
    pub body: String,
    pub base64_encoded: bool,
}

#[derive(Clone, Copy)]
struct DeviceMetricsBaseline {
    viewport: (f32, f32),
    device_scale_factor: f32,
}

pub struct Page {
    pub id: String,
    pub frame_id: String,
    pub url: Option<Url>,
    pub dom: Option<DomTree>,
    /// Live child frame realms, in creation order. Declared before `js` on
    /// purpose: a realm holds a V8 handle into that isolate, and fields drop in
    /// declaration order, so the frames must go first.
    pub frames: Vec<FrameRealm>,
    pub js: Option<ObscuraJsRuntime>,
    pub lifecycle: LifecycleState,
    pub http_client: Arc<ObscuraHttpClient>,
    pub context: Arc<BrowserContext>,
    pub title: String,
    /// Source document URL for the current document. This is deliberately
    /// separate from `url`: direct automation navigations have no referrer,
    /// while a navigation requested by page script uses the previous document.
    pub referrer: String,
    /// CSS viewport used by responsive page JavaScript and CDP screenshots.
    /// The physical `screen` fingerprint remains independent.
    pub viewport: (f32, f32),
    /// Optional CDP physical-screen override. This is separate from the CSS
    /// viewport and survives navigation, matching device-metrics emulation.
    screen_size_override: Option<(f32, f32)>,
    screen_metrics_emulated: bool,
    /// Metrics captured when CDP device emulation is first enabled. Chromium
    /// keeps this baseline across subsequent override calls and restores it
    /// only when the override is cleared.
    device_metrics_baseline: Option<DeviceMetricsBaseline>,
    /// Output device pixels per CSS pixel for CDP surface capture. Layout and
    /// CSSOM stay in CSS pixels; Emulation.setDeviceMetricsOverride owns this
    /// independent raster scale.
    pub device_scale_factor: f32,
    /// DevTools override for the compositor's base surface. It is page-owned,
    /// so it survives document navigation without leaking to other targets.
    default_background_color_override: Option<[u8; 4]>,
    /// WHATWG canonical name of the current document's character encoding
    /// (e.g. "UTF-8", "EUC-JP"), detected when the response body is decoded.
    /// Exposed to JS as `document.characterSet` and used for the URL query
    /// encoding override on `<a>`/`<area>` hrefs in legacy-charset documents.
    pub encoding: String,
    /// Monotonic origin for the current document's CSS animation timeline.
    /// It is reset once author styles are installed, so stylesheet download
    /// latency does not incorrectly advance newly-created animations.
    document_timeline_origin: std::time::Instant,
    /// Optional page-scoped ceiling for an end-to-end navigation. Automation
    /// frontends set this from their request timeout so a caller asking for a
    /// 50-second navigation is not silently cut off by the process default.
    /// Pages without an override retain the environment-configurable default.
    navigation_timeout: Option<std::time::Duration>,
    /// Optional per-page cap. Pages without a value keep the default.
    navigation_chain_limit: Option<usize>,
    /// Navigation history for Page.getNavigationHistory / navigateToHistoryEntry.
    /// Entries are URLs in visit order; `history_index` is the current position.
    /// Pushed on every successful navigation; truncated on goBack -> new nav.
    pub history: Vec<String>,
    pub history_index: usize,
    pub network_events: Vec<NetworkEvent>,
    response_bodies: std::collections::HashMap<String, StoredResponseBody>,
    response_body_order: std::collections::VecDeque<String>,
    network_event_counter: u32,
    pub intercept_enabled: bool,
    pub intercept_block_patterns: Vec<String>,
    pub blocked_url_patterns: Vec<String>,
    intercept_tx: Option<tokio::sync::mpsc::UnboundedSender<obscura_js::ops::InterceptedRequest>>,
    // Scripts to execute in the page's JS context BEFORE any of the page's
    // own scripts run — the CDP `Page.addScriptToEvaluateOnNewDocument`
    // contract. Includes `Runtime.addBinding` shims so puppeteer's
    // `exposeFunction` bindings exist before inline `<script>` tags execute.
    preload_scripts: Vec<String>,
    /// Whether at least one attached CDP session enabled the Runtime domain.
    /// Page-owned so the subscription survives replacement of the JS runtime
    /// during navigation.
    runtime_events_enabled: std::cell::Cell<bool>,
    /// Document-owned HTML script preparation flags saved while the V8 realm
    /// is suspended for CDP/MCP tab switching.  These are restored only when
    /// the same surviving DomTree is resumed; navigation clears them.
    suspended_started_script_ids: Vec<u32>,
    /// Passive on_request/on_response callbacks, scoped to this page (issue
    /// #408): they fire only for requests this page drives and die with it.
    /// Arc because the JS runtime state holds a second handle for fetch()/XHR.
    callbacks: Arc<CallbackRegistry>,
    #[cfg(feature = "stealth")]
    pub stealth_client: Option<Arc<StealthHttpClient>>,
}

const MAX_STYLESHEET_IMPORT_DEPTH: u8 = 4;
const MAX_STYLESHEET_RESOURCES: usize = 128;
const DEFAULT_NAVIGATION_TIMEOUT_MS: u64 = 30_000;

/// How many child frame realms one document may hold at once.
///
/// Real pages use a handful; the cap exists so a page that creates iframes in a
/// loop cannot make the engine hold an unbounded number of contexts and DOM
/// trees. Frames are released when the document is replaced.
fn max_live_frames() -> usize {
    std::env::var("OBSCURA_MAX_LIVE_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(64)
}

/// The first navigation counts. The low default stops a page that resets
/// `location` on every load.
const DEFAULT_NAVIGATION_CHAIN_LIMIT: usize = 10;

fn default_navigation_timeout() -> std::time::Duration {
    navigation_timeout_from_env_value(std::env::var("OBSCURA_NAV_TIMEOUT_MS").ok().as_deref())
}

fn navigation_timeout_from_env_value(value: Option<&str>) -> std::time::Duration {
    let milliseconds = value
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_NAVIGATION_TIMEOUT_MS);
    std::time::Duration::from_millis(milliseconds)
}

fn default_navigation_chain_limit() -> usize {
    navigation_chain_limit_from_env_value(std::env::var("OBSCURA_NAV_CHAIN_LIMIT").ok().as_deref())
}

fn navigation_chain_limit_from_env_value(value: Option<&str>) -> usize {
    // Only an unreadable value falls back to the default, so
    // `OBSCURA_NAV_CHAIN_LIMIT=0` and `set_navigation_chain_limit(0)` agree.
    value
        .and_then(|value| value.parse::<usize>().ok())
        .map(|limit| limit.max(1))
        .unwrap_or(DEFAULT_NAVIGATION_CHAIN_LIMIT)
}

fn duration_millis_u64(duration: std::time::Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[derive(Clone)]
struct LoadedStylesheet {
    response_url: Url,
    imports: Vec<StylesheetImport>,
    rules: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StylesheetImport {
    url: String,
    media: Option<String>,
}

#[derive(Clone, Copy)]
enum AuthorStylesheetTarget {
    Linked(usize),
    InlineImport(usize),
}

fn canonical_stylesheet_url(mut url: Url) -> (String, Url) {
    url.set_fragment(None);
    (url.to_string(), url)
}

/// Expand a cached stylesheet graph in CSS cascade order. Network deduplication
/// is separate from expansion: a shared import is downloaded once but expanded
/// at each import position, while the active stack cuts cycles.
fn materialize_stylesheet_graph(
    key: &str,
    sheets: &std::collections::HashMap<String, LoadedStylesheet>,
    aliases: &std::collections::HashMap<String, String>,
    active: &mut std::collections::HashSet<String>,
) -> Option<String> {
    let actual_key = aliases.get(key).map(String::as_str).unwrap_or(key);
    if !active.insert(actual_key.to_string()) {
        return None;
    }
    let Some(sheet) = sheets.get(actual_key).cloned() else {
        active.remove(actual_key);
        return None;
    };

    let mut output = String::new();
    for import in &sheet.imports {
        let Ok(import_url) = sheet.response_url.join(&import.url) else {
            continue;
        };
        let (import_key, _) = canonical_stylesheet_url(import_url);
        if let Some(imported) = materialize_stylesheet_graph(&import_key, sheets, aliases, active) {
            if let Some(media) = import.media.as_deref() {
                output.push_str("@media ");
                output.push_str(media);
                output.push_str(" {\n");
                output.push_str(&imported);
                output.push_str("\n}\n");
            } else {
                output.push_str(&imported);
                output.push('\n');
            }
        }
    }
    output.push_str(&rebase_css_urls(&sheet.rules, &sheet.response_url));
    active.remove(actual_key);
    Some(output)
}

/// Preserve the URL base of a fetched stylesheet after it is materialized as
/// inline CSS. Relative `url(...)` values resolve against the stylesheet's
/// URL in browsers, not the document URL; failing to rebase them drops common
/// background, mask, cursor, and font assets from nested theme directories.
fn rebase_css_urls(css: &str, base: &url::Url) -> String {
    let mut out = String::with_capacity(css.len());
    let mut index = 0usize;
    while index < css.len() {
        let rest = &css[index..];
        if rest.starts_with("/*") {
            if let Some(end) = rest[2..].find("*/") {
                let length = end + 4;
                out.push_str(&rest[..length]);
                index += length;
            } else {
                out.push_str(rest);
                break;
            }
            continue;
        }
        let Some(first) = rest.chars().next() else {
            break;
        };
        if first == '"' || first == '\'' {
            let quote = first;
            let mut escaped = false;
            let mut length = quote.len_utf8();
            for ch in rest[quote.len_utf8()..].chars() {
                length += ch.len_utf8();
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == quote {
                    break;
                }
            }
            out.push_str(&rest[..length]);
            index += length;
            continue;
        }
        let is_url = rest
            .get(..4)
            .map_or(false, |prefix| prefix.eq_ignore_ascii_case("url("));
        if !is_url {
            out.push(first);
            index += first.len_utf8();
            continue;
        }

        let mut quote = None;
        let mut escaped = false;
        let mut end = None;
        for (offset, ch) in rest[4..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            match quote {
                Some(open) if ch == open => quote = None,
                Some(_) => {}
                None if ch == '"' || ch == '\'' => quote = Some(ch),
                None if ch == ')' => {
                    end = Some(4 + offset);
                    break;
                }
                None => {}
            }
        }
        let Some(end) = end else {
            out.push_str(rest);
            break;
        };
        let raw = rest[4..end].trim();
        let value = if raw.len() >= 2
            && ((raw.starts_with('"') && raw.ends_with('"'))
                || (raw.starts_with('\'') && raw.ends_with('\'')))
        {
            &raw[1..raw.len() - 1]
        } else {
            raw
        };
        let resolved = if value.is_empty()
            || value.starts_with('#')
            || value.contains("var(")
            || url::Url::parse(value).is_ok()
        {
            None
        } else {
            base.join(value).ok().map(|url| url.to_string())
        };
        if let Some(resolved) = resolved {
            out.push_str("url(\"");
            for ch in resolved.chars() {
                if ch == '\\' || ch == '"' {
                    out.push('\\');
                }
                out.push(ch);
            }
            out.push_str("\")");
        } else {
            out.push_str(&rest[..=end]);
        }
        index += end + 1;
    }
    out
}

/// Extract network-backed `url(...)` assets while respecting CSS comments and
/// strings. Linked sheets have already been rebased before materialization;
/// inline declarations are resolved against the document base here.
fn css_resource_urls(css: &str, base: &url::Url) -> Vec<String> {
    let mut urls = Vec::new();
    let mut index = 0usize;
    while index < css.len() {
        let rest = &css[index..];
        if rest.starts_with("/*") {
            if let Some(end) = rest[2..].find("*/") {
                index += end + 4;
            } else {
                break;
            }
            continue;
        }
        // `@import url(...)` is a stylesheet dependency, not a paint asset.
        // It is fetched by the bounded stylesheet graph above. Letting the
        // generic image/font warmup rediscover it issues a second request with
        // the wrong ResourceType::Image classification.
        if let Some(length) = css_import_rule_len(rest) {
            index += length;
            continue;
        }
        // A `@font-face` `src` list is a priority order, not a set of
        // resources. Scanning it generically warms every entry of every
        // descriptor; the renderer considers far fewer. `css_font_face_rule`
        // collects the ones it will consider and reports the whole block as
        // consumed, the same shape as the `@import` skip above.
        if let Some((length, sources)) = css_font_face_rule(rest, base) {
            urls.extend(sources);
            index += length;
            continue;
        }
        let Some(first) = rest.chars().next() else {
            break;
        };
        if first == '"' || first == '\'' {
            let quote = first;
            let mut escaped = false;
            let mut length = quote.len_utf8();
            for ch in rest[quote.len_utf8()..].chars() {
                length += ch.len_utf8();
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == quote {
                    break;
                }
            }
            index += length;
            continue;
        }
        if !rest
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("url("))
        {
            index += first.len_utf8();
            continue;
        }
        let mut quote = None;
        let mut escaped = false;
        let mut end = None;
        for (offset, ch) in rest[4..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            match quote {
                Some(open) if ch == open => quote = None,
                Some(_) => {}
                None if ch == '"' || ch == '\'' => quote = Some(ch),
                None if ch == ')' => {
                    end = Some(4 + offset);
                    break;
                }
                None => {}
            }
        }
        let Some(end) = end else { break };
        push_css_url(&rest[4..end], base, &mut urls);
        index += end + 1;
    }
    urls
}

/// Record one `url(...)` value when it names a network resource.
///
/// Shared by the generic scan and the `@font-face` path so the two cannot
/// disagree about quoting, fragments, `data:` or an unresolved `var()`.
fn push_css_url(raw: &str, base: &url::Url, urls: &mut Vec<String>) {
    let raw = raw.trim();
    let value = if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
    {
        &raw[1..raw.len() - 1]
    } else {
        raw
    };
    if value.is_empty()
        || value.starts_with('#')
        || value.starts_with("data:")
        || value.contains("var(")
    {
        return;
    }
    if let Ok(mut url) = base.join(value) {
        url.set_fragment(None);
        if matches!(url.scheme(), "http" | "https") {
            urls.push(url.to_string());
        }
    }
}

/// Return the byte length of a leading `@font-face` block, together with the
/// sources the renderer will actually consider, in source order.
///
/// A `src` list is a priority order, not a set. Both rules here are taken from
/// the layer this warms, `obscura-render`: `font_face_declaration` ends in
/// `.last()`, so a rule carrying several `src` descriptors uses the final one,
/// which is the cascade and what the `src: url(.eot); src: url(...)` idiom
/// relies on; and `font_source_may_be_supported` drops `.eot` and `.svg` after
/// stripping the query and fragment. Warming anything else fetches bytes the
/// renderer has already ruled out.
///
/// A malformed block is left to the normal scanner, so this cannot swallow the
/// rules that follow it.
fn css_font_face_rule(css: &str, base: &url::Url) -> Option<(usize, Vec<String>)> {
    if !css.get(..10)?.eq_ignore_ascii_case("@font-face") {
        return None;
    }
    let open_relative = css[10..].find('{')?;
    if !css[10..10 + open_relative].trim().is_empty() {
        return None;
    }
    let open = 10 + open_relative;
    let close = open + css_block_end(&css[open..])?;

    let mut urls = Vec::new();
    if let Some(src) = css_last_declaration(&css[open + 1..close], "src") {
        for value in css_url_values(src) {
            if font_source_is_decodable(value) {
                push_css_url(value, base, &mut urls);
            }
        }
    }
    Some((close + 1, urls))
}

/// Byte offset of the brace closing the block that `css` opens, or `None` when
/// the block is unterminated. Braces inside strings do not count.
fn css_block_end(css: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (offset, ch) in css.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(open) = quote {
            if ch == open {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// Value of the last declaration named `name` in a declaration block. Last
/// rather than first, because that is what the cascade resolves to and what
/// `obscura-render` reads.
fn css_last_declaration<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let mut found = None;
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut boundaries: Vec<usize> = Vec::new();
    for (offset, ch) in block.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(open) = quote {
            if ch == open {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => boundaries.push(offset),
            _ => {}
        }
    }
    boundaries.push(block.len());
    for end in boundaries {
        let declaration = &block[start..end];
        start = (end + 1).min(block.len());
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        if property.trim().eq_ignore_ascii_case(name) {
            found = Some(value.trim());
        }
    }
    found
}

/// The `url(...)` values of one declaration, unquoted, in source order.
fn css_url_values(value: &str) -> Vec<&str> {
    let lower = value.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(relative) = lower[cursor..].find("url(") {
        let start = cursor + relative + 4;
        let Some(end_relative) = value[start..].find(')') else {
            break;
        };
        let end = start + end_relative;
        let raw = value[start..end].trim();
        let unquoted = if raw.len() >= 2
            && ((raw.starts_with('"') && raw.ends_with('"'))
                || (raw.starts_with('\'') && raw.ends_with('\'')))
        {
            &raw[1..raw.len() - 1]
        } else {
            raw
        };
        if !unquoted.is_empty() {
            out.push(unquoted);
        }
        cursor = end + 1;
    }
    out
}

/// Whether the renderer can decode a font source, by the same extension rule
/// `obscura-render` applies before it will even consider one.
fn font_source_is_decodable(src: &str) -> bool {
    let path = src
        .split(['?', '#'])
        .next()
        .unwrap_or(src)
        .to_ascii_lowercase();
    !path.ends_with(".eot") && !path.ends_with(".svg")
}

/// Return the byte length of a leading CSS `@import` rule, including its
/// terminating semicolon. Semicolons inside quoted URLs, comments, or `url()`
/// parentheses do not end the rule. A malformed import is left to the normal
/// scanner so this helper cannot swallow following declarations.
fn css_import_rule_len(css: &str) -> Option<usize> {
    let prefix = css.get(..7)?;
    if !prefix.eq_ignore_ascii_case("@import") {
        return None;
    }
    if css[7..]
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return None;
    }

    let bytes = css.as_bytes();
    let mut index = 7usize;
    let mut quote = None;
    let mut escaped = false;
    let mut paren_depth = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == open {
                quote = None;
            }
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let Some(end) = css[index + 2..].find("*/") else {
                return None;
            };
            index += end + 4;
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            b';' if paren_depth == 0 => return Some(index + 1),
            b'{' if paren_depth == 0 => return None,
            _ => {}
        }
        index += 1;
    }
    None
}

fn render_resource_type(url: &url::Url) -> ResourceType {
    let path = url.path().to_ascii_lowercase();
    if [".woff", ".woff2", ".ttf", ".otf", ".eot"]
        .iter()
        .any(|extension| path.ends_with(extension))
    {
        ResourceType::Font
    } else {
        ResourceType::Image
    }
}

/// Pull leading `@import` rules out of a stylesheet. Returns each import target
/// URL with its optional media condition plus the CSS with those `@import`
/// statements removed. Browsers fetch media-gated imports even when they do
/// not match the current screen; preserving the condition lets the same bytes
/// participate in a later PDF print cascade. Handles `@import "x.css";`,
/// `@import url("x.css");`, `@import url(x.css);` and an optional trailing
/// media query.
fn split_css_imports(css: &str) -> (Vec<StylesheetImport>, String) {
    let mut urls = Vec::new();
    let mut stripped = String::with_capacity(css.len());
    let mut rest = css;
    loop {
        let Some(pos) = rest.find("@import") else {
            stripped.push_str(rest);
            break;
        };
        // Real sheets place `@import` at the top (after an optional @charset), so
        // scanning for it anywhere is safe in practice and tolerates minified
        // whitespace. Text before this match carries through unchanged.
        stripped.push_str(&rest[..pos]);
        let after = &rest[pos + "@import".len()..];
        let Some(semi) = after.find(';') else {
            // Malformed; keep the remainder verbatim.
            stripped.push_str(&rest[pos..]);
            break;
        };
        let stmt = &after[..semi];
        if let Some(target) = parse_import_url(stmt) {
            urls.push(target);
        } else {
            // Could not parse a URL; preserve the statement so we don't lose it.
            stripped.push_str("@import");
            stripped.push_str(&after[..=semi]);
        }
        rest = &after[semi + 1..];
    }
    (urls, stripped)
}

/// Extract the URL and optional trailing media query from an `@import`
/// statement body (the text between `@import` and `;`).
fn parse_import_url(stmt: &str) -> Option<StylesheetImport> {
    let s = stmt.trim();
    let is_url_fn = s.len() >= 4 && s[..4].eq_ignore_ascii_case("url(");
    let (url, media) = if is_url_fn {
        let rest = &s[4..];
        let end = rest.find(')')?;
        let inner = rest[..end].trim().trim_matches(|c| c == '"' || c == '\'');
        (inner.to_string(), rest[end + 1..].trim())
    } else {
        let quote = s.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let rest = &s[1..];
        let end = rest.find(quote)?;
        (rest[..end].to_string(), rest[end + 1..].trim())
    };
    if url.is_empty() {
        return None;
    }
    Some(StylesheetImport {
        url,
        media: (!media.is_empty()).then(|| media.to_string()),
    })
}

/// Materialize a fetched linked sheet immediately after its source `<link>`.
///
/// Keeping each sheet at its document position matters when linked and inline
/// author sheets are interleaved. Appending one aggregate `<style>` to `<head>`
/// makes every external rule later than every inline rule, which changes the
/// CSS cascade even when the external fetches themselves complete in order.
/// The synthetic style retains the link's effective media query so the same
/// fetched bytes can enter print layout without leaking into screen layout.
fn materialize_linked_stylesheet_script(link_index: usize, css: &str) -> String {
    let escaped_css = escape_for_js_template_literal(css);
    format!(
        r#"(function() {{
            var links = document.querySelectorAll('link[rel~="stylesheet"]');
            var link = links[{link_index}];
            if (!link || !link.parentNode) return;
            var style = null;
            function effectiveMedia() {{
                // Until the generic Element shim reflects HTMLLinkElement.media,
                // `this.media = "all"` creates an own property while the parsed
                // media="print" attribute remains unchanged.
                if (Object.prototype.hasOwnProperty.call(link, 'media')) {{
                    return String(link.media || '');
                }}
                return link.getAttribute('media') || '';
            }}
            function syncSheet() {{
                if (!style) {{
                    style = document.createElement('style');
                    style.setAttribute('data-obscura-external-stylesheets', '');
                    style.textContent = `{escaped_css}`;
                    globalThis.__obscura_registerLinkedStylesheet(link, style);
                }}
                var enabled = link.parentNode
                    && !link.disabled
                    && !link.hasAttribute('disabled');
                if (!enabled) {{
                    if (style && style.parentNode) style.parentNode.removeChild(style);
                    return;
                }}
                var media = effectiveMedia().trim();
                if (media) style.setAttribute('media', media);
                else style.removeAttribute('media');
                if (!style.parentNode) {{
                    link.parentNode.insertBefore(style, link.nextSibling);
                }}
            }}

            // A non-matching sheet still loads and fires its event. Its handler
            // may then make the sheet applicable (the common
            // media=print/onload="this.media='all'" async-CSS pattern).
            syncSheet();
            try {{ link.dispatchEvent(new Event('load')); }}
            finally {{ syncSheet(); }}
        }})()"#
    )
}

/// Materialize one fetched `@import` immediately before its source inline
/// `<style>`. Imported rules precede the importing sheet in the author cascade,
/// and inherit the source sheet's own media condition in addition to the
/// import rule's media wrapper.
fn materialize_inline_import_script(style_index: usize, css: &str) -> String {
    let escaped_css = escape_for_js_template_literal(css);
    format!(
        r#"(function() {{
            var styles = document.querySelectorAll('style');
            var source = null;
            var authorIndex = -1;
            for (var i = 0; i < styles.length; i++) {{
                var candidate = styles[i];
                if (candidate.hasAttribute('data-obscura-external-stylesheets')
                    || candidate.hasAttribute('data-obscura-inline-import')) continue;
                authorIndex++;
                if (authorIndex === {style_index}) {{ source = candidate; break; }}
            }}
            if (!source || !source.parentNode) return;
            var imported = document.createElement('style');
            imported.setAttribute('data-obscura-inline-import', '');
            var media = source.getAttribute('media') || '';
            if (media.trim()) imported.setAttribute('media', media);
            imported.textContent = `{escaped_css}`;
            source.parentNode.insertBefore(imported, source);
        }})()"#
    )
}

/// Discover linked author sheets in document order.
///
/// Media queries control whether a loaded sheet participates in the cascade;
/// they do not suppress its fetch or `load` event. Keep the index among all
/// stylesheet links so the materialization script addresses the same node.
fn linked_stylesheet_requests(dom: &DomTree) -> Vec<(usize, String)> {
    let link_ids = dom
        .query_selector_all("link[rel~=\"stylesheet\"]")
        .unwrap_or_default();
    let mut links = Vec::new();
    for (link_index, lid) in link_ids.into_iter().enumerate() {
        if let Some(node) = dom.get_node(lid) {
            // Disabled alternate sheets remain dormant until script enables
            // them. Media-gated sheets are different: they still load.
            if node.get_attribute("disabled").is_some() {
                continue;
            }
            if let Some(href) = node.get_attribute("href") {
                links.push((link_index, href.to_string()));
            }
        }
    }
    links
}

/// Discover fetchable `@import` rules in inline author sheets. The source
/// index excludes Obscura's own materialized sheets so it remains stable while
/// imports are inserted before their source nodes.
fn inline_stylesheet_import_requests(dom: &DomTree) -> Vec<(usize, StylesheetImport)> {
    let style_ids = dom.query_selector_all("style").unwrap_or_default();
    let mut imports = Vec::new();
    let mut author_index = 0usize;
    for style_id in style_ids {
        let Some(node) = dom.get_node(style_id) else {
            continue;
        };
        if node
            .get_attribute("data-obscura-external-stylesheets")
            .is_some()
            || node.get_attribute("data-obscura-inline-import").is_some()
        {
            continue;
        }
        let (style_imports, _) = split_css_imports(&dom.text_content(style_id));
        imports.extend(
            style_imports
                .into_iter()
                .map(|import| (author_index, import)),
        );
        author_index += 1;
    }
    imports
}

impl Page {
    pub fn new(id: String, context: Arc<BrowserContext>) -> Self {
        let http_client = context.http_client.clone();
        // Chromium convention: the main frame's frameId == the targetId.
        // Playwright's frame manager looks up the main frame by targetId
        // (via target._targetInfo.targetId), so any divergence here makes
        // Page.getFrameTree return a frame the client cannot match,
        // triggering a Target.closeTarget and "Frame has been detached".
        let frame_id = id.clone();
        #[cfg(feature = "stealth")]
        let stealth_client = if context.stealth {
            // The wreq client backing StealthHttpClient does not speak SOCKS5.
            // Callers must validate the proxy scheme up front and fail loudly
            // (see obscura-cli) rather than silently rewriting socks5:// to
            // http://, which only works when the upstream happens to be a
            // Clash-style mixed-mode proxy and breaks plain SOCKS5 servers
            // like `ssh -ND` (#160).
            Some(Arc::new(StealthHttpClient::with_proxy(
                context.cookie_jar.clone(),
                context.proxy_url.as_deref(),
            )))
        } else {
            None
        };

        Page {
            id,
            frame_id,
            url: None,
            dom: None,
            frames: Vec::new(),
            js: None,
            lifecycle: LifecycleState::Idle,
            http_client,
            context,
            title: String::new(),
            referrer: String::new(),
            viewport: (1280.0, 720.0),
            screen_size_override: None,
            screen_metrics_emulated: false,
            device_metrics_baseline: None,
            device_scale_factor: 1.0,
            default_background_color_override: None,
            encoding: "UTF-8".to_string(),
            document_timeline_origin: std::time::Instant::now(),
            navigation_timeout: None,
            navigation_chain_limit: None,
            history: Vec::new(),
            history_index: 0,
            network_events: Vec::new(),
            response_bodies: std::collections::HashMap::new(),
            response_body_order: std::collections::VecDeque::new(),
            network_event_counter: 0,
            intercept_enabled: false,
            intercept_block_patterns: Vec::new(),
            blocked_url_patterns: Vec::new(),
            intercept_tx: None,
            preload_scripts: Vec::new(),
            runtime_events_enabled: std::cell::Cell::new(false),
            suspended_started_script_ids: Vec::new(),
            callbacks: Arc::new(CallbackRegistry::new()),
            #[cfg(feature = "stealth")]
            stealth_client,
        }
    }

    /// Set the end-to-end navigation deadline for this page. This page-scoped
    /// value takes precedence over `OBSCURA_NAV_TIMEOUT_MS`; callers that do
    /// not set it retain the existing environment-configurable 30s default.
    pub fn set_navigation_timeout(&mut self, timeout: std::time::Duration) {
        self.navigation_timeout = Some(timeout);
    }

    /// Return the effective end-to-end navigation deadline for this page.
    pub fn navigation_timeout(&self) -> std::time::Duration {
        self.navigation_timeout
            .unwrap_or_else(default_navigation_timeout)
    }

    /// Takes precedence over `OBSCURA_NAV_CHAIN_LIMIT`. A limit of 0 is
    /// raised to 1, which would otherwise report success having loaded
    /// nothing.
    pub fn set_navigation_chain_limit(&mut self, limit: usize) {
        self.navigation_chain_limit = Some(limit.max(1));
    }

    pub fn navigation_chain_limit(&self) -> usize {
        self.navigation_chain_limit
            .unwrap_or_else(default_navigation_chain_limit)
    }

    fn should_block_url(&self, url: &str) -> bool {
        for pattern in &self.blocked_url_patterns {
            if url_matches_cdp_pattern(pattern, url) {
                return true;
            }
        }
        if self.intercept_enabled {
            for pattern in &self.intercept_block_patterns {
                if url_matches_cdp_pattern(pattern, url) {
                    return true;
                }
            }
        }
        false
    }

    /// Gives every frame document the page has fetched a realm of its own, and
    /// runs the scripts that came with it (issue #600).
    ///
    /// Building a realm needs the whole runtime, which an op cannot reach, so
    /// the JS side queues the fetched document and this drains the queue between
    /// event loop turns. Reports whether anything was attached, so a caller can
    /// settle and come back for frames that these frames created.
    async fn attach_pending_frames(&mut self) -> bool {
        let pending = match self.js.as_ref() {
            Some(js) => js.take_pending_frames(),
            None => return false,
        };
        if pending.is_empty() {
            return false;
        }

        for frame in pending {
            // A realm is a live v8::Context plus a DOM tree, and the page realm
            // holds its window and document, so nothing here can be collected
            // while the document lives. Frames are released when the document
            // is replaced, so a page that churns iframes would otherwise grow
            // the process without bound. Refuse past the cap rather than let a
            // page decide how much memory to take.
            let cap = max_live_frames();
            if self.frames.len() >= cap {
                tracing::warn!(
                    "refusing a realm for frame {}: already at the {} live frame cap",
                    frame.url,
                    cap,
                );
                self.forget_frame_references(frame.frame_id, frame.parent_frame_id);
                continue;
            }
            let realm = match self.js.as_mut().and_then(|js| {
                FrameRealm::new(js, frame.frame_id, frame.parent_frame_id, &frame.url, &frame.html)
            }) {
                Some(realm) => realm,
                None => {
                    tracing::warn!("could not build a realm for frame {}", frame.url);
                    self.forget_frame_references(frame.frame_id, frame.parent_frame_id);
                    continue;
                }
            };

            // A frame's scripts resolve and are fetched against the frame's own
            // URL, so they need fetching before run_document_scripts, which
            // resolves sources synchronously.
            let wanted = match self.js.as_mut() {
                Some(js) => realm.external_script_urls(js),
                None => Vec::new(),
            };
            let mut sources: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for url in wanted {
                let Ok(parsed) = Url::parse(&url) else { continue };
                if self.should_block_url(&url) {
                    continue;
                }
                match self.do_fetch(&parsed).await {
                    Ok(response) => {
                        sources.insert(url, String::from_utf8_lossy(&response.body).into_owned());
                    }
                    Err(error) => {
                        tracing::warn!("frame script {} failed: {}", url, error);
                    }
                }
            }

            if let Some(js) = self.js.as_mut() {
                if let Err(error) = realm.set_viewport(
                    js,
                    frame.viewport_width as f64,
                    frame.viewport_height as f64,
                ) {
                    tracing::debug!("frame {} viewport setup failed: {error}", frame.url);
                }
                // Page.addScriptToEvaluateOnNewDocument applies to every new
                // document, including child frames. Debug hooks and browser
                // automation setup must be present before frame scripts run.
                for source in &self.preload_scripts {
                    if let Err(error) = realm.execute_script(js, source) {
                        tracing::debug!("frame {} preload failed: {error}", frame.url);
                    }
                }
                for problem in realm.run_document_scripts(js, |url| sources.get(url).cloned()) {
                    tracing::debug!("frame {}: {}", frame.url, problem);
                }
                // The frame's scripts have run, so its document is loaded. Say
                // so: everything a widget defers to DOMContentLoaded or load
                // hangs on this, which is most of its interface.
                if let Err(error) = realm.dispatch_load_events(js) {
                    tracing::debug!("frame {} load events failed: {error}", frame.url);
                }
            }
            self.frames.push(realm);
        }
        true
    }

    /// Hands each queued `postMessage` to the realm it was addressed to.
    ///
    /// Reports whether anything was delivered, because a message usually causes
    /// a reply: a widget posts its result, the page answers, and the exchange
    /// only finishes if the caller settles and drains again.
    fn deliver_frame_messages(&mut self) -> bool {
        let pending = match self.js.as_ref() {
            Some(js) => js.take_pending_frame_messages(),
            None => return false,
        };
        if pending.is_empty() {
            return false;
        }

        for message in pending {
            let escaped_data = serde_json::to_string(&message.data_json).unwrap_or_default();
            let escaped_origin = serde_json::to_string(&message.origin).unwrap_or_default();
            let escaped_target_origin =
                serde_json::to_string(&message.target_origin).unwrap_or_default();
            if message.target_frame_id == 0 {
                let Some(js) = self.js.as_mut() else { continue };
                let script = format!(
                    "globalThis.__obscura_deliverMessage({escaped_data}, {escaped_origin}, {}, {escaped_target_origin});",
                    message.source_frame_id,
                );
                if let Err(error) = js.execute_script("<frame-message>", &script) {
                    tracing::debug!("message to the page failed: {error}");
                }
                continue;
            }

            let Some(index) = self
                .frames
                .iter()
                .position(|frame| frame.frame_id() == message.target_frame_id)
            else {
                // The frame was torn down between the send and the drain.
                tracing::debug!("message for frame {} which is gone", message.target_frame_id);
                continue;
            };
            let Some(js) = self.js.as_mut() else { continue };
            if let Err(error) = self.frames[index].deliver_message(
                js,
                &message.data_json,
                &message.origin,
                message.source_frame_id,
                &message.target_origin,
            ) {
                tracing::debug!("message to frame {} failed: {error}", message.target_frame_id);
            }
        }
        true
    }

    /// Removes the JS references owned by the iframe's parent realm and by the
    /// page's same-origin frame table. This also covers a frame rejected before
    /// a `FrameRealm` exists, so the normal drop path cannot be skipped.
    fn forget_frame_references(&mut self, frame_id: u32, parent_frame_id: u32) {
        let script = format!(
            "if (globalThis.__obscura_frameElements[{frame_id}] &&\
             globalThis.__obscura_frameElements[{frame_id}]._frameId === {frame_id}) {{\
               globalThis.__obscura_frameElements[{frame_id}]._frameId = 0;\
               if (globalThis.__obscura_frameElements[{frame_id}]._iframeWin)\
                 globalThis.__obscura_frameElements[{frame_id}]._iframeWin._frameId = 0;\
             }}\
             delete globalThis.__obscura_frameObjects[{frame_id}];\
             delete globalThis.__obscura_frameWindows[{frame_id}];\
             delete globalThis.__obscura_frameElements[{frame_id}];"
        );
        self.execute_frame_owner_script(parent_frame_id, &script);
        if parent_frame_id != 0 {
            let page_script = format!(
                "delete globalThis.__obscura_frameObjects[{frame_id}];"
            );
            if let Some(js) = self.js.as_mut() {
                if let Err(error) = js.execute_script("<frame-detach>", &page_script) {
                    tracing::debug!("releasing page frame reference failed: {error}");
                }
            }
        }
    }

    /// Executes cleanup in the realm that owns the iframe element. Nested
    /// iframe registries live in their parent frame, not in the page realm.
    fn execute_frame_owner_script(&mut self, parent_frame_id: u32, script: &str) {
        if parent_frame_id == 0 {
            if let Some(js) = self.js.as_mut() {
                if let Err(error) = js.execute_script("<frame-detach>", script) {
                    tracing::debug!("releasing frame references failed: {error}");
                }
            }
            return;
        }

        let Some(index) = self
            .frames
            .iter()
            .position(|frame| frame.frame_id() == parent_frame_id)
        else {
            return;
        };
        let Some(js) = self.js.as_mut() else { return };
        if let Err(error) = self.frames[index].execute_script(js, script) {
            tracing::debug!("releasing nested frame references failed: {error}");
        }
    }

    /// Discards the realms whose iframe element has left the document.
    ///
    /// A browser discards a child browsing context when its element is
    /// removed: the document and its context are torn down. Nothing here can
    /// be collected on its own, because the page realm holds each frame's
    /// window and document so that `contentWindow` can be the frame's real
    /// object, so a page that replaces an iframe repeatedly would otherwise
    /// accumulate contexts and DOM trees for the life of the document.
    ///
    /// A frame nested inside a discarded frame is reached on a later pass,
    /// once its own parent is gone.
    fn release_detached_frames(&mut self) {
        if self.frames.is_empty() {
            return;
        }
        // Each realm reports its own frames whose element is still connected.
        // Liveness is asked of the element rather than of a document query,
        // because an iframe inside a shadow root is absent from
        // `document.querySelectorAll('iframe')` — the shape a challenge widget
        // uses — and treating it as detached tears down a live frame.
        const LIVE_FRAME_IDS: &str = "__obscura_liveFrameIds()";

        let mut live: Vec<u32> = Vec::new();
        let Some(js) = self.js.as_mut() else { return };
        match js.evaluate(LIVE_FRAME_IDS) {
            Ok(value) => live.extend(serde_json::from_value::<Vec<u32>>(value).unwrap_or_default()),
            // A query that did not run says nothing about which frames are
            // gone, and an empty answer here reads as "all of them". Holding
            // them is bounded by the live-frame cap; discarding a live frame
            // is not recoverable, so leave the tree alone.
            Err(error) => {
                tracing::debug!("could not list the page's live frames: {error}");
                return;
            }
        }
        // A frame's own children are in that frame's document, not the page's.
        for index in 0..self.frames.len() {
            let Some(js) = self.js.as_mut() else { return };
            match self.frames[index].evaluate(js, LIVE_FRAME_IDS) {
                Ok(value) => {
                    live.extend(serde_json::from_value::<Vec<u32>>(value).unwrap_or_default())
                }
                Err(error) => {
                    tracing::debug!("could not list a frame's live frames: {error}");
                    return;
                }
            }
        }

        let discarded: Vec<(u32, u32)> = self
            .frames
            .iter()
            .map(|frame| (frame.frame_id(), frame.parent_frame_id()))
            .filter(|(id, _)| !live.contains(id))
            .collect();
        if discarded.is_empty() {
            return;
        }

        // Clean owner realms before dropping any parent. This matters for a
        // nested child whose iframe registry lives in a parent that is also
        // being removed during this pass.
        for &(frame_id, parent_frame_id) in &discarded {
            self.forget_frame_references(frame_id, parent_frame_id);
        }
        self.frames.retain(|frame| live.contains(&frame.frame_id()));
        tracing::debug!("discarded {} detached frame realm(s)", discarded.len());
    }

    /// Moves the frame tree forward by one step: give any fetched frame
    /// document a realm, then hand on any message waiting for a realm.
    ///
    /// Reports whether anything happened, so a caller can pump again. A new
    /// frame runs scripts that can post, and a message usually causes a reply,
    /// so neither queue is finished until both are quiet.
    async fn advance_frames(&mut self) -> bool {
        let attached = self.attach_pending_frames().await;
        let delivered = self.deliver_frame_messages();
        self.release_detached_frames();
        attached || delivered
    }

    /// URLs of the page's live child frames, in creation order.
    pub fn frame_urls(&self) -> Vec<String> {
        self.frames
            .iter()
            .map(|frame| frame.url().to_string())
            .collect()
    }

    /// Evaluates an expression inside one of the page's child frames.
    pub fn evaluate_in_frame(
        &mut self,
        index: usize,
        expression: &str,
    ) -> Result<serde_json::Value, String> {
        let realm = self.frames.get(index).ok_or("no such frame")?;
        let js = self.js.as_mut().ok_or("no runtime")?;
        realm.evaluate(js, expression)
    }

    /// Update the page's CSS viewport. Calling this before navigation makes
    /// responsive scripts observe it from their first instruction; calling it
    /// on a live page mirrors CDP's device-metrics override surfaces.
    pub fn set_viewport(&mut self, viewport: (f32, f32)) {
        if !viewport.0.is_finite()
            || !viewport.1.is_finite()
            || viewport.0 <= 0.0
            || viewport.1 <= 0.0
        {
            return;
        }
        self.viewport = viewport;
        if let Some(js) = &mut self.js {
            js.set_viewport(viewport.0 as f64, viewport.1 as f64);
        }
    }

    /// Set or clear the CDP physical-screen override independently of layout.
    pub fn set_screen_size_override(&mut self, size: Option<(f32, f32)>, emulated: bool) {
        self.screen_size_override = size.filter(|(width, height)| {
            width.is_finite() && height.is_finite() && *width > 0.0 && *height > 0.0
        });
        self.screen_metrics_emulated = emulated;
        if let Some(js) = &mut self.js {
            js.set_screen_size_override(
                self.screen_size_override
                    .map(|(width, height)| (width as f64, height as f64)),
                self.screen_metrics_emulated,
            );
        }
    }

    /// Apply CDP device metrics relative to the metrics that were active when
    /// emulation was first enabled. A zero protocol dimension/scale is passed
    /// as `None` and therefore restores that axis from the retained baseline.
    pub fn apply_device_metrics_override(
        &mut self,
        width: Option<f32>,
        height: Option<f32>,
        device_scale_factor: Option<f32>,
        screen_size: Option<(f32, f32)>,
        mobile: bool,
    ) {
        let baseline = *self
            .device_metrics_baseline
            .get_or_insert(DeviceMetricsBaseline {
                viewport: self.viewport,
                device_scale_factor: self.device_scale_factor,
            });
        let viewport = (
            width.unwrap_or(baseline.viewport.0),
            height.unwrap_or(baseline.viewport.1),
        );
        self.set_viewport(viewport);

        // Blink uses the effective widget size as the screen size for mobile
        // emulation when no complete explicit screen size was supplied.
        let effective_screen_size = screen_size.or_else(|| mobile.then_some(viewport));
        self.set_screen_size_override(effective_screen_size, true);
        self.set_device_scale_factor(device_scale_factor.unwrap_or(baseline.device_scale_factor));
    }

    /// Disable CDP device metrics and restore the state captured by the first
    /// override. Clearing while emulation is inactive is intentionally a no-op.
    pub fn clear_device_metrics_override(&mut self) {
        let Some(baseline) = self.device_metrics_baseline.take() else {
            return;
        };
        self.set_viewport(baseline.viewport);
        self.set_screen_size_override(None, false);
        self.set_device_scale_factor(baseline.device_scale_factor);
    }

    /// Set the screenshot surface density without changing CSS layout. CDP
    /// uses zero to disable its override, which restores the native 1x surface
    /// in Obscura's headless-only model.
    pub fn set_device_scale_factor(&mut self, device_scale_factor: f32) {
        if !device_scale_factor.is_finite() || device_scale_factor < 0.0 {
            return;
        }
        self.device_scale_factor = if device_scale_factor == 0.0 {
            1.0
        } else {
            device_scale_factor
        };
        if let Some(js) = &mut self.js {
            let _ = js.execute_script(
                "<device-metrics>",
                &format!("globalThis.devicePixelRatio={};", self.device_scale_factor),
            );
        }
    }

    pub fn set_default_background_color_override(&mut self, color: Option<[u8; 4]>) {
        self.default_background_color_override = color;
    }

    #[cfg(feature = "render")]
    fn capture_surface_color(&self) -> [u8; 4] {
        self.default_background_color_override
            .unwrap_or([255, 255, 255, 255])
    }

    async fn do_fetch(&self, url: &Url) -> Result<Response, ObscuraNetError> {
        #[cfg(feature = "stealth")]
        if let Some(ref stealth) = self.stealth_client {
            return stealth.fetch(url).await;
        }
        self.http_client
            .fetch_with_callbacks(url, Some(&self.callbacks))
            .await
    }
    fn init_js(&mut self) {
        // init_js is also the new-document path.  Only resume_js explicitly
        // takes these IDs out before entering here and restores them after the
        // same DomTree is installed; a navigation must never inherit IDs from
        // a suspended prior document whose allocator may reuse them.
        self.suspended_started_script_ids.clear();
        // Drop any existing runtime so the JS realm starts clean on
        // every navigation. The old code reused the V8 isolate and
        // only re-bound `globalThis.document`, leaving window.onload,
        // custom window properties and event handlers from the prior
        // page in place. That made it possible for a page to set
        // attacker-controlled state, trigger a navigation, and then
        // run code in the next document's context.
        if self.js.is_some() {
            // Every frame realm holds a V8 handle into this isolate, so the
            // frames of the outgoing document must go before the runtime does.
            self.frames.clear();
            let _ = self.js.take();
        }

        // Thread the BrowserContext's proxy through to the ES-module loader
        // and op_fetch_url so dynamic imports and JS fetch() honour the
        // configured upstream proxy (#139). When proxy_url is None this is
        // equivalent to with_base_url() (direct connection).
        let mut rt = ObscuraJsRuntime::with_base_url_and_proxy(
            &self.url_string(),
            self.context.proxy_url.clone(),
        );
        rt.set_url(&self.url_string());
        rt.set_encoding(&self.encoding);
        rt.set_title(&self.title);
        rt.set_referrer(&self.referrer);

        #[cfg(feature = "stealth")]
        if self.stealth_client.is_some() {
            rt.set_stealth(true);
            rt.set_user_agent(obscura_net::STEALTH_USER_AGENT);
            rt.set_platform(
                obscura_net::STEALTH_NAVIGATOR_PLATFORM,
                obscura_net::STEALTH_UA_PLATFORM,
                obscura_net::STEALTH_UA_PLATFORM_VERSION,
            );
        } else {
            if let Ok(ua) = self.http_client.user_agent.try_read() {
                rt.set_user_agent(&ua);
            }
            rt.set_platform(
                &self.context.platform,
                &self.context.ua_platform,
                &self.context.ua_platform_version,
            );
        }
        #[cfg(not(feature = "stealth"))]
        {
            if let Ok(ua) = self.http_client.user_agent.try_read() {
                rt.set_user_agent(&ua);
            }
            rt.set_platform(
                &self.context.platform,
                &self.context.ua_platform,
                &self.context.ua_platform_version,
            );
        }
        if let Some((lat, lon)) = env_geolocation() {
            rt.set_geolocation(lat, lon);
        }
        rt.set_viewport(self.viewport.0 as f64, self.viewport.1 as f64);
        rt.set_screen_size_override(
            self.screen_size_override
                .map(|(width, height)| (width as f64, height as f64)),
            self.screen_metrics_emulated,
        );

        rt.set_cookie_jar(self.context.cookie_jar.clone());
        rt.set_http_client(self.http_client.clone());
        rt.set_callbacks(self.callbacks.clone());
        rt.set_blocked_urls(self.blocked_url_patterns.clone());
        #[cfg(feature = "stealth")]
        if let Some(ref stealth) = self.stealth_client {
            rt.set_stealth_client(stealth.clone());
        }

        if let Some(tx) = &self.intercept_tx {
            rt.set_intercept_tx(tx.clone());
        }
        // Re-apply intercept_enabled: enable_interception()/enable_intercept()
        // called before the first navigation sets this on the Page while the
        // runtime does not exist yet, so the new runtime would otherwise start
        // with interception disabled and op_fetch_url would never intercept.
        rt.set_intercept_enabled(self.intercept_enabled);
        rt.set_runtime_events_enabled(self.runtime_events_enabled.get());

        if let Some(dom) = self.dom.take() {
            rt.set_dom(dom);
        }

        rt.run_page_init();
        let _ = rt.execute_script(
            "<device-metrics>",
            &format!("globalThis.devicePixelRatio={};", self.device_scale_factor),
        );

        self.js = Some(rt);
    }

    /// Resolve the document base URL per HTML spec:
    /// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url
    /// Falls back to self.url when no <base href> exists.
    fn resolve_base_url(&self) -> Option<url::Url> {
        let doc_url = self.url.as_ref()?;
        let base_href: Option<String> = self.js.as_ref().and_then(|js| {
            js.with_dom(|dom| match dom.query_selector("base[href]") {
                Ok(Some(nid)) => dom
                    .get_node(nid)
                    .and_then(|n| n.get_attribute("href").map(|s| s.to_string())),
                _ => None,
            })
            .flatten()
        });
        match base_href {
            Some(href) => doc_url.join(&href).ok(),
            None => Some(doc_url.clone()),
        }
    }

    async fn fetch_stylesheets(&mut self) -> Vec<(AuthorStylesheetTarget, String)> {
        let (all_links, inline_imports) = match &self.js {
            Some(js) => js
                .with_dom(|dom| {
                    (
                        linked_stylesheet_requests(dom),
                        inline_stylesheet_import_requests(dom),
                    )
                })
                .unwrap_or_default(),
            None => {
                tracing::info!("fetch_stylesheets: no js runtime");
                return Vec::new();
            }
        };

        tracing::info!(
            "fetch_stylesheets: found {} stylesheet links and {} inline imports",
            all_links.len(),
            inline_imports.len()
        );

        let Some(document_url) = self.url.clone() else {
            return Vec::new();
        };
        let document_base = self
            .resolve_base_url()
            .unwrap_or_else(|| document_url.clone());
        let mut roots = Vec::new();
        let mut scheduled = std::collections::HashSet::new();
        let mut pending = Vec::new();
        for (link_index, href) in all_links {
            let Ok(resolved) = document_base.join(&href) else {
                continue;
            };
            let (key, resolved) = canonical_stylesheet_url(resolved);
            if !subresource_allowed(Some(&document_url), resolved.as_str()) {
                tracing::warn!(
                    "blocking cross-scheme <link rel=stylesheet href>: page={} href={}",
                    self.url_string(),
                    resolved,
                );
                continue;
            }
            if self.should_block_url(resolved.as_str()) {
                tracing::info!("Blocked stylesheet by interception: {}", resolved);
                continue;
            }
            roots.push((AuthorStylesheetTarget::Linked(link_index), key.clone(), None));
            if scheduled.insert(key.clone()) {
                if scheduled.len() <= MAX_STYLESHEET_RESOURCES {
                    pending.push((key, resolved, 0u8));
                }
            }
        }
        for (style_index, import) in inline_imports {
            let Ok(resolved) = document_base.join(&import.url) else {
                continue;
            };
            let (key, resolved) = canonical_stylesheet_url(resolved);
            if !subresource_allowed(Some(&document_url), resolved.as_str())
                || self.should_block_url(resolved.as_str())
            {
                tracing::info!("Blocked inline stylesheet import: {}", resolved);
                continue;
            }
            roots.push((
                AuthorStylesheetTarget::InlineImport(style_index),
                key.clone(),
                import.media,
            ));
            if scheduled.insert(key.clone()) && scheduled.len() <= MAX_STYLESHEET_RESOURCES {
                pending.push((key, resolved, 1u8));
            }
        }

        let mut sheets = std::collections::HashMap::new();
        let mut aliases = std::collections::HashMap::new();
        while !pending.is_empty() {
            let batch = std::mem::take(&mut pending);
            let client = self.http_client.clone();
            #[cfg(feature = "stealth")]
            let stealth_client = self.stealth_client.clone();
            let callbacks = self.callbacks.clone();
            let initiator = document_url.clone();
            use futures::StreamExt as _;
            let results: Vec<_> =
                futures::stream::iter(batch.into_iter().map(|(key, requested_url, depth)| {
                    let client = client.clone();
                    #[cfg(feature = "stealth")]
                    let stealth_client = stealth_client.clone();
                    let callbacks = callbacks.clone();
                    let initiator = initiator.clone();
                    async move {
                        let request =
                            ResourceRequest::subresource(ResourceType::Stylesheet, &initiator);
                        #[cfg(feature = "stealth")]
                        let result = if let Some(stealth_client) = stealth_client {
                            stealth_client
                                .fetch_resource_with_callbacks(
                                    &requested_url,
                                    request,
                                    Some(&callbacks),
                                )
                                .await
                        } else {
                            client
                                .fetch_resource_with_callbacks(
                                    &requested_url,
                                    request,
                                    Some(&callbacks),
                                )
                                .await
                        };
                        #[cfg(not(feature = "stealth"))]
                        let result = client
                            .fetch_resource_with_callbacks(
                                &requested_url,
                                request,
                                Some(&callbacks),
                            )
                            .await;
                        (key, requested_url, depth, result)
                    }
                }))
                .buffered(16)
                .collect()
                .await;

            for (key, requested_url, depth, result) in results {
                let response = match result {
                    Ok(response) => response,
                    Err(error) => {
                        tracing::debug!("Failed to fetch stylesheet {}: {}", requested_url, error);
                        continue;
                    }
                };
                let response_url = response.url.clone();
                self.record_network_event_with_body(
                    response_url.as_str(),
                    "GET",
                    "Stylesheet",
                    response.status,
                    &response.headers,
                    &response.body,
                    false,
                );

                let (response_key, response_url) = canonical_stylesheet_url(response_url);
                if let Some(existing) = aliases.get(&response_key).cloned() {
                    aliases.insert(key, existing);
                    continue;
                }
                let css = obscura_net::decode_non_html(&response.body, response.content_type());
                let (imports, rules) = split_css_imports(&css);
                let imports = if depth < MAX_STYLESHEET_IMPORT_DEPTH {
                    imports
                } else {
                    Vec::new()
                };
                aliases.insert(key.clone(), key.clone());
                aliases.insert(response_key, key.clone());
                sheets.insert(
                    key,
                    LoadedStylesheet {
                        response_url: response_url.clone(),
                        imports: imports.clone(),
                        rules,
                    },
                );

                if depth >= MAX_STYLESHEET_IMPORT_DEPTH {
                    continue;
                }
                for import in imports {
                    let Ok(import_url) = response_url.join(&import.url) else {
                        continue;
                    };
                    let (import_key, import_url) = canonical_stylesheet_url(import_url);
                    if aliases.contains_key(&import_key) || scheduled.contains(&import_key) {
                        continue;
                    }
                    if scheduled.len() >= MAX_STYLESHEET_RESOURCES {
                        tracing::warn!(
                            "stylesheet resource cap reached at {} resources",
                            MAX_STYLESHEET_RESOURCES
                        );
                        continue;
                    }
                    if !subresource_allowed(Some(&document_url), import_url.as_str())
                        || self.should_block_url(import_url.as_str())
                    {
                        tracing::info!("Blocked stylesheet import: {}", import_url);
                        continue;
                    }
                    scheduled.insert(import_key.clone());
                    pending.push((import_key, import_url, depth + 1));
                }
            }
        }

        roots
            .into_iter()
            .filter_map(|(target, key, media)| {
                materialize_stylesheet_graph(
                    &key,
                    &sheets,
                    &aliases,
                    &mut std::collections::HashSet::new(),
                )
                .map(|css| {
                    let css = match media {
                        Some(media) => format!("@media {media} {{\n{css}\n}}\n"),
                        None => css,
                    };
                    (target, css)
                })
            })
            .collect()
    }

    async fn execute_scripts(&mut self) {
        self.execute_scripts_with_module_budget(None).await;
    }

    /// Drive only dynamic script elements which participate in the current
    /// document's load-event delay set. Browser script runners keep this set
    /// separate from arbitrary post-load imports, timers, and enhancement
    /// scripts; navigation readiness must not turn those into an implicit
    /// multi-second settle.
    async fn drive_load_delaying_scripts(
        js: &mut ObscuraJsRuntime,
        deadline: tokio::time::Instant,
    ) -> bool {
        while js.has_pending_load_delaying_scripts() {
            let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now())
            else {
                return false;
            };
            if remaining.is_zero() {
                return false;
            }
            let poll_budget = remaining.min(tokio::time::Duration::from_millis(25));
            match tokio::time::timeout(
                poll_budget,
                js.run_load_delaying_event_loop_tick(),
            )
            .await
            {
                Ok(Ok(_idle)) => {
                    if js.has_pending_load_delaying_scripts() {
                        tokio::task::yield_now().await;
                    }
                }
                Ok(Err(error)) => {
                    if obscura_js::runtime::is_fatal_event_loop_error(&error) {
                        tracing::warn!("load-delaying dynamic script event loop failed: {error}");
                        return false;
                    }
                    // A load-delaying script threw or left an unhandled
                    // rejection. Chrome reports the error and runs the rest;
                    // killing the pump would strand every still-pending script
                    // (#699). The absolute deadline above bounds a page that
                    // errors on every turn.
                    tracing::warn!("load-delaying script task error, continuing: {error}");
                    tokio::task::yield_now().await;
                }
                Err(_) => {
                    // This timeout only cancels a parked event-loop poll. The
                    // shared absolute deadline above remains authoritative.
                }
            }
        }
        true
    }

    async fn execute_scripts_with_module_budget(&mut self, module_budget_override: Option<u64>) {
        let scripts_started = std::time::Instant::now();
        tracing::info!(
            "execute_scripts called, js runtime exists: {}",
            self.js.is_some()
        );
        // Soft deadline on the entire script-execution phase. Heavy SPAs
        // (GitHub, Linear, CodeSandbox) ship 50+ scripts and our serial
        // fetch + execute loop can blow past a Puppeteer/Playwright goto
        // timeout. The old 10s default was too tight: a heavy React/Vue/Angular
        // SPA had its remaining scripts skipped before the app booted, so it
        // never fired its XHR/fetch calls and page.on('response') saw nothing
        // (issue #361). Only pages that actually run past the deadline are
        // affected; fast pages finish and return well before it, so a larger
        // budget costs them nothing. 30s gives an app room to initialize while
        // the per-phase watchdog (armed at this + 1s) still bounds a real
        // synchronous hang. Raise it further with OBSCURA_SCRIPT_DEADLINE_MS=<ms>
        // for very heavy SPAs on slow networks (pair it with a matching client
        // navigation timeout).
        let script_deadline_ms: u64 = std::env::var("OBSCURA_SCRIPT_DEADLINE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30_000);
        let script_deadline =
            tokio::time::Instant::now() + tokio::time::Duration::from_millis(script_deadline_ms);

        // Hard backstop over the WHOLE script-execution phase. Inline scripts
        // run back-to-back with no await between them, so neither the soft
        // deadline above (only checked between scripts) nor the per-script guard
        // can interrupt a page that burns the budget across many synchronous
        // scripts (the real-world SPA / anti-bot busy-loop hang). This watchdog
        // terminates the isolate if cumulative synchronous script work overruns.
        let exec_wd = self
            .js
            .as_mut()
            .map(|js| js.arm_watchdog(std::time::Duration::from_millis(script_deadline_ms + 1000)));

        #[derive(Debug, Clone, Copy)]
        enum ScriptKind {
            Classic,
            Module,
            ImportMap,
        }

        #[derive(Debug)]
        struct ScriptInfo {
            src: Option<String>,
            inline: String,
            is_defer: bool,
            is_async: bool,
            kind: ScriptKind,
            nid: u32,
            /// Document base URL at this element's parser encounter point.
            base_url: String,
        }

        let all_scripts = match &self.js {
            Some(js) => {
                let document_url = self.url_string();
                js.with_dom(|dom| {
                    let script_ids = dom.query_selector_all("script").unwrap_or_default();
                    let mut bases_at_script = std::collections::HashMap::new();
                    let mut active_base = url::Url::parse(&document_url).ok();
                    let mut found_base = false;
                    for nid in dom.descendants(dom.document()) {
                        let Some(node) = dom.get_node(nid) else {
                            continue;
                        };
                        let Some(name) = node.as_element() else {
                            continue;
                        };
                        if name.local.as_ref() == "base" && !found_base {
                            if let Some(href) = node.get_attribute("href") {
                                found_base = true;
                                if let Some(resolved) =
                                    active_base.as_ref().and_then(|base| base.join(href).ok())
                                {
                                    active_base = Some(resolved);
                                }
                            }
                        } else if name.local.as_ref() == "script" {
                            bases_at_script.insert(
                                nid.raw(),
                                active_base
                                    .as_ref()
                                    .map(ToString::to_string)
                                    .unwrap_or_else(|| document_url.clone()),
                            );
                        }
                    }
                    let mut scripts = Vec::new();

                    for sid in script_ids {
                        if let Some(node) = dom.get_node(sid) {
                            let src = node.get_attribute("src").map(|s| s.to_string());
                            let script_type = node
                                .get_attribute("type")
                                .unwrap_or("")
                                .trim()
                                .to_ascii_lowercase();
                            let is_defer = node.get_attribute("defer").is_some();
                            let is_async = node.get_attribute("async").is_some();
                            let kind = match script_type.as_str() {
                                "module" => ScriptKind::Module,
                                "importmap" => ScriptKind::ImportMap,
                                "" | "text/javascript" | "application/javascript" => {
                                    ScriptKind::Classic
                                }
                                _ => continue,
                            };

                            let inline_code = if src.is_none() {
                                dom.text_content(sid)
                            } else {
                                String::new()
                            };

                            if matches!(kind, ScriptKind::ImportMap)
                                || src.is_some()
                                || !inline_code.trim().is_empty()
                            {
                                scripts.push(ScriptInfo {
                                    src,
                                    inline: inline_code,
                                    is_defer,
                                    is_async,
                                    kind,
                                    nid: sid.raw(),
                                    base_url: bases_at_script
                                        .get(&sid.raw())
                                        .cloned()
                                        .unwrap_or_else(|| document_url.clone()),
                                });
                            }
                        }
                    }
                    scripts
                })
                .unwrap_or_default()
            }
            None => return,
        };

        // HTML scripts have an "already started" flag. Mark every
        // parser-discovered script before running page code so React/Next
        // hydration can move or hoist those nodes without appendChild
        // executing them a second time.
        if let Some(js) = &mut self.js {
            let ids = all_scripts
                .iter()
                .map(|script| script.nid.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let _ = js.execute_script(
                "<parser-scripts>",
                &format!("globalThis.__markParserScripts([{}]);", ids),
            );
        }

        tracing::info!("Found {} parser-discovered scripts", all_scripts.len());
        let mut fetch_tasks: Vec<(usize, String)> = Vec::new();

        for (i, script) in all_scripts.iter().enumerate() {
            if !matches!(script.kind, ScriptKind::Classic) {
                continue;
            }
            if let Some(src_url) = &script.src {
                let full_url = if src_url.starts_with("http://") || src_url.starts_with("https://")
                {
                    src_url.clone()
                } else {
                    url::Url::parse(&script.base_url)
                        .ok()
                        .and_then(|base| base.join(src_url).ok())
                        .map(|url| url.to_string())
                        .unwrap_or_else(|| src_url.clone())
                };

                if !subresource_allowed(self.url.as_ref(), &full_url) {
                    // Block file://, data:, javascript:, and other
                    // off-origin schemes from being injected as a
                    // <script src>. Without this an http page can
                    // include <script src="file:///etc/passwd"> and
                    // see the body parsed as JS source.
                    tracing::warn!(
                        "blocking cross-scheme <script src>: page={} src={}",
                        self.url_string(),
                        full_url,
                    );
                    continue;
                }
                if self.should_block_url(&full_url) {
                    tracing::info!("Blocked script by interception: {}", full_url);
                    continue;
                }
                fetch_tasks.push((i, full_url));
            }
        }

        let client = self.http_client.clone();
        let page_callbacks = self.callbacks.clone();
        let script_initiator = self
            .url
            .clone()
            .unwrap_or_else(|| Url::parse("about:blank").unwrap());
        let fetch_futures: Vec<_> = fetch_tasks
            .iter()
            .map(|(idx, url)| {
                let client = client.clone();
                let cbs = page_callbacks.clone();
                let initiator = script_initiator.clone();
                let url = url.clone();
                let idx = *idx;
                async move {
                    let parsed =
                        Url::parse(&url).unwrap_or_else(|_| Url::parse("about:blank").unwrap());
                    if parsed.scheme() == "data" {
                        // data: URIs are inline; decode locally, no network fetch.
                        // Instagram and other Meta properties serve their bootstrap
                        // as <script src="data:application/x-javascript;base64,...">.
                        let body = decode_data_uri(&url).unwrap_or_default();
                        let content_type = url
                            .strip_prefix("data:")
                            .and_then(|s| s.split(',').next())
                            .unwrap_or("application/javascript")
                            .split(';')
                            .next()
                            .unwrap_or("application/javascript")
                            .to_string();
                        let mut headers = std::collections::HashMap::new();
                        headers.insert("content-type".to_string(), content_type);
                        let resp = obscura_net::Response {
                            url: parsed,
                            status: 200,
                            headers,
                            body,
                            redirected_from: Vec::new(),
                        };
                        return Some((idx, url, resp));
                    }
                    let request = ResourceRequest::subresource(ResourceType::Script, &initiator);
                    match client
                        .fetch_resource_with_callbacks(&parsed, request, Some(&cbs))
                        .await
                    {
                        Ok(resp) => Some((idx, url, resp)),
                        Err(e) => {
                            tracing::warn!("Failed to fetch script {}: {}", url, e);
                            None
                        }
                    }
                }
            })
            .collect();

        // Bound concurrency: a page with 100 external scripts would
        // otherwise open 100 sockets at once, exhausting the connection
        // pool / ephemeral ports and triggering OS-level backpressure.
        // 16 is well above the per-host pool ceiling most browsers use
        // and matches what real Chrome does for a given origin.
        use futures::StreamExt as _;
        let fetch_stream = futures::stream::iter(fetch_futures).buffer_unordered(16);
        let fetch_results = match tokio::time::timeout_at(
            script_deadline,
            fetch_stream.collect::<Vec<_>>(),
        )
        .await
        {
            Ok(results) => results,
            Err(_) => {
                tracing::warn!(
                    "execute_scripts: fetch deadline reached, some scripts may not have loaded"
                );
                Vec::new()
            }
        };

        let mut fetched: std::collections::HashMap<usize, (String, String, obscura_net::Response)> =
            std::collections::HashMap::new();
        for result in fetch_results {
            if let Some((idx, url, resp)) = result {
                if !script_response_is_executable(resp.status) {
                    self.record_network_event_with_body(
                        &url,
                        "GET",
                        "Script",
                        resp.status,
                        &resp.headers,
                        &resp.body,
                        false,
                    );
                    tracing::warn!(
                        "Refusing to execute script {} after HTTP {}",
                        url,
                        resp.status
                    );
                    continue;
                }
                // Script bodies: only the HTTP Content-Type charset matters
                // (no in-band meta-charset for JS).
                let code = obscura_net::decode_non_html(&resp.body, resp.content_type());
                fetched.insert(idx, (url, code, resp));
            }
        }

        // Spec: readyState is "loading" while parser-discovered scripts execute.
        // Scripts that check readyState === 'loading' will register DOMContentLoaded
        // listeners instead of calling their callback immediately.
        if let Some(js) = &mut self.js {
            let _ = js.execute_script(
                "<ready-state>",
                "globalThis.__documentReadyState__ = 'loading';",
            );
        }

        // CDP `Page.addScriptToEvaluateOnNewDocument` contract: preload
        // sources must run BEFORE any of the page's own scripts. This is
        // also where puppeteer's `exposeFunction` wrapper installs itself —
        // if preload runs after page scripts, every early binding call
        // hits an undefined function and silently no-ops.
        let preload_sources = self.preload_scripts.clone();
        if let Some(js) = &mut self.js {
            for source in &preload_sources {
                if let Err(e) = js.execute_script_guarded("<preload>", source.as_str()) {
                    tracing::debug!("Preload script error: {}", e);
                }
            }
        }

        // Per-module budget. Modules on an already-rendered page are
        // enhancement, not the app: give them a short budget so one slow
        // non-essential module (e.g. YC's bookface, whose top-level eval
        // idle-waits ~10s) cannot block navigation completion. A page whose
        // body is still an empty shell IS the SPA (issue #205), so give it the
        // full script budget and the app module still mounts.
        let module_budget_ms: u64 = {
            let body_nodes = self
                .js
                .as_ref()
                .and_then(|js| {
                    js.with_dom(|dom| {
                        dom.query_selector("body")
                            .ok()
                            .flatten()
                            .map(|b| dom.descendants(b).len())
                            .unwrap_or(0)
                    })
                })
                .unwrap_or(0);
            let short_ms: u64 = module_budget_override.unwrap_or_else(|| {
                std::env::var("OBSCURA_MODULE_BUDGET_MS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(3_000)
            });
            // A rendered body has hundreds of descendants; an unmounted Vite/Next
            // shell is <root> plus maybe a spinner.
            if module_budget_override.is_some() || body_nodes > 50 {
                short_ms
            } else {
                script_deadline_ms
            }
        };
        // V8 can flag an overrun while a synchronous renderer host call is in
        // progress, but it cannot preempt Rust after entering that call. Allow
        // one bounded, finite style/layout flush without weakening the
        // page-wide script deadline. Private test overrides keep zero grace.
        let module_hostcall_grace_ms = if module_budget_override.is_some() {
            0
        } else {
            std::env::var("OBSCURA_MODULE_HOSTCALL_GRACE_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5_000)
        };

        enum ScheduledScript {
            Classic(usize),
            Module {
                prepared: obscura_js::runtime::PreparedModule,
                url: Option<String>,
                remaining_active_ms: u64,
                graph_elapsed_ms: u64,
                queued_at: std::time::Instant,
            },
        }

        let remaining_budget_ms = |deadline: tokio::time::Instant| -> Option<u64> {
            let remaining = deadline.checked_duration_since(tokio::time::Instant::now())?;
            if remaining.is_zero() {
                return None;
            }
            let millis = remaining
                .as_millis()
                .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0));
            Some(millis.min(u128::from(u64::MAX)) as u64)
        };
        let elapsed_ms_ceil = |elapsed: std::time::Duration| -> u64 {
            elapsed
                .as_micros()
                .div_ceil(1_000)
                .max(1)
                .min(u128::from(u64::MAX)) as u64
        };
        let evaluation_budget_ms = |remaining_active_ms: u64| -> Option<u64> {
            let remaining_page_ms = remaining_budget_ms(script_deadline)?;
            let budget = remaining_active_ms
                .saturating_add(module_hostcall_grace_ms)
                .min(remaining_page_ms);
            (budget != 0).then_some(budget)
        };

        let execute_classic =
            |page: &mut Self,
             script: &ScriptInfo,
             fetched_script: Option<(String, String, obscura_net::Response)>| {
                if script.src.is_some() {
                    if let Some((url, code, resp)) = fetched_script {
                        tracing::info!("Executing script ({} bytes): {}", code.len(), url);
                        let execution_url = resp.url.to_string();
                        page.record_network_event_with_body(
                            &url,
                            "GET",
                            "Script",
                            resp.status,
                            &resp.headers,
                            &resp.body,
                            false,
                        );
                        if let Some(js) = &mut page.js {
                            let _ = js.execute_script(
                                "<current-script>",
                                &format!("globalThis.__currentScriptNid={};", script.nid),
                            );
                            if let Err(error) = js.execute_script_guarded(&execution_url, &code) {
                                tracing::warn!("Script error ({}): {}", execution_url, error);
                            }
                            let _ = js.execute_script(
                                "<current-script>",
                                "globalThis.__currentScriptNid=0;",
                            );
                        }
                    }
                } else if !script.inline.is_empty() {
                    if let Some(js) = &mut page.js {
                        let _ = js.execute_script(
                            "<current-script>",
                            &format!("globalThis.__currentScriptNid={};", script.nid),
                        );
                        if let Err(error) =
                            js.execute_script_guarded(&script.base_url, &script.inline)
                        {
                            tracing::warn!("Inline script error: {}", error);
                        }
                        let _ = js
                            .execute_script("<current-script>", "globalThis.__currentScriptNid=0;");
                    }
                }
            };

        let mut post_parse = Vec::new();

        // Process parser-discovered scripts in encounter order. Import maps
        // register at their exact position; module graphs start there too, but
        // evaluation of non-async modules remains post-parse.
        for (index, script) in all_scripts.iter().enumerate() {
            if tokio::time::Instant::now() >= script_deadline {
                tracing::warn!(
                    "execute_scripts: deadline reached, skipping {} remaining scripts",
                    all_scripts.len() - index,
                );
                break;
            }

            match script.kind {
                ScriptKind::ImportMap => {
                    if script.src.is_some() {
                        tracing::warn!("External import maps are not supported");
                        continue;
                    }
                    if let Some(js) = &self.js {
                        if let Err(error) = js.add_import_map(&script.inline, &script.base_url) {
                            tracing::warn!("Ignoring invalid import map: {}", error);
                        }
                    }
                }
                ScriptKind::Classic => {
                    if script.is_defer && !script.is_async && script.src.is_some() {
                        post_parse.push(ScheduledScript::Classic(index));
                    } else {
                        let fetched_script = fetched.remove(&index);
                        execute_classic(self, script, fetched_script);
                    }
                }
                ScriptKind::Module => {
                    // Graph loading and evaluation share one active-work
                    // allowance. Queue time behind other post-parse scripts is
                    // not work performed by this module.
                    let Some(remaining_page_ms) = remaining_budget_ms(script_deadline) else {
                        tracing::warn!("ES module budget exhausted before graph preparation");
                        continue;
                    };
                    let prepare_budget_ms = module_budget_ms.min(remaining_page_ms);
                    let prepare_started = std::time::Instant::now();
                    let (prepared, module_url) = if let Some(src) = &script.src {
                        let full_url = if src.starts_with("http://")
                            || src.starts_with("https://")
                            || src.starts_with("data:")
                        {
                            src.clone()
                        } else {
                            url::Url::parse(&script.base_url)
                                .ok()
                                .and_then(|base| base.join(src).ok())
                                .map(|url| url.to_string())
                                .unwrap_or_else(|| src.clone())
                        };
                        tracing::info!("Preparing ES module graph: {}", full_url);
                        let result = match &mut self.js {
                            Some(js) => js.prepare_module(&full_url, prepare_budget_ms).await,
                            None => continue,
                        };
                        tracing::debug!(
                            phase = "module-graph",
                            module = %full_url,
                            elapsed_ms = prepare_started.elapsed().as_millis(),
                            budget_ms = prepare_budget_ms,
                            success = result.is_ok(),
                            "ES module phase complete",
                        );
                        match result {
                            Ok(prepared) => (prepared, Some(full_url)),
                            Err(error) => {
                                tracing::warn!("ES module error ({}): {}", full_url, error);
                                continue;
                            }
                        }
                    } else {
                        let result = match &mut self.js {
                            Some(js) => {
                                js.prepare_inline_module(
                                    &script.inline,
                                    &script.base_url,
                                    prepare_budget_ms,
                                )
                                .await
                            }
                            None => continue,
                        };
                        tracing::debug!(
                            phase = "module-graph",
                            module = "<inline>",
                            elapsed_ms = prepare_started.elapsed().as_millis(),
                            budget_ms = prepare_budget_ms,
                            success = result.is_ok(),
                            "ES module phase complete",
                        );
                        match result {
                            Ok(prepared) => (prepared, None),
                            Err(error) => {
                                tracing::warn!("Inline ES module error: {}", error);
                                continue;
                            }
                        }
                    };
                    let graph_elapsed_ms = elapsed_ms_ceil(prepare_started.elapsed());
                    let remaining_active_ms = module_budget_ms.saturating_sub(graph_elapsed_ms);
                    if remaining_active_ms == 0 {
                        tracing::warn!(
                            module = module_url.as_deref().unwrap_or("<inline>"),
                            graph_elapsed_ms,
                            active_budget_ms = module_budget_ms,
                            "ES module exhausted its active budget during graph preparation",
                        );
                        continue;
                    }
                    let scheduled = ScheduledScript::Module {
                        prepared,
                        url: module_url,
                        remaining_active_ms,
                        graph_elapsed_ms,
                        queued_at: std::time::Instant::now(),
                    };
                    if script.is_async {
                        let ScheduledScript::Module {
                            prepared,
                            url,
                            remaining_active_ms,
                            graph_elapsed_ms,
                            queued_at,
                        } = scheduled
                        else {
                            unreachable!();
                        };
                        let Some(evaluation_budget_ms) = evaluation_budget_ms(remaining_active_ms)
                        else {
                            tracing::warn!(
                                module = url.as_deref().unwrap_or("<inline>"),
                                graph_elapsed_ms,
                                queue_wait_ms = queued_at.elapsed().as_millis(),
                                "ES module exhausted the page budget before evaluation",
                            );
                            continue;
                        };
                        let queue_wait_ms = queued_at.elapsed().as_millis();
                        let evaluation_started = std::time::Instant::now();
                        let result = match &mut self.js {
                            Some(js) => {
                                js.evaluate_prepared_module(prepared, evaluation_budget_ms)
                                    .await
                            }
                            None => continue,
                        };
                        tracing::debug!(
                            phase = "module-evaluation",
                            module = url.as_deref().unwrap_or("<inline>"),
                            elapsed_ms = evaluation_started.elapsed().as_millis(),
                            graph_elapsed_ms,
                            queue_wait_ms,
                            remaining_active_ms,
                            evaluation_ceiling_ms = evaluation_budget_ms,
                            success = result.is_ok(),
                            "ES module phase complete",
                        );
                        if let Err(error) = result {
                            tracing::warn!("ES module evaluation error: {}", error);
                        } else if let Some(url) = url {
                            tracing::info!("ES module loaded: {}", url);
                            self.record_network_event(
                                &url,
                                "GET",
                                "Script",
                                200,
                                &std::collections::HashMap::new(),
                                0,
                            );
                        }
                    } else {
                        post_parse.push(scheduled);
                    }
                }
            }
        }

        // Parsing has finished before defer scripts and non-async modules run.
        // They still gate DOMContentLoaded, but observe the browser's
        // `interactive` readyState while they execute.
        if let Some(js) = &mut self.js {
            let _ = js.execute_script(
                "<ready-state-interactive>",
                "globalThis.__documentReadyState__ = 'interactive';",
            );
        }

        for scheduled in post_parse {
            if tokio::time::Instant::now() >= script_deadline {
                tracing::warn!("execute_scripts: deadline reached during post-parse scripts");
                break;
            }
            match scheduled {
                ScheduledScript::Classic(index) => {
                    let script = &all_scripts[index];
                    let fetched_script = fetched.remove(&index);
                    execute_classic(self, script, fetched_script);
                }
                ScheduledScript::Module {
                    prepared,
                    url,
                    remaining_active_ms,
                    graph_elapsed_ms,
                    queued_at,
                } => {
                    let Some(evaluation_budget_ms) = evaluation_budget_ms(remaining_active_ms)
                    else {
                        tracing::warn!(
                            module = url.as_deref().unwrap_or("<inline>"),
                            graph_elapsed_ms,
                            queue_wait_ms = queued_at.elapsed().as_millis(),
                            "ES module exhausted the page budget before post-parse evaluation",
                        );
                        continue;
                    };
                    let queue_wait_ms = queued_at.elapsed().as_millis();
                    let evaluation_started = std::time::Instant::now();
                    let result = match &mut self.js {
                        Some(js) => {
                            js.evaluate_prepared_module(prepared, evaluation_budget_ms)
                                .await
                        }
                        None => continue,
                    };
                    tracing::debug!(
                        phase = "module-evaluation",
                        module = url.as_deref().unwrap_or("<inline>"),
                        elapsed_ms = evaluation_started.elapsed().as_millis(),
                        graph_elapsed_ms,
                        queue_wait_ms,
                        remaining_active_ms,
                        evaluation_ceiling_ms = evaluation_budget_ms,
                        success = result.is_ok(),
                        "ES module phase complete",
                    );
                    if let Err(error) = result {
                        tracing::warn!("ES module evaluation error: {}", error);
                    } else if let Some(url) = url {
                        tracing::info!("ES module loaded: {}", url);
                        self.record_network_event(
                            &url,
                            "GET",
                            "Script",
                            200,
                            &std::collections::HashMap::new(),
                            0,
                        );
                    }
                }
            }
        }

        if let Some(js) = &mut self.js {
            // DOMContentLoaded follows parser/defer/module work, but async
            // dynamic script elements do not gate it. They do remain in the
            // document's load-event delay set, including scripts inserted by
            // a DOMContentLoaded listener.
            let _ = js.execute_script(
                "<dom-content-loaded>",
                "try { document.dispatchEvent(new Event('DOMContentLoaded', {bubbles:false,cancelable:false})); } catch(e) {}\n\
                 try { window.dispatchEvent(new Event('DOMContentLoaded', {bubbles:false,cancelable:false})); } catch(e) {}",
            );

            let load_blockers_finished =
                Self::drive_load_delaying_scripts(js, script_deadline).await;
            if !load_blockers_finished {
                tracing::warn!(
                    "script deadline reached with load-delaying dynamic scripts still pending"
                );
            }

            // readyState becomes complete before the load event. A script
            // inserted by an onload handler is therefore post-load work and
            // remains pending until an explicit caller settle/wait.
            let _ = js.execute_script(
                "<load-event>",
                "globalThis.__documentReadyState__ = 'complete';\n\
                 if (typeof window.onload === 'function') { try { window.onload(); } catch(e) {} }\n\
                 try { window.dispatchEvent(new Event('load', {bubbles:false,cancelable:false})); } catch(e) {}",
            );
        }
        if let Some(token) = exec_wd {
            if let Some(js) = self.js.as_mut() {
                js.disarm_watchdog(token);
            }
        }
        tracing::debug!(
            phase = "script-execution-total",
            elapsed_ms = scripts_started.elapsed().as_millis(),
            budget_ms = script_deadline_ms,
            "script execution phase complete",
        );
    }

    pub async fn navigate(&mut self, url_str: &str) -> Result<(), PageError> {
        self.navigate_with_wait(url_str, crate::lifecycle::WaitUntil::Load)
            .await
    }

    pub async fn navigate_with_wait(
        &mut self,
        url_str: &str,
        wait_until: crate::lifecycle::WaitUntil,
    ) -> Result<(), PageError> {
        self.navigate_with_wait_post(url_str, wait_until, "GET", "")
            .await
    }

    pub async fn navigate_with_wait_post(
        &mut self,
        url_str: &str,
        wait_until: crate::lifecycle::WaitUntil,
        method: &str,
        body: &str,
    ) -> Result<(), PageError> {
        // Hard ceiling on a single end-to-end navigation. Without this a slow
        // primary fetch or a runaway settle loop can hold the V8 lock for
        // arbitrarily long (we've measured 60+ seconds on JS-heavy news
        // sites), wedging every other in-flight CDP request because the
        // dispatcher holds the lock across the entire handler. 30 seconds
        // matches reqwest's default per-request timeout — the worst case is
        // one slow primary GET plus one slow JS-redirect chain step. Override
        // with `OBSCURA_NAV_TIMEOUT_MS=NN`, or set a page-scoped deadline when
        // the automation request already has an explicit timeout.
        let nav_timeout = self.navigation_timeout();
        let nav_timeout_ms = duration_millis_u64(nav_timeout);

        let result = match tokio::time::timeout(
            nav_timeout,
            self.navigate_with_wait_post_inner(url_str, wait_until, method, body, ""),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => {
                self.lifecycle = crate::lifecycle::LifecycleState::Failed;
                Err(PageError::NetworkError(format!(
                    "navigation exceeded {nav_timeout_ms}ms deadline"
                )))
            }
        };
        if result.is_ok() {
            self.push_history(self.url_string());
        }
        result
    }

    /// Drive the JS event loop after navigation so deferred work can run:
    /// pending timers (setTimeout / setInterval), queued microtasks, in-flight
    /// fetches, and completion callbacks such as testharness's
    /// `add_completion_callback`. Returns as soon as the loop goes idle, or
    /// after `max_ms`. Without this the page is observed exactly as it stood at
    /// the load event, before any async work settles, which silently strands
    /// timer-driven tests and dynamic pages.
    pub async fn settle(&mut self, max_ms: u64) {
        if max_ms == 0 {
            return;
        }
        let settle_started = std::time::Instant::now();
        // Pump, then give any frame document that finished fetching a realm of
        // its own. Attaching one runs its scripts, which can start timers,
        // fetches and further frames, so keep alternating until no new frame
        // appears or the budget is gone (issue #600).
        loop {
            let remaining = max_ms.saturating_sub(settle_started.elapsed().as_millis() as u64);
            if remaining == 0 {
                break;
            }
            if let Some(js) = &mut self.js {
                if std::env::var_os("OBSCURA_STRICT_SETTLE").is_some() {
                    Self::settle_runtime_for_duration(js, remaining).await;
                } else {
                    // A deno_core event loop remains "busy" for any future timer,
                    // including analytics intervals and animation loops which do
                    // not make the page more ready. Require a short window without
                    // observable document/network/script activity instead. The
                    // absolute caller budget and V8 watchdog still bound both
                    // asynchronous work and synchronous microtask storms.
                    let _ = js.run_event_loop_until_quiescent(remaining, 150).await;
                }
            }
            if !self.advance_frames().await {
                break;
            }
        }
        #[cfg(feature = "render")]
        {
            // Timers, fetch completions, and framework commits commonly append
            // images or @font-face rules during settling. Seed those resources
            // here so a following capture remains a fast observation of the
            // retained page rather than initiating its own network phase.
            let warmup_ms = std::env::var("OBSCURA_RENDER_RESOURCE_SETTLE_WARMUP_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(1_000);
            let remaining_ms = remaining_settle_resource_warmup_ms(
                max_ms,
                settle_started.elapsed(),
                warmup_ms,
            );
            if remaining_ms != 0 {
                let _ = self.prepare_screenshot_resources(remaining_ms).await;
            }
        }
    }

    /// Pump the event loop and retain the full requested wall-clock delay.
    /// The CLI uses this for an explicitly supplied `--wait`; callers asking
    /// for a fixed capture delay should not be silently shortened by adaptive
    /// readiness heuristics.
    pub async fn settle_for_duration(&mut self, duration_ms: u64) {
        if duration_ms == 0 {
            return;
        }
        if let Some(js) = &mut self.js {
            Self::settle_runtime_for_duration(js, duration_ms).await;
        }
        // A fixed wait must retain its full wall clock, so frames get their
        // realms once at the end instead of being interleaved as in `settle`.
        // Their document scripts still run; only their own deferred work is
        // left for a later settle.
        self.advance_frames().await;
    }

    /// Advance one wake-driven browser task for a continuously owned page.
    /// `true` means deno_core reached full idle; `false` means one wake/task was
    /// delivered and the owner should offer another turn after servicing any
    /// higher-priority automation commands.
    #[doc(hidden)]
    pub async fn run_autonomous_event_loop_turn(&mut self) -> Result<bool, String> {
        let reached_idle = match self.js.as_mut() {
            Some(js) => js.run_autonomous_event_loop_turn().await,
            None => Ok(true),
        }?;
        // Dynamic iframe fetches finish on the page event loop, but their
        // realms must be built by Page between turns. Keep the autonomous CDP
        // pump on the same generic frame path as settle(), so a client that
        // stays attached can observe and run child documents as they arrive.
        let frame_work = self.advance_frames().await;
        Ok(reached_idle && !frame_work)
    }

    async fn settle_runtime_for_duration(js: &mut ObscuraJsRuntime, duration_ms: u64) {
        let started = tokio::time::Instant::now();
        let _ = js.run_event_loop_for_duration(duration_ms).await;
        let requested = tokio::time::Duration::from_millis(duration_ms);
        let elapsed = started.elapsed();
        if elapsed < requested {
            tokio::time::sleep(requested - elapsed).await;
        }
    }

    /// Append the current URL to the history stack, truncating any forward
    /// entries past the cursor (matches real Chrome: navigating after a
    /// goBack clobbers the forward history).
    pub fn push_history(&mut self, url: String) {
        if url.is_empty() {
            return;
        }
        // Don't dupe consecutive entries (Page.reload would otherwise pile up).
        if self.history.get(self.history_index) == Some(&url) {
            return;
        }
        if !self.history.is_empty() && self.history_index < self.history.len() - 1 {
            self.history.truncate(self.history_index + 1);
        }
        self.history.push(url);
        self.history_index = self.history.len() - 1;
    }

    /// Move the history cursor without re-navigating; used by
    /// Page.navigateToHistoryEntry which then drives the actual fetch.
    pub fn set_history_index(&mut self, idx: usize) {
        if idx < self.history.len() {
            self.history_index = idx;
        }
    }

    async fn navigate_with_wait_post_inner(
        &mut self,
        url_str: &str,
        wait_until: crate::lifecycle::WaitUntil,
        method: &str,
        body: &str,
        initial_referrer: &str,
    ) -> Result<(), PageError> {
        let mut current_url = url_str.to_string();
        let mut current_method = method.to_string();
        let mut current_body = body.to_string();
        let mut document_referrer = initial_referrer.to_string();
        let chain_limit = self.navigation_chain_limit();
        for chain in 0..chain_limit {
            self.navigate_single(
                &current_url,
                wait_until,
                &current_method,
                &current_body,
                &document_referrer,
            )
            .await?;
            if let Some((next_url, next_method, next_body)) = self.take_pending_navigation() {
                if cross_scheme_to_file(&current_url, &next_url) {
                    // SOP gate. A web page must not be able to drive
                    // a navigation to file:// and then read the loaded
                    // document. Without this an http(s) page sets
                    // window.onload, calls location.href = "file:..."
                    // and harvests document.body from a local file
                    // once the new document loads.
                    tracing::warn!(
                        "blocking JS-initiated cross-scheme navigation to file: {} -> {}",
                        current_url,
                        next_url,
                    );
                    break;
                }
                tracing::info!(
                    "JS-triggered navigation chain: {} {} -> {}",
                    current_method,
                    current_url,
                    next_url
                );
                document_referrer = self
                    .url
                    .as_ref()
                    .and_then(|source| {
                        Url::parse(&next_url)
                            .ok()
                            .map(|target| navigation_referrer(source, &target))
                    })
                    .unwrap_or_default();
                current_url = next_url;
                current_method = next_method;
                current_body = next_body;
                if chain + 1 == chain_limit {
                    // Hit the cap and the page still wants to keep
                    // chaining. Surface that as an error instead of
                    // returning Ok(()) so callers can distinguish a
                    // successful load from a navigation storm.
                    return Err(PageError::TooManyClientNavigations(chain_limit));
                }
                continue;
            }
            break;
        }
        Ok(())
    }

    async fn navigate_single(
        &mut self,
        url_str: &str,
        wait_until: crate::lifecycle::WaitUntil,
        method: &str,
        body: &str,
        referrer: &str,
    ) -> Result<(), PageError> {
        let url = Url::parse(url_str).map_err(|e| PageError::InvalidUrl(e.to_string()))?;

        self.lifecycle = LifecycleState::Loading;
        self.referrer = referrer.to_string();
        self.url = Some(url.clone());
        self.network_events.clear();

        if self.context.obey_robots {
            if url.scheme() == "http" || url.scheme() == "https" {
                let origin = url.origin().ascii_serialization();
                if !self.context.robots_cache.contains(&origin) {
                    let mut robots_url = url.clone();
                    robots_url.set_path("/robots.txt");
                    robots_url.set_query(None);
                    robots_url.set_fragment(None);
                    let body = match self
                        .http_client
                        .fetch_with_callbacks(&robots_url, Some(&self.callbacks))
                        .await
                    {
                        Ok(resp) if resp.status == 200 => {
                            String::from_utf8_lossy(&resp.body).into_owned()
                        }
                        _ => String::new(),
                    };
                    self.context.robots_cache.parse_and_store(
                        &origin,
                        &body,
                        &self.context.user_agent,
                    );
                }

                if !self.context.robots_cache.is_allowed(&origin, url.path()) {
                    self.lifecycle = LifecycleState::Failed;
                    return Err(PageError::NetworkError(format!(
                        "Blocked by robots.txt: {}",
                        url
                    )));
                }
            }
        }

        if url.scheme() == "about" {
            self.navigate_blank();
            self.init_js();
            // Preloads (Page.addScriptToEvaluateOnNewDocument, the
            // Runtime.addBinding shim) must run on about:blank too —
            // puppeteer's `browser.newPage()` lands on about:blank and
            // a follow-up `exposeFunction` is unusable otherwise.
            let preload_sources = self.preload_scripts.clone();
            if let Some(js) = &mut self.js {
                for source in &preload_sources {
                    if let Err(e) = js.execute_script_guarded("<preload>", source.as_str()) {
                        tracing::debug!("Preload script error on about:blank: {}", e);
                    }
                }
            }
            return Ok(());
        }

        let response = if url.scheme() == "data" {
            let content_type = url_str
                .strip_prefix("data:")
                .and_then(|s| s.split(',').next())
                .unwrap_or("text/html")
                .split(';')
                .next()
                .unwrap_or("text/html")
                .to_string();
            let body_bytes = decode_data_uri(url_str).unwrap_or_default();
            let mut headers = std::collections::HashMap::new();
            headers.insert("content-type".to_string(), content_type);
            Ok(obscura_net::Response {
                url: url.clone(),
                status: 200,
                headers,
                body: body_bytes,
                redirected_from: Vec::new(),
            })
        } else if method == "POST" {
            self.http_client
                .post_form_with_callbacks(&url, body, Some(&self.callbacks))
                .await
        } else {
            self.do_fetch(&url).await
        }
        .map_err(|e| {
            self.lifecycle = LifecycleState::Failed;
            PageError::NetworkError(e.to_string())
        })?;

        // Apply Chromium's charset-aware DevTools body policy. This preserves
        // both non-UTF-8 text and opaque binary bytes without lossy conversion.
        let request_id = self.record_network_event_inner(
            url.as_str(),
            "GET",
            "Document",
            response.status,
            &response.headers,
            response.body.len(),
        );
        let body = &response.body;
        let content_type = response.content_type();
        self.store_response_body_with(request_id, body.len(), || {
            stored_devtools_document_response_body(body, content_type)
        });

        if !response.redirected_from.is_empty() {
            self.url = Some(response.url.clone());
        }

        // Honor the response charset: HTTP Content-Type → <meta charset> sniff
        // in the first 1KB → UTF-8 fallback. Without this, every non-UTF-8
        // page (GBK, Big5, Shift-JIS, Windows-125x, EUC-KR, ISO-8859-x)
        // came through as replacement characters.
        let (body_text, encoding_name) =
            obscura_net::decode_response_with_name(&response.body, response.content_type());
        self.encoding = encoding_name.to_string();
        let dom = parse_html(&body_text);

        self.title = dom
            .query_selector("title")
            .ok()
            .flatten()
            .map(|title_id| dom.text_content(title_id))
            .unwrap_or_default();

        self.dom = Some(dom);
        self.init_js();
        let author_stylesheets = self.fetch_stylesheets().await;

        // Inject CSS as a global so getComputedStyle and any CSS-aware shim
        // can read it. Has to happen before scripts run, regardless of
        // waitUntil, so handlers that read window.__obscura_css see it.
        if !author_stylesheets.is_empty() {
            if let Some(js) = &mut self.js {
                let combined_css = author_stylesheets
                    .iter()
                    .map(|(_, css)| css.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                // Use the thorough template-literal escape that
                // covers U+2028 / U+2029 and other control chars.
                // The previous escaper only handled `, \, and ${,
                // letting attacker-controlled CSS containing a raw
                // U+2028 break out of the template literal and run
                // arbitrary JS in the page's V8 realm.
                let escaped = escape_for_js_template_literal(&combined_css);
                let code = format!("globalThis.__obscura_css = `{}`;", escaped);
                let _ = js.execute_script("<css>", &code);
                for (target, css) in &author_stylesheets {
                    let code = match target {
                        AuthorStylesheetTarget::Linked(link_index) => {
                            materialize_linked_stylesheet_script(*link_index, css)
                        }
                        AuthorStylesheetTarget::InlineImport(style_index) => {
                            materialize_inline_import_script(*style_index, css)
                        }
                    };
                    let _ = js.execute_script("<fetch_stylesheets>", &code);
                }
            }
        }
        self.document_timeline_origin = std::time::Instant::now();
        #[cfg(feature = "render")]
        if let Some(js) = &self.js {
            js.reset_animation_timeline();
        }
        if let Some(js) = &mut self.js {
            let _ = js.execute_script("<iframe-load>",
                "(function() { var iframes = document.querySelectorAll('iframe[src]'); for (var i = 0; i < iframes.length; i++) { var src = iframes[i].getAttribute('src'); if (src && src !== 'about:blank') iframes[i]._loadIframeSrc(src); } })()");
        }

        // Scripts can synchronously flush style/layout through
        // getComputedStyle(), geometry, ResizeObserver, or IntersectionObserver.
        // Seed their image/font dependencies concurrently through the page
        // transport first. Otherwise the first CSSOM read falls into the
        // renderer's synchronous resource loader and serial network latency pins
        // V8, making framework startup take many seconds. This is deliberately
        // bounded: navigation should not wait indefinitely for decorative
        // resources.
        #[cfg(feature = "render")]
        {
            let warmup_ms = std::env::var("OBSCURA_RENDER_RESOURCE_WARMUP_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(1_000);
            let _ = self.prepare_screenshot_resources(warmup_ms).await;
        }

        // Spec: DOMContentLoaded fires AFTER parser-blocking scripts run,
        // not before. Skipping execute_scripts() on the DCL path meant
        // every inline <script> in the page was silently dropped: form
        // listeners never registered, frameworks never bootstrapped,
        // page.click() handlers were no-ops. Now scripts run regardless
        // of waitUntil and DCL means "DOM parsed AND scripts executed".
        self.execute_scripts().await;

        #[cfg(feature = "render")]
        {
            // Page scripts and their bounded post-script event-loop pass can
            // create responsive images, inline styles, and @font-face rules
            // that did not exist during the parser warmup above. Discover them
            // before navigation becomes capture-ready. Known parser resources
            // are filtered by the render cache, so ordinary pages pay only the
            // inexpensive scan on this second pass.
            let warmup_ms = std::env::var("OBSCURA_RENDER_RESOURCE_POST_SCRIPT_WARMUP_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(1_000);
            let _ = self.prepare_screenshot_resources(warmup_ms).await;
        }

        self.lifecycle = LifecycleState::DomContentLoaded;

        // Before any `wait_until` can return, because the frames belong to the
        // document rather than to one readiness level. Puppeteer and Playwright
        // send `Page.navigate` with no `waitUntil`, which lands here and returns
        // on the next line, so building frames further down left every real CDP
        // client seeing a page with no frames at all.
        self.build_document_frames().await;

        if wait_until == crate::lifecycle::WaitUntil::DomContentLoaded {
            return Ok(());
        }

        if let Some(js) = &mut self.js {
            if let Ok(new_title) = js.evaluate("document.title") {
                if let Some(t) = new_title.as_str() {
                    self.title = t.to_string();
                }
            }
        }

        self.lifecycle = LifecycleState::Loaded;

        if matches!(
            wait_until,
            crate::lifecycle::WaitUntil::NetworkIdle0 | crate::lifecycle::WaitUntil::NetworkIdle2
        ) {
            let threshold = match wait_until {
                crate::lifecycle::WaitUntil::NetworkIdle0 => 0,
                crate::lifecycle::WaitUntil::NetworkIdle2 => 2,
                _ => 0,
            };

            // Same hazard as the post-script settle: a synchronous poll can pin
            // the thread past the 5s network-idle deadline, so arm a watchdog
            // that terminates the isolate ~500ms past it.
            let netidle_wd = self
                .js
                .as_mut()
                .map(|js| js.arm_watchdog(std::time::Duration::from_millis(5500)));
            let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
            let mut idle_since: Option<tokio::time::Instant> = None;

            loop {
                let active = self.http_client.active_requests();
                let now = tokio::time::Instant::now();

                if active <= threshold {
                    if idle_since.is_none() {
                        idle_since = Some(now);
                    }
                    if now.duration_since(idle_since.unwrap())
                        >= tokio::time::Duration::from_millis(500)
                    {
                        break;
                    }
                } else {
                    idle_since = None;
                }

                if now >= deadline {
                    tracing::debug!(
                        "Network idle timeout reached with {} active requests",
                        active
                    );
                    break;
                }

                if let Some(js) = &mut self.js {
                    let _ = tokio::time::timeout(
                        tokio::time::Duration::from_millis(50),
                        js.run_event_loop(),
                    )
                    .await;
                } else {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }

            if let Some(token) = netidle_wd {
                if let Some(js) = self.js.as_mut() {
                    js.disarm_watchdog(token);
                }
            }
            self.lifecycle = LifecycleState::NetworkIdle;
        }

        Ok(())
    }

    /// Builds the child frames of the document that just loaded.
    ///
    /// Loading a document includes loading the frames in it, so this belongs to
    /// navigation rather than to `settle`: a CDP client that only navigates
    /// would otherwise be told the page has no frames, because nothing had
    /// given them realms yet.
    ///
    /// A frame's document is fetched by page script, so it arrives from the
    /// event loop rather than being ready the moment parsing ends. Pages
    /// without an iframe skip the pumping entirely and pay one native selector
    /// query.
    async fn build_document_frames(&mut self) {
        // How many rounds of "attach a frame, let it start its own" to follow.
        // A frame can add a frame, so this needs a bound rather than a loop
        // until quiet: a page that adds one on every turn would never finish.
        const ROUNDS: usize = 8;
        const ROUND_MS: u64 = 50;

        let has_iframe = self
            .with_dom(|dom| dom.query_selector("iframe").ok().flatten().is_some())
            .unwrap_or(false);
        if !has_iframe {
            return;
        }

        for _ in 0..ROUNDS {
            if let Some(js) = &mut self.js {
                let _ = tokio::time::timeout(
                    tokio::time::Duration::from_millis(ROUND_MS),
                    js.run_event_loop(),
                )
                .await;
            }
            if !self.advance_frames().await {
                break;
            }
        }
    }

    pub fn navigate_blank(&mut self) {
        self.frames.clear();
        self.js = None;
        self.url = Some(Url::parse("about:blank").unwrap());
        self.dom = Some(parse_html(
            "<!DOCTYPE html><html><head></head><body></body></html>",
        ));
        self.title = String::new();
        self.lifecycle = LifecycleState::Loaded;
        self.document_timeline_origin = std::time::Instant::now();
    }

    pub fn url_string(&self) -> String {
        self.url
            .as_ref()
            .map(|u| u.to_string())
            .unwrap_or_else(|| "about:blank".to_string())
    }

    pub fn with_dom<R>(&self, f: impl FnOnce(&DomTree) -> R) -> Option<R> {
        if let Some(js) = &self.js {
            return js.with_dom(f);
        }
        self.dom.as_ref().map(f)
    }

    /// Concurrently seed the synchronous renderer cache through the owning
    /// page transport. This removes serial image/font HTTP from the first
    /// screenshot while retaining cookies, proxy policy, interception, CORS,
    /// response limits, and connection pooling.
    #[cfg(feature = "render")]
    pub async fn prepare_screenshot_resources(&mut self, max_ms: u64) -> usize {
        let started = std::time::Instant::now();
        if max_ms == 0 || self.js.is_none() {
            return 0;
        }
        let Some(document_url) = self.url.clone() else {
            return 0;
        };
        let base_url = self
            .resolve_base_url()
            .unwrap_or_else(|| document_url.clone());
        let mut candidates = std::collections::BTreeMap::new();

        if let Some(js) = &self.js {
            for (raw, profile) in js.pending_render_image_urls() {
                if let Ok(mut url) = url::Url::parse(&raw) {
                    url.set_fragment(None);
                    candidates.insert((url.to_string(), Some(profile)), ResourceType::Image);
                }
            }
            let css_sources = js
                .with_dom(|dom| {
                    let mut sources = Vec::new();
                    for id in dom.descendants(dom.document()) {
                        let Some(node) = dom.get_node(id) else {
                            continue;
                        };
                        if node
                            .as_element()
                            .is_some_and(|element| element.local.as_ref() == "style")
                        {
                            sources.push(dom.text_content(id));
                        }
                        if let Some(style) = node.get_attribute("style") {
                            sources.push(style.to_string());
                        }
                        if node
                            .as_element()
                            .is_some_and(|element| element.local.as_ref() == "use")
                        {
                            if let Some(href) = node
                                .get_attribute("href")
                                .or_else(|| node.get_attribute("xlink:href"))
                            {
                                sources.push(format!("url({href})"));
                            }
                        }
                    }
                    sources
                })
                .unwrap_or_default();
            for css in css_sources {
                for raw in css_resource_urls(&css, &base_url) {
                    if let Ok(mut url) = url::Url::parse(&raw) {
                        let kind = render_resource_type(&url);
                        url.set_fragment(None);
                        candidates.insert((url.to_string(), None), kind);
                    }
                }
            }
            candidates.retain(|(url, profile), _| match profile {
                Some(profile) => !js.render_image_resource_is_known(url, *profile),
                None => !js.render_resource_is_known(url),
            });
        }

        candidates.retain(|(url, _), _| {
            subresource_allowed(Some(&document_url), url) && !self.should_block_url(url)
        });
        if candidates.len() > 128 {
            candidates = candidates.into_iter().take(128).collect();
        }
        if candidates.is_empty() {
            return 0;
        }

        let requested: Vec<(String, Option<obscura_js::ImageRequestProfile>, ResourceType)> =
            candidates
                .into_iter()
                .map(|((url, profile), kind)| (url, profile, kind))
                .collect();
        let client = self.http_client.clone();
        #[cfg(feature = "stealth")]
        let stealth_client = self.stealth_client.clone();
        let callbacks = self.callbacks.clone();
        let initiator = document_url.clone();
        use futures::StreamExt as _;
        let requests = futures::stream::iter(requested.into_iter().map(|(raw, profile, kind)| {
            let client = client.clone();
            #[cfg(feature = "stealth")]
            let stealth_client = stealth_client.clone();
            let callbacks = callbacks.clone();
            let initiator = initiator.clone();
            async move {
                let parsed = url::Url::parse(&raw).expect("validated render resource URL");
                let mut request = ResourceRequest::subresource(kind, &initiator);
                match profile {
                    Some(obscura_js::ImageRequestProfile::CorsSameOrigin) => {
                        request.mode = obscura_net::RequestMode::Cors;
                        request.credentials = obscura_net::RequestCredentials::SameOrigin;
                    }
                    Some(obscura_js::ImageRequestProfile::CorsInclude) => {
                        request.mode = obscura_net::RequestMode::Cors;
                        request.credentials = obscura_net::RequestCredentials::Include;
                    }
                    _ => {}
                }
                #[cfg(feature = "stealth")]
                let result = if let Some(stealth_client) = stealth_client {
                    stealth_client
                        .fetch_resource_with_callbacks(&parsed, request, Some(&callbacks))
                        .await
                } else {
                    client
                        .fetch_resource_with_callbacks(&parsed, request, Some(&callbacks))
                        .await
                };
                #[cfg(not(feature = "stealth"))]
                let result = client
                    .fetch_resource_with_callbacks(&parsed, request, Some(&callbacks))
                    .await;
                (raw, profile, kind, result)
            }
        }))
        .buffer_unordered(16);
        futures::pin_mut!(requests);
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(max_ms);
        let mut loaded = 0usize;
        loop {
            match tokio::time::timeout_at(deadline, requests.next()).await {
                Ok(Some((raw, profile, kind, result))) => {
                    let outcome = match result {
                        Ok(response) => {
                            self.record_network_event_with_body(
                                response.url.as_str(),
                                "GET",
                                match kind {
                                    ResourceType::Font => "Font",
                                    _ => "Image",
                                },
                                response.status,
                                &response.headers,
                                &response.body,
                                true,
                            );
                            if (200..300).contains(&response.status) {
                                loaded += 1;
                                Some(response.body)
                            } else {
                                None
                            }
                        }
                        Err(_) => None,
                    };
                    if let Some(js) = &mut self.js {
                        match profile {
                            Some(profile) => {
                                js.seed_render_image_resource(raw, profile, outcome)
                            }
                            None => js.seed_render_resource(raw, outcome),
                        }
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
        // A deadline drops unfinished futures without negative-caching them,
        // so a later warmup can retry slow resources.
        drop(requests);
        tracing::debug!(
            loaded,
            elapsed_ms = started.elapsed().as_millis(),
            "prepared screenshot resources through page transport"
        );
        loaded
    }

    /// Rasterize the current DOM to PNG bytes at `viewport` (CSS pixels), when
    /// the render feature is compiled in. None if the page has no DOM or the
    /// viewport is zero-sized.
    #[cfg(feature = "render")]
    pub fn screenshot(&self, viewport: (f32, f32)) -> Option<Vec<u8>> {
        self.screenshot_with_animation_sample(viewport, self.live_animation_sample())
    }

    /// Rasterize every CSS animation at one explicit local time. This mirrors
    /// Web Animations `currentTime` and is intended for deterministic parity
    /// capture; ordinary screenshots use each live instance's start epoch.
    #[cfg(feature = "render")]
    pub fn screenshot_at_animation_time(
        &self,
        viewport: (f32, f32),
        animation_sample_time: obscura_js::AnimationSampleTime,
    ) -> Option<Vec<u8>> {
        self.screenshot_with_animation_sample(
            viewport,
            obscura_js::AnimationSample {
                time: animation_sample_time,
                mode: obscura_js::AnimationSampleMode::LocalOverride,
            },
        )
    }

    #[cfg(feature = "render")]
    pub fn screenshot_with_animation_sample(
        &self,
        viewport: (f32, f32),
        animation_sample: obscura_js::AnimationSample,
    ) -> Option<Vec<u8>> {
        // Needed to resolve the relative image URLs ("logo.svg") that make up
        // the overwhelming majority of real markup.
        let base_url = self.resolve_base_url();
        let base_url = base_url.as_ref().map(|u| u.as_str());
        if let Some(js) = &self.js {
            if !js.set_animation_sample(animation_sample) {
                return None;
            }
            if let Some(png) = js.screenshot_prepared_with_surface_color(
                viewport,
                base_url,
                self.capture_surface_color(),
            ) {
                return Some(png);
            }
        }
        // Compatibility path for a page without a JS runtime or an ad-hoc
        // viewport/base that does not match the runtime's CSSOM render key.
        let scroll = self
            .js
            .as_ref()
            .map(|js| js.scroll_offset())
            .unwrap_or((0.0, 0.0));
        // When there IS a runtime, paint against the resource cache it already
        // holds for this document. Building a fresh one here refetched every
        // image on every capture, so a caller repeating a screenshot at a
        // viewport that does not match the prepared key paid the whole network
        // cost per frame.
        if let Some(js) = &self.js {
            if let Some(png) = js.screenshot_unprepared_with_retained_resources(
                viewport,
                base_url,
                scroll,
                animation_sample.time,
                self.capture_surface_color(),
            ) {
                return Some(png);
            }
        }
        self.with_dom(|dom| {
            obscura_js::screenshot_png_scrolled_at_animation_time_with_surface_color(
                dom,
                viewport,
                base_url,
                scroll,
                animation_sample.time,
                self.capture_surface_color(),
            )
        })
            .flatten()
    }

    /// Rasterize an immutable document-space rectangle from the page's retained
    /// layout. Unlike [`Self::screenshot`], this may address content outside the
    /// live viewport and scale the output without relayout or scripted scroll.
    #[cfg(feature = "render")]
    pub fn screenshot_region(
        &self,
        region: obscura_js::CaptureRegion,
    ) -> Result<Vec<u8>, obscura_js::CaptureError> {
        self.screenshot_region_with_animation_sample(region, self.live_animation_sample())
    }

    #[cfg(feature = "render")]
    pub fn screenshot_region_at_animation_time(
        &self,
        region: obscura_js::CaptureRegion,
        animation_sample_time: obscura_js::AnimationSampleTime,
    ) -> Result<Vec<u8>, obscura_js::CaptureError> {
        self.screenshot_region_with_animation_sample(
            region,
            obscura_js::AnimationSample {
                time: animation_sample_time,
                mode: obscura_js::AnimationSampleMode::LocalOverride,
            },
        )
    }

    #[cfg(feature = "render")]
    pub fn screenshot_region_with_animation_sample(
        &self,
        region: obscura_js::CaptureRegion,
        animation_sample: obscura_js::AnimationSample,
    ) -> Result<Vec<u8>, obscura_js::CaptureError> {
        let js = self
            .js
            .as_ref()
            .ok_or(obscura_js::CaptureError::PaintFailed)?;
        if !js.set_animation_sample(animation_sample) {
            return Err(obscura_js::CaptureError::PaintFailed);
        }
        js.screenshot_prepared_region_with_surface_color(region, self.capture_surface_color())
    }

    /// Scrollable document dimensions from the retained render layout. Unlike
    /// DOM properties evaluated in page JavaScript, this cannot be shadowed or
    /// monkey-patched by the document being captured.
    #[cfg(feature = "render")]
    pub fn prepared_content_size(&self) -> Option<(f32, f32)> {
        self.prepared_content_size_with_animation_sample(self.live_animation_sample())
    }

    #[cfg(feature = "render")]
    pub fn prepared_content_size_at_animation_time(
        &self,
        animation_sample_time: obscura_js::AnimationSampleTime,
    ) -> Option<(f32, f32)> {
        self.prepared_content_size_with_animation_sample(obscura_js::AnimationSample {
            time: animation_sample_time,
            mode: obscura_js::AnimationSampleMode::LocalOverride,
        })
    }

    #[cfg(feature = "render")]
    pub fn prepared_content_size_with_animation_sample(
        &self,
        animation_sample: obscura_js::AnimationSample,
    ) -> Option<(f32, f32)> {
        let js = self.js.as_ref()?;
        js.set_animation_sample(animation_sample)
            .then(|| js.prepared_content_size())
            .flatten()
    }

    #[cfg(feature = "render")]
    pub fn live_animation_sample(&self) -> obscura_js::AnimationSample {
        if let Some(js) = &self.js {
            return js.live_animation_sample();
        }
        let milliseconds = self.document_timeline_origin.elapsed().as_secs_f64() * 1_000.0;
        obscura_js::AnimationSample {
            time: obscura_js::AnimationSampleTime {
                milliseconds: milliseconds.min(f64::from(f32::MAX)) as f32,
            },
            mode: obscura_js::AnimationSampleMode::DocumentTime,
        }
    }

    #[cfg(feature = "render")]
    pub fn prepared_has_active_css_animations(&self) -> bool {
        self.js
            .as_ref()
            .is_some_and(|js| js.prepared_has_active_css_animations())
    }

    /// Renderer-owned root scroll offset for document-space capture routing.
    #[cfg(feature = "render")]
    pub fn screenshot_scroll_offset(&self) -> (f32, f32) {
        self.js
            .as_ref()
            .map(|js| js.scroll_offset())
            .unwrap_or((0.0, 0.0))
    }

    /// Absolute URLs the page pulled in via fetch()/XHR (issue #301). Empty
    /// when the page has no live JS runtime.
    pub fn fetched_urls(&self) -> Vec<String> {
        self.js
            .as_ref()
            .map(|js| js.fetched_urls())
            .unwrap_or_default()
    }

    /// Move network events recorded for script-initiated requests
    /// (fetch/XHR/dynamic resource) from the JS runtime into this page's
    /// network_events, so the CDP layer emits Network.requestWillBeSent /
    /// responseReceived for them (issue #406). Idempotent: the runtime's queue
    /// is drained, so calling this repeatedly does not duplicate events. The
    /// fetch-{N} request id is preserved so Network.getResponseBody resolves.
    pub fn sync_js_network_events(&mut self) {
        let events = match self.js.as_ref() {
            Some(js) => js.take_js_network_events(),
            None => return,
        };
        for ev in events {
            self.network_events.push(NetworkEvent {
                request_id: ev.request_id,
                url: ev.url,
                method: ev.method,
                resource_type: "Fetch".to_string(),
                status: ev.status,
                headers: std::collections::HashMap::new(),
                response_headers: Arc::new(ev.response_headers),
                body_size: ev.body_size,
                timestamp: ev.timestamp,
            });
        }
    }

    pub fn dom(&self) -> Option<&DomTree> {
        self.dom.as_ref()
    }

    /// V8 isolate handle for this page's runtime, if it has been initialized.
    /// Lets the CDP dispatcher arm a per-command watchdog (which bounds any one
    /// command so a hung page cannot hold this connection's V8 lock forever)
    /// without taking `&mut self`.
    pub fn isolate_handle(&self) -> Option<obscura_js::runtime::IsolateHandle> {
        self.js.as_ref().map(|js| js.isolate_handle())
    }

    /// Clear a V8 termination left by a per-command watchdog so the next command
    /// on this page can run. No-op if the runtime is absent or not terminating.
    pub fn cancel_v8_termination(&mut self) {
        if let Some(js) = self.js.as_mut() {
            js.cancel_termination();
        }
    }

    /// Like [`Self::evaluate`] but bounded by a V8 watchdog so a runaway
    /// expression cannot hang the process. A non-zero `timeout` of zero falls
    /// back to the unbounded path.
    pub fn evaluate_with_timeout(
        &mut self,
        expression: &str,
        timeout: std::time::Duration,
    ) -> serde_json::Value {
        if let Some(js) = &mut self.js {
            match js.evaluate_with_timeout(expression, timeout) {
                Ok(val) => val,
                Err(e) => {
                    tracing::debug!(
                        "JS eval error/timeout for '{}': {}",
                        truncate_on_char_boundary(expression, 80),
                        e
                    );
                    serde_json::Value::Null
                }
            }
        } else {
            self.evaluate(expression)
        }
    }

    pub fn evaluate(&mut self, expression: &str) -> serde_json::Value {
        if let Some(js) = &mut self.js {
            match js.evaluate(expression) {
                Ok(val) => val,
                Err(e) => {
                    tracing::debug!(
                        "JS eval error for '{}': {}",
                        truncate_on_char_boundary(expression, 80),
                        e
                    );
                    serde_json::Value::Null
                }
            }
        } else {
            match expression.trim() {
                "document.title" => serde_json::Value::String(self.title.clone()),
                "document.URL" | "document.location.href" | "window.location.href" => {
                    serde_json::Value::String(self.url_string())
                }
                _ => serde_json::Value::Null,
            }
        }
    }

    pub async fn evaluate_for_cdp(
        &mut self,
        expression: &str,
        return_by_value: bool,
        await_promise: bool,
    ) -> obscura_js::runtime::RemoteObjectInfo {
        if let Some(js) = &mut self.js {
            match js
                .evaluate_for_cdp(expression, return_by_value, await_promise)
                .await
            {
                Ok(info) => info,
                Err(e) => {
                    tracing::debug!("evaluate_for_cdp error: {}", e);
                    obscura_js::runtime::RemoteObjectInfo {
                        js_type: "undefined".into(),
                        subtype: None,
                        class_name: String::new(),
                        description: String::new(),
                        object_id: None,
                        value: None,
                    }
                }
            }
        } else {
            let val = self.evaluate(expression);
            obscura_js::runtime::RemoteObjectInfo {
                js_type: match &val {
                    serde_json::Value::String(_) => "string".into(),
                    serde_json::Value::Number(_) => "number".into(),
                    serde_json::Value::Bool(_) => "boolean".into(),
                    _ => "undefined".into(),
                },
                subtype: None,
                class_name: String::new(),
                description: String::new(),
                object_id: None,
                value: Some(val),
            }
        }
    }

    pub async fn evaluate_for_cdp_with_timeout(
        &mut self,
        expression: &str,
        return_by_value: bool,
        await_promise: bool,
        await_timeout_ms: u64,
    ) -> Result<obscura_js::runtime::RemoteObjectInfo, String> {
        if let Some(js) = &mut self.js {
            js.evaluate_for_cdp_with_timeout(
                expression,
                return_by_value,
                await_promise,
                await_timeout_ms,
            )
            .await
        } else {
            let value = self.evaluate(expression);
            Ok(obscura_js::runtime::RemoteObjectInfo {
                js_type: match &value {
                    serde_json::Value::String(_) => "string".into(),
                    serde_json::Value::Number(_) => "number".into(),
                    serde_json::Value::Bool(_) => "boolean".into(),
                    _ => "undefined".into(),
                },
                subtype: None,
                class_name: String::new(),
                description: String::new(),
                object_id: None,
                value: Some(value),
            })
        }
    }

    pub async fn call_function_on_for_cdp(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        args: &[serde_json::Value],
        return_by_value: bool,
        await_promise: bool,
    ) -> obscura_js::runtime::RemoteObjectInfo {
        if let Some(js) = &mut self.js {
            match js
                .call_function_on_for_cdp(
                    function_declaration,
                    object_id,
                    args,
                    return_by_value,
                    await_promise,
                )
                .await
            {
                Ok(info) => info,
                Err(e) => {
                    tracing::debug!("callFunctionOn error: {}", e);
                    obscura_js::runtime::RemoteObjectInfo {
                        js_type: "undefined".into(),
                        subtype: None,
                        class_name: String::new(),
                        description: String::new(),
                        object_id: None,
                        value: None,
                    }
                }
            }
        } else {
            obscura_js::runtime::RemoteObjectInfo {
                js_type: "undefined".into(),
                subtype: None,
                class_name: String::new(),
                description: String::new(),
                object_id: None,
                value: None,
            }
        }
    }

    pub async fn call_function_on_for_cdp_with_timeout(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        args: &[serde_json::Value],
        return_by_value: bool,
        await_promise: bool,
        await_timeout_ms: u64,
    ) -> Result<obscura_js::runtime::RemoteObjectInfo, String> {
        let js = self.js.as_mut().ok_or("JavaScript runtime unavailable")?;
        js.call_function_on_for_cdp_with_timeout(
            function_declaration,
            object_id,
            args,
            return_by_value,
            await_promise,
            await_timeout_ms,
        )
        .await
    }

    pub fn set_blocked_urls(&mut self, patterns: Vec<String>) {
        self.blocked_url_patterns = patterns.clone();
        if let Some(js) = &self.js {
            js.set_blocked_urls(patterns);
        }
    }

    pub fn release_object(&mut self, object_id: &str) {
        if let Some(js) = &mut self.js {
            js.release_object(object_id);
        }
    }

    fn record_network_event(
        &mut self,
        url: &str,
        method: &str,
        resource_type: &str,
        status: u16,
        response_headers: &std::collections::HashMap<String, String>,
        body_size: usize,
    ) {
        self.record_network_event_inner(
            url,
            method,
            resource_type,
            status,
            response_headers,
            body_size,
        );
    }

    fn record_network_event_with_body(
        &mut self,
        url: &str,
        method: &str,
        resource_type: &str,
        status: u16,
        response_headers: &std::collections::HashMap<String, String>,
        body: &[u8],
        base64_encoded: bool,
    ) {
        let request_id = self.record_network_event_inner(
            url,
            method,
            resource_type,
            status,
            response_headers,
            body.len(),
        );
        self.store_response_body(request_id, body, base64_encoded);
    }

    fn record_network_event_inner(
        &mut self,
        url: &str,
        method: &str,
        resource_type: &str,
        status: u16,
        response_headers: &std::collections::HashMap<String, String>,
        body_size: usize,
    ) -> String {
        self.network_event_counter += 1;
        let request_id = format!("{}.{}", self.id, self.network_event_counter);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        self.network_events.push(NetworkEvent {
            request_id: request_id.clone(),
            url: url.to_string(),
            method: method.to_string(),
            resource_type: resource_type.to_string(),
            status,
            headers: std::collections::HashMap::new(),
            response_headers: Arc::new(response_headers.clone()),
            body_size,
            timestamp,
        });
        request_id
    }

    fn store_response_body(&mut self, request_id: String, body: &[u8], base64_encoded: bool) {
        self.store_response_body_with(request_id, body.len(), || StoredResponseBody {
            body: if base64_encoded {
                BASE64.encode(body)
            } else {
                String::from_utf8_lossy(body).to_string()
            },
            base64_encoded,
        });
    }

    fn store_response_body_with<F>(
        &mut self,
        request_id: String,
        source_len: usize,
        make_stored: F,
    )
    where
        F: FnOnce() -> StoredResponseBody,
    {
        let max_entries = response_body_entry_limit();
        let max_bytes = response_body_byte_limit();
        if max_entries == 0 || max_bytes == 0 || source_len > max_bytes {
            return;
        }
        let stored = make_stored();
        self.response_bodies.insert(request_id.clone(), stored);
        self.response_body_order.push_back(request_id);
        while self.response_body_order.len() > max_entries {
            if let Some(oldest) = self.response_body_order.pop_front() {
                self.response_bodies.remove(&oldest);
            }
        }
    }

    pub fn get_response_body(&self, request_id: &str) -> Option<StoredResponseBody> {
        self.response_bodies.get(request_id).cloned().or_else(|| {
            self.js
                .as_ref()?
                .get_network_response_body(request_id)
                .map(|body| StoredResponseBody {
                    body: body.body,
                    base64_encoded: body.base64_encoded,
                })
        })
    }

    /// Take a stored response body as raw bytes for CDP streaming
    /// (Fetch.takeResponseBodyAsStream). Removes it from the in-memory cache and
    /// transfers ownership to the caller, so a large body is held once and freed
    /// when the stream is closed rather than lingering in this long-running
    /// process (issue #360). Binary bodies are stored base64 (byte-exact); text
    /// bodies return their UTF-8 bytes. Returns None if the body was never
    /// cached (e.g. it exceeded OBSCURA_NETWORK_BODY_BUFFER_BYTES and was
    /// dropped) or the id is unknown.
    pub fn take_response_body_raw(&mut self, request_id: &str) -> Option<Vec<u8>> {
        let stored = if let Some(body) = self.response_bodies.remove(request_id) {
            self.response_body_order.retain(|id| id != request_id);
            body
        } else {
            self.js
                .as_ref()?
                .get_network_response_body(request_id)
                .map(|b| StoredResponseBody {
                    body: b.body,
                    base64_encoded: b.base64_encoded,
                })?
        };
        if stored.base64_encoded {
            BASE64.decode(stored.body.as_bytes()).ok()
        } else {
            Some(stored.body.into_bytes())
        }
    }

    /// Make the body stored under `from_id` also retrievable under `to_id`.
    /// The main navigation resource is stored under its internal request id, but
    /// the CDP layer reports it to clients with the navigation's loaderId as the
    /// requestId (Chrome's `requestId === loaderId` convention). Without this
    /// alias, `Network.getResponseBody(loaderId)` misses and a client navigating
    /// straight to an image or other resource cannot read the main-response body
    /// (issue #340).
    pub fn alias_response_body(&mut self, from_id: &str, to_id: &str) {
        if from_id == to_id || self.response_bodies.contains_key(to_id) {
            return;
        }
        if let Some(body) = self.response_bodies.get(from_id).cloned() {
            self.response_bodies.insert(to_id.to_string(), body);
            self.response_body_order.push_back(to_id.to_string());
        }
    }

    pub fn clear_response_bodies(&mut self) {
        self.response_bodies.clear();
        self.response_body_order.clear();
        if let Some(js) = &self.js {
            js.clear_network_response_bodies();
        }
    }

    pub fn execute_preload_script(&mut self, source: &str) -> Result<(), String> {
        if let Some(js) = &mut self.js {
            js.execute_script("<preload>", source)
        } else {
            Err("No JS runtime".to_string())
        }
    }

    pub fn suspend_js(&mut self) {
        let Some(js) = &self.js else {
            return;
        };
        let started_script_ids = js.started_script_ids();
        let dom = js.take_dom();
        if let Some(dom) = dom {
            self.dom = Some(dom);
            self.suspended_started_script_ids = started_script_ids;
        } else {
            self.suspended_started_script_ids.clear();
        }
        // Every frame realm holds a V8 handle into this isolate, so the frames
        // go before the runtime does — the same order init_js keeps on a new
        // document. Suspending is a teardown of the realm the frames live in,
        // and a realm cannot be suspended and resumed the way the page's DOM
        // can, so they are rebuilt when the page next loads a document.
        self.frames.clear();
        self.js = None;
    }

    pub fn resume_js(&mut self) {
        if self.js.is_some() {
            return;
        }
        let started_script_ids = std::mem::take(&mut self.suspended_started_script_ids);
        self.init_js();
        if let Some(js) = &self.js {
            js.restore_started_script_ids(&started_script_ids);
        }
    }

    pub fn has_js(&self) -> bool {
        self.js.is_some()
    }

    pub fn release_object_group(&mut self) {
        if let Some(js) = &mut self.js {
            js.release_object_group();
        }
    }

    pub fn take_pending_navigation(&self) -> Option<(String, String, String)> {
        if let Some(js) = &self.js {
            js.take_pending_navigation()
        } else {
            None
        }
    }

    pub fn take_pending_binding_calls(&self) -> Vec<(String, String)> {
        if let Some(js) = &self.js {
            js.take_pending_binding_calls()
        } else {
            Vec::new()
        }
    }

    pub fn take_pending_runtime_events(&mut self) -> Vec<obscura_js::ops::RuntimeEvent> {
        if let Some(js) = &mut self.js {
            js.take_pending_runtime_events()
        } else {
            Vec::new()
        }
    }

    pub fn set_runtime_events_enabled(&self, enabled: bool) {
        self.runtime_events_enabled.set(enabled);
        if let Some(js) = &self.js {
            js.set_runtime_events_enabled(enabled);
        }
    }

    pub fn set_preload_scripts(&mut self, scripts: Vec<String>) {
        self.preload_scripts = scripts;
    }

    /// Append a script that runs in the page before any of the page's own
    /// `<script>` tags, matching CDP `Page.addScriptToEvaluateOnNewDocument`.
    /// Takes effect on the next navigation (`goto` / `navigate*`).
    pub fn add_preload_script(&mut self, script: &str) {
        self.preload_scripts.push(script.to_string());
    }

    /// Enable CDP-Fetch-style interception of JS-initiated `fetch()`/XHR.
    /// Returns a receiver yielding every such request; resolve each through its
    /// `resolver` with `InterceptResolution::{Continue, Fulfill, Fail}` to pass,
    /// mock, or block it. Works in stealth and non-stealth. Mirrors how the CDP
    /// server wires the channel (`obscura-cdp/src/server.rs`).
    pub fn enable_interception(
        &mut self,
    ) -> tokio::sync::mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest> {
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<obscura_js::ops::InterceptedRequest>();
        self.set_intercept_tx(tx);
        self.enable_intercept(true);
        rx
    }

    /// Register a passive callback fired for every JS `fetch()`/XHR (and
    /// navigation) request this page makes, once the method/headers/body are
    /// known and before it is sent. Non-blocking; use `enable_interception` to
    /// mutate or block. Returns a stable id; pass it to `off_request` to
    /// detach (issue #408). Scoped to this page: it never sees sibling pages'
    /// requests and dies with the page.
    pub fn on_request(&mut self, cb: RequestCallback) -> u64 {
        self.callbacks.add_request(cb)
    }

    /// Register a passive callback fired with every JS `fetch()`/XHR (and
    /// navigation) response this page receives, including its body.
    /// Non-blocking. The main path for crawlers that need to capture API
    /// response payloads. Returns a stable id for `off_response`. Page-scoped
    /// like `on_request`.
    pub fn on_response(&mut self, cb: ResponseCallback) -> u64 {
        self.callbacks.add_response(cb)
    }

    /// Detach a request observer registered with `on_request`. Returns true if
    /// one was removed.
    pub fn off_request(&mut self, id: u64) -> bool {
        self.callbacks.remove_request(id)
    }

    /// Detach a response observer registered with `on_response`. Returns true if
    /// one was removed.
    pub fn off_response(&mut self, id: u64) -> bool {
        self.callbacks.remove_response(id)
    }

    pub async fn process_pending_navigation(&mut self) -> Result<bool, PageError> {
        if let Some((url, method, body)) = self.take_pending_navigation() {
            let source_url = self
                .url
                .as_ref()
                .and_then(|source| {
                    Url::parse(&url)
                        .ok()
                        .map(|target| navigation_referrer(source, &target))
                })
                .unwrap_or_default();
            let nav_timeout = self.navigation_timeout();
            let nav_timeout_ms = duration_millis_u64(nav_timeout);
            let result = tokio::time::timeout(
                nav_timeout,
                self.navigate_with_wait_post_inner(
                    &url,
                    crate::lifecycle::WaitUntil::Load,
                    &method,
                    &body,
                    &source_url,
                ),
            )
            .await
            .map_err(|_| {
                self.lifecycle = crate::lifecycle::LifecycleState::Failed;
                PageError::NetworkError(format!("navigation exceeded {nav_timeout_ms}ms deadline"))
            })?;
            result?;
            self.push_history(self.url_string());
            Ok(true)
        } else {
            // Fork: a page that routed itself through history has still
            // navigated. See fork_virtual_url.rs.
            Ok(self.sync_virtual_url())
        }
    }

    pub fn set_intercept_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<obscura_js::ops::InterceptedRequest>,
    ) {
        self.intercept_tx = Some(tx.clone());
        if let Some(js) = &self.js {
            js.set_intercept_tx(tx);
        }
    }

    pub fn enable_intercept(&mut self, enabled: bool) {
        self.intercept_enabled = enabled;
        if let Some(js) = &self.js {
            js.set_intercept_enabled(enabled);
        }
    }
}

fn script_response_is_executable(status: u16) -> bool {
    (200..=299).contains(&status)
}

fn url_matches_cdp_pattern(pattern: &str, url: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let mut remainder = url;
    let mut first = true;
    for part in pattern.split('*') {
        if part.is_empty() {
            continue;
        }

        let Some(index) = remainder.find(part) else {
            return false;
        };

        if first && !pattern.starts_with('*') && index != 0 {
            return false;
        }

        remainder = &remainder[index + part.len()..];
        first = false;
    }

    pattern.ends_with('*') || remainder.is_empty()
}

#[cfg(test)]
mod tests {
    use super::{
        css_resource_urls, linked_stylesheet_requests, materialize_linked_stylesheet_script,
        materialize_stylesheet_graph, navigation_chain_limit_from_env_value, navigation_referrer,
        navigation_timeout_from_env_value, parse_import_url, rebase_css_urls,
        script_response_is_executable, split_css_imports, truncate_on_char_boundary,
        url_matches_cdp_pattern, LoadedStylesheet, StylesheetImport,
        DEFAULT_NAVIGATION_CHAIN_LIMIT,
    };
    #[cfg(feature = "render")]
    use super::remaining_settle_resource_warmup_ms;
    use base64::Engine as _;
    use obscura_dom::parse_html;

    #[test]
    fn document_devtools_body_matches_fetch_store_decoding_boundaries() {
        assert_eq!(
            super::decode_devtools_document_response_body(b"", None),
            Some(String::new()),
        );
        for (body, content_type, expected) in [
            (
                &[0x81, 0x8d][..],
                None,
                Some("\u{81}\u{8d}".to_string()),
            ),
            (b"ABC", Some("application/octet-stream"), None),
            (&[0xff], Some("text/plain"), Some("ÿ".to_string())),
            (&[0xff], Some("application/json"), None),
            (
                &[0x80],
                Some("text/plain; CHARSET=\"windows-1252\""),
                Some("€".to_string()),
            ),
            (
                &[0xd6, 0xd0, 0xce, 0xc4],
                Some("text/html; Charset='GBK'"),
                Some("中文".to_string()),
            ),
            (&[0xff], Some("text/plain; charset=not-real"), None),
        ] {
            assert_eq!(
                super::decode_devtools_document_response_body(body, content_type),
                expected,
                "unexpected DevTools body policy for {content_type:?}",
            );
        }
        for content_type in [
            "application/x-ecmascript",
            "application/x-javascript",
            "text/javascript1.5",
            "text/json",
        ] {
            assert_eq!(
                super::decode_devtools_document_response_body(&[0xff], Some(content_type)),
                None,
                "{content_type} must use Chromium's UTF-8 decoder",
            );
        }
        assert_eq!(
            super::decode_devtools_document_response_body(
                b"<html/>",
                Some("application/xhtml+xml"),
            ),
            Some("<html/>".to_string()),
        );
        for content_type in ["image/svg+xml", "text/xsl", "foo/bar+xml"] {
            assert_eq!(
                super::decode_devtools_document_response_body(b"<xml/>", Some(content_type)),
                None,
                "{content_type} has no Chromium raw-resource text decoder",
            );
        }
    }

    #[test]
    fn navigation_timeout_environment_default_remains_thirty_seconds() {
        assert_eq!(
            navigation_timeout_from_env_value(None),
            std::time::Duration::from_secs(30)
        );
        assert_eq!(
            navigation_timeout_from_env_value(Some("not-a-timeout")),
            std::time::Duration::from_secs(30)
        );
    }

    #[test]
    fn navigation_timeout_environment_override_remains_available() {
        assert_eq!(
            navigation_timeout_from_env_value(Some("42000")),
            std::time::Duration::from_secs(42)
        );
    }

    #[test]
    fn navigation_chain_limit_environment_default_remains_ten() {
        assert_eq!(navigation_chain_limit_from_env_value(None), 10);
        assert_eq!(
            navigation_chain_limit_from_env_value(Some("not-a-limit")),
            10
        );
    }

    #[test]
    fn navigation_chain_limit_environment_override_remains_available() {
        assert_eq!(navigation_chain_limit_from_env_value(Some("25")), 25);
    }

    #[test]
    fn navigation_chain_limit_raises_a_zero_that_would_load_nothing() {
        assert_eq!(navigation_chain_limit_from_env_value(Some("0")), 1);
    }

    #[test]
    fn css_resource_discovery_ignores_strings_comments_data_and_fragments() {
        let base = url::Url::parse("https://example.test/css/app/main.css").unwrap();
        let css = r#"
            /* url(ignored.png) */
            .copy::before { content: "url(also-ignored.png)"; }
            @import URL("theme.css") print;
            @import url("semi;colon.css") screen;
            .hero { background: url('../img/hero.png'); }
            .icon { mask: URL("https://cdn.test/icon.svg#shape"); }
            .inline { background: url(data:image/svg+xml,<svg/>); }
            .local { mask: url(#local); }
        "#;
        assert_eq!(
            css_resource_urls(css, &base),
            vec![
                "https://example.test/css/img/hero.png".to_string(),
                "https://cdn.test/icon.svg".to_string(),
            ]
        );
    }

    #[test]
    fn font_face_warmup_takes_the_last_src_descriptor_and_skips_undecodable_sources() {
        // The IE8 idiom: a bare `.eot` in its own descriptor, then the real
        // list. The renderer resolves the cascade to the second descriptor and
        // then drops `.eot` and `.svg`, so three of the six are all it will
        // ever consider. The warmup used to fetch all six.
        let base = url::Url::parse("https://example.test/css/app.css").unwrap();
        let css = r#"
            @font-face {
              font-family: 'Probe';
              src: url('/font/probe.eot');
              src: url('/font/probe.eot?#iefix') format('embedded-opentype'),
                   url('/font/probe.woff2') format('woff2'),
                   url('/font/probe.woff') format('woff'),
                   url('/font/probe.ttf') format('truetype'),
                   url('/font/probe.svg#probe') format('svg');
            }
        "#;
        assert_eq!(
            css_resource_urls(css, &base),
            vec![
                "https://example.test/font/probe.woff2".to_string(),
                "https://example.test/font/probe.woff".to_string(),
                "https://example.test/font/probe.ttf".to_string(),
            ]
        );
    }

    #[test]
    fn font_face_warmup_ignores_local_sources_and_keeps_scanning_after_the_block() {
        // `local()` names an installed face and is not a fetch, and the block
        // has to be reported as consumed at exactly its closing brace, or the
        // rule after it would be skipped with it.
        let base = url::Url::parse("https://example.test/css/app.css").unwrap();
        let css = r#"
            @font-face {
              font-family: 'Probe';
              src: local('Probe'), url('probe.woff2') format('woff2');
            }
            .hero { background: url('../img/hero.png'); }
        "#;
        assert_eq!(
            css_resource_urls(css, &base),
            vec![
                "https://example.test/css/probe.woff2".to_string(),
                "https://example.test/img/hero.png".to_string(),
            ]
        );
    }

    #[test]
    fn font_face_warmup_skips_data_sources_and_survives_a_brace_in_a_string() {
        let base = url::Url::parse("https://example.test/css/app.css").unwrap();
        let css = r#"
            @font-face {
              font-family: 'A }';
              src: url(data:font/woff2;base64,d09GMg==) format('woff2'),
                   url('fallback.woff') format('woff');
            }
            .after { background: url('after.png'); }
        "#;
        assert_eq!(
            css_resource_urls(css, &base),
            vec![
                "https://example.test/css/fallback.woff".to_string(),
                "https://example.test/css/after.png".to_string(),
            ]
        );
    }

    #[test]
    fn an_unterminated_font_face_block_falls_back_to_the_generic_scan() {
        // Better to warm too much than to drop the rest of the stylesheet on
        // the floor, which is the same policy `css_import_rule_len` follows.
        let base = url::Url::parse("https://example.test/css/app.css").unwrap();
        let css = "@font-face { src: url('probe.woff2') format('woff2');";
        assert_eq!(
            css_resource_urls(css, &base),
            vec!["https://example.test/css/probe.woff2".to_string()]
        );
    }

    fn spawn_stylesheet_graph_server(
        expected_requests: usize,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let response_origin = origin.clone();
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                request_tx.send(path.clone()).unwrap();
                let (content_type, body) = match path.as_str() {
                    "/" => (
                        "text/html",
                        r#"<!doctype html><html><head>
                            <link rel="stylesheet" href="/css/root.css#first">
                            <link rel="stylesheet" href="/css/root.css#second">
                            <link rel="preload stylesheet" href="/theme/second.css">
                        </head><body></body></html>"#
                            .to_string(),
                    ),
                    "/css/root.css" => (
                        "text/css",
                        "@import '/css/nested/shared.css';@import '/blocked.css';@import '/intercepted.css';.root{background:url('img/root.png')}".to_string(),
                    ),
                    "/theme/second.css" => (
                        "text/css",
                        "@import '../css/nested/shared.css';.second{background:url('img/second.png')}".to_string(),
                    ),
                    "/css/nested/shared.css" => (
                        "text/css",
                        "@import '../root.css';.shared{background:url('../img/shared.png')}".to_string(),
                    ),
                    _ => ("text/plain", "unexpected".to_string()),
                };
                let status = if path == "/blocked.css" || path == "/intercepted.css" {
                    "500 Unexpected Request"
                } else {
                    "200 OK"
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nX-Origin: {response_origin}\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (origin, request_rx)
    }

    fn spawn_inline_import_server() -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            // Five requests are expected after import/image deduplication. Keep
            // two extra accepts alive so a regression's bogus CSS-as-image
            // warmup still reaches the request callback and server cleanly.
            for _ in 0..7 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                request_tx.send(path.clone()).unwrap();
                let (content_type, body) = match path.as_str() {
                    "/" => (
                        "text/html",
                        r#"<!doctype html><style media="screen, print">
                            @import url('/a.css') print;
                            @import '/b.css' print;
                            .local { color: white; background-image: url('/local.svg') }
                        </style><div class="local imported-a imported-b">marker</div>"#,
                    ),
                    "/a.css" => (
                        "text/css",
                        ".imported-a{background:#9020d0 url('/imported.svg')}",
                    ),
                    "/b.css" => ("text/css", ".imported-b{border-color:#f0d020}"),
                    "/local.svg" | "/imported.svg" => (
                        "image/svg+xml",
                        r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><rect width="1" height="1" fill="white"/></svg>"#,
                    ),
                    _ => ("text/plain", "unexpected"),
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (origin, request_rx)
    }

    #[test]
    fn default_navigation_referrer_matches_strict_origin_when_cross_origin() {
        let source = url::Url::parse("https://user:pass@source.example/path?q=1#fragment").unwrap();
        let same_origin = url::Url::parse("https://source.example/next").unwrap();
        let cross_origin = url::Url::parse("https://target.example/next").unwrap();
        let downgrade = url::Url::parse("http://source.example/next").unwrap();

        assert_eq!(
            navigation_referrer(&source, &same_origin),
            "https://source.example/path?q=1"
        );
        assert_eq!(
            navigation_referrer(&source, &cross_origin),
            "https://source.example/"
        );
        assert_eq!(navigation_referrer(&source, &downgrade), "");

        let data_source = url::Url::parse("data:text/html,source").unwrap();
        assert_eq!(navigation_referrer(&data_source, &cross_origin), "");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_navigation_referrer_survives_http_redirects() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 2048];
                let length = stream.read(&mut request).unwrap();
                let request_text = String::from_utf8_lossy(&request[..length]);
                let path = request_text
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/");
                let response = match path {
                    "/source" => {
                        let body = "<script>location.href='/redirect'</script>";
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len(),
                        )
                    }
                    "/redirect" => "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    "/final" => {
                        let body = "<!doctype html><title>final</title>";
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len(),
                        )
                    }
                    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                };
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            "referrer-redirect".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new("referrer-redirect".to_string(), context);
        let source = format!("http://{address}/source");
        page.navigate(&source).await.unwrap();

        let observed = page
            .js
            .as_mut()
            .unwrap()
            .evaluate("[document.URL, document.referrer]")
            .unwrap();
        assert_eq!(
            observed,
            serde_json::json!([format!("http://{address}/final"), source])
        );
    }

    /// `/hop/N` sets `location.href = "/hop/N-1"`, `/hop/0` is the target.
    /// An unreadable path gets a 404, so a broken fixture fails the test.
    fn client_navigation_chain_address(name: &str, connections: usize) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let name = name.to_string();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..connections {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0u8; 2048];
                let length = stream.read(&mut buffer).unwrap_or(0);
                let hop = String::from_utf8_lossy(&buffer[..length])
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .and_then(|path| path.strip_prefix("/hop/"))
                    .and_then(|hop| hop.parse::<usize>().ok());
                let response = match hop {
                    Some(0) => {
                        let body = format!("<!doctype html><title>{name}</title>");
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len(),
                        )
                    }
                    Some(hop) => {
                        let body = format!("<script>location.href='/hop/{}'</script>", hop - 1);
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len(),
                        )
                    }
                    None => {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    }
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        address
    }

    /// Without a limit set the getter reaches its fallback to the environment.
    fn page_without_chain_limit(name: &str) -> super::Page {
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            name.to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        super::Page::new(name.to_string(), context)
    }

    /// The only place where the name of the environment variable is checked
    /// as a string, and the variable is the only way to raise the limit at
    /// runtime. Without this test a typo in the name would stay green.
    #[test]
    fn navigation_chain_limit_reads_the_environment_variable_by_name() {
        std::env::set_var("OBSCURA_NAV_CHAIN_LIMIT", "17");

        let page = page_without_chain_limit("chain-from-environment");

        assert_eq!(page.navigation_chain_limit(), 17);
    }

    #[test]
    fn a_per_page_navigation_chain_limit_wins_over_the_environment() {
        std::env::set_var("OBSCURA_NAV_CHAIN_LIMIT", "17");

        let mut page = page_without_chain_limit("chain-per-page-wins");
        page.set_navigation_chain_limit(4);

        assert_eq!(page.navigation_chain_limit(), 4);
    }

    /// The limit is always set explicitly, never inherited. Otherwise anyone
    /// running the suite with `OBSCURA_NAV_CHAIN_LIMIT` set would see these
    /// tests fail through no fault of the code.
    fn chain_page(name: &str, limit: usize) -> super::Page {
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            name.to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new(name.to_string(), context);
        page.set_navigation_chain_limit(limit);
        page
    }

    #[tokio::test(flavor = "current_thread")]
    async fn navigation_chain_default_allows_nine_client_navigations() {
        let address = client_navigation_chain_address("arrived", 10);
        let mut page = chain_page("chain-within-default", DEFAULT_NAVIGATION_CHAIN_LIMIT);

        page.navigate(&format!("http://{address}/hop/9"))
            .await
            .unwrap();

        assert_eq!(page.url_string(), format!("http://{address}/hop/0"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn navigation_chain_beyond_the_default_reports_client_navigations() {
        let address = client_navigation_chain_address("arrived", 10);
        let mut page = chain_page("chain-beyond-default", DEFAULT_NAVIGATION_CHAIN_LIMIT);

        let error = page
            .navigate(&format!("http://{address}/hop/10"))
            .await
            .expect_err("a chain past the limit must not report success");

        assert!(
            matches!(error, super::PageError::TooManyClientNavigations(10)),
            "unexpected error: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "Too many client-initiated navigations, the chain reached its limit of 10 documents"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn raised_navigation_chain_limit_reaches_a_longer_chain() {
        let address = client_navigation_chain_address("arrived", 12);
        let mut page = chain_page("chain-raised", 12);

        page.navigate(&format!("http://{address}/hop/11"))
            .await
            .unwrap();

        assert_eq!(page.url_string(), format!("http://{address}/hop/0"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_zero_navigation_chain_limit_still_loads_the_initial_document() {
        let address = client_navigation_chain_address("arrived", 1);
        let mut page = chain_page("chain-zero", 0);

        page.navigate(&format!("http://{address}/hop/0"))
            .await
            .unwrap();

        assert_eq!(page.url_string(), format!("http://{address}/hop/0"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn linked_stylesheet_graph_fetches_once_and_preserves_order_and_bases() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (origin, requests) = spawn_stylesheet_graph_server(4);
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            "stylesheet-graph".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new("stylesheet-graph".to_string(), context);
        page.set_blocked_urls(vec!["*blocked.css".to_string()]);
        page.intercept_block_patterns = vec!["*intercepted.css".to_string()];
        page.enable_intercept(true);

        let request_count = std::sync::Arc::new(AtomicUsize::new(0));
        let response_count = std::sync::Arc::new(AtomicUsize::new(0));
        let observed_requests = request_count.clone();
        page.on_request(std::sync::Arc::new(move |request| {
            if request.resource_type == obscura_net::ResourceType::Stylesheet {
                observed_requests.fetch_add(1, Ordering::SeqCst);
            }
        }));
        let observed_responses = response_count.clone();
        page.on_response(std::sync::Arc::new(move |request, _| {
            if request.resource_type == obscura_net::ResourceType::Stylesheet {
                observed_responses.fetch_add(1, Ordering::SeqCst);
            }
        }));

        page.navigate(&format!("{origin}/")).await.unwrap();

        let mut paths = (0..4)
            .map(|_| {
                requests
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "/".to_string(),
                "/css/nested/shared.css".to_string(),
                "/css/root.css".to_string(),
                "/theme/second.css".to_string(),
            ]
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 3);
        assert_eq!(response_count.load(Ordering::SeqCst), 3);
        assert_eq!(
            page.network_events
                .iter()
                .filter(|event| event.resource_type == "Stylesheet")
                .count(),
            3
        );

        let sheets = page
            .js
            .as_ref()
            .unwrap()
            .with_dom(|dom| {
                dom.query_selector_all("style[data-obscura-external-stylesheets]")
                    .unwrap()
                    .into_iter()
                    .map(|nid| dom.text_content(nid))
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(sheets.len(), 3);
        assert_eq!(sheets[0], sheets[1], "duplicate links reuse one download");
        let shared = sheets[0].find(".shared").unwrap();
        let root = sheets[0].find(".root").unwrap();
        assert!(shared < root, "imports precede the importing sheet");
        assert!(sheets[0].contains(&format!("url(\"{origin}/css/img/shared.png\")")));
        assert!(sheets[0].contains(&format!("url(\"{origin}/css/img/root.png\")")));
        let root = sheets[2].find(".root").unwrap();
        let shared = sheets[2].find(".shared").unwrap();
        let second = sheets[2].find(".second").unwrap();
        assert!(
            root < shared && shared < second,
            "cycle is cut without reordering rules"
        );
        assert!(sheets[2].contains(&format!("url(\"{origin}/theme/img/second.png\")")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_imports_fetch_in_order_and_materialize_before_source_style() {
        let (origin, requests) = spawn_inline_import_server();
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            "inline-imports".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new("inline-imports".to_string(), context);
        page.set_viewport((100.0, 80.0));
        let observed_requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_requests = observed_requests.clone();
        page.on_request(std::sync::Arc::new(move |request| {
            callback_requests
                .lock()
                .unwrap()
                .push((request.url.path().to_string(), request.resource_type));
        }));
        page.navigate(&format!("{origin}/")).await.unwrap();

        let mut paths = (0..3)
            .map(|_| {
                requests
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(paths, vec!["/", "/a.css", "/b.css"]);
        let observed_requests = observed_requests.lock().unwrap();
        for path in ["/a.css", "/b.css"] {
            assert_eq!(
                observed_requests
                    .iter()
                    .filter(|(request_path, _)| request_path == path)
                    .map(|(_, resource_type)| *resource_type)
                    .collect::<Vec<_>>(),
                vec![obscura_net::ResourceType::Stylesheet],
                "an inline import must fetch exactly once as a stylesheet"
            );
        }
        #[cfg(feature = "render")]
        {
            for path in ["/local.svg", "/imported.svg"] {
                assert_eq!(
                    observed_requests
                        .iter()
                        .filter(|(request_path, _)| request_path == path)
                        .map(|(_, resource_type)| *resource_type)
                        .collect::<Vec<_>>(),
                    vec![obscura_net::ResourceType::Image],
                    "ordinary rule assets must remain in render warmup"
                );
            }
        }
        drop(observed_requests);

        let styles = page
            .js
            .as_ref()
            .unwrap()
            .with_dom(|dom| {
                dom.query_selector_all("style")
                    .unwrap()
                    .into_iter()
                    .map(|nid| {
                        let node = dom.get_node(nid).unwrap();
                        (
                            node.get_attribute("data-obscura-inline-import").is_some(),
                            node.get_attribute("media").map(str::to_string),
                            dom.text_content(nid),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(styles.len(), 3);
        assert!(styles[0].0 && styles[0].2.contains(".imported-a"));
        assert!(styles[1].0 && styles[1].2.contains(".imported-b"));
        assert!(!styles[2].0 && styles[2].2.contains(".local"));
        assert_eq!(styles[0].1.as_deref(), Some("screen, print"));
        assert_eq!(styles[1].1.as_deref(), Some("screen, print"));
        assert!(styles[0].2.starts_with("@media print {\n"));
        assert!(styles[1].2.starts_with("@media print {\n"));

        #[cfg(feature = "render")]
        {
            let pdf = page
                .raster_pdf(crate::RasterPdfOptions {
                    print_background: true,
                    paper_width_in: 100.0 / 72.0,
                    paper_height_in: 80.0 / 72.0,
                    margin_top_in: 0.0,
                    margin_bottom_in: 0.0,
                    margin_left_in: 0.0,
                    margin_right_in: 0.0,
                    ..crate::RasterPdfOptions::default()
                })
                .expect("inline-import print PDF");
            assert!(pdf.starts_with(b"%PDF-1.4"));
        }
    }

    fn client_replacement_page(name: &str, deferred: bool) -> super::Page {
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            name.to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new(name.to_string(), context);
        let server_content = (0..45)
            .map(|index| format!("<p>server content item {index} with enough text</p>"))
            .collect::<String>();
        let start = if deferred {
            "window.addEventListener('mount-client', () => setTimeout(mountClient, 0));"
        } else {
            "mountClient();"
        };
        let html = format!(
            r#"<!doctype html><html><body><main id="ssr">{server_content}</main><script>
                function mountClient() {{
                    document.body.innerHTML = '<button id="client" data-clicks="0">Client view</button>';
                    const button = document.getElementById('client');
                    button.addEventListener('click', () => {{
                        button.setAttribute('data-clicks', String(Number(button.getAttribute('data-clicks')) + 1));
                    }});
                }}
                {start}
            </script></body></html>"#,
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(html);
        page.url =
            Some(url::Url::parse(&format!("data:text/html;base64,{encoded}")).expect("data URL"));
        page
    }

    fn assert_client_replacement_survived(page: &mut super::Page) {
        let state = page
            .js
            .as_mut()
            .expect("page runtime")
            .evaluate(
                r#"
                var clientReplacementCheck = true;
                const button = document.getElementById('client');
                if (button) button.dispatchEvent(new Event('click'));
                return {
                    staleServerContent: !!document.getElementById('ssr'),
                    clientPresent: !!button,
                    clientText: button ? button.textContent : null,
                    clicks: button ? button.getAttribute('data-clicks') : null,
                    bodyElements: document.querySelectorAll('body *').length
                };
                "#,
            )
            .expect("inspect client replacement");
        assert_eq!(
            state,
            serde_json::json!({
                "staleServerContent": false,
                "clientPresent": true,
                "clientText": "Client view",
                "clicks": "1",
                "bodyElements": 1,
            }),
        );
    }

    fn spawn_parser_import_map_server(
        expected_requests: usize,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 2048];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                request_tx.send(path.clone()).unwrap();
                let (status, body) = match path.as_str() {
                    "/app/before.js" => ("200 OK", "export const value = 'before-first-module';"),
                    "/app/later.js" => ("200 OK", "export const value = 'later-map';"),
                    "/app/async.js" => (
                        "200 OK",
                        "import('too-late')\
                           .then(module => globalThis.__async_before_map = module.value)\
                           .catch(() => globalThis.__async_before_map = 'rejected');",
                    ),
                    _ => ("404 Not Found", "not found"),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{}", address), request_rx)
    }

    fn spawn_delayed_classic_script_server(
        delay: std::time::Duration,
        body: &'static str,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let length = stream.read(&mut request).unwrap();
            let path = String::from_utf8_lossy(&request[..length])
                .lines()
                .next()
                .and_then(|line| line.split_ascii_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            request_tx.send(path).unwrap();
            std::thread::sleep(delay);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{address}"), request_rx)
    }

    fn spawn_script_resource_cache_server(
        distinct: bool,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::Ordering;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let script_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_requests = script_requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let observed_requests = observed_requests.clone();
                std::thread::spawn(move || {
                    let mut request = [0u8; 2048];
                    let length = stream.read(&mut request).unwrap_or(0);
                    let request_text = String::from_utf8_lossy(&request[..length]);
                    let path = request_text
                        .lines()
                        .next()
                        .and_then(|line| line.split_ascii_whitespace().nth(1))
                        .unwrap_or("/");
                    let (content_type, cache_control, body) = if path == "/duplicate.html" {
                        let tags = (0..32)
                            .map(|_| "<script src='/shared.js'></script>")
                            .collect::<String>();
                        (
                            "text/html",
                            "no-store",
                            format!(
                                "<!doctype html><html><body><script>globalThis.__runs=0</script>{tags}</body></html>"
                            ),
                        )
                    } else if path == "/distinct.html" {
                        let tags = (0..24)
                            .map(|index| format!("<script src='/distinct/{index}.js'></script>"))
                            .collect::<String>();
                        (
                            "text/html",
                            "no-store",
                            format!(
                                "<!doctype html><html><body><script>globalThis.__runs=0</script>{tags}</body></html>"
                            ),
                        )
                    } else if path == "/shared.js" || path.starts_with("/distinct/") {
                        observed_requests.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(80));
                        (
                            "application/javascript",
                            "public, max-age=3600",
                            "globalThis.__runs=(globalThis.__runs||0)+1;".to_string(),
                        )
                    } else {
                        ("text/plain", "no-store", "not found".to_string())
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nCache-Control: {cache_control}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len(),
                    );
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });
        let page = if distinct {
            "distinct.html"
        } else {
            "duplicate.html"
        };
        (format!("http://{address}/{page}"), script_requests)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_cacheable_scripts_fetch_once_but_execute_for_each_element() {
        use std::sync::atomic::Ordering;

        let (url, script_requests) = spawn_script_resource_cache_server(false);
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            "duplicate-script-cache".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new("duplicate-script-cache".to_string(), context);

        page.navigate(&url).await.unwrap();

        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__runs")
                .unwrap(),
            serde_json::json!(32.0),
            "a cached response must still execute for every script element",
        );
        assert_eq!(script_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn distinct_cacheable_scripts_keep_distinct_network_requests() {
        use std::sync::atomic::Ordering;

        let (url, script_requests) = spawn_script_resource_cache_server(true);
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            "distinct-script-cache".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new("distinct-script-cache".to_string(), context);

        page.navigate(&url).await.unwrap();

        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__runs")
                .unwrap(),
            serde_json::json!(24.0),
        );
        assert_eq!(script_requests.load(Ordering::SeqCst), 24);
    }

    #[test]
    fn external_scripts_require_a_successful_http_status() {
        assert!(script_response_is_executable(200));
        assert!(script_response_is_executable(204));
        assert!(script_response_is_executable(299));
        assert!(!script_response_is_executable(0));
        assert!(!script_response_is_executable(304));
        assert!(!script_response_is_executable(401));
        assert!(!script_response_is_executable(404));
        assert!(!script_response_is_executable(500));
    }

    /// `/` puts its iframe inside a closed shadow root, `/plain.html` puts the
    /// same iframe straight in the document, and `/child.html` is the frame.
    async fn spawn_shadow_frame_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 2048];
                    let read = socket.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..read]).to_string();
                    let body = if request.starts_with("GET /child.html ") {
                        "<html><body><script>window.__ran = 'YES';</script></body></html>"
                    } else if request.starts_with("GET /plain.html ") {
                        "<html><body><iframe src=\"/child.html\"></iframe></body></html>"
                    } else {
                        "<html><body><div id=\"host\"></div><script>\
                         var r = document.getElementById('host').attachShadow({mode:'closed'});\
                         var f = document.createElement('iframe');\
                         f.src = '/child.html';\
                         r.appendChild(f);\
                         </script></body></html>"
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}/")
    }

    /// A `FrameRealm` owns a `v8::Global` into the runtime's isolate, which is
    /// why `init_js` clears the frames before it drops the runtime. `suspend_js`
    /// drops the same runtime and has to honour the same order, otherwise the
    /// realms of a suspended page outlive the isolate they point into and the
    /// next navigation drops those handles against a different one.
    #[tokio::test]
    async fn suspending_the_runtime_releases_the_frame_realms_it_owns() {
        std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
        let base = spawn_shadow_frame_server().await;
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            "suspend-frames".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new("suspend-frames".to_string(), context);
        page.navigate(&format!("{base}plain.html")).await.unwrap();
        assert_eq!(
            page.frame_urls().len(),
            1,
            "the page never built its child frame, so this proves nothing"
        );

        page.suspend_js();
        assert!(
            page.frame_urls().is_empty(),
            "the frame realms outlived the isolate they hold a handle into: {:?}",
            page.frame_urls()
        );

        // The page still works afterwards: resuming rebuilds the runtime, and
        // navigating again builds the frames of the new document.
        page.resume_js();
        page.navigate(&format!("{base}plain.html")).await.unwrap();
        assert_eq!(page.frame_urls().len(), 1);
    }

    fn frame_page(name: &str) -> super::Page {
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            name.to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        super::Page::new(name.to_string(), context)
    }

    /// An iframe inside a shadow root is absent from
    /// `document.querySelectorAll('iframe')` — real Chrome reports 0 for it too
    /// — so a liveness check built on that query reads a live frame as detached
    /// and tears it down. That is the shape a challenge widget uses, so the
    /// frame it depends on would be discarded moments after it loaded.
    #[tokio::test(flavor = "current_thread")]
    async fn a_frame_inside_a_shadow_root_survives_the_detach_sweep() {
        std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
        let base = spawn_shadow_frame_server().await;
        let mut page = frame_page("shadow-frame-survives");
        page.navigate(&base).await.unwrap();
        page.settle(1_000).await;

        assert_eq!(
            page.frames.len(),
            1,
            "the shadow-root frame was discarded as detached: {:?}",
            page.frame_urls()
        );
        // The realm is not merely alive; the page can still reach into it.
        let published = page
            .js
            .as_mut()
            .unwrap()
            .evaluate("Object.keys(globalThis.__obscura_frameObjects).length")
            .unwrap();
        assert_eq!(published.as_f64(), Some(1.0), "the page cannot reach the frame");
    }

    /// The sweep still does its job: an iframe removed from the document has
    /// its realm and every reference the page realm holds to it released.
    #[tokio::test(flavor = "current_thread")]
    async fn removing_an_iframe_releases_its_realm() {
        std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
        let base = spawn_shadow_frame_server().await;
        let mut page = frame_page("detached-frame-released");
        page.navigate(&format!("{base}plain.html")).await.unwrap();
        page.settle(1_000).await;
        assert_eq!(page.frames.len(), 1, "no frame to remove");

        page.js
            .as_mut()
            .unwrap()
            .evaluate("(document.querySelector('iframe').remove(), 1)")
            .unwrap();
        page.release_detached_frames();

        assert!(page.frames.is_empty(), "the detached realm was kept");
        let left = page
            .js
            .as_mut()
            .unwrap()
            .evaluate(
                "Object.keys(globalThis.__obscura_frameObjects).length\
                 + Object.keys(globalThis.__obscura_frameWindows).length\
                 + Object.keys(globalThis.__obscura_frameElements).length",
            )
            .unwrap();
        assert_eq!(
            left.as_f64(),
            Some(0.0),
            "a reference to the discarded frame survived, so its context cannot be collected"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn suspend_resume_preserves_document_script_start_state() {
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            "script-state-suspend".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new("script-state-suspend".to_string(), context);
        page.url = Some(url::Url::parse("http://example.com/suspend.html").unwrap());
        page.dom = Some(parse_html(
            r#"<html><head></head><body data-parser-runs="0" data-dynamic-runs="0" data-inert-runs="0">
            <script id="parser">
              document.body.setAttribute("data-parser-runs", String(Number(document.body.getAttribute("data-parser-runs")) + 1));
            </script>
            </body></html>"#,
        ));
        page.init_js();
        page.execute_scripts().await;

        let before = page
            .js
            .as_mut()
            .unwrap()
            .evaluate(
                r#"
                var scriptStateSetup = true;
                const dynamic = document.createElement("script");
                dynamic.id = "dynamic";
                dynamic.textContent =
                  'document.body.setAttribute("data-dynamic-runs", String(Number(document.body.getAttribute("data-dynamic-runs")) + 1))';
                document.body.appendChild(dynamic);

                const holder = document.createElement("div");
                holder.innerHTML =
                  '<script id="inert">document.body.setAttribute("data-inert-runs", String(Number(document.body.getAttribute("data-inert-runs")) + 1))<\/script>';
                document.body.appendChild(holder.firstChild);
                return [
                  document.body.getAttribute("data-parser-runs"),
                  document.body.getAttribute("data-dynamic-runs"),
                  document.body.getAttribute("data-inert-runs")
                ];
                "#,
            )
            .unwrap();
        assert_eq!(before, serde_json::json!(["1", "1", "0"]));

        page.suspend_js();
        page.suspend_js();
        page.resume_js();

        let after = page
            .js
            .as_mut()
            .unwrap()
            .evaluate(
                r#"
                var scriptStateCheck = true;
                for (const id of ["parser", "dynamic", "inert"]) {
                  const script = document.getElementById(id);
                  document.head.appendChild(script);
                  document.body.appendChild(script.cloneNode(true));
                }
                return [
                  document.body.getAttribute("data-parser-runs"),
                  document.body.getAttribute("data-dynamic-runs"),
                  document.body.getAttribute("data-inert-runs")
                ];
                "#,
            )
            .unwrap();
        assert_eq!(after, serde_json::json!(["1", "1", "0"]));
    }

    #[test]
    fn new_document_does_not_inherit_suspended_script_ids() {
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            "script-state-navigation".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new("script-state-navigation".to_string(), context);
        page.url = Some(url::Url::parse("http://example.com/old.html").unwrap());
        page.dom = Some(parse_html(
            "<html><head></head><body><script id=old></script></body></html>",
        ));
        page.init_js();
        page.js
            .as_mut()
            .unwrap()
            .evaluate(
                "var setup = true; const old = document.getElementById('old'); globalThis.__markParserScripts([old._nid]); return old._nid;",
            )
            .unwrap();
        page.suspend_js();

        page.url = Some(url::Url::parse("http://example.com/new.html").unwrap());
        page.dom = Some(parse_html(
            "<html><head></head><body data-fresh-runs=0><script id=fresh>document.body.setAttribute('data-fresh-runs', '1')</script></body></html>",
        ));
        page.init_js();
        let result = page
            .js
            .as_mut()
            .unwrap()
            .evaluate(
                "var check = true; document.head.appendChild(document.getElementById('fresh')); return document.body.getAttribute('data-fresh-runs');",
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("1"));
    }

    fn import_map_test_page(name: &str, base: &str, html: &str) -> super::Page {
        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            name.to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new(name.to_string(), context);
        page.url = Some(url::Url::parse(&format!("{}/app/index.html", base)).unwrap());
        page.dom = Some(parse_html(html));
        page.init_js();
        page
    }

    #[tokio::test(flavor = "current_thread")]
    async fn module_graph_and_evaluation_share_one_active_budget() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let length = stream.read(&mut request).unwrap();
            let path = String::from_utf8_lossy(&request[..length])
                .lines()
                .next()
                .and_then(|line| line.split_ascii_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            request_tx.send(path).unwrap();

            // Spend part of the module's allowance loading its graph. The
            // synchronous top-level work then fits in a freshly reset budget,
            // but cannot fit in the shared active load+evaluation budget.
            std::thread::sleep(std::time::Duration::from_millis(100));
            let body = "export const delayed = true;";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let base = format!("http://{address}");
        let mut page = import_map_test_page(
            "shared-module-budget",
            &base,
            r#"<html><head><script type="module">
                import "./delayed.js";
                globalThis.__shared_deadline_started = true;
                const until = Date.now() + 300;
                while (Date.now() < until) {}
                globalThis.__shared_deadline_completed = true;
            </script></head><body></body></html>"#,
        );
        page.execute_scripts_with_module_budget(Some(350)).await;

        assert_eq!(
            request_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/app/delayed.js",
        );
        let state = page
            .js
            .as_mut()
            .unwrap()
            .evaluate(
                "[globalThis.__shared_deadline_started === true, \
                  globalThis.__shared_deadline_completed === true]",
            )
            .unwrap();
        assert_eq!(
            state,
            serde_json::json!([true, false]),
            "evaluation must be terminated at the remaining shared deadline",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_module_does_not_spend_its_budget_waiting_for_deferred_script() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 2048];
                let length = stream.read(&mut request).unwrap();
                let path = String::from_utf8_lossy(&request[..length])
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                let body = if path.ends_with("deferred.js") {
                    "const until=Date.now()+500;while(Date.now()<until){}"
                } else {
                    "export const ready=true;"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let base = format!("http://{address}");
        let mut page = import_map_test_page(
            "module-queue-budget",
            &base,
            r#"<html><head>
                <script defer src="./deferred.js"></script>
                <script type="module">
                    import { ready } from "./quick.js";
                    globalThis.__queued_module_completed = ready;
                </script>
            </head><body></body></html>"#,
        );
        page.execute_scripts_with_module_budget(Some(300)).await;

        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__queued_module_completed === true")
                .unwrap(),
            serde_json::json!(true),
            "queue latency must not consume a module's active-work budget",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parser_import_map_before_first_module_controls_resolution() {
        let (base, requests) = spawn_parser_import_map_server(1);
        let mut page = import_map_test_page(
            "import-map-order",
            &base,
            r#"<html><head>
            <script type="importmap">{"imports":{"ordered":"./before.js"}}</script>
            <script type="module">
                import { value } from "ordered";
                globalThis.__parser_import_map_value = value;
            </script>
            <script type="importmap">{"imports":{"ordered":"./after.js"}}</script>
        </head><body></body></html>"#,
        );
        page.execute_scripts().await;

        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__parser_import_map_value")
                .unwrap(),
            serde_json::json!("before-first-module"),
        );
        assert_eq!(
            requests
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/app/before.js"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn later_import_map_adds_unrelated_rule_without_rebinding_resolved_rule() {
        let (base, requests) = spawn_parser_import_map_server(2);
        let mut page = import_map_test_page(
            "multiple-import-map-order",
            &base,
            r#"<html><head>
            <script type="importmap">{"imports":{"fixed":"./before.js"}}</script>
            <script type="module">
                import { value } from "fixed";
                globalThis.__first_map_value = value;
            </script>
            <script type="importmap">{"imports":{"fixed":"./after.js","later":"./later.js"}}</script>
            <script type="module">
                import { value as fixed } from "fixed";
                import { value as later } from "later";
                globalThis.__later_map_values = [fixed, later];
            </script>
        </head><body></body></html>"#,
        );
        page.execute_scripts().await;

        let js = page.js.as_mut().unwrap();
        assert_eq!(
            js.evaluate("globalThis.__first_map_value").unwrap(),
            serde_json::json!("before-first-module")
        );
        assert_eq!(
            js.evaluate("globalThis.__later_map_values").unwrap(),
            serde_json::json!(["before-first-module", "later-map"])
        );
        let paths = (0..2)
            .map(|_| {
                requests
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(paths.contains(&"/app/before.js".to_string()), "{paths:?}");
        assert!(paths.contains(&"/app/later.js".to_string()), "{paths:?}");
        assert!(!paths.contains(&"/app/after.js".to_string()), "{paths:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn classic_dynamic_import_does_not_see_a_later_parser_import_map() {
        let (base, _requests) = spawn_parser_import_map_server(1);
        let mut page = import_map_test_page(
            "classic-before-import-map",
            &base,
            r#"<html><head>
            <script>
                import("too-late")
                    .then(() => globalThis.__classic_before_map = "resolved")
                    .catch(() => globalThis.__classic_before_map = "rejected");
            </script>
            <script type="importmap">{"imports":{"too-late":"./later.js"}}</script>
        </head><body></body></html>"#,
        );
        page.execute_scripts().await;
        page.settle_for_duration(500).await;
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__classic_before_map")
                .unwrap(),
            serde_json::json!("rejected"),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ready_async_classic_script_runs_before_a_later_parser_import_map() {
        let (base, requests) = spawn_parser_import_map_server(2);
        let mut page = import_map_test_page(
            "async-classic-before-map",
            &base,
            r#"<html><head>
            <script async src="./async.js"></script>
            <script type="importmap">{"imports":{"too-late":"./later.js"}}</script>
        </head><body></body></html>"#,
        );
        page.execute_scripts().await;
        page.settle_for_duration(500).await;
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__async_before_map")
                .unwrap(),
            serde_json::json!("rejected"),
        );
        assert_eq!(
            requests
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/app/async.js"
        );
        assert!(requests.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dynamically_inserted_import_map_controls_later_dynamic_import() {
        let (base, requests) = spawn_parser_import_map_server(1);
        let mut page = import_map_test_page(
            "dynamic-import-map",
            &base,
            r#"<html><head></head><body>
            <script>
                const map = document.createElement("script");
                map.type = "importmap";
                map.textContent = JSON.stringify({imports:{dynamicName:"./later.js"}});
                document.head.appendChild(map);
                import("dynamicName")
                    .then(module => globalThis.__dynamic_map_value = module.value)
                    .catch(error => globalThis.__dynamic_map_value = error.message);
            </script>
        </body></html>"#,
        );
        page.execute_scripts().await;
        page.settle_for_duration(500).await;
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__dynamic_map_value")
                .unwrap(),
            serde_json::json!("later-map"),
        );
        assert_eq!(
            requests
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/app/later.js"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preload_dynamic_script_delays_load_but_not_dom_content_loaded() {
        let (base, requests) = spawn_delayed_classic_script_server(
            std::time::Duration::from_millis(150),
            "globalThis.__lifecycleOrder.push('dynamic-exec');",
        );
        let html = format!(
            r#"<html><head></head><body><script>
                globalThis.__lifecycleOrder = [];
                document.addEventListener('DOMContentLoaded', () =>
                    globalThis.__lifecycleOrder.push('dom-content-loaded'));
                window.onload = () =>
                    globalThis.__lifecycleOrder.push('window-onload');
                window.addEventListener('load', () =>
                    globalThis.__lifecycleOrder.push('window-load'));
                const script = document.createElement('script');
                script.src = '{base}/preload-dynamic.js';
                script.onload = () => globalThis.__lifecycleOrder.push('script-load');
                document.head.appendChild(script);
            </script></body></html>"#,
        );
        let mut page = import_map_test_page(
            "preload-dynamic-lifecycle",
            "http://127.0.0.1:9",
            &html,
        );

        page.execute_scripts().await;

        assert_eq!(
            requests
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/preload-dynamic.js",
        );
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__lifecycleOrder")
                .unwrap(),
            serde_json::json!([
                "dom-content-loaded",
                "dynamic-exec",
                "script-load",
                "window-onload",
                "window-load"
            ]),
            "dynamic async scripts gate load, not DOMContentLoaded",
        );
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate(
                    "globalThis.__lifecycleOrder.filter(value => value === 'window-onload').length",
                )
                .unwrap(),
            serde_json::json!(1.0),
            "window.onload must fire exactly once",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn load_delaying_script_progresses_through_continuously_ready_timer_work() {
        let (base, requests) = spawn_delayed_classic_script_server(
            std::time::Duration::from_millis(75),
            "globalThis.__fairDynamicRan = true;",
        );
        let html = format!(
            r#"<html><head></head><body><script>
                globalThis.__schedulerTicks = 0;
                setInterval(() => globalThis.__schedulerTicks++, 0);
                const script = document.createElement('script');
                script.src = '{base}/fair-dynamic.js';
                script.onload = () => globalThis.__fairDynamicLoaded = true;
                document.head.appendChild(script);
            </script></body></html>"#,
        );
        let mut page = import_map_test_page(
            "load-delayer-scheduler-fairness",
            "http://127.0.0.1:9",
            &html,
        );
        let started = std::time::Instant::now();

        page.execute_scripts().await;

        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "continuous ready work must not starve a load-delaying fetch; elapsed={elapsed:?}",
        );
        assert_eq!(
            requests
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/fair-dynamic.js",
        );
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate(
                    "[globalThis.__fairDynamicRan === true, \
                     globalThis.__fairDynamicLoaded === true, \
                     globalThis.__schedulerTicks > 0]",
                )
                .unwrap(),
            serde_json::json!([true, true, true]),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn load_delaying_script_driver_respects_absolute_deadline() {
        let (base, requests) = spawn_delayed_classic_script_server(
            std::time::Duration::from_secs(1),
            "globalThis.__lateDynamicRan = true;",
        );
        let mut page = import_map_test_page(
            "load-delayer-deadline",
            "http://127.0.0.1:9",
            "<html><head></head><body></body></html>",
        );
        page.js
            .as_mut()
            .unwrap()
            .execute_script(
                "install-load-delayer",
                &format!(
                    "globalThis.__documentReadyState__ = 'loading'; \
                     const script = document.createElement('script'); \
                     script.src = '{base}/slow-dynamic.js'; \
                     document.head.appendChild(script);",
                ),
            )
            .unwrap();
        assert!(page
            .js
            .as_mut()
            .unwrap()
            .has_pending_load_delaying_scripts());
        let started = std::time::Instant::now();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(125);

        let completed = super::Page::drive_load_delaying_scripts(
            page.js.as_mut().unwrap(),
            deadline,
        )
        .await;

        let elapsed = started.elapsed();
        assert!(!completed, "the delayed resource must exceed the deadline");
        assert!(
            elapsed >= std::time::Duration::from_millis(100)
                && elapsed < std::time::Duration::from_millis(500),
            "the driver must honor its absolute wall-clock bound; elapsed={elapsed:?}",
        );
        assert!(page
            .js
            .as_mut()
            .unwrap()
            .has_pending_load_delaying_scripts());
        assert_eq!(
            requests
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/slow-dynamic.js",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn post_load_dynamic_script_waits_only_when_caller_requests_settle() {
        let (base, requests) = spawn_delayed_classic_script_server(
            std::time::Duration::from_millis(400),
            "globalThis.__postLoadDynamicRan = true;",
        );
        let html = format!(
            r#"<html><body><script>
                window.addEventListener('load', () => {{
                    const script = document.createElement('script');
                    script.src = '{base}/post-load.js';
                    document.head.appendChild(script);
                }});
            </script></body></html>"#,
        );
        let mut page = import_map_test_page("post-load-dynamic-lifecycle", &base, &html);
        let started = std::time::Instant::now();

        page.execute_scripts().await;

        let navigation_elapsed = started.elapsed();
        assert!(
            navigation_elapsed < std::time::Duration::from_millis(300),
            "post-load enhancement must not extend navigation; elapsed={navigation_elapsed:?}",
        );
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate(
                    "[document.readyState, globalThis.__postLoadDynamicRan === true, \
                     globalThis.__obscura_hasPendingDynamicScripts(), \
                     globalThis.__obscura_hasPendingLoadDelayingScripts()]",
                )
                .unwrap(),
            serde_json::json!(["complete", false, true, false]),
            "a script prepared by load is pending enhancement work, not a load blocker",
        );

        page.settle_for_duration(700).await;

        assert_eq!(
            requests
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/post-load.js",
        );
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__postLoadDynamicRan === true")
                .unwrap(),
            serde_json::json!(true),
            "an explicit caller settle must drive post-load script completion",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timer_hydration_runs_during_explicit_adaptive_settle_not_navigation_load() {
        let mut page = import_map_test_page(
            "timer-hydration-lifecycle",
            "http://example.com",
            r#"<html><body><main id="app">Server shell</main><script>
                window.addEventListener('load', () => {
                    setTimeout(() => {
                        document.getElementById('app').textContent = 'Hydrated app';
                        document.body.setAttribute('data-hydrated', 'true');
                    }, 80);
                });
            </script></body></html>"#,
        );

        page.execute_scripts().await;
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate(
                    "[document.readyState, document.body.getAttribute('data-hydrated'), \
                     document.getElementById('app').textContent]",
                )
                .unwrap(),
            serde_json::json!(["complete", null, "Server shell"]),
            "navigation load observes load semantics without inventing a timer settle",
        );

        page.settle(500).await;
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate(
                    "[document.body.getAttribute('data-hydrated'), \
                     document.getElementById('app').textContent]",
                )
                .unwrap(),
            serde_json::json!(["true", "Hydrated app"]),
            "the automation caller's adaptive settle must retain timer hydration",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lazy_module_graph_is_post_load_work_until_caller_settles() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 2048];
                let length = stream.read(&mut request).unwrap();
                let request_text = String::from_utf8_lossy(&request[..length]);
                let path = request_text
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/");
                let body = match path {
                    "/app/lazy.js" => {
                        "import { ready } from './lazy-child.js'; export { ready };"
                    }
                    "/app/lazy-child.js" => {
                        // Cross the lifecycle's 500ms fast-settle floor on a
                        // descendant edge. deno_core must propagate the lazy
                        // graph marker beyond its root for this to stay alive.
                        std::thread::sleep(std::time::Duration::from_millis(700));
                        "export const ready = 'lazy-ready';"
                    }
                    unexpected => panic!("unexpected module request: {unexpected}"),
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let base = format!("http://{address}");
        let mut page = import_map_test_page(
            "lazy-module-readiness",
            &base,
            r#"<html><body><script>
                import("./lazy.js").then(module => {
                    document.body.setAttribute("data-lazy-state", module.ready);
                });
            </script></body></html>"#,
        );
        let started = std::time::Instant::now();
        page.execute_scripts().await;

        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "dynamic import() must not become an implicit navigation settle",
        );
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("document.body.getAttribute('data-lazy-state')")
                .unwrap(),
            serde_json::Value::Null,
        );

        page.settle_for_duration(1_000).await;

        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("document.body.getAttribute('data-lazy-state')")
                .unwrap(),
            serde_json::json!("lazy-ready"),
            "an explicit caller settle must drive the lazy module graph",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_fetch_does_not_extend_dynamic_module_settle() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = accepted_tx.send(());
            let mut request = [0u8; 2048];
            let length = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..length])
                .starts_with("GET /app/analytics "));
            std::thread::sleep(std::time::Duration::from_secs(2));
            let body = "{}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes());
        });

        let base = format!("http://{address}");
        let html = format!(
            r#"<html><body><script>
                globalThis.__analyticsStarted = true;
                fetch("{base}/app/analytics").catch(error => {{
                    globalThis.__analyticsError = error.message;
                }});
            </script></body></html>"#,
        );
        let mut page = import_map_test_page(
            "ordinary-fetch-readiness",
            &base,
            &html,
        );
        let started = std::time::Instant::now();
        page.execute_scripts().await;
        let elapsed = started.elapsed();

        assert!(
            accepted_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_ok(),
            "ordinary fetch fixture must actually start its network request",
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1_500),
            "ordinary fetch/XHR must retain the fast settle path; elapsed={elapsed:?}",
        );
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__analyticsStarted")
                .unwrap(),
            serde_json::json!(true),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dynamic_import_map_uses_live_document_base_at_insertion() {
        let (base, requests) = spawn_parser_import_map_server(1);
        let mut page = import_map_test_page(
            "dynamic-import-map-base",
            &base,
            r#"<html><head><base href="/old/"></head><body>
            <script>
                document.querySelector("base").setAttribute("href", "/app/");
                const map = document.createElement("script");
                map.type = "importmap";
                map.textContent = JSON.stringify({imports:{liveBase:"./later.js"}});
                document.head.appendChild(map);
                import("liveBase")
                    .then(module => globalThis.__dynamic_map_base = module.value)
                    .catch(error => globalThis.__dynamic_map_base = error.message);
            </script>
        </body></html>"#,
        );
        page.execute_scripts().await;
        page.settle_for_duration(500).await;
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__dynamic_map_base")
                .unwrap(),
            serde_json::json!("later-map"),
        );
        assert_eq!(
            requests
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/app/later.js"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn later_base_element_does_not_rebase_an_earlier_import_map() {
        let (base, requests) = spawn_parser_import_map_server(1);
        let mut page = import_map_test_page(
            "temporal-import-map-base",
            &base,
            r#"<html><head>
            <script type="importmap">{"imports":{"fixed":"./before.js"}}</script>
            <base href="/assets/">
            <script type="module">
                import { value } from "fixed";
                globalThis.__temporal_base_value = value;
            </script>
        </head><body></body></html>"#,
        );
        page.execute_scripts().await;
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("globalThis.__temporal_base_value")
                .unwrap(),
            serde_json::json!("before-first-module"),
        );
        assert_eq!(
            requests
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/app/before.js"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn page_transport_prefetches_once_and_capture_reuses_the_bytes() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0u8; 2048];
                        let read = stream.read(&mut request).unwrap_or(0);
                        let first = String::from_utf8_lossy(&request[..read])
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .to_string();
                        seen_tx.send(first).unwrap();
                        let body = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="#f00"/></svg>"##;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: image/svg+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        stream.write_all(response.as_bytes()).unwrap();
                        stream.write_all(body).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            "render-prefetch".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new("render-prefetch".to_string(), context);
        page.set_viewport((100.0, 80.0));
        let page_url = format!("http://{address}/page");
        let asset_network_url = format!("http://{address}/asset.svg");
        let asset_url = format!("{asset_network_url}#icon");
        let dom = parse_html(&format!(
            r#"<html><body><img src="{asset_url}" style="width:20px;height:10px"></body></html>"#
        ));
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(dom);
        runtime.set_url(&page_url);
        runtime.set_viewport(100.0, 80.0);
        runtime.run_page_init();
        page.js = Some(runtime);
        page.url = Some(url::Url::parse(&page_url).unwrap());

        assert_eq!(page.prepare_screenshot_resources(1_000).await, 1);
        assert_eq!(
            page.js
                .as_mut()
                .unwrap()
                .evaluate("document.querySelector('img').currentSrc")
                .unwrap(),
            serde_json::json!(asset_url),
            "cache/network fragment normalization must not alter currentSrc"
        );
        page.screenshot(page.viewport).expect("prefetched capture");
        assert!(seen_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
            .starts_with("GET /asset.svg "));
        assert!(
            seen_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "capture must not open a second synchronous renderer request"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn render_resource_deadline_does_not_negative_cache_cancelled_requests() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            std::thread::sleep(std::time::Duration::from_millis(100));
            let body = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"/>"##;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/svg+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
        });

        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            "render-deadline".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new("render-deadline".to_string(), context);
        page.set_viewport((100.0, 80.0));
        let page_url = format!("http://{address}/page");
        let asset_url = format!("http://{address}/slow.svg");
        let dom = parse_html(&format!(
            r#"<html><body><img src="{asset_url}"></body></html>"#
        ));
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(dom);
        runtime.set_url(&page_url);
        runtime.set_viewport(100.0, 80.0);
        runtime.run_page_init();
        page.js = Some(runtime);
        page.url = Some(url::Url::parse(&page_url).unwrap());

        assert_eq!(page.prepare_screenshot_resources(5).await, 0);
        assert!(
            !page
                .js
                .as_ref()
                .unwrap()
                .render_resource_is_known(&asset_url),
            "a deadline-cancelled request must remain retryable"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn navigation_post_script_warmup_seeds_dynamic_images_and_fonts() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let read = stream.read(&mut request).unwrap_or(0);
                let path = String::from_utf8_lossy(&request[..read])
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                seen_tx.send(path.clone()).unwrap();
                let (content_type, body): (&str, &[u8]) = match path.as_str() {
                    "/page" => (
                        "text/html",
                        br#"<!doctype html><html><head></head><body><script>
                            const image = document.createElement('img');
                            image.src = '/dynamic.svg';
                            document.body.appendChild(image);
                            const style = document.createElement('style');
                            style.textContent = "@font-face{font-family:Dynamic;src:url('/dynamic.woff2')}body{font-family:Dynamic}";
                            document.head.appendChild(style);
                        </script></body></html>"#,
                    ),
                    "/dynamic.svg" => (
                        "image/svg+xml",
                        br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#,
                    ),
                    "/dynamic.woff2" => ("font/woff2", b"not-a-real-font"),
                    _ => ("text/plain", b"not found"),
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.write_all(body).unwrap();
            }
        });

        let context = std::sync::Arc::new(crate::BrowserContext::with_storage_and_network(
            "dynamic-render-warmup".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = super::Page::new("dynamic-render-warmup".to_string(), context);
        let page_url = format!("http://{address}/page");
        page.navigate(&page_url).await.unwrap();

        let mut paths = (0..3)
            .map(|_| seen_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap())
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "/dynamic.svg".to_string(),
                "/dynamic.woff2".to_string(),
                "/page".to_string(),
            ]
        );
        let js = page.js.as_ref().expect("navigation runtime");
        assert!(js.render_resource_is_known(&format!(
            "http://{address}/dynamic.svg"
        )));
        assert!(js.render_resource_is_known(&format!(
            "http://{address}/dynamic.woff2"
        )));
    }

    #[cfg(feature = "render")]
    #[test]
    fn page_screenshot_uses_the_live_window_scroll_offset() {
        let context = std::sync::Arc::new(crate::BrowserContext::new("scroll-test".to_string()));
        let mut page = super::Page::new("scroll-page".to_string(), context);
        page.set_viewport((100.0, 80.0));

        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:80px;background:#ff0000"></div>
                <div id="second" style="height:80px;background:#0000ff"></div>
                <div style="position:fixed;left:0;top:0;width:20px;height:20px;background:#00ff00"></div>
            </body></html>"#,
        );
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(dom);
        runtime.set_url("https://example.test/scroll");
        runtime.set_viewport(100.0, 80.0);
        runtime.run_page_init();
        page.js = Some(runtime);
        page.url = Some(url::Url::parse("https://example.test/scroll").unwrap());

        let before = page.screenshot(page.viewport).expect("top screenshot");
        assert_eq!(
            page.evaluate(
                "return (document.getElementById('second').scrollIntoView(), window.scrollY)"
            )
            .as_f64(),
            Some(80.0)
        );
        let after = page.screenshot(page.viewport).expect("scrolled screenshot");

        assert_ne!(
            before, after,
            "Page screenshot must paint the scrolled viewport"
        );
        assert_eq!(
            page.js.as_ref().expect("runtime").scroll_offset(),
            (0.0, 80.0)
        );
    }

    #[test]
    fn truncate_never_splits_a_multibyte_char() {
        // A caller-supplied expression whose byte 80 lands inside a multi-byte
        // char would make `&expression[..80]` panic; the helper truncates safely.
        let s = format!("{}€tail", "a".repeat(79));
        assert!(!s.is_char_boundary(80), "setup: byte 80 splits the € char");
        let t = truncate_on_char_boundary(&s, 80);
        assert!(s.starts_with(t));
        assert_eq!(t.len(), 79, "should stop right before the € char");
        assert_eq!(truncate_on_char_boundary("short", 80), "short");
    }

    #[test]
    fn parse_import_url_extracts_url_forms() {
        for (source, expected_url) in [
            (" url(\"basic.css\")", "basic.css"),
            (" url(basic.css)", "basic.css"),
            (" \"basic.css\"", "basic.css"),
            (" 'theme.css'", "theme.css"),
            (" URL('x.css')", "x.css"),
        ] {
            assert_eq!(
                parse_import_url(source),
                Some(StylesheetImport {
                    url: expected_url.to_string(),
                    media: None,
                })
            );
        }
    }

    #[test]
    fn parse_import_url_preserves_print_and_color_scheme_media() {
        assert_eq!(
            parse_import_url("url(\"p.css\") print"),
            Some(StylesheetImport {
                url: "p.css".to_string(),
                media: Some("print".to_string()),
            })
        );
        assert_eq!(
            parse_import_url("url(\"d.css\") (prefers-color-scheme: dark)"),
            Some(StylesheetImport {
                url: "d.css".to_string(),
                media: Some("(prefers-color-scheme: dark)".to_string()),
            })
        );
        assert_eq!(
            parse_import_url("url(\"a.css\") print, screen"),
            Some(StylesheetImport {
                url: "a.css".to_string(),
                media: Some("print, screen".to_string()),
            })
        );
    }

    #[test]
    fn split_css_imports_pulls_imports_and_strips_them() {
        let css = "@import url(\"basic.css\");\nbody { color: red; }";
        let (imports, stripped) = split_css_imports(css);
        assert_eq!(
            imports,
            vec![StylesheetImport {
                url: "basic.css".to_string(),
                media: None,
            }]
        );
        assert!(!stripped.contains("@import"));
        assert!(stripped.contains("body { color: red; }"));
    }

    #[test]
    fn split_css_imports_leaves_import_free_css_untouched() {
        let css = "body { color: red; }";
        let (imports, stripped) = split_css_imports(css);
        assert!(imports.is_empty());
        assert_eq!(stripped, css);
    }

    #[test]
    fn materialized_import_graph_retains_print_condition_and_import_base() {
        let root_url = url::Url::parse("https://example.test/css/root.css").unwrap();
        let print_url = root_url.join("print/print.css").unwrap();
        let mut sheets = std::collections::HashMap::new();
        sheets.insert(
            root_url.to_string(),
            LoadedStylesheet {
                response_url: root_url.clone(),
                imports: vec![StylesheetImport {
                    url: "print/print.css".to_string(),
                    media: Some("print".to_string()),
                }],
                rules: ".root{color:red}".to_string(),
            },
        );
        sheets.insert(
            print_url.to_string(),
            LoadedStylesheet {
                response_url: print_url.clone(),
                imports: Vec::new(),
                rules: ".print{background:url(../mark.svg)}".to_string(),
            },
        );
        let aliases = std::collections::HashMap::from([
            (root_url.to_string(), root_url.to_string()),
            (print_url.to_string(), print_url.to_string()),
        ]);
        let materialized = materialize_stylesheet_graph(
            root_url.as_str(),
            &sheets,
            &aliases,
            &mut std::collections::HashSet::new(),
        )
        .expect("materialized graph");

        assert!(materialized.starts_with("@media print {\n"));
        assert!(materialized.contains(
            r#".print{background:url("https://example.test/css/mark.svg")}"#
        ));
        assert!(materialized.ends_with(".root{color:red}"));
    }

    #[test]
    fn stylesheet_asset_urls_keep_the_importing_sheets_base() {
        let base = url::Url::parse("https://example.com/css/theme/app.css").unwrap();
        let css = r#"
            .hero { background:url("../img/hero.png") }
            .icon { mask-image:URL('./icons/mark.svg') }
            .data { background:url("data:image/svg+xml,<svg></svg>") }
            .fragment { mask:url(#shape) }
            .copy::before { content:"url(../not-an-asset.png)" }
            /* url(../not-an-asset-either.png) */
        "#;
        let rebased = rebase_css_urls(css, &base);

        assert!(rebased.contains(r#"url("https://example.com/css/img/hero.png")"#));
        assert!(rebased.contains(r#"url("https://example.com/css/theme/icons/mark.svg")"#));
        assert!(rebased.contains(r#"url("data:image/svg+xml,<svg></svg>")"#));
        assert!(rebased.contains("url(#shape)"));
        assert!(rebased.contains(r#"content:"url(../not-an-asset.png)""#));
        assert!(rebased.contains("/* url(../not-an-asset-either.png) */"));
    }

    #[test]
    fn stylesheet_rel_token_selector_includes_preloaded_stylesheets() {
        let dom = parse_html(
            r#"<link rel="preload stylesheet" href="app.css">
               <link rel="preload" href="font.woff2">"#,
        );
        let links = dom
            .query_selector_all(r#"link[rel~="stylesheet"]"#)
            .expect("valid selector");
        assert_eq!(links.len(), 1);
        assert_eq!(
            dom.get_node(links[0])
                .and_then(|node| node.get_attribute("href").map(str::to_owned)),
            Some("app.css".to_string())
        );
    }

    #[test]
    fn media_gated_stylesheets_are_fetched_but_disabled_sheets_are_not() {
        let dom = parse_html(
            r#"<link rel="stylesheet" href="screen.css">
               <link rel="stylesheet" href="async.css" media="print"
                     onload="this.media='all'">
               <link rel="stylesheet" href="dark.css"
                     media="(prefers-color-scheme: dark)">
               <link rel="stylesheet" href="disabled.css" disabled>"#,
        );

        assert_eq!(
            linked_stylesheet_requests(&dom),
            vec![
                (0, "screen.css".to_string()),
                (1, "async.css".to_string()),
                (2, "dark.css".to_string()),
            ]
        );
    }

    #[test]
    fn print_media_onload_can_activate_a_fetched_stylesheet() {
        let dom = parse_html(
            r#"<html><head>
                <link id="async" rel="stylesheet" href="async.css" media="print"
                      onload="this.media='all';this.setAttribute('data-loaded','yes')">
            </head><body></body></html>"#,
        );
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(dom);
        runtime.run_page_init();
        runtime
            .execute_script(
                "<async-sheet>",
                &materialize_linked_stylesheet_script(0, ".target{color:red}"),
            )
            .expect("load and materialize async linked sheet");

        let state = runtime
            .with_dom(|dom| {
                let link = dom
                    .query_selector("#async")
                    .expect("valid selector")
                    .expect("async link");
                let styles = dom
                    .query_selector_all("style[data-obscura-external-stylesheets]")
                    .expect("valid selector");
                (
                    dom.get_node(link)
                        .and_then(|node| node.get_attribute("data-loaded").map(str::to_owned)),
                    styles.first().map(|&nid| dom.text_content(nid)),
                )
            })
            .expect("live DOM");

        assert_eq!(
            state.0.as_deref(),
            Some("yes"),
            "link load handler must run"
        );
        assert_eq!(
            state.1.as_deref(),
            Some(".target{color:red}"),
            "the handler's `this.media = 'all'` must activate the sheet"
        );
    }

    #[test]
    fn true_print_stylesheet_loads_and_remains_media_gated() {
        let dom = parse_html(
            r#"<html><head>
                <link id="print" rel="stylesheet" href="print.css" media="print"
                      onload="this.setAttribute('data-loaded','yes')">
            </head><body></body></html>"#,
        );
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(dom);
        runtime.run_page_init();
        runtime
            .execute_script(
                "<print-sheet>",
                &materialize_linked_stylesheet_script(0, "body{display:none}"),
            )
            .expect("finish print linked sheet load");

        let state = runtime
            .with_dom(|dom| {
                let link = dom
                    .query_selector("#print")
                    .expect("valid selector")
                    .expect("print link");
                (
                    dom.get_node(link)
                        .and_then(|node| node.get_attribute("data-loaded").map(str::to_owned)),
                    dom.query_selector("style[data-obscura-external-stylesheets]")
                        .expect("valid selector")
                        .and_then(|style| {
                            dom.get_node(style)
                                .and_then(|node| node.get_attribute("media").map(str::to_owned))
                        }),
                )
            })
            .expect("live DOM");

        assert_eq!(
            state.0.as_deref(),
            Some("yes"),
            "print link still fires load"
        );
        assert_eq!(
            state.1.as_deref(),
            Some("print"),
            "the fetched sheet must remain available for PDF print selection"
        );
    }

    #[test]
    fn materialized_linked_stylesheets_expose_link_owned_cssom_with_origin_security() {
        let dom = parse_html(
            r#"<html><head>
                <link id="same" rel="stylesheet" href="/assets/app.css" title="app">
                <style id="inline">.inline { color: green }</style>
                <link id="cross" rel="stylesheet" href="https://cdn.example.test/theme.css">
            </head><body></body></html>"#,
        );
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(dom);
        runtime.set_url("https://example.test/products/widget");
        runtime.run_page_init();
        runtime
            .execute_script(
                "<same-origin-sheet>",
                &materialize_linked_stylesheet_script(
                    0,
                    ".app { color: red } .wide { width: 20px }",
                ),
            )
            .expect("materialize same-origin linked sheet");
        runtime
            .execute_script(
                "<cross-origin-sheet>",
                &materialize_linked_stylesheet_script(1, ".secret { color: purple }"),
            )
            .expect("materialize cross-origin linked sheet");

        let result = runtime
            .evaluate(
                r#"
                (() => {
                    const list = document.styleSheets;
                    const same = document.getElementById('same');
                    const inline = document.getElementById('inline');
                    const cross = document.getElementById('cross');
                    const sameSheet = same.sheet;
                    const sameRules = sameSheet.cssRules;
                    const crossSheet = cross.sheet;
                    const security = [];
                    for (const operation of [
                        () => crossSheet.cssRules,
                        () => crossSheet.rules,
                        () => crossSheet.insertRule('.leak {}', 0),
                        () => crossSheet.deleteRule(0),
                        () => crossSheet.replaceSync('.leak {}'),
                    ]) {
                        try { operation(); security.push('missing'); }
                        catch (error) { security.push(error && error.name); }
                    }
                    sameSheet.insertRule('.added { height: 9px }', sameRules.length);
                    const source = document.querySelector(
                        'style[data-obscura-external-stylesheets]'
                    );
                    return {
                        stableList: list === document.styleSheets,
                        length: list.length,
                        order: [list[0] === sameSheet, list[1] === inline.sheet,
                                list[2] === crossSheet],
                        sameIdentity: same.sheet === sameSheet,
                        owner: sameSheet.ownerNode === same,
                        href: sameSheet.href,
                        title: sameSheet.title,
                        rulesIdentity: sameSheet.cssRules === sameRules,
                        rules: Array.from(sameRules, rule => rule.selectorText),
                        sourceUpdated: source.textContent.includes('.added'),
                        crossOwner: crossSheet.ownerNode === cross,
                        crossHref: crossSheet.href,
                        bridgeSheetsHidden: same.nextSibling.sheet === null
                            && cross.nextSibling.sheet === null,
                        security,
                    };
                })()
                "#,
            )
            .expect("inspect linked stylesheet CSSOM");

        assert_eq!(
            result,
            serde_json::json!({
                "stableList": true,
                "length": 3,
                "order": [true, true, true],
                "sameIdentity": true,
                "owner": true,
                "href": "https://example.test/assets/app.css",
                "title": "app",
                "rulesIdentity": true,
                "rules": [".app", ".wide", ".added"],
                "sourceUpdated": true,
                "crossOwner": true,
                "crossHref": "https://cdn.example.test/theme.css",
                "bridgeSheetsHidden": true,
                "security": ["SecurityError", "SecurityError", "SecurityError",
                             "SecurityError", "SecurityError"],
            })
        );
    }

    #[test]
    fn external_stylesheets_keep_their_positions_between_inline_sheets() {
        let dom = parse_html(
            r#"<html><head>
                <link rel="stylesheet" href="first.css">
                <style data-name="inline">.target{height:20px}</style>
                <link rel="preload stylesheet" href="second.css">
            </head><body></body></html>"#,
        );
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(dom);
        runtime.run_page_init();
        runtime
            .execute_script(
                "<first-sheet>",
                &materialize_linked_stylesheet_script(0, ".target{height:10px}"),
            )
            .expect("materialize first linked sheet");
        runtime
            .execute_script(
                "<second-sheet>",
                &materialize_linked_stylesheet_script(1, ".target{height:30px}"),
            )
            .expect("materialize second linked sheet");

        let sheet_text = runtime
            .with_dom(|dom| {
                dom.query_selector_all("style")
                    .expect("valid selector")
                    .into_iter()
                    .map(|nid| dom.text_content(nid))
                    .collect::<Vec<_>>()
            })
            .expect("live DOM");
        assert_eq!(
            sheet_text,
            vec![
                ".target{height:10px}",
                ".target{height:20px}",
                ".target{height:30px}",
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parser_script_body_replacement_survives_navigation() {
        let mut page = client_replacement_page("parser-client-replacement", false);
        let target = page.url_string();

        page.navigate(&target)
            .await
            .expect("navigate replacement page");

        assert_client_replacement_survived(&mut page);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timer_body_replacement_survives_settle() {
        let mut page = client_replacement_page("timer-client-replacement", true);
        let target = page.url_string();
        page.navigate(&target)
            .await
            .expect("navigate deferred replacement page");

        let before_timer = page
            .js
            .as_mut()
            .expect("page runtime")
            .evaluate(
                "var scheduleClientReplacement = true; window.dispatchEvent(new Event('mount-client')); return !!document.getElementById('ssr');",
            )
            .expect("schedule client replacement");
        assert_eq!(before_timer, serde_json::json!(true));

        page.settle(100).await;

        assert_client_replacement_survived(&mut page);
    }

    #[cfg(feature = "render")]
    #[test]
    fn settle_resource_warmup_uses_only_remaining_absolute_budget() {
        assert_eq!(
            remaining_settle_resource_warmup_ms(
                1_000,
                std::time::Duration::from_millis(250),
                1_000,
            ),
            750
        );
        assert_eq!(
            remaining_settle_resource_warmup_ms(
                1_000,
                std::time::Duration::from_millis(250),
                100,
            ),
            100
        );
        assert_eq!(
            remaining_settle_resource_warmup_ms(
                1_000,
                std::time::Duration::from_micros(999_500),
                1_000,
            ),
            0,
            "a sub-millisecond remainder cannot safely fund a millisecond timeout"
        );
        assert_eq!(
            remaining_settle_resource_warmup_ms(
                1_000,
                std::time::Duration::from_millis(1_001),
                1_000,
            ),
            0
        );
    }

    #[test]
    fn url_matches_cdp_pattern_handles_wildcards_across_url_parts() {
        assert!(url_matches_cdp_pattern(
            "*://*.gstatic.com/*.woff2",
            "https://fonts.gstatic.com/s/inter/v18/UcCO3FwrK3iLTcviYwYZ8UA3.woff2",
        ));
        assert!(url_matches_cdp_pattern(
            "*://*.google.com/maps/vt/*",
            "https://www.google.com/maps/vt/pb=!1m4!1m3",
        ));
        assert!(url_matches_cdp_pattern(
            "https://example.com/assets/*",
            "https://example.com/assets/app.js",
        ));
        assert!(!url_matches_cdp_pattern(
            "https://example.com/assets/*",
            "https://cdn.example.com/assets/app.js",
        ));
        assert!(!url_matches_cdp_pattern(
            "*://*.gstatic.com/*.woff2",
            "https://fonts.gstatic.com/s/inter/v18/font.woff",
        ));
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PageError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    /// A page kept triggering its own navigations until the chain's limit
    /// was exhausted. HTTP 3xx redirects are followed one layer down, in
    /// obscura-net, and report `ObscuraNetError::TooManyRedirects`.
    #[error("Too many client-initiated navigations, the chain reached its limit of {0} documents")]
    TooManyClientNavigations(usize),
}

impl From<ObscuraNetError> for PageError {
    fn from(e: ObscuraNetError) -> Self {
        PageError::NetworkError(e.to_string())
    }
}

/// Apply Chromium's charset-aware CDP policy to Page-owned response bodies.
/// The JS fetch store keeps the same small private policy in obscura-js; sharing
/// it would expose an engine-internal DevTools detail as public Rust API.
fn stored_devtools_document_response_body(
    body: &[u8],
    content_type: Option<&str>,
) -> StoredResponseBody {
    if let Some(body) = decode_devtools_document_response_body(body, content_type) {
        StoredResponseBody {
            body,
            base64_encoded: false,
        }
    } else {
        StoredResponseBody {
            body: BASE64.encode(body),
            base64_encoded: true,
        }
    }
}

fn decode_devtools_document_response_body(
    body: &[u8],
    content_type: Option<&str>,
) -> Option<String> {
    if body.is_empty() {
        return Some(String::new());
    }

    let Some(content_type) = content_type else {
        return obscura_net::decode_with_label("windows-1252", body, true, false);
    };
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    let decoder = document_devtools_text_decoder(&mime)?;
    let explicit_charset = content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("charset") {
            return None;
        }
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        (!value.is_empty()).then_some(value)
    });
    let encoding = if let Some(charset) = explicit_charset {
        charset
    } else if matches!(decoder, DocumentDevtoolsTextDecoder::Html) {
        obscura_net::encoding::detect_encoding(body, Some(content_type)).0.name()
    } else if matches!(decoder, DocumentDevtoolsTextDecoder::Utf8) {
        "utf-8"
    } else {
        "windows-1252"
    };
    obscura_net::decode_with_label(encoding, body, true, false)
}

#[derive(Clone, Copy)]
enum DocumentDevtoolsTextDecoder {
    Html,
    Utf8,
    Plain,
}

fn document_devtools_text_decoder(mime: &str) -> Option<DocumentDevtoolsTextDecoder> {
    if mime == "text/html" {
        return Some(DocumentDevtoolsTextDecoder::Html);
    }
    if matches!(
        mime,
        "application/javascript"
            | "application/ecmascript"
            | "application/x-ecmascript"
            | "application/x-javascript"
            | "text/ecmascript"
            | "text/javascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
            | "text/x-javascript"
    ) || matches!(mime, "application/json" | "text/json")
        || mime.ends_with("+json")
        || matches!(mime, "application/xml" | "text/xml")
        || (mime.starts_with("application/") && mime.ends_with("+xml"))
    {
        return Some(DocumentDevtoolsTextDecoder::Utf8);
    }
    (mime.starts_with("text/") && mime != "text/xsl")
        .then_some(DocumentDevtoolsTextDecoder::Plain)
}

fn response_body_entry_limit() -> usize {
    std::env::var("OBSCURA_NETWORK_BODY_BUFFER_ENTRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128)
}

fn response_body_byte_limit() -> usize {
    std::env::var("OBSCURA_NETWORK_BODY_BUFFER_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2 * 1024 * 1024)
}
