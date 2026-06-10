use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use obscura_browser::BrowserContext;
use obscura_net::CookieJar;

use crate::config::BrowserConfig;
use crate::cookie::CookieStore;
use crate::error::Error;
use crate::page::Page;

static NEXT_PAGE_ID: AtomicU64 = AtomicU64::new(1);

pub struct Browser {
    context: Arc<BrowserContext>,
    cookie_jar: Arc<CookieJar>,
}

impl Browser {
    pub fn new() -> Result<Self, Error> {
        Self::build(BrowserConfig::default())
    }

    pub fn build(config: BrowserConfig) -> Result<Self, Error> {
        let context = if let Some(ref dir) = config.storage_dir {
            BrowserContext::with_storage_full(
                "api".to_string(),
                config.proxy,
                config.stealth,
                config.user_agent,
                Some(dir.clone()),
            )
        } else {
            BrowserContext::with_full_options(
                "api".to_string(),
                config.proxy,
                config.stealth,
                config.user_agent,
            )
        };

        let context = Arc::new(context);
        let cookie_jar = context.cookie_jar.clone();

        Ok(Browser { context, cookie_jar })
    }

    pub fn builder() -> BrowserBuilder {
        BrowserBuilder::default()
    }

    pub async fn new_page(&self) -> Result<Page, Error> {
        let id = NEXT_PAGE_ID.fetch_add(1, Ordering::Relaxed);
        let page = obscura_browser::Page::new(
            format!("page-{}", id),
            self.context.clone(),
        );
        Ok(Page {
            inner: page,
        })
    }

    /// Access the cookie store for this browser session.
    pub fn cookies(&self) -> CookieStore {
        CookieStore::new(self.cookie_jar.clone())
    }
}

#[derive(Default)]
pub struct BrowserBuilder {
    config: BrowserConfig,
}

impl BrowserBuilder {
    pub fn proxy(mut self, proxy: impl Into<String>) -> Self {
        self.config.proxy = Some(proxy.into());
        self
    }
    pub fn stealth(mut self, stealth: bool) -> Self {
        self.config.stealth = stealth;
        self
    }
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.config.user_agent = Some(ua.into());
        self
    }
    pub fn storage_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.config.storage_dir = Some(dir.into());
        self
    }
    pub fn build(self) -> Result<Browser, Error> {
        Browser::build(self.config)
    }
}
