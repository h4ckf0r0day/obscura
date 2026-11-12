#!/usr/bin/env python3
"""Verify headed MCP using only Python's standard library; --watch keeps it live."""
import argparse
import base64
import json
import os
from pathlib import Path
import selectors
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


class Session:
    def __init__(self, binary, ready_file=None, is_native=False):
        self.errors = tempfile.TemporaryFile()
        arguments = ['mcp', '--headed'] if is_native else ['mcp', '--web-viewer', '--viewer-ready-file', str(ready_file)]
        self.process = subprocess.Popen(
            [binary, *arguments],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errors,
            start_new_session=True,
        )
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)
        self.pending = bytearray()
        self.request_id = 0

    def request(self, method, params, interrupt_with_frame=None):
        self.request_id += 1
        message = dict(jsonrpc='2.0', id=self.request_id, method=method, params=params)
        payload = (json.dumps(message) + '\n').encode()
        if interrupt_with_frame:
            split = len(payload) // 2
            self.process.stdin.write(payload[:split])
            self.process.stdin.flush()
            frame(interrupt_with_frame)
            payload = payload[split:]
        self.process.stdin.write(payload)
        self.process.stdin.flush()
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if b'\n' not in self.pending:
                if not self.selector.select(max(0, deadline - time.monotonic())):
                    raise TimeoutError(method)
                chunk = os.read(self.process.stdout.fileno(), 65536)
                if not chunk:
                    self.errors.seek(0)
                    raise RuntimeError(self.errors.read().decode())
                self.pending.extend(chunk)
                continue
            line, _, remaining = self.pending.partition(b'\n')
            self.pending[:] = remaining
            response = json.loads(line)
            assert response.get('id') == self.request_id, response
            assert 'error' not in response, response
            result = response['result']
            assert not result.get('isError'), result
            return result
        raise TimeoutError(method)

    def call(self, name, arguments=None):
        return self.request('tools/call', dict(name=name, arguments=arguments or {}))

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.selector.close()
        self.errors.close()


def frame(url):
    with urllib.request.urlopen(url + 'frame', timeout=15) as response:
        return json.load(response)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default=os.environ.get('OBSCURA_BIN', 'obscura'))
    parser.add_argument('--watch', action='store_true')
    parser.add_argument('--url', help='Optional real page instead of the counter')
    args = parser.parse_args()
    fixture = Path(__file__).with_name('counter.html').read_text()
    url = args.url or 'data:text/html,' + urllib.parse.quote(fixture)
    with tempfile.TemporaryDirectory(prefix='obscura-headed-') as directory:
        ready_file = Path(directory) / 'viewer.url'
        session = Session(args.binary, ready_file)
        viewer_url = None
        try:
            session.request('initialize', dict(
                protocolVersion='2024-11-05', capabilities={},
                clientInfo=dict(name='headed-repro', version='1'),
            ))
            viewer_url = session.call('browser_viewer')['content'][0]['text']
            assert ready_file.read_text() == viewer_url
            assert frame(viewer_url)['image'] is None
            session.request('ping', {}, interrupt_with_frame=viewer_url)
            session.call('browser_navigate', dict(url=url))
            before = frame(viewer_url)
            assert base64.b64decode(before['image']).startswith(b'\x89PNG\r\n\x1a\n')
            print('Viewer:', viewer_url, flush=True)
            if args.url:
                assert args.watch, '--url is for interactive --watch sessions'
            else:
                session.call('browser_click', dict(selector='#counter'))
                after = frame(viewer_url)
                assert before['image'] != after['image']
                assert 'Count: 1' in session.call('browser_snapshot')['content'][0]['text']
                session.call('browser_tab_new', dict(url='data:text/html,second'))
                assert frame(viewer_url)['tabId'] != before['tabId']
                session.call('browser_tab_switch', dict(tab_id=before['tabId']))
                assert frame(viewer_url)['image'] == after['image']
                session.call('browser_evaluate', dict(expression='window.timerFinished=false; setTimeout(()=>window.timerFinished=true, 30); true'))
                time.sleep(0.1)
                frame(viewer_url)
                assert session.call('browser_evaluate', dict(expression='window.timerFinished'))['content'][0]['text'] == 'true'
                session.call('browser_tab_close', dict(tab_id=before['tabId']))
                assert frame(viewer_url)['tabId'] != before['tabId']
                print('PASS: MCP click, tab switch, tab close, and PNG capture', flush=True)
                session.call('browser_close')
                assert frame(viewer_url)['image'] is None
                session.call('browser_navigate', dict(url=url))
            if args.watch:
                print('Ctrl+C to stop. Counter clicks happen through MCP every two seconds.', flush=True)
                while True:
                    time.sleep(2)
                    if not args.url:
                        session.call('browser_click', dict(selector='#counter'))
            session.call('browser_close')
        except KeyboardInterrupt:
            pass
        finally:
            session.close()
        assert not ready_file.exists(), 'Ready file must disappear on stdio EOF'
        if viewer_url:
            try:
                urllib.request.urlopen(viewer_url, timeout=2)
            except urllib.error.URLError:
                pass
            else:
                raise AssertionError('Viewer survived MCP shutdown')
        print('PASS: MCP stdout stayed JSON; viewer stopped on EOF', flush=True)


if __name__ == '__main__':
    main()
