//! Obscura extensions for remote clients. Website I/O stays in the native engine.
use serde_json::{json, Value};
use crate::dispatch::CdpContext;

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    let context = ctx.get_session_page(session_id)
        .map(|page| page.context.clone()).unwrap_or_else(|| ctx.default_context.clone());
    match method {
        "getTransportInfo" => {
            let active = cfg!(feature = "stealth") && context.stealth;
            #[cfg(feature = "stealth")]
            let (user_agent, platform) = if active {
                (obscura_net::STEALTH_USER_AGENT, obscura_net::STEALTH_NAVIGATOR_PLATFORM)
            } else {
                (context.user_agent.as_str(), context.platform.as_str())
            };
            #[cfg(not(feature = "stealth"))]
            let (user_agent, platform) = (context.user_agent.as_str(), context.platform.as_str());
            Ok(json!({
                "stealthCompiled": cfg!(feature = "stealth"),
                "stealthActive": active,
                "transport": if active { "wreq/BoringSSL" } else { "reqwest" },
                "userAgent": user_agent, "platform": platform,
                "proxyConfigured": context.proxy_url.is_some(),
                "privateNetworkAllowed": context.allow_private_network,
                "nativeDownload": active,
            }))
        }
        "download" => {
            #[cfg(not(feature = "stealth"))]
            { let _ = params; Err("Native download requires a build with the stealth feature".into()) }
            #[cfg(feature = "stealth")]
            {
                if !context.stealth { return Err("Native download requires --stealth".into()); }
                let url = params.get("url").and_then(Value::as_str).ok_or("url is required")?;
                let url = url::Url::parse(url).map_err(|e| e.to_string())?;
                if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
                    return Err("Only HTTP(S) URLs without embedded credentials are supported".into());
                }
                let limit = match params.get("maxBytes") {
                    None => 32 * 1024 * 1024,
                    Some(value) => value.as_u64().filter(|n| (1..=32 * 1024 * 1024).contains(n))
                        .ok_or("maxBytes must be between 1 and 33554432")? as usize,
                };
                let timeout = match params.get("timeoutMs") {
                    None => 60_000,
                    Some(value) => value.as_u64().filter(|n| (1..=60_000).contains(n))
                        .ok_or("timeoutMs must be between 1 and 60000")?,
                };
                let client = obscura_net::StealthHttpClient::with_proxy(
                    context.cookie_jar.clone(), context.proxy_url.as_deref(), context.allow_private_network);
                let mut request = obscura_net::ResourceRequest::navigation();
                request.max_response_bytes = limit;
                let response = tokio::time::timeout(std::time::Duration::from_millis(timeout),
                    client.fetch_resource_with_callbacks(&url, request, None)).await
                    .map_err(|_| "Native download timed out".to_string())?
                    .map_err(|e| e.to_string())?;
                let size = response.body.len();
                let stream = ctx.io_streams.insert(response.body)?;
                Ok(json!({ "url": response.url.as_str(), "status": response.status,
                    "headers": response.headers, "bytes": size, "stream": stream }))
            }
        }
        _ => Err(format!("Unknown Obscura method: {}", method)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn status_reports_compile_and_runtime_separately_without_proxy_secrets() {
        let context = std::sync::Arc::new(obscura_browser::BrowserContext::with_options(
            "test".into(), Some("http://secret:password@proxy.invalid:8080".into()), false));
        let mut ctx = CdpContext::new_with_shared_context(context);
        let status = handle("getTransportInfo", &json!({}), &mut ctx, &None).await.unwrap();
        assert_eq!(status["stealthActive"], false);
        assert_eq!(status["stealthCompiled"], cfg!(feature = "stealth"));
        assert_eq!(status["proxyConfigured"], true);
        assert!(!status.to_string().contains("password"));
        assert!(handle("download", &json!({"url": "https://example.com"}), &mut ctx, &None).await.is_err());
    }

    #[cfg(feature = "stealth")]
    #[tokio::test]
    async fn native_download_rejects_non_http_urls_and_invalid_limits_before_io() {
        let context = std::sync::Arc::new(obscura_browser::BrowserContext::with_options("test".into(), None, true));
        let mut ctx = CdpContext::new_with_shared_context(context);
        for url in ["file:///etc/passwd", "data:private", "https://user:secret@example.com"] {
            assert!(handle("download", &json!({"url": url}), &mut ctx, &None).await.is_err());
        }
        for limit in [0, 33554433] {
            assert!(handle("download", &json!({"url": "https://example.com", "maxBytes": limit}), &mut ctx, &None).await.is_err());
        }
    }
}
