use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use obscura_net::{CookieJar, DocumentInterruption, DocumentProgress, DocumentRequest,
    ObscuraHttpClient, ResourceRequest, ResourceType, Response};

#[path = "support/partial_server.rs"]
mod fixture;

fn response(url: &str, body: Vec<u8>, mime: &str) -> Response {
    Response { url: url.parse().unwrap(), status: 200,
        headers: HashMap::from([("content-type".into(), mime.into())]), body, redirected_from: Vec::new() }
}

#[tokio::test]
async fn cancellation_moves_one_primary_buffer_and_excludes_nested_interception() {
    let progress = DocumentProgress::default();
    let bytes = vec![42; 4096];
    let pointer = bytes.as_ptr() as usize;
    let outcome = tokio::time::timeout(Duration::from_millis(20), progress.scope(async {
        let primary = DocumentRequest::claim(ResourceType::Document)
            .response(response("https://example.test/main", bytes, "text/html"), "POST", "https://example.test/");
        DocumentProgress::mark_intercepted(Some(&"https://example.test/nested".parse().unwrap()));
        let nested = DocumentRequest::claim(ResourceType::Document)
            .response(response("https://example.test/nested", vec![9], "text/html"), "GET", "");
        drop(nested);
        assert!(progress.take().is_none());
        std::future::pending::<()>().await;
        drop(primary);
    })).await;
    assert!(outcome.is_err());
    let kept = progress.take().unwrap();
    assert_eq!(kept.response.url.as_str(), "https://example.test/main");
    assert_eq!(kept.response.body.as_ptr() as usize, pointer);
    assert_eq!(kept.response.body, vec![42; 4096]);
    assert_eq!(kept.method, "POST");
    assert_eq!(kept.referrer, "https://example.test/");
    assert!(kept.intercepted_document_url.is_none());
    assert!(!kept.intercepted);
    assert!(!kept.body_complete);
    assert_eq!(kept.cause, DocumentInterruption::Cancelled);
    assert!(progress.take().is_none());
}

#[tokio::test]
async fn non_documents_non_html_and_non_timeout_failures_are_not_recovered() {
    for (resource, mime, failed) in [
        (ResourceType::Image, "text/html", false),
        (ResourceType::Document, "application/json", false),
        (ResourceType::Document, "image/png", false),
        (ResourceType::Document, "text/html", true),
    ] {
        let progress = DocumentProgress::default();
        progress.scope(async {
            let mut primary = DocumentRequest::claim(resource)
                .response(response("https://example.test/", vec![42], mime), "GET", "");
            if failed { primary.failed(false); }
            drop(primary);
        }).await;
        assert!(progress.take().is_none(), "{resource:?}, {mime}");
    }
}

#[tokio::test]
async fn real_body_timeout_preserves_html_without_changing_healthy_requests_or_caps() {
    let (origin, _) = fixture::serve().await;
    let mut client = ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()), None, true);
    client.timeout = Duration::from_millis(150);
    let progress = DocumentProgress::default();
    let url = format!("{origin}/slow").parse().unwrap();
    assert!(progress.scope(client.fetch(&url)).await.is_err());
    let kept = progress.take().unwrap();
    assert_eq!(kept.cause, DocumentInterruption::BodyTimeout);
    assert!(!kept.body_complete);
    assert!(kept.response.body.starts_with(b"<!doctype html>"));
    let healthy = format!("{origin}/healthy").parse().unwrap();
    assert_eq!(progress.scope(client.fetch(&healthy)).await.unwrap().status, 200);
    assert!(progress.take().is_none());
    let oversize = format!("{origin}/oversize").parse().unwrap();
    assert!(progress.scope(client.fetch(&oversize)).await.is_err());
    assert!(progress.take().is_none());
    let request = ResourceRequest::navigation().with_max_response_bytes(8);
    assert!(progress.scope(client.fetch_resource_with_callbacks(&healthy, request, None)).await.is_err());
    assert!(progress.take().is_none());
}
