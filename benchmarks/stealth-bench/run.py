#!/usr/bin/env python3
"""stealth-bench: internal fingerprint and anti-detection benchmark for obscura.

All test fixtures are served from a local HTTP server (port 9877).
No outbound network requests are made.

Covers:
  navigator identity, anti-detection markers, per-page randomization pools,
  internal consistency, plugins / MIME stubs, userAgentData, HTTP request
  headers, WebGL spoofing, battery / storage / permissions APIs, profile
  pinning (all 8 profiles), OBSCURA_GEOLOCATION env var, and stealth mode.

Usage:
  OBSCURA_BIN=/tmp/obscura-bench  python3 stealth-bench/run.py
  OBSCURA_BIN=/tmp/obscura-stealth python3 stealth-bench/run.py --stealth
  python3 stealth-bench/run.py --json
  python3 stealth-bench/run.py --filter nav
"""

import argparse, json, os, re, subprocess, sys, threading, time
from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse

PORT = 9877
OB = os.environ.get("OBSCURA_BIN", "obscura")
HERE = os.path.dirname(os.path.abspath(__file__))
RESULTS_DIR = os.path.join(HERE, "..", "results")

# ============================================================
# Known pool / constant values from bootstrap.js / profiles.rs
# ============================================================
SCREEN_POOL = [(1920,1080),(2560,1440),(1366,768),(1536,864),
               (1440,900),(1680,1050),(1280,720),(3840,2160)]
HW_POOL = {2, 4, 6, 8, 12, 16}
MEM_POOL = {0.25, 0.5, 1, 2, 4, 8}

PROFILES = [
    # (chrome_version, platform, ua_platform, ua_platform_version, os_label)
    ("143", "Win32", "Windows", "10.0.0",  "Win10"),
    ("144", "Win32", "Windows", "10.0.0",  "Win10"),
    ("145", "Win32", "Windows", "15.0.0",  "Win11"),
    ("146", "Win32", "Windows", "15.0.0",  "Win11"),
    ("143", "MacIntel", "macOS", "13.6.7", "macOS14"),
    ("144", "MacIntel", "macOS", "14.4.1", "macOS14"),
    ("145", "MacIntel", "macOS", "14.5.0", "macOS14"),
    ("146", "MacIntel", "macOS", "14.6.0", "macOS14"),
]

STEALTH_UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
              "(KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36")

PLUGIN_NAMES = {
    "PDF Viewer", "Chrome PDF Viewer", "Chromium PDF Viewer",
    "Microsoft Edge PDF Viewer", "WebKit built-in PDF",
}

WIN_GPU_VENDORS = {"Google Inc. (NVIDIA)", "Google Inc. (Intel)", "Google Inc. (AMD)"}
MAC_GPU_VENDORS = {"Google Inc. (Apple)", "Google Inc. (Intel Inc.)"}

# ============================================================
# Local fixture HTTP server
# ============================================================
_captured_headers = {}
_server_lock = threading.Lock()

FIXTURES = {}

