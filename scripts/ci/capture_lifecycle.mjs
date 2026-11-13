import assert from 'node:assert/strict';
import {fork, execFileSync} from 'node:child_process';
import {readFileSync, writeFileSync, mkdirSync, existsSync, openSync, closeSync} from 'node:fs';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const pngHeader = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

async function client(endpoint, specification, output) {
  const test = JSON.parse(specification);
  const result = {url: test.url, expectedTitle: test.title, phase: 'connect', ok: false};
  let socket;
  const pending = new Map();
  let serial = 0;
  try {
    const version = await (await fetch(endpoint + '/json/version', {signal: AbortSignal.timeout(5000)})).json();
    socket = new WebSocket(version.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(Error('WebSocket open timeout')), 5000);
      socket.addEventListener('open', () => {clearTimeout(timer); resolve();}, {once: true});
      socket.addEventListener('error', () => {clearTimeout(timer); reject(Error('WebSocket open failed'));}, {once: true});
    });
    socket.addEventListener('message', event => {
      const response = JSON.parse(event.data), request = pending.get(response.id);
      if (!request) return;
      pending.delete(response.id); clearTimeout(request.timer);
      response.error ? request.reject(Error(JSON.stringify(response.error))) : request.resolve(response.result);
    });
    const call = (method, params = {}, sessionId, timeout = 5000) => new Promise((resolve, reject) => {
      const id = ++serial;
      const timer = setTimeout(() => {pending.delete(id); reject(Error(method + ' timeout'));}, timeout);
      pending.set(id, {resolve, reject, timer});
      socket.send(JSON.stringify({id, method, params, ...(sessionId ? {sessionId} : {})}));
    });
    const {targetId} = await call('Target.createTarget', {url: 'about:blank'});
    const {sessionId} = await call('Target.attachToTarget', {targetId, flatten: true});
    result.phase = 'navigation';
    const start = performance.now();
    const navigation = await call('Page.navigate', {url: test.url}, sessionId, 35000);
    result.navigationMs = performance.now() - start;
    assert(!navigation.errorText, navigation.errorText);
    const ready = await call('Runtime.evaluate', {
      expression: 'JSON.stringify({readyState:document.readyState,title:document.title,url:location.href})', returnByValue: true,
    }, sessionId);
    result.page = JSON.parse(ready.result.value);
    assert.equal(result.page.title, test.title, 'unexpected document, not a valid capture reproduction');
    result.phase = 'capture';
    writeFileSync(output, JSON.stringify(result, null, 2) + '\n');
    const captureStart = performance.now();
    const capture = call('Page.captureScreenshot', {format: 'png'}, sessionId, 60000);
    process.send?.({phase: 'capture-sent'});
    const image = await capture;
    result.captureMs = performance.now() - captureStart;
    const bytes = Buffer.from(image.data, 'base64');
    assert(bytes.subarray(0, 8).equals(pngHeader), 'invalid PNG signature');
    assert(bytes.length > 10000, 'unexpectedly small screenshot for the fixture');
    result.image = {bytes: bytes.length, width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20), format: 'png'};
    writeFileSync(output + '.png', bytes);
    result.ok = true; result.phase = 'complete';
  } catch (error) {result.error = String(error);}
  finally {
    for (const request of pending.values()) clearTimeout(request.timer);
    try {socket?.close();} catch {}
    writeFileSync(output, JSON.stringify(result, null, 2) + '\n');
  }
  // Exiting the client closes its TCP socket even if the server cannot finish a close handshake.
  process.exit(result.ok ? 0 : 1);
}

