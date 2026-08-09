use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};

use obscura_dom::{parse_html, DomTree};
use wasm_bindgen::prelude::*;

const ABI_VERSION: u32 = 1;

fn panic_message(operation: &str, payload: Box<dyn Any + Send>) -> String {
    let detail = if let Some(message) = payload.downcast_ref::<&str>() {
        *message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        "unknown Rust panic"
    };
    format!("Obscura WASM {operation} panicked: {detail}")
}

fn boundary_value<T>(operation: &str, call: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(value) => value,
        Err(payload) => wasm_bindgen::throw_str(&panic_message(operation, payload)),
    }
}

fn boundary_result<T>(
    operation: &str,
    call: impl FnOnce() -> Result<T, String>,
) -> Result<T, JsValue> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(JsValue::from_str(&error)),
        Err(payload) => Err(JsValue::from_str(&panic_message(operation, payload))),
    }
}

/// Portable part of an Obscura page.
///
/// JavaScript execution deliberately belongs to the host runtime. In Node,
/// that is the V8 isolate already owned by Node; embedding rusty_v8 in this
/// wasm module would create a nested VM and is not a supported wasm32 target.
#[wasm_bindgen]
pub struct ObscuraCore {
    dom: DomTree,
}

#[wasm_bindgen]
impl ObscuraCore {
    #[wasm_bindgen(constructor)]
    pub fn new(html: &str) -> Self {
        Self {
            dom: boundary_value("constructor", || parse_html(html)),
        }
    }

    /// Replace the document using Obscura's existing html5ever-backed parser.
    pub fn set_html(&mut self, html: &str) {
        boundary_value("set_html", || {
            self.dom = parse_html(html);
        });
    }

    /// Serialize the complete document.
    pub fn html(&self) -> String {
        boundary_value("html", || self.dom.outer_html(self.dom.document()))
    }

    /// Serialize the document element without the document doctype.
    ///
    /// This is kept separate from `html()` because browsers expose these as
    /// different values: `document.documentElement.outerHTML` is the `<html>`
    /// element, while serializing the document may also include its doctype.
    pub fn document_element_html(&self) -> Result<String, JsValue> {
        boundary_result("document_element_html", || {
            let node = self
                .dom
                .query_selector("html")?
                .ok_or_else(|| "document has no html element".to_string())?;
            Ok(self.dom.outer_html(node))
        })
    }

    /// Return the first matching element's serialized HTML, or `undefined`.
    pub fn query_html(&self, selector: &str) -> Result<Option<String>, JsValue> {
        boundary_result("query_html", || {
            self.dom
                .query_selector(selector)
                .map(|node| node.map(|node| self.dom.outer_html(node)))
        })
    }

    /// Return the first matching element's textContent, or `undefined`.
    pub fn query_text(&self, selector: &str) -> Result<Option<String>, JsValue> {
        boundary_result("query_text", || {
            self.dom
                .query_selector(selector)
                .map(|node| node.map(|node| self.dom.text_content(node)))
        })
    }

    pub fn query_count(&self, selector: &str) -> Result<u32, JsValue> {
        boundary_result("query_count", || {
            let count = self.dom.query_selector_all(selector)?.len();
            u32::try_from(count).map_err(|_| "selector result exceeds u32".to_string())
        })
    }
}

#[wasm_bindgen]
pub fn version() -> String {
    boundary_value("version", || env!("CARGO_PKG_VERSION").to_string())
}

/// Monotonic version for the JavaScript/WASM ownership and serialization ABI.
#[wasm_bindgen]
pub fn abi_version() -> u32 {
    ABI_VERSION
}

/// Machine-readable capability probe used by the Node Worker harness.
#[wasm_bindgen]
pub fn probe() -> String {
    boundary_value("probe", || {
        r#"{"abiVersion":1,"dom":true,"selectors":true,"javascript":"host","embeddedV8":false}"#
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_queries_with_the_existing_dom_engine() {
        let core = ObscuraCore::new(
            "<!doctype html><main><h1 class='title'>Obscura</h1><p>portable</p></main>",
        );
        assert_eq!(core.query_count("main > *").unwrap(), 2);
        assert_eq!(
            core.query_text(".title").unwrap().as_deref(),
            Some("Obscura")
        );
        assert!(core
            .query_html("main")
            .unwrap()
            .unwrap()
            .contains("portable"));
        let document_element = core.document_element_html().unwrap();
        assert!(document_element.starts_with("<html"));
        assert!(!document_element.to_ascii_lowercase().contains("<!doctype"));
        assert_eq!(abi_version(), 1);
        assert!(probe().contains(r#""abiVersion":1"#));
    }
}
