use std::sync::Arc;

use obscura_net::{CookieJar, FileUrlPolicy, ObscuraHttpClient, RobotsCache};

pub struct BrowserContext {
    pub id: String,
    pub cookie_jar: Arc<CookieJar>,
    pub http_client: Arc<ObscuraHttpClient>,
    pub user_agent: String,
    pub proxy_url: Option<String>,
    pub robots_cache: Arc<RobotsCache>,
    pub obey_robots: bool,
    pub stealth: bool,
}

impl BrowserContext {
    pub fn new(id: String) -> Self {
        Self::with_options_and_file_url_policy(id, None, false, FileUrlPolicy::Deny)
    }

    pub fn new_with_file_url_policy(id: String, file_url_policy: FileUrlPolicy) -> Self {
        Self::with_options_and_file_url_policy(id, None, false, file_url_policy)
    }

    pub fn with_options(id: String, proxy_url: Option<String>, stealth: bool) -> Self {
        Self::with_options_and_file_url_policy(id, proxy_url, stealth, FileUrlPolicy::Deny)
    }

    pub fn with_options_and_file_url_policy(
        id: String,
        proxy_url: Option<String>,
        stealth: bool,
        file_url_policy: FileUrlPolicy,
    ) -> Self {
        let cookie_jar = Arc::new(CookieJar::new());
        let mut client = ObscuraHttpClient::with_options_and_file_url_policy(
            cookie_jar.clone(),
            proxy_url.as_deref(),
            file_url_policy,
        );
        if stealth {
            client.block_trackers = true;
        }
        let http_client = Arc::new(client);
        BrowserContext {
            id,
            cookie_jar,
            http_client,
            user_agent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36".to_string(),
            proxy_url,
            robots_cache: Arc::new(RobotsCache::new()),
            obey_robots: false,
            stealth,
        }
    }

    pub fn with_proxy(id: String, proxy_url: Option<String>) -> Self {
        Self::with_options(id, proxy_url, false)
    }
}
