#![cfg(feature = "webgl")]

use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

fn runtime() -> ObscuraJsRuntime {
    let mut runtime = ObscuraJsRuntime::new();
    runtime.set_dom(obscura_dom::parse_html("<body style='margin:0'><canvas width='16' height='16'></canvas></body>"));
    runtime.set_url("http://example.test/");
    runtime.set_viewport(1280.0, 720.0);
    runtime.run_page_init();
    runtime.evaluate(r#"(() => { globalThis.canvas = document.querySelector('canvas');
        globalThis.gl = canvas.getContext('webgl', {premultipliedAlpha:false});
        if (!gl) throw new Error('WebGL tests require Mesa EGL/GLES2');
        globalThis.pixel = () => {const pixel = new Uint8Array(4); gl.readPixels(8,8,1,1,gl.RGBA,gl.UNSIGNED_BYTE,pixel); return [...pixel];};
        globalThis.program = () => {
          const shaders = [
            [gl.VERTEX_SHADER, 'attribute vec2 position; varying vec2 uv; void main(){uv=(position+1.0)*0.5;gl_Position=vec4(position,0.,1.);}'],
            [gl.FRAGMENT_SHADER, globalThis.fragment || 'precision mediump float;void main(){gl_FragColor=vec4(1.,0.,0.,1.);}']
          ].map(([kind, source]) => {const shader=gl.createShader(kind);gl.shaderSource(shader,source);gl.compileShader(shader);if(!gl.getShaderParameter(shader,gl.COMPILE_STATUS))throw new Error(gl.getShaderInfoLog(shader));return shader;});
          const program=gl.createProgram();shaders.forEach(shader=>gl.attachShader(program,shader));gl.linkProgram(program);
          if(!gl.getProgramParameter(program,gl.LINK_STATUS))throw new Error(gl.getProgramInfoLog(program));
          gl.useProgram(program);const buffer=gl.createBuffer();gl.bindBuffer(gl.ARRAY_BUFFER,buffer);
          gl.bufferData(gl.ARRAY_BUFFER,new Float32Array([-1,-1,3,-1,-1,3]),gl.STATIC_DRAW);
          const position=gl.getAttribLocation(program,'position');gl.enableVertexAttribArray(position);gl.vertexAttribPointer(position,2,gl.FLOAT,false,0,0);
          return program;
        }; })()"#).unwrap();
    runtime
}

#[test]
fn context_reads_real_clear_pixels_and_preserves_context_mode() {
    let mut runtime = runtime();
    assert_eq!(runtime.evaluate(r#"(() => {
        gl.clearColor(0.25,0.5,0.75,1); gl.clear(gl.COLOR_BUFFER_BIT);
        return [pixel(), canvas.getContext('webgl')===gl, canvas.getContext('2d'), canvas.getContext('webgl2'), gl instanceof WebGLRenderingContext, gl.getError()];
    })()"#).unwrap(), json!([[64,128,191,255],true,null,null,true,0]));
}

#[test]
fn shader_triangle_produces_pixels() {
    let mut runtime = runtime();
    assert_eq!(runtime.evaluate("(() => {program();gl.drawArrays(gl.TRIANGLES,0,3);return [pixel(),gl.getError()];})()").unwrap(), json!([[255,0,0,255],0]));
}

#[test]
fn invalid_shader_reports_compiler_failure() {
    let mut runtime = runtime();
    assert_eq!(runtime.evaluate(r#"(() => {
        const shader=gl.createShader(gl.FRAGMENT_SHADER);
        gl.shaderSource(shader,'not a shader');gl.compileShader(shader);
        return [gl.getShaderParameter(shader,gl.COMPILE_STATUS),gl.getShaderInfoLog(shader).length>0];
    })()"#).unwrap(), json!([false,true]));
}

#[test]
fn textured_triangle_uses_uploaded_rgba() {
    let mut runtime = runtime();
    assert_eq!(runtime.evaluate(r#"(() => {
        globalThis.fragment='precision mediump float;varying vec2 uv;uniform sampler2D image;void main(){gl_FragColor=texture2D(image,uv);}';
        const pipeline=program();const texture=gl.createTexture();gl.bindTexture(gl.TEXTURE_2D,texture);
        gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_MIN_FILTER,gl.NEAREST);
        gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_MAG_FILTER,gl.NEAREST);
        gl.texImage2D(gl.TEXTURE_2D,0,gl.RGBA,1,1,0,gl.RGBA,gl.UNSIGNED_BYTE,new Uint8Array([10,220,30,255]));
        gl.uniform1i(gl.getUniformLocation(pipeline,'image'),0);gl.drawArrays(gl.TRIANGLES,0,3);
        return [pixel(),gl.getError()];
    })()"#).unwrap(), json!([[10,220,30,255],0]));
}

#[test]
fn invalid_buffer_and_texture_lengths_reject_without_native_reads() {
    let mut runtime = runtime();
    assert_eq!(runtime.evaluate(r#"(() => {
        program();gl.bufferData(gl.ARRAY_BUFFER,new Float32Array([0,0]),gl.STATIC_DRAW);
        gl.drawArrays(gl.TRIANGLES,0,3);const drawError=gl.getError();
        gl.bindTexture(gl.TEXTURE_2D,gl.createTexture());
        gl.texImage2D(gl.TEXTURE_2D,0,gl.RGBA,16,16,0,gl.RGBA,gl.UNSIGNED_BYTE,new Uint8Array(4));
        return [drawError,gl.getError()];
    })()"#).unwrap(), json!([1282,1282]));
}

