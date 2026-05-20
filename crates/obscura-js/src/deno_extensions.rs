use std::sync::Arc;

use deno_core::Extension;
use deno_core::ExtensionFileSource;
use deno_web::BlobStore;
use deno_web::TimersPermission;

/// Permission shim for `deno_web` timer / `performance.now()` ops.
///
/// Obscura is a single-tenant scraping browser, not a multi-origin runtime —
/// there is no untrusted code we'd need to fuzz-protect against by clamping
/// `performance.now()` resolution. Always grant high-resolution time so the
/// JS surface matches real Chrome (Spectre mitigations don't apply to a
/// headless single-isolate process anyway).
pub struct ObscuraTimersPermission;

impl TimersPermission for ObscuraTimersPermission {
    #[inline(always)]
    fn allow_hrtime(&mut self) -> bool {
        true
    }
}

fn with_esm_entry(mut ext: Extension, entry: &'static str) -> Extension {
    ext.esm_entry_point = Some(entry);
    ext
}

/// `deno_url` ships two ESM files (`00_url.js`, `01_urlpattern.js`) that only
/// `export` their classes — Deno's own runtime is responsible for exposing
/// them on `globalThis`. We add a synthetic entry that imports both files and
/// installs the global bindings, then point `esm_entry_point` at it.
fn deno_url_with_globals() -> Extension {
    add_synthetic_entry(
        deno_url::deno_url::init(),
        "ext:obscura/deno_url_entry.js",
        r#"
import { URL, URLSearchParams } from "ext:deno_url/00_url.js";
import { URLPattern } from "ext:deno_url/01_urlpattern.js";
Object.defineProperty(globalThis, "URL", { value: URL, writable: true, configurable: true });
Object.defineProperty(globalThis, "URLSearchParams", { value: URLSearchParams, writable: true, configurable: true });
Object.defineProperty(globalThis, "URLPattern", { value: URLPattern, writable: true, configurable: true });
"#,
    )
}

/// `deno_web` ships ~18 ESM modules covering Blob/File, FormData/streams,
/// TextEncoder/Decoder, Event/EventTarget, AbortController, structuredClone,
/// timers, atob/btoa, performance, MessageChannel/Port, and DOMException.
/// None of them touch `globalThis` directly — Deno's runtime sets up the
/// globals separately. We do the same here via a synthetic entry that
/// imports every public class and exposes it on `globalThis`.
fn deno_web_with_globals() -> Extension {
    let ext =
        deno_web::deno_web::init::<ObscuraTimersPermission>(Arc::new(BlobStore::default()), None);
    add_synthetic_entry(
        ext,
        "ext:obscura/deno_web_entry.js",
        r#"
import { DOMException } from "ext:deno_web/01_dom_exception.js";
import {
  CloseEvent,
  CustomEvent,
  ErrorEvent,
  Event,
  EventTarget,
  MessageEvent,
  ProgressEvent,
  PromiseRejectionEvent,
  reportError,
  saveGlobalThisReference,
} from "ext:deno_web/02_event.js";
saveGlobalThisReference(globalThis);
import { AbortController, AbortSignal } from "ext:deno_web/03_abort_signal.js";
import { structuredClone } from "ext:deno_web/02_structured_clone.js";
import {
  setTimeout,
  setInterval,
  clearTimeout,
  clearInterval,
} from "ext:deno_web/02_timers.js";
import { atob, btoa } from "ext:deno_web/05_base64.js";
import {
  ByteLengthQueuingStrategy,
  CountQueuingStrategy,
  ReadableByteStreamController,
  ReadableStream,
  ReadableStreamBYOBReader,
  ReadableStreamBYOBRequest,
  ReadableStreamDefaultController,
  ReadableStreamDefaultReader,
  TransformStream,
  TransformStreamDefaultController,
  WritableStream,
  WritableStreamDefaultController,
  WritableStreamDefaultWriter,
} from "ext:deno_web/06_streams.js";
import {
  TextDecoder,
  TextDecoderStream,
  TextEncoder,
  TextEncoderStream,
} from "ext:deno_web/08_text_encoding.js";
import { Blob, File } from "ext:deno_web/09_file.js";
import { FileReader } from "ext:deno_web/10_filereader.js";
import { MessageChannel, MessagePort } from "ext:deno_web/13_message_port.js";
import { CompressionStream, DecompressionStream } from "ext:deno_web/14_compression.js";
import {
  Performance,
  performance,
  PerformanceEntry,
  PerformanceMark,
  PerformanceMeasure,
} from "ext:deno_web/15_performance.js";
// Side-effect imports: deno_web ships these modules but our synthetic entry
// only follows the import graph reachable from itself, so we load them
// explicitly to satisfy the snapshot's "all modules evaluated" check.
// We do not re-expose their exports globally:
// - 12_location.js: bootstrap.js installs its own `location` object backed by
//   the runtime navigation state, so we keep that.
// - 04_global_interfaces.js: Window/WorkerGlobalScope descriptors, irrelevant
//   to a single-page scraping context where `globalThis` is the only realm.
// - 16_image_data.js: canvas ImageData, unused by obscura.
import "ext:deno_web/12_location.js";
import "ext:deno_web/04_global_interfaces.js";
import "ext:deno_web/16_image_data.js";

const expose = (name, value) => {
  Object.defineProperty(globalThis, name, {
    value,
    writable: true,
    configurable: true,
  });
};

expose("DOMException", DOMException);
expose("Event", Event);
expose("EventTarget", EventTarget);
expose("CustomEvent", CustomEvent);
expose("ErrorEvent", ErrorEvent);
expose("MessageEvent", MessageEvent);
expose("ProgressEvent", ProgressEvent);
expose("PromiseRejectionEvent", PromiseRejectionEvent);
expose("CloseEvent", CloseEvent);
expose("reportError", reportError);
expose("AbortController", AbortController);
expose("AbortSignal", AbortSignal);
expose("structuredClone", structuredClone);
expose("setTimeout", setTimeout);
expose("setInterval", setInterval);
expose("clearTimeout", clearTimeout);
expose("clearInterval", clearInterval);
expose("atob", atob);
expose("btoa", btoa);
expose("ReadableStream", ReadableStream);
expose("ReadableStreamBYOBReader", ReadableStreamBYOBReader);
expose("ReadableStreamBYOBRequest", ReadableStreamBYOBRequest);
expose("ReadableStreamDefaultReader", ReadableStreamDefaultReader);
expose("ReadableByteStreamController", ReadableByteStreamController);
expose("ReadableStreamDefaultController", ReadableStreamDefaultController);
expose("WritableStream", WritableStream);
expose("WritableStreamDefaultController", WritableStreamDefaultController);
expose("WritableStreamDefaultWriter", WritableStreamDefaultWriter);
expose("TransformStream", TransformStream);
expose("TransformStreamDefaultController", TransformStreamDefaultController);
expose("ByteLengthQueuingStrategy", ByteLengthQueuingStrategy);
expose("CountQueuingStrategy", CountQueuingStrategy);
expose("TextEncoder", TextEncoder);
expose("TextDecoder", TextDecoder);
expose("TextEncoderStream", TextEncoderStream);
expose("TextDecoderStream", TextDecoderStream);
expose("Blob", Blob);
expose("File", File);
expose("FileReader", FileReader);
expose("MessageChannel", MessageChannel);
expose("MessagePort", MessagePort);
expose("CompressionStream", CompressionStream);
expose("DecompressionStream", DecompressionStream);
expose("performance", performance);
expose("Performance", Performance);
expose("PerformanceEntry", PerformanceEntry);
expose("PerformanceMark", PerformanceMark);
expose("PerformanceMeasure", PerformanceMeasure);
"#,
    )
}

