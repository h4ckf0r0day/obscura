use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub struct PausedRequest {
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub resource_type: String,
    pub resolver: tokio::sync::oneshot::Sender<FetchResolution>,
}

pub enum FetchResolution {
    Continue {
        url: Option<String>,
        method: Option<String>,
        headers: Option<HashMap<String, String>>,
        post_data: Option<Vec<u8>>,
    },
    Fulfill {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    Fail {
        reason: String,
    },
}

pub struct FetchInterceptState {
    pub enabled: bool,
    pub patterns: Vec<String>,
    pub session_id: Option<String>,
    pub paused: HashMap<String, PausedRequest>,
    request_counter: u64,
}

impl FetchInterceptState {
    pub fn new() -> Self {
        FetchInterceptState {
            enabled: false,
            patterns: Vec::new(),
            session_id: None,
            paused: HashMap::new(),
            request_counter: 0,
        }
    }

    pub fn next_request_id(&mut self) -> String {
        self.request_counter += 1;
        format!("interception-{}", self.request_counter)
    }

    pub fn should_pause_url(&self, url: &str) -> bool {
        self.enabled
            && (self.patterns.is_empty()
                || self
                    .patterns
                    .iter()
                    .any(|pattern| cdp_url_pattern_matches(pattern, url)))
    }
}

pub fn cdp_url_pattern_matches(pattern: &str, url: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern == "*" {
        return true;
    }

    wildcard_match(pattern, url)
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star = None;
    let mut star_text = 0usize;

    while t < text.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_text = t;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => {
            let patterns = params
                .get("patterns")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| {
                            p.get("urlPattern")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec!["*".to_string()]);

            ctx.fetch_intercept.enabled = true;
            ctx.fetch_intercept.patterns = patterns.clone();
            ctx.fetch_intercept.session_id = session_id.clone();
            let tx_clone = ctx.intercept_tx.clone();
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.intercept_enabled = true;
                page.intercept_patterns = patterns.clone();
                if let Some(tx) = tx_clone {
                    page.set_intercept_tx(tx, patterns.clone());
                }
            }

            tracing::info!("Fetch interception enabled");
            Ok(json!({}))
        }
        "disable" => {
            ctx.fetch_intercept.enabled = false;
            ctx.fetch_intercept.patterns.clear();
            ctx.fetch_intercept.session_id = None;
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.intercept_enabled = false;
                page.intercept_patterns.clear();
                page.intercept_block_patterns.clear();
            }
            let paused: Vec<_> = ctx.fetch_intercept.paused.drain().collect();
            for (_, req) in paused {
                let _ = req.resolver.send(FetchResolution::Continue {
                    url: None,
                    method: None,
                    headers: None,
                    post_data: None,
                });
            }
            Ok(json!({}))
        }
        "continueRequest" => {
            let request_id = params
                .get("requestId")
                .and_then(|v| v.as_str())
                .ok_or("requestId required")?;

            let post_data = decode_optional_base64(params.get("postData"))?;
            if let Some(paused) = ctx.fetch_intercept.paused.remove(request_id) {
                let _ = paused.resolver.send(FetchResolution::Continue {
                    url: params
                        .get("url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    method: params
                        .get("method")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    headers: parse_cdp_headers(params.get("headers")),
                    post_data,
                });
            }
            Ok(json!({}))
        }
        "fulfillRequest" => {
            let request_id = params
                .get("requestId")
                .and_then(|v| v.as_str())
                .ok_or("requestId required")?;

            let status = params
                .get("responseCode")
                .and_then(|v| v.as_u64())
                .unwrap_or(200) as u16;
            let headers: HashMap<String, String> = params
                .get("responseHeaders")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|h| {
                            let name = h.get("name")?.as_str()?.to_string();
                            let value = h.get("value")?.as_str()?.to_string();
                            Some((name, value))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let body =
                decode_base64_body(params.get("body").and_then(|v| v.as_str()).unwrap_or(""))?;

            if let Some(paused) = ctx.fetch_intercept.paused.remove(request_id) {
                let _ = paused.resolver.send(FetchResolution::Fulfill {
                    status,
                    headers: headers.into_iter().collect(),
                    body,
                });
            }
            Ok(json!({}))
        }
        "failRequest" => {
            let request_id = params
                .get("requestId")
                .and_then(|v| v.as_str())
                .ok_or("requestId required")?;

            let reason = params
                .get("errorReason")
                .and_then(|v| v.as_str())
                .unwrap_or("Failed")
                .to_string();

            if let Some(paused) = ctx.fetch_intercept.paused.remove(request_id) {
                let _ = paused.resolver.send(FetchResolution::Fail { reason });
            }
            Ok(json!({}))
        }
        "getResponseBody" => {
            let request_id = params
                .get("requestId")
                .and_then(|v| v.as_str())
                .ok_or("requestId required")?;
            let body = ctx
                .network_response_bodies
                .lock()
                .await
                .get(request_id)
                .cloned()
                .ok_or_else(|| {
                    format!("No resource with given identifier found: {}", request_id)
                })?;
            Ok(body.cdp_value())
        }
        _ => Err(format!("Unknown Fetch method: {}", method)),
    }
}

