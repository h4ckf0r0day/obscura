#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Set, list or clear the active device emulation preference.
 *
 * The preference is stored in emulation.json and applied by nav.js, eval.js,
 * click.js and screenshot.js. Emulation changes layout only: measured on
 * Obscura 0.2.7, Emulation.setUserAgentOverride never reaches the network and
 * reverts on navigation, so a UA-sniffing site still sees the real UA. Use
 * OBSCURA_USER_AGENT on serve for a real UA change.
 */

import {
  brokerRequest,
  ensureBroker,
  sessionAdapter,
} from "./broker-client.js";
import {
  applyDevicePreset,
  clearDeviceEmulation,
  listDevicePresets,
  resolveDevicePreset,
} from "./devices.js";
import {
  clearEmulationPreference,
  readEmulationPreference,
  writeEmulationPreference,
} from "./emulation-state.js";
import { parseFlags } from "./cli.js";

const DEBUG = process.env.DEBUG === "1";
const log = DEBUG ? (...args) => console.error("[debug]", ...args) : () => {};

const { positional, has, unknown } = parseFlags(process.argv.slice(2), { values: [] });

if (has("--list")) {
  for (const preset of listDevicePresets()) {
    console.log(
      `${preset.id.padEnd(12)} ${preset.width}x${preset.height} @${preset.deviceScaleFactor}x (${preset.title})`,
    );
  }
  process.exit(0);
}

if (unknown.length > 0) {
  console.error(`✗ Unknown option: ${unknown.join(", ")}`);
  process.exit(1);
}

const reset = has("--reset");
const landscape = has("--landscape");
const deviceName = positional[0];

if (reset && landscape) {
  console.error("✗ --landscape cannot be combined with --reset");
  process.exit(1);
}

if ((reset && deviceName) || (!reset && !deviceName)) {
  console.log("Usage: emulate.js <device> [--landscape]");
  console.log("       emulate.js --reset");
  console.log("       emulate.js --list");
  console.log("\nExamples:");
  console.log("  emulate.js --list");
  console.log("  emulate.js iphone-14");
  console.log("  emulate.js pixel-7 --landscape");
  console.log("  emulate.js --reset");
  process.exit(1);
}

const preset = reset ? null : resolveDevicePreset(deviceName);
if (!reset && !preset) {
  console.error(`✗ Unknown device preset: ${deviceName}`);
  console.error("  Available: " + listDevicePresets().map((p) => p.id).join(", "));
  process.exit(1);
}

const globalTimeout = setTimeout(() => {
  console.error("✗ Global timeout exceeded (30s)");
  process.exit(1);
}, 30000);

try {
  await ensureBroker();
  log("broker ready");
  const cdp = sessionAdapter(undefined);

  if (reset) {
    // Device metrics are per-session CDP state, so every open tab carries its
    // own override. Clearing only the default tab would leave the others
    // emulated for the rest of their lives.
    const { tabs } = await brokerRequest({ op: "tabs" }, { timeout: 5000 });
    let cleared = 0;
    for (const tab of tabs) {
      try {
        await clearDeviceEmulation(sessionAdapter(tab.tabId), null);
        cleared += 1;
      } catch (e) {
        log(`could not clear ${tab.tabId}: ${e.message}`);
      }
    }
    clearEmulationPreference();
    console.log(`✓ Cleared device emulation on ${cleared} tab(s)`);
  } else {
    const metrics = await applyDevicePreset(cdp, null, preset, { landscape });
    writeEmulationPreference({ device: preset.id, landscape });
    console.log(
      `✓ Emulating ${preset.title} at ${metrics.width}x${metrics.height} @${metrics.deviceScaleFactor}x${landscape ? " landscape" : ""}`,
    );
    console.log("  Saved as the active preference for nav/eval/click/screenshot");
    console.log("  Layout only: Obscura's UA override never reaches the network");
  }

  const active = readEmulationPreference();
  if (active) console.log(`  active: ${active.device}${active.landscape ? " (landscape)" : ""}`);
} catch (e) {
  console.error("✗", e.message);
  process.exitCode = 1;
} finally {
  clearTimeout(globalTimeout);
}
