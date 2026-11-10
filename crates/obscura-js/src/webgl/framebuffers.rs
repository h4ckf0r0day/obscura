use glow::HasContext;
use serde_json::{json, Value};
use super::{object_id, unsigned, Context, Result};

impl Context {
    pub(super) unsafe fn framebuffer_command(&mut self, name: &str, args: &[Value]) -> Result<Value> {
        let gl = &self.surface.gl;
        match name {
            "createFramebuffer" => {
                if self.framebuffers.len() >= 256 { return Err(glow::OUT_OF_MEMORY); }
                let framebuffer = gl.create_framebuffer().map_err(|_| glow::OUT_OF_MEMORY)?;
                self.framebuffers.insert(framebuffer.0.get(), (framebuffer, None));
                return Ok(json!(framebuffer.0.get()));
            }
            "deleteFramebuffer" => {
                let id = object_id(args, 0)?;
                if let Some((framebuffer, _)) = self.framebuffers.remove(&id) { gl.delete_framebuffer(framebuffer); }
                if self.framebuffer == id { self.framebuffer = 0; }
            }
            "bindFramebuffer" => {
                if unsigned(args, 0)? != glow::FRAMEBUFFER { return Err(glow::INVALID_ENUM); }
                let id = object_id(args, 1)?;
                let framebuffer = if id == 0 { None } else { Some(self.framebuffers.get(&id).ok_or(glow::INVALID_OPERATION)?.0) };
                gl.bind_framebuffer(glow::FRAMEBUFFER, framebuffer);
                self.framebuffer = id;
            }
            "framebufferTexture2D" => {
                if unsigned(args, 0)? != glow::FRAMEBUFFER || unsigned(args, 1)? != glow::COLOR_ATTACHMENT0
                    || unsigned(args, 2)? != glow::TEXTURE_2D || unsigned(args, 4)? != 0 { return Err(glow::INVALID_ENUM); }
                let id = object_id(args, 3)?;
                let texture = if id == 0 { None } else { Some(self.textures.get(&id).ok_or(glow::INVALID_OPERATION)?.handle) };
                let (_, attachment) = self.framebuffers.get_mut(&self.framebuffer).ok_or(glow::INVALID_OPERATION)?;
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, texture, 0);
                *attachment = (id != 0).then_some(id);
            }
            "checkFramebufferStatus" => {
                if unsigned(args, 0)? != glow::FRAMEBUFFER { return Err(glow::INVALID_ENUM); }
                return Ok(json!(gl.check_framebuffer_status(glow::FRAMEBUFFER)));
            }
            _ => return Err(glow::INVALID_OPERATION),
        }
        Ok(Value::Null)
    }
}
