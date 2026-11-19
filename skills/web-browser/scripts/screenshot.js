#!/bin/sh
':' //; exec "$(command -v node 2>/dev/null || echo /usr/bin/node)" "$0" "$@"
/**
 * Screenshot the active broker tab (Obscura only — no Chrome, no puppeteer).
 *
 * Obscura 0.2.7 fails `Page.captureScreenshot` with "bitmap is too large" under
 * THREE measured limits, not one:
 *   - ~16.77 Mpx of OUTPUT pixels (1280x13100 OK, 1280x13110 fails);
 *   - an output dimension ceiling of 32768 px (100x32768 OK, 100x32769 fails);
 *   - a CSS CLIP dimension ceiling of 32768 px (100x33000 fails at scale 0.1),
 *     so a 40000-px-tall page cannot be captured in a single clip at all.
 * A high-DPR device multiplies output pixels (a 412x20000 css page at dpr 2.625
 * is 1082x52500), so full-page shots hit these easily. This script therefore:
 *
 *   1. Captures one clip when it fits all three budgets.
 *   2. Otherwise tiles vertically in CSS px and STITCHES the tiles into one PNG
 *      with node:zlib only (decode IDAT -> inverse-filter -> concat scanlines ->
 *      re-deflate). Verified pixel-exact against the source tiles and re-read by
 *      `file` as 8-bit/color RGBA, non-interlaced. A tile that still fails is
 *      retried at half height, because not every cap is modellable: a
 *      `repeating-linear-gradient` background keeps failing at 1280x32768 css
 *      even at scale 0.5 (measured), i.e. that paint path ignores `scale`.
 *   3. If stitching is impossible (interlaced/16-bit/unexpected colour type),
 *      it writes `<base>.<n>.png` per tile, prints every path, then `tiles: N`
 *      and `css: <w>x<h>` so the caller can stitch or read them itself.
 *
 * Output contract: a stitched (or viewport) shot prints exactly one line on
 * stdout — the file path — so `out=$(screenshot.js)` works. Metadata goes to
 * stderr. The multi-file fallback prints the paths plus the two summary lines
 * on stdout.
 *
 * Page.captureScreenshot needs only `clip`; `fromSurface` and
 * `captureBeyondViewport` are no-ops on Obscura (kept for Chrome parity).
 *
 * --device applies emulation only for this shot and then restores whatever
 * emulation preference was active. Note: Emulation.setUserAgentOverride does
 * NOT reach the network on Obscura (measured: wire UA stays Chrome/145) and is
 * dropped on the next navigation, so device emulation here changes LAYOUT ONLY
 * and cannot fool UA/CH-UA sniffing sites.
 */

import { mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { deflateSync, inflateSync } from "node:zlib";
import { cdpCall, ensureBroker, evaluate } from "./broker-client.js";
import { parseFlags } from "./cli.js";
import {
  applyDevicePreset,
  clearDeviceEmulation,
  listDevicePresets,
  resolveDevicePreset,
} from "./devices.js";
import { applyActiveEmulation, readEmulationPreference } from "./emulation-state.js";

const DEBUG = process.env.DEBUG === "1";
const log = DEBUG ? (...args) => console.error("[debug]", ...args) : () => {};

// Measured on obscura 0.2.7, three independent limits on one capture:
//   1. total OUTPUT pixels: 1280x13100 (16.77 Mpx) succeeds, 1280x13110 (16.78
//      Mpx) fails -> budget 16.77 Mpx, use 16.0 Mpx for margin;
//   2. output dimension: 100x32768 succeeds, 100x32769 fails (same for width);
//   3. CSS CLIP dimension: 100x32768 succeeds, 100x33000 fails *even at
//      scale 0.1*, so a tall page cannot be captured in one clip at all.
// Tiles are therefore sized against all three.
const MAX_PIXELS = 16_000_000;
const MAX_DIMENSION = 32_000; // 32768 measured, with margin
// Stitched output lives in RAM as RGBA, so refuse absurd pages instead of OOMing.
const MAX_STITCH_PIXELS = 400_000_000;

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const CHANNELS_BY_COLOR_TYPE = { 0: 1, 2: 3, 3: 1, 4: 2, 6: 4 };

let CRC_TABLE = null;

function crcTable() {
  if (CRC_TABLE) return CRC_TABLE;
  CRC_TABLE = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    CRC_TABLE[n] = c;
  }
  return CRC_TABLE;
}

function crc32(buf) {
  const table = crcTable();
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = table[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length, 0);
  const typeBytes = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBytes, data])), 0);
  return Buffer.concat([length, typeBytes, data, crc]);
}

