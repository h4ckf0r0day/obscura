use std::panic::{catch_unwind, AssertUnwindSafe};
use deno_core::{op2, OpDecl, OpState};
use serde_json::{json, Value};
use crate::ops::{image_request_profile, SharedState};

pub(crate) fn declarations() -> Vec<OpDecl> {
    vec![op_webgl_create(), op_webgl_command(), op_webgl_resize(), op_webgl_snapshot(), op_webgl_read_pixels(),
        op_webgl_upload(), op_webgl_upload_image(), op_webgl_decode_image()]
}

#[op2(nofast)]
fn op_webgl_create(state: &OpState, width: u32, height: u32, alpha: bool, premultiplied: bool) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        state.borrow::<SharedState>().borrow_mut().webgl.create(width, height, alpha, premultiplied)
    })).unwrap_or(0)
}

#[op2]
#[serde]
fn op_webgl_command(state: &OpState, context: u32, #[string] name: &str, #[serde] args: Vec<serde_json::Value>) -> serde_json::Value {
    catch_unwind(AssertUnwindSafe(|| {
        if name.len() > 64 || args.len() > 32 { return json!({"error": glow::INVALID_VALUE}); }
        let shared = state.borrow::<SharedState>();
        let mut page = shared.borrow_mut();
        page.webgl.get_mut(context).map(|context| context.command(name, &args))
            .unwrap_or_else(|| json!({"error": glow::INVALID_OPERATION}))
    })).unwrap_or_else(|_| json!({"error": glow::INVALID_OPERATION}))
}

#[op2(nofast)]
fn op_webgl_resize(state: &OpState, context: u32, width: u32, height: u32) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        state.borrow::<SharedState>().borrow_mut().webgl.get_mut(context)
            .is_some_and(|context| context.resize(width, height))
    })).unwrap_or(false)
}

#[op2(nofast)]
fn op_webgl_snapshot(state: &OpState, context: u32, #[buffer] output: &mut [u8]) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        state.borrow::<SharedState>().borrow_mut().webgl.get_mut(context)
            .is_some_and(|context| context.snapshot(output))
    })).unwrap_or(false)
}

#[op2]
fn op_webgl_read_pixels(state: &OpState, context: u32, #[serde] region: Vec<i32>, #[buffer] output: &mut [u8]) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        state.borrow::<SharedState>().borrow_mut().webgl.get_mut(context)
            .is_some_and(|context| context.read_pixels(&region, output))
    })).unwrap_or(false)
}

#[op2]
fn op_webgl_upload(state: &OpState, context: u32, #[string] kind: &str,
    #[serde] args: Vec<Value>, #[buffer] bytes: &[u8]) -> bool
{
    catch_unwind(AssertUnwindSafe(|| {
        if bytes.len() > super::MAX_BYTES { return false; }
        let shared = state.borrow::<SharedState>();
        let mut page = shared.borrow_mut();
        let Some(context) = page.webgl.get_mut(context) else { return false; };
        match kind {
            "bufferData" => context.upload_buffer(args.first().and_then(Value::as_u64).unwrap_or(0) as u32,
                bytes, args.get(1).and_then(Value::as_u64).unwrap_or(0) as u32, None),
            "bufferSubData" => context.upload_buffer(args.first().and_then(Value::as_u64).unwrap_or(0) as u32,
                bytes, 0, Some(args.get(1).and_then(Value::as_u64).unwrap_or(u64::MAX) as usize)),
            "texImage2D" => context.upload_texture(&args, bytes),
            _ => false,
        }
    })).unwrap_or(false)
}

#[op2]
fn op_webgl_upload_image(state: &OpState, context: u32, nid: u32, #[serde] args: Vec<Value>) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let shared = state.borrow::<SharedState>();
        let mut page = shared.borrow_mut();
        let Some(dom) = page.dom.as_ref() else { return false; };
        let node = obscura_dom::NodeId::new(nid);
        let profile = image_request_profile(dom, node);
        let Some((url, _, true, Some(_))) = page.render_resources
            .cached_image_element_metadata(dom, node, page.viewport, Some(&page.url)) else { return false; };
        if profile == obscura_render::ImageRequestProfile::NoCorsInclude && !url.starts_with("data:") {
            let origins = url::Url::parse(&page.url).ok().zip(url::Url::parse(&url).ok());
            if !origins.is_some_and(|(document, image)| document.origin() == image.origin()) { return false; }
        }
        let Some(bytes) = page.render_resources.cached_image_bytes(&url, profile) else { return false; };
        let Ok(mut reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format() else { return false; };
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(super::MAX_BYTES as u64);
        reader.limits(limits);
        let Ok(image) = reader.decode() else { return false; };
        let image = image.into_rgba8();
        if args.len() != 5 { return false; }
        let upload = vec![args[0].clone(), args[1].clone(), args[2].clone(), json!(image.width()),
            json!(image.height()), args[3].clone(), args[4].clone()];
        page.webgl.get_mut(context).is_some_and(|context| context.upload_texture(&upload, image.as_raw()))
    })).unwrap_or(false)
}

#[op2]
#[buffer]
fn op_webgl_decode_image(#[buffer] bytes: &[u8]) -> Vec<u8> {
    catch_unwind(AssertUnwindSafe(|| {
        if bytes.len() > super::MAX_BYTES { return Vec::new(); }
        let Ok(mut reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format() else { return Vec::new(); };
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(super::MAX_BYTES as u64);
        reader.limits(limits);
        let Ok(image) = reader.decode() else { return Vec::new(); };
        let image = image.into_rgba8();
        if image.as_raw().len() > super::MAX_BYTES { return Vec::new(); }
        let mut output = Vec::with_capacity(8 + image.as_raw().len());
        output.extend_from_slice(&image.width().to_le_bytes());
        output.extend_from_slice(&image.height().to_le_bytes());
        output.extend_from_slice(image.as_raw());
        output
    })).unwrap_or_default()
}
