use glow::HasContext;
use serde_json::{json, Value};
use super::{integer, object_id, unsigned, Context, Result, Texture, MAX_BYTES, MAX_TEXTURE_BYTES};

impl Context {
    pub fn upload_texture(&mut self, args: &[Value], bytes: &[u8]) -> bool {
        let result = (|| -> Result<()> {
            self.surface.activate().map_err(|_| glow::INVALID_OPERATION)?;
            let target = unsigned(args, 0)?;
            let level = integer(args, 1)?;
            let internal = unsigned(args, 2)?;
            let width = unsigned(args, 3)?;
            let height = unsigned(args, 4)?;
            let format = unsigned(args, 5)?;
            let kind = unsigned(args, 6)?;
            if target != glow::TEXTURE_2D || format != glow::RGBA || kind != glow::UNSIGNED_BYTE || internal != format { return Err(glow::INVALID_ENUM); }
            if level != 0 || width > 8192 || height > 8192 { return Err(glow::INVALID_VALUE); }
            let expected = (width as usize).checked_mul(height as usize).and_then(|size| size.checked_mul(4)).ok_or(glow::INVALID_VALUE)?;
            if expected > MAX_BYTES || bytes.len() != expected { return Err(glow::INVALID_VALUE); }
            let id = *self.texture_units.get(&self.active_texture).ok_or(glow::INVALID_OPERATION)?;
            let total: usize = self.textures.values().map(|texture| texture.bytes).sum();
            let texture = self.textures.get_mut(&id).ok_or(glow::INVALID_OPERATION)?;
            if total - texture.bytes + expected > MAX_TEXTURE_BYTES { return Err(glow::OUT_OF_MEMORY); }
            let mut converted = bytes.to_vec();
            let bitmap = args.get(7).and_then(Value::as_bool).unwrap_or(false);
            if self.flip_y && !bitmap && width > 0 {
                let row = width as usize * 4;
                for y in 0..height as usize {
                    converted[y * row..(y + 1) * row].copy_from_slice(&bytes[(height as usize - y - 1) * row..(height as usize - y) * row]);
                }
            }
            if self.unpack_premultiplied && !bitmap {
                for pixel in converted.chunks_exact_mut(4) {
                    let alpha = pixel[3] as u32;
                    for channel in &mut pixel[..3] { *channel = ((*channel as u32 * alpha + 127) / 255) as u8; }
                }
            }
            unsafe {
                self.surface.gl.bind_texture(target, Some(texture.handle));
                self.surface.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                self.surface.gl.tex_image_2d(target, level, internal as i32, width as i32, height as i32,
                    0, format, kind, glow::PixelUnpackData::Slice(Some(&converted)));
            }
            texture.bytes = expected;
            texture.width = width;
            texture.height = height;
            Ok(())
        })();
        if let Err(error) = result { self.error = error; return false; }
        true
    }

    pub(super) unsafe fn texture_command(&mut self, name: &str, args: &[Value]) -> Result<Value> {
        let gl = &self.surface.gl;
        match name {
            "createTexture" => {
                if self.textures.len() >= 1024 { return Err(glow::OUT_OF_MEMORY); }
                let texture = gl.create_texture().map_err(|_| glow::OUT_OF_MEMORY)?;
                self.textures.insert(texture.0.get(), Texture { handle: texture, bytes: 0, width: 0, height: 0 });
                return Ok(json!(texture.0.get()));
            }
            "deleteTexture" => {
                let id = object_id(args, 0)?;
                if let Some(texture) = self.textures.remove(&id) { gl.delete_texture(texture.handle); }
                self.texture_units.retain(|_, texture| *texture != id);
            }
            "activeTexture" => {
                let texture = unsigned(args, 0)?;
                let maximum = gl.get_parameter_i32(glow::MAX_COMBINED_TEXTURE_IMAGE_UNITS).min(32) as u32;
                if texture < glow::TEXTURE0 || texture - glow::TEXTURE0 >= maximum { return Err(glow::INVALID_ENUM); }
                self.active_texture = texture - glow::TEXTURE0;
                gl.active_texture(texture);
            }
            "bindTexture" => {
                let target = unsigned(args, 0)?;
                if target != glow::TEXTURE_2D { return Err(glow::INVALID_ENUM); }
                let id = object_id(args, 1)?;
                let texture = if id == 0 { None } else { Some(self.textures.get(&id).ok_or(glow::INVALID_OPERATION)?.handle) };
                self.texture_units.insert(self.active_texture, id);
                gl.bind_texture(target, texture);
            }
            "texParameteri" | "texParameterf" => {
                let target = unsigned(args, 0)?;
                if target != glow::TEXTURE_2D { return Err(glow::INVALID_ENUM); }
                gl.tex_parameter_i32(target, unsigned(args, 1)?, integer(args, 2)?);
            }
            "generateMipmap" => {
                if unsigned(args, 0)? != glow::TEXTURE_2D { return Err(glow::INVALID_ENUM); }
                gl.generate_mipmap(glow::TEXTURE_2D);
            }
            "pixelStorei" => {
                let name = unsigned(args, 0)?;
                let value = args.get(1).and_then(Value::as_bool).map(i32::from).or_else(|| args.get(1).and_then(Value::as_i64).and_then(|value| i32::try_from(value).ok())).ok_or(glow::INVALID_VALUE)?;
                match name {
                    0x9240 => self.flip_y = value != 0,
                    0x9241 => self.unpack_premultiplied = value != 0,
                    0x9243 if value == 0 || value == 0x9244 => {},
                    glow::PACK_ALIGNMENT | glow::UNPACK_ALIGNMENT if matches!(value, 1 | 2 | 4 | 8) => gl.pixel_store_i32(name, value),
                    _ => return Err(glow::INVALID_ENUM),
                }
            }
            _ => return Err(glow::INVALID_OPERATION),
        }
        Ok(Value::Null)
    }
}
