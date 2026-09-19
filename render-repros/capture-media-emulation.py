#!/usr/bin/env python3
"""Capture the reduced-motion fixture through public Playwright/CDP operations.

Requires Playwright and Pillow, like the existing rendering harness. Run with
--endpoint http://127.0.0.1:9222 for Obscura, or --chromium-bin PATH for Chrome.
The output directory must not already exist. A failing upstream run is useful
before evidence; it is not a passing regression gate.
"""
import argparse
import io
import json
from pathlib import Path
from urllib.parse import quote
from PIL import Image
from playwright.sync_api import sync_playwright


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    engine = parser.add_mutually_exclusive_group(required=True)
    engine.add_argument('--endpoint')
    engine.add_argument('--chromium-bin')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    fixture = Path(__file__).with_name('reduced-motion-emulation.html').read_text()
    url = 'data:text/html,' + quote(fixture)
    results = []
    with sync_playwright() as driver:
        browser = (driver.chromium.connect_over_cdp(args.endpoint) if args.endpoint else
                   driver.chromium.launch(executable_path=args.chromium_bin, headless=True))
        context = browser.new_context(viewport={'width': 640, 'height': 480}, device_scale_factor=1)
        page = context.new_page()
        page.goto(url)
        for mode, expected in [('no-preference', 100), ('reduce', 40), ('no-preference', 100)]:
            page.emulate_media(reduced_motion=mode)
            observed = page.evaluate("""() => ({width: document.querySelector('#probe').getBoundingClientRect().width,
                reduced: matchMedia('(prefers-reduced-motion: reduce)').matches})""")
            png = page.screenshot(path=str(args.out / f'{len(results)}-{mode}.png'))
            image = Image.open(io.BytesIO(png)).convert('RGB')
            green = [(x, y) for y in range(image.height) for x in range(image.width)
                     if image.getpixel((x, y)) == (34, 170, 102)]
            width = max(x for x, _ in green) - min(x for x, _ in green) + 1 if green else 0
            passed = observed == {'width': expected, 'reduced': mode == 'reduce'} and width == expected
            results.append({'mode': mode, 'observed': observed, 'painted_width': width, 'pass': passed})
            if mode == 'reduce':
                page.screenshot(path=str(args.out / 'reduced-full-page.png'), full_page=True)
                page.screenshot(path=str(args.out / 'reduced-clip.png'), clip={'x': 0, 'y': 60, 'width': 200, 'height': 180})
                page.evaluate('window.scrollTo(0, 200)')
                page.screenshot(path=str(args.out / 'reduced-scrolled.png'))
                page.evaluate('window.scrollTo(0, 0)')
                pdf = page.pdf(width='640px', height='480px', print_background=True,
                               margin={'top': '0', 'right': '0', 'bottom': '0', 'left': '0'}, page_ranges='1')
                (args.out / 'reduced.pdf').write_bytes(pdf)
                results.append({'operation': 'PDF and preference restoration',
                                'pass': pdf.startswith(b'%PDF-') and page.evaluate("document.querySelector('#probe').getBoundingClientRect().width") == expected})
        context.close()
        scaled = browser.new_context(viewport={'width': 480, 'height': 320}, device_scale_factor=2,
                                     reduced_motion='reduce')
        page = scaled.new_page()
        page.goto(url)
        png = page.screenshot(path=str(args.out / 'reduced-dpr2.png'))
        dimensions = list(Image.open(io.BytesIO(png)).size)
        results.append({'operation': 'viewport and DPR', 'dimensions': dimensions,
                        'pass': dimensions == [960, 640] and page.evaluate("document.querySelector('#probe').getBoundingClientRect().width") == 40})
        scaled.close()
        browser.close()
    (args.out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    print(json.dumps(results, indent=2))
    return 0 if all(row['pass'] for row in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