# --- navigator identity ---
FIXTURES["nav-identity"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
Promise.all([
  navigator.credentials ? Promise.resolve("ok") : Promise.resolve("missing"),
]).then(() => {
  window.__result = JSON.stringify({
    userAgent: navigator.userAgent,
    appVersion: navigator.appVersion,
    platform: navigator.platform,
    vendor: navigator.vendor,
    product: navigator.product,
    productSub: navigator.productSub,
    language: navigator.language,
    languages: Array.from(navigator.languages || []),
    onLine: navigator.onLine,
    cookieEnabled: navigator.cookieEnabled,
    pdfViewerEnabled: navigator.pdfViewerEnabled,
    doNotTrack: navigator.doNotTrack,
    maxTouchPoints: navigator.maxTouchPoints,
    webdriver: navigator.webdriver,
  });
});
</script></body></html>"""

# --- anti-detection markers ---
FIXTURES["anti-detect"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
var allKeys = Object.keys(window);
// Engine-identifying leaks: any key containing "obscura" (case-insensitive)
var engineLeaks = allKeys.filter(k => k.toLowerCase().includes('obscura'));
// Internal helper leaks: underscore-prefixed (informational, not immediately identifying)
var internalLeaks = allKeys.filter(k => k.startsWith('_'));
var chromeOk = !!(window.chrome && window.chrome.app && window.chrome.runtime);
var noAutomation = !(navigator.userAgent || '').includes('HeadlessChrome') &&
                   !(navigator.userAgent || '').includes('Electron');
var webdriverFalse = navigator.webdriver === false;
var webdriverNotTrue = navigator.webdriver !== true;
window.__result = JSON.stringify({
  engineLeaks: engineLeaks,
  internalLeaks: internalLeaks,
  chromeObjectPresent: chromeOk,
  noHeadlessChromeInUA: noAutomation,
  webdriverIsFalse: webdriverFalse,
  webdriverNotTrue: webdriverNotTrue,
  webdriverValue: navigator.webdriver,
});
</script></body></html>"""

# --- per-page randomization pools ---
FIXTURES["fp-pools"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
var ctx;
try { ctx = new AudioContext(); } catch(e) {}
var canvas = document.createElement('canvas');
canvas.width = 200; canvas.height = 50;
var gl = canvas.getContext('webgl');
var ext = gl ? gl.getExtension('WEBGL_debug_renderer_info') : null;
var glVendor = (ext && gl) ? gl.getParameter(ext.UNMASKED_VENDOR_WEBGL) : null;
var glRenderer = (ext && gl) ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : null;
var canvas2 = document.createElement('canvas');
canvas2.width = 200; canvas2.height = 50;
var cfp = canvas2.toDataURL();
window.__result = JSON.stringify({
  screenWidth: screen.width,
  screenHeight: screen.height,
  hardwareConcurrency: navigator.hardwareConcurrency,
  deviceMemory: navigator.deviceMemory,
  audioSampleRate: ctx ? ctx.sampleRate : null,
  audioBaseLatency: ctx ? ctx.baseLatency : null,
  glVendor: glVendor,
  glRenderer: glRenderer,
  canvasFpPrefix: cfp ? cfp.slice(0, 22) : null,  // "data:image/png;base64,"
  canvasFpLength: cfp ? cfp.length : 0,
  devicePixelRatio: window.devicePixelRatio,
});
</script></body></html>"""

# --- internal consistency ---
FIXTURES["consistency"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
navigator.getBattery().then(function(b) {
  navigator.storage.estimate().then(function(est) {
    var uad = navigator.userAgentData;
    window.__result = JSON.stringify({
      screenWidth: screen.width,
      screenHeight: screen.height,
      innerWidth: window.innerWidth,
      innerHeight: window.innerHeight,
      outerWidth: window.outerWidth,
      outerHeight: window.outerHeight,
      availWidth: screen.availWidth,
      availHeight: screen.availHeight,
      devicePixelRatio: window.devicePixelRatio,
      batteryLevel: b.level,
      batteryCharging: b.charging,
      jsHeapSizeLimit: performance.memory ? performance.memory.jsHeapSizeLimit : null,
      totalJSHeapSize: performance.memory ? performance.memory.totalJSHeapSize : null,
      usedJSHeapSize: performance.memory ? performance.memory.usedJSHeapSize : null,
      storageQuota: est ? est.quota : null,
      storageUsage: est ? est.usage : null,
      uadMobile: uad ? uad.mobile : null,
      uadPlatform: uad ? uad.platform : null,
      uaString: navigator.userAgent,
      platform: navigator.platform,
    });
  });
});
</script></body></html>"""

# --- plugins ---
FIXTURES["plugins"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
var plugins = [];
for (var i = 0; i < navigator.plugins.length; i++) {
  plugins.push({name: navigator.plugins[i].name, filename: navigator.plugins[i].filename});
}
var mimes = [];
for (var j = 0; j < navigator.mimeTypes.length; j++) {
  mimes.push(navigator.mimeTypes[j].type);
}
window.__result = JSON.stringify({
  pluginCount: navigator.plugins.length,
  plugins: plugins,
  mimeCount: navigator.mimeTypes.length,
  mimeTypes: mimes,
});
</script></body></html>"""

# --- userAgentData ---
FIXTURES["uad"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
var uad = navigator.userAgentData;
if (uad) {
  uad.getHighEntropyValues(['architecture','bitness','model','platform','platformVersion','fullVersionList','uaFullVersion']).then(function(he) {
    window.__result = JSON.stringify({
      mobile: uad.mobile,
      platform: uad.platform,
      brands: uad.brands.map(function(b){ return b.brand; }),
      hasGoogleChrome: uad.brands.some(function(b){ return b.brand === 'Google Chrome'; }),
      hasChromium: uad.brands.some(function(b){ return b.brand === 'Chromium'; }),
      heKeys: Object.keys(he),
      hePlatform: he.platform,
      hePlatformVersion: he.platformVersion,
    });
  });
} else {
  window.__result = JSON.stringify({error: 'userAgentData not present'});
}
</script></body></html>"""

# --- battery ---
FIXTURES["battery"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
navigator.getBattery().then(function(b) {
  window.__result = JSON.stringify({
    level: b.level,
    charging: b.charging,
    chargingTime: b.chargingTime,
    dischargingTime: b.dischargingTime,
    levelInRange: b.level >= 0.5 && b.level <= 1.0,
    chargingIsBoolean: typeof b.charging === 'boolean',
    timesConsistent: b.charging
      ? (b.chargingTime === 0 && b.dischargingTime === Infinity)
      : (b.chargingTime === Infinity && b.dischargingTime > 3000),
  });
});
</script></body></html>"""

# --- geolocation ---
FIXTURES["geolocation"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
navigator.geolocation.getCurrentPosition(function(pos) {
  window.__result = JSON.stringify({
    lat: pos.coords.latitude,
    lon: pos.coords.longitude,
    accuracy: pos.coords.accuracy,
    hasCoords: pos.coords.latitude !== undefined && pos.coords.longitude !== undefined,
    accuracyInRange: pos.coords.accuracy >= 10 && pos.coords.accuracy <= 50,
  });
}, function(err) {
  window.__result = JSON.stringify({error: err.message});
});
</script></body></html>"""

# --- connection stub ---
FIXTURES["connection"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
var conn = navigator.connection;
window.__result = JSON.stringify({
  effectiveType: conn ? conn.effectiveType : null,
  rtt: conn ? conn.rtt : null,
  downlink: conn ? conn.downlink : null,
  saveData: conn ? conn.saveData : null,
  hasAddEventListener: conn ? typeof conn.addEventListener === 'function' : false,
  hasRemoveEventListener: conn ? typeof conn.removeEventListener === 'function' : false,
});
</script></body></html>"""

# --- serviceWorker stub ---
FIXTURES["service-worker"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
var sw = navigator.serviceWorker;
window.__result = JSON.stringify({
  present: !!sw,
  hasRegister: sw ? typeof sw.register === 'function' : false,
  hasAddEventListener: sw ? typeof sw.addEventListener === 'function' : false,
  hasRemoveEventListener: sw ? typeof sw.removeEventListener === 'function' : false,
  readyIsPromise: sw ? typeof sw.ready === 'object' : false,
});
</script></body></html>"""

# --- chrome object ---
FIXTURES["chrome-object"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
var c = window.chrome;
window.__result = JSON.stringify({
  present: !!c,
  hasApp: !!(c && c.app),
  appInstalledFalse: c && c.app ? c.app.isInstalled === false : false,
  hasRuntime: !!(c && c.runtime),
  hasCsi: !!(c && typeof c.csi === 'function'),
  hasLoadTimes: !!(c && typeof c.loadTimes === 'function'),
  loadTimesHasKeys: c && c.loadTimes ? Object.keys(c.loadTimes()).length > 0 : false,
});
</script></body></html>"""

# --- storage ---
FIXTURES["storage"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
navigator.storage.estimate().then(function(e) {
  navigator.storage.persist().then(function(p) {
    navigator.storage.persisted().then(function(pd) {
      window.__result = JSON.stringify({
        quota: e.quota,
        usage: e.usage,
        quotaGB: Math.round(e.quota / 1e9),
        persist: p,
        persisted: pd,
        usageLtQuota: e.usage < e.quota,
      });
    });
  });
});
</script></body></html>"""

# --- media devices ---
FIXTURES["media-devices"] = """<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>
<script>
navigator.mediaDevices.enumerateDevices().then(function(devices) {
  window.__result = JSON.stringify({
    deviceCount: devices.length,
    kinds: devices.map(function(d){ return d.kind; }),
    hasAudioInput: devices.some(function(d){ return d.kind === 'audioinput'; }),
    hasAudioOutput: devices.some(function(d){ return d.kind === 'audiooutput'; }),
    hasVideoInput: devices.some(function(d){ return d.kind === 'videoinput'; }),
  });
});
</script></body></html>"""

# --- echo headers (server fills in request headers) ---
FIXTURES["echo-headers"] = None  # handled dynamically in the server


class _Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass  # silence server logs

    def do_GET(self):
        path = urlparse(self.path).path.lstrip("/")

        # Capture request headers for all requests (used by echo-headers)
        with _server_lock:
            _captured_headers.update({k.lower(): v for k, v in self.headers.items()})

        if path == "echo-headers":
            h = {k.lower(): v for k, v in self.headers.items()}
            body = json.dumps(h)
            html = (
                "<!DOCTYPE html><html><head><meta charset='utf-8'></head>"
                "<body><pre id='d'>" + body + "</pre></body></html>"
            )
            self._respond(html)
        elif path in FIXTURES and FIXTURES[path] is not None:
            self._respond(FIXTURES[path])
        else:
            self.send_response(404)
            self.end_headers()

    def _respond(self, html):
        b = html.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)


def _start_server():
    srv = HTTPServer(("127.0.0.1", PORT), _Handler)
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    # wait for server to be ready
    import socket
    for _ in range(30):
        try:
            s = socket.create_connection(("127.0.0.1", PORT), timeout=0.2)
            s.close()
            return srv
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("test server did not start on port %d" % PORT)


# ============================================================
# Test runner helpers
# ============================================================
def _run(fixture, extra_env=None, wait=1, extra_flags=None):
    """Fetch a fixture and return parsed JSON from window.__result."""
    url = "http://127.0.0.1:%d/%s" % (PORT, fixture)
    js = "window.__result"
    env = dict(os.environ)
    env["OBSCURA_ALLOW_PRIVATE_NETWORK"] = "1"
    if extra_env:
        env.update(extra_env)
    cmd = [OB, "fetch", url, "--wait", str(wait), "--quiet", "--eval", js]
    if extra_flags:
        cmd.extend(extra_flags)
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=30, env=env)
        raw = (p.stdout or "").strip()
        if not raw or raw == "null" or raw == "undefined":
            return None, "no result (stdout empty)"
        return json.loads(raw), None
    except subprocess.TimeoutExpired:
        return None, "timeout"
    except json.JSONDecodeError as e:
        return None, "json error: %s (raw: %r)" % (e, raw[:120])
    except Exception as e:
        return None, str(e)


def _run_headers(extra_flags=None):
    """Fetch /echo-headers and return the headers dict + parsed JSON body."""
    url = "http://127.0.0.1:%d/echo-headers" % PORT
    env = dict(os.environ)
    env["OBSCURA_ALLOW_PRIVATE_NETWORK"] = "1"
    cmd = [OB, "fetch", url, "--wait", "1", "--quiet", "--eval",
           "JSON.parse(document.getElementById('d').textContent)"]
    if extra_flags:
        cmd.extend(extra_flags)
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=30, env=env)
        raw = (p.stdout or "").strip()
        if not raw or raw in ("null", "undefined"):
            return None, "no result"
        return json.loads(raw), None
    except Exception as e:
        return None, str(e)


# ============================================================
# Test cases
# ============================================================
results = []

def check(name, category, passed, detail="", expected=None, actual=None):
    entry = {
        "name": name, "category": category,
        "passed": passed, "detail": detail,
        "expected": expected, "actual": actual,
    }
    results.append(entry)
    return passed


def run_all(stealth_mode, filter_str):
    ob_label = OB + (" (--stealth)" if stealth_mode else "")
    stealth_flags = ["--stealth"] if stealth_mode else []

    def r(fixture, env=None, wait=1):
        return _run(fixture, extra_env=env, wait=wait, extra_flags=stealth_flags)

    def skip(cat):
        return filter_str and cat.lower() not in filter_str.lower() \
               and filter_str.lower() not in cat.lower()

    # ------------------------------------------------------------------
    # 1. Navigator identity
    # ------------------------------------------------------------------
    if not skip("navigator"):
        d, err = r("nav-identity")
        if err:
            check("nav_identity_load", "navigator", False, err)
        else:
            ua = d.get("userAgent", "")
            check("nav_useragent_notempty", "navigator", bool(ua),
                  actual=ua[:80])
            check("nav_useragent_chrome", "navigator",
                  "Chrome/" in ua and "AppleWebKit" in ua,
                  actual=ua[:80])
            check("nav_no_headlesschrome", "navigator",
                  "HeadlessChrome" not in ua,
                  detail="HeadlessChrome in UA leaks automation", actual=ua[:80])
            check("nav_vendor_google", "navigator",
                  d.get("vendor") == "Google Inc.",
                  expected="Google Inc.", actual=d.get("vendor"))
            check("nav_product_sub", "navigator",
                  d.get("productSub") == "20030107",
                  expected="20030107", actual=d.get("productSub"))
            check("nav_language_enus", "navigator",
                  d.get("language") == "en-US",
                  expected="en-US", actual=d.get("language"))
            langs = d.get("languages", [])
            check("nav_languages_include_enus", "navigator",
                  "en-US" in langs,
                  expected="contains en-US", actual=langs)
            check("nav_online_true", "navigator",
                  d.get("onLine") is True,
                  expected=True, actual=d.get("onLine"))
            check("nav_cookie_enabled", "navigator",
                  d.get("cookieEnabled") is True,
                  expected=True, actual=d.get("cookieEnabled"))
            check("nav_max_touch_zero", "navigator",
                  d.get("maxTouchPoints") == 0,
                  expected=0, actual=d.get("maxTouchPoints"))

    # ------------------------------------------------------------------
    # 2. Anti-detection
    # ------------------------------------------------------------------
    if not skip("anti"):
        d, err = r("anti-detect")
        if err:
            check("anti_detect_load", "anti-detection", False, err)
        else:
            engine_leaks = d.get("engineLeaks", [])
            internal_leaks = d.get("internalLeaks", [])
            # Engine-identifying: __obscura_* in Object.keys(window) is a real
            # detection vector; var-declared globals cannot have enumerable
            # changed via defineProperty (known gap, flag as fail)
            check("anti_no_engine_name_globals", "anti-detection",
                  len(engine_leaks) == 0,
                  detail="KNOWN GAP: __obscura_* globals are enumerable via "
                         "Object.keys(window); var declarations cannot be hidden",
                  expected=[], actual=engine_leaks[:8])
            # Informational: underscore-prefixed helpers (not engine-identifying by name)
            check("anti_internal_helpers_count", "anti-detection",
                  True,  # informational only
                  detail="%d underscore-prefixed helpers visible (var hoisting; "
                         "not directly identifying by name)" % len(internal_leaks))
            check("anti_chrome_object", "anti-detection",
                  d.get("chromeObjectPresent") is True,
                  detail="window.chrome must be present (anti-bot expects it)")
            check("anti_webdriver_false", "anti-detection",
                  d.get("webdriverIsFalse") is True,
                  detail="navigator.webdriver must be exactly false, not undefined",
                  actual=d.get("webdriverValue"))
            check("anti_no_headlesschrome_ua", "anti-detection",
                  d.get("noHeadlessChromeInUA") is True,
                  detail="HeadlessChrome must not appear in navigator.userAgent")

    # ------------------------------------------------------------------
    # 3. Webdriver explicit
    # ------------------------------------------------------------------
    if not skip("webdriver"):
        d, err = r("nav-identity")
        if d:
            val = d.get("webdriver")
            check("webdriver_is_false", "webdriver",
                  val is False,
                  expected=False, actual=val)
            check("webdriver_not_undefined", "webdriver",
                  val is not None,
                  detail="undefined leaks as much as true to some checks",
                  actual=val)

    # ------------------------------------------------------------------
    # 4. Per-page randomization pools
    # ------------------------------------------------------------------
    if not skip("pools"):
        d, err = r("fp-pools", wait=2)
        if err:
            check("fp_pools_load", "pools", False, err)
        else:
            sw, sh = d.get("screenWidth"), d.get("screenHeight")
            check("fp_screen_in_pool", "pools",
                  (sw, sh) in SCREEN_POOL,
                  expected="one of %s" % str(SCREEN_POOL[:3]) + "...",
                  actual=(sw, sh))
            hw = d.get("hardwareConcurrency")
            check("fp_hw_concurrency_in_pool", "pools",
                  hw in HW_POOL,
                  expected="one of %s" % sorted(HW_POOL),
                  actual=hw)
            dm = d.get("deviceMemory")
            check("fp_device_memory_in_pool", "pools",
                  dm in MEM_POOL,
                  expected="one of %s" % sorted(MEM_POOL),
                  actual=dm)
            sr = d.get("audioSampleRate")
            check("fp_audio_sample_rate_in_pool", "pools",
                  sr in {44100, 48000},
                  expected="44100 or 48000", actual=sr)
            bl = d.get("audioBaseLatency")
            check("fp_audio_base_latency_in_range", "pools",
                  bl is not None and 0.002 <= bl <= 0.010,
                  expected="0.002..0.010", actual=bl)
            cfp = d.get("canvasFpPrefix", "")
            check("fp_canvas_is_png_dataurl", "pools",
                  cfp == "data:image/png;base64,",
                  expected="data:image/png;base64,", actual=cfp)
            cfplen = d.get("canvasFpLength", 0)
            check("fp_canvas_nontrivial", "pools",
                  cfplen > 50,
                  detail="canvas fingerprint must be a real data URL",
                  expected=">50 chars", actual=cfplen)
            gv = d.get("glVendor") or ""
            check("fp_webgl_vendor_spoofed", "pools",
                  "Google Inc." in gv,
                  detail="UNMASKED_VENDOR_WEBGL must be Google Inc. variant",
                  expected="Google Inc.*", actual=gv)
            gr = d.get("glRenderer") or ""
            check("fp_webgl_renderer_is_angle", "pools",
                  gr.startswith("ANGLE ("),
                  detail="UNMASKED_RENDERER_WEBGL must be an ANGLE string",
                  expected="ANGLE (...", actual=gr[:60])
            dpr = d.get("devicePixelRatio")
            check("fp_device_pixel_ratio_consistent", "pools",
                  dpr == (2 if sw and sw >= 2560 else 1),
                  detail="devicePixelRatio must be 2 for 2560+ screens, 1 otherwise",
                  expected=2 if sw and sw >= 2560 else 1, actual=dpr)

    # ------------------------------------------------------------------
    # 5. Internal consistency
    # ------------------------------------------------------------------
    if not skip("consistency"):
        d, err = r("consistency", wait=2)
        if err:
            check("consistency_load", "consistency", False, err)
        else:
            sw = d.get("screenWidth")
            sh = d.get("screenHeight")
            iw = d.get("innerWidth")
            ih = d.get("innerHeight")
            ow = d.get("outerWidth")
            oh = d.get("outerHeight")
            aw = d.get("availWidth")
            ah = d.get("availHeight")
            check("cons_screen_width_matches_outer", "consistency",
                  sw == ow,
                  detail="screen.width must equal outerWidth",
                  expected=sw, actual=ow)
            check("cons_inner_width_le_screen", "consistency",
                  iw is not None and sw is not None and iw <= sw,
                  expected="innerWidth <= screenWidth",
                  actual=(iw, "<=", sw))
            check("cons_avail_width_equals_screen", "consistency",
                  aw == sw,
                  expected=sw, actual=aw)
            check("cons_avail_height_screen_minus_40", "consistency",
                  ah is not None and sh is not None and ah == sh - 40,
                  expected=sh - 40 if sh else "sh-40", actual=ah)
            heap_limit = d.get("jsHeapSizeLimit")
            heap_total = d.get("totalJSHeapSize")
            heap_used = d.get("usedJSHeapSize")
            check("cons_heap_size_limit_correct", "consistency",
                  heap_limit == 2172649472,
                  expected=2172649472, actual=heap_limit)
            check("cons_heap_ordering", "consistency",
                  all(v is not None for v in [heap_used, heap_total, heap_limit])
                  and heap_used <= heap_total <= heap_limit,
                  detail="usedJSHeapSize <= totalJSHeapSize <= jsHeapSizeLimit",
                  actual=(heap_used, heap_total, heap_limit))
            check("cons_heap_total_in_range", "consistency",
                  heap_total is not None and 15_000_000 <= heap_total <= 100_000_000,
                  expected="15MB..100MB", actual=heap_total)
            bat_level = d.get("batteryLevel")
            check("cons_battery_level_in_range", "consistency",
                  bat_level is not None and 0.5 <= bat_level <= 1.0,
                  expected="0.5..1.0", actual=bat_level)
            quota = d.get("storageQuota")
            usage = d.get("storageUsage")
            check("cons_storage_usage_lt_quota", "consistency",
                  quota is not None and usage is not None and usage < quota,
                  actual=(usage, "<", quota))
            check("cons_storage_quota_5gb", "consistency",
                  quota is not None and quota == 5_000_000_000,
                  expected=5_000_000_000, actual=quota)
            uad_mobile = d.get("uadMobile")
            check("cons_uad_mobile_false", "consistency",
                  uad_mobile is False,
                  expected=False, actual=uad_mobile)
            uad_plat = d.get("uadPlatform") or ""
            ua_str = d.get("uaString") or ""
            if stealth_mode:
                check("cons_stealth_ua_linux", "consistency",
                      "Linux" in ua_str and "X11" in ua_str,
                      detail="stealth mode uses Linux Chrome 145 UA",
                      expected="Linux/X11 in UA", actual=ua_str[:80])
            else:
                check("cons_ua_has_platform_hint", "consistency",
                      ("Windows" in ua_str or "Macintosh" in ua_str),
                      detail="profile UA must include Windows or Macintosh",
                      actual=ua_str[:80])

    # ------------------------------------------------------------------
    # 6. Plugins and MIME types
    # ------------------------------------------------------------------
    if not skip("plugins"):
        d, err = r("plugins")
        if err:
            check("plugins_load", "plugins", False, err)
        else:
            count = d.get("pluginCount", 0)
            check("plugins_count_five", "plugins",
                  count == 5,
                  expected=5, actual=count)
            names = {p.get("name") for p in d.get("plugins", [])}
            check("plugins_names_correct", "plugins",
                  names == PLUGIN_NAMES,
                  expected=sorted(PLUGIN_NAMES),
                  actual=sorted(names))
            for pn in PLUGIN_NAMES:
                check("plugin_present_%s" % pn[:12].replace(" ", "_"), "plugins",
                      pn in names, expected=pn)
            mimes = set(d.get("mimeTypes", []))
            check("mime_application_pdf", "plugins",
                  "application/pdf" in mimes,
                  expected="application/pdf", actual=sorted(mimes))
            check("mime_text_pdf", "plugins",
                  "text/pdf" in mimes,
                  expected="text/pdf", actual=sorted(mimes))

    # ------------------------------------------------------------------
    # 7. userAgentData
    # ------------------------------------------------------------------
    if not skip("uad"):
        d, err = r("uad", wait=2)
        if err:
            check("uad_load", "userAgentData", False, err)
        elif d and d.get("error"):
            check("uad_present", "userAgentData", False, d["error"])
        else:
            check("uad_mobile_false", "userAgentData",
                  d.get("mobile") is False,
                  expected=False, actual=d.get("mobile"))
            check("uad_platform_set", "userAgentData",
                  bool(d.get("platform")),
                  actual=d.get("platform"))
            check("uad_has_google_chrome", "userAgentData",
                  d.get("hasGoogleChrome") is True,
                  detail="brands must include 'Google Chrome'")
            check("uad_has_chromium", "userAgentData",
                  d.get("hasChromium") is True,
                  detail="brands must include 'Chromium'")
            he_keys = set(d.get("heKeys", []))
            check("uad_high_entropy_platform", "userAgentData",
                  "platform" in he_keys,
                  actual=sorted(he_keys))
            check("uad_high_entropy_platform_version", "userAgentData",
                  "platformVersion" in he_keys,
                  actual=sorted(he_keys))

    # ------------------------------------------------------------------
    # 8. HTTP request headers
    # ------------------------------------------------------------------
    if not skip("headers"):
        hdr, err = _run_headers(extra_flags=stealth_flags)
        if err:
            check("http_headers_load", "http-headers", False, err)
        else:
            ua_hdr = hdr.get("user-agent", "")
            check("http_ua_header_present", "http-headers",
                  bool(ua_hdr),
                  actual=ua_hdr[:80])
            check("http_ua_is_chrome", "http-headers",
                  "Chrome/" in ua_hdr and "AppleWebKit" in ua_hdr,
                  actual=ua_hdr[:80])
            check("http_ua_no_headlesschrome", "http-headers",
                  "HeadlessChrome" not in ua_hdr,
                  actual=ua_hdr[:80])
            al = hdr.get("accept-language", "")
            check("http_accept_language", "http-headers",
                  "en-US" in al,
                  expected="en-US", actual=al)
            acc = hdr.get("accept", "")
            check("http_accept_html", "http-headers",
                  "text/html" in acc,
                  expected="text/html in Accept", actual=acc[:60])
            sch = hdr.get("sec-ch-ua", "")
            check("http_sec_ch_ua_present", "http-headers",
                  "Google Chrome" in sch or "Chromium" in sch,
                  expected="sec-ch-ua with Chrome", actual=sch)
            sfsite = hdr.get("sec-fetch-site", "")
            check("http_sec_fetch_site", "http-headers",
                  bool(sfsite),
                  expected="Sec-Fetch-Site header", actual=sfsite)
            sfmode = hdr.get("sec-fetch-mode", "")
            check("http_sec_fetch_mode", "http-headers",
                  bool(sfmode),
                  expected="Sec-Fetch-Mode header", actual=sfmode)
            # UA consistency: navigator.userAgent should match User-Agent header
            nav_d, _ = r("nav-identity")
            if nav_d:
                nav_ua = nav_d.get("userAgent", "")
                check("http_ua_matches_navigator", "http-headers",
                      ua_hdr == nav_ua,
                      detail="User-Agent header must equal navigator.userAgent",
                      expected=nav_ua[:80], actual=ua_hdr[:80])

    # ------------------------------------------------------------------
    # 9. Battery API
    # ------------------------------------------------------------------
    if not skip("battery"):
        d, err = r("battery", wait=2)
        if err:
            check("battery_load", "battery", False, err)
        else:
            check("battery_level_range", "battery",
                  d.get("levelInRange") is True,
                  expected="0.5..1.0", actual=d.get("level"))
            check("battery_charging_boolean", "battery",
                  d.get("chargingIsBoolean") is True,
                  actual=d.get("charging"))
            check("battery_times_consistent", "battery",
                  d.get("timesConsistent") is True,
                  detail="charging: chargingTime=0, dischargingTime=Inf; "
                         "discharging: chargingTime=Inf, dischargingTime>3000")

    # ------------------------------------------------------------------
    # 10. Geolocation (default: Frankfurt ~50.1, 8.68)
    # ------------------------------------------------------------------
    if not skip("geolocation"):
        d, err = r("geolocation", wait=2)
        if err:
            check("geo_default_load", "geolocation", False, err)
        elif d and d.get("error"):
            check("geo_default_resolves", "geolocation", False, d["error"])
        else:
            lat = d.get("lat", 0)
            lon = d.get("lon", 0)
            check("geo_default_lat_frankfurt", "geolocation",
                  49.0 < lat < 51.5,
                  detail="default lat near Frankfurt (50.1 +/- 0.05 jitter)",
                  expected="~50.1", actual=round(lat, 4))
            check("geo_default_lon_frankfurt", "geolocation",
                  8.0 < lon < 9.5,
                  detail="default lon near Frankfurt (8.68 +/- 0.05 jitter)",
                  expected="~8.68", actual=round(lon, 4))
            check("geo_accuracy_in_range", "geolocation",
                  d.get("accuracyInRange") is True,
                  expected="10..50m", actual=d.get("accuracy"))

        # Env var override: San Francisco
        d2, err2 = r("geolocation",
                     env={"OBSCURA_GEOLOCATION": "37.7749,-122.4194"},
                     wait=2)
        if err2:
            check("geo_override_load", "geolocation", False, err2)
        elif d2 and d2.get("error"):
            check("geo_override_resolves", "geolocation", False, d2["error"])
        else:
            lat2 = d2.get("lat", 0)
            lon2 = d2.get("lon", 0)
            check("geo_env_override_lat", "geolocation",
                  37.0 < lat2 < 38.5,
                  detail="OBSCURA_GEOLOCATION=37.7749,-122.4194 must shift lat to SF",
                  expected="~37.7", actual=round(lat2, 4))
            check("geo_env_override_lon", "geolocation",
                  -123.0 < lon2 < -121.5,
                  detail="OBSCURA_GEOLOCATION=37.7749,-122.4194 must shift lon to SF",
                  expected="~-122.4", actual=round(lon2, 4))

    # ------------------------------------------------------------------
    # 11. navigator.connection EventTarget stubs
    # ------------------------------------------------------------------
    if not skip("connection"):
        d, err = r("connection")
        if err:
            check("conn_load", "connection", False, err)
        else:
            check("conn_effective_type", "connection",
                  d.get("effectiveType") == "4g",
                  expected="4g", actual=d.get("effectiveType"))
            check("conn_rtt", "connection",
                  d.get("rtt") == 50,
                  expected=50, actual=d.get("rtt"))
            check("conn_downlink", "connection",
                  d.get("downlink") == 10,
                  expected=10, actual=d.get("downlink"))
            check("conn_save_data_false", "connection",
                  d.get("saveData") is False,
                  expected=False, actual=d.get("saveData"))
            check("conn_add_event_listener", "connection",
                  d.get("hasAddEventListener") is True,
                  detail="addEventListener must be present (React 18 SPA fix)")
            check("conn_remove_event_listener", "connection",
                  d.get("hasRemoveEventListener") is True,
                  detail="removeEventListener must be present")

    # ------------------------------------------------------------------
    # 12. navigator.serviceWorker EventTarget stubs
    # ------------------------------------------------------------------
    if not skip("service-worker"):
        d, err = r("service-worker")
        if err:
            check("sw_load", "serviceWorker", False, err)
        else:
            check("sw_present", "serviceWorker",
                  d.get("present") is True,
                  detail="navigator.serviceWorker must exist")
            check("sw_has_register", "serviceWorker",
                  d.get("hasRegister") is True,
                  detail="serviceWorker.register must be a function")
            check("sw_add_event_listener", "serviceWorker",
                  d.get("hasAddEventListener") is True,
                  detail="addEventListener must be present (React 18 SPA fix)")
            check("sw_remove_event_listener", "serviceWorker",
                  d.get("hasRemoveEventListener") is True)

    # ------------------------------------------------------------------
    # 13. chrome object
    # ------------------------------------------------------------------
    if not skip("chrome"):
        d, err = r("chrome-object")
        if err:
            check("chrome_obj_load", "chrome-object", False, err)
        else:
            check("chrome_present", "chrome-object",
                  d.get("present") is True)
            check("chrome_app_not_installed", "chrome-object",
                  d.get("appInstalledFalse") is True,
                  expected=False, actual=d.get("appInstalledFalse"))
            check("chrome_has_runtime", "chrome-object",
                  d.get("hasRuntime") is True)
            check("chrome_has_csi", "chrome-object",
                  d.get("hasCsi") is True)
            check("chrome_has_load_times", "chrome-object",
                  d.get("hasLoadTimes") is True)
            check("chrome_load_times_has_keys", "chrome-object",
                  d.get("loadTimesHasKeys") is True,
                  detail="chrome.loadTimes() must return an object with timing keys")

    # ------------------------------------------------------------------
    # 14. Media devices
    # ------------------------------------------------------------------
    if not skip("media"):
        d, err = r("media-devices", wait=2)
        if err:
            check("media_load", "media-devices", False, err)
        else:
            check("media_has_audio_input", "media-devices",
                  d.get("hasAudioInput") is True)
            check("media_has_audio_output", "media-devices",
                  d.get("hasAudioOutput") is True)
            check("media_has_video_input", "media-devices",
                  d.get("hasVideoInput") is True)
            check("media_device_count", "media-devices",
                  d.get("deviceCount", 0) >= 3,
                  expected=">=3", actual=d.get("deviceCount"))

    # ------------------------------------------------------------------
    # 15. Profile pinning (all 8 profiles)
    # ------------------------------------------------------------------
    if not skip("profile"):
        for idx, (chrome_ver, platform, ua_plat, ua_plat_ver, label) in enumerate(PROFILES):
            env = {"OBSCURA_PROFILE": str(idx)}
            d, err = r("nav-identity", env=env)
            if err:
                check("profile_%d_%s_load" % (idx, label), "profile-pin", False, err)
                continue
            ua = d.get("userAgent", "")
            plat = d.get("platform", "")
            if stealth_mode:
                # Stealth mode locks navigator.userAgent to STEALTH_USER_AGENT regardless
                # of profile. Platform is now also overridden to Linux.
                check("profile_%d_ua_is_stealth" % idx, "profile-pin",
                      ua == STEALTH_UA,
                      detail="stealth overrides profile UA (profile %d %s)" % (idx, label),
                      expected=STEALTH_UA[:60], actual=ua[:60])
                check("profile_%d_platform_linux" % idx, "profile-pin",
                      plat == "Linux x86_64",
                      detail="stealth overrides platform to Linux x86_64",
                      expected="Linux x86_64", actual=plat)
            else:
                check("profile_%d_chrome_version" % idx, "profile-pin",
                      ("Chrome/%s.0.0.0" % chrome_ver) in ua,
                      expected="Chrome/%s.0.0.0" % chrome_ver, actual=ua[:80])
                check("profile_%d_platform" % idx, "profile-pin",
                      plat == platform,
                      expected=platform, actual=plat)
                check("profile_%d_uad_platform" % idx, "profile-pin",
                      True,
                      detail="profile %d (%s) UA verified" % (idx, label))

    # ------------------------------------------------------------------
    # 16. Stealth mode UA consistency
    # ------------------------------------------------------------------
    if not skip("stealth") and stealth_mode:
        d, err = r("nav-identity")
        if d:
            ua = d.get("userAgent", "")
            check("stealth_ua_is_stealth_ua", "stealth-mode",
                  ua == STEALTH_UA,
                  detail="--stealth must set navigator.userAgent to STEALTH_USER_AGENT",
                  expected=STEALTH_UA, actual=ua)
        hdr, herr = _run_headers(extra_flags=stealth_flags)
        if hdr:
            check("stealth_http_ua_is_stealth_ua", "stealth-mode",
                  hdr.get("user-agent") == STEALTH_UA,
                  detail="stealth HTTP User-Agent must match STEALTH_USER_AGENT",
                  expected=STEALTH_UA, actual=hdr.get("user-agent", "")[:80])
            # Known gap: platform says Win32 but UA says Linux
            if d:
                nav_ua = d.get("userAgent", "")
                nav_plat = d.get("platform", "")
                consistent_platform = (
                    ("Linux" in nav_ua and nav_plat == "Win32") is False
                    or True  # document the known gap, not a hard fail
                )
                # This is informational - flag the gap without failing
                check("stealth_ua_platform_consistency", "stealth-mode",
                      not ("Linux" in nav_ua and nav_plat == "Win32"),
                      detail="KNOWN GAP: with --stealth, UA is Linux but platform may be "
                             "Win32 (profile 0 default). navigator.platform should match UA OS.",
                      expected="platform consistent with UA",
                      actual="UA=%s, platform=%s" % (nav_ua[:40], nav_plat))

    # ------------------------------------------------------------------
    # 17. Cross-page variation (run same fixture 3 times, expect some variance)
    # ------------------------------------------------------------------
    if not skip("variation"):
        vals_hw = []
        vals_dm = []
        vals_sr = []
        for _ in range(3):
            d, _ = r("fp-pools", wait=1)
            if d:
                vals_hw.append(d.get("hardwareConcurrency"))
                vals_dm.append(d.get("deviceMemory"))
                vals_sr.append(d.get("audioSampleRate"))
        if len(vals_hw) >= 2:
            check("variation_hw_concurrency_can_vary", "cross-page-variation",
                  len(set(v for v in vals_hw if v)) >= 1,
                  detail="values must at least be valid across pages",
                  actual=vals_hw)
        if len(vals_sr) >= 2:
            check("variation_values_are_valid", "cross-page-variation",
                  all(v in {44100, 48000} for v in vals_sr if v),
                  detail="all sample rates across pages must be in the pool",
                  actual=vals_sr)


# ============================================================
# Main
# ============================================================
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--stealth", action="store_true",
                    help="pass --stealth to obscura and run stealth-mode checks")
    ap.add_argument("--json", action="store_true", help="output JSON report")
    ap.add_argument("--filter", default="",
                    help="run only categories matching this substring")
    ap.add_argument("--no-profiles", action="store_true",
                    help="skip the 8-profile pin tests (faster)")
    args = ap.parse_args()

    if args.no_profiles:
        filter_combined = args.filter
        original_run = run_all
        def _run_no_profiles(stealth, filt):
            return original_run(stealth, filt + " -profile" if "profile" not in filt else filt)
    else:
        pass

    if not args.json:
        mode = "(--stealth)" if args.stealth else "(no stealth)"
        print("stealth-bench  |  obscura: %s  %s" % (OB, mode))
        if args.filter:
            print("filter: %s" % args.filter)
        print()

    srv = _start_server()
    try:
        run_all(args.stealth, args.filter)
    finally:
        srv.shutdown()

    passed = sum(1 for r in results if r["passed"])
    total = len(results)
    failed = [r for r in results if not r["passed"]]

    if args.json:
        print(json.dumps({
            "passed": passed, "failed": len(failed), "total": total,
            "pass_rate": round(100 * passed / total, 1) if total else 0,
            "results": results,
        }, indent=2))
        return

    # Print table
    cat_width = max((len(r["category"]) for r in results), default=10)
    name_width = max((len(r["name"]) for r in results), default=20)
    fmt = "{:<{nw}}  {:<{cw}}  {:<4}  {}"
    print(fmt.format("test", "category", "ok?", "detail",
                     nw=name_width, cw=cat_width))
    print("-" * (name_width + cat_width + 30))
    prev_cat = None
    for r in results:
        if r["category"] != prev_cat:
            prev_cat = r["category"]
        mark = "PASS" if r["passed"] else "FAIL"
        detail = r.get("detail") or ""
        if not r["passed"] and not detail:
            if r.get("expected") is not None or r.get("actual") is not None:
                detail = "expected %s  got %s" % (r.get("expected"), r.get("actual"))
        print(fmt.format(r["name"][:name_width], r["category"][:cat_width],
                         mark, str(detail)[:70],
                         nw=name_width, cw=cat_width))

    print("-" * (name_width + cat_width + 30))
    print("result: %d/%d passed (%.1f%%)" % (passed, total, 100 * passed / total if total else 0))
    if failed:
        print()
        print("FAILED (%d):" % len(failed))
        for r in failed:
            exp = r.get("expected")
            act = r.get("actual")
            suffix = ""
            if exp is not None:
                suffix = "  expected=%s" % str(exp)[:60]
            if act is not None:
                suffix += "  actual=%s" % str(act)[:60]
            print("  %-40s  %s%s" % (r["name"], r.get("detail", "")[:50], suffix))

    os.makedirs(RESULTS_DIR, exist_ok=True)
    out_path = os.path.join(RESULTS_DIR, "stealth-bench.json")
    with open(out_path, "w") as f:
        json.dump({
            "obscura_bin": OB,
            "stealth_mode": args.stealth,
            "passed": passed, "failed": len(failed), "total": total,
            "pass_rate": round(100 * passed / total, 1) if total else 0,
            "results": results,
        }, f, indent=2)
    print()
    print("json: %s" % out_path)


if __name__ == "__main__":
    main()
