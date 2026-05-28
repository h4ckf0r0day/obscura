use std::sync::Arc;
use std::time::Duration;

use obscura_browser::lifecycle::WaitUntil;
use obscura_browser::{BrowserContext, Page as InnerPage};
use serde_json::Value;

use crate::error::Error;

/// A browser tab/page.
pub struct Page {
    pub(crate) inner: InnerPage,
    pub(crate) context: Arc<BrowserContext>,
}

impl Page {
    /// Navigate to URL and wait for load.
    pub async fn goto(&mut self, url: &str) -> Result<(), Error> {
        self.inner
            .navigate_with_wait(url, WaitUntil::Load)
            .await
            .map_err(|e| Error::Navigation(e.to_string()))
    }

    /// Get current URL.
    pub fn url(&self) -> String {
        self.inner.url_string()
    }

    /// Execute JS in the page.
    pub fn evaluate(&mut self, expression: &str) -> Value {
        self.inner.evaluate(expression)
    }

    /// Get page HTML content.
    pub fn content(&mut self) -> String {
        let val = self.evaluate("document.documentElement.outerHTML");
        val.as_str().unwrap_or("").to_string()
    }

    /// Wait for CSS selector to appear.
    pub async fn wait_for_selector(
        &mut self,
        _selector: &str,
        _timeout: Duration,
    ) -> Result<(), Error> {
        Err(Error::Timeout("wait_for_selector: not yet implemented".into()))
    }
}
