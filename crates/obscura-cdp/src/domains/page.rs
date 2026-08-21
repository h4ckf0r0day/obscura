use obscura_browser::lifecycle::WaitUntil;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;
use crate::types::CdpEvent;
use crate::util::url_is_file_scheme;

#[cfg(feature = "render")]
use crate::dispatch::{ScreencastFormat, ScreencastState};

#[cfg(feature = "render")]
const DEFAULT_SCREENSHOT_QUALITY: i64 = 80;

/// CDP frame ids are strings, while a realm is identified by number, so a
/// child's protocol id is derived from its page's. It stays stable for the
/// life of the frame, which is what lets a client match an attach event to the
/// frame it later sees in the tree.
fn child_frame_id(page_frame_id: &str, frame_id: u32) -> String {
    format!("{page_frame_id}-frame-{frame_id}")
}

fn child_frame_value(page_frame_id: &str, frame: &obscura_js::frame::FrameRealm) -> Value {
    let id = child_frame_id(page_frame_id, frame.frame_id());
    let parent_id = if frame.parent_frame_id() == 0 {
        page_frame_id.to_string()
    } else {
        child_frame_id(page_frame_id, frame.parent_frame_id())
    };
    json!({
        "id": id,
        "parentId": parent_id,
        "loaderId": format!("{id}-loader"),
        "url": frame.url(),
        "domainAndRegistry": "",
        "securityOrigin": frame.origin(),
        "mimeType": "text/html",
        "adFrameStatus": { "adFrameType": "none" },
    })
}

/// The page's child frames, flattened with each parent ahead of its children
/// so an attach event never names a parent the client has not seen yet.
pub fn child_frame_values(page: &obscura_browser::Page) -> Vec<Value> {
    fn walk(page: &obscura_browser::Page, parent_frame_id: u32, out: &mut Vec<Value>) {
        for frame in page
            .frames
            .iter()
            .filter(|frame| frame.parent_frame_id() == parent_frame_id)
        {
            out.push(child_frame_value(&page.frame_id, frame));
            walk(page, frame.frame_id(), out);
        }
    }

    let mut out = Vec::new();
    walk(page, 0, &mut out);
    out
}

/// The page's live frame hierarchy. `childFrames` used to be hardcoded empty,
/// so a client was told the page had no frames however many it had built.
fn frame_tree(page: &obscura_browser::Page) -> Value {
    fn children(page: &obscura_browser::Page, parent_frame_id: u32) -> Vec<Value> {
        page.frames
            .iter()
            .filter(|frame| frame.parent_frame_id() == parent_frame_id)
            .map(|frame| {
                json!({
                    "frame": child_frame_value(&page.frame_id, frame),
                    "childFrames": children(page, frame.frame_id()),
                })
            })
            .collect()
    }

    json!({
        "frame": {
            "id": page.frame_id,
            "loaderId": "initial-loader",
            "url": page.url_string(),
            "domainAndRegistry": "",
            "securityOrigin": page.url_string(),
            "mimeType": "text/html",
            "adFrameStatus": { "adFrameType": "none" },
        },
        "childFrames": children(page, 0),
    })
}
#[cfg(feature = "render")]
const MAX_SCREENCAST_FRAMES_IN_FLIGHT: u8 = 2;
#[cfg(feature = "render")]
const MAX_LONG_PNG_PIXELS: u64 = 32 * 1024 * 1024;
#[cfg(feature = "render")]
const MAX_LONG_PNG_DIMENSION: u32 = 128 * 1024 - 1;

#[cfg(feature = "render")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScreenshotFormat {
    Png,
    Jpeg,
    Webp,
}

#[cfg(feature = "render")]
#[derive(Clone, Copy, Debug)]
struct ScreenshotClip {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
}

#[cfg(feature = "render")]
#[derive(Clone, Copy, Debug)]
struct ScreenshotOptions {
    format: ScreenshotFormat,
    quality: u8,
    quality_supplied: bool,
    clip: Option<ScreenshotClip>,
    from_surface: bool,
    capture_beyond_viewport: bool,
    optimize_for_speed: bool,
}

#[cfg(feature = "render")]
fn screenshot_bool(params: &Value, name: &str, default: bool) -> Result<bool, String> {
    match params.get(name) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("Invalid parameters: {name} must be a boolean")),
    }
}

#[cfg(feature = "render")]
fn screenshot_number(object: &serde_json::Map<String, Value>, name: &str) -> Result<f64, String> {
    let value = object
        .get(name)
        .ok_or_else(|| format!("Invalid parameters: mandatory clip.{name} field missing"))?
        .as_f64()
        .ok_or_else(|| format!("Invalid parameters: clip.{name} must be a number"))?;
    if !value.is_finite() {
        return Err(format!("Invalid parameters: clip.{name} must be finite"));
    }
    Ok(value)
}

#[cfg(feature = "render")]
fn parse_screenshot_options(params: &Value) -> Result<ScreenshotOptions, String> {
    if !params.is_object() {
        return Err("Invalid parameters: expected an object".to_string());
    }

    let format = match params.get("format") {
        None => ScreenshotFormat::Png,
        Some(Value::String(value)) => match value.as_str() {
            "png" => ScreenshotFormat::Png,
            "jpeg" => ScreenshotFormat::Jpeg,
            "webp" => ScreenshotFormat::Webp,
            _ => return Err("Invalid image format".to_string()),
        },
        Some(_) => return Err("Invalid parameters: format must be a string".to_string()),
    };

    let quality_supplied = params.get("quality").is_some();
    let quality = match params.get("quality") {
        None => DEFAULT_SCREENSHOT_QUALITY,
        Some(value) => value
            .as_i64()
            .filter(|value| i32::try_from(*value).is_ok())
            .ok_or_else(|| "Invalid parameters: quality must be an integer".to_string())?,
    };
    // Chromium accepts an int32 outside [0, 100], but deliberately falls back
    // to its default quality instead of rejecting the request.
    let quality = if (0..=100).contains(&quality) {
        quality
    } else {
        DEFAULT_SCREENSHOT_QUALITY
    } as u8;

    let clip = match params.get("clip") {
        None => None,
        Some(Value::Object(object)) => {
            let clip = ScreenshotClip {
                x: screenshot_number(object, "x")?,
                y: screenshot_number(object, "y")?,
                width: screenshot_number(object, "width")?,
                height: screenshot_number(object, "height")?,
                scale: screenshot_number(object, "scale")?,
            };
            if clip.width == 0.0 {
                return Err("Cannot take screenshot with 0 width.".to_string());
            }
            if clip.height == 0.0 {
                return Err("Cannot take screenshot with 0 height.".to_string());
            }
            // Chromium's current handler only checks zero, but negative sizes
            // and scales enter invalid gfx sizes and may stall the request.
            // Fail deterministically instead of risking an allocation or hang.
            if clip.width < 0.0 {
                return Err("Cannot take screenshot with negative width.".to_string());
            }
            if clip.height < 0.0 {
                return Err("Cannot take screenshot with negative height.".to_string());
            }
            if clip.scale <= 0.0 {
                return Err("Cannot take screenshot with non-positive scale.".to_string());
            }
            Some(clip)
        }
        Some(_) => return Err("Invalid parameters: clip must be an object".to_string()),
    };

    Ok(ScreenshotOptions {
        format,
        quality,
        quality_supplied,
        clip,
        from_surface: screenshot_bool(params, "fromSurface", true)?,
        capture_beyond_viewport: screenshot_bool(params, "captureBeyondViewport", false)?,
        optimize_for_speed: screenshot_bool(params, "optimizeForSpeed", false)?,
    })
}

#[cfg(feature = "render")]
fn capture_error_message(error: obscura_browser::CaptureError) -> String {
    match error {
        obscura_browser::CaptureError::InvalidRegion => {
            "Page.captureScreenshot received an invalid capture region".to_string()
        }
        obscura_browser::CaptureError::AllocationLimitExceeded => {
            "Page.captureScreenshot bitmap is too large".to_string()
        }
        obscura_browser::CaptureError::PaintFailed => {
            "Page.captureScreenshot failed: the page has no retained DOM surface to render"
                .to_string()
        }
        obscura_browser::CaptureError::EncodeFailed => {
            "Page.captureScreenshot renderer PNG encoding failed".to_string()
        }
    }
}

#[cfg(feature = "render")]
fn chromium_clip_region(
    clip: ScreenshotClip,
    device_scale_factor: f64,
) -> Result<obscura_browser::CaptureRegion, String> {
    // Chromium first converts the CSS clip size to an integer gfx::Size, then
    // applies the effective clip/device scale and rounds to output pixels.
    // Keeping this calculation here avoids asking the raster layer to infer a
    // protocol-specific size from the original fractional rectangle.
    let width = clip.width.trunc();
    let height = clip.height.trunc();
    if width <= 0.0 || height <= 0.0 {
        return Err("Screenshot clip is too small at the requested scale.".to_string());
    }
    let effective_scale = clip.scale * device_scale_factor;
    let output_width = (width * effective_scale).round();
    let output_height = (height * effective_scale).round();
    if !effective_scale.is_finite()
        || effective_scale <= 0.0
        || !output_width.is_finite()
        || !output_height.is_finite()
        || output_width <= 0.0
        || output_height <= 0.0
        || output_width > f64::from(u32::MAX)
        || output_height > f64::from(u32::MAX)
    {
        return Err("Page.captureScreenshot bitmap is too large".to_string());
    }
    Ok(obscura_browser::CaptureRegion::with_output_size(
        clip.x as f32,
        clip.y as f32,
        width as f32,
        height as f32,
        effective_scale as f32,
        output_width as u32,
        output_height as u32,
    ))
}

#[cfg(feature = "render")]
fn encode_screenshot(
    image: &image::RgbaImage,
    options: ScreenshotOptions,
) -> Result<Vec<u8>, String> {
    use image::ImageEncoder as _;

    let mut output = Vec::new();
    match options.format {
        ScreenshotFormat::Png => {
            let encoder = if options.optimize_for_speed {
                image::codecs::png::PngEncoder::new_with_quality(
                    &mut output,
                    image::codecs::png::CompressionType::Fast,
                    image::codecs::png::FilterType::NoFilter,
                )
            } else {
                image::codecs::png::PngEncoder::new(&mut output)
            };
            encoder
                .write_image(
                    image.as_raw(),
                    image.width(),
                    image.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|error| format!("PNG screenshot encoding failed: {error}"))?;
        }
        ScreenshotFormat::Jpeg => {
            let rgb = image::DynamicImage::ImageRgba8(image.clone()).to_rgb8();
            // image's pure-Rust JPEG encoder defines its quality range as
            // 1..=100; Chromium permits zero, whose effective result is the
            // lowest-quality encoding.
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut output,
                options.quality.max(1),
            );
            encoder
                .write_image(
                    rgb.as_raw(),
                    rgb.width(),
                    rgb.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|error| format!("JPEG screenshot encoding failed: {error}"))?;
        }
        ScreenshotFormat::Webp => {
            if options.quality_supplied {
                return Err(
                    "WebP screenshot quality is not supported by the current lossless encoder"
                        .to_string(),
                );
            }
            image::codecs::webp::WebPEncoder::new_lossless(&mut output)
                .write_image(
                    image.as_raw(),
                    image.width(),
                    image.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|error| format!("WebP screenshot encoding failed: {error}"))?;
        }
    }
    Ok(output)
}

