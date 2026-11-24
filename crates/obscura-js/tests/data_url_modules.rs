use obscura_js::runtime::ObscuraJsRuntime;

#[tokio::test(flavor = "current_thread")]
async fn data_url_modules_enforce_the_decoded_module_body_limit() {
    use base64::Engine as _;
    let source = "globalThis.__dataCapRuns=(globalThis.__dataCapRuns||0)+1;";
    let previous_limit = std::env::var_os("OBSCURA_FETCH_MAX_BODY_BYTES");
    // nextest runs each test in its own process, so the override cannot race.
    std::env::set_var("OBSCURA_FETCH_MAX_BODY_BYTES", source.len().to_string());
    let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/document");
    for body in [source.to_string(), format!("{source} ")] {
        let encoded = body.bytes().map(|byte| format!("%{byte:02X}")).collect::<String>();
        for url in [
            format!("data:text/javascript,{encoded}"),
            format!("data:text/javascript;base64,{}", base64::engine::general_purpose::STANDARD.encode(&body)),
        ] {
            let loaded = rt.load_module(&url, 1_000).await;
            if body.len() == source.len() {
                loaded.expect("encoded source at the decoded byte limit must load");
            } else {
                let error = loaded.expect_err("source above the module body limit must be rejected");
                assert!(error.contains("maximum module body size"), "{error}");
            }
        }
    }
    assert_eq!(rt.evaluate("__dataCapRuns").unwrap().as_f64(), Some(2.0));
    match previous_limit {
        Some(value) => std::env::set_var("OBSCURA_FETCH_MAX_BODY_BYTES", value),
        None => std::env::remove_var("OBSCURA_FETCH_MAX_BODY_BYTES"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn data_url_modules_preserve_graph_identity_and_decode_utf8() {
    use base64::Engine as _;
    let child = format!("data:TEXT/JAVASCRIPT;BaSe64,{}", base64::engine::general_purpose::STANDARD.encode(
        "globalThis.__dataModuleRuns=(globalThis.__dataModuleRuns||0)+1;export const value=7;"
    ));
    let root = format!(
        "data:application/javascript;charset=windows-1252,import{{value}}from'{child}';globalThis.__dataModuleResult=[value,'%E2%9C%93',import.meta.url];#not-source"
    );
    let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/document");
    rt.load_module(&root, 1_000).await.unwrap();
    rt.load_inline_module(
        &format!("globalThis.__dataModuleAgain=(await import('{child}')).value;"),
        "https://example.com/document", 1_000,
    ).await.unwrap();
    assert_eq!(rt.evaluate("__dataModuleResult").unwrap(), serde_json::json!([7, "✓", root]));
    assert_eq!(rt.evaluate("[__dataModuleRuns,__dataModuleAgain]").unwrap(), serde_json::json!([1, 7]));
    let events = rt.take_js_network_events();
    assert_eq!(events.len(), 2, "entry and dependency must remain observable without HTTP");
    for event in events {
        assert_eq!(event.status, 200);
        assert_eq!(event.resource_type, "Script");
        assert!(event.error_text.is_none());
        assert!(rt.get_network_response_body(&event.request_id).is_some());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn data_url_modules_reject_invalid_mime_encoding_and_relative_imports() {
    let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/document");
    for url in [
        "data:,globalThis.__invalidDataModule=true;",
        "data:text/plain,globalThis.__invalidDataModule=true;",
        "data:application/json,globalThis.__invalidDataModule=true;",
        "data:text/javascript;base64,%%%",
        "data:text/javascript",
        "data:text/javascript,import'./relative.js';globalThis.__invalidDataModule=true;",
    ] {
        assert!(rt.load_module(url, 1_000).await.is_err(), "must reject {url}");
    }
    assert_eq!(rt.evaluate("typeof __invalidDataModule").unwrap(), serde_json::json!("undefined"));
}

#[tokio::test(flavor = "current_thread")]
async fn data_url_module_dependencies_keep_the_network_policy() {
    let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/document");
    rt.set_http_client(std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
        std::sync::Arc::new(obscura_net::CookieJar::new()), None, false,
    )));
    let error = rt.load_module(
        "data:text/javascript,import'http://127.0.0.1:9/private.js';globalThis.__privateModule=true;", 1_000,
    ).await.unwrap_err();
    assert!(error.contains("Access to private/internal IP address"), "network policy must reject the dependency: {error}");
    assert_eq!(rt.evaluate("typeof __privateModule").unwrap(), serde_json::json!("undefined"));
}

#[tokio::test(flavor = "current_thread")]
async fn data_url_modules_strip_a_utf8_bom_before_a_hashbang() {
    let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/document");
    rt.load_module(
        "data:text/javascript,%EF%BB%BF%23!hashbang%0AglobalThis.__dataBom=7;", 1_000,
    ).await.unwrap();
    assert_eq!(rt.evaluate("__dataBom").unwrap().as_f64(), Some(7.0));
}
