use serde_json::{json, Value};

pub async fn handle(method: &str, _params: &Value) -> Result<Value, String> {
    match method {
        "getProcessInfo" => Ok(json!({
            "processInfo": [{
                "type": "browser",
                "id": std::process::id(),
                "cpuTime": 0,
            }],
        })),
        _ => Err(format!("Unknown SystemInfo method: {method}")),
    }
}
