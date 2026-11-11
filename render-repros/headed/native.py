#!/usr/bin/env python3
"""Verify OS input and rendered pixels in a native window on Linux/X11."""
import argparse
import json
import os
from pathlib import Path
import sys
import time
import urllib.parse

sys.dont_write_bytecode = True
from repro import Session
from x11 import Desktop


def wait_for(probe, expected=True, timeout=10):
    deadline = time.monotonic() + timeout
    latest = None
    while time.monotonic() < deadline:
        latest = probe()
        if latest == expected:
            return latest
        time.sleep(0.05)
    raise AssertionError(f'Expected {expected!r}, received {latest!r}')


def evaluate(session, expression):
    return session.call('browser_evaluate', dict(expression=expression))['content'][0]['text']


def point(session, desktop, window, selector):
    expression = f'''JSON.stringify((()=>{{const box=document.querySelector({json.dumps(selector)}).getBoundingClientRect();return [(box.left+box.width/2)/innerWidth,(box.top+box.height/2)/innerHeight]}})())'''
    fractions = json.loads(evaluate(session, expression))
    _, _, width, height = desktop.geometry(window)
    return fractions[0] * width, fractions[1] * height


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default=os.environ.get('OBSCURA_BIN', 'obscura'))
    parser.add_argument('--watch', action='store_true')
    parser.add_argument('--url', help='Observe a real page instead of running synthetic input checks')
    arguments = parser.parse_args()
    os.environ['WINIT_UNIX_BACKEND'] = 'x11'
    desktop = Desktop()
    session = Session(arguments.binary, is_native=True)
    process_id = None
    try:
        session.request('initialize', dict(protocolVersion='2024-11-05', capabilities={}, clientInfo=dict(name='native-repro', version='1')))
        metadata = json.loads(session.call('browser_viewer')['content'][0]['text'])
        assert metadata['mode'] == 'native'
        process_id = metadata['pid']
        window = desktop.find(process_id)
        assert window, 'Native OS window was not created'
        fixture = Path(__file__).with_name('counter.html').read_text()
        url = arguments.url or 'data:text/html,' + urllib.parse.quote(fixture)
        session.call('browser_navigate', dict(url=url))
        print('Native window process:', process_id, flush=True)
        if arguments.url:
            assert arguments.watch, '--url requires --watch'
        else:
            color_point = point(session, desktop, window, '#color')
            wait_for(lambda: desktop.pixel(window, color_point), 0xb83535)
            desktop.click(window, point(session, desktop, window, '#counter'))
            wait_for(lambda: evaluate(session, "document.querySelector('#counter').textContent"), 'Count: 1')
            wait_for(lambda: desktop.pixel(window, color_point), 0x287e47)
            session.call('browser_click', dict(selector='#counter'))
            wait_for(lambda: desktop.pixel(window, color_point), 0xb83535)
            print('PASS: OS click reaches MCP state; MCP click changes native pixels', flush=True)

            desktop.click(window, point(session, desktop, window, '#note'))
            desktop.type_text('Obscura42')
            wait_for(lambda: evaluate(session, "document.querySelector('#note').value"), 'Obscura42')
            desktop.key('BackSpace', True)
            desktop.key('BackSpace', False)
            wait_for(lambda: evaluate(session, "document.querySelector('#note').value"), 'Obscura4')
            desktop.scroll()
            wait_for(lambda: evaluate(session, 'scrollY > 0'), 'true')
            print('PASS: native keyboard, editing, and wheel scrolling', flush=True)

            before = evaluate(session, 'innerWidth')
            desktop.resize(window, 900, 600)
            wait_for(lambda: evaluate(session, 'innerWidth') != before)
            print('PASS: native resize updates the MCP viewport', flush=True)
        if arguments.watch:
            print('Interact with the Obscura application window. Ctrl+C stops the MCP session.', flush=True)
            while True:
                time.sleep(1)
        desktop.close_window(window)
        wait_for(lambda: desktop.find(process_id) is None)
        assert evaluate(session, 'document.title')
        print('PASS: closing the window leaves MCP operational', flush=True)
    except KeyboardInterrupt:
        pass
    finally:
        session.close()
    if process_id:
        wait_for(lambda: desktop.find(process_id) is None)
    print('PASS: clean JSON stdout and native-window cleanup', flush=True)


if __name__ == '__main__':
    main()
