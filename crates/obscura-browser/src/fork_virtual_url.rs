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
use obscura_js::runtime::HistoryUpdate;

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
        if !matches!(&value, HistoryUpdate::Entries { .. }) { return false; }
        self.adopt_virtual_url(value)
    }

    fn adopt_virtual_url(&mut self, value: HistoryUpdate) -> bool {
        let virtual_url = match &value {
            HistoryUpdate::Url(url) | HistoryUpdate::Entries { url, .. } => url,
        };
        if virtual_url.is_empty() {
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
        if matches!(&value, HistoryUpdate::Entries { .. }) {
            self.apply_history_update(value);
            self.url = Some(parsed);
            self.sync_js_session_history();
        } else if changed {
            self.url = Some(parsed);
            self.push_same_document_history(self.url_string());
        }
        changed
    }
}
