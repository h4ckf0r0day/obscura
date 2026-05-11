use std::collections::HashMap;

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
        post_data: Option<String>,
    },
    Fulfill {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    },
    Fail {
        reason: String,
    },
}

pub struct FetchInterceptState {
    pub enabled: bool,
    pub patterns: Vec<String>,
    pub paused: HashMap<String, PausedRequest>,
    request_counter: u64,
}

impl FetchInterceptState {
    pub fn new() -> Self {
        FetchInterceptState {
            enabled: false,
            patterns: Vec::new(),
            paused: HashMap::new(),
            request_counter: 0,
        }
    }

    pub fn next_request_id(&mut self) -> String {
        self.request_counter += 1;
        format!("interception-{}", self.request_counter)
    }
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
            let tx_clone = ctx.intercept_tx.clone();
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.intercept_enabled = true;
                page.intercept_block_patterns = patterns.clone();
                if let Some(tx) = tx_clone {
                    page.set_intercept_tx(tx);
                }
            }

            tracing::info!("Fetch interception enabled");
            Ok(json!({}))
        }
        "disable" => {
            ctx.fetch_intercept.enabled = false;
            ctx.fetch_intercept.patterns.clear();
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.intercept_enabled = false;
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

            if let Some(paused) = ctx.fetch_intercept.paused.remove(request_id) {
                let _ = paused.resolver.send(FetchResolution::Continue {
                    url: params.get("url").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    method: params.get("method").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    headers: None,
                    post_data: params.get("postData").and_then(|v| v.as_str()).map(|s| s.to_string()),
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
            let body = params
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

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
            Ok(json!({ "body": "", "base64Encoded": false }))
        }
        _ => Err(format!("Unknown Fetch method: {}", method)),
    }
}
