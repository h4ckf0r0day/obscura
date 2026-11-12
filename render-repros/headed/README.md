# Native headed MCP reproduction

Build the desktop feature:

```sh
cargo build --release -p obscura-cli --bins --features headed
```

`counter.html` is a self-contained counter, text input, color swatch, and scroll
fixture. No external site or browser is needed.

## Native OS input and pixels (Linux/X11)

The driver uses only Python's standard library plus the desktop's `libX11` and
`libXtst`. X11 runtime libraries include `libxkbcommon-x11` and `libxcb-xkb`
(on Ubuntu: `libxkbcommon-x11-0`, `libxcb-xkb1`). A display is required, either
WSLg/X11 or an isolated Xvfb session:

```sh
OBSCURA_BIN=/path/to/obscura python3 render-repros/headed/native.py
# Alternatively, for CI:
xvfb-run -a env OBSCURA_BIN=/path/to/obscura python3 render-repros/headed/native.py
```

The test opens a real native window, finds it by its process id, sends OS mouse
and keyboard events, and checks their effects through MCP. It also reads the
native window's pixels after an MCP click, checks Backspace, wheel scrolling,
resize, window closing, and clean JSON stdout. Closing the window leaves MCP
operational; ending the owning session removes the window.

To keep the fixture open for manual interaction:

```sh
OBSCURA_BIN=/path/to/obscura python3 render-repros/headed/native.py --watch
```

`--watch --url https://example.com` observes a real page without synthetic input.
This driver forces X11 for deterministic OS-event injection. Normal
`obscura mcp --headed` can also use Wayland and does not force X11.

## Read-only web viewer

```sh
OBSCURA_BIN=/path/to/obscura python3 render-repros/headed/repro.py
OBSCURA_BIN=/path/to/obscura python3 render-repros/headed/repro.py --watch
```

This driver starts `--web-viewer`, checks same-session clicks and tab changes,
fragmented stdin, autonomous timers, ready-file discovery, and shutdown on EOF.
The printed URL belongs to that separate read-only web mode. Render builds
without the desktop feature can run this driver.

On the baseline revision, neither display mode exists. Starting a separate CDP
server does not expose the page owned by this MCP process.
