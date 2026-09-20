#!/usr/bin/env python3
"""Check a fetched iframe's viewport through public Playwright/CDP operations.

Use --endpoint for a local Obscura server started with --allow-private-network,
or --chromium-bin for the reference browser. The fixture only serves loopback.
Generated evidence belongs in a new --out directory outside the source tree.
"""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import threading
from urllib.parse import parse_qs, urlparse
from playwright.sync_api import sync_playwright


class Fixture(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == '/child':
            html = """<!doctype html><style>body{margin:0}
            #box{position:fixed;inset:0;width:100vw;height:100vh}</style>
            <div id="box"></div><script>
            const r = document.getElementById('box').getBoundingClientRect();
            parent.postMessage([r.width,r.height], '*');</script>"""
        elif url.path == '/':
            query = parse_qs(url.query)
            width, height = int(query['width'][0]), int(query['height'][0])
            html = f"""<!doctype html><style>body{{margin:24px}}
            #result{{width:240px;height:24px;background:#cc2222}}
            iframe{{position:absolute;left:-10000px;width:{width}px;height:{height}px;border:0}}
            </style><h1>Fetched iframe viewport</h1><div id="result"></div>
            <pre id="report"></pre><script>
            addEventListener('message', e => {{
              window.observed = e.data;
              document.getElementById('report').textContent = JSON.stringify(e.data);
              if (JSON.stringify(e.data) === '[{width},{height}]')
                document.getElementById('result').style.background = '#22aa66';
            }});</script><iframe src="/child"></iframe>"""
        else:
            self.send_error(404)
            return
        body = html.encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    engine = parser.add_mutually_exclusive_group(required=True)
    engine.add_argument('--endpoint')
    engine.add_argument('--chromium-bin')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    results = []
    server = ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with sync_playwright() as driver:
            browser = (driver.chromium.connect_over_cdp(args.endpoint) if args.endpoint else
                       driver.chromium.launch(executable_path=args.chromium_bin, headless=True))
            try:
                for width, height in [(300, 65), (200, 90)]:
                    context = browser.new_context(viewport={'width': 800, 'height': 600})
                    try:
                        page = context.new_page()
                        page.goto(f'http://127.0.0.1:{server.server_port}/?width={width}&height={height}')
                        page.wait_for_function('Array.isArray(window.observed)', timeout=5000)
                        observed = page.evaluate('window.observed')
                        page.screenshot(path=str(args.out / f'{width}x{height}.png'))
                        results.append({'viewport': [width, height], 'observed': observed,
                                        'pass': observed == [width, height]})
                    finally:
                        context.close()
            finally:
                browser.close()
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    (args.out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    print(json.dumps(results, indent=2))
    return 0 if all(row['pass'] for row in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
