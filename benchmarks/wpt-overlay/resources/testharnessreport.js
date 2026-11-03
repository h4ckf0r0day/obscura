// Obscura WPT report overlay.
//
// This file replaces wpt/resources/testharnessreport.js. testharness.js loads it
// after the harness is defined and expects it to wire up result reporting. We use
// it to dump the final results onto window.__wptresults_json as a JSON string so
// the CDP runner can read them back with a single Runtime.evaluate call.
//
// It also captures console output and uncaught errors in-page. Obscura's CDP
// server does not emit Runtime.consoleAPICalled / Runtime.exceptionThrown, so
// in-page capture is the only way to surface the real root cause (an uncaught
// exception from a missing DOM/JS API is the most common conformance failure).
// The runner folds these into each result for the triage bug catcher.
//
// Shape:
//   {"harness":{"status":<int>,"message":<str|null>,"stack":<str|null>},
//    "tests":[{"name":<str>,"status":<int>,"message":<str|null>,"stack":<str|null>}, ...],
//    "console":[<str>, ...], "errors":[<str>, ...]}
//
// Keep this ES5-ish: no modules, no arrow functions, no let/const. It has to run in
// whatever JS environment the test page provides.

(function () {
  "use strict";

  var MAX_LINES = 100;
  var MAX_LEN = 2000;
  var consoleLines = [];
  var errorLines = [];

  // Disable testharness's in-page results rendering. We read results only via
  // add_completion_callback below, never the DOM results table. The table path
  // (Tests.result -> notify_result -> output) throws on DOM APIs Obscura does
  // not fully implement, and that exception aborts completion and strands the
  // whole file with no result. wptrunner likewise runs with output off.
  try {
    if (typeof setup === "function") {
      setup({ output: false });
    }
  } catch (e) {
    // testharness not loaded yet, or setup missing; harmless.
  }

  function clamp(s) {
    s = String(s);
    if (s.length > MAX_LEN) {
      s = s.slice(0, MAX_LEN) + "...";
    }
    return s;
  }

  function record(arr, line) {
    if (arr.length < MAX_LINES) {
      try {
        arr.push(clamp(line));
      } catch (e) {
        // ignore a value that cannot be stringified
      }
    }
  }

  // Install capture as early as possible so errors thrown by the test's own
  // scripts (which run after this file) are seen. These listeners are passive:
  // we never preventDefault, so testharness.js still does its own bookkeeping.
  (function installCapture() {
    try {
      var levels = ["error", "warn", "log", "info"];
      for (var i = 0; i < levels.length; i++) {
        (function (level) {
          var orig = console[level];
          console[level] = function () {
            try {
              var parts = [];
              for (var j = 0; j < arguments.length; j++) {
                parts.push(String(arguments[j]));
              }
              record(consoleLines, "[" + level + "] " + parts.join(" "));
            } catch (e) {
              // never let capture break the page
            }
            if (typeof orig === "function") {
              try {
                return orig.apply(console, arguments);
              } catch (e) {
                // some environments have a non-callable original
              }
            }
          };
        })(levels[i]);
      }
    } catch (e) {
      // console may be locked down; continue
    }

    try {
      window.addEventListener("error", function (ev) {
        var where = "";
        if (ev && ev.filename) {
          where = " (" + ev.filename + ":" + (ev.lineno || 0) + ":" + (ev.colno || 0) + ")";
        }
        var stack = ev && ev.error && ev.error.stack ? " " + ev.error.stack : "";
        var msg = ev && ev.message ? ev.message : "uncaught error";
        record(errorLines, msg + where + stack);
      }, true);
    } catch (e) {
      // addEventListener may be missing; fall back below
    }

    try {
      window.addEventListener("unhandledrejection", function (ev) {
        var reason = ev && ev.reason !== undefined ? ev.reason : "unhandled rejection";
        var stack = reason && reason.stack ? " " + reason.stack : "";
        record(errorLines, "unhandledrejection: " + reason + stack);
      }, true);
    } catch (e) {
      // ignore
    }

    // window.onerror fallback for environments without addEventListener.
    try {
      var priorOnError = window.onerror;
      window.onerror = function (message, source, lineno, colno, error) {
        var stack = error && error.stack ? " " + error.stack : "";
        record(errorLines, String(message) + " (" + source + ":" + lineno + ":" + colno + ")" + stack);
        if (typeof priorOnError === "function") {
          return priorOnError.apply(this, arguments);
        }
        return false;
      };
    } catch (e) {
      // ignore
    }
  })();

  // Always start as null so the runner can distinguish "not done yet" from "done".
  try {
    window.__wptresults_json = null;
  } catch (e) {
    // window might not be writable in some odd contexts; ignore and continue.
  }

  function str_or_null(v) {
    if (v === undefined || v === null) {
      return null;
    }
    try {
      return String(v);
    } catch (e) {
      return null;
    }
  }

  function int_or_zero(v) {
    var n = parseInt(v, 10);
    if (isNaN(n)) {
      return 0;
    }
    return n;
  }

  function build_payload(tests, harness_status) {
    var out = {
      harness: { status: 0, message: null, stack: null },
      tests: [],
      console: consoleLines.slice(0, MAX_LINES),
      errors: errorLines.slice(0, MAX_LINES)
    };

    try {
      if (harness_status) {
        out.harness.status = int_or_zero(harness_status.status);
        out.harness.message = str_or_null(harness_status.message);
        out.harness.stack = str_or_null(harness_status.stack);
      }
    } catch (e) {
      // Leave the defaults in place.
    }

    try {
      if (tests && tests.length) {
        for (var i = 0; i < tests.length; i++) {
          var t = tests[i];
          if (!t) {
            continue;
          }
          out.tests.push({
            name: str_or_null(t.name),
            status: int_or_zero(t.status),
            message: str_or_null(t.message),
            stack: str_or_null(t.stack)
          });
        }
      }
    } catch (e) {
      // Partial test list is better than none.
    }

    return out;
  }

  function on_complete(tests, harness_status) {
    try {
      var payload = build_payload(tests, harness_status);
      window.__wptresults_json = JSON.stringify(payload);
    } catch (e) {
      // Last-resort fallback so the runner sees something rather than hanging.
      try {
        window.__wptresults_json = JSON.stringify({
          harness: { status: 1, message: "report serialization failed: " + e, stack: null },
          tests: [],
          console: consoleLines.slice(0, MAX_LINES),
          errors: errorLines.slice(0, MAX_LINES)
        });
      } catch (e2) {
        // Give up. Runner will time out and record that.
      }
    }
  }

  function register() {
    if (typeof add_completion_callback === "function") {
      try {
        add_completion_callback(on_complete);
        return true;
      } catch (e) {
        return false;
      }
    }
    return false;
  }

  // add_completion_callback is usually defined by the time this script runs, but
  // load ordering is not guaranteed across every WPT variant, so poll until it
  // shows up and then stop.
  if (!register()) {
    var attempts = 0;
    var timer = setInterval(function () {
      attempts++;
      if (register() || attempts > 200) {
        clearInterval(timer);
      }
    }, 25);
  }
})();