/// Encode a full-page PNG a bounded horizontal strip at a time. The ordinary
/// single-surface path stays faster for captures within the renderer's limits;
/// this fallback exists for tall pages whose complete RGBA surface would exceed
/// that bound. Each strip is independently validated by the renderer and only
/// one decoded strip is live while the PNG encoder consumes its scanlines.
#[cfg(feature = "render")]
fn encode_long_full_page_png(
    page: &obscura_browser::Page,
    content_size: (f32, f32),
    scale: f32,
    animation_sample: obscura_js::AnimationSample,
    optimize_for_speed: bool,
) -> Result<Vec<u8>, String> {
    use std::io::Write as _;

    let (width, height) = content_size;
    if !width.is_finite()
        || !height.is_finite()
        || !scale.is_finite()
        || width <= 0.0
        || height <= 0.0
        || scale <= 0.0
    {
        return Err("Page.captureScreenshot received an invalid full-page region".to_string());
    }

    let native_width = width.ceil() as u64;
    let native_height = height.ceil() as u64;
    let output_width_value = (f64::from(width) * f64::from(scale)).round();
    let output_height_value = (f64::from(height) * f64::from(scale)).round();
    if native_width == 0
        || native_height == 0
        || native_width > u64::from(obscura_js::MAX_CAPTURE_DIMENSION)
        || !output_width_value.is_finite()
        || !output_height_value.is_finite()
        || output_width_value <= 0.0
        || output_height_value <= 0.0
        || output_width_value > f64::from(obscura_js::MAX_CAPTURE_DIMENSION)
        || output_height_value > f64::from(MAX_LONG_PNG_DIMENSION)
    {
        return Err("Page.captureScreenshot long PNG dimensions are too large".to_string());
    }
    let output_width = output_width_value as u32;
    let output_height = output_height_value as u32;
    let native_pixels = native_width
        .checked_mul(native_height)
        .ok_or_else(|| "Page.captureScreenshot long PNG size overflow".to_string())?;
    let output_pixels = u64::from(output_width)
        .checked_mul(u64::from(output_height))
        .ok_or_else(|| "Page.captureScreenshot long PNG size overflow".to_string())?;
    if native_pixels > MAX_LONG_PNG_PIXELS || output_pixels > MAX_LONG_PNG_PIXELS {
        return Err(format!(
            "Page.captureScreenshot long PNG exceeds the {}-pixel safety limit",
            MAX_LONG_PNG_PIXELS
        ));
    }

    let max_native_rows = (obscura_js::MAX_CAPTURE_PIXELS / native_width)
        .min(u64::from(obscura_js::MAX_CAPTURE_DIMENSION));
    let max_output_rows = (obscura_js::MAX_CAPTURE_PIXELS / u64::from(output_width))
        .min(u64::from(obscura_js::MAX_CAPTURE_DIMENSION));
    let native_bounded_output_rows =
        (max_native_rows.saturating_sub(1) as f64 * f64::from(scale)).floor() as u64;
    if max_native_rows == 0 || max_output_rows == 0 || native_bounded_output_rows == 0 {
        return Err("Page.captureScreenshot bitmap is too large".to_string());
    }
    // Keep each decoded strip materially below the renderer's absolute limit.
    // This leaves headroom for the encoder's scanlines and compressed output.
    let target_output_rows = max_output_rows
        .min(native_bounded_output_rows)
        .min(4096)
        .max(1) as u32;

    let mut encoded = Vec::new();
    let mut encoder = png::Encoder::new(&mut encoded, output_width, output_height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    if optimize_for_speed {
        encoder.set_compression(png::Compression::Fast);
        encoder.set_filter(png::Filter::NoFilter);
    }
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("Page.captureScreenshot striped PNG header failed: {error}"))?;
    {
        let mut stream = writer
            .stream_writer_with_size(64 * 1024)
            .map_err(|error| format!("Page.captureScreenshot striped PNG stream failed: {error}"))?;
        let mut output_y = 0u32;
        while output_y < output_height {
            let next_output_y = output_y
                .saturating_add(target_output_rows)
                .min(output_height);
            // Derive document-space boundaries from global output rows. The
            // neighboring strips therefore share the exact same rounded edge,
            // rather than accumulating independent per-strip rounding error.
            let css_y = output_y as f64 / f64::from(scale);
            let css_end = if next_output_y == output_height {
                f64::from(height)
            } else {
                next_output_y as f64 / f64::from(scale)
            };
            let css_height = css_end - css_y;
            if css_height <= 0.0 || css_height.ceil() as u64 > max_native_rows {
                return Err("Page.captureScreenshot could not form a bounded PNG strip".to_string());
            }
            let strip_height = next_output_y - output_y;
            let region = obscura_browser::CaptureRegion::with_output_size(
                0.0,
                css_y as f32,
                width,
                css_height as f32,
                scale,
                output_width,
                strip_height,
            );
            let strip_png = page
                .screenshot_region_with_animation_sample(region, animation_sample)
                .map_err(capture_error_message)?;
            let strip = image::load_from_memory_with_format(&strip_png, image::ImageFormat::Png)
                .map_err(|error| {
                    format!("Page.captureScreenshot could not decode a PNG strip: {error}")
                })?
                .to_rgba8();
            if strip.dimensions() != (output_width, strip_height) {
                return Err(format!(
                    "Page.captureScreenshot PNG strip has dimensions {:?}, expected ({output_width}, {strip_height})",
                    strip.dimensions()
                ));
            }
            stream
                .write_all(strip.as_raw())
                .map_err(|error| format!("Page.captureScreenshot striped PNG write failed: {error}"))?;
            output_y = next_output_y;
        }
        stream
            .finish()
            .map_err(|error| format!("Page.captureScreenshot striped PNG finish failed: {error}"))?;
    }
    writer
        .finish()
        .map_err(|error| format!("Page.captureScreenshot striped PNG finish failed: {error}"))?;
    Ok(encoded)
}

/// Keep capture itself an observation of the retained page state. Chromium's
/// capture methods do not start a hidden resource-loading phase, and doing so
/// here added up to three seconds to every screenshot/PDF/screencast start.
/// The browser navigation and settle paths seed render resources proactively.
/// Retain the old environment variable only as an explicit diagnostic escape
/// hatch for callers investigating a slow or missing asset.
#[cfg(feature = "render")]
pub(crate) async fn prepare_capture_resources_if_requested(
    page: &mut obscura_browser::Page,
) {
    let Some(deadline_ms) = std::env::var("OBSCURA_RENDER_RESOURCE_DEADLINE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value != 0)
    else {
        return;
    };
    let _ = page.prepare_screenshot_resources(deadline_ms).await;
}

#[cfg(feature = "render")]
fn screencast_int32(params: &Value, name: &str) -> Result<Option<i64>, String> {
    match params.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_i64()
            .filter(|value| i32::try_from(*value).is_ok())
            .map(Some)
            .ok_or_else(|| format!("Invalid parameters: {name} must be an integer")),
    }
}

#[cfg(feature = "render")]
fn parse_screencast_state(params: &Value, session_id: i64) -> Result<ScreencastState, String> {
    if !params.is_object() {
        return Err("Invalid parameters: expected an object".into());
    }
    let format = match params.get("format") {
        None => ScreencastFormat::Png,
        Some(Value::String(value)) if value == "png" => ScreencastFormat::Png,
        Some(Value::String(value)) if value == "jpeg" => ScreencastFormat::Jpeg,
        Some(Value::String(_)) => {
            return Err("Invalid parameters: screencast format must be png or jpeg".into())
        }
        Some(_) => return Err("Invalid parameters: format must be a string".into()),
    };
    let quality = screencast_int32(params, "quality")?.unwrap_or(DEFAULT_SCREENSHOT_QUALITY);
    let quality = if (0..=100).contains(&quality) {
        quality
    } else {
        DEFAULT_SCREENSHOT_QUALITY
    } as u8;
    let dimension = |name: &str| -> Result<Option<u32>, String> {
        Ok(screencast_int32(params, name)?
            .filter(|value| *value > 0)
            .map(|value| value as u32))
    };
    let every_nth_frame = screencast_int32(params, "everyNthFrame")?.unwrap_or(1);
    if every_nth_frame <= 0 {
        return Err("Invalid parameters: everyNthFrame must be greater than zero".into());
    }
    Ok(ScreencastState {
        format,
        quality,
        max_width: dimension("maxWidth")?,
        max_height: dimension("maxHeight")?,
        every_nth_frame: every_nth_frame as u32,
        command_frame_counter: 0,
        session_id,
        frames_in_flight: 0,
        observed_activity_generation: 0,
        autonomous_frame_pending: false,
    })
}

#[cfg(feature = "render")]
fn encode_screencast_frame(
    renderer_png: Vec<u8>,
    state: &ScreencastState,
) -> Result<Vec<u8>, String> {
    if state.format == ScreencastFormat::Png
        && state.max_width.is_none()
        && state.max_height.is_none()
    {
        return Ok(renderer_png);
    }
    let source = image::load_from_memory_with_format(&renderer_png, image::ImageFormat::Png)
        .map_err(|error| format!("Page.startScreencast could not decode renderer PNG: {error}"))?
        .to_rgba8();
    let mut scale = 1.0_f64;
    if let Some(max_width) = state.max_width {
        scale = scale.min(f64::from(max_width) / f64::from(source.width()));
    }
    if let Some(max_height) = state.max_height {
        scale = scale.min(f64::from(max_height) / f64::from(source.height()));
    }
    let size = (
        (f64::from(source.width()) * scale).round().max(1.0) as u32,
        (f64::from(source.height()) * scale).round().max(1.0) as u32,
    );
    let raster = if size == source.dimensions() {
        source
    } else {
        image::imageops::resize(
            &source,
            size.0,
            size.1,
            image::imageops::FilterType::Triangle,
        )
    };
    let format = match state.format {
        ScreencastFormat::Png => ScreenshotFormat::Png,
        ScreencastFormat::Jpeg => ScreenshotFormat::Jpeg,
    };
    encode_screenshot(
        &raster,
        ScreenshotOptions {
            format,
            quality: state.quality,
            quality_supplied: true,
            clip: None,
            from_surface: true,
            capture_beyond_viewport: false,
            optimize_for_speed: false,
        },
    )
}

/// Queue a visible-viewport frame through normal CDP event transport. This is
/// intentionally command-driven until Obscura has a compositor frame pump.
#[cfg(feature = "render")]
pub(crate) fn queue_screencast_frame(
    ctx: &mut CdpContext,
    cdp_session_id: &Option<String>,
    force: bool,
) -> Result<bool, String> {
    let cdp_session_id = cdp_session_id
        .as_deref()
        .ok_or("Page.startScreencast requires an attached target session")?;
    let state = {
        let Some(state) = ctx.screencasts.get_mut(cdp_session_id) else {
            return Ok(false);
        };
        if state.frames_in_flight >= MAX_SCREENCAST_FRAMES_IN_FLIGHT {
            return Ok(false);
        }
        if !force {
            state.command_frame_counter = state.command_frame_counter.saturating_add(1);
            if state.command_frame_counter % u64::from(state.every_nth_frame) != 0 {
                return Ok(false);
            }
        }
        state.clone()
    };
    let attached_session = Some(cdp_session_id.to_string());
    let (viewport, scroll, png, activity_generation) = {
        let page = ctx
            .get_session_page_mut(&attached_session)
            .ok_or("No page for session")?;
        let animation_sample = page.live_animation_sample();
        let viewport = page.viewport;
        let scroll = page
            .evaluate("[window.scrollX, window.scrollY]")
            .as_array()
            .map(|values| {
                (
                    values.first().and_then(Value::as_f64).unwrap_or(0.0),
                    values.get(1).and_then(Value::as_f64).unwrap_or(0.0),
                )
            })
            .unwrap_or((0.0, 0.0));
        obscura_browser::validate_capture_region(obscura_browser::CaptureRegion::new(
            scroll.0 as f32,
            scroll.1 as f32,
            viewport.0,
            viewport.1,
            1.0,
        ))
        .map_err(capture_error_message)?;
        let png = page
            .screenshot_with_animation_sample(viewport, animation_sample)
            .ok_or_else(|| {
                "Page.startScreencast failed: the page has no visible DOM surface to render"
                    .to_string()
            })?;
        let activity_generation = page
            .js
            .as_ref()
            .map(|js| js.activity_generation())
            .unwrap_or(0);
        (viewport, scroll, png, activity_generation)
    };
    let encoded = encode_screencast_frame(png, &state)?;
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(encoded);
    let Some(live) = ctx.screencasts.get_mut(cdp_session_id) else {
        return Ok(false);
    };
    if live.session_id != state.session_id {
        return Ok(false);
    }
    live.frames_in_flight = live.frames_in_flight.saturating_add(1);
    live.observed_activity_generation = activity_generation;
    live.autonomous_frame_pending = false;
    ctx.pending_events.push(CdpEvent {
        method: "Page.screencastFrame".into(),
        params: json!({
            "data": data,
            "metadata": {
                "offsetTop": 0.0, "pageScaleFactor": 1.0,
                "deviceWidth": viewport.0, "deviceHeight": viewport.1,
                "scrollOffsetX": scroll.0, "scrollOffsetY": scroll.1,
                "timestamp": timestamp(),
            },
            "sessionId": state.session_id,
        }),
        session_id: Some(cdp_session_id.to_string()),
    });
    Ok(true)
}

