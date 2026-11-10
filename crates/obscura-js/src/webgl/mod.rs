mod buffers;
mod egl;
mod programs;
mod state;
mod textures;
mod framebuffers;
pub(crate) mod ops;

use std::collections::{HashMap, HashSet};
use glow::HasContext;
use serde_json::{json, Value};

pub(super) const MAX_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_TEXTURE_BYTES: usize = 256 * 1024 * 1024;
const MAX_CONTEXTS: usize = 8;
const MAX_PIXELS: usize = 16 * 1024 * 1024;
pub(super) type Result<T> = std::result::Result<T, u32>;

#[derive(Default)]
pub(crate) struct Registry {
    contexts: HashMap<u32, Context>,
    next_id: u32,
}

impl Registry {
    pub fn create(&mut self, width: u32, height: u32, alpha: bool, premultiplied: bool) -> u32 {
        if self.contexts.len() >= MAX_CONTEXTS || !valid_size(width, height) { return 0; }
        let retained: usize = self.contexts.values().map(|context| context.width as usize * context.height as usize * 4).sum();
        if retained + width as usize * height as usize * 4 > 256 * 1024 * 1024 { return 0; }
        let Ok(context) = Context::new(width, height, alpha, premultiplied) else { return 0; };
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.contexts.insert(self.next_id, context);
        self.next_id
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Context> { self.contexts.get_mut(&id) }
    pub fn clear(&mut self) { self.contexts.clear(); }
}

pub(super) struct Attribute {
    buffer: u32,
    components: usize,
    component_bytes: usize,
    stride: usize,
    offset: usize,
}

pub(super) struct Texture {
    handle: glow::NativeTexture,
    bytes: usize,
    width: u32,
    height: u32,
}

pub(crate) struct Context {
    surface: egl::Surface,
    width: u32,
    height: u32,
    alpha: bool,
    premultiplied: bool,
    pixels: Vec<u8>,
    shaders: HashMap<u32, glow::NativeShader>,
    programs: HashMap<u32, glow::NativeProgram>,
    uniforms: HashMap<u32, (u32, glow::NativeUniformLocation)>,
    next_uniform: u32,
    buffers: HashMap<u32, (glow::NativeBuffer, Vec<u8>)>,
    array_buffer: u32,
    element_buffer: u32,
    attributes: HashMap<u32, Attribute>,
    enabled_attributes: HashSet<u32>,
    textures: HashMap<u32, Texture>,
    framebuffers: HashMap<u32, (glow::NativeFramebuffer, Option<u32>)>,
    framebuffer: u32,
    texture_units: HashMap<u32, u32>,
    active_texture: u32,
    flip_y: bool,
    unpack_premultiplied: bool,
    error: u32,
}

impl Context {
    fn new(width: u32, height: u32, alpha: bool, premultiplied: bool) -> std::result::Result<Self, String> {
        let surface = egl::Surface::new(width.max(1), height.max(1))?;
        unsafe { surface.gl.viewport(0, 0, width as i32, height as i32); }
        let context = Self {
            surface, width, height, alpha, premultiplied, pixels: Vec::new(),
            shaders: HashMap::new(), programs: HashMap::new(), uniforms: HashMap::new(), next_uniform: 0,
            buffers: HashMap::new(), array_buffer: 0, element_buffer: 0,
            attributes: HashMap::new(), enabled_attributes: HashSet::new(),
            textures: HashMap::new(), framebuffers: HashMap::new(), framebuffer: 0,
            texture_units: HashMap::new(), active_texture: 0,
            flip_y: false, unpack_premultiplied: false, error: 0,
        };
        context.clear_canvas();
        Ok(context)
    }

    pub fn command(&mut self, name: &str, args: &[Value]) -> Value {
        if self.surface.activate().is_err() { return json!({"error": glow::INVALID_OPERATION}); }
        let result = unsafe {
            match name {
                "createShader" | "shaderSource" | "compileShader" | "deleteShader"
                | "getShaderParameter" | "getShaderInfoLog" | "createProgram"
                | "attachShader" | "detachShader" | "linkProgram" | "useProgram"
                | "deleteProgram" | "getProgramParameter" | "getProgramInfoLog"
                | "getAttribLocation" | "bindAttribLocation" | "getUniformLocation"
                | "getActiveUniform" | "getActiveAttrib" | "getShaderPrecisionFormat"
                | "uniform1f" | "uniform2f" | "uniform3f" | "uniform4f"
                | "uniform1i" | "uniform2i" | "uniform3i" | "uniform4i"
                | "uniform1fv" | "uniform2fv" | "uniform3fv" | "uniform4fv"
                | "uniformMatrix2fv" | "uniformMatrix3fv" | "uniformMatrix4fv" => self.program_command(name, args),
                "createBuffer" | "deleteBuffer" | "bindBuffer" | "enableVertexAttribArray"
                | "disableVertexAttribArray" | "vertexAttribPointer" | "drawArrays" | "drawElements" => self.buffer_command(name, args),
                "createTexture" | "deleteTexture" | "bindTexture" | "activeTexture"
                | "texParameteri" | "texParameterf" | "generateMipmap" | "pixelStorei" => self.texture_command(name, args),
                "createFramebuffer" | "deleteFramebuffer" | "bindFramebuffer"
                | "framebufferTexture2D" | "checkFramebufferStatus" => self.framebuffer_command(name, args),
                _ => self.state_command(name, args),
            }
        };
        match result {
            Ok(value) => value,
            Err(error) => {
                if self.error == 0 { self.error = error; }
                Value::Null
            }
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) -> bool {
        if !valid_size(width, height) { return false; }
        if self.surface.resize(width.max(1), height.max(1)).is_err() { return false; }
        self.width = width;
        self.height = height;
        self.pixels.clear();
        self.clear_canvas();
        true
    }

    fn clear_canvas(&self) {
        unsafe {
            let gl = &self.surface.gl;
            let mut color = [0.0; 4];
            let mut mask = [0; 4];
            gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut color);
            gl.get_parameter_i32_slice(glow::COLOR_WRITEMASK, &mut mask);
            let scissor = gl.is_enabled(glow::SCISSOR_TEST);
            let framebuffer = self.framebuffers.get(&self.framebuffer).map(|(framebuffer, _)| *framebuffer);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask(true, true, true, true);
            gl.clear_color(0.0, 0.0, 0.0, if self.alpha { 0.0 } else { 1.0 });
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.clear_color(color[0], color[1], color[2], color[3]);
            gl.color_mask(mask[0] != 0, mask[1] != 0, mask[2] != 0, mask[3] != 0);
            if scissor { gl.enable(glow::SCISSOR_TEST); }
            gl.bind_framebuffer(glow::FRAMEBUFFER, framebuffer);
        }
    }