/// `deno_crypto` ships `00_crypto.js` exporting `Crypto`, `crypto`,
/// `CryptoKey`, `SubtleCrypto`. Same export-only pattern — we re-expose
/// `crypto` (the instance) and its constructors on `globalThis`.
fn deno_crypto_with_globals() -> Extension {
    let ext = deno_crypto::deno_crypto::init(None);
    add_synthetic_entry(
        ext,
        "ext:obscura/deno_crypto_entry.js",
        r#"
import { Crypto, crypto, CryptoKey, SubtleCrypto } from "ext:deno_crypto/00_crypto.js";
Object.defineProperty(globalThis, "crypto", { value: crypto, writable: true, configurable: true });
Object.defineProperty(globalThis, "Crypto", { value: Crypto, writable: true, configurable: true });
Object.defineProperty(globalThis, "CryptoKey", { value: CryptoKey, writable: true, configurable: true });
Object.defineProperty(globalThis, "SubtleCrypto", { value: SubtleCrypto, writable: true, configurable: true });
"#,
    )
}

fn add_synthetic_entry(
    mut ext: Extension,
    specifier: &'static str,
    source: &'static str,
) -> Extension {
    let mut files = ext.esm_files.into_owned();
    files.push(ExtensionFileSource::new_computed(
        specifier,
        Arc::from(source),
    ));
    ext.esm_files = std::borrow::Cow::Owned(files);
    ext.esm_entry_point = Some(specifier);
    ext
}

/// Returns the deno extensions required by both the snapshot build and the
/// runtime, in dependency order.
///
/// - `deno_webidl` (foundation)
/// - `deno_console` (library dep of `deno_url`'s `customInspect`; does not
///   replace `globalThis.console`)
/// - `deno_url` (URL, URLSearchParams, URLPattern)
/// - `deno_web` (Blob, FormData-adjacent classes, TextEncoder/Decoder, Event,
///   EventTarget, AbortController, structuredClone, timers, atob/btoa,
///   performance, MessageChannel, streams, DOMException, FileReader,
///   CompressionStream)
/// - `deno_crypto` (Web Crypto API + secure CSPRNG)
pub fn build() -> Vec<Extension> {
    vec![
        with_esm_entry(
            deno_webidl::deno_webidl::init(),
            "ext:deno_webidl/00_webidl.js",
        ),
        with_esm_entry(
            deno_console::deno_console::init(),
            "ext:deno_console/01_console.js",
        ),
        deno_url_with_globals(),
        deno_web_with_globals(),
        deno_crypto_with_globals(),
    ]
}
