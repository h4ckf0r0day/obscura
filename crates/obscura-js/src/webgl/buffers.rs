use glow::HasContext;
use serde_json::{json, Value};
use super::{object_id, unsigned, Attribute, Context, Result, MAX_BYTES};

impl Context {
    fn bound_buffer(&self, target: u32) -> Result<u32> {
        match target {
            glow::ARRAY_BUFFER => Ok(self.array_buffer),
            glow::ELEMENT_ARRAY_BUFFER => Ok(self.element_buffer),
            _ => Err(glow::INVALID_ENUM),
        }
    }

    pub fn upload_buffer(&mut self, target: u32, data: &[u8], usage: u32, offset: Option<usize>) -> bool {
        let result = (|| -> Result<()> {
            self.surface.activate().map_err(|_| glow::INVALID_OPERATION)?;
            let id = self.bound_buffer(target)?;
            let total: usize = self.buffers.values().map(|(_, data)| data.len()).sum();
            let (_, retained) = self.buffers.get_mut(&id).ok_or(glow::INVALID_OPERATION)?;
            if let Some(offset) = offset {
                let end = offset.checked_add(data.len()).ok_or(glow::INVALID_VALUE)?;
                if end > retained.len() { return Err(glow::INVALID_VALUE); }
                retained[offset..end].copy_from_slice(data);
                unsafe { self.surface.gl.buffer_sub_data_u8_slice(target, offset as i32, data); }
                return Ok(());
            }
            if data.len() > MAX_BYTES || total - retained.len() + data.len() > MAX_BYTES { return Err(glow::OUT_OF_MEMORY); }
            if !matches!(usage, glow::STATIC_DRAW | glow::DYNAMIC_DRAW | glow::STREAM_DRAW) { return Err(glow::INVALID_ENUM); }
            retained.clear();
            retained.extend_from_slice(data);
            unsafe { self.surface.gl.buffer_data_u8_slice(target, data, usage); }
            Ok(())
        })();
        if let Err(error) = result { self.error = error; return false; }
        true
    }

    fn validate_vertices(&self, last: usize) -> Result<()> {
        for location in &self.enabled_attributes {
            let attribute = self.attributes.get(location).ok_or(glow::INVALID_OPERATION)?;
            let (_, buffer) = self.buffers.get(&attribute.buffer).ok_or(glow::INVALID_OPERATION)?;
            let size = attribute.components * attribute.component_bytes;
            let stride = if attribute.stride == 0 { size } else { attribute.stride };
            let end = last.checked_mul(stride).and_then(|bytes| bytes.checked_add(attribute.offset)).and_then(|bytes| bytes.checked_add(size)).ok_or(glow::INVALID_OPERATION)?;
            if end > buffer.len() { return Err(glow::INVALID_OPERATION); }
        }
        Ok(())
    }

    pub(super) unsafe fn buffer_command(&mut self, name: &str, args: &[Value]) -> Result<Value> {
        let gl = &self.surface.gl;
        match name {
            "createBuffer" => {
                if self.buffers.len() >= 4096 { return Err(glow::OUT_OF_MEMORY); }
                let buffer = gl.create_buffer().map_err(|_| glow::OUT_OF_MEMORY)?;
                self.buffers.insert(buffer.0.get(), (buffer, Vec::new()));
                return Ok(json!(buffer.0.get()));
            }
            "deleteBuffer" => {
                let id = object_id(args, 0)?;
                if let Some((buffer, _)) = self.buffers.remove(&id) { gl.delete_buffer(buffer); }
                if self.array_buffer == id { self.array_buffer = 0; }
                if self.element_buffer == id { self.element_buffer = 0; }
            }
            "bindBuffer" => {
                let target = unsigned(args, 0)?;
                self.bound_buffer(target)?;
                let id = object_id(args, 1)?;
                let buffer = if id == 0 { None } else { Some(self.buffers.get(&id).ok_or(glow::INVALID_OPERATION)?.0) };
                gl.bind_buffer(target, buffer);
                if target == glow::ARRAY_BUFFER { self.array_buffer = id; } else { self.element_buffer = id; }
            }
            "enableVertexAttribArray" | "disableVertexAttribArray" => {
                let location = unsigned(args, 0)?;
                if location >= gl.get_parameter_i32(glow::MAX_VERTEX_ATTRIBS) as u32 { return Err(glow::INVALID_VALUE); }
                if name == "enableVertexAttribArray" { self.enabled_attributes.insert(location); gl.enable_vertex_attrib_array(location); }
                else { self.enabled_attributes.remove(&location); gl.disable_vertex_attrib_array(location); }
            }
            "vertexAttribPointer" => {
                let location = unsigned(args, 0)?;
                let components = unsigned(args, 1)? as usize;
                let kind = unsigned(args, 2)?;
                let stride = unsigned(args, 4)? as usize;
                let offset = unsigned(args, 5)? as usize;
                let component_bytes = match kind { glow::FLOAT => 4, glow::SHORT | glow::UNSIGNED_SHORT => 2, glow::BYTE | glow::UNSIGNED_BYTE => 1, _ => return Err(glow::INVALID_ENUM) };
                if !(1..=4).contains(&components) || stride > 255 || stride % component_bytes != 0 || offset % component_bytes != 0 { return Err(glow::INVALID_VALUE); }
                if !self.buffers.contains_key(&self.array_buffer) { return Err(glow::INVALID_OPERATION); }
                self.attributes.insert(location, Attribute { buffer: self.array_buffer, components, component_bytes, stride, offset });
                gl.vertex_attrib_pointer_f32(location, components as i32, kind, args.get(3).and_then(Value::as_bool).unwrap_or(false), stride as i32, offset as i32);
            }
            "drawArrays" => {
                let first = unsigned(args, 1)? as usize;
                let count = unsigned(args, 2)? as usize;
                if count > 1_000_000 { return Err(glow::INVALID_VALUE); }
                if count > 0 { self.validate_vertices(first.checked_add(count - 1).ok_or(glow::INVALID_VALUE)?)?; }
                gl.draw_arrays(unsigned(args, 0)?, first as i32, count as i32);
            }
            "drawElements" => {
                let count = unsigned(args, 1)? as usize;
                let kind = unsigned(args, 2)?;
                let offset = unsigned(args, 3)? as usize;
                let bytes = match kind { glow::UNSIGNED_BYTE => 1, glow::UNSIGNED_SHORT => 2, _ => return Err(glow::INVALID_ENUM) };
                if count > 1_000_000 || offset % bytes != 0 { return Err(glow::INVALID_VALUE); }
                let (_, buffer) = self.buffers.get(&self.element_buffer).ok_or(glow::INVALID_OPERATION)?;
                let end = offset.checked_add(count.checked_mul(bytes).ok_or(glow::INVALID_VALUE)?).ok_or(glow::INVALID_VALUE)?;
                let indices = buffer.get(offset..end).ok_or(glow::INVALID_OPERATION)?;
                if count > 0 {
                    let last = if bytes == 1 { *indices.iter().max().unwrap() as usize }
                        else { indices.chunks_exact(2).map(|pair| u16::from_ne_bytes([pair[0], pair[1]]) as usize).max().unwrap() };
                    self.validate_vertices(last)?;
                }
                gl.draw_elements(unsigned(args, 0)?, count as i32, kind, offset as i32);
            }
            _ => return Err(glow::INVALID_OPERATION),
        }
        Ok(Value::Null)
    }
}
