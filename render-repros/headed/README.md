# Headed MCP reproduction

Use a render-enabled build. No external site, display server, Python packages,
Chrome, or CDP client is required.

```sh
OBSCURA_BIN=/path/to/obscura python3 render-repros/headed/repro.py
```

The script starts a stdio MCP process with `--headed`, loads `counter.html`
through a data URL, and compares PNG frames before and after an MCP click.
It also checks tab switching, active-tab closing, empty sessions, ready-file
discovery, clean JSON stdout, and viewer shutdown on stdin EOF.

To watch the counter change, open the URL printed by:

```sh
OBSCURA_BIN=/path/to/obscura python3 render-repros/headed/repro.py --watch
```

The viewer is read-only. The script clicks through MCP every two seconds.
Pause and Resume affect only capture requests, not the running page. A custom
page can be observed with `--watch --url https://example.com`; that mode does
not synthesize counter clicks.

On the baseline revision, `--headed` is rejected. Starting a separate CDP
server does not expose the page owned by this MCP process.
