use std::sync::Arc;

use deno_core::Extension;
use deno_core::ExtensionFileSource;

fn with_esm_entry(mut ext: Extension, entry: &'static str) -> Extension {
    ext.esm_entry_point = Some(entry);
    ext
}

/// deno_url ships two ESM files (`00_url.js`, `01_urlpattern.js`) that only
/// `export` their classes — Deno's own runtime is responsible for exposing
/// them on `globalThis`. We add a synthetic entry that imports both files and
/// installs the global bindings, then point `esm_entry_point` at it.
fn deno_url_with_globals() -> Extension {
    let mut ext = deno_url::deno_url::init();
    let mut files = ext.esm_files.into_owned();
    files.push(ExtensionFileSource::new_computed(
        "ext:obscura/deno_url_entry.js",
        Arc::from(
            r#"
import { URL, URLSearchParams } from "ext:deno_url/00_url.js";
import { URLPattern } from "ext:deno_url/01_urlpattern.js";
Object.defineProperty(globalThis, "URL", { value: URL, writable: true, configurable: true });
Object.defineProperty(globalThis, "URLSearchParams", { value: URLSearchParams, writable: true, configurable: true });
Object.defineProperty(globalThis, "URLPattern", { value: URLPattern, writable: true, configurable: true });
"#,
        ),
    ));
    ext.esm_files = std::borrow::Cow::Owned(files);
    ext.esm_entry_point = Some("ext:obscura/deno_url_entry.js");
    ext
}

/// Returns the deno extensions required by both the snapshot build and the
/// runtime: `deno_webidl`, `deno_console` (needed as a library dep by
/// `deno_url`'s `customInspect` — does not replace `globalThis.console`), and
/// `deno_url` wired to expose `URL` / `URLSearchParams` / `URLPattern`
/// globally.
pub fn build() -> Vec<Extension> {
    vec![
        with_esm_entry(deno_webidl::deno_webidl::init(), "ext:deno_webidl/00_webidl.js"),
        with_esm_entry(deno_console::deno_console::init(), "ext:deno_console/01_console.js"),
        deno_url_with_globals(),
    ]
}
