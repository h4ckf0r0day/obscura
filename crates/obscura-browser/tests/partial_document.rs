use std::sync::Arc;
use std::time::Duration;
use obscura_browser::{BrowserContext, LifecycleState, Page};
use serde_json::json;

#[path = "../../obscura-net/tests/support/partial_server.rs"]
mod fixture;

fn page(stealth: bool) -> Page {
    Page::new("partial".into(), Arc::new(BrowserContext::with_storage_and_network(
        "partial".into(), None, stealth, None, None, true)))
}

#[tokio::test(flavor = "current_thread")]
async fn navigation_deadline_retains_dom_without_executing_or_resuming_author_code() {
    check_deadline(false).await;
}

#[cfg(feature = "stealth")]
#[tokio::test(flavor = "current_thread")]
async fn stealth_navigation_deadline_retains_dom_without_executing_author_code() {
    check_deadline(true).await;
}

async fn check_deadline(stealth: bool) {
    let (origin, _) = fixture::serve().await;
    let mut page = page(stealth);
    page.navigate("data:text/html,<title>old</title><script>globalThis.oldMarker=true;</script>").await.unwrap();
    page.add_preload_script("globalThis.preloadRan=true;");
    page.set_navigation_timeout(Duration::from_millis(350));
    let url = format!("{origin}/slow");
    let error = page.navigate(&url).await.unwrap_err();
    assert!(error.to_string().contains("received HTML retained"));
    assert_eq!(page.lifecycle, LifecycleState::Failed);
    assert_eq!(page.url_string(), url);
    assert_eq!(page.title, "partial");
    assert!(page.js.is_none());
    assert!(page.dom().unwrap().is_document_incomplete());
    assert!(page.dom().unwrap().get_element_by_id("arrived").is_some());
    let event = page.network_events.last().unwrap();
    assert_eq!(event.error_text.as_deref(), Some("net::ERR_TIMED_OUT"));
    assert!(page.get_response_body(&event.request_id).is_none());
    page.settle(10).await;
    assert!(page.js.is_none());
    #[cfg(feature = "render")]
    assert!(page.screenshot((800.0, 600.0)).is_some());
    assert!(page.js.is_none());

    page.resume_js();
    let value = page.evaluate_with_timeout(r#"(() => [document.title,document.readyState,
        typeof oldMarker,typeof prefixRan,typeof eofRan,typeof preloadRan,typeof frameRan])()"#,
        Duration::from_millis(100));
    assert_eq!(value, json!(["partial","loading","undefined","undefined","undefined","undefined","undefined"]));
    page.settle(10).await;
    assert!(page.frame_urls().is_empty());
    page.set_navigation_timeout(Duration::from_secs(2));
    page.navigate(&format!("{origin}/healthy")).await.unwrap();
    assert_eq!(page.evaluate("[document.title,document.readyState,healthyRan]"), json!(["healthy","complete",true]));
    assert!(!page.with_dom(|dom| dom.is_document_incomplete()).unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn late_healthy_document_keeps_the_full_navigation_budget() {
    let (origin, _) = fixture::serve().await;
    let mut page = page(false);
    page.set_navigation_timeout(Duration::from_secs(2));
    page.navigate(&format!("{origin}/late")).await.unwrap();
    assert!(page.with_dom(|dom| dom.get_element_by_id("tail").is_some()).unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn failed_non_html_empty_invalid_or_oversized_transfers_keep_the_old_document() {
    let (origin, _) = fixture::serve().await;
    let mut page = page(false);
    let old = "data:text/html,<title>old</title><script>globalThis.oldMarker=true;</script>";
    page.navigate(old).await.unwrap();
    let old = page.url_string();
    page.set_navigation_timeout(Duration::from_millis(350));
    for path in ["noheaders", "nobytes", "json", "protocol", "oversize", "redirect-file"] {
        assert!(page.navigate(&format!("{origin}/{path}")).await.is_err(), "{path}");
        assert_eq!(page.url_string(), old, "{path}");
        assert_eq!(page.evaluate("[document.title,oldMarker]"), json!(["old",true]), "{path}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn robots_and_ssrf_failures_cannot_be_recovered_as_the_document() {
    let (origin, requests) = fixture::serve().await;
    let mut context = BrowserContext::with_storage_and_network("robots".into(), None, false, None, None, true);
    context.obey_robots = true;
    let mut page = Page::new("robots".into(), Arc::new(context));
    page.navigate("data:text/html,<title>old</title>").await.unwrap();
    page.set_navigation_timeout(Duration::from_millis(350));
    assert!(page.navigate(&format!("{origin}/healthy")).await.is_err());
    assert_eq!(page.title, "old");
    assert_eq!(*requests.lock().unwrap(), vec!["/robots.txt".to_string()]);
    let mut blocked = Page::new("ssrf".into(), Arc::new(BrowserContext::with_storage_and_network(
        "ssrf".into(), None, false, None, None, false)));
    blocked.navigate("data:text/html,<title>old</title>").await.unwrap();
    assert!(blocked.navigate(&format!("{origin}/healthy")).await.is_err());
    assert_eq!(blocked.title, "old");
}

#[tokio::test(flavor = "current_thread")]
async fn body_timeout_with_budget_remaining_and_redirects_retain_the_final_document() {
    let (origin, _) = fixture::serve().await;
    let mut page = page(false);
    let mut client = obscura_net::ObscuraHttpClient::with_full_options(page.context.cookie_jar.clone(), None, true);
    client.timeout = Duration::from_millis(150);
    page.http_client = Arc::new(client);
    page.set_navigation_timeout(Duration::from_secs(2));
    let error = page.navigate(&format!("{origin}/redirect")).await.unwrap_err();
    assert!(error.to_string().contains("document body timeout"));
    assert_eq!(page.url_string(), format!("{origin}/slow"));
    assert_eq!(page.title, "partial");
    assert!(page.js.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn recovered_intercepted_rewrites_preserve_document_urls_and_real_redirects() {
    use obscura_js::ops::InterceptResolution;
    let (origin, _) = fixture::serve().await;
    let original = "http://127.0.0.1:9/original";
    for path in ["slow", "redirect"] {
        let mut page = page(false);
        page.set_navigation_timeout(Duration::from_millis(350));
        let mut requests = page.enable_interception();
        let rewritten = format!("{origin}/{path}");
        let (result, ()) = tokio::join!(page.navigate(original), async {
            let request = requests.recv().await.unwrap();
            assert_eq!(request.url, original);
            request.resolver.send(InterceptResolution::Continue {
                url: Some(rewritten), method: None, headers: None, body: None,
            }).ok().unwrap();
            // A real redirect pauses again before its target, as in Chrome (#1540).
            if path == "redirect" {
                let target = requests.recv().await.unwrap();
                assert_eq!(target.url, format!("{origin}/slow"));
                target.resolver.send(InterceptResolution::Continue {
                    url: None, method: None, headers: None, body: None,
                }).ok().unwrap();
            }
        });
        assert!(result.is_err());
        assert_eq!(page.title, "partial");
        let expected = if path == "slow" { original.into() } else { format!("{origin}/slow") };
        assert_eq!(page.url_string(), expected);
        let event = page.network_events.last().unwrap();
        assert_eq!(event.url, expected);
        assert!(event.intercepted);
        assert_eq!(event.error_text.as_deref(), Some("net::ERR_TIMED_OUT"));
    }
}
