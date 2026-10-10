#!/usr/bin/env python3
"""Record raw Chromium CDP semantics for the long full-page capture fixture."""

import argparse
import base64
import io
import json
from pathlib import Path

from PIL import Image
from playwright.sync_api import sync_playwright


def rgb(image, x, y):
    return list(image.getpixel((x, y)))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--fixture",
        type=Path,
        default=Path(__file__).with_name("long-full-page-capture.html"),
    )
    parser.add_argument("--output", type=Path)
    parser.add_argument("--executable")
    args = parser.parse_args()

    launch = {"headless": True}
    if args.executable:
        launch["executable_path"] = args.executable

    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(**launch)
        context = browser.new_context(
            viewport={"width": 1000, "height": 700},
            device_scale_factor=1,
            locale="en-US",
            timezone_id="UTC",
            color_scheme="light",
        )
        page = context.new_page()
        page.goto(args.fixture.resolve().as_uri(), wait_until="load", timeout=30_000)
        page.evaluate("window.scrollTo(0, 8000)")
        before_scroll = page.evaluate("[window.scrollX, window.scrollY]")
        cdp = context.new_cdp_session(page)
        result = cdp.send(
            "Page.captureScreenshot",
            {"format": "png", "captureBeyondViewport": True},
        )
        after_scroll = page.evaluate("[window.scrollX, window.scrollY]")
        with Image.open(io.BytesIO(base64.b64decode(result["data"]))) as opened:
            image = opened.convert("RGBA")

        report = {
            "browser": "chromium",
            "version": browser.version,
            "cdp_method": "Page.captureScreenshot",
            "parameters": {"format": "png", "captureBeyondViewport": True},
            "viewport": [1000, 700],
            "device_scale_factor": 1,
            "scroll_before": before_scroll,
            "scroll_after": after_scroll,
            "dimensions": list(image.size),
            "pixels": {
                "top": rgb(image, 500, 100),
                "middle": rgb(image, 500, 8500),
                "bottom": rgb(image, 500, 16975),
                "fixed_at_live_scroll": rgb(image, 10, 8015),
                "full_page_origin": rgb(image, 10, 15),
            },
            "boundary_triplets": {
                str(boundary): [rgb(image, 900, y) for y in range(boundary - 1, boundary + 2)]
                for boundary in (4096, 8192, 12288, 16384)
            },
        }
        serialized = json.dumps(report, indent=2, sort_keys=True)
        if args.output:
            args.output.write_text(serialized + "\n", encoding="utf-8")
        print(serialized)
        context.close()
        browser.close()


if __name__ == "__main__":
    main()
