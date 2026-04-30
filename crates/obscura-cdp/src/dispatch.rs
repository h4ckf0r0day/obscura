use std::collections::HashMap;
use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};
use obscura_js::ops::{InterceptResolution, InterceptedRequest};
use serde_json::json;

use crate::domains;
use crate::domains::fetch::FetchInterceptState;
use crate::types::{CdpEvent, CdpRequest, CdpResponse};

pub struct CdpContext {
    pub pages: Vec<Page>,
    pub sessions: HashMap<String, String>, // session_id -> page_id
    pub pending_events: Vec<CdpEvent>,
    pub default_context: Arc<BrowserContext>,
    page_counter: u32,
    pub preload_scripts: Vec<(String, String)>, // (identifier, source)
    pub preload_counter: u32,
    // World names registered via Page.createIsolatedWorld. After every
    // navigation Obscura clears execution contexts (via
    // Runtime.executionContextsCleared) and must re-emit a
    // Runtime.executionContextCreated for each registered world, otherwise
    // Playwright/Puppeteer hang waiting for their utility world to come
    // back. Stored as plain Strings (not by-page) — for now we only model
    // a single page in CdpContext anyway.
    pub isolated_worlds: Vec<String>,
    pub fetch_intercept: FetchInterceptState,
    pub intercept_tx: Option<tokio::sync::mpsc::UnboundedSender<InterceptedRequest>>,
}

impl CdpContext {
    pub fn new() -> Self {
        Self::new_with_options(None, false)
    }

    pub fn new_with_proxy(proxy: Option<String>) -> Self {
        Self::new_with_options(proxy, false)
    }

    pub fn new_with_options(proxy: Option<String>, stealth: bool) -> Self {
        let default_context = Arc::new(BrowserContext::with_options(
            "default".to_string(),
            proxy,
            stealth,
        ));
        CdpContext {
            pages: Vec::new(),
            sessions: HashMap::new(),
            pending_events: Vec::new(),
            default_context,
            page_counter: 0,
            preload_scripts: Vec::new(),
            preload_counter: 0,
            fetch_intercept: FetchInterceptState::new(),
            intercept_tx: None,
            isolated_worlds: Vec::new(),
        }
    }

    pub fn create_page(&mut self) -> String {
        self.page_counter += 1;
        let page_id = format!("page-{}", self.page_counter);
        let mut page = Page::new(page_id.clone(), self.default_context.clone());
        page.navigate_blank();
        self.pages.push(page);
        page_id
    }

    pub fn get_page(&self, id: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    pub fn get_page_mut(&mut self, id: &str) -> Option<&mut Page> {
        self.pages.iter_mut().find(|p| p.id == id)
    }

    pub fn remove_page(&mut self, id: &str) {
        self.pages.retain(|p| p.id != id);
        self.sessions.retain(|_, v| v != id);
    }

    pub fn get_session_page(&self, session_id: &Option<String>) -> Option<&Page> {
        let page_id = session_id
            .as_ref()
            .and_then(|sid| self.sessions.get(sid))?;
        self.get_page(page_id)
    }

    pub fn get_session_page_mut(&mut self, session_id: &Option<String>) -> Option<&mut Page> {
        let page_id = session_id
            .as_ref()
            .and_then(|sid| self.sessions.get(sid))
            .cloned()?;

        let target_has_js = self.pages.iter().any(|p| p.id == page_id && p.has_js());

        if !target_has_js {
            for page in &mut self.pages {
                if page.id != page_id && page.has_js() {
                    page.suspend_js();
                    break;
                }
            }
            if let Some(target) = self.pages.iter_mut().find(|p| p.id == page_id) {
                target.resume_js();
            }
        }

        self.get_page_mut(&page_id)
    }
}

pub async fn dispatch(req: &CdpRequest, ctx: &mut CdpContext) -> CdpResponse {
    // headless_chrome (and older Puppeteer) wrap every CDP call inside
    // Target.sendMessageToTarget. Unwrap the inner message, dispatch it
    // recursively against the embedded session, and emit the inner
    // response as a Target.receivedMessageFromTarget event — the
    // legacy contract Chrome itself still honours.
    if req.method == "Target.sendMessageToTarget" {
        return dispatch_send_message_to_target(req, ctx).await;
    }

    let (domain, method) = match req.method.split_once('.') {
        Some((d, m)) => (d, m),
        None => {
            return CdpResponse::error(
                req.id,
                -32601,
                format!("Invalid method format: {}", req.method),
                req.session_id.clone(),
            );
        }
    };

    let result = match domain {
        "Target" => domains::target::handle(method, &req.params, ctx).await,
        "Browser" => domains::browser::handle(method, &req.params).await,
        "Page" => domains::page::handle(method, &req.params, ctx, &req.session_id).await,
        "DOM" => domains::dom::handle(method, &req.params, ctx, &req.session_id).await,
        "Runtime" => domains::runtime::handle(method, &req.params, ctx, &req.session_id).await,
        "Network" => domains::network::handle(method, &req.params, ctx, &req.session_id).await,
        "Fetch" => domains::fetch::handle(method, &req.params, ctx, &req.session_id).await,
        "Input" => domains::input::handle(method, &req.params, ctx, &req.session_id).await,
        "Storage" => domains::storage::handle(method, &req.params, ctx, &req.session_id).await,
        "LP" => domains::lp::handle(method, &req.params, ctx, &req.session_id).await,
        "Accessibility" => domains::accessibility::handle(method, &req.params, ctx, &req.session_id).await,
        "Emulation" | "Log" | "Performance" | "Security" | "CSS"
        | "ServiceWorker" | "Inspector"
        | "Debugger" | "Profiler" | "HeapProfiler" | "Overlay" => {
            Ok(json!({}))
        }
        _ => Err(format!("Unknown domain: {}", domain)),
    };

    match result {
        Ok(value) => CdpResponse::success(req.id, value, req.session_id.clone()),
        Err(msg) => {
            tracing::warn!("CDP error for {}: {}", req.method, msg);
            CdpResponse::error(req.id, -32601, msg, req.session_id.clone())
        }
    }
}

async fn dispatch_send_message_to_target(req: &CdpRequest, ctx: &mut CdpContext) -> CdpResponse {
    let session_id = req
        .params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let message = match req.params.get("message").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => {
            return CdpResponse::error(
                req.id,
                -32602,
                "sendMessageToTarget requires a message string".into(),
                req.session_id.clone(),
            );
        }
    };

