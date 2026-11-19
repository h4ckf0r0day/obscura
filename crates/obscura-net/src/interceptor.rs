use std::collections::HashMap;

use crate::client::{RequestInfo, Response};

pub enum InterceptAction {
    Continue,
    Block,
    Fulfill(Response),
    ModifyHeaders(HashMap<String, String>),
}

#[async_trait::async_trait]
pub trait RequestInterceptor {
    async fn intercept(&self, request: &RequestInfo) -> InterceptAction;
}

/// What a navigation does with a redirect target it is about to request.
pub enum RedirectDecision {
    /// Request the target, or `url` in its place. `Some` headers replace the
    /// request headers for it and every later hop.
    Follow { url: Option<url::Url>, headers: Option<HashMap<String, String>> },
    /// Stop the navigation with this network error text.
    Fail(String),
    /// Answer the target with this response instead of requesting it.
    Fulfill(Response),
}

/// Consulted before a navigation requests each redirect target, so an
/// interceptor that paused the first request also sees, and can refuse, the
/// requests that follow it (CDP `Fetch.requestPaused` per hop, like Chrome).
pub trait RedirectHook: Send + Sync {
    fn before_redirect<'a>(
        &'a self,
        url: &'a url::Url,
        method: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = RedirectDecision> + Send + 'a>>;
}