#[test]
fn separate_contexts_do_not_share_drawing_buffers_or_objects() {
    let mut runtime = runtime();
    assert_eq!(runtime.evaluate(r#"(() => {
        const other=document.createElement('canvas');other.width=other.height=16;
        const second=other.getContext('webgl',{premultipliedAlpha:false});
        gl.clearColor(1,0,0,1);gl.clear(gl.COLOR_BUFFER_BIT);
        second.clearColor(0,1,0,1);second.clear(second.COLOR_BUFFER_BIT);
        second.bindBuffer(second.ARRAY_BUFFER,gl.createBuffer());
        const secondPixel=new Uint8Array(4);second.readPixels(8,8,1,1,second.RGBA,second.UNSIGNED_BYTE,secondPixel);
        return [pixel(),[...secondPixel],second.getError()];
    })()"#).unwrap(), json!([[255,0,0,255],[0,255,0,255],1282]));
}

#[test]
fn canvas_resize_reallocates_and_bounds_drawing_buffers() {
    let mut runtime = runtime();
    assert_eq!(runtime.evaluate(r#"(() => {
        canvas.width=32;canvas.height=24;gl.viewport(0,0,32,24);
        gl.clearColor(0,0,1,1);gl.clear(gl.COLOR_BUFFER_BIT);
        const huge=document.createElement('canvas');huge.width=100000;
        return [gl.drawingBufferWidth,gl.drawingBufferHeight,pixel(),huge.getContext('webgl')];
    })()"#).unwrap(), json!([32,24,[0,0,255,255],null]));
}

#[test]
fn framebuffer_rendering_is_separate_from_the_canvas() {
    let mut runtime = runtime();
    assert_eq!(runtime.evaluate(r#"(() => {
        gl.clearColor(1,0,0,1);gl.clear(gl.COLOR_BUFFER_BIT);
        const texture=gl.createTexture();gl.bindTexture(gl.TEXTURE_2D,texture);
        gl.texImage2D(gl.TEXTURE_2D,0,gl.RGBA,16,16,0,gl.RGBA,gl.UNSIGNED_BYTE,null);
        const framebuffer=gl.createFramebuffer();gl.bindFramebuffer(gl.FRAMEBUFFER,framebuffer);
        gl.framebufferTexture2D(gl.FRAMEBUFFER,gl.COLOR_ATTACHMENT0,gl.TEXTURE_2D,texture,0);
        const complete=gl.checkFramebufferStatus(gl.FRAMEBUFFER)===gl.FRAMEBUFFER_COMPLETE;
        gl.clearColor(0,1,0,1);gl.clear(gl.COLOR_BUFFER_BIT);const offscreen=pixel();
        gl.bindFramebuffer(gl.FRAMEBUFFER,null);
        return [complete,offscreen,pixel(),gl.getError()];
    })()"#).unwrap(), json!([true,[0,255,0,255],[255,0,0,255],0]));
}

#[tokio::test]
async fn webgl_pixels_reach_the_screenshot_surface() {
    let mut runtime = runtime();
    runtime.evaluate("(() => {program();gl.drawArrays(gl.TRIANGLES,0,3);})()").unwrap();
    runtime.run_event_loop_bounded(20).await.unwrap();
    runtime.evaluate("canvas.getBoundingClientRect().width").unwrap();
    let png = runtime.screenshot_prepared((1280.0, 720.0), Some("http://example.test/")).expect("prepared screenshot");
    let pixels = image::load_from_memory(&png).unwrap().into_rgba8();
    assert_eq!(pixels.get_pixel(8, 8).0, [255, 0, 0, 255]);
}

#[tokio::test]
async fn blob_image_bitmap_decodes_and_uploads_real_pixels() {
    use base64::Engine;
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(1, 1, image::Rgba([25, 190, 80, 255]))
        .write_to(&mut encoded, image::ImageFormat::Png).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(encoded.into_inner());
    let mut runtime = runtime();
    runtime.evaluate(&format!(r#"(() => {{
        globalThis.fragment='precision mediump float;varying vec2 uv;uniform sampler2D image;void main(){{gl_FragColor=texture2D(image,uv);}}';
        const pipeline=program();
        const bytes=Uint8Array.from(atob('{encoded}'),character=>character.charCodeAt(0));
        createImageBitmap(new Blob([bytes],{{type:'image/png'}}),{{imageOrientation:'flipY',premultiplyAlpha:'none'}}).then(bitmap=>{{
            gl.bindTexture(gl.TEXTURE_2D,gl.createTexture());
            gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_MIN_FILTER,gl.NEAREST);
            gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_MAG_FILTER,gl.NEAREST);
            gl.texImage2D(gl.TEXTURE_2D,0,gl.RGBA,gl.RGBA,gl.UNSIGNED_BYTE,bitmap);
            gl.uniform1i(gl.getUniformLocation(pipeline,'image'),0);
            gl.drawArrays(gl.TRIANGLES,0,3);globalThis.bitmapPixel=pixel();bitmap.close();
        }}).catch(error=>globalThis.bitmapError=String(error));
    }})()"#)).unwrap();
    runtime.run_event_loop_bounded(40).await.unwrap();
    assert_eq!(runtime.evaluate("globalThis.bitmapError || globalThis.bitmapPixel").unwrap(), json!([25,190,80,255]));
}
