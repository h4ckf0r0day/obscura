use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn get(viewer_url: &str, method: &str, suffix: &str, extra: &str) -> String {
    let url = url::Url::parse(viewer_url).unwrap();
    let authority = format!("127.0.0.1:{}", url.port().unwrap());
    let mut stream = tokio::net::TcpStream::connect(&authority).await.unwrap();
    stream.write_all(format!(
        "{method} {}{suffix} HTTP/1.1\r\nHost: {authority}\r\n{extra}\r\n", url.path(),
    ).as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn viewer_routes_are_read_only_and_capability_scoped() {
    let viewer = Viewer::start(HeadedOptions::default()).await.unwrap();
    let html = get(&viewer.url, "GET", "", "").await;
    assert!(html.starts_with("HTTP/1.1 200"));
    assert!(html.contains("Page rendered by Obscura"));
    assert!(html.contains("Cache-Control: no-store"));
    assert!(get(&viewer.url, "POST", "frame", "").await.starts_with("HTTP/1.1 405"));
    assert!(get(&viewer.url, "GET", "", "Origin: https://evil.example\r\n").await.starts_with("HTTP/1.1 403"));
    let root = url::Url::parse(&viewer.url).unwrap().join("/").unwrap();
    assert!(get(root.as_str(), "GET", "", "").await.starts_with("HTTP/1.1 404"));
    assert!(viewer.requests.is_empty());
}

#[tokio::test]
async fn viewer_frames_are_requested_from_the_owner_and_listener_stops_on_drop() {
    let mut viewer = Viewer::start(HeadedOptions::default()).await.unwrap();
    let url = viewer.url.clone();
    let fetch = tokio::spawn(async move { get(&url, "GET", "frame", "").await });
    let request = viewer.requests.recv().await.unwrap();
    request.send(Ok(json_frame())).unwrap();
    let response = fetch.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("tab-7"));
    let port = url::Url::parse(&viewer.url).unwrap().port().unwrap();
    drop(viewer);
    tokio::task::yield_now().await;
    assert!(tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await.is_err());
}

#[tokio::test]
async fn viewer_bounds_pending_frames_without_blocking_its_listener() {
    let mut viewer = Viewer::start(HeadedOptions::default()).await.unwrap();
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..9 {
        let url = viewer.url.clone();
        requests.spawn(async move { get(&url, "GET", "frame", "").await });
    }
    let overloaded = tokio::time::timeout(std::time::Duration::from_secs(2), requests.join_next()).await.unwrap().unwrap().unwrap();
    assert!(overloaded.starts_with("HTTP/1.1 503"));
    assert_eq!(viewer.requests.len(), 8);
    for _ in 0..8 {
        viewer.requests.recv().await.unwrap().send(Ok(json_frame())).unwrap();
    }
    while let Some(response) = requests.join_next().await {
        assert!(response.unwrap().starts_with("HTTP/1.1 200"));
    }
}

fn json_frame() -> Value {
    serde_json::json!({ "tabId": "tab-7", "url": "about:blank", "image": null })
}

#[tokio::test]
async fn independent_viewers_have_unique_urls_and_clean_up_only_their_ready_file() {
    let ready_file = std::env::temp_dir().join(format!("obscura-viewer-{}", uuid::Uuid::new_v4()));
    let viewer = Viewer::start(HeadedOptions { port: 0, ready_file: Some(ready_file.clone()) }).await.unwrap();
    let second = Viewer::start(HeadedOptions::default()).await.unwrap();
    assert_ne!(viewer.url, second.url);
    assert_eq!(std::fs::read_to_string(&ready_file).unwrap(), viewer.url);
    assert!(Viewer::start(HeadedOptions { port: 0, ready_file: Some(ready_file.clone()) }).await.is_err());
    drop(viewer);
    assert!(!ready_file.exists());
    let viewer = Viewer::start(HeadedOptions { port: 0, ready_file: Some(ready_file.clone()) }).await.unwrap();
    std::fs::write(&ready_file, "replacement").unwrap();
    drop(viewer);
    assert_eq!(std::fs::read_to_string(&ready_file).unwrap(), "replacement");
    std::fs::remove_file(ready_file).unwrap();
}

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn capture_observes_mcp_clicks_tabs_and_close_without_creating_a_page() {
    let mut state = BrowserState::new(None, None, false);
    assert!(capture(&mut state).unwrap()["image"].is_null());
    assert!(state.tabs.is_empty());
    crate::tool_navigate(&serde_json::json!({
        "url": "data:text/html,<body style='margin:0;background:red'><button onclick=\"document.body.style.background='blue'\">Change</button>",
    }), &mut state).await.unwrap();
    state.page_mut().set_viewport((64.0, 48.0));
    let before = capture(&mut state).unwrap();
    crate::tool_click(&serde_json::json!({ "selector": "button" }), &mut state).await.unwrap();
    let after = capture(&mut state).unwrap();
    assert_ne!(before["image"], after["image"]);
    assert_eq!(state.page_mut().viewport, (64.0, 48.0));
    let first = state.active_tab.clone().unwrap();
    let second = state.new_tab();
    state.page_mut().navigate("data:text/html,second").await.unwrap();
    assert_eq!(capture(&mut state).unwrap()["tabId"], second);
    state.active_tab = Some(first.clone());
    assert_eq!(capture(&mut state).unwrap()["tabId"], first);
    crate::tool_close(&mut state).unwrap();
    assert!(capture(&mut state).unwrap()["image"].is_null());
    assert!(state.tabs.is_empty());
}

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn viewer_tool_reports_only_its_own_session_url() {
    let mut state = BrowserState::new(None, None, false);
    let params = serde_json::json!({ "name": "browser_viewer", "arguments": {} });
    let disabled = crate::handle_tool_call(Value::from(1), &params, &mut state).await.result.unwrap();
    assert_eq!(disabled["isError"], true);
    state.viewer_url = Some("http://127.0.0.1:1234/test/".into());
    let enabled = crate::handle_tool_call(Value::from(2), &params, &mut state).await.result.unwrap();
    assert_eq!(enabled["content"][0]["text"], state.viewer_url.unwrap());
}