/// Advance active page task queues for one bounded compositor slice and emit
/// a frame when connected-document activity has changed. Chromium feeds
/// `Page.screencastFrame` from its video consumer on compositor frames; this
/// is Obscura's single-threaded equivalent until it owns a real compositor.
///
/// Generation tracking avoids full-page raster work while the page is idle.
/// A dirty generation is retained across `everyNthFrame` sampling and the
/// acknowledgement window, ensuring neither mechanism loses the latest frame.
#[cfg(feature = "render")]
pub(crate) async fn pump_screencast_frames(ctx: &mut CdpContext) {
    let sessions: Vec<String> = ctx.screencasts.keys().cloned().collect();
    for cdp_session_id in sessions {
        if !ctx.screencasts.contains_key(&cdp_session_id) {
            continue;
        }
        let attached_session = Some(cdp_session_id.clone());
        let (activity_generation, css_animation_active) = {
            let Some(page) = ctx.get_session_page_mut(&attached_session) else {
                continue;
            };
            let Some(js) = page.js.as_ref() else {
                continue;
            };
            let generation = js.activity_generation();
            let css_animation_active = page.prepared_has_active_css_animations();
            (generation, css_animation_active)
        };

        let should_attempt = {
            let Some(state) = ctx.screencasts.get_mut(&cdp_session_id) else {
                continue;
            };
            if state.observed_activity_generation != activity_generation || css_animation_active {
                state.observed_activity_generation = activity_generation;
                state.autonomous_frame_pending = true;
            }
            state.autonomous_frame_pending
        };
        if !should_attempt {
            continue;
        }
        if let Err(error) = queue_screencast_frame(ctx, &attached_session, false) {
            tracing::warn!(cdp_session_id, "could not produce autonomous screencast frame: {error}");
        }
    }
}

#[cfg(feature = "render")]
pub(crate) fn command_can_change_screencast_frame(method: &str) -> bool {
    matches!(
        method,
        "Page.navigate"
            | "Page.reload"
            | "Page.navigateToHistoryEntry"
            | "Runtime.evaluate"
            | "Runtime.callFunctionOn"
            | "Input.dispatchMouseEvent"
            | "Input.dispatchKeyEvent"
            | "Input.dispatchTouchEvent"
            | "Emulation.setDeviceMetricsOverride"
            | "Emulation.clearDeviceMetricsOverride"
            | "Emulation.setDefaultBackgroundColorOverride"
            | "DOM.setAttributeValue"
            | "DOM.removeNode"
            | "DOM.focus"
            | "DOM.setFileInputFiles"
    )
}

/// Emit the post-navigation event stream into `ctx.pending_events`. Shared
/// by both the in-process `do_navigate` path and the spawned path in
/// `server::process_navigation`, so the recent goto-returns-Response /
/// per-isolated-world fixes don't have to be duplicated.
pub fn emit_navigation_events(
    ctx: &mut CdpContext,
    session_id: &Option<String>,
    frame_id: &str,
    loader_id: &str,
    page_url: &str,
    page_id: &str,
    network_events: &[obscura_browser::NetworkEvent],
    wait_until: WaitUntil,
    reached_network_idle: bool,
) {
    ctx.current_loader_ids
        .insert(page_id.to_string(), loader_id.to_string());
    let es = session_id.clone();
    let ts = timestamp();

    // Real Chrome uses the navigation's loaderId as the main document's
    // request id, and Puppeteer/Playwright identify the navigation response
    // via `requestId === loaderId && type === "Document"` (issue #189).
    let nav_request_ids: Vec<String> = {
        let mut nav_seen = false;
        network_events
            .iter()
            .map(|ev| {
                if !nav_seen && ev.resource_type == "Document" && ev.url == page_url {
                    nav_seen = true;
                    loader_id.to_string()
                } else {
                    ev.request_id.clone()
                }
            })
            .collect()
    };
    let nav_idx: Option<usize> = network_events
        .iter()
        .position(|ev| ev.resource_type == "Document" && ev.url == page_url);

    // The main resource's body is stored under its internal request id, but the
    // client sees it as `loader_id` (the requestId we report above). Alias it so
    // Network.getResponseBody(loaderId) resolves, which is the only way a client
    // navigating straight to an image/PDF/other resource can read the main body
    // (issue #340). Also read the real Content-Type so frameNavigated reports the
    // actual mime instead of a hardcoded text/html.
    let mut nav_mime = "text/html".to_string();
    if let Some(idx) = nav_idx {
        let internal_id = &network_events[idx].request_id;
        if let Some(ct) = network_events[idx].response_headers.get("content-type") {
            // Strip any `; charset=...` parameter; frameNavigated wants the essence.
            nav_mime = ct.split(';').next().unwrap_or(ct).trim().to_string();
        }
        if internal_id != loader_id {
            if let Some(page) = ctx.get_page_mut(page_id) {
                page.alias_response_body(internal_id, loader_id);
            }
        }
    }

    // Playwright needs `Network.requestWillBeSent` for the main document to
    // arrive BEFORE `Page.frameNavigated` (issue #190).
    if let Some(idx) = nav_idx {
        let net_event = &network_events[idx];
        let rid = &nav_request_ids[idx];
        ctx.pending_events.push(CdpEvent {
            method: "Network.requestWillBeSent".into(),
            params: json!({"requestId": rid, "loaderId": loader_id, "documentURL": page_url, "request": {"url": net_event.url, "method": net_event.method, "headers": net_event.headers}, "timestamp": net_event.timestamp, "wallTime": net_event.timestamp, "initiator": {"type": "other"}, "type": net_event.resource_type, "frameId": frame_id}),
            session_id: es.clone(),
        });
    }

    // executionContextsCleared invalidates every prior context id, so a
    // Runtime.evaluate / callFunctionOn targeting a pre-navigation context
    // must be rejected (Chrome: "Cannot find context with specified id"). The
    // default world (id 2) and isolated worlds are re-registered below as their
    // executionContextCreated events are emitted. Issue #407: previously this
    // set was insert-only, so stale ids kept validating and grew unbounded.
    ctx.valid_context_ids.clear();
    let mut phase1 = vec![
        CdpEvent {
            method: "Page.lifecycleEvent".into(),
            params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "init", "timestamp": ts}),
            session_id: es.clone(),
        },
        CdpEvent {
            method: "Runtime.executionContextsCleared".into(),
            params: json!({}),
            session_id: es.clone(),
        },
        CdpEvent {
            method: "Page.frameNavigated".into(),
            params: json!({"frame": {"id": frame_id, "loaderId": loader_id, "url": page_url, "domainAndRegistry": "", "securityOrigin": page_url, "mimeType": nav_mime, "adFrameStatus": {"adFrameType": "none"}}, "type": "Navigation"}),
            session_id: es.clone(),
        },
        CdpEvent {
            method: "Runtime.executionContextCreated".into(),
            params: json!({"context": {"id": 2, "origin": page_url, "name": "", "uniqueId": format!("ctx-nav-{}", page_id), "auxData": {"isDefault": true, "type": "default", "frameId": frame_id}}}),
            session_id: es.clone(),
        },
    ];
    // The default world is re-created as context id 2; re-register it. Isolated
    // worlds register themselves via next_isolated_context in the loop below.
    ctx.valid_context_ids.insert(2);
    let world_names: Vec<String> = if ctx.isolated_worlds.is_empty() {
        vec!["__puppeteer_utility_world__24.40.0".to_string()]
    } else {
        ctx.isolated_worlds.clone()
    };
    // Issue #192: fresh, monotonically increasing executionContextId per re-create.
    for world_name in &world_names {
        let world_ctx_id = ctx.next_isolated_context();
        phase1.push(CdpEvent {
            method: "Runtime.executionContextCreated".into(),
            params: json!({"context": {"id": world_ctx_id, "origin": page_url, "name": world_name, "uniqueId": format!("ctx-isolated-nav-{}-{}", page_id, world_ctx_id), "auxData": {"isDefault": false, "type": "isolated", "frameId": frame_id}}}),
            session_id: es.clone(),
        });
    }
    phase1.push(CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "commit", "timestamp": ts}), session_id: es.clone() });
    ctx.pending_events.extend(phase1);

    if ctx.fetch_intercept.enabled {
        for (i, net_event) in network_events.iter().enumerate() {
            let rid = &nav_request_ids[i];
            ctx.pending_events.push(CdpEvent {
                method: "Fetch.requestPaused".into(),
                params: json!({
                    "requestId": rid,
                    "request": {
                        "url": net_event.url,
                        "method": net_event.method,
                        "headers": net_event.headers,
                    },
                    "frameId": frame_id,
                    "resourceType": net_event.resource_type,
                    "networkId": rid,
                }),
                session_id: es.clone(),
            });
        }
    }

    for (i, net_event) in network_events.iter().enumerate() {
        let rid = &nav_request_ids[i];
        if Some(i) != nav_idx {
            ctx.pending_events.push(CdpEvent {
                method: "Network.requestWillBeSent".into(),
                params: json!({"requestId": rid, "loaderId": loader_id, "documentURL": page_url, "request": {"url": net_event.url, "method": net_event.method, "headers": net_event.headers}, "timestamp": net_event.timestamp, "wallTime": net_event.timestamp, "initiator": {"type": "other"}, "type": net_event.resource_type, "frameId": frame_id}),
                session_id: es.clone(),
            });
        }
        ctx.pending_events.push(CdpEvent {
            method: "Network.responseReceived".into(),
            params: json!({"requestId": rid, "loaderId": loader_id, "timestamp": net_event.timestamp, "type": net_event.resource_type, "response": {"url": net_event.url, "status": net_event.status, "statusText": "", "headers": &*net_event.response_headers, "mimeType": net_event.response_headers.get("content-type").cloned().unwrap_or_default()}, "frameId": frame_id}),
            session_id: es.clone(),
        });
        ctx.pending_events.push(CdpEvent {
            method: "Network.loadingFinished".into(),
            params: json!({"requestId": rid, "timestamp": net_event.timestamp, "encodedDataLength": net_event.body_size}),
            session_id: es.clone(),
        });
    }

    let mut phase3 = vec![
        CdpEvent {
            method: "Page.lifecycleEvent".into(),
            params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "DOMContentLoaded", "timestamp": ts}),
            session_id: es.clone(),
        },
        CdpEvent {
            method: "Page.domContentEventFired".into(),
            params: json!({"timestamp": ts}),
            session_id: es.clone(),
        },
        CdpEvent {
            method: "Page.lifecycleEvent".into(),
            params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "load", "timestamp": ts}),
            session_id: es.clone(),
        },
        CdpEvent {
            method: "Page.loadEventFired".into(),
            params: json!({"timestamp": ts}),
            session_id: es.clone(),
        },
    ];
    if reached_network_idle || matches!(wait_until, WaitUntil::Load | WaitUntil::DomContentLoaded) {
        let idle_ts = timestamp();
        phase3.push(CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "networkIdle", "timestamp": idle_ts}), session_id: es.clone() });
    }
    phase3.push(CdpEvent {
        method: "Page.frameStoppedLoading".into(),
        params: json!({"frameId": frame_id}),
        session_id: es,
    });
    ctx.pending_events.extend(phase3);

    // Target.targetInfoChanged: strict CDP clients (browser-use, and
    // Puppeteer/Playwright `page.url()` tracking) cache the TargetInfo from
    // attachedToTarget and only refresh it on this event. Without it they keep
    // reporting the pre-navigation url/title (about:blank) and never see the
    // loaded page. Emit it browser-level (no sessionId) with the new url/title.
    let (tic_title, tic_ctx) = ctx
        .get_page(page_id)
        .map(|p| (p.title.clone(), p.context.id.clone()))
        .unwrap_or_default();
    ctx.pending_events.push(CdpEvent::new(
        "Target.targetInfoChanged",
        json!({
            "targetInfo": {
                "targetId": page_id,
                "type": "page",
                "title": tic_title,
                "url": page_url,
                "attached": true,
                "canAccessOpener": false,
                "browserContextId": tic_ctx,
            }
        }),
    ));
}