    let inner: CdpRequest = match serde_json::from_str(message) {
        Ok(r) => r,
        Err(e) => {
            return CdpResponse::error(
                req.id,
                -32700,
                format!("sendMessageToTarget message is not a valid CDP request: {e}"),
                req.session_id.clone(),
            );
        }
    };

    // Override the inner session with the one supplied by the wrapper so
    // the inner dispatch routes against the right page. Boxing the future
    // sidesteps the async-fn recursion limit.
    let inner_with_session = CdpRequest {
        id: inner.id,
        method: inner.method.clone(),
        params: inner.params,
        session_id: session_id.clone().or(inner.session_id),
    };
    let inner_response = Box::pin(dispatch(&inner_with_session, ctx)).await;

    // Re-emit the inner response as the legacy event headless_chrome (and
    // older Puppeteer) listen for instead of correlating responses by id.
    let inner_serialized = serde_json::to_string(&inner_response).unwrap_or_else(|_| "{}".into());
    ctx.pending_events.push(CdpEvent {
        method: "Target.receivedMessageFromTarget".to_string(),
        params: json!({
            "sessionId": session_id.clone().unwrap_or_default(),
            "message": inner_serialized,
            "targetId": session_id.clone().unwrap_or_default(),
        }),
        session_id: req.session_id.clone(),
    });

    CdpResponse::success(req.id, json!({}), req.session_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CdpRequest;

    fn req(method: &str) -> CdpRequest {
        CdpRequest {
            id: 1,
            method: method.into(),
            params: json!({}),
            session_id: None,
        }
    }

    #[tokio::test]
    async fn unknown_domain_still_errors() {
        let mut ctx = CdpContext::new();
        let resp = dispatch(&req("DefinitelyNotADomain.enable"), &mut ctx).await;
        let err = resp.error.expect("unknown domain must surface as error");
        assert_eq!(err.code, -32601);
        assert!(err.message.contains("Unknown domain"));
    }

    #[tokio::test]
    async fn send_message_to_target_unwraps_inner_call() {
        let mut ctx = CdpContext::new();
        let inner = json!({
            "id": 42,
            "method": "Browser.getVersion",
            "params": {},
        });
        let outer = CdpRequest {
            id: 99,
            method: "Target.sendMessageToTarget".into(),
            params: json!({
                "sessionId": "sess-1",
                "message": inner.to_string(),
            }),
            session_id: None,
        };

        let resp = dispatch(&outer, &mut ctx).await;
        assert!(resp.error.is_none(), "wrapper must succeed: {:?}", resp.error);
        assert_eq!(resp.id, 99);
        assert_eq!(resp.result, Some(json!({})));

        // headless_chrome reads the inner response from the
        // receivedMessageFromTarget event, not from the wrapper response.
        let evt = ctx
            .pending_events
            .iter()
            .find(|e| e.method == "Target.receivedMessageFromTarget")
            .expect("receivedMessageFromTarget event must be emitted");
        assert_eq!(evt.params["sessionId"], "sess-1");
        let inner_msg = evt.params["message"].as_str().expect("message is a string");
        let parsed: serde_json::Value = serde_json::from_str(inner_msg).unwrap();
        assert_eq!(parsed["id"], 42);
        // Browser.getVersion returns a populated result object, not an error.
        assert!(parsed.get("result").is_some(), "inner response carries result");
        assert!(parsed.get("error").is_none(), "inner response is not an error");
    }

    #[tokio::test]
    async fn send_message_to_target_rejects_invalid_message() {
        let mut ctx = CdpContext::new();
        let outer = CdpRequest {
            id: 5,
            method: "Target.sendMessageToTarget".into(),
            params: json!({
                "sessionId": "sess-1",
                "message": "{not valid json",
            }),
            session_id: None,
        };
        let resp = dispatch(&outer, &mut ctx).await;
        let err = resp.error.expect("malformed inner messages must error");
        assert_eq!(err.code, -32700);
    }
}
