use std::collections::HashSet;
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
    /// Target ids closed via `Target.closeTarget` while their live `Page`
    /// still belongs to another connection (thread-per-connection #430 means
    /// a remote page cannot be torn down from the closing connection). The
    /// owning connection keeps the `Page` object, so without a tombstone its
    /// next `sync_registry` (getTargets, navigation, createTarget) would
    /// re-register the closed target and it would resurface in every
    /// connection's Target.getTargets and /json/list. Tombstoned ids are
    /// skipped by `upsert`; the entry is cleared once the owner really drops
    /// the page (`remove_page` or connection `Drop`).
    closed: HashSet<String>,
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

/// Upper bound on tombstoned target ids. Tombstones normally clear when the
/// owner syncs and drops the page; this caps the worst case where an idle
/// owner never syncs while remote closes pile up (each entry is a small
/// String, so 1024 is a few KB).
const MAX_TOMBSTONES: usize = 1024;

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
    /// creation position while updating the url/title after navigation. Ids
    /// tombstoned via `mark_closed` are ignored: their live `Page` still
    /// exists on the owning connection, but the target must stay closed
    /// everywhere until the owner actually drops it.
    pub fn upsert(&self, info: TargetInfo) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if state.closed.contains(&info.target_id) {
            return;
        }
        if let Some(existing) = state.targets.iter_mut().find(|t| t.target_id == info.target_id) {
            *existing = info;
        } else {
            state.targets.push(info);
        }
    }

    /// Remove the given targets (page closed, context disposed, or the owning
    /// connection went away). This is real teardown: the live `Page` is gone,
    /// so any tombstone for the id is cleared too and the target can never
    /// resurface.
    pub fn remove_pages(&self, target_ids: &[String]) {
        if target_ids.is_empty() {
            return;
        }
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.targets.retain(|t| !target_ids.contains(&t.target_id));
        for id in target_ids {
            state.closed.remove(id);
        }
    }

    /// Mark targets as closed from the registry's perspective. Used by
    /// `Target.closeTarget` on a page owned by another connection: the entry
    /// leaves the live list immediately and the id is tombstoned so the
    /// owner's next `sync_registry` cannot resurrect it. The tombstone is
    /// normally cleared when the owner syncs and drops the page for real
    /// (`sync_registry`/`remove_pages`/`Drop`), which bounds growth by
    /// connection activity. The hard cap below is the backstop for an idle
    /// owner that never syncs.
    pub fn mark_closed(&self, target_ids: &[String]) {
        if target_ids.is_empty() {
            return;
        }
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.targets.retain(|t| !target_ids.contains(&t.target_id));
        for id in target_ids {
            if !state.closed.insert(id.clone()) {
                continue;
            }
            // Backstop against unbounded growth. Normally a tombstone lives
            // only until the owner's next sync, but an owner that never syncs
            // would otherwise accumulate one per remote close. When the cap
            // is hit, evict an existing tombstone other than the one just
            // inserted; the affected page could resurface on its owner's next
            // sync, but that takes >MAX remote closes against a single idle
            // owner, the page is visible again in getTargets, and it can
            // simply be closed again.
            if state.closed.len() > MAX_TOMBSTONES {
                if let Some(victim) = state.closed.iter().find(|v| **v != *id).cloned() {
                    state.closed.remove(&victim);
                }
            }
        }
    }

    /// Whether a target id is tombstoned (closed by another connection while
    /// its live `Page` still exists on the owner).
    pub fn is_closed(&self, target_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closed
            .contains(target_id)
    }

    /// Snapshot of every live target, in creation order.
    pub fn all(&self) -> Vec<TargetInfo> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .targets
            .clone()
    }

    #[cfg(test)]
    fn tombstone_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closed
            .len()
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

    /// A remotely closed target stays closed even when the owner's
    /// `sync_registry` tries to re-register it: `mark_closed` tombstones the
    /// id and `upsert` ignores it. Only `remove_pages` (real teardown by the
    /// owner) clears the tombstone.
    #[test]
    fn mark_closed_tombstones_until_real_teardown() {
        let registry = TargetRegistry::default();
        let id = "page-7".to_string();
        registry.upsert(TargetInfo {
            target_id: id.clone(),
            title: String::new(),
            url: "about:blank".into(),
            browser_context_id: "default".into(),
        });

        // Remote close: entry leaves the live list immediately…
        registry.mark_closed(&[id.clone()]);
        assert!(registry.all().is_empty(), "closed target must leave the live list");

        // …and a later upsert (the owner's sync_registry after navigation or
        // getTargets) must not resurrect it.
        registry.upsert(TargetInfo {
            target_id: id.clone(),
            title: "Resurrected".into(),
            url: "https://example.com/".into(),
            browser_context_id: "default".into(),
        });
        assert!(
            registry.all().is_empty(),
            "tombstoned target must not resurface on sync, got: {:?}",
            registry.all()
        );

        // Owner really drops the page → tombstone cleared, but no entry.
        registry.remove_pages(&[id.clone()]);
        assert!(registry.all().is_empty());

        // A brand-new page id is never affected by tombstones.
        let fresh = "page-8".to_string();
        registry.upsert(TargetInfo {
            target_id: fresh.clone(),
            title: String::new(),
            url: "about:blank".into(),
            browser_context_id: "default".into(),
        });
        assert_eq!(registry.all().len(), 1);
        assert_eq!(registry.all()[0].target_id, "page-8");
    }

    /// The tombstone set is capped so an owner that never syncs cannot grow
    /// it unboundedly (normally the owner's sync drops the closed page and
    /// clears its tombstone, but an idle owner would otherwise accumulate one
    /// per remote close).
    #[test]
    fn mark_closed_caps_tombstone_growth() {
        let registry = TargetRegistry::default();
        for i in 0..(MAX_TOMBSTONES * 2) {
            registry.mark_closed(&[format!("page-{i}")]);
        }
        assert!(
            registry.tombstone_count() <= MAX_TOMBSTONES,
            "tombstones must be capped, got {}",
            registry.tombstone_count()
        );
        assert!(registry.all().is_empty(), "live list must stay empty");

        // A fresh close still works after evictions.
        registry.mark_closed(&["page-fresh".to_string()]);
        assert!(registry.is_closed("page-fresh"));
    }
}