/**
 * Decode a non-interlaced 8-bit PNG to raw scanlines (filters removed).
 * Only what Obscura emits is supported; anything else throws so the caller can
 * fall back to writing separate tiles instead of producing a broken file.
 */
function decodePng(buf) {
  if (!buf.subarray(0, 8).equals(PNG_SIGNATURE)) throw new Error("not a PNG");
  let offset = 8;
  let header = null;
  const idat = [];
  while (offset < buf.length) {
    const length = buf.readUInt32BE(offset);
    const type = buf.subarray(offset + 4, offset + 8).toString("ascii");
    const data = buf.subarray(offset + 8, offset + 8 + length);
    if (type === "IHDR") {
      header = {
        width: data.readUInt32BE(0),
        height: data.readUInt32BE(4),
        bitDepth: data[8],
        colorType: data[9],
        interlace: data[12],
      };
    } else if (type === "IDAT") {
      idat.push(data);
    } else if (type === "IEND") {
      break;
    }
    offset += 12 + length;
  }
  if (!header) throw new Error("PNG has no IHDR");
  if (header.interlace !== 0) throw new Error("interlaced PNG unsupported");
  if (header.bitDepth !== 8) throw new Error(`bit depth ${header.bitDepth} unsupported`);
  const channels = CHANNELS_BY_COLOR_TYPE[header.colorType];
  if (!channels) throw new Error(`colour type ${header.colorType} unsupported`);

  const raw = inflateSync(Buffer.concat(idat));
  const stride = header.width * channels;
  const pixels = Buffer.alloc(stride * header.height);
  for (let y = 0; y < header.height; y++) {
    const filter = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    const rowStart = y * stride;
    for (let x = 0; x < stride; x++) {
      const left = x >= channels ? pixels[rowStart + x - channels] : 0;
      const up = y > 0 ? pixels[rowStart - stride + x] : 0;
      const upLeft = x >= channels && y > 0 ? pixels[rowStart - stride + x - channels] : 0;
      let value = line[x];
      switch (filter) {
        case 0:
          break;
        case 1:
          value = (value + left) & 0xff;
          break;
        case 2:
          value = (value + up) & 0xff;
          break;
        case 3:
          value = (value + ((left + up) >> 1)) & 0xff;
          break;
        case 4: {
          const p = left + up - upLeft;
          const pa = Math.abs(p - left);
          const pb = Math.abs(p - up);
          const pc = Math.abs(p - upLeft);
          value = (value + (pa <= pb && pa <= pc ? left : pb <= pc ? up : upLeft)) & 0xff;
          break;
        }
        default:
          throw new Error(`bad PNG filter ${filter} on row ${y}`);
      }
      pixels[rowStart + x] = value;
    }
  }
  return { ...header, channels, pixels };
}

