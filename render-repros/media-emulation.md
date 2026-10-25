# Reduced-motion emulation reproduction

`reduced-motion-emulation.html` participates in the default render suite. Its
normal-motion green bar must measure 100×30 CSS pixels. The public CDP probe
also selects reduced motion, where both geometry and painted width must be 40.

With the existing rendering harness's Python dependencies (Playwright and
Pillow), start a render-enabled Obscura CDP server and run:

```sh
obscura serve --host 127.0.0.1 --port 9222
python3 render-repros/capture-media-emulation.py \
  --endpoint http://127.0.0.1:9222 --out /tmp/media-obscura
python3 render-repros/capture-media-emulation.py \
  --chromium-bin /path/to/chrome --out /tmp/media-chrome
```

Use separate, initially absent output directories for old/new engine revisions.
The fixture uses a data URL and needs no external website. The probe records
normal → reduced → normal state, the actual green pixels in screenshots,
clipped/full-page/scrolled captures, a different viewport at DPR 2, and PDF
export followed by verification that the screen preference remains selected.
`1-reduce.png` is the useful before/after comparison; the returned
`painted_width` distinguishes an accepted command from changed rendering.
The JSON report and process status identify failed assertions. These checks
cover this small fixture, not all print, media-feature or browser behavior.
