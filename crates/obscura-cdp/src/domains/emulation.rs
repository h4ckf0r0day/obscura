use serde_json::{json, Value};

use crate::dispatch::CdpContext;

const MAX_DEVICE_METRIC_DIMENSION: i64 = 10_000_000;

fn metric_dimension(params: &Value, name: &str) -> Result<u32, String> {
    let value = params
        .get(name)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("Emulation.setDeviceMetricsOverride requires integer {name}"))?;
    if !(0..=MAX_DEVICE_METRIC_DIMENSION).contains(&value) {
        return Err(format!(
            "Emulation.setDeviceMetricsOverride {name} must be between 0 and {MAX_DEVICE_METRIC_DIMENSION}"
        ));
    }
    Ok(value as u32)
}

fn optional_metric_dimension(params: &Value, name: &str) -> Result<Option<u32>, String> {
    params
        .get(name)
        .map(|_| metric_dimension(params, name))
        .transpose()
}

fn default_background_color(params: &Value) -> Result<Option<[u8; 4]>, String> {
    let Some(color) = params.get("color") else {
        return Ok(None);
    };
    let color = color.as_object().ok_or(
        "Emulation.setDefaultBackgroundColorOverride color must be an RGBA object",
    )?;
    let channel = |name: &str| -> Result<u8, String> {
        let value = color.get(name).and_then(Value::as_i64).ok_or_else(|| {
            format!(
                "Emulation.setDefaultBackgroundColorOverride requires integer color.{name}"
            )
        })?;
        Ok(value.clamp(0, 255) as u8)
    };
    let alpha = match color.get("a") {
        Some(value) => value.as_f64().ok_or(
            "Emulation.setDefaultBackgroundColorOverride color.a must be a number",
        )?,
        None => 1.0,
    };
    if !alpha.is_finite() {
        return Err(
            "Emulation.setDefaultBackgroundColorOverride color.a must be finite".to_string(),
        );
    }
    Ok(Some([
        channel("r")?,
        channel("g")?,
        channel("b")?,
        ((alpha as f32).clamp(0.0, 1.0) * 255.0).round() as u8,
    ]))
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "setDeviceMetricsOverride" => {
            let width = metric_dimension(params, "width")?;
            let height = metric_dimension(params, "height")?;
            let device_scale_factor = params
                .get("deviceScaleFactor")
                .and_then(Value::as_f64)
                .ok_or("Emulation.setDeviceMetricsOverride requires deviceScaleFactor")?;
            if !device_scale_factor.is_finite() || device_scale_factor < 0.0 {
                return Err(
                    "Emulation.setDeviceMetricsOverride requires a non-negative finite deviceScaleFactor"
                        .to_string(),
                );
            }
            let mobile = params
                .get("mobile")
                .and_then(Value::as_bool)
                .ok_or("Emulation.setDeviceMetricsOverride requires boolean mobile")?;
            // Parse optional dimensions independently. Even an incomplete
            // screen-size pair must reject a malformed or out-of-range member.
            let screen_width = optional_metric_dimension(params, "screenWidth")?;
            let screen_height = optional_metric_dimension(params, "screenHeight")?;
            let screen_size = match (screen_width, screen_height) {
                (Some(screen_width), Some(screen_height))
                    if screen_width > 0 && screen_height > 0 =>
                {
                    Some((screen_width as f32, screen_height as f32))
                }
                _ => None,
            };
            let page = ctx
                .get_session_page_mut(session_id)
                .ok_or("No page for session")?;
            page.apply_device_metrics_override(
                (width > 0).then_some(width as f32),
                (height > 0).then_some(height as f32),
                (device_scale_factor > 0.0).then_some(device_scale_factor as f32),
                screen_size,
                mobile,
            );
            Ok(json!({}))
        }
        "clearDeviceMetricsOverride" => {
            let page = ctx
                .get_session_page_mut(session_id)
                .ok_or("No page for session")?;
            page.clear_device_metrics_override();
            Ok(json!({}))
        }
        "setDefaultBackgroundColorOverride" => {
            let color = default_background_color(params)?;
            let page = ctx
                .get_session_page_mut(session_id)
                .ok_or("No page for session")?;
            page.set_default_background_color_override(color);
            Ok(json!({}))
        }
        // Touch emulation does not affect layout yet, but acknowledging it is
        // compatible with clients that pair it with a metrics override.
        "setTouchEmulationEnabled" => Ok(json!({})),
        _ => Ok(json!({})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn device_metrics_override_updates_page_and_window_viewport() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("viewport-session".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id);

        handle(
            "setDeviceMetricsOverride",
            &json!({
                "width": 1024,
                "height": 768,
                "deviceScaleFactor": 2,
                "mobile": false,
                "screenWidth": 1440,
                "screenHeight": 900
            }),
            &mut ctx,
            &session_id,
        )
        .await
        .expect("metrics override");

        let page = ctx
            .get_session_page_mut(&session_id)
            .expect("page for session");
        assert_eq!(page.viewport, (1024.0, 768.0));
        assert_eq!(page.device_scale_factor, 2.0);
        assert_eq!(
            page.evaluate(
                "return [innerWidth, innerHeight, visualViewport.width,\
                         visualViewport.height, screen.width, screen.height,\
                         screen.availWidth, screen.availHeight, devicePixelRatio];"
            ),
            json!([1024, 768, 1024, 768, 1440, 900, 1440, 900, 2])
        );
    }

    #[tokio::test]
    async fn omitted_screen_metrics_clear_only_the_screen_override() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("screen-session".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id);

        for params in [
            json!({
                "width": 1024,
                "height": 768,
                "deviceScaleFactor": 1,
                "mobile": false,
                "screenWidth": 1111,
                "screenHeight": 777
            }),
            json!({
                "width": 800,
                "height": 600,
                "deviceScaleFactor": 1,
                "mobile": false
            }),
        ] {
            handle("setDeviceMetricsOverride", &params, &mut ctx, &session_id)
                .await
                .unwrap();
        }

        let page = ctx.get_session_page_mut(&session_id).unwrap();
        assert_eq!(page.viewport, (800.0, 600.0));
        assert_eq!(
            page.evaluate(
                "return [innerWidth, innerHeight, screen.width !== 1111,\
                         screen.height !== 777];"
            ),
            json!([800, 600, true, true])
        );
    }

    #[tokio::test]
    async fn device_metrics_override_rejects_fractional_and_out_of_range_dimensions() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("viewport-session".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id);
        for params in [
            json!({"width": 0.5, "height": 768, "deviceScaleFactor": 1, "mobile": false}),
            json!({"width": 10_000_001, "height": 768, "deviceScaleFactor": 1, "mobile": false}),
            json!({"width": -1, "height": 768, "deviceScaleFactor": 1, "mobile": false}),
        ] {
            assert!(
                handle("setDeviceMetricsOverride", &params, &mut ctx, &session_id)
                    .await
                    .is_err(),
                "must reject {params}"
            );
        }
    }

    #[tokio::test]
    async fn zero_dimensions_disable_size_override() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("zero-size-session".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id);
        let page = ctx.get_session_page_mut(&session_id).unwrap();
        page.set_viewport((1111.0, 777.0));
        page.set_device_scale_factor(1.5);

        handle(
            "setDeviceMetricsOverride",
            &json!({
                "width": 0,
                "height": 0,
                "deviceScaleFactor": 0,
                "mobile": false
            }),
            &mut ctx,
            &session_id,
        )
        .await
        .expect("zero disables the overrides");

        let page = ctx.get_session_page_mut(&session_id).unwrap();
        assert_eq!(page.viewport, (1111.0, 777.0));
        assert_eq!(page.device_scale_factor, 1.5);
    }

    #[tokio::test]
    async fn repeated_overrides_keep_baseline_and_clear_restores_it() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("scale-session".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id);
        let page = ctx.get_session_page_mut(&session_id).unwrap();
        page.set_viewport((1111.0, 777.0));
        page.set_device_scale_factor(1.5);
        let baseline_screen = page.evaluate(
            "return [screen.width, screen.height, screen.availWidth, screen.availHeight];",
        );

        handle(
            "setDeviceMetricsOverride",
            &json!({
                "width": 640,
                "height": 480,
                "deviceScaleFactor": 3,
                "mobile": false,
                "screenWidth": 900,
                "screenHeight": 700
            }),
            &mut ctx,
            &session_id,
        )
        .await
        .unwrap();
        assert_eq!(
            ctx.get_session_page(&session_id)
                .unwrap()
                .device_scale_factor,
            3.0
        );

        handle(
            "setDeviceMetricsOverride",
            &json!({
                "width": 0,
                "height": 333,
                "deviceScaleFactor": 0,
                "mobile": false
            }),
            &mut ctx,
            &session_id,
        )
        .await
        .expect("zero restores the corresponding baseline metric");
        let page = ctx.get_session_page(&session_id).unwrap();
        assert_eq!(page.viewport, (1111.0, 333.0));
        assert_eq!(page.device_scale_factor, 1.5);

        handle(
            "clearDeviceMetricsOverride",
            &json!({}),
            &mut ctx,
            &session_id,
        )
        .await
        .unwrap();
        let page = ctx.get_session_page_mut(&session_id).unwrap();
        assert_eq!(page.viewport, (1111.0, 777.0));
        assert_eq!(page.device_scale_factor, 1.5);
        assert_eq!(
            page.evaluate(
                "return [screen.width, screen.height, screen.availWidth, screen.availHeight];"
            ),
            baseline_screen
        );
    }

    #[tokio::test]
    async fn inactive_clear_is_a_no_op() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("inactive-clear-session".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id);
        let page = ctx.get_session_page_mut(&session_id).unwrap();
        page.set_viewport((901.0, 607.0));
        page.set_device_scale_factor(1.25);

        handle(
            "clearDeviceMetricsOverride",
            &json!({}),
            &mut ctx,
            &session_id,
        )
        .await
        .unwrap();

        let page = ctx.get_session_page(&session_id).unwrap();
        assert_eq!(page.viewport, (901.0, 607.0));
        assert_eq!(page.device_scale_factor, 1.25);
    }

    #[tokio::test]
    async fn mobile_without_complete_screen_size_uses_effective_viewport() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("mobile-screen-session".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id);

        for params in [
            json!({
                "width": 800,
                "height": 600,
                "deviceScaleFactor": 1,
                "mobile": true
            }),
            json!({
                "width": 700,
                "height": 500,
                "deviceScaleFactor": 1,
                "mobile": true,
                "screenWidth": 1000
            }),
        ] {
            handle("setDeviceMetricsOverride", &params, &mut ctx, &session_id)
                .await
                .unwrap();
        }

        let page = ctx.get_session_page_mut(&session_id).unwrap();
        assert_eq!(
            page.evaluate(
                "return [screen.width, screen.height, screen.availWidth, screen.availHeight];"
            ),
            json!([700, 500, 700, 500])
        );
    }

    #[tokio::test]
    async fn validates_mobile_and_each_optional_screen_dimension() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("validation-session".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id);

        for params in [
            json!({"width": 800, "height": 600, "deviceScaleFactor": 1}),
            json!({"width": 800, "height": 600, "deviceScaleFactor": 1, "mobile": "false"}),
            json!({"width": 800, "height": 600, "deviceScaleFactor": 1, "mobile": false, "screenWidth": -1}),
            json!({"width": 800, "height": 600, "deviceScaleFactor": 1, "mobile": false, "screenHeight": 0.5}),
            json!({"width": 800, "height": 600, "deviceScaleFactor": 1, "mobile": false, "screenWidth": 10_000_001}),
        ] {
            assert!(
                handle("setDeviceMetricsOverride", &params, &mut ctx, &session_id)
                    .await
                    .is_err(),
                "must reject {params}"
            );
        }
    }

    #[test]
    fn default_background_color_matches_blink_defaults_rounding_and_clamping() {
        assert_eq!(default_background_color(&json!({})), Ok(None));
        assert_eq!(
            default_background_color(&json!({"color": {"r": 1, "g": 2, "b": 3}})),
            Ok(Some([1, 2, 3, 255]))
        );
        assert_eq!(
            default_background_color(&json!({
                "color": {"r": -20, "g": 400, "b": 30, "a": 16.0 / 255.0}
            })),
            Ok(Some([0, 255, 30, 16]))
        );
        assert_eq!(
            default_background_color(&json!({
                "color": {"r": 1, "g": 2, "b": 3, "a": 2}
            })),
            Ok(Some([1, 2, 3, 255]))
        );
        assert_eq!(
            default_background_color(&json!({
                "color": {"r": 1, "g": 2, "b": 3, "a": -1}
            })),
            Ok(Some([1, 2, 3, 0]))
        );
    }

    #[test]
    fn default_background_color_rejects_malformed_rgba() {
        for params in [
            json!({"color": null}),
            json!({"color": {"g": 2, "b": 3}}),
            json!({"color": {"r": 1.5, "g": 2, "b": 3}}),
            json!({"color": {"r": 1, "g": 2, "b": 3, "a": "opaque"}}),
        ] {
            assert!(
                default_background_color(&params).is_err(),
                "must reject {params}"
            );
        }
    }
}