/// Emit completed script-initiated requests after the document lifecycle has
/// already finished. These requests belong to the current document loader and
/// must not replay frame navigation or load lifecycle events.
pub(crate) fn emit_runtime_network_events(
    ctx: &mut CdpContext,
    session_id: &Option<String>,
    frame_id: &str,
    page_url: &str,
    page_id: &str,
    network_events: &[obscura_browser::NetworkEvent],
) {
    if network_events.is_empty() {
        return;
    }
    let loader_id = ctx
        .current_loader_ids
        .get(page_id)
        .cloned()
        .unwrap_or_else(|| format!("loader-blank-{page_id}"));
    for network_event in network_events {
        let request_id = &network_event.request_id;
        ctx.pending_events.push(CdpEvent {
            method: "Network.requestWillBeSent".into(),
            params: json!({
                "requestId": request_id,
                "loaderId": loader_id,
                "documentURL": page_url,
                "request": {
                    "url": network_event.url,
                    "method": network_event.method,
                    "headers": network_event.headers,
                },
                "timestamp": network_event.timestamp,
                "wallTime": network_event.timestamp,
                "initiator": {"type": "script"},
                "type": network_event.resource_type,
                "frameId": frame_id,
            }),
            session_id: session_id.clone(),
        });
        ctx.pending_events.push(CdpEvent {
            method: "Network.responseReceived".into(),
            params: json!({
                "requestId": request_id,
                "loaderId": loader_id,
                "timestamp": network_event.timestamp,
                "type": network_event.resource_type,
                "response": {
                    "url": network_event.url,
                    "status": network_event.status,
                    "statusText": "",
                    "headers": &*network_event.response_headers,
                    "mimeType": network_event.response_headers
                        .get("content-type")
                        .cloned()
                        .unwrap_or_default(),
                },
                "frameId": frame_id,
            }),
            session_id: session_id.clone(),
        });
        ctx.pending_events.push(CdpEvent {
            method: "Network.loadingFinished".into(),
            params: json!({
                "requestId": request_id,
                "timestamp": network_event.timestamp,
                "encodedDataLength": network_event.body_size,
            }),
            session_id: session_id.clone(),
        });
    }
}

/// Parse the `waitUntil` argument that Puppeteer/Playwright pass on
/// `Page.navigate`.
pub fn parse_wait_until(params: &Value) -> WaitUntil {
    params
        .get("waitUntil")
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(WaitUntil::from_str(s))
            } else if let Some(arr) = v.as_array() {
                arr.iter()
                    .filter_map(|item| item.as_str())
                    .map(WaitUntil::from_str)
                    .max_by_key(|w| match w {
                        WaitUntil::DomContentLoaded => 0,
                        WaitUntil::Load => 1,
                        WaitUntil::NetworkIdle2 => 2,
                        WaitUntil::NetworkIdle0 => 3,
                    })
            } else {
                None
            }
        })
        // Puppeteer and Playwright drive navigation via `Page.navigate`
        // without a server-side waitUntil — they wait for `Page.lifecycleEvent`
        // on the client side. Defaulting the server to `Load` means we run
        // every parser/deferred/async script on JS-heavy pages before
        // emitting `load`, which on sites like github.com / reddit.com
        // pushes nav past 25s and clients time out at 15s. Real Chrome
        // streams `DOMContentLoaded` as soon as the parser is done; we
        // batch our event emission at the end of navigation, so the
        // closest we can get is to default to `DomContentLoaded` and skip
        // the full-load wait. CLI callers that pass `--wait-until load`
        // (or `networkidle*`) are unaffected; they get the old behaviour.
        .unwrap_or(WaitUntil::DomContentLoaded)
}

async fn do_navigate(
    url: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    let wait_until = parse_wait_until(params);

    // Block CDP-initiated file:// navigation by default.
    // Anyone who can reach the CDP port (default localhost,
    // but Docker images bind 0.0.0.0) could otherwise read
    // any file the obscura process can read. Opt in via
    // `obscura serve --allow-file-access` when local-HTML
    // testing is the intended workflow.
    let allow_file_access = ctx
        .get_session_page(session_id)
        .map(|page| page.context.allow_file_access)
        .unwrap_or(ctx.default_context.allow_file_access);
    if url_is_file_scheme(url) && !allow_file_access {
        return Err(
            "Page.navigate to file:// is disabled. Restart with `obscura serve --allow-file-access` to enable.".to_string()
        );
    }

    let preload_scripts: Vec<String> = ctx.preload_scripts.iter().map(|(_, s)| s.clone()).collect();

    let (frame_id, loader_id, network_events, page_url, page_id, reached_network_idle) = {
        let page = ctx
            .get_session_page_mut(session_id)
            .ok_or("No page for session")?;
        let frame_id = page.frame_id.clone();
        let loader_id = format!("loader-{}", uuid::Uuid::new_v4());

        // Preloads (addBinding shims, addScriptToEvaluateOnNewDocument sources)
        // must run BEFORE the page's own scripts (CDP contract). Hand them to
        // the page so navigate_single can inject them at the right point.
        page.set_preload_scripts(preload_scripts);

        let nav_method = params
            .get("__method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET");
        let nav_body = params.get("__body").and_then(|v| v.as_str()).unwrap_or("");
        if nav_method == "POST" && !nav_body.is_empty() {
            page.navigate_with_wait_post(url, wait_until, nav_method, nav_body)
                .await
                .map_err(|e| e.to_string())?;
        } else {
            page.navigate_with_wait(url, wait_until)
                .await
                .map_err(|e| e.to_string())?;
        }

        let reached_network_idle = page.lifecycle.is_network_idle();
        // Fold in script-initiated requests (fetch/XHR/dynamic resource) so they
        // emit as Network events alongside static subresources (#406).
        page.sync_js_network_events();
        let network_events: Vec<_> = page.network_events.drain(..).collect();
        let page_url = page.url_string();
        let page_id = page.id.clone();
        (
            frame_id,
            loader_id,
            network_events,
            page_url,
            page_id,
            reached_network_idle,
        )
    };

    emit_navigation_events(
        ctx,
        session_id,
        &frame_id,
        &loader_id,
        &page_url,
        &page_id,
        &network_events,
        wait_until,
        reached_network_idle,
    );

    Ok(json!({
        "frameId": frame_id,
        "loaderId": loader_id,
    }))
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => Ok(json!({})),
        "navigate" => {
            let url = params
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or("url required")?;
            do_navigate(url, params, ctx, session_id).await
        }
        "reload" => {
            let current_url = ctx
                .get_session_page(session_id)
                .map(|p| p.url_string())
                .unwrap_or_else(|| "about:blank".to_string());
            let reload_params = json!({
                "waitUntil": params.get("waitUntil").cloned().unwrap_or(json!("load"))
            });
            do_navigate(&current_url, &reload_params, ctx, session_id).await
        }
        "getFrameTree" => {
            let page = ctx
                .get_session_page(session_id)
                .ok_or("No page for session")?;
            Ok(json!({ "frameTree": frame_tree(page) }))
        }
        "createIsolatedWorld" => {
            let (frame_id_param, world_name, page_url, page_id) = {
                let page = ctx
                    .get_session_page(session_id)
                    .ok_or("No page for session")?;
                (
                    params
                        .get("frameId")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&page.frame_id)
                        .to_string(),
                    params
                        .get("worldName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    page.url_string(),
                    page.id.clone(),
                )
            };
            // Track this world so Page.navigate can re-emit a context for it
            // post-navigation. Without this, Playwright (and Puppeteer)
            // hang in any operation that uses the utility world — including
            // page.title() — because their utility world is gone after
            // Runtime.executionContextsCleared and never re-created.
            if !world_name.is_empty() && !ctx.isolated_worlds.contains(&world_name) {
                ctx.isolated_worlds.push(world_name.clone());
            }
            // Issue #192: every isolated world emission gets a fresh id from
            // the monotonic counter and is registered as a valid contextId.
            // Reusing id 100 across navigations made Playwright's bookkeeping
            // diverge (it expected 101 on the second nav) and Runtime.evaluate
            // failed with "Cannot find context with specified id: 101".
            let context_id = ctx.next_isolated_context();

            ctx.pending_events.push(CdpEvent {
                method: "Runtime.executionContextCreated".to_string(),
                params: json!({
                    "context": {
                        "id": context_id,
                        "origin": page_url,
                        "name": world_name,
                        "uniqueId": format!("ctx-isolated-{}-{}", page_id, context_id),
                        "auxData": {
                            "isDefault": false,
                            "type": "isolated",
                            "frameId": frame_id_param,
                        }
                    }
                }),
                session_id: session_id.clone(),
            });

            Ok(json!({ "executionContextId": context_id }))
        }
        "setLifecycleEventsEnabled" => Ok(json!({})),
        "addScriptToEvaluateOnNewDocument" => {
            let source = params.get("source").and_then(|v| v.as_str()).unwrap_or("");
            ctx.preload_counter += 1;
            let identifier = format!("{}", ctx.preload_counter);
            if !source.is_empty() {
                ctx.preload_scripts
                    .push((identifier.clone(), source.to_string()));
            }
            Ok(json!({ "identifier": identifier }))
        }
        "removeScriptToEvaluateOnNewDocument" => {
            let identifier = params
                .get("identifier")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            ctx.preload_scripts.retain(|(id, _)| id != identifier);
            Ok(json!({}))
        }
        "setInterceptFileChooserDialog" => Ok(json!({})),
        // Obscura does not download files to disk, so there is no behavior to
        // configure; ack it so clients that set it do not warn (issue #340).
        "setDownloadBehavior" => Ok(json!({})),
        "getLayoutMetrics" => {
            // Playwright calls this before every page.screenshot(). Report the
            // same live CSS viewport that responsive page code and paint use.
            let (width, height) = ctx
                .get_session_page(session_id)
                .map(|page| (page.viewport.0 as f64, page.viewport.1 as f64))
                .unwrap_or((1280.0, 720.0));
            let mut page_x = 0.0;
            let mut page_y = 0.0;
            let mut content_width = width;
            let mut content_height = height;
            if let Some(values) = ctx
                .get_session_page_mut(session_id)
                .map(|page| {
                    page.evaluate(
                        "[window.scrollX, window.scrollY, \
                         document.documentElement && document.documentElement.scrollWidth, \
                         document.documentElement && document.documentElement.scrollHeight]",
                    )
                })
                .and_then(|value| value.as_array().cloned())
            {
                page_x = values.first().and_then(Value::as_f64).unwrap_or(0.0);
                page_y = values.get(1).and_then(Value::as_f64).unwrap_or(0.0);
                content_width = values
                    .get(2)
                    .and_then(Value::as_f64)
                    .filter(|value| *value > 0.0)
                    .unwrap_or(width);
                content_height = values
                    .get(3)
                    .and_then(Value::as_f64)
                    .filter(|value| *value > 0.0)
                    .unwrap_or(height);
            }
            let layout_viewport = json!({
                "pageX": page_x, "pageY": page_y,
                "clientWidth": width, "clientHeight": height,
            });
            let visual_viewport = json!({
                "offsetX": 0.0, "offsetY": 0.0,
                "pageX": page_x, "pageY": page_y,
                "clientWidth": width, "clientHeight": height,
                "scale": 1.0, "zoom": 1.0,
            });
            let content_size = json!({
                "x": 0.0, "y": 0.0,
                "width": content_width, "height": content_height,
            });
            Ok(json!({
                "layoutViewport": layout_viewport,
                "visualViewport": visual_viewport,
                "contentSize": content_size,
                "cssLayoutViewport": layout_viewport,
                "cssVisualViewport": visual_viewport,
                "cssContentSize": content_size,
            }))
        }
        "getNavigationHistory" => {
            let page = ctx
                .get_session_page(session_id)
                .ok_or("No page for session")?;
            // Synthesize an entry for the current page when history is empty
            // (initial about:blank, never-navigated targets). Puppeteer's
            // goBack reads `currentIndex` and `entries[currentIndex-1]`;
            // an empty entries[] used to make every back/forward fail.
            let entries: Vec<Value> = if page.history.is_empty() {
                vec![json!({
                    "id": 0,
                    "url": page.url_string(),
                    "userTypedURL": page.url_string(),
                    "title": page.title,
                    "transitionType": "typed",
                })]
            } else {
                page.history.iter().enumerate().map(|(i, url)| json!({
                    "id": i as u64,
                    "url": url,
                    "userTypedURL": url,
                    "title": if i == page.history_index { page.title.clone() } else { String::new() },
                    "transitionType": "typed",
                })).collect()
            };
            Ok(json!({
                "currentIndex": if page.history.is_empty() { 0 } else { page.history_index },
                "entries": entries,
            }))
        }
        "navigateToHistoryEntry" => {
            let entry_id = params.get("entryId").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let target_url = {
                let page = ctx
                    .get_session_page_mut(session_id)
                    .ok_or("No page for session")?;
                let url = page.history.get(entry_id).cloned();
                if url.is_some() {
                    page.set_history_index(entry_id);
                }
                url
            };
            if let Some(url) = target_url {
                // Stash + restore history so push_history doesn't clobber
                // the cursor we just moved.
                let stash = {
                    let page = ctx
                        .get_session_page_mut(session_id)
                        .ok_or("No page for session")?;
                    (page.history.clone(), page.history_index)
                };
                let (frame_id, page_id, network_events, page_url, reached_idle) = {
                    let page = ctx
                        .get_session_page_mut(session_id)
                        .ok_or("No page for session")?;
                    page.navigate_with_wait(&url, WaitUntil::DomContentLoaded)
                        .await
                        .map_err(|e| e.to_string())?;
                    page.history = stash.0;
                    page.history_index = stash.1;
                    (
                        page.frame_id.clone(),
                        page.id.clone(),
                        page.network_events.drain(..).collect::<Vec<_>>(),
                        page.url_string(),
                        page.lifecycle.is_network_idle(),
                    )
                };
                let loader_id = format!("loader-{}", uuid::Uuid::new_v4());
                emit_navigation_events(
                    ctx,
                    session_id,
                    &frame_id,
                    &loader_id,
                    &page_url,
                    &page_id,
                    &network_events,
                    WaitUntil::DomContentLoaded,
                    reached_idle,
                );
            }
            Ok(json!({}))
        }
        "resetNavigationHistory" => {
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.history.clear();
                page.history_index = 0;
            }
            Ok(json!({}))
        }
        "printToPDF" => crate::domains::pdf::print_to_pdf(params, ctx, session_id).await,
        "startScreencast" => {
            #[cfg(feature = "render")]
            {
                let cdp_session = session_id
                    .as_ref()
                    .ok_or("Page.startScreencast requires an attached target session")?
                    .clone();
                let page = ctx
                    .get_session_page_mut(session_id)
                    .ok_or("No page for session")?;
                prepare_capture_resources_if_requested(page).await;
                let stream_id = ctx.next_screencast_session();
                let state = parse_screencast_state(params, stream_id)?;
                ctx.screencasts.insert(cdp_session.clone(), state);
                let pending_before = ctx.pending_events.len();
                ctx.pending_events.push(CdpEvent {
                    method: "Page.screencastVisibilityChanged".into(),
                    params: json!({"visible": true}),
                    session_id: session_id.clone(),
                });
                if let Err(error) = queue_screencast_frame(ctx, session_id, true) {
                    ctx.pending_events.truncate(pending_before);
                    ctx.screencasts.remove(&cdp_session);
                    return Err(error);
                }
                tracing::debug!(cdp_session, stream_id, "started activity-driven screencast");
                Ok(json!({
                    "obscuraFrameSource": "activity-driven",
                    "obscuraAutonomousFrames": true,
                }))
            }
            #[cfg(not(feature = "render"))]
            Err("Page.startScreencast requires a build with the render feature".into())
        }
        "stopScreencast" => {
            #[cfg(feature = "render")]
            {
                if let Some(cdp_session) = session_id.as_ref() {
                    ctx.screencasts.remove(cdp_session);
                }
                Ok(json!({}))
            }
            #[cfg(not(feature = "render"))]
            Err("Page.stopScreencast requires a build with the render feature".into())
        }
        "screencastFrameAck" => {
            #[cfg(feature = "render")]
            {
                let acknowledged = screencast_int32(params, "sessionId")?
                    .ok_or("Invalid parameters: sessionId is required")?;
                if let Some(cdp_session) = session_id.as_ref() {
                    if let Some(state) = ctx.screencasts.get_mut(cdp_session) {
                        // Ignore delayed acknowledgements from a replaced stream.
                        if state.session_id == acknowledged {
                            state.frames_in_flight = state.frames_in_flight.saturating_sub(1);
                        }
                    }
                }
                Ok(json!({}))
            }
            #[cfg(not(feature = "render"))]
            Err("Page.screencastFrameAck requires a build with the render feature".into())
        }
        "captureScreenshot" => {
            #[cfg(feature = "render")]
            {
                let options = parse_screenshot_options(params)?;
                if !options.from_surface {
                    return Err(
                        "Page.captureScreenshot fromSurface=false is not supported: Obscura has no separate browser-window compositor surface"
                            .to_string(),
                    );
                }
                if options.format == ScreenshotFormat::Webp && options.quality_supplied {
                    return Err(
                        "WebP screenshot quality is not supported by the current lossless encoder"
                            .to_string(),
                    );
                }

                let page = ctx
                    .get_session_page_mut(session_id)
                    .ok_or("No page for session")?;
                prepare_capture_resources_if_requested(page).await;
                let animation_sample = page.live_animation_sample();
                let viewport = page.viewport;
                let device_scale_factor = f64::from(page.device_scale_factor);
                let trusted_scroll = page.screenshot_scroll_offset();
                let scroll = (f64::from(trusted_scroll.0), f64::from(trusted_scroll.1));
                let full_page_size = if options.capture_beyond_viewport && options.clip.is_none() {
                    let content_size = page
                        .prepared_content_size_with_animation_sample(animation_sample)
                        .ok_or_else(|| {
                            "Page.captureScreenshot failed: no retained document size".to_string()
                        })?;
                    Some((
                        content_size.0.max(viewport.0),
                        content_size.1.max(viewport.1),
                    ))
                } else {
                    None
                };

                let region = if let Some(clip) = options.clip {
                    Some(chromium_clip_region(clip, device_scale_factor)?)
                } else if let Some(content_size) = full_page_size {
                    Some(obscura_browser::CaptureRegion::new(
                        0.0,
                        0.0,
                        content_size.0,
                        content_size.1,
                        page.device_scale_factor,
                    ))
                } else if page.device_scale_factor != 1.0 {
                    Some(obscura_browser::CaptureRegion::new(
                        scroll.0 as f32,
                        scroll.1 as f32,
                        viewport.0,
                        viewport.1,
                        page.device_scale_factor,
                    ))
                } else {
                    None
                };

                let (png, png_is_final_encoding) = match region {
                    Some(region) => match page
                        .screenshot_region_with_animation_sample(region, animation_sample)
                    {
                        Ok(png) => (png, false),
                        Err(obscura_browser::CaptureError::AllocationLimitExceeded)
                            if options.format == ScreenshotFormat::Png
                                && options.clip.is_none()
                                && options.capture_beyond_viewport =>
                        {
                            (
                                encode_long_full_page_png(
                                    page,
                                    full_page_size
                                        .expect("full-page route has retained dimensions"),
                                    page.device_scale_factor,
                                    animation_sample,
                                    options.optimize_for_speed,
                                )?,
                                true,
                            )
                        }
                        Err(error) => return Err(capture_error_message(error)),
                    },
                    None => {
                        obscura_browser::validate_capture_region(
                            obscura_browser::CaptureRegion::new(
                                scroll.0 as f32,
                                scroll.1 as f32,
                                viewport.0,
                                viewport.1,
                                1.0,
                            ),
                        )
                        .map_err(capture_error_message)?;
                        (page.screenshot_with_animation_sample(viewport, animation_sample)
                            .ok_or_else(|| {
                                "Page.captureScreenshot failed: the page has no DOM to render"
                                    .to_string()
                            })?, false)
                    }
                };

                // Keep the common path allocation-free and byte-for-byte
                // compatible with the renderer's native PNG encoder.
                let encoded = if options.format == ScreenshotFormat::Png
                    && (!options.optimize_for_speed || png_is_final_encoding)
                {
                    png
                } else {
                    let source = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                        .map_err(|error| {
                            format!("Page.captureScreenshot could not decode renderer PNG: {error}")
                        })?
                        .to_rgba8();
                    encode_screenshot(&source, options)?
                };

                use base64::Engine as _;
                let data = base64::engine::general_purpose::STANDARD.encode(encoded);
                Ok(json!({ "data": data }))
            }
            #[cfg(not(feature = "render"))]
            Err("Page.captureScreenshot requires a build with the render feature".to_string())
        }
        "captureSnapshot" => {
            // A DOM/layer-tree snapshot (not a raster image). Distinct from
            // captureScreenshot; keep the clear error so clients fail fast.
            Err(format!(
                "Page.{method} is not supported by Obscura: no layout or paint engine. \
                 For visual snapshots, drive a real headless Chromium for the \
                 screenshot leg of your pipeline and use Obscura for the scraping leg."
            ))
        }
        _ => Err(format!("Unknown Page method: {}", method)),
    }
}

