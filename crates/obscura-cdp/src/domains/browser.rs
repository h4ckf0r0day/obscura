use serde_json::{json, Value};

pub async fn handle(method: &str, _params: &Value) -> Result<Value, String> {
    match method {
        "getVersion" => Ok(json!({
            "protocolVersion": "1.3",
            "product": "Chrome/145.0.0.0",
            "revision": "@0000000000000000000000000000000000000000",
            "userAgent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
            "jsVersion": "14.5.0.0",
        })),
        "close" => {
            Ok(json!({}))
        }
        "getWindowForTarget" => Ok(json!({
            "windowId": 1,
            "bounds": {
                "left": 0,
                "top": 0,
                "width": 1280,
                "height": 720,
                "windowState": "normal",
            }
        })),
        "setDownloadBehavior" => Ok(json!({})),
        "getWindowBounds" => Ok(json!({
            "bounds": { "left": 0, "top": 0, "width": 1280, "height": 720, "windowState": "normal" }
        })),
        // No-op acks for window-management methods Playwright sends during
        // page setup. We don't model real OS windows, but answering with {}
        // lets the client's setup sequence complete instead of tearing down
        // the page on an unknown-method error.
        "setWindowBounds" => Ok(json!({})),
        _ => Err(format!("Unknown Browser method: {}", method)),
    }
}
