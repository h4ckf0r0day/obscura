#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};
use core::ops::Range;

use crate::{AttrsOwned, FamilyOwned, HashMap, ShapeGlyph};

const MAX_ENTRIES: usize = 512;
const MAX_RETAINED_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RUN_BYTES: usize = 1024;
pub(crate) const MAX_RUN_SPANS: usize = 16;

/// Key for caching shape runs.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct ShapeRunKey {
    pub text: String,
    pub default_attrs: AttrsOwned,
    pub attrs_spans: Vec<(Range<usize>, AttrsOwned)>,
}

impl ShapeRunKey {
    fn retained_bytes(&self) -> usize {
        fn attrs_bytes(attrs: &AttrsOwned) -> usize {
            let family_bytes = match &attrs.family_owned {
                FamilyOwned::Name(name) => name.len(),
                _ => 0,
            };
            family_bytes
                + attrs.font_features.features.capacity() * core::mem::size_of::<crate::Feature>()
                + attrs.font_variations.allocated_bytes()
        }
        core::mem::size_of::<Self>() + self.text.capacity()
            + self.attrs_spans.capacity() * core::mem::size_of::<(Range<usize>, AttrsOwned)>()
            + attrs_bytes(&self.default_attrs)
            + self.attrs_spans.iter().map(|(_, attrs)| attrs_bytes(attrs)).sum::<usize>()
    }

    pub(crate) fn is_cacheable(&self) -> bool {
        self.text.len() <= MAX_RUN_BYTES
            && self.attrs_spans.len() <= MAX_RUN_SPANS
            && self.retained_bytes() <= MAX_ENTRY_BYTES
    }
}

/// A helper structure for caching shape runs.
#[derive(Clone, Default)]
pub struct ShapeRunCache {
    age: u64,
    cache: HashMap<ShapeRunKey, (u64, Vec<ShapeGlyph>, usize)>,
    retained_bytes: usize,
}

impl ShapeRunCache {
    /// Get cache item, updating age if found
    pub fn get(&mut self, key: &ShapeRunKey) -> Option<&Vec<ShapeGlyph>> {
        self.cache.get_mut(key).map(|(age, glyphs, _)| {
            *age = self.age;
            &*glyphs
        })
    }

    /// Insert cache item with current age
    pub fn insert(&mut self, key: ShapeRunKey, glyphs: Vec<ShapeGlyph>) {
        let bytes = key.retained_bytes()
            + core::mem::size_of::<(u64, Vec<ShapeGlyph>, usize)>()
            + glyphs.capacity() * core::mem::size_of::<ShapeGlyph>();
        if !key.is_cacheable() || bytes > MAX_ENTRY_BYTES {
            return;
        }
        if let Some((_, _, previous_bytes)) = self.cache.remove(&key) {
            self.retained_bytes -= previous_bytes;
        }
        // This is disposable memoization. Clearing at either bound avoids an
        // eviction scan or a second copy of every key. Hash-table overhead is
        // separately bounded by MAX_ENTRIES; payload accounting uses capacity.
        if self.cache.len() >= MAX_ENTRIES || self.retained_bytes + bytes > MAX_RETAINED_BYTES {
            self.cache.clear();
            self.retained_bytes = 0;
        }
        self.retained_bytes += bytes;
        self.cache.insert(key, (self.age, glyphs, bytes));
    }

    /// Remove anything in the cache with an age older than `keep_ages`
    pub fn trim(&mut self, keep_ages: u64) {
        self.cache.retain(|_key, (age, _glyphs, bytes)| {
            let retain = age.saturating_add(keep_ages) >= self.age;
            if !retain {
                self.retained_bytes -= *bytes;
            }
            retain
        });
        // Increase age
        self.age += 1;
    }
}

impl core::fmt::Debug for ShapeRunCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("ShapeRunCache").finish()
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::Attrs;

    fn key(index: usize) -> ShapeRunKey {
        ShapeRunKey {
            text: format!("word{index}"),
            default_attrs: AttrsOwned::new(&Attrs::new()),
            attrs_spans: Vec::new(),
        }
    }

    #[test]
    fn shape_run_cache_bounds_entries_payload_and_replacement_accounting() {
        let mut cache = ShapeRunCache::default();
        for index in 0..MAX_ENTRIES + 4 {
            cache.insert(key(index), Vec::new());
            assert!(cache.cache.len() <= MAX_ENTRIES);
            assert!(cache.retained_bytes <= MAX_RETAINED_BYTES);
        }
        assert!(cache.get(&key(0)).is_none());
        let bytes = cache.retained_bytes;
        cache.insert(key(MAX_ENTRIES + 3), Vec::new());
        assert_eq!(cache.retained_bytes, bytes);
        cache = ShapeRunCache::default();
        let mut evictions = 0;
        for index in 0..300 {
            let previous = cache.cache.len();
            cache.insert(key(index), Vec::with_capacity(200));
            evictions += usize::from(cache.cache.len() < previous);
            assert!(cache.retained_bytes <= MAX_RETAINED_BYTES);
            assert_eq!(cache.retained_bytes, cache.cache.values().map(|(_, _, bytes)| bytes).sum());
        }
        assert!(evictions > 0, "the payload bound must evict before the entry bound");
        cache.trim(0);
        cache.trim(0);
        assert!(cache.cache.is_empty());
        assert_eq!(cache.retained_bytes, 0);
    }

    #[test]
    fn shape_run_cache_rejects_oversized_keys_and_allocations() {
        let mut cache = ShapeRunCache::default();
        let mut long = key(0);
        long.text = "x".repeat(MAX_RUN_BYTES + 1);
        cache.insert(long, Vec::new());
        cache.insert(key(1), Vec::with_capacity(MAX_ENTRY_BYTES));
        let mut spans = key(2);
        spans.attrs_spans = (0..MAX_RUN_SPANS + 1)
            .map(|index| (index..index + 1, AttrsOwned::new(&Attrs::new()))).collect();
        cache.insert(spans, Vec::new());
        let mut allocation = key(3);
        allocation.default_attrs.font_features.features.reserve(MAX_ENTRY_BYTES);
        cache.insert(allocation, Vec::new());
        assert!(cache.cache.is_empty());
        assert_eq!(cache.retained_bytes, 0);
    }
}