function encodePng({ width, height, colorType, channels, pixels }) {
  const stride = width * channels;
  const raw = Buffer.alloc((stride + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (stride + 1)] = 0; // filter type 0 (None)
    pixels.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = colorType;
  ihdr[10] = 0; // deflate
  ihdr[11] = 0; // adaptive filtering
  ihdr[12] = 0; // non-interlaced
  return Buffer.concat([
    PNG_SIGNATURE,
    pngChunk("IHDR", ihdr),
    pngChunk("IDAT", deflateSync(raw, { level: 6 })),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

/** Concatenate top-to-bottom tiles (equal width/channels) into one PNG. */
function stitchVertical(buffers) {
  const images = buffers.map(decodePng);
  const width = images[0].width;
  const colorType = images[0].colorType;
  const channels = images[0].channels;
  images.forEach((image, index) => {
    if (image.width !== width) throw new Error(`tile ${index} width ${image.width} != ${width}`);
    if (image.colorType !== colorType) throw new Error(`tile ${index} colour type differs`);
  });
  const height = images.reduce((sum, image) => sum + image.height, 0);
  const pixels = Buffer.alloc(width * channels * height);
  let offset = 0;
  for (const image of images) {
    image.pixels.copy(pixels, offset);
    offset += image.pixels.length;
  }
  return encodePng({ width, height, colorType, channels, pixels });
}

function pngSize(buf) {
  return { width: buf.readUInt32BE(16), height: buf.readUInt32BE(20) };
}

function printUsage() {
  console.log("Usage: screenshot.js [--full-page] [--device=<preset>] [--landscape]");
  console.log("                    [--scale=<n>] [--path=<file>] [--tab=<id>]");
  console.log("\nOptions:");
  console.log("  --full-page       Whole page. Tiled past Obscura's ~16.9 Mpx cap and");
  console.log("                    stitched into one PNG with node:zlib.");
  console.log("  --device=<preset> Temporary layout emulation (UA never reaches the wire).");
  console.log("  --landscape       Rotate the device preset.");
  console.log("  --scale=<n>       Clip scale; <1 shrinks output pixels and dodges the cap.");
  console.log("  --path=<file>     Write here instead of os.tmpdir().");
  console.log("\nExamples:");
  console.log("  screenshot.js");
  console.log("  screenshot.js --full-page");
  console.log("  screenshot.js --full-page --scale=0.5");
  console.log("  screenshot.js --device pixel-7 --landscape");
  process.exit(1);
}

const { get, has, unknown } = parseFlags(process.argv.slice(2));

if (unknown.length > 0) {
  console.error(`✗ Unknown option: ${unknown.join(", ")}`);
  printUsage();
}

if (has("--help")) printUsage();

const fullPage = has("--full-page");
const landscape = has("--landscape");
const deviceName = get("--device", undefined);
const scaleArg = Number(get("--scale", 1));
const outputPath = get("--path", undefined);
const tabId = get("--tab", undefined);

if (deviceName && !resolveDevicePreset(deviceName)) {
  console.error(`✗ Unknown device preset: ${deviceName}`);
  console.error("  Available presets:");
  for (const item of listDevicePresets()) console.error(`  - ${item.id}`);
  process.exit(1);
}
if (landscape && !deviceName) {
  console.error("✗ --landscape requires --device=<preset>");
  process.exit(1);
}
if (!Number.isFinite(scaleArg) || scaleArg <= 0 || scaleArg > 4) {
  console.error("✗ --scale must be a number in (0, 4]");
  process.exit(1);
}
const scale = scaleArg === 1 ? 1 : scaleArg;

const globalTimeout = setTimeout(() => {
  console.error("✗ Global timeout exceeded (180s)");
  process.exit(1);
}, 180000);

/** devices.js/emulation-state.js call cdp.send(method, params, sessionId). */
function sessionAdapter() {
  return {
    send: (method, params = {}, _sessionId, timeout) =>
      cdpCall(method, params, { tabId, timeout }),
  };
}

async function capture(params) {
  const { data } = await cdpCall("Page.captureScreenshot", params, { tabId, timeout: 30000 });
  return Buffer.from(data, "base64");
}

function basePath() {
  if (outputPath) return String(outputPath).replace(/\.png$/i, "");
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  return join(tmpdir(), `screenshot-${stamp}`);
}

try {
  await ensureBroker();
  log("broker ready");

  const cdp = sessionAdapter();
  const preset = deviceName ? resolveDevicePreset(deviceName) : null;
  let temporaryEmulation = false;

  if (preset) {
    log("applying temporary device emulation", preset.id);
    await applyDevicePreset(cdp, null, preset, { landscape });
    temporaryEmulation = true;
  } else {
    // Match nav.js: honour the persisted emulation preference for this tab.
    await applyActiveEmulation(cdp, null);
  }

  try {
    const base = basePath();

    if (!fullPage) {
      log("capturing viewport");
      const buf = await capture({ format: "png" });
      const file = `${base}.png`;
      mkdirSync(dirname(file), { recursive: true });
      writeFileSync(file, buf);
      const size = pngSize(buf);
      console.error(`✓ png ${size.width}x${size.height} (viewport)`);
      console.log(file);
    } else {
      const metrics = await cdpCall("Page.getLayoutMetrics", {}, { tabId, timeout: 10000 });
      const contentSize = metrics.cssContentSize || metrics.contentSize;
      if (!contentSize) throw new Error("Page.getLayoutMetrics returned no cssContentSize");

      const dpr = (await evaluate("devicePixelRatio", { tabId })) || 1;
      const width = Math.max(1, Math.ceil(contentSize.width));
      const height = Math.max(1, Math.ceil(contentSize.height));
      log(`css ${width}x${height}, dpr ${dpr}, scale ${scale}`);

      // Output pixels = css * dpr * scale (measured: a 400x4000 css page at
      // dpr 2 comes back 800x4000 at scale 1 and 400x2000 at scale 0.5).
      const outWidth = Math.max(1, Math.ceil(width * dpr * scale));
      const fullOutHeight = Math.max(1, Math.ceil(height * dpr * scale));

      const clip = (y, tileHeight) => ({
        format: "png",
        fromSurface: true, // no-op on Obscura, kept for Chrome parity
        captureBeyondViewport: true, // no-op on Obscura
        clip: { x: 0, y, width, height: tileHeight, scale },
      });

      let tiles = [];
      const fitsOneClip =
        outWidth <= MAX_DIMENSION &&
        fullOutHeight <= MAX_DIMENSION &&
        height <= MAX_DIMENSION &&
        outWidth * fullOutHeight <= MAX_PIXELS;

      if (fitsOneClip) {
        log("single full-page clip");
        try {
          tiles = [{ buf: await capture(clip(0, height)) }];
        } catch (e) {
          if (!/too large/i.test(e.message)) throw e;
          log("single clip rejected by the bitmap cap, tiling anyway");
          tiles = [];
        }
      }

      if (tiles.length === 0) {
        // tileH is CSS px and must satisfy all three limits:
        //   css tile height           <= MAX_DIMENSION
        //   css tile height * dpr * scale <= MAX_DIMENSION (output height)
        //   outWidth * (that)         <= MAX_PIXELS
        const outPerCssRow = dpr * scale;
        const byDimension = Math.floor(MAX_DIMENSION / outPerCssRow);
        const byPixels = Math.floor(MAX_PIXELS / (outWidth * outPerCssRow));
        const tileHeight = Math.max(1, Math.min(MAX_DIMENSION, byDimension, byPixels));
        const tileCount = Math.ceil(height / tileHeight);
        const stitchedPixels = outWidth * fullOutHeight;

        log(`tiling: ${tileCount} tile(s) of ${tileHeight} css px`);
        if (stitchedPixels > MAX_STITCH_PIXELS) {
          throw new Error(
            `tiled output would be ${outWidth}x${fullOutHeight} ` +
              `(${(stitchedPixels / 1e6).toFixed(0)} Mpx), above the ` +
              `${(MAX_STITCH_PIXELS / 1e6).toFixed(0)} Mpx in-memory stitch budget; ` +
              `retry with a larger --scale or --device to shrink it`,
          );
        }

        // Not every cap is modellable: a `repeating-linear-gradient` background
        // still fails at 1280x32768 css even at scale 0.5 (measured), i.e. that
        // paint path ignores `scale`. So a tile that still fails is halved and
        // retried rather than trusting the arithmetic, and the next tile starts
        // where the shrunken one actually ended.
        let y = 0;
        while (y < height) {
          let tileCssHeight = Math.min(tileHeight, height - y);
          for (;;) {
            try {
              // eslint-disable-next-line no-await-in-loop
              tiles.push({ y, cssHeight: tileCssHeight, buf: await capture(clip(y, tileCssHeight)) });
              break;
            } catch (e) {
              if (!/too large/i.test(e.message) || tileCssHeight <= 1) throw e;
              log(`tile y=${y} h=${tileCssHeight} too large; retrying at half height`);
              tileCssHeight = Math.floor(tileCssHeight / 2) || 1;
            }
          }
          y += tileCssHeight;
        }
      }

      if (tiles.length === 1) {
        const file = `${base}.png`;
        mkdirSync(dirname(file), { recursive: true });
        writeFileSync(file, tiles[0].buf);
        const size = pngSize(tiles[0].buf);
        console.error(
          `✓ png ${size.width}x${size.height} (css ${width}x${height}, 1 tile)`,
        );
        console.log(file);
      } else {
        let stitched = null;
        try {
          stitched = stitchVertical(tiles.map((tile) => tile.buf));
        } catch (e) {
          log("stitch failed:", e.message);
          console.error(`! could not stitch tiles (${e.message}); writing them separately`);
        }

        if (stitched) {
          const file = `${base}.png`;
          mkdirSync(dirname(file), { recursive: true });
          writeFileSync(file, stitched);
          const size = pngSize(stitched);
          console.error(
            `✓ png ${size.width}x${size.height} (css ${width}x${height}, ${tiles.length} tiles stitched)`,
          );
          console.log(file);
        } else {
          mkdirSync(dirname(base), { recursive: true });
          const paths = tiles.map((tile, index) => {
            const file = `${base}.${index + 1}.png`;
            writeFileSync(file, tile.buf);
            return file;
          });
          const first = pngSize(tiles[0].buf);
          console.error(
            `✓ ${tiles.length} PNG tiles written (each ${first.width}x, css ${width}x${height})`,
          );
          for (const file of paths) console.log(file);
          console.log(`tiles: ${tiles.length}`);
          console.log(`css: ${width}x${height}`);
        }
      }
    }
  } finally {
    if (temporaryEmulation) {
      log("restoring previous emulation");
      try {
        if (readEmulationPreference()) await applyActiveEmulation(cdp, null);
        else await clearDeviceEmulation(cdp, null);
      } catch (e) {
        log("failed to restore emulation:", e.message);
      }
    }
  }
} catch (e) {
  console.error("✗", e.message);
  process.exitCode = 1;
} finally {
  clearTimeout(globalTimeout);
}
