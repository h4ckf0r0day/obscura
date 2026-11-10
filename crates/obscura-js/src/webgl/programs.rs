use glow::HasContext;
use serde_json::{json, Value};
use super::{float, integer, object_id, string, unsigned, Context, Result};

impl Context {
    unsafe fn shader(&self, id: u32) -> Result<glow::NativeShader> {
        self.shaders.get(&id).copied().ok_or(glow::INVALID_OPERATION)
    }

    unsafe fn program(&self, id: u32) -> Result<glow::NativeProgram> {
        self.programs.get(&id).copied().ok_or(glow::INVALID_OPERATION)
    }

    pub(super) unsafe fn program_command(&mut self, name: &str, args: &[Value]) -> Result<Value> {
        let gl = &self.surface.gl;
        match name {
            "createShader" => {
                let kind = unsigned(args, 0)?;
                if !matches!(kind, glow::VERTEX_SHADER | glow::FRAGMENT_SHADER) { return Err(glow::INVALID_ENUM); }
                if self.shaders.len() >= 256 { return Err(glow::OUT_OF_MEMORY); }
                let shader = gl.create_shader(kind).map_err(|_| glow::OUT_OF_MEMORY)?;
                self.shaders.insert(shader.0.get(), shader);
                return Ok(json!(shader.0.get()));
            }
            "shaderSource" => gl.shader_source(self.shader(unsigned(args, 0)?)?, string(args, 1)?),
            "compileShader" => gl.compile_shader(self.shader(unsigned(args, 0)?)?),
            "deleteShader" => {
                if let Some(shader) = self.shaders.remove(&object_id(args, 0)?) { gl.delete_shader(shader); }
            }
            "getShaderParameter" => {
                let shader = self.shader(unsigned(args, 0)?)?;
                return match unsigned(args, 1)? {
                    glow::COMPILE_STATUS => Ok(json!(gl.get_shader_compile_status(shader))),
                    _ => Err(glow::INVALID_ENUM),
                };
            }
            "getShaderInfoLog" => return Ok(json!(gl.get_shader_info_log(self.shader(unsigned(args, 0)?)?))),
            "createProgram" => {
                if self.programs.len() >= 256 { return Err(glow::OUT_OF_MEMORY); }
                let program = gl.create_program().map_err(|_| glow::OUT_OF_MEMORY)?;
                self.programs.insert(program.0.get(), program);
                return Ok(json!(program.0.get()));
            }
            "attachShader" => gl.attach_shader(self.program(unsigned(args, 0)?)?, self.shader(unsigned(args, 1)?)?),
            "detachShader" => gl.detach_shader(self.program(unsigned(args, 0)?)?, self.shader(unsigned(args, 1)?)?),
            "linkProgram" => {
                let id = unsigned(args, 0)?;
                self.uniforms.retain(|_, (program, _)| *program != id);
                gl.link_program(self.program(id)?);
            }
            "useProgram" => {
                let id = object_id(args, 0)?;
                gl.use_program(if id == 0 { None } else { Some(self.program(id)?) });
            }
            "deleteProgram" => {
                let id = object_id(args, 0)?;
                self.uniforms.retain(|_, (program, _)| *program != id);
                if let Some(program) = self.programs.remove(&id) { gl.delete_program(program); }
            }
            "getProgramParameter" => {
                let program = self.program(unsigned(args, 0)?)?;
                return match unsigned(args, 1)? {
                    glow::LINK_STATUS => Ok(json!(gl.get_program_link_status(program))),
                    glow::VALIDATE_STATUS => Ok(json!(gl.get_program_validate_status(program))),
                    glow::ACTIVE_UNIFORMS | glow::ACTIVE_ATTRIBUTES | glow::ATTACHED_SHADERS =>
                        Ok(json!(gl.get_program_parameter_i32(program, unsigned(args, 1)?))),
                    _ => Err(glow::INVALID_ENUM),
                };
            }
            "getProgramInfoLog" => return Ok(json!(gl.get_program_info_log(self.program(unsigned(args, 0)?)?))),
            "getAttribLocation" => return Ok(json!(gl.get_attrib_location(self.program(unsigned(args, 0)?)?, string(args, 1)?).map(|value| value as i32).unwrap_or(-1))),
            "bindAttribLocation" => gl.bind_attrib_location(self.program(unsigned(args, 0)?)?, unsigned(args, 1)?, string(args, 2)?),
            "getUniformLocation" => {
                let Some(location) = gl.get_uniform_location(self.program(unsigned(args, 0)?)?, string(args, 1)?) else { return Ok(Value::Null); };
                if self.uniforms.len() >= 4096 { return Err(glow::OUT_OF_MEMORY); }
                self.next_uniform = self.next_uniform.wrapping_add(1).max(1);
                self.uniforms.insert(self.next_uniform, (unsigned(args, 0)?, location));
                return Ok(json!(self.next_uniform));
            }
            "getActiveUniform" => return Ok(gl.get_active_uniform(self.program(unsigned(args, 0)?)?, unsigned(args, 1)?)
                .map(|uniform| json!({"name": uniform.name, "size": uniform.size, "type": uniform.utype})).unwrap_or(Value::Null)),
            "getActiveAttrib" => return Ok(gl.get_active_attribute(self.program(unsigned(args, 0)?)?, unsigned(args, 1)?)
                .map(|attribute| json!({"name": attribute.name, "size": attribute.size, "type": attribute.atype})).unwrap_or(Value::Null)),
            "getShaderPrecisionFormat" => return Ok(gl.get_shader_precision_format(unsigned(args, 0)?, unsigned(args, 1)?)
                .map(|format| json!({"rangeMin": format.range_min, "rangeMax": format.range_max, "precision": format.precision})).unwrap_or(Value::Null)),
            _ => return self.uniform_command(name, args),
        }
        Ok(Value::Null)
    }