    pub fn snapshot(&mut self, output: &mut [u8]) -> bool {
        let expected = self.width as usize * self.height as usize * 4;
        if output.len() != expected || self.surface.activate().is_err() { return false; }
        if expected == 0 { return true; }
        self.pixels.resize(expected, 0);
        unsafe {
            let framebuffer = self.framebuffers.get(&self.framebuffer).map(|(framebuffer, _)| *framebuffer);
            self.surface.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.surface.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            self.surface.gl.read_pixels(0, 0, self.width as i32, self.height as i32,
                glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut self.pixels)));
            self.surface.gl.bind_framebuffer(glow::FRAMEBUFFER, framebuffer);
        }
        let row_bytes = self.width as usize * 4;
        for (row, source) in self.pixels.chunks_exact(row_bytes).rev().enumerate() {
            output[row * row_bytes..(row + 1) * row_bytes].copy_from_slice(source);
        }
        for pixel in output.chunks_exact_mut(4) {
            if !self.alpha { pixel[3] = 255; }
            if self.premultiplied && pixel[3] > 0 && pixel[3] < 255 {
                let alpha = pixel[3] as u32;
                for channel in &mut pixel[..3] { *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8; }
            }
        }
        true
    }

    pub fn read_pixels(&mut self, region: &[i32], output: &mut [u8]) -> bool {
        if region.len() != 4 { return false; }
        let [x, y, width, height] = [region[0], region[1], region[2], region[3]];
        if width < 0 || height < 0 || !valid_size(width as u32, height as u32) { return false; }
        let expected = width as usize * height as usize * 4;
        if output.len() < expected || self.surface.activate().is_err() { return false; }
        output[..expected].fill(0);
        let dimensions = self.framebuffers.get(&self.framebuffer)
            .and_then(|(_, texture)| texture.and_then(|texture| self.textures.get(&texture)))
            .map(|texture| (texture.width, texture.height)).unwrap_or((self.width, self.height));
        let left = x.max(0).min(dimensions.0 as i32);
        let bottom = y.max(0).min(dimensions.1 as i32);
        let right = x.saturating_add(width).max(0).min(dimensions.0 as i32);
        let top = y.saturating_add(height).max(0).min(dimensions.1 as i32);
        if right <= left || top <= bottom { return true; }
        let row_bytes = (right - left) as usize * 4;
        let mut pixels = vec![0; row_bytes * (top - bottom) as usize];
        unsafe {
            self.surface.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            self.surface.gl.read_pixels(left, bottom, right - left, top - bottom,
                glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut pixels)));
        }
        for (row, source) in pixels.chunks_exact(row_bytes).enumerate() {
            let destination = ((bottom - y) as usize + row) * width as usize * 4 + (left - x) as usize * 4;
            output[destination..destination + row_bytes].copy_from_slice(source);
        }
        true
    }
}

fn valid_size(width: u32, height: u32) -> bool {
    width <= 8192 && height <= 8192 && width as usize * height as usize <= MAX_PIXELS
}

pub(super) fn unsigned(args: &[Value], index: usize) -> Result<u32> {
    args.get(index).and_then(Value::as_u64).and_then(|value| u32::try_from(value).ok()).ok_or(glow::INVALID_VALUE)
}

pub(super) fn integer(args: &[Value], index: usize) -> Result<i32> {
    args.get(index).and_then(Value::as_i64).and_then(|value| i32::try_from(value).ok()).ok_or(glow::INVALID_VALUE)
}

pub(super) fn float(args: &[Value], index: usize) -> Result<f32> {
    args.get(index).and_then(Value::as_f64).filter(|value| value.is_finite()).map(|value| value as f32).ok_or(glow::INVALID_VALUE)
}

pub(super) fn string<'a>(args: &'a [Value], index: usize) -> Result<&'a str> {
    args.get(index).and_then(Value::as_str).filter(|value| value.len() <= 1024 * 1024).ok_or(glow::INVALID_VALUE)
}

pub(super) fn object_id(args: &[Value], index: usize) -> Result<u32> {
    if args.get(index).is_some_and(Value::is_null) { return Ok(0); }
    unsigned(args, index)
}