if (process.argv[2] === '--client') {
  await client(...process.argv.slice(3));
} else {
  const [endpoint, pidText, directory] = process.argv.slice(2);
  assert(endpoint && /^\d+$/.test(pidText || '') && directory, 'usage: capture_lifecycle.mjs ENDPOINT SERVER_PID OUTPUT_DIRECTORY');
  const pid = Number(pidText);
  mkdirSync(directory, {recursive: false});
  const cases = JSON.parse(process.env.CAPTURE_CASES || JSON.stringify([
    {url: 'https://en.wikipedia.org/wiki/Seoul', title: 'Seoul - Wikipedia'},
    {url: 'https://en.wikipedia.org/wiki/Busan', title: 'Busan - Wikipedia'},
  ]));
  assert.equal(cases.length, 2);
  const ticks = Number(execFileSync('getconf', ['CLK_TCK'], {encoding: 'utf8'}));
  function resources() {
    let cpuSeconds = 0, rssKiB = 0;
    const queue = [pid], seen = new Set();
    while (queue.length) {
      const current = queue.pop(); if (seen.has(current)) continue; seen.add(current);
      try {
        const fields = readFileSync(`/proc/${current}/stat`, 'utf8').split(')').pop().trim().split(/\s+/);
        cpuSeconds += (Number(fields[11]) + Number(fields[12])) / ticks;
        rssKiB += Number(readFileSync(`/proc/${current}/status`, 'utf8').match(/^VmRSS:\s+(\d+)/m)?.[1] || 0);
        const children = readFileSync(`/proc/${current}/task/${current}/children`, 'utf8').trim();
        if (children) queue.push(...children.split(/\s+/).map(Number));
      } catch (error) {if (current === pid) throw error;}
    }
    return {timeMs: performance.now(), cpuSeconds, rssKiB};
  }
  async function attempt(label, test, abandon = false) {
    const output = join(directory, label + '.json'), fd = openSync(output + '.log', 'wx');
    const child = fork(fileURLToPath(import.meta.url), ['--client', endpoint, JSON.stringify(test), output], {stdio: ['ignore', fd, fd, 'ipc']});
    let captureSent = false, killed = false, killTimer;
    const deadline = setTimeout(() => {killed = true; child.kill('SIGKILL');}, 110000);
    child.on('message', message => {
      if (message.phase === 'capture-sent') {
        captureSent = true;
        if (abandon) killTimer = setTimeout(() => {killed = true; child.kill('SIGKILL');}, 5);
      }
    });
    const exit = await new Promise((resolve, reject) => {
      child.once('error', reject); child.once('exit', (code, signal) => resolve({code, signal}));
    }).finally(() => {clearTimeout(deadline); clearTimeout(killTimer); closeSync(fd);});
    await sleep(1000);
    const before = resources(); await sleep(2000); const after = resources();
    const busyCores = (after.cpuSeconds - before.cpuSeconds) / ((after.timeMs - before.timeMs) / 1000);
    return {label, abandon, captureSent, killed, exit, result: existsSync(output) ? JSON.parse(readFileSync(output, 'utf8')) : null,
      before, after, busyCores};
  }
  const report = {architecture: process.arch, platform: process.platform, pid, endpoint,
    methodology: 'Fresh raw CDP connection per attempt; no layout reads before capture. Client process exits or is killed after sending capture. CPU and RSS cover the server process tree. Timings are diagnostic, not benchmark claims.', attempts: []};
  let status = 1;
  try {
    for (let i = 0; i < 3; i++) {
      report.attempts.push(await attempt('capture-' + i, cases[0]));
      console.log(report.attempts.at(-1).label, JSON.stringify({ok: report.attempts.at(-1).result?.ok, busyCores: report.attempts.at(-1).busyCores}));
    }
    if (report.attempts.every(attempt => attempt.result?.ok)) report.attempts.push(await attempt('disconnect-during-capture', cases[0], true));
    report.attempts.push(await attempt('control-after-disconnect', cases[1]));
    const abandoned = report.attempts.find(attempt => attempt.abandon);
    const lastRss = report.attempts.at(-1).after.rssKiB, firstRss = report.attempts[0].after.rssKiB;
    report.checks = {
      completedCaptures: report.attempts.filter(attempt => !attempt.abandon).every(attempt => attempt.result?.ok),
      activeCaptureDisconnected: Boolean(abandoned?.captureSent && abandoned?.killed && abandoned?.result?.phase === 'capture'),
      cpuQuiescent: report.attempts.every(attempt => attempt.busyCores < 0.25),
      boundedRssGrowth: lastRss - firstRss < 96 * 1024,
    };
    status = Object.values(report.checks).every(Boolean) ? 0 : 1;
    console.log(JSON.stringify(report.checks));
  } catch (error) {report.error = String(error);}
  finally {writeFileSync(join(directory, 'result.json'), JSON.stringify(report, null, 2) + '\n');}
  process.exitCode = status;
}
