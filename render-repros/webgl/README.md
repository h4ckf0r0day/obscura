# WebGL rendering and EGL lifetime

The optional `webgl` feature requires EGL, OpenGL ES 2, and a usable driver.
The regression suite uses Mesa software rendering and does not require a display.

On Ubuntu 22.04, install the runtime packages:

```sh
sudo apt-get install libegl1 libegl-mesa0 libgles2 libgl1-mesa-dri
```

Run the rendering and lifetime regressions:

```sh
LIBGL_ALWAYS_SOFTWARE=1 cargo nextest run --release \
  -p obscura-js --features webgl --test webgl --no-fail-fast
```

## Minimal thread-exit regression

```sh
LIBGL_ALWAYS_SOFTWARE=1 cargo nextest run --release \
  -p obscura-js --features webgl --test webgl \
  -E 'test(context_cleanup_survives_owner_thread_exit_and_reinitialization)'
```

The test creates a runtime on an owner thread, clears a canvas, verifies its
pixels, drops the runtime, and joins the thread. It repeats this on three
successive threads to cover both cleanup and reuse of the shared display.

With Ubuntu 22.04's Mesa 23.2.1 and GLVND 1.4.0, releasing the last display
previously unloaded `libEGL.so.1` before the owner thread exited. Its thread-local
cleanup callback then pointed into unmapped code and caused `SIGSEGV`.
GDB identifies the crash address inside the library's earlier executable mapping.

The loader cache retains one pair of EGL/GLES library handles for the process
lifetime so callbacks stay mapped, including after failed initialization.
Pages still destroy their own EGL contexts, pbuffer surfaces, and displays.
Failed library or display initialization is not cached and can be retried.
`unavailable_egl_backend_returns_null_and_exits_cleanly` also covers a missing
EGL vendor and repeated context requests without crashing on thread exit.

`triangle.html` separately exercises real shader compilation and canvas pixels.
