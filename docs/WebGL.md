# Experimental WebGL rendering

Build the opt-in Linux EGL/GLES backend:

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo build --release -p obscura-cli --bins --features webgl
```

The feature enables rendering and loads `libEGL.so.1` and `libGLESv2.so.2` only
when a page requests a WebGL context. Mesa's surfaceless llvmpipe backend works
without a GPU or display server. No browser process is launched. If the runtime
libraries or a usable context are unavailable, `getContext('webgl')` returns
`null`. Existing builds without the feature retain that behavior.

## Reproduction

Serve `render-repros/webgl/`, then capture the shader-drawn triangle:

```sh
python3 -m http.server 8765 --bind 127.0.0.1 --directory render-repros/webgl
obscura --allow-private-network fetch http://127.0.0.1:8765/triangle.html --screenshot triangle.png
```

The canvas is red because a real vertex/fragment program draws into its backing
surface. A successful context allocation or shader compile is not sufficient
evidence: tests inspect `readPixels` and the captured PNG.

## Initial surface

- WebGL 1 shader compilation, linking, uniforms, vertex/index buffers, and draws.
- RGBA/unsigned-byte 2D textures, including Blob-backed `ImageBitmap` decoding.
- Texture-backed color framebuffers and offscreen composition.
- Canvas resize, separate context/resource ownership, and screenshot readback.
- Coalesced readback after each script task, using the existing canvas backing
  surface and paint-damage path.

This is an experimental rendering subset, not a WebGL conformance claim.
WebGL2, extensions, depth/stencil renderbuffers, non-RGBA texture formats,
explicit mip-level uploads, context restoration, and complete WebGL validation
are not implemented. `getContext('webgl2')` returns `null`, and the extension
list is empty. The initial backend uses non-antialiased color pbuffers.

Buffer and image uploads validate byte lengths before native calls. Canvas
dimensions, contexts, shaders, buffers, and textures are bounded. HTML image
uploads use the existing credential/CORS-specific cache and do not fetch
resources synchronously.

## Tests

On a Linux machine with Mesa EGL/GLES available:

```sh
cargo nextest run --release -p obscura-js --features webgl --test webgl
```

Run the existing render and no-render suites as well. The optional backend must
not change the behavior of a build that does not enable it.
