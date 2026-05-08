use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "setDeviceMetricsOverride" => {
            let width = params
                .get("width")
                .and_then(|v| v.as_u64())
                .unwrap_or(1920)
                .min(u32::MAX as u64) as u32;
            let height = params
                .get("height")
                .and_then(|v| v.as_u64())
                .unwrap_or(1000)
                .min(u32::MAX as u64) as u32;
            let device_scale_factor = params
                .get("deviceScaleFactor")
                .and_then(|v| v.as_f64())
                .unwrap_or(2.0);

            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.set_viewport_metrics(width, height, device_scale_factor);
            }

            Ok(json!({}))
        }
        "clearDeviceMetricsOverride" => {
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.set_viewport_metrics(1920, 1000, 2.0);
            }
            Ok(json!({}))
        }
        "setTouchEmulationEnabled" | "setEmulatedMedia" | "setTimezoneOverride"
        | "setLocaleOverride" | "setCPUThrottlingRate" | "setScriptExecutionDisabled"
        | "setFocusEmulationEnabled" | "setScrollbarsHidden" | "setDefaultBackgroundColorOverride" => {
            Ok(json!({}))
        }
        _ => Err(format!("Unknown Emulation method: {}", method)),
    }
}