fn timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::CdpContext;

    #[test]
    fn runtime_network_events_reuse_the_document_loader_without_lifecycle_replay() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some(format!("{page_id}-session"));
        ctx.sessions
            .insert(session_id.clone().unwrap(), page_id.clone());
        ctx.current_loader_ids
            .insert(page_id.clone(), "loader-current".into());
        let event = obscura_browser::NetworkEvent {
            request_id: "fetch-7".into(),
            url: "https://example.test/data.json".into(),
            method: "GET".into(),
            resource_type: "Fetch".into(),
            status: 200,
            headers: std::collections::HashMap::new(),
            response_headers: std::sync::Arc::new(std::collections::HashMap::from([(
                "content-type".into(),
                "application/json".into(),
            )])),
            body_size: 12,
            timestamp: 42.0,
        };

        emit_runtime_network_events(
            &mut ctx,
            &session_id,
            "frame-1",
            "https://example.test/",
            &page_id,
            &[event],
        );

        assert_eq!(ctx.pending_events.len(), 3);
        assert_eq!(ctx.pending_events[0].method, "Network.requestWillBeSent");
        assert_eq!(ctx.pending_events[0].params["loaderId"], "loader-current");
        assert_eq!(ctx.pending_events[1].method, "Network.responseReceived");
        assert_eq!(ctx.pending_events[1].params["loaderId"], "loader-current");
        assert_eq!(ctx.pending_events[2].method, "Network.loadingFinished");
        assert!(ctx.pending_events.iter().all(|event| {
            !matches!(
                event.method.as_str(),
                "Page.frameNavigated" | "Page.lifecycleEvent"
            )
        }));
    }

    #[tokio::test]
    async fn get_layout_metrics_returns_chrome_default_viewport() {
        let mut ctx = CdpContext::new();
        let result = handle("getLayoutMetrics", &json!({}), &mut ctx, &None)
            .await
            .expect("getLayoutMetrics should succeed without a session");

        // CDP spec requires three top-level shapes; Playwright's screenshot
        // path reads contentSize.width/height to size the capture. Without
        // them the screenshot call panics with "cannot read property of
        // undefined".
        for key in [
            "layoutViewport",
            "visualViewport",
            "contentSize",
            "cssLayoutViewport",
            "cssVisualViewport",
            "cssContentSize",
        ] {
            assert!(result.get(key).is_some(), "missing key: {key}");
        }

        let layout = &result["layoutViewport"];
        assert_eq!(layout["clientWidth"].as_f64(), Some(1280.0));
        assert_eq!(layout["clientHeight"].as_f64(), Some(720.0));

        let visual = &result["visualViewport"];
        assert_eq!(visual["scale"].as_f64(), Some(1.0));
        assert_eq!(visual["clientWidth"].as_f64(), Some(1280.0));

        let content = &result["contentSize"];
        assert_eq!(content["width"].as_f64(), Some(1280.0));
        // Without a live page the content height falls back to the viewport.
        assert_eq!(content["height"].as_f64(), Some(720.0));
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn cdp_metrics_and_capture_follow_the_scrolled_viewport() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{page_id}-session");
        ctx.sessions.insert(session_id.clone(), page_id);
        let session = Some(session_id);
        ctx.get_session_page_mut(&session)
            .expect("page")
            .set_viewport((100.0, 80.0));

        handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0'><body style='margin:0'><div style='height:80px;background:red'></div><div style='height:80px;background:blue'></div><div style='position:fixed;left:0;top:0;width:20px;height:20px;background:green'></div></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate");

        let top = handle("captureScreenshot", &json!({}), &mut ctx, &session)
            .await
            .expect("top capture");
        ctx.get_session_page_mut(&session)
            .expect("page")
            .evaluate("return (window.scrollTo(0, 80), window.scrollY)");

        let metrics = handle("getLayoutMetrics", &json!({}), &mut ctx, &session)
            .await
            .expect("metrics");
        assert_eq!(metrics["layoutViewport"]["pageY"].as_f64(), Some(80.0));
        assert_eq!(metrics["visualViewport"]["pageY"].as_f64(), Some(80.0));
        assert_eq!(metrics["contentSize"]["width"].as_f64(), Some(100.0));
        assert!(
            metrics["contentSize"]["height"]
                .as_f64()
                .unwrap_or_default()
                >= 160.0,
            "contentSize must expose the scrollable document: {metrics}"
        );

        let scrolled = handle("captureScreenshot", &json!({}), &mut ctx, &session)
            .await
            .expect("scrolled capture");
        let top_data = top["data"].as_str().expect("top png");
        let scrolled_data = scrolled["data"].as_str().expect("scrolled png");
        assert_ne!(
            top_data, scrolled_data,
            "CDP captureScreenshot must paint the current scroll offset"
        );
    }

    #[cfg(feature = "render")]
    fn decode_capture(result: &Value) -> (image::ImageFormat, image::RgbaImage) {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(result["data"].as_str().expect("screenshot data"))
            .expect("base64 screenshot");
        let format = image::guess_format(&bytes).expect("image format");
        let raster = image::load_from_memory(&bytes)
            .expect("decodable screenshot")
            .to_rgba8();
        (format, raster)
    }

    #[cfg(feature = "render")]
    async fn screenshot_fixture() -> (CdpContext, Option<String>) {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{page_id}-session");
        ctx.sessions.insert(session_id.clone(), page_id);
        let session = Some(session_id);
        ctx.get_session_page_mut(&session)
            .expect("page")
            .set_viewport((100.0, 80.0));
        handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0'><body style='margin:0;height:160px;background:red'><div style='position:absolute;left:50px;top:0;width:50px;height:80px;background:blue'></div><div style='position:absolute;left:0;top:80px;width:100px;height:80px;background:green'></div></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate screenshot fixture");
        (ctx, session)
    }

    #[cfg(feature = "render")]
    async fn transparent_surface_fixture(
        height: u32,
    ) -> (CdpContext, Option<String>) {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{page_id}-session");
        ctx.sessions.insert(session_id.clone(), page_id);
        let session = Some(session_id);
        ctx.get_session_page_mut(&session)
            .expect("page")
            .set_viewport((100.0, 80.0));
        handle(
            "navigate",
            &json!({
                "url": format!("data:text/html,<html style='margin:0;background:transparent'><body style='margin:0;height:{height}px;background:transparent'></body></html>"),
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate transparent surface fixture");
        (ctx, session)
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn default_background_override_matches_chromium_across_capture_state() {
        let (mut ctx, session) = transparent_surface_fixture(160).await;

        let (_, default_raster) = decode_capture(
            &handle("captureScreenshot", &json!({}), &mut ctx, &session)
                .await
                .expect("default white capture"),
        );
        assert_eq!(default_raster.get_pixel(50, 40).0, [255, 255, 255, 255]);

        crate::domains::emulation::handle(
            "setDefaultBackgroundColorOverride",
            &json!({"color": {"r": 0, "g": 0, "b": 255, "a": 1}}),
            &mut ctx,
            &session,
        )
        .await
        .expect("opaque blue override");
        let (_, blue) = decode_capture(
            &handle("captureScreenshot", &json!({}), &mut ctx, &session)
                .await
                .expect("blue capture"),
        );
        assert!(blue.pixels().all(|pixel| pixel.0 == [0, 0, 255, 255]));

        crate::domains::emulation::handle(
            "setDefaultBackgroundColorOverride",
            &json!({"color": {"r": 0, "g": 0, "b": 0, "a": 0}}),
            &mut ctx,
            &session,
        )
        .await
        .expect("transparent override");
        let (_, transparent) = decode_capture(
            &handle("captureScreenshot", &json!({}), &mut ctx, &session)
                .await
                .expect("transparent capture"),
        );
        assert!(transparent.pixels().all(|pixel| pixel.0 == [0, 0, 0, 0]));

        crate::domains::emulation::handle(
            "setDefaultBackgroundColorOverride",
            &json!({"color": {"r": 255, "g": 0, "b": 0, "a": 16.0 / 255.0}}),
            &mut ctx,
            &session,
        )
        .await
        .expect("semi-transparent override");
        crate::domains::emulation::handle(
            "setDeviceMetricsOverride",
            &json!({"width": 40, "height": 30, "deviceScaleFactor": 2, "mobile": false}),
            &mut ctx,
            &session,
        )
        .await
        .expect("metrics override");
        let (_, semi) = decode_capture(
            &handle("captureScreenshot", &json!({}), &mut ctx, &session)
                .await
                .expect("semi-transparent metrics capture"),
        );
        assert_eq!(semi.dimensions(), (80, 60));
        assert!(semi.pixels().all(|pixel| pixel.0 == [255, 0, 0, 16]));

        handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0;background:transparent'><body style='margin:0;background:transparent'></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate with live override");
        let (_, after_navigation) = decode_capture(
            &handle("captureScreenshot", &json!({}), &mut ctx, &session)
                .await
                .expect("capture after navigation"),
        );
        assert_eq!(after_navigation.get_pixel(20, 15).0, [255, 0, 0, 16]);

        crate::domains::emulation::handle(
            "setDefaultBackgroundColorOverride",
            &json!({}),
            &mut ctx,
            &session,
        )
        .await
        .expect("clear override");
        let (_, cleared) = decode_capture(
            &handle("captureScreenshot", &json!({}), &mut ctx, &session)
                .await
                .expect("capture after clear"),
        );
        assert_eq!(cleared.get_pixel(20, 15).0, [255, 255, 255, 255]);
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn default_background_override_is_target_isolated() {
        let (mut ctx, first_session) = transparent_surface_fixture(80).await;
        let second_page_id = ctx.create_page();
        let second_session_id = format!("{second_page_id}-session");
        ctx.sessions
            .insert(second_session_id.clone(), second_page_id);
        let second_session = Some(second_session_id);
        ctx.get_session_page_mut(&second_session)
            .expect("second page")
            .set_viewport((100.0, 80.0));
        handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0;background:transparent'><body style='margin:0;background:transparent'></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &second_session,
        )
        .await
        .expect("navigate second page");
        crate::domains::emulation::handle(
            "setDefaultBackgroundColorOverride",
            &json!({"color": {"r": 20, "g": 40, "b": 60}}),
            &mut ctx,
            &first_session,
        )
        .await
        .expect("first-page override");

        let (_, first) = decode_capture(
            &handle("captureScreenshot", &json!({}), &mut ctx, &first_session)
                .await
                .expect("first-page capture"),
        );
        let (_, second) = decode_capture(
            &handle("captureScreenshot", &json!({}), &mut ctx, &second_session)
                .await
                .expect("second-page capture"),
        );
        assert_eq!(first.get_pixel(50, 40).0, [20, 40, 60, 255]);
        assert_eq!(second.get_pixel(50, 40).0, [255, 255, 255, 255]);
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn default_background_override_covers_clips_full_page_and_screencast_damage() {
        let (mut ctx, session) = transparent_surface_fixture(160).await;
        crate::domains::emulation::handle(
            "setDefaultBackgroundColorOverride",
            &json!({"color": {"r": 7, "g": 19, "b": 31, "a": 1}}),
            &mut ctx,
            &session,
        )
        .await
        .expect("surface override");

        for clip in [
            json!({"x": 90, "y": 0, "width": 20, "height": 20, "scale": 1}),
            json!({"x": 0, "y": 120, "width": 20, "height": 20, "scale": 1}),
        ] {
            let (_, raster) = decode_capture(
                &handle(
                    "captureScreenshot",
                    &json!({"captureBeyondViewport": false, "clip": clip}),
                    &mut ctx,
                    &session,
                )
                .await
                .expect("off-viewport clip with default false"),
            );
            assert_eq!(raster.dimensions(), (20, 20));
            assert!(raster
                .pixels()
                .all(|pixel| pixel.0 == [7, 19, 31, 255]));
        }

        ctx.get_session_page_mut(&session)
            .expect("page")
            .evaluate("window.scrollTo(0, 80)");
        let (_, above_scroll) = decode_capture(
            &handle(
                "captureScreenshot",
                &json!({
                    "clip": {"x": 0, "y": 0, "width": 20, "height": 20, "scale": 1}
                }),
                &mut ctx,
                &session,
            )
            .await
            .expect("document clip above live scroll"),
        );
        assert_eq!(above_scroll.get_pixel(10, 10).0, [7, 19, 31, 255]);
        assert_eq!(
            ctx.get_session_page(&session)
                .expect("page")
                .screenshot_scroll_offset(),
            (0.0, 80.0),
            "clip capture must not mutate live scroll"
        );

        let (_, full_page) = decode_capture(
            &handle(
                "captureScreenshot",
                &json!({"captureBeyondViewport": true}),
                &mut ctx,
                &session,
            )
            .await
            .expect("full-page override capture"),
        );
        assert_eq!(full_page.dimensions(), (100, 160));
        assert_eq!(full_page.get_pixel(50, 140).0, [7, 19, 31, 255]);

        ctx.pending_events.clear();
        handle("startScreencast", &json!({}), &mut ctx, &session)
            .await
            .expect("start screencast");
        ctx.pending_events.clear();
        let response = crate::dispatch::dispatch(
            &crate::types::CdpRequest {
                id: 99,
                method: "Emulation.setDefaultBackgroundColorOverride".to_string(),
                params: json!({"color": {"r": 90, "g": 80, "b": 70, "a": 1}}),
                session_id: session.clone(),
            },
            &mut ctx,
        )
        .await;
        assert!(response.error.is_none(), "override response: {response:?}");
        let event = ctx
            .pending_events
            .iter()
            .find(|event| event.method == "Page.screencastFrame")
            .expect("background override must damage active screencast");
        let (_, frame) = decode_capture(&event.params);
        assert_eq!(frame.get_pixel(50, 40).0, [90, 80, 70, 255]);
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn screencast_initial_frame_metadata_and_encoding_match_options() {
        let (mut ctx, session) = screenshot_fixture().await;
        ctx.pending_events.clear();
        let result = handle(
            "startScreencast",
            &json!({
                "format": "jpeg", "quality": 35, "maxWidth": 50, "maxHeight": 100,
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("start screencast");
        assert_eq!(result["obscuraFrameSource"], "activity-driven");
        assert_eq!(result["obscuraAutonomousFrames"], true);
        assert_eq!(ctx.pending_events.len(), 2);
        assert_eq!(
            ctx.pending_events[0].method,
            "Page.screencastVisibilityChanged"
        );
        let frame = &ctx.pending_events[1];
        assert_eq!(frame.method, "Page.screencastFrame");
        assert_eq!(frame.session_id, session);
        let (format, raster) = decode_capture(&frame.params);
        assert_eq!(format, image::ImageFormat::Jpeg);
        assert_eq!(raster.dimensions(), (50, 40));
        assert_eq!(frame.params["metadata"]["deviceWidth"], 100.0);
        assert_eq!(frame.params["metadata"]["deviceHeight"], 80.0);
        assert_eq!(frame.params["metadata"]["scrollOffsetY"], 0.0);
        assert!(
            frame.params["metadata"]["timestamp"]
                .as_f64()
                .unwrap_or(0.0)
                > 0.0
        );
        assert_eq!(frame.params["sessionId"], 1);

        // Chromium's PageHandler::DetermineSnapshotSize scales the surface
        // through gfx::ToRoundedSize. Preserve the fractional 40.8px height
        // instead of truncating it to 40px.
        ctx.pending_events.clear();
        handle(
            "startScreencast",
            &json!({"format": "png", "maxWidth": 51}),
            &mut ctx,
            &session,
        )
        .await
        .expect("start fractionally scaled screencast");
        let rounded_frame = ctx
            .pending_events
            .iter()
            .find(|event| event.method == "Page.screencastFrame")
            .expect("fractionally scaled initial frame");
        let (format, raster) = decode_capture(&rounded_frame.params);
        assert_eq!(format, image::ImageFormat::Png);
        assert_eq!(raster.dimensions(), (51, 41));
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn css_animation_drives_autonomous_screencast_frames_until_completion() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{page_id}-session");
        ctx.sessions.insert(session_id.clone(), page_id);
        let session = Some(session_id);
        ctx.get_session_page_mut(&session)
            .expect("page")
            .set_viewport((80.0, 60.0));
        handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0'><head><style>@keyframes hide{from{opacity:1}to{opacity:0}}#cover{width:80px;height:60px;background:red;animation:hide 100ms linear forwards}</style></head><body style='margin:0;background:lime'><div id=cover></div></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate animation fixture");

        ctx.pending_events.clear();
        handle("startScreencast", &json!({}), &mut ctx, &session)
            .await
            .expect("start screencast");
        let initial = ctx
            .pending_events
            .iter()
            .find(|event| event.method == "Page.screencastFrame")
            .expect("initial frame")
            .params["data"]
            .as_str()
            .unwrap()
            .to_string();
        let generation = ctx
            .get_session_page_mut(&session)
            .unwrap()
            .js
            .as_ref()
            .unwrap()
            .activity_generation();

        ctx.pending_events.clear();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        pump_screencast_frames(&mut ctx).await;
        let (animated, frame_session_id) = {
            let event = ctx
                .pending_events
                .iter()
                .find(|event| event.method == "Page.screencastFrame")
                .expect("CSS-only animation frame");
            (
                event.params["data"].as_str().unwrap().to_string(),
                event.params["sessionId"].as_u64().unwrap(),
            )
        };
        assert_ne!(animated, initial);
        assert_eq!(
            ctx.get_session_page_mut(&session)
                .unwrap()
                .js
                .as_ref()
                .unwrap()
                .activity_generation(),
            generation,
            "CSS timeline damage must not depend on DOM or task activity"
        );

        for _ in 0..2 {
            handle(
                "screencastFrameAck",
                &json!({"sessionId": frame_session_id}),
                &mut ctx,
                &session,
            )
            .await
            .expect("ack frame");
        }
        ctx.pending_events.clear();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        pump_screencast_frames(&mut ctx).await;
        assert!(
            ctx.pending_events
                .iter()
                .any(|event| event.method == "Page.screencastFrame"),
            "the stream must emit the finite animation's final frame"
        );
        assert!(!ctx
            .get_session_page_mut(&session)
            .unwrap()
            .prepared_has_active_css_animations());

        handle(
            "screencastFrameAck",
            &json!({"sessionId": frame_session_id}),
            &mut ctx,
            &session,
        )
        .await
        .expect("ack final frame");
        ctx.pending_events.clear();
        pump_screencast_frames(&mut ctx).await;
        assert!(
            ctx.pending_events.is_empty(),
            "a completed finite animation must not keep rasterizing idle frames"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn screencast_sampling_backpressure_and_stale_acks_are_bounded() {
        let (mut ctx, session) = screenshot_fixture().await;
        ctx.pending_events.clear();
        handle(
            "startScreencast",
            &json!({"everyNthFrame": 2}),
            &mut ctx,
            &session,
        )
        .await
        .expect("start sampled stream");
        let old_id = ctx.pending_events[1].params["sessionId"].as_i64().unwrap();
        ctx.pending_events.clear();
        assert!(!queue_screencast_frame(&mut ctx, &session, false).unwrap());
        assert!(queue_screencast_frame(&mut ctx, &session, false).unwrap());
        assert!(
            !queue_screencast_frame(&mut ctx, &session, false).unwrap(),
            "two unacknowledged frames must apply backpressure before capture"
        );
        let key = session.as_ref().unwrap();
        handle(
            "screencastFrameAck",
            &json!({"sessionId": old_id + 99}),
            &mut ctx,
            &session,
        )
        .await
        .expect("stale ack");
        assert_eq!(ctx.screencasts[key].frames_in_flight, 2);
        handle(
            "screencastFrameAck",
            &json!({"sessionId": old_id}),
            &mut ctx,
            &session,
        )
        .await
        .expect("current ack");
        assert_eq!(ctx.screencasts[key].frames_in_flight, 1);

        ctx.pending_events.clear();
        handle("startScreencast", &json!({}), &mut ctx, &session)
            .await
            .expect("restart");
        let new_id = ctx.pending_events[1].params["sessionId"].as_i64().unwrap();
        assert!(new_id > old_id);
        handle(
            "screencastFrameAck",
            &json!({"sessionId": old_id}),
            &mut ctx,
            &session,
        )
        .await
        .expect("old generation ack");
        assert_eq!(ctx.screencasts[key].frames_in_flight, 1);
        handle("stopScreencast", &json!({}), &mut ctx, &session)
            .await
            .expect("stop");
        assert!(!ctx.screencasts.contains_key(key));
        assert!(!queue_screencast_frame(&mut ctx, &session, false).unwrap());
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn capture_methods_do_not_start_default_resource_warmups() {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0u8; 2048];
                        let _ = stream.read(&mut request);
                        observed.fetch_add(1, Ordering::SeqCst);
                        let body = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: image/svg+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.write_all(body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        let context = std::sync::Arc::new(
            obscura_browser::BrowserContext::with_storage_and_network(
                "capture-without-warmup".to_string(),
                None,
                false,
                None,
                None,
                true,
            ),
        );
        let mut ctx = CdpContext::new_with_shared_context(context);
        let page_id = ctx.create_page();
        let session_id = format!("{page_id}-session");
        ctx.sessions.insert(session_id.clone(), page_id);
        let session = Some(session_id);
        ctx.get_session_page_mut(&session)
            .expect("page")
            .set_viewport((100.0, 80.0));
        handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0'><body style='margin:0;height:160px'></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate capture fixture");

        for (index, method) in ["captureScreenshot", "startScreencast", "printToPDF"]
            .into_iter()
            .enumerate()
        {
            let asset = format!("http://{address}/capture-{index}.svg");
            let script = format!(
                "(function(){{const box=document.createElement('div');box.setAttribute('style','width:10px;height:10px;background-image:url('+{}+')');document.body.appendChild(box);}})()",
                serde_json::to_string(&asset).unwrap()
            );
            ctx.get_session_page_mut(&session)
                .expect("page")
                .evaluate(&script);
            handle(method, &json!({}), &mut ctx, &session)
                .await
                .unwrap_or_else(|error| panic!("{method} failed: {error}"));
        }

        std::thread::sleep(std::time::Duration::from_millis(75));
        assert_eq!(
            requests.load(Ordering::SeqCst),
            0,
            "capture APIs must observe retained state without initiating network warmup"
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn screencast_options_validate_protocol_shapes() {
        assert!(parse_screencast_state(&json!({"format": "webp"}), 1).is_err());
        assert!(parse_screencast_state(&json!({"everyNthFrame": 0}), 1).is_err());
        assert!(parse_screencast_state(&json!({"maxWidth": 20.5}), 1).is_err());
        let state =
            parse_screencast_state(&json!({"quality": 101, "maxWidth": 0, "maxHeight": -1}), 1)
                .expect("Chromium-compatible fallbacks");
        assert_eq!(state.quality, DEFAULT_SCREENSHOT_QUALITY as u8);
        assert_eq!(state.max_width, None);
        assert_eq!(state.max_height, None);
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn capture_screenshot_preserves_default_png_and_honors_clip_scale() {
        let (mut ctx, session) = screenshot_fixture().await;
        let native = {
            let page = ctx.get_session_page(&session).expect("page");
            page.screenshot(page.viewport).expect("native png")
        };
        let default = handle("captureScreenshot", &json!({}), &mut ctx, &session)
            .await
            .expect("default screenshot");
        use base64::Engine as _;
        let default_bytes = base64::engine::general_purpose::STANDARD
            .decode(default["data"].as_str().expect("default data"))
            .expect("default base64");
        assert_eq!(
            default_bytes, native,
            "default CDP path must preserve the renderer PNG bytes exactly"
        );

        let clipped = handle(
            "captureScreenshot",
            &json!({
                "captureBeyondViewport": true,
                "clip": {"x": 50.0, "y": 0.0, "width": 50.0, "height": 40.0, "scale": 2.0}
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("clipped screenshot");
        let (format, raster) = decode_capture(&clipped);
        assert_eq!(format, image::ImageFormat::Png);
        assert_eq!(raster.dimensions(), (100, 80));
        let center = raster.get_pixel(50, 40).0;
        assert!(
            center[2] > 200 && center[0] < 50,
            "clip x/y must select the blue half of the live surface: {center:?}"
        );

        // Empirical Chromium result: the fractional CSS size first becomes a
        // 10x9 gfx::Size, then 1.1x output scaling rounds to 11x10 pixels.
        let fractional = handle(
            "captureScreenshot",
            &json!({
                "clip": {"x": 50.0, "y": 0.0, "width": 10.9, "height": 9.9, "scale": 1.1}
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("fractional Chromium clip");
        let (_, raster) = decode_capture(&fractional);
        assert_eq!(raster.dimensions(), (11, 10));

        let off_viewport = handle(
            "captureScreenshot",
            &json!({
                "captureBeyondViewport": true,
                "clip": {"x": 0.0, "y": 100.0, "width": 20.0, "height": 20.0, "scale": 1.0}
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("off-viewport document-space clip");
        let (_, raster) = decode_capture(&off_viewport);
        assert_eq!(raster.dimensions(), (20, 20));
        let center = raster.get_pixel(10, 10).0;
        assert!(
            center[1] > 80 && center[0] < 50 && center[2] < 50,
            "captureBeyondViewport must paint off-viewport document content: {center:?}"
        );

        ctx.get_session_page_mut(&session)
            .expect("page")
            .evaluate("window.scrollTo(0, 80)");
        let page_coordinate_clip = handle(
            "captureScreenshot",
            &json!({
                "clip": {"x": 0.0, "y": 80.0, "width": 20.0, "height": 20.0, "scale": 1.0}
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("clip in page coordinates after scroll");
        let (_, raster) = decode_capture(&page_coordinate_clip);
        let center = raster.get_pixel(10, 10).0;
        assert!(
            center[1] > 80 && center[0] < 50 && center[2] < 50,
            "clip coordinates must remain page-relative after scrolling: {center:?}"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn default_capture_rejects_oversized_viewport_before_raster_allocation() {
        let (mut ctx, session) = screenshot_fixture().await;
        ctx.get_session_page_mut(&session)
            .expect("page")
            .set_viewport((32_768.0, 32_768.0));

        let error = handle("captureScreenshot", &json!({}), &mut ctx, &session)
            .await
            .expect_err("a 4 GiB RGBA surface must be rejected before allocation");
        assert!(error.contains("bitmap is too large"), "{error}");
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn long_full_page_png_is_contiguous_and_preserves_live_scroll_and_fixed_geometry() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{page_id}-session");
        ctx.sessions.insert(session_id.clone(), page_id);
        let session = Some(session_id);
        ctx.get_session_page_mut(&session)
            .expect("page")
            .set_viewport((1000.0, 700.0));
        handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0;background:transparent'><body style='margin:0;width:1000px;height:17000px;background:transparent'><div style='position:absolute;left:0;top:8490px;width:1000px;height:30px;background:rgb(20,160,40)'></div><div style='position:absolute;left:0;top:16950px;width:1000px;height:50px;background:rgb(20,40,200)'></div><div style='position:fixed;z-index:2;left:0;top:10px;width:20px;height:20px;background:rgb(240,220,10)'></div></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate long screenshot fixture");
        crate::domains::emulation::handle(
            "setDefaultBackgroundColorOverride",
            &json!({"color": {"r": 180, "g": 20, "b": 30, "a": 1}}),
            &mut ctx,
            &session,
        )
        .await
        .expect("long-page surface override");
        ctx.get_session_page_mut(&session)
            .expect("page")
            .evaluate("window.scrollTo(0, 8000)");
        assert_eq!(
            ctx.get_session_page(&session)
                .expect("page")
                .screenshot_scroll_offset(),
            (0.0, 8000.0)
        );

        let capture = handle(
            "captureScreenshot",
            &json!({"format": "png", "captureBeyondViewport": true}),
            &mut ctx,
            &session,
        )
        .await
        .expect("striped full-page capture");
        let (format, raster) = decode_capture(&capture);
        assert_eq!(format, image::ImageFormat::Png);
        assert_eq!(raster.dimensions(), (1000, 17_000));
        assert_eq!(raster.get_pixel(500, 100).0, [180, 20, 30, 255]);
        assert_eq!(raster.get_pixel(500, 8500).0, [20, 160, 40, 255]);
        assert_eq!(raster.get_pixel(500, 16_975).0, [20, 40, 200, 255]);
        assert_eq!(
            raster.get_pixel(10, 8015).0,
            [240, 220, 10, 255],
            "fixed content must retain its one live-scroll document position"
        );
        assert_eq!(
            raster.get_pixel(10, 15).0,
            [180, 20, 30, 255],
            "fixed content must not be repeated at the full-page origin"
        );
        for boundary in [4096, 8192, 12_288, 16_384] {
            for y in (boundary - 1)..=(boundary + 1) {
                assert_eq!(
                    raster.get_pixel(900, y).0,
                    [180, 20, 30, 255],
                    "visible seam around globally rounded strip row {boundary}"
                );
            }
        }
        assert_eq!(
            ctx.get_session_page(&session)
                .expect("page")
                .screenshot_scroll_offset(),
            (0.0, 8000.0),
            "full-page capture must not mutate the live scroll position"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn long_full_page_png_uses_global_device_pixel_boundaries_at_dpr_two() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{page_id}-session");
        ctx.sessions.insert(session_id.clone(), page_id);
        let session = Some(session_id);
        ctx.get_session_page_mut(&session)
            .expect("page")
            .set_viewport((1000.0, 500.0));
        handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0'><body style='margin:0;width:1000px;height:4250px;background:rgb(70,40,190)'></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate DPR fixture");
        crate::domains::emulation::handle(
            "setDeviceMetricsOverride",
            &json!({
                "width": 1000,
                "height": 500,
                "deviceScaleFactor": 2,
                "mobile": false
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("set DPR two");

        let capture = handle(
            "captureScreenshot",
            &json!({"captureBeyondViewport": true}),
            &mut ctx,
            &session,
        )
        .await
        .expect("DPR two striped capture");
        let (_, raster) = decode_capture(&capture);
        assert_eq!(raster.dimensions(), (2000, 8500));
        for boundary in [4096, 8192] {
            for y in (boundary - 1)..=(boundary + 1) {
                assert_eq!(
                    raster.get_pixel(1500, y).0,
                    [70, 40, 190, 255],
                    "DPR two seam around global output row {boundary}"
                );
            }
        }
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn long_full_page_png_rejects_more_than_thirty_two_megapixels_before_striping() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{page_id}-session");
        ctx.sessions.insert(session_id.clone(), page_id);
        let session = Some(session_id);
        ctx.get_session_page_mut(&session)
            .expect("page")
            .set_viewport((1000.0, 700.0));
        handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0'><body style='margin:0;width:1000px;height:34000px;background:red'></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate over-cap fixture");

        let error = handle(
            "captureScreenshot",
            &json!({"captureBeyondViewport": true}),
            &mut ctx,
            &session,
        )
        .await
        .expect_err("34 megapixels must fail before striped raster allocation");
        assert!(
            error.contains("33554432-pixel safety limit"),
            "unexpected over-cap error: {error}"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn capture_screenshot_encodes_jpeg_and_lossless_webp() {
        let (mut ctx, session) = screenshot_fixture().await;
        let jpeg = handle(
            "captureScreenshot",
            &json!({"format": "jpeg", "quality": 35}),
            &mut ctx,
            &session,
        )
        .await
        .expect("jpeg screenshot");
        let (format, raster) = decode_capture(&jpeg);
        assert_eq!(format, image::ImageFormat::Jpeg);
        assert_eq!(raster.dimensions(), (100, 80));

        let fast_png = handle(
            "captureScreenshot",
            &json!({"optimizeForSpeed": true}),
            &mut ctx,
            &session,
        )
        .await
        .expect("fast PNG screenshot");
        let (format, raster) = decode_capture(&fast_png);
        assert_eq!(format, image::ImageFormat::Png);
        assert_eq!(raster.dimensions(), (100, 80));

        let webp = handle(
            "captureScreenshot",
            &json!({"format": "webp"}),
            &mut ctx,
            &session,
        )
        .await
        .expect("lossless webp screenshot");
        let (format, raster) = decode_capture(&webp);
        assert_eq!(format, image::ImageFormat::WebP);
        assert_eq!(raster.dimensions(), (100, 80));

        let error = handle(
            "captureScreenshot",
            &json!({"format": "webp", "quality": 35}),
            &mut ctx,
            &session,
        )
        .await
        .expect_err("lossy WebP must not be silently faked");
        assert!(error.contains("lossless encoder"), "{error}");
    }

    #[cfg(feature = "render")]
    #[test]
    fn capture_screenshot_validates_like_chromium() {
        let options = parse_screenshot_options(&json!({
            "format": "jpeg",
            "quality": -1,
        }))
        .expect("out-of-range integer quality falls back");
        assert_eq!(options.quality, 80);
        let options = parse_screenshot_options(&json!({
            "format": "jpeg",
            "quality": 101,
        }))
        .expect("out-of-range integer quality falls back");
        assert_eq!(options.quality, 80);
        assert_eq!(
            parse_screenshot_options(&json!({"format": "gif"})).expect_err("bad format"),
            "Invalid image format"
        );
        assert!(parse_screenshot_options(&json!({"format": 3}))
            .expect_err("format type")
            .contains("format must be a string"));
        assert!(parse_screenshot_options(&json!({"quality": 50.5}))
            .expect_err("quality type")
            .contains("quality must be an integer"));
        assert!(parse_screenshot_options(&json!({"fromSurface": "false"}))
            .expect_err("fromSurface type")
            .contains("fromSurface must be a boolean"));
        assert!(
            parse_screenshot_options(&json!({"captureBeyondViewport": 1}))
                .expect_err("captureBeyondViewport type")
                .contains("captureBeyondViewport must be a boolean")
        );
        assert_eq!(
            parse_screenshot_options(&json!({
                "clip": {"x": 0, "y": 0, "width": 0, "height": 10, "scale": 1}
            }))
            .expect_err("zero width"),
            "Cannot take screenshot with 0 width."
        );
        assert!(parse_screenshot_options(&json!({
            "clip": {"x": 0, "y": 0, "width": 10, "height": 10}
        }))
        .expect_err("missing scale")
        .contains("mandatory clip.scale field missing"));
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn capture_screenshot_supports_full_page_and_off_viewport_clips() {
        let (mut ctx, session) = screenshot_fixture().await;
        ctx.get_session_page_mut(&session).expect("page").evaluate(
            "Object.defineProperty(globalThis,'innerWidth',{value:4096,configurable:true});\
                 Object.defineProperty(globalThis,'innerHeight',{value:4096,configurable:true})",
        );
        let beyond = handle(
            "captureScreenshot",
            &json!({"captureBeyondViewport": true}),
            &mut ctx,
            &session,
        )
        .await
        .expect("full-page document capture");
        let (_, raster) = decode_capture(&beyond);
        assert_eq!(raster.dimensions(), (100, 160));
        let bottom = raster.get_pixel(50, 120).0;
        assert!(
            bottom[1] > 80 && bottom[0] < 50 && bottom[2] < 50,
            "full-page capture must include below-fold content: {bottom:?}"
        );

        let off_surface = handle(
            "captureScreenshot",
            &json!({"fromSurface": false}),
            &mut ctx,
            &session,
        )
        .await
        .expect_err("browser-window compositor is unsupported");
        assert!(off_surface.contains("fromSurface=false"), "{off_surface}");

        let off_viewport = handle(
            "captureScreenshot",
            &json!({
                "clip": {"x": 90, "y": 0, "width": 20, "height": 20, "scale": 1}
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("Chromium accepts a partial off-viewport surface clip");
        let (_, raster) = decode_capture(&off_viewport);
        assert_eq!(raster.dimensions(), (20, 20));
        assert_eq!(raster.get_pixel(5, 10).0, [0, 0, 255, 255]);
        assert_eq!(
            raster.get_pixel(15, 10).0,
            [255, 0, 0, 255],
            "Chromium 145 propagates the body canvas background beyond its box"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test]
    async fn capture_screenshot_combines_device_and_clip_scale_without_relayout() {
        let (mut ctx, session) = screenshot_fixture().await;
        crate::domains::emulation::handle(
            "setDeviceMetricsOverride",
            &json!({
                "width": 100,
                "height": 80,
                "deviceScaleFactor": 2,
                "mobile": false
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("device metrics override");

        let full_viewport = handle("captureScreenshot", &json!({}), &mut ctx, &session)
            .await
            .expect("2x viewport capture");
        let (_, raster) = decode_capture(&full_viewport);
        assert_eq!(raster.dimensions(), (200, 160));

        let clip = handle(
            "captureScreenshot",
            &json!({
                "clip": {"x": 50, "y": 0, "width": 20, "height": 10, "scale": 1.5}
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("device-scaled clip");
        let (_, raster) = decode_capture(&clip);
        assert_eq!(
            raster.dimensions(),
            (60, 30),
            "output scale is clip.scale times deviceScaleFactor"
        );
        let center = raster.get_pixel(30, 15).0;
        assert!(center[2] > 200 && center[0] < 50, "{center:?}");

        let page = ctx.get_session_page(&session).expect("page");
        assert_eq!(page.viewport, (100.0, 80.0));
        assert_eq!(page.device_scale_factor, 2.0);
    }

    #[tokio::test]
    async fn unknown_page_method_still_errors() {
        let mut ctx = CdpContext::new();
        let err = handle("notARealMethod", &json!({}), &mut ctx, &None)
            .await
            .expect_err("unknown methods must surface as errors");
        assert!(err.contains("Unknown Page method"));
    }

    #[tokio::test]
    async fn print_to_pdf_is_explicit_without_a_renderable_session() {
        // Regression for #53: Page.printToPDF must be handled explicitly so
        // Playwright clients receive a descriptive error rather than the
        // generic "Unknown Page method" fallback.
        let mut ctx = CdpContext::new();
        let err = handle("printToPDF", &json!({}), &mut ctx, &None)
            .await
            .expect_err("printToPDF without a page session must error");
        assert!(
            !err.contains("Unknown Page method"),
            "printToPDF must NOT fall through to the catch-all: {err}"
        );
        #[cfg(feature = "render")]
        assert!(err.contains("No page for session"), "{err}");
        #[cfg(not(feature = "render"))]
        assert!(
            err.contains("requires a build with the render feature"),
            "{err}"
        );
    }

    /// Regression for #45: same idea as printToPDF for captureScreenshot.
    /// Playwright's `page.screenshot()` calls Page.captureScreenshot via CDP;
    /// without an explicit arm, clients see "Unknown Page method" and have
    /// no idea why their screenshot request failed.
    #[tokio::test]
    async fn capture_screenshot_returns_descriptive_unsupported_error() {
        let mut ctx = CdpContext::new();
        let err = handle("captureScreenshot", &json!({}), &mut ctx, &None)
            .await
            .expect_err("captureScreenshot must error until a real paint exists");
        assert!(
            !err.contains("Unknown Page method"),
            "captureScreenshot must NOT fall through to the catch-all: {err}"
        );
        #[cfg(not(feature = "render"))]
        assert!(
            err.contains("requires a build with the render feature"),
            "error must clearly state screenshot needs the render feature: {err}"
        );
        // Same for the MHTML snapshot sibling method.
        let err2 = handle("captureSnapshot", &json!({}), &mut ctx, &None)
            .await
            .expect_err("captureSnapshot must error until a real renderer exists");
        assert!(
            !err2.contains("Unknown Page method"),
            "captureSnapshot must NOT fall through: {err2}"
        );
    }

    #[tokio::test]
    async fn navigation_emits_target_info_changed_with_url_and_title() {
        // Strict CDP clients (browser-use, Puppeteer/Playwright `page.url()`)
        // refresh a target's url/title only on Target.targetInfoChanged. A
        // navigation must emit it with the post-nav url/title, otherwise those
        // clients stay stuck on the pre-nav about:blank.
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{}-session", page_id);
        ctx.sessions.insert(session_id.clone(), page_id.clone());

        let params = json!({
            "url": "data:text/html,<title>Hello</title><button>B</button>",
            "waitUntil": "load",
        });
        handle("navigate", &params, &mut ctx, &Some(session_id.clone()))
            .await
            .expect("navigate should succeed");

        let evt = ctx
            .pending_events
            .iter()
            .find(|e| e.method == "Target.targetInfoChanged")
            .expect("navigation must emit Target.targetInfoChanged");
        // Browser-level event (no sessionId) so the root connection's
        // targetInfoChanged handler receives it.
        assert!(
            evt.session_id.is_none(),
            "targetInfoChanged must be browser-level (no sessionId)"
        );
        let info = evt.params["targetInfo"].clone();

        // The payload must carry the live post-navigation url/title and the
        // canAccessOpener field strict clients require on every TargetInfo.
        let (exp_url, exp_title) = {
            let page = ctx.get_page(&page_id).expect("page exists");
            (page.url_string(), page.title.clone())
        };
        assert_eq!(info["targetId"], json!(page_id));
        assert_eq!(info["type"], "page");
        assert_eq!(info["url"], json!(exp_url));
        assert_eq!(info["title"], json!(exp_title));
        assert!(
            info["url"]
                .as_str()
                .unwrap_or_default()
                .starts_with("data:"),
            "url should reflect the navigated page, got {}",
            info["url"]
        );
        assert_eq!(info["canAccessOpener"], json!(false));
    }
}
