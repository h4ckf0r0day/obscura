use std::future::Future;
use std::sync::{Arc, Mutex};
use url::Url;

use crate::{ResourceType, Response};

tokio::task_local! {
    static DOCUMENT_PROGRESS: DocumentProgress;
}

#[derive(Clone, Default)]
pub struct DocumentProgress(Arc<Mutex<ProgressState>>);

#[derive(Default)]
struct ProgressState {
    claimed: bool,
    intercepted: bool,
    intercepted_document_url: Option<Url>,
    interrupted: Option<InterruptedDocument>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentInterruption {
    Cancelled,
    BodyTimeout,
}

pub struct InterruptedDocument {
    pub response: Response,
    pub method: String,
    pub referrer: String,
    pub body_complete: bool,
    pub cause: DocumentInterruption,
    pub intercepted: bool,
    pub intercepted_document_url: Option<Url>,
}

impl DocumentProgress {
    /// Scope only the primary document request, after robots and before callbacks.
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        if let Ok(mut state) = self.0.lock() {
            if !state.claimed {
                state.intercepted = false;
                state.intercepted_document_url = None;
            }
        }
        DOCUMENT_PROGRESS.scope(self.clone(), future).await
    }

    pub fn mark_intercepted(rewritten_document_url: Option<&Url>) {
        let _ = DOCUMENT_PROGRESS.try_with(|progress| {
            if let Ok(mut state) = progress.0.lock() {
                if !state.claimed {
                    state.intercepted = true;
                    state.intercepted_document_url = rewritten_document_url.cloned();
                }
            }
        });
    }

    pub fn take(&self) -> Option<InterruptedDocument> {
        self.0.lock().ok()?.interrupted.take()
    }
}

pub struct DocumentRequest(Option<DocumentProgress>);

impl DocumentRequest {
    /// Nested requests made by callbacks must not replace the primary response.
    pub fn claim(resource_type: ResourceType) -> Self {
        let progress = (resource_type == ResourceType::Document)
            .then(|| DOCUMENT_PROGRESS.try_with(Clone::clone).ok()).flatten();
        let claimed = progress.as_ref().is_some_and(|progress| {
            let Ok(mut state) = progress.0.lock() else { return false; };
            if state.claimed { return false; }
            state.claimed = true;
            true
        });
        Self(if claimed { progress } else { None })
    }

    pub fn response(self, response: Response, method: &str, referrer: &str) -> DocumentResponse {
        let eligible = self.0.is_some() && response.content_type().is_none_or(|header| {
            let mime = header.split(';').next().unwrap_or("").trim();
            mime.is_empty() || mime.eq_ignore_ascii_case("text/html")
                || mime.eq_ignore_ascii_case("application/xhtml+xml")
        });
        DocumentResponse {
            request: self, response: Some(response), eligible, body_complete: false,
            body_timeout: false,
            method: if eligible { method.into() } else { String::new() },
            referrer: if eligible { referrer.into() } else { String::new() },
        }
    }
}

impl Drop for DocumentRequest {
    fn drop(&mut self) {
        if let Some(progress) = &self.0 {
            if let Ok(mut state) = progress.0.lock() { state.claimed = false; }
        }
    }
}

/// Own the single body buffer until the fetch (including callbacks) completes.
/// Cancellation moves it out once; ordinary reads do not lock or copy chunks.
pub struct DocumentResponse {
    request: DocumentRequest,
    response: Option<Response>,
    eligible: bool,
    body_complete: bool,
    body_timeout: bool,
    method: String,
    referrer: String,
}

impl DocumentResponse {
    pub fn get(&self) -> &Response { self.response.as_ref().unwrap() }
    pub fn get_mut(&mut self) -> &mut Response { self.response.as_mut().unwrap() }
    pub fn complete(&mut self) { self.body_complete = true; }
    pub fn failed(&mut self, timeout: bool) {
        self.body_timeout = timeout;
        if !timeout { self.eligible = false; }
    }
    pub fn finish(mut self) -> Response { self.response.take().unwrap() }
}

impl Drop for DocumentResponse {
    fn drop(&mut self) {
        if !self.eligible || self.response.as_ref().is_none_or(|response| response.body.is_empty()) {
            return;
        }
        let Some(progress) = &self.request.0 else { return; };
        let Ok(mut state) = progress.0.lock() else { return; };
        let intercepted_document_url = state.intercepted_document_url.take();
        let intercepted = state.intercepted;
        state.interrupted = Some(InterruptedDocument {
            response: self.response.take().unwrap(),
            method: std::mem::take(&mut self.method),
            referrer: std::mem::take(&mut self.referrer),
            body_complete: self.body_complete,
            cause: if self.body_timeout { DocumentInterruption::BodyTimeout } else { DocumentInterruption::Cancelled },
            intercepted_document_url,
            intercepted,
        });
    }
}
