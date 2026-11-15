//! `SystemInfo.getProcessInfo` reports the server process as the one browser process.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Value {
    let resp = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: session.map(str::to_string),
        },
        ctx,
    )
    .await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

#[tokio::test]
async fn process_info_reports_the_server_as_browser() {
    let mut ctx = CdpContext::new();
    let result = cdp(&mut ctx, 1, "SystemInfo.getProcessInfo", json!({}), None).await;
    let info = result["processInfo"].as_array().expect("processInfo array");
    assert_eq!(info.len(), 1);
    assert_eq!(info[0]["type"], "browser");
    assert_eq!(info[0]["id"], json!(std::process::id()));
    assert_eq!(info[0]["cpuTime"], json!(0));
}

#[tokio::test]
async fn unknown_systeminfo_method_is_an_error() {
    let mut ctx = CdpContext::new();
    let resp = dispatch(
        &CdpRequest {
            id: 1,
            method: "SystemInfo.getFeatureState".to_string(),
            params: json!({}),
            session_id: None,
        },
        &mut ctx,
    )
    .await;
    let err = resp.error.expect("expected an error");
    assert!(
        format!("{err:?}").contains("Unknown SystemInfo method"),
        "unexpected error: {err:?}"
    );
}
