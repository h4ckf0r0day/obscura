// Regression test for issue #678: sessionStorage belongs to a browsing
// context and an origin. An entry must survive a same-origin navigation in
// the same tab, and another target navigating or running a script must not
// wipe the first tab's store.
//
// Run after building the release binary:
//   node crates/obscura-cdp/tests/session_storage.mjs
//
// Node 22+ (uses the global WebSocket). No packages needed.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { access } from 'node:fs/promises';
import http from 'node:http';
import net from 'node:net';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const obscuraBin = process.env.OBSCURA_BIN || join(
  root, 'target', 'release', process.platform === 'win32' ? 'obscura.exe' : 'obscura',
);

await access(obscuraBin).catch(() => {
  throw new Error(`Build Obscura first: cargo build --release -p obscura-cli\nMissing: ${obscuraBin}`);
});

async function listen(server) {
  await new Promise((done, fail) => {
    server.once('error', fail);
    server.listen(0, '127.0.0.1', done);
  });
  return `http://127.0.0.1:${server.address().port}`;
}

async function freePort() {
  const server = net.createServer();
  await new Promise((done, fail) => {
    server.once('error', fail);
    server.listen(0, '127.0.0.1', done);
  });
  const port = server.address().port;
  await new Promise(done => server.close(done));
  return port;
}

async function waitForServer(child, port) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (child.exitCode !== null) throw new Error(`Obscura stopped with code ${child.exitCode}`);
    try {
      if ((await fetch(`http://127.0.0.1:${port}/json/version`)).ok) return;
    } catch {}
    await new Promise(done => setTimeout(done, 100));
  }
  throw new Error('Obscura did not become ready');
}

const web = http.createServer((_request, response) => {
  response.writeHead(200, { 'content-type': 'text/html' });
  response.end('<!doctype html><title>session storage test</title><body>ok</body>');
});
const otherWeb = http.createServer((_request, response) => {
  response.writeHead(200, { 'content-type': 'text/html' });
  response.end('<!doctype html><title>second origin</title><body>ok</body>');
});
const originA = await listen(web);
const originB = await listen(otherWeb);
const cdpPort = await freePort();
const obscura = spawn(
  obscuraBin,
  ['--allow-private-network', 'serve', '--port', String(cdpPort)],
  { cwd: root, stdio: 'ignore', windowsHide: true },
);

await waitForServer(obscura, cdpPort);
const { webSocketDebuggerUrl } = await (await fetch(`http://127.0.0.1:${cdpPort}/json/version`)).json();
const ws = new WebSocket(webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));

let id = 0;
const pending = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  const p = pending.get(m.id);
  if (!p) return;
  pending.delete(m.id);
  m.error ? p.rej(new Error(m.error.message)) : p.res(m.result);
};
const send = (method, params = {}, sessionId) =>
  new Promise((res, rej) => {
    const i = ++id;
    pending.set(i, { res, rej });
    ws.send(JSON.stringify({ id: i, method, params, ...(sessionId ? { sessionId } : {}) }));
  });

async function open() {
  const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
  await send('Page.enable', {}, sessionId);
  await send('Runtime.enable', {}, sessionId);
  return sessionId;
}

const evaluate = async (sessionId, expression) =>
  (await send('Runtime.evaluate', { expression, returnByValue: true }, sessionId)).result?.value;

async function goto(sessionId, url) {
  await send('Page.navigate', { url }, sessionId);
  const want = new URL(url).origin;
  let last = null;
  for (let i = 0; i < 200; i++) {
    last = JSON.parse(await evaluate(sessionId, 'JSON.stringify([location.origin, document.readyState])'));
    if (last[0] === want && last[1] === 'complete') return;
    await new Promise(r => setTimeout(r, 100));
  }
  throw new Error(`navigation to ${url} did not settle: ${JSON.stringify(last)}`);
}

const STORE = 'sessionStorage';
const write = (key, value) =>
  `(() => { ${STORE}.setItem(${JSON.stringify(key)}, ${JSON.stringify(value)}); return ${STORE}.getItem(${JSON.stringify(key)}); })()`;
const read = (key) => `${STORE}.getItem(${JSON.stringify(key)})`;

let session;
try {
  // Part 1: the entry survives a same-origin navigation in the same tab.
  session = await open();
  await goto(session, originA);
  assert.equal(await evaluate(session, write('k', 'value')), 'value');
  assert.equal(await evaluate(session, read('k')), 'value');
  await goto(session, originA);
  assert.equal(await evaluate(session, read('k')), 'value', 'entry must survive a same-origin navigation');

  // Part 2: another target must not wipe the store of the first tab,
  // whether it navigates (any origin) or only runs a script.
  const t1 = await open();
  await goto(t1, originA);
  assert.equal(await evaluate(t1, write('t1_k', 't1_value')), 't1_value');
  const t2 = await open();
  assert.equal(await evaluate(t1, read('t1_k')), 't1_value');
  await goto(t2, originB);
  assert.equal(await evaluate(t1, read('t1_k')), 't1_value', 'entry must survive a navigation in another tab');
  const t3 = await open();
  await goto(t3, originA);
  assert.equal(await evaluate(t1, write('t1_k', 't1_value')), 't1_value');
  await evaluate(t3, 'String(1 + 1)');
  assert.equal(await evaluate(t1, read('t1_k')), 't1_value', 'entry must survive a Runtime.evaluate in another tab');

  // Part 3: tabs are isolated from each other. A fresh tab on the same
  // origin starts with an empty session store.
  assert.equal(await evaluate(t3, read('t1_k')), null, 'sessionStorage must not cross tabs');
} finally {
  ws.close();
  obscura.kill();
  web.close();
  otherWeb.close();
}

console.log(JSON.stringify({ ok: true }));
