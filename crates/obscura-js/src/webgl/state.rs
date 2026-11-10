use glow::HasContext;
use serde_json::{json, Value};
use super::{float, integer, unsigned, Context, Result};

impl Context {
    pub(super) unsafe fn state_command(&mut self, name: &str, args: &[Value]) -> Result<Value> {
        let gl = &self.surface.gl;
        match name {
            "clearColor" => gl.clear_color(float(args, 0)?, float(args, 1)?, float(args, 2)?, float(args, 3)?),
            "clearDepth" => gl.clear_depth_f32(float(args, 0)?),
            "clearStencil" => gl.clear_stencil(integer(args, 0)?),
            "clear" => gl.clear(unsigned(args, 0)?),
            "viewport" => gl.viewport(integer(args, 0)?, integer(args, 1)?, integer(args, 2)?, integer(args, 3)?),
            "scissor" => gl.scissor(integer(args, 0)?, integer(args, 1)?, integer(args, 2)?, integer(args, 3)?),
            "enable" => gl.enable(unsigned(args, 0)?),
            "disable" => gl.disable(unsigned(args, 0)?),
            "isEnabled" => return Ok(json!(gl.is_enabled(unsigned(args, 0)?))),
            "blendFunc" => gl.blend_func(unsigned(args, 0)?, unsigned(args, 1)?),
            "blendFuncSeparate" => gl.blend_func_separate(unsigned(args, 0)?, unsigned(args, 1)?, unsigned(args, 2)?, unsigned(args, 3)?),
            "blendEquation" => gl.blend_equation(unsigned(args, 0)?),
            "blendEquationSeparate" => gl.blend_equation_separate(unsigned(args, 0)?, unsigned(args, 1)?),
            "blendColor" => gl.blend_color(float(args, 0)?, float(args, 1)?, float(args, 2)?, float(args, 3)?),
            "depthFunc" => gl.depth_func(unsigned(args, 0)?),
            "depthMask" => gl.depth_mask(args.first().and_then(Value::as_bool).ok_or(glow::INVALID_VALUE)?),
            "depthRange" => gl.depth_range_f32(float(args, 0)?, float(args, 1)?),
            "cullFace" => gl.cull_face(unsigned(args, 0)?),
            "frontFace" => gl.front_face(unsigned(args, 0)?),
            "lineWidth" => gl.line_width(float(args, 0)?),
            "polygonOffset" => gl.polygon_offset(float(args, 0)?, float(args, 1)?),
            "colorMask" => {
                let flags = args.iter().map(|value| value.as_bool().ok_or(glow::INVALID_VALUE)).collect::<Result<Vec<_>>>()?;
                if flags.len() != 4 { return Err(glow::INVALID_VALUE); }
                gl.color_mask(flags[0], flags[1], flags[2], flags[3]);
            }
            "flush" => gl.flush(),
            "finish" => gl.finish(),
            "getError" => {
                let error = std::mem::take(&mut self.error);
                return Ok(json!(if error == 0 { gl.get_error() } else { error }));
            }
            "getParameter" => return self.parameter(unsigned(args, 0)?),
            "getSupportedExtensions" => return Ok(json!([])),
            _ => return Err(glow::INVALID_OPERATION),
        }
        Ok(Value::Null)
    }

    unsafe fn parameter(&self, name: u32) -> Result<Value> {
        let gl = &self.surface.gl;
        match name {
            glow::VENDOR | glow::RENDERER => Ok(json!(gl.get_parameter_string(name))),
            glow::VERSION => Ok(json!(format!("WebGL 1.0 ({})", gl.get_parameter_string(name)))),
            glow::SHADING_LANGUAGE_VERSION => Ok(json!(format!("WebGL GLSL ES 1.0 ({})", gl.get_parameter_string(name)))),
            glow::MAX_TEXTURE_SIZE | glow::MAX_CUBE_MAP_TEXTURE_SIZE | glow::MAX_RENDERBUFFER_SIZE => Ok(json!(gl.get_parameter_i32(name).min(8192))),
            glow::MAX_VERTEX_ATTRIBS | glow::MAX_VERTEX_UNIFORM_VECTORS | glow::MAX_VARYING_VECTORS
            | glow::MAX_COMBINED_TEXTURE_IMAGE_UNITS | glow::MAX_VERTEX_TEXTURE_IMAGE_UNITS
            | glow::MAX_FRAGMENT_UNIFORM_VECTORS | glow::MAX_TEXTURE_IMAGE_UNITS
            | glow::RED_BITS | glow::GREEN_BITS | glow::BLUE_BITS | glow::ALPHA_BITS
            | glow::DEPTH_BITS | glow::STENCIL_BITS | glow::ACTIVE_TEXTURE
            | glow::CURRENT_PROGRAM | glow::ARRAY_BUFFER_BINDING | glow::ELEMENT_ARRAY_BUFFER_BINDING
            | glow::TEXTURE_BINDING_2D | glow::FRAMEBUFFER_BINDING => Ok(json!(gl.get_parameter_i32(name))),
            glow::VIEWPORT | glow::SCISSOR_BOX => {
                let mut values = [0; 4]; gl.get_parameter_i32_slice(name, &mut values); Ok(json!(values))
            }
            glow::MAX_VIEWPORT_DIMS => {
                let mut values = [0; 2]; gl.get_parameter_i32_slice(name, &mut values); Ok(json!(values))
            }
            glow::COLOR_CLEAR_VALUE => {
                let mut values = [0.0; 4]; gl.get_parameter_f32_slice(name, &mut values); Ok(json!(values))
            }
            glow::ALIASED_LINE_WIDTH_RANGE | glow::ALIASED_POINT_SIZE_RANGE => {
                let mut values = [0.0; 2]; gl.get_parameter_f32_slice(name, &mut values); Ok(json!(values))
            }
            _ => Err(glow::INVALID_ENUM),
        }
    }
}
