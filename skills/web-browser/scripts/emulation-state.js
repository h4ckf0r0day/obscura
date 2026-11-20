import {
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { applyDevicePreset, resolveDevicePreset } from "./devices.js";

const HOME = process.env["HOME"] || homedir();
const BROWSER_ROOT = join(HOME, ".cache", "agent-web", "browser");
const EMULATION_STATE_FILE = join(BROWSER_ROOT, "emulation.json");

function ensureStateDir() {
  if (!existsSync(BROWSER_ROOT)) {
    mkdirSync(BROWSER_ROOT, { recursive: true });
  }
}

export function readEmulationPreference() {
  if (!existsSync(EMULATION_STATE_FILE)) return null;
  try {
    const data = JSON.parse(readFileSync(EMULATION_STATE_FILE, "utf8"));
    if (!data || typeof data !== "object") return null;
    if (typeof data.device !== "string" || data.device.length === 0) return null;
    return {
      device: data.device.toLowerCase(),
      landscape: data.landscape === true,
      updatedAt: typeof data.updatedAt === "string" ? data.updatedAt : null,
    };
  } catch {
    return null;
  }
}

export function writeEmulationPreference({ device, landscape }) {
  ensureStateDir();
  const payload = {
    device: String(device).toLowerCase(),
    landscape: landscape === true,
    updatedAt: new Date().toISOString(),
  };
  writeFileSync(EMULATION_STATE_FILE, `${JSON.stringify(payload, null, 2)}\n`);
}

export function clearEmulationPreference() {
  try {
    rmSync(EMULATION_STATE_FILE, { force: true });
  } catch {
    // ignore
  }
}

export async function applyActiveEmulation(cdp, sessionId) {
  const preference = readEmulationPreference();
  if (!preference) return null;

  const preset = resolveDevicePreset(preference.device);
  if (!preset) {
    clearEmulationPreference();
    return null;
  }

  const metrics = await applyDevicePreset(cdp, sessionId, preset, {
    landscape: preference.landscape,
  });

  return {
    preset,
    ...metrics,
  };
}