    unsafe fn uniform_command(&self, name: &str, args: &[Value]) -> Result<Value> {
        if args.first().is_some_and(Value::is_null) { return Ok(Value::Null); }
        let (program, location) = self.uniforms.get(&unsigned(args, 0)?).ok_or(glow::INVALID_OPERATION)?;
        let gl = &self.surface.gl;
        if gl.get_parameter_i32(glow::CURRENT_PROGRAM) as u32 != *program { return Err(glow::INVALID_OPERATION); }
        match name {
            "uniform1f" => gl.uniform_1_f32(Some(location), float(args, 1)?),
            "uniform2f" => gl.uniform_2_f32(Some(location), float(args, 1)?, float(args, 2)?),
            "uniform3f" => gl.uniform_3_f32(Some(location), float(args, 1)?, float(args, 2)?, float(args, 3)?),
            "uniform4f" => gl.uniform_4_f32(Some(location), float(args, 1)?, float(args, 2)?, float(args, 3)?, float(args, 4)?),
            "uniform1i" => gl.uniform_1_i32(Some(location), integer(args, 1)?),
            "uniform2i" => gl.uniform_2_i32(Some(location), integer(args, 1)?, integer(args, 2)?),
            "uniform3i" => gl.uniform_3_i32(Some(location), integer(args, 1)?, integer(args, 2)?, integer(args, 3)?),
            "uniform4i" => gl.uniform_4_i32(Some(location), integer(args, 1)?, integer(args, 2)?, integer(args, 3)?, integer(args, 4)?),
            _ => {
                let is_matrix = name.starts_with("uniformMatrix");
                if is_matrix && args.get(1).and_then(Value::as_bool) != Some(false) { return Err(glow::INVALID_VALUE); }
                let values = args.get(if is_matrix { 2 } else { 1 }).and_then(Value::as_array).ok_or(glow::INVALID_VALUE)?;
                if values.len() > 16 * 1024 { return Err(glow::INVALID_VALUE); }
                let values = values.iter().map(|value| value.as_f64().filter(|value| value.is_finite()).map(|value| value as f32).ok_or(glow::INVALID_VALUE)).collect::<Result<Vec<_>>>()?;
                match name {
                    "uniform1fv" => gl.uniform_1_f32_slice(Some(location), &values),
                    "uniform2fv" if values.len() % 2 == 0 => gl.uniform_2_f32_slice(Some(location), &values),
                    "uniform3fv" if values.len() % 3 == 0 => gl.uniform_3_f32_slice(Some(location), &values),
                    "uniform4fv" if values.len() % 4 == 0 => gl.uniform_4_f32_slice(Some(location), &values),
                    "uniformMatrix2fv" if values.len() % 4 == 0 => gl.uniform_matrix_2_f32_slice(Some(location), false, &values),
                    "uniformMatrix3fv" if values.len() % 9 == 0 => gl.uniform_matrix_3_f32_slice(Some(location), false, &values),
                    "uniformMatrix4fv" if values.len() % 16 == 0 => gl.uniform_matrix_4_f32_slice(Some(location), false, &values),
                    _ => return Err(glow::INVALID_VALUE),
                }
            }
        }
        Ok(Value::Null)
    }
}
