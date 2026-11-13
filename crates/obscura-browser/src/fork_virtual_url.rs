//! Fork-only: adopt a URL the page routed to itself.
//!
//! A single page app answers a click by calling `history.pushState` and
//! rendering the next view in place. `bootstrap.js` tracks that in
//! `__virtualUrl` so `location.href` reads correctly, but nothing on the Rust
//! side ever looked at it, so the page had moved on while `page.url()` still
//! reported the old document. To a CDP client that is a click that did nothing,
//! and `tools/ab/clicklocal.mjs` reports it as `spa route: FAILED`.
//!
//! Ported from fork commit `d7dca7a`. Kept out of `page.rs` so an upstream
//! rewrite of that file does not touch it; Rust allows an inherent impl in any
//! module of the defining crate, so the call site still reads
//! `self.sync_virtual_url()`.

use url::Url;

use crate::page::Page;

impl Page {
    /// Adopt a URL the page routed to itself, without fetching anything.
    ///
    /// Returns whether the URL changed.
    pub fn sync_virtual_url(&mut self) -> bool {
        let Some(js) = self.js.as_mut() else {
            return false;
        };
        let Ok(value) = js.take_history_update() else {
            return false;
        };
        self.adopt_virtual_url(value)
    }

    /// Flush history mutations before replacing a document, without adopting
    /// a Location setter's uncommitted navigation preview.
    pub fn sync_history_updates(&mut self) -> bool {
        let Some(js) = self.js.as_mut() else { return false; };
        let Ok(value) = js.take_history_update() else { return false; };
        if !value.is_array() { return false; }
        self.adopt_virtual_url(value)
    }

    fn adopt_virtual_url(&mut self, value: serde_json::Value) -> bool {
        let updates = value.as_array();
        let virtual_url = updates.and_then(|fields| fields.first()).unwrap_or(&value);
        let Some(virtual_url) = virtual_url.as_str().filter(|url| !url.is_empty()) else {
            return false;
        };
        let Ok(parsed) = Url::parse(virtual_url) else {
            return false;
        };
        // page.url keys the file:// sub-resource gate (#1069).
        if crate::page::cross_scheme_to_file(&self.url_string(), virtual_url) {
            return false;
        }
        let changed = self.url.as_ref() != Some(&parsed);
        if let Some(fields) = updates {
            self.apply_history_update(fields);
            self.url = Some(parsed);
            self.sync_js_session_history();
        } else if changed {
            self.url = Some(parsed);
            self.push_same_document_history(self.url_string());
        }
        changed
    }
}
