// Standalone evidence fixture for #634's opt-in retained-target reconnect.
//
// It is not part of the normal Rust test suite because it needs a Playwright
// installation (no browser download; connectOverCDP only). Run:
//
//   OBSCURA_BIN=target/release/obscura \
//   PLAYWRIGHT_MODULE=/path/to/playwright \
//   node crates/obscura-cdp/tests/playwright-reconnect.cjs
//
// The fixture proves the public client lifecycle: fill text and attach a file
// through CDP, disconnect Playwright while the server stays up, then reconnect
// with the explicit header and get the same target with its form state in the
// normal Playwright contexts.

const assert = require("node:assert/strict");
const { spawn } = require("node:child_process");
const { once } = require("node:events");
const { mkdtemp, rm, writeFile } = require("node:fs/promises");
const { createServer } = require("node:http");
const { createServer: createPortProbe } = require("node:net");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const { setTimeout: delay } = require("node:timers/promises");
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright");

const RECONNECT_HEADERS = { "x-obscura-reconnect": "1" };

async function pickPort() {
  return await new Promise(resolve => {
    const probe = createPortProbe();
    probe.listen(0, "127.0.0.1", () => {
      const port = probe.address().port;
      probe.close(() => resolve(port));
    });
  });
}

async function main() {
  const scratch = await mkdtemp(join(tmpdir(), "obscura-reconnect-"));
  const server = createServer((_req, res) => {
    res.writeHead(200, { "content-type": "text/html" });
    res.end('<!doctype html><form><input id="name"><input id="file" type="file"></form>');
  });
  let child, childDone, first, second, plain, afterClose;
  try {
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const url = `http://127.0.0.1:${server.address().port}/form`;
    const fixture = join(scratch, "fixture.txt");
    await writeFile(fixture, "fixture\n");
    const port = await pickPort();
    child = spawn(process.env.OBSCURA_BIN || "obscura", [
      "serve", "--host", "127.0.0.1", "--port", String(port),
      "--allow-private-network", "--allow-file-access",
      "--storage-dir", join(scratch, "storage"),
    ], { stdio: ["ignore", "inherit", "inherit"] });
    childDone = new Promise(resolve => {
      child.once("exit", resolve);
      child.once("error", resolve);
    });
    await once(child, "spawn");
    let ready = false;
    for (let i = 0; i < 100 && !ready && child.exitCode === null; i++) {
      ready = await fetch(`http://127.0.0.1:${port}/json/version`)
        .then(response => response.ok)
        .catch(() => false);
      if (!ready) await delay(50);
    }
    assert.ok(ready, "Obscura did not publish the local CDP endpoint");
    const endpoint = `http://127.0.0.1:${port}`;

    // First client: no opt-in. Create the fixture page and mutate it through
    // the same CDP surface a real automation client uses.
    first = await chromium.connectOverCDP(endpoint, { timeout: 5000 });
    const page = await first.contexts()[0].newPage();
    await page.goto(url);
    await page.locator("#name").fill("reconnect fixture");
    const initialCdp = await first.newBrowserCDPSession();
    const { targetInfos: initialTargets } = await initialCdp.send("Target.getTargets");
    const targetInfo = initialTargets.find(target => target.url === url);
    assert.ok(targetInfo, "expected the original fixture page in initial CDP target discovery");
    const before = await page.context().newCDPSession(page);
    const { root } = await before.send("DOM.getDocument");
    const { nodeId } = await before.send("DOM.querySelector", { nodeId: root.nodeId, selector: "#file" });
    await before.send("DOM.setFileInputFiles", { nodeId, files: [fixture] });
    const readForm = tab => tab.evaluate(() => ({
      answer: document.querySelector("#name").value,
      attachment: document.querySelector("#file").files[0]?.name,
    }));
    const expected = { answer: "reconnect fixture", attachment: "fixture.txt" };
    assert.deepEqual(await readForm(page), expected);
    await before.detach();
    await first.close(); // Disconnect Playwright while keeping the server up.
    first = undefined;

    // A non-opted-in client must remain isolated: it gets its own blank page,
    // never the retained fixture.
    plain = await chromium.connectOverCDP(endpoint, { timeout: 5000 });
    const plainPages = plain.contexts().flatMap(context => context.pages());
    assert.ok(
      plainPages.every(tab => tab.url() !== url),
      "retained target must not leak into a live non-opted-in client",
    );
    const plainCdp = await plain.newBrowserCDPSession();
    const { targetInfos: beforeReconnect } = await plainCdp.send("Target.getTargets");

    // Reconnect with the explicit header. Playwright forwards it to the WS
    // handshake, where Obscura opts this connection into retained targets.
    second = await chromium.connectOverCDP(endpoint, {
      timeout: 5000,
      headers: RECONNECT_HEADERS,
    });
    let browserCdp = await second.newBrowserCDPSession();
    const { targetInfos } = await browserCdp.send("Target.getTargets");
    assert.equal(targetInfos.length, beforeReconnect.length, "reconnect must not allocate an implicit target");
    assert.ok(
      targetInfos.some(target => target.targetId === targetInfo.targetId && target.url === url),
      "the original target must still be retained after disconnect",
    );
    const pagesForFixture = () => second.contexts()
      .flatMap(context => context.pages())
      .filter(tab => tab.url() === url);
    let matches = [];
    for (let i = 0; i < 20 && !matches.length; i++) {
      matches = pagesForFixture();
      if (!matches.length) await delay(100);
    }
    assert.equal(matches.length, 1, "expected the original tab exactly once in Playwright after opt-in reconnect");
    assert.deepEqual(await readForm(matches[0]), expected);
    assert.equal(
      targetInfos.find(target => target.url === url)?.targetId,
      targetInfo.targetId,
      "reconnect must keep the same target id",
    );

    for (let i = 0; i < 3; i++) {
      await second.close();
      second = await chromium.connectOverCDP(endpoint, { timeout: 5000, headers: RECONNECT_HEADERS });
      browserCdp = await second.newBrowserCDPSession();
      const { targetInfos: afterReconnect } = await browserCdp.send("Target.getTargets");
      assert.equal(afterReconnect.length, beforeReconnect.length, "repeated reconnect must not grow the target registry");
      const [retained] = pagesForFixture();
      assert.ok(retained, "retained target must remain usable after repeated reconnects");
      assert.deepEqual(await readForm(retained), expected);
    }

    // Close the retained target through the reconnect client; a later
    // opted-in connection must not announce it.
    await browserCdp.send("Target.closeTarget", { targetId: targetInfo.targetId });
    await second.close();
    second = undefined;
    afterClose = await chromium.connectOverCDP(endpoint, {
      timeout: 5000,
      headers: RECONNECT_HEADERS,
    });
    const afterPages = afterClose.contexts()
      .flatMap(context => context.pages())
      .filter(tab => tab.url() === url);
    assert.equal(afterPages.length, 0, "a closed retained target must not be attached again");

    console.log(JSON.stringify({
      targetId: targetInfo.targetId,
      answer: expected.answer,
      attachment: expected.attachment,
      pagesAfterReconnect: matches.length,
      pagesAfterClose: afterPages.length,
    }));
    console.log("PASS same target, form text, and file attachment after opt-in reconnect");
  } finally {
    await Promise.allSettled([first?.close(), second?.close(), plain?.close(), afterClose?.close()]);
    if (child) {
      if (child.exitCode === null && child.signalCode === null) child.kill();
      await childDone;
    }
    if (server.listening) await new Promise(resolve => server.close(resolve));
    await rm(scratch, { recursive: true, force: true });
  }
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
