use super::*;
use crate::{DocumentInterruption, DocumentProgress};
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "../tests/support/partial_server.rs"]
mod fixture;

#[tokio::test]
async fn callback_wait_cancellation_retains_a_completed_body_without_fabricating_callbacks() {
    let (origin, _) = fixture::serve().await;
    let client = ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()), None, true);
    let callbacks = CallbackRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    callbacks.add_response(Arc::new(move |_, _| { observed.fetch_add(1, Ordering::SeqCst); }));
    let lock = callbacks.on_response.write().await;
    let progress = DocumentProgress::default();
    let url = format!("{origin}/healthy").parse().unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(250),
        progress.scope(client.fetch_with_callbacks(&url, Some(&callbacks)))).await.is_err());
    drop(lock);
    let kept = progress.take().unwrap();
    assert!(kept.body_complete);
    assert_eq!(kept.cause, DocumentInterruption::Cancelled);
    assert_eq!(kept.response.status, 200);
    assert!(kept.response.body.starts_with(b"<!doctype html>"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
