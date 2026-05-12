#!/usr/bin/env node
// obscura-mcp plugin bootstrap
// Runs on SessionStart via hooks.json.
// Uses only Node.js built-ins — no npm install needed.

"use strict";

const { spawnSync } = require("child_process");
const { createWriteStream, chmodSync, readFileSync } = require("fs");
const { join } = require("path");
const https = require("https");
const os = require("os");

const REPO = "epicsagas/obscura-mcp";
const OBSCURA_REPO = "epicsagas/obscura";
const MCP_BINARY = "obscura-mcp";
const OBSCURA_BINARY = "obscura";
const INSTALLER_SH = `https://github.com/${REPO}/releases/latest/download/obscura-mcp-installer.sh`;
const OBSCURA_INSTALLER_SH = `https://github.com/${OBSCURA_REPO}/releases/latest/download/obscura-installer.sh`;

function log(msg) {
  process.stderr.write(`[obscura-mcp plugin] ${msg}\n`);
}

function hasCommand(cmd) {
  const r = spawnSync(cmd, ["--version"], { stdio: "pipe", shell: false });
  return r.status === 0;
}

function getBinaryVersion(cmd) {
  try {
    const r = spawnSync(cmd, ["--version"], { stdio: "pipe", shell: false });
    if (r.status === 0) {
      const output = r.stdout.toString().trim();
      const match = output.match(/(\d+\.\d+\.\d+)/);
      return match ? match[1] : null;
    }
  } catch (_) {}
  return null;
}

function getPluginVersion() {
  try {
    const manifestPath = join(
      process.env.CLAUDE_PLUGIN_ROOT || "",
      ".claude-plugin",
      "plugin.json"
    );
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    return manifest.version || null;
  } catch (_) {}
  return null;
}

function semverGt(a, b) {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < 3; i++) {
    if (pa[i] > pb[i]) return true;
    if (pa[i] < pb[i]) return false;
  }
  return false;
}

function downloadFile(url, dest) {
  return new Promise((resolve, reject) => {
    const file = createWriteStream(dest);
    const follow = (u) => {
      https
        .get(u, (res) => {
          if (res.statusCode === 301 || res.statusCode === 302) {
            follow(res.headers.location);
            res.resume();
            return;
          }
          if (res.statusCode !== 200) {
            reject(new Error(`HTTP ${res.statusCode} for ${u}`));
            return;
          }
          res.pipe(file);
          file.on("finish", () => file.close(resolve));
        })
        .on("error", reject);
    };
    follow(url);
  });
}

async function installBinary(name, installerUrl) {
  if (os.platform() === "win32") {
    log(`${name} installer is not available for Windows.`);
    log(`Build from source: cargo install ${name}`);
    return false;
  }

  const tmp = join(os.tmpdir(), `${name}-installer.sh`);
  log(`Downloading ${name} installer...`);
  await downloadFile(installerUrl, tmp);
  chmodSync(tmp, 0o755);
  const r = spawnSync("sh", [tmp], { stdio: "inherit" });
  if (r.status !== 0) throw new Error(`${name} installer failed`);
  return true;
}

function seed() {
  // Register MCP + install skill into Claude Code
  spawnSync(MCP_BINARY, ["install", "claude"], { stdio: "inherit" });
}

async function main() {
  const pluginVersion = getPluginVersion();

  // ── 1. Ensure obscura binary ─────────────────────────────────────────────
  if (!hasCommand(OBSCURA_BINARY)) {
    log(`${OBSCURA_BINARY} not found — installing...`);
    try {
      await installBinary(OBSCURA_BINARY, OBSCURA_INSTALLER_SH);
      if (!hasCommand(OBSCURA_BINARY)) {
        log(`${OBSCURA_BINARY} install succeeded but binary not in PATH.`);
        log(`Add it to PATH or set OBSCURA_BIN env var.`);
      }
    } catch (e) {
      log(`${OBSCURA_BINARY} install failed: ${e.message}`);
      log(`Install manually: https://github.com/${OBSCURA_REPO}#installation`);
    }
  }

  // ── 2. Ensure obscura-mcp binary ─────────────────────────────────────────
  if (!hasCommand(MCP_BINARY)) {
    log(`${MCP_BINARY} not found — installing...`);
    try {
      await installBinary(MCP_BINARY, INSTALLER_SH);
    } catch (e) {
      log(`Install failed: ${e.message}`);
      log(`Install manually: https://github.com/${REPO}#installation`);
      process.exit(0);
    }
    if (hasCommand(MCP_BINARY)) seed();
    return;
  }

  // ── 3. Check for update ───────────────────────────────────────────────────
  if (pluginVersion) {
    const binaryVersion = getBinaryVersion(MCP_BINARY);
    if (binaryVersion && semverGt(pluginVersion, binaryVersion)) {
      log(`Updating ${MCP_BINARY} ${binaryVersion} → ${pluginVersion}...`);
      try {
        await installBinary(MCP_BINARY, INSTALLER_SH);
        const newVersion = getBinaryVersion(MCP_BINARY);
        if (newVersion) log(`Updated to ${newVersion}`);
      } catch (e) {
        log(`Update failed: ${e.message}`);
        log(`Continuing with ${binaryVersion}`);
      }
    }
  }

  // ── 4. Register MCP + skill ───────────────────────────────────────────────
  seed();
}

main().catch((e) => {
  log(`Unexpected error: ${e.message}`);
  process.exit(0);
});
