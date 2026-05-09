use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use obscura_dom::{parse_html, DomTree};
use obscura_js::runtime::ObscuraJsRuntime;
use obscura_net::{ObscuraHttpClient, ObscuraNetError, Response};
use url::Url;

use crate::context::BrowserContext;
use crate::lifecycle::LifecycleState;

#[cfg(feature = "stealth")]
use obscura_net::StealthHttpClient;

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

pub struct Page {
    pub id: String,
    pub frame_id: String,
    pub url: Option<Url>,
    pub dom: Option<DomTree>,
    pub js: Option<ObscuraJsRuntime>,
    pub lifecycle: LifecycleState,
    pub http_client: Arc<ObscuraHttpClient>,
    pub context: Arc<BrowserContext>,
    pub title: String,
    pub network_events: Vec<NetworkEvent>,
    network_event_counter: u32,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub device_scale_factor: f64,
    pub user_agent_override: Option<serde_json::Value>,
    pub intercept_enabled: bool,
    pub intercept_block_patterns: Vec<String>,
    intercept_tx: Option<tokio::sync::mpsc::UnboundedSender<obscura_js::ops::InterceptedRequest>>,
    #[cfg(feature = "stealth")]
    pub stealth_client: Option<Arc<StealthHttpClient>>,
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
            Some(Arc::new(StealthHttpClient::new(context.cookie_jar.clone())))
        } else {
            None
        };

        Page {
            id,
            frame_id,
            url: None,
            dom: None,
            js: None,
            lifecycle: LifecycleState::Idle,
            http_client,
            context,
            title: String::new(),
            network_events: Vec::new(),
            network_event_counter: 0,
            viewport_width: 1920,
            viewport_height: 1000,
            device_scale_factor: 2.0,
            user_agent_override: None,
            intercept_enabled: false,
            intercept_block_patterns: Vec::new(),
            intercept_tx: None,
            #[cfg(feature = "stealth")]
            stealth_client,
        }
    }

    fn should_block_url(&self, url: &str) -> bool {
        if !self.intercept_enabled || self.intercept_block_patterns.is_empty() {
            return false;
        }
        for pattern in &self.intercept_block_patterns {
            if pattern == "*" {
                return true;
            }
            if pattern.starts_with('*') && pattern.ends_with('*') {
                if url.contains(&pattern[1..pattern.len() - 1]) {
                    return true;
                }
            } else if pattern.starts_with('*') {
                if url.ends_with(&pattern[1..]) {
                    return true;
                }
            } else if pattern.ends_with('*') {
                if url.starts_with(&pattern[..pattern.len() - 1]) {
                    return true;
                }
            } else if url.contains(pattern) {
                return true;
            }
        }
        false
    }

    async fn do_fetch(&self, url: &Url) -> Result<Response, ObscuraNetError> {
        #[cfg(feature = "stealth")]
        if let Some(ref stealth) = self.stealth_client {
            return stealth.fetch(url).await;
        }
        self.http_client.fetch(url).await
    }
    fn init_js(&mut self) {
        if self.js.is_some() {
            let url_str = self.url_string();
            let title = self.title.clone();
            let dom = self.dom.take();

            let js = self.js.as_mut().unwrap();
            js.set_page_id(&self.id);
            js.set_url(&url_str);
            js.set_title(&title);
            if let Ok(ua) = self.http_client.user_agent.try_read() {
                js.set_user_agent(&ua);
            }
            js.set_viewport_metrics(
                self.viewport_width,
                self.viewport_height,
                self.device_scale_factor,
            );
            if let Some(override_params) = &self.user_agent_override {
                js.apply_user_agent_override(override_params);
            }

            if let Some(d) = dom {
                js.set_dom(d);
            }

            let _ = js.execute_script(
                "<reset>",
                "_cache.clear(); globalThis.__obscura_objects = {}; globalThis.__obscura_oid = 0; \
                 _iframeRegistry.length = 0; globalThis.length = 0; \
                 globalThis._formValues = {}; globalThis._formChecked = {}; \
                 globalThis._eventRegistry = {}; \
                 globalThis.document = new Document(+_dom('document_node_id'));",
            );

            return;
        }

        let mut rt = ObscuraJsRuntime::with_base_url(&self.url_string());
        rt.set_page_id(&self.id);
        rt.set_url(&self.url_string());
        rt.set_title(&self.title);

        #[cfg(feature = "stealth")]
        if self.stealth_client.is_some() {
            rt.set_user_agent(obscura_net::STEALTH_USER_AGENT);
        } else if let Ok(ua) = self.http_client.user_agent.try_read() {
            rt.set_user_agent(&ua);
        }
        #[cfg(not(feature = "stealth"))]
        if let Ok(ua) = self.http_client.user_agent.try_read() {
            rt.set_user_agent(&ua);
        }

        rt.set_viewport_metrics(
            self.viewport_width,
            self.viewport_height,
            self.device_scale_factor,
        );
        if let Some(override_params) = &self.user_agent_override {
            rt.apply_user_agent_override(override_params);
        }
        rt.set_cookie_jar(self.context.cookie_jar.clone());
        rt.set_http_client(self.http_client.clone());

        if let Some(tx) = &self.intercept_tx {
            if self.intercept_enabled {
                rt.set_intercept_tx(tx.clone());
            } else {
                rt.set_network_event_tx(tx.clone());
            }
        }

        if let Some(dom) = self.dom.take() {
            rt.set_dom(dom);
        }

        self.js = Some(rt);
    }

    async fn execute_scripts(&mut self) {
        tracing::info!(
            "execute_scripts called, js runtime exists: {}",
            self.js.is_some()
        );

        #[derive(Debug)]
        struct ScriptInfo {
            node_id: u32,
            src: Option<String>,
            inline: String,
            is_defer: bool,
            is_async: bool,
            is_module: bool,
        }

        let all_scripts = match &self.js {
            Some(js) => js
                .with_dom(|dom| {
                    let script_ids = dom.query_selector_all("script").unwrap_or_default();
                    let mut scripts = Vec::new();

                    for sid in script_ids {
                        if let Some(node) = dom.get_node(sid) {
                            let src = node.get_attribute("src").map(|s| s.to_string());
                            let script_type = node.get_attribute("type").unwrap_or("").to_string();
                            let is_defer = node.get_attribute("defer").is_some();
                            let is_async = node.get_attribute("async").is_some();
                            let is_module = script_type == "module";

                            if !script_type.is_empty()
                                && script_type != "text/javascript"
                                && script_type != "application/javascript"
                                && script_type != "module"
                            {
                                continue;
                            }

                            let inline_code = if src.is_none() {
                                dom.text_content(sid)
                            } else {
                                String::new()
                            };

                            if src.is_some() || !inline_code.trim().is_empty() {
                                scripts.push(ScriptInfo {
                                    node_id: sid.raw(),
                                    src,
                                    inline: inline_code,
                                    is_defer,
                                    is_async,
                                    is_module,
                                });
                            }
                        }
                    }
                    scripts
                })
                .unwrap_or_default(),
            None => return,
        };

        let mut regular = Vec::new();
        let mut deferred = Vec::new();
        let mut async_scripts = Vec::new();

        let mut module_scripts = Vec::new();

        for script in all_scripts {
            if script.is_module {
                module_scripts.push(script);
                continue;
            }
            if script.is_defer {
                deferred.push(script);
            } else if script.is_async {
                async_scripts.push(script);
            } else {
                regular.push(script);
            }
        }

        let scripts = regular;

        tracing::info!(
            "Found {} regular + {} deferred + {} async scripts",
            scripts.len(),
            deferred.len(),
            async_scripts.len()
        );
        let all_to_execute: Vec<ScriptInfo> = scripts
            .into_iter()
            .chain(deferred.into_iter())
            .chain(async_scripts.into_iter())
            .collect();

        let mut resolved: Vec<(usize, String)> = Vec::new();
        let mut fetch_tasks: Vec<(usize, String)> = Vec::new();
        let mut data_scripts: std::collections::HashMap<usize, (String, String)> =
            std::collections::HashMap::new();

        for (i, script) in all_to_execute.iter().enumerate() {
            if let Some(src_url) = &script.src {
                if let Some(code) = decode_data_script(src_url) {
                    data_scripts.insert(i, (src_url.clone(), code));
                    continue;
                }

                let full_url = if src_url.starts_with("http://") || src_url.starts_with("https://")
                {
                    src_url.clone()
                } else if let Some(base) = &self.url {
                    base.join(src_url)
                        .map(|u| u.to_string())
                        .unwrap_or_else(|_| src_url.clone())
                } else {
                    src_url.clone()
                };

                if self.should_block_url(&full_url) {
                    tracing::info!("Blocked script by interception: {}", full_url);
                    continue;
                }
                resolved.push((i, full_url.clone()));
                fetch_tasks.push((i, full_url));
            }
        }

        let client = self.http_client.clone();
        let fetch_futures: Vec<_> = fetch_tasks
            .iter()
            .map(|(idx, url)| {
                let client = client.clone();
                let url = url.clone();
                let idx = *idx;
                async move {
                    let parsed =
                        Url::parse(&url).unwrap_or_else(|_| Url::parse("about:blank").unwrap());
                    match client.fetch(&parsed).await {
                        Ok(resp) => Some((idx, url, resp)),
                        Err(e) => {
                            tracing::warn!("Failed to fetch script {}: {}", url, e);
                            None
                        }
                    }
                }
            })
            .collect();

        let fetch_results = futures::future::join_all(fetch_futures).await;

        let mut fetched: std::collections::HashMap<usize, (String, String, obscura_net::Response)> =
            std::collections::HashMap::new();
        for result in fetch_results {
            if let Some((idx, url, resp)) = result {
                let code = String::from_utf8_lossy(&resp.body).to_string();
                fetched.insert(idx, (url, code, resp));
            }
        }

        for (i, script) in all_to_execute.iter().enumerate() {
            let mut pump_after_script = false;

            if script.src.is_some() {
                if let Some((url, code)) = data_scripts.remove(&i) {
                    tracing::info!("Executing data script ({} bytes)", code.len());
                    if let Some(js) = &mut self.js {
                        js.set_current_script(script.node_id);
                        if let Err(e) = js.execute_script_guarded(&url, &code) {
                            tracing::warn!("Data script error: {}", e);
                        }
                        let _ = js.execute_script(
                            "<resource-load>",
                            &format!(
                                "globalThis.__obscura_finish_resource_node({}, false, '')",
                                script.node_id
                            ),
                        );
                        js.clear_current_script();
                    }
                } else if let Some((url, code, resp)) = fetched.remove(&i) {
                    tracing::info!("Executing script ({} bytes): {}", code.len(), url);
                    self.record_network_event(
                        &url,
                        "GET",
                        "Script",
                        resp.status,
                        &resp.headers,
                        resp.body.len(),
                    );
                    if let Some(js) = &mut self.js {
                        js.set_current_script(script.node_id);
                        if let Err(e) = js.execute_script_guarded(&url, &code) {
                            tracing::warn!("Script error ({}): {}", url, e);
                        }
                        let _ = js.execute_script(
                            "<resource-load>",
                            &format!(
                                "globalThis.__obscura_finish_resource_node({}, false, '')",
                                script.node_id
                            ),
                        );
                        js.clear_current_script();
                        pump_after_script = true;
                    }
                }
            } else if !script.inline.is_empty() {
                let inline_len = script.inline.len();
                if let Some(js) = &mut self.js {
                    js.set_current_script(script.node_id);
                    if let Err(e) = js.execute_script_guarded("<inline>", &script.inline) {
                        tracing::warn!("Inline script error: {}", e);
                    }
                    let _ = js.execute_script(
                        "<resource-load>",
                        &format!(
                            "globalThis.__obscura_finish_resource_node({}, false, '')",
                            script.node_id
                        ),
                    );
                    js.clear_current_script();
                    pump_after_script = inline_len >= 10_000;
                }
            }

            if pump_after_script {
                self.pump_event_loop_for(tokio::time::Duration::from_millis(300))
                    .await;
            }
        }

        for module_script in &module_scripts {
            if let Some(ref src) = module_script.src {
                let full_url = if src.starts_with("http://") || src.starts_with("https://") {
                    src.clone()
                } else if let Some(base) = &self.url {
                    base.join(src)
                        .map(|u| u.to_string())
                        .unwrap_or_else(|_| src.clone())
                } else {
                    src.clone()
                };

                tracing::info!("Loading ES module: {}", full_url);
                let mut module_loaded = false;
                if let Some(js) = &mut self.js {
                    js.set_current_script(module_script.node_id);
                    match js.load_module(&full_url).await {
                        Ok(()) => {
                            tracing::info!("ES module loaded: {}", full_url);
                            module_loaded = true;
                            let _ = js.execute_script(
                                "<resource-load>",
                                &format!(
                                    "globalThis.__obscura_finish_resource_node({}, false, '')",
                                    module_script.node_id
                                ),
                            );
                        }
                        Err(e) => {
                            tracing::warn!("ES module error ({}): {}", full_url, e);
                        }
                    }
                    js.clear_current_script();
                }
                if module_loaded {
                    self.record_network_event(
                        &full_url,
                        "GET",
                        "Script",
                        200,
                        &std::collections::HashMap::new(),
                        0,
                    );
                }
                self.pump_event_loop_for(tokio::time::Duration::from_millis(300))
                    .await;
            } else if !module_script.inline.is_empty() {
                let base = self.url_string();
                if let Some(js) = &mut self.js {
                    js.set_current_script(module_script.node_id);
                    if let Err(e) = js.load_inline_module(&module_script.inline, &base).await {
                        tracing::warn!("Inline ES module error: {}", e);
                    }
                    let _ = js.execute_script(
                        "<resource-load>",
                        &format!(
                            "globalThis.__obscura_finish_resource_node({}, false, '')",
                            module_script.node_id
                        ),
                    );
                    js.clear_current_script();
                }
                self.pump_event_loop_for(tokio::time::Duration::from_millis(300))
                    .await;
            }
        }

        if let Some(js) = &mut self.js {
            let _ = js.execute_script("<load-events>",
                "if (typeof window.onload === 'function') { try { window.onload(); } catch(e) {} }\n\
                 try { document._readyState = 'interactive'; } catch(e) {}\n\
                 try { document.dispatchEvent(new Event('DOMContentLoaded')); } catch(e) {}\n\
                 try { document._readyState = 'complete'; } catch(e) {}\n\
                 try { window.dispatchEvent(new Event('load')); } catch(e) {}");
        }

        self.pump_event_loop_for(tokio::time::Duration::from_secs(3))
            .await;
    }

    pub async fn pump_event_loop_for(&mut self, duration: tokio::time::Duration) {
        let http_client = self.http_client.clone();
        let Some(js) = &mut self.js else {
            return;
        };

        let deadline = tokio::time::Instant::now() + duration;
        let mut idle_count = 0u32;

        loop {
            let result = js
                .run_event_loop_slice(
                    tokio::time::Duration::from_millis(25),
                    std::time::Duration::from_secs(2),
                )
                .await;

            if let Err(e) = result {
                tracing::debug!("JS event loop pump error: {}", e);
                break;
            }

            let active = http_client.active_requests();
            if active == 0 {
                idle_count += 1;
                if idle_count >= 6 {
                    break;
                }
                tokio::task::yield_now().await;
            } else {
                idle_count = 0;
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            }

            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
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
        let mut current_url = url_str.to_string();
        let mut current_method = method.to_string();
        let mut current_body = body.to_string();
        for _chain in 0..10 {
            self.navigate_single(&current_url, wait_until, &current_method, &current_body)
                .await?;
            if let Some((next_url, next_method, next_body)) = self.take_pending_navigation() {
                tracing::info!(
                    "JS-triggered navigation chain: {} {} -> {}",
                    current_method,
                    current_url,
                    next_url
                );
                current_url = next_url;
                current_method = next_method;
                current_body = next_body;
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
    ) -> Result<(), PageError> {
        let url = Url::parse(url_str).map_err(|e| PageError::InvalidUrl(e.to_string()))?;

        self.lifecycle = LifecycleState::Loading;
        self.url = Some(url.clone());
        self.network_events.clear();

        if self.context.obey_robots {
            if let Some(domain) = url.host_str() {
                if self.context.robots_cache.is_allowed(domain, "/robots.txt") {
                    let robots_url = format!("{}://{}/robots.txt", url.scheme(), domain);
                    if let Ok(robots_url) = Url::parse(&robots_url) {
                        if let Ok(resp) = self.http_client.fetch(&robots_url).await {
                            if resp.status == 200 {
                                let body = String::from_utf8_lossy(&resp.body);
                                self.context.robots_cache.parse_and_store(
                                    domain,
                                    &body,
                                    &self.context.user_agent,
                                );
                            }
                        }
                    }
                }

                if !self.context.robots_cache.is_allowed(domain, url.path()) {
                    self.lifecycle = LifecycleState::Failed;
                    return Err(PageError::NetworkError(format!(
                        "Blocked by robots.txt: {}",
                        url
                    )));
                }
            }
        }

        let response = if method == "POST" {
            self.http_client.post_form(&url, body).await
        } else {
            self.do_fetch(&url).await
        }
        .map_err(|e| {
            self.lifecycle = LifecycleState::Failed;
            PageError::NetworkError(e.to_string())
        })?;

        self.record_network_event(
            url.as_str(),
            "GET",
            "Document",
            response.status,
            &response.headers,
            response.body.len(),
        );

        if !response.redirected_from.is_empty() {
            self.url = Some(response.url.clone());
        }

        let body_text = String::from_utf8_lossy(&response.body).to_string();
        let dom = parse_html(&body_text);

        self.title = dom
            .query_selector("title")
            .ok()
            .flatten()
            .map(|title_id| dom.text_content(title_id))
            .unwrap_or_default();

        let stylesheet_urls: Vec<String> = dom
            .query_selector_all("link")
            .unwrap_or_default()
            .iter()
            .filter_map(|&nid| {
                let node = dom.get_node(nid)?;
                let rel = node.get_attribute("rel")?;
                if rel.to_lowercase() != "stylesheet" {
                    return None;
                }
                node.get_attribute("href").map(|s| s.to_string())
            })
            .collect();

        let mut css_fetch_urls: Vec<String> = Vec::new();
        for href in &stylesheet_urls {
            let full_url = if href.starts_with("http://") || href.starts_with("https://") {
                href.clone()
            } else if let Some(base) = &self.url {
                base.join(href)
                    .map(|u| u.to_string())
                    .unwrap_or_else(|_| href.clone())
            } else {
                href.clone()
            };
            if self.should_block_url(&full_url) {
                tracing::info!("Blocked stylesheet by interception: {}", full_url);
                continue;
            }
            css_fetch_urls.push(full_url);
        }

        let client = self.http_client.clone();
        let css_futures: Vec<_> = css_fetch_urls
            .iter()
            .map(|full_url| {
                let client = client.clone();
                let url_str = full_url.clone();
                async move {
                    let parsed =
                        Url::parse(&url_str).unwrap_or_else(|_| Url::parse("about:blank").unwrap());
                    match client.fetch(&parsed).await {
                        Ok(resp) => Some((url_str, resp)),
                        Err(e) => {
                            tracing::debug!("Failed to fetch stylesheet {}: {}", url_str, e);
                            None
                        }
                    }
                }
            })
            .collect();

        let css_results = futures::future::join_all(css_futures).await;
        let mut css_sources = Vec::new();
        for result in css_results {
            if let Some((url_str, resp)) = result {
                let css = String::from_utf8_lossy(&resp.body).to_string();
                self.record_network_event(
                    &url_str,
                    "GET",
                    "Stylesheet",
                    resp.status,
                    &resp.headers,
                    resp.body.len(),
                );
                css_sources.push(css);
            }
        }

        self.dom = Some(dom);
        self.lifecycle = LifecycleState::DomContentLoaded;

        if wait_until == crate::lifecycle::WaitUntil::DomContentLoaded {
            self.init_js();
            return Ok(());
        }

        self.init_js();

        if !css_sources.is_empty() {
            if let Some(js) = &mut self.js {
                let combined_css = css_sources.join("\n");
                let escaped = combined_css
                    .replace('\\', "\\\\")
                    .replace('`', "\\`")
                    .replace("${", "\\${");
                let code = format!("globalThis.__obscura_css = `{}`;", escaped);
                let _ = js.execute_script("<css>", &code);
            }
        }
        if let Some(js) = &mut self.js {
            let _ = js.execute_script("<iframe-load>",
                "(function() { var iframes = document.querySelectorAll('iframe[src]'); for (var i = 0; i < iframes.length; i++) { var src = iframes[i].getAttribute('src'); if (src && src !== 'about:blank') iframes[i]._loadIframeSrc(src); } })()");
        }

        self.execute_scripts().await;

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
                    let _ = js
                        .run_event_loop_slice(
                            tokio::time::Duration::from_millis(50),
                            std::time::Duration::from_millis(250),
                        )
                        .await;
                } else {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }

            self.lifecycle = LifecycleState::NetworkIdle;
        }

        Ok(())
    }

    pub fn navigate_blank(&mut self) {
        self.js = None;
        self.url = Some(Url::parse("about:blank").unwrap());
        self.dom = Some(parse_html(
            "<!DOCTYPE html><html><head></head><body></body></html>",
        ));
        self.title = String::new();
        self.lifecycle = LifecycleState::Loaded;
    }

    pub fn set_viewport_metrics(&mut self, width: u32, height: u32, device_scale_factor: f64) {
        self.viewport_width = width.max(1);
        self.viewport_height = height.max(1);
        self.device_scale_factor = if device_scale_factor.is_finite() && device_scale_factor > 0.0 {
            device_scale_factor
        } else {
            2.0
        };

        if let Some(js) = &mut self.js {
            js.set_viewport_metrics(
                self.viewport_width,
                self.viewport_height,
                self.device_scale_factor,
            );
        }
    }

    pub fn set_user_agent_override(&mut self, params: serde_json::Value) {
        self.user_agent_override = Some(params);
        if let (Some(js), Some(override_params)) = (&mut self.js, &self.user_agent_override) {
            js.apply_user_agent_override(override_params);
        }
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

    pub fn dom(&self) -> Option<&DomTree> {
        self.dom.as_ref()
    }

    pub fn evaluate(&mut self, expression: &str) -> serde_json::Value {
        if let Some(js) = &mut self.js {
            match js.evaluate(expression) {
                Ok(val) => val,
                Err(e) => {
                    tracing::debug!(
                        "JS eval error for '{}': {}",
                        &expression[..expression.len().min(80)],
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

    pub fn evaluate_for_cdp(
        &mut self,
        expression: &str,
        return_by_value: bool,
    ) -> obscura_js::runtime::RemoteObjectInfo {
        if let Some(js) = &mut self.js {
            match js.evaluate_for_cdp(expression, return_by_value) {
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

    pub fn set_blocked_urls(&mut self, patterns: Vec<String>) {
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
        self.network_event_counter += 1;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        self.network_events.push(NetworkEvent {
            request_id: format!("{}.{}", self.id, self.network_event_counter),
            url: url.to_string(),
            method: method.to_string(),
            resource_type: resource_type.to_string(),
            status,
            headers: std::collections::HashMap::new(),
            response_headers: Arc::new(response_headers.clone()),
            body_size,
            timestamp,
        });
    }

    pub fn execute_preload_script(&mut self, source: &str) -> Result<(), String> {
        if let Some(js) = &mut self.js {
            js.execute_script("<preload>", source)
        } else {
            Err("No JS runtime".to_string())
        }
    }

    pub fn suspend_js(&mut self) {
        if let Some(js) = &self.js {
            if let Some(dom) = js.take_dom() {
                self.dom = Some(dom);
            }
        }
        self.js = None;
    }

    pub fn resume_js(&mut self) {
        if self.js.is_some() {
            return;
        }
        self.init_js();
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

    pub fn set_intercept_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<obscura_js::ops::InterceptedRequest>,
    ) {
        self.intercept_tx = Some(tx.clone());
        self.intercept_enabled = true;
        if let Some(js) = &self.js {
            js.set_intercept_tx(tx);
        }
    }

    pub fn set_network_event_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<obscura_js::ops::InterceptedRequest>,
    ) {
        self.intercept_tx = Some(tx.clone());
        if let Some(js) = &self.js {
            js.set_network_event_tx(tx);
        }
    }
}

fn decode_data_script(src: &str) -> Option<String> {
    if !src.starts_with("data:") {
        return None;
    }

    let comma = src.find(',')?;
    let (metadata, data) = src.split_at(comma);
    let data = &data[1..];
    let bytes = if metadata.to_ascii_lowercase().contains(";base64") {
        BASE64.decode(data).ok()?
    } else {
        percent_decode(data)
    };

    String::from_utf8(bytes).ok()
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                decoded.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }

        decoded.push(bytes[i]);
        i += 1;
    }

    decoded
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
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
}

impl From<ObscuraNetError> for PageError {
    fn from(e: ObscuraNetError) -> Self {
        PageError::NetworkError(e.to_string())
    }
}
