use std::sync::{Arc, Mutex};

/// Metadata snapshot for a live page target, visible to every CDP connection
/// and to the HTTP control plane (`/json/list`).
///
/// Only plain, sync-readable fields live here. The actual `Page` objects stay
/// owned by their creating connection's `CdpContext` (thread-per-connection
/// #430 confines each page's V8 isolate to one OS thread); the registry is a
/// lightweight mirror that lets `Target.getTargets` on *any* connection and
/// `/json/list` on the accept thread report every live page with its current
/// url/title.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TargetInfo {
    pub target_id: String,
    pub title: String,
    pub url: String,
    pub browser_context_id: String,
}

#[derive(Default)]
struct RegistryState {
    /// All live page targets, in creation order (Chrome lists targets in
    /// creation order; preserving it keeps clients' bookkeeping stable).
    targets: Vec<TargetInfo>,
    /// Globally-unique page id counter. Each connection used to mint its own
    /// `page-N` ids from a per-context counter, so every connection's first
    /// page collided on "page-1" in any shared view. The registry hands out
    /// ids so target ids are unique across the whole server.
    next_page_id: u64,
}

/// Process-wide page target registry, shared by every CDP connection and the
/// HTTP accept thread. Clone is cheap (one `Arc` bump); all mutation goes
/// through a short std mutex that never spans an await.
#[derive(Clone, Default)]
pub struct TargetRegistry {
    inner: Arc<Mutex<RegistryState>>,
}

impl TargetRegistry {
    /// Claim the next globally-unique page id (`page-1`, `page-2`, ...).
    pub fn next_page_id(&self) -> u64 {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.next_page_id = state.next_page_id.saturating_add(1);
        state.next_page_id
    }

    /// Insert or refresh a target. Replacing by `target_id` keeps the original
    /// creation position while updating the url/title after navigation.
    pub fn upsert(&self, info: TargetInfo) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = state.targets.iter_mut().find(|t| t.target_id == info.target_id) {
            *existing = info;
        } else {
            state.targets.push(info);
        }
    }

    /// Remove the given targets (page closed, context disposed, or the owning
    /// connection went away).
    pub fn remove_pages(&self, target_ids: &[String]) {
        if target_ids.is_empty() {
            return;
        }
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.targets.retain(|t| !target_ids.contains(&t.target_id));
    }

    /// Snapshot of every live target, in creation order.
    pub fn all(&self) -> Vec<TargetInfo> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .targets
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_across_shared_registry_views() {
        let registry = TargetRegistry::default();
        assert_eq!(registry.next_page_id(), 1);
        assert_eq!(registry.next_page_id(), 2);
        assert_eq!(registry.next_page_id(), 3);
    }

    #[test]
    fn upsert_refreshes_in_place_and_all_returns_snapshot() {
        let registry = TargetRegistry::default();
        registry.upsert(TargetInfo {
            target_id: "page-1".into(),
            title: String::new(),
            url: "about:blank".into(),
            browser_context_id: "default".into(),
        });
        registry.upsert(TargetInfo {
            target_id: "page-2".into(),
            title: String::new(),
            url: "about:blank".into(),
            browser_context_id: "default".into(),
        });

        // Refresh page-1 after navigation; it must keep its creation slot.
        registry.upsert(TargetInfo {
            target_id: "page-1".into(),
            title: "Example".into(),
            url: "https://example.com/".into(),
            browser_context_id: "default".into(),
        });

        let all = registry.all();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].target_id, "page-1");
        assert_eq!(all[0].url, "https://example.com/");
        assert_eq!(all[1].target_id, "page-2");

        registry.remove_pages(&["page-1".to_string()]);
        let all = registry.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].target_id, "page-2");
    }
}