fn parse_cdp_headers(value: Option<&Value>) -> Option<HashMap<String, String>> {
    let value = value?;
    if let Some(entries) = value.as_array() {
        let headers = entries
            .iter()
            .filter_map(|entry| {
                Some((
                    entry.get("name")?.as_str()?.to_string(),
                    entry.get("value")?.as_str()?.to_string(),
                ))
            })
            .collect::<HashMap<_, _>>();
        return Some(headers);
    }

    value.as_object().map(|map| {
        map.iter()
            .filter_map(|(name, value)| {
                value
                    .as_str()
                    .map(|value| (name.clone(), value.to_string()))
            })
            .collect()
    })
}

pub(crate) fn decode_base64_body(input: &str) -> Result<Vec<u8>, String> {
    BASE64
        .decode(input)
        .map_err(|error| format!("Invalid base64 body: {}", error))
}

pub(crate) fn decode_optional_base64(value: Option<&Value>) -> Result<Option<Vec<u8>>, String> {
    value
        .and_then(Value::as_str)
        .map(decode_base64_body)
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdp_url_pattern_supports_playwright_wildcards() {
        assert!(cdp_url_pattern_matches(
            "**/graphql/query*",
            "https://www.instagram.com/graphql/query/?doc_id=1"
        ));
        assert!(cdp_url_pattern_matches(
            "*://www.instagram.com/api/graphql*",
            "https://www.instagram.com/api/graphql"
        ));
        assert!(!cdp_url_pattern_matches(
            "**/graphql/query*",
            "https://www.instagram.com/static/bundle.js"
        ));
    }

    #[test]
    fn fetch_intercept_state_pauses_only_matching_urls() {
        let mut state = FetchInterceptState::new();
        state.enabled = true;
        state.patterns = vec!["**/graphql/query*".to_string()];

        assert!(state.should_pause_url("https://www.instagram.com/graphql/query/?variables=abc"));
        assert!(!state.should_pause_url("https://www.instagram.com/static/bundle.js"));
    }

    #[test]
    fn fetch_intercept_state_empty_patterns_match_all_when_enabled() {
        let mut state = FetchInterceptState::new();
        state.enabled = true;
        assert!(state.should_pause_url("https://www.instagram.com/anything"));

        state.enabled = false;
        assert!(!state.should_pause_url("https://www.instagram.com/anything"));
    }

    #[test]
    fn cdp_bodies_use_strict_standard_base64_and_preserve_binary() {
        assert_eq!(
            decode_base64_body("AP8BgA==").unwrap(),
            vec![0, 255, 1, 128]
        );
        assert!(decode_base64_body("not base64!").is_err());
        assert!(decode_base64_body("_-8=").is_err());
    }

    #[tokio::test]
    async fn malformed_continue_body_does_not_consume_paused_request() {
        let mut ctx = CdpContext::new();
        let (resolver, _receiver) = tokio::sync::oneshot::channel();
        ctx.fetch_intercept.paused.insert(
            "request-1".to_string(),
            PausedRequest {
                request_id: "request-1".to_string(),
                url: "https://example.com/upload".to_string(),
                method: "POST".to_string(),
                headers: HashMap::new(),
                resource_type: "Fetch".to_string(),
                resolver,
            },
        );

        let error = handle(
            "continueRequest",
            &json!({"requestId":"request-1","postData":"%%%"}),
            &mut ctx,
            &None,
        )
        .await
        .expect_err("malformed base64 must be rejected");

        assert!(error.contains("Invalid base64 body"));
        assert!(ctx.fetch_intercept.paused.contains_key("request-1"));
    }
}
