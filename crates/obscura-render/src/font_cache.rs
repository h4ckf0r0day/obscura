//! Process-wide font caching for the HTML text engine ([`crate::inline`]).
//!
//! `TextEngine::new_with_web_fonts_and_emoji` used to build a brand-new
//! `fontdb::Database` from scratch on every call: parsing the 14 embedded
//! bundled faces plus every authored `@font-face`/dynamic `FontFace` on every
//! single document, even pages that reuse the exact same page fonts as the
//! previous request. `fontdb::Database::load_font_source` parses the sfnt
//! tables synchronously, which is the multi-second cost reported in #879 for
//! multi-MB CJK faces.
//!
//! This module adds two independent caches, mirroring `RenderResourceCache`'s
//! FIFO-bounded shape (`paint.rs`) and `svg_font_database`'s once-per-process
//! base database (`paint.rs`):
//!
//! - [`load_cached_web_font`]: a process-wide, mutex-guarded, FIFO-bounded
//!   cache from a font's raw bytes to the `fontdb::FaceInfo`s it produces.
//!   A cache hit uses `Database::push_face_info`, which stores an
//!   already-parsed `FaceInfo` without re-parsing.
//! - Extra `--font-dir` directories: [`configure_extra_font_directories`]
//!   registers directories to fold into the base bundled-face database
//!   (built once per process, see `inline::base_font_database`) the first
//!   time it is built.
use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use cosmic_text::fontdb;

const DEFAULT_FONT_BYTE_CACHE_ENTRIES: usize = 128;
const DEFAULT_FONT_BYTE_CACHE_BYTES: usize = 128 * 1024 * 1024;

/// One cached font source: the parsed faces plus the raw byte count charged
/// against the cache's byte bound (once per source, not once per face, since
/// every face in a collection shares the same underlying bytes).
struct CachedFontSource {
    faces: Vec<fontdb::FaceInfo>,
    bytes: usize,
}

/// Page-provided font bytes shared across every document/browser context in
/// this process. Bounded by both entry count and total retained byte size,
/// evicting the oldest entry first, matching `RenderResourceCache` (`paint.rs`).
struct FontByteCache {
    entries: HashMap<u64, CachedFontSource>,
    order: VecDeque<u64>,
    retained_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl FontByteCache {
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            retained_bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    fn get(&self, hash: u64) -> Option<Vec<fontdb::FaceInfo>> {
        self.entries.get(&hash).map(|entry| entry.faces.clone())
    }

    fn insert(&mut self, hash: u64, faces: Vec<fontdb::FaceInfo>, bytes: usize) {
        if self.entries.contains_key(&hash) || self.max_entries == 0 || bytes > self.max_bytes {
            return;
        }
        while self.entries.len() >= self.max_entries
            || self.retained_bytes.saturating_add(bytes) > self.max_bytes
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.retained_bytes = self.retained_bytes.saturating_sub(evicted.bytes);
            }
        }
        self.retained_bytes = self.retained_bytes.saturating_add(bytes);
        self.order.push_back(hash);
        self.entries.insert(hash, CachedFontSource { faces, bytes });
    }
}

fn font_byte_cache() -> &'static Mutex<FontByteCache> {
    static CACHE: OnceLock<Mutex<FontByteCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(FontByteCache::new(
            DEFAULT_FONT_BYTE_CACHE_ENTRIES,
            DEFAULT_FONT_BYTE_CACHE_BYTES,
        ))
    })
}

/// Content-address a font source's raw bytes.
///
/// This cache is content-addressed and the content is attacker-influenced (a
/// screenshot service renders untrusted pages, and `document.fonts.add()`
/// bytes come straight from page script). Do **not** switch this to
/// `std::collections::hash_map::DefaultHasher::new()`: its seed is fixed
/// (`(0, 0)`), not randomized, which would make a deliberate hash collision
/// (poisoning one document's cache entry with another's face data) at least
/// theoretically searchable offline. `RandomState` is seeded from the OS RNG
/// once per process start, the same guarantee `HashMap::new()` relies on by
/// default, at zero extra dependency cost. The `RandomState` instance itself
/// is built once and reused so identical bytes keep hashing identically for
/// the life of the process (otherwise every lookup would miss).
pub(crate) fn hash_font_bytes(data: &[u8]) -> u64 {
    static STATE: OnceLock<RandomState> = OnceLock::new();
    STATE.get_or_init(RandomState::new).hash_one(data)
}

/// Insert an already-parsed `FaceInfo` into `db` without re-parsing, and
/// recover the `fontdb::ID` it was assigned.
///
/// `Database::push_face_info` (fontdb 0.16) does not return the new id, so
/// recover it by diffing the face id set before/after the call; `db` only
/// ever grows by exactly one face per call.
fn push_face_info_and_get_id(db: &mut fontdb::Database, mut info: fontdb::FaceInfo) -> fontdb::ID {
    info.id = fontdb::ID::dummy();
    let before: HashSet<fontdb::ID> = db.faces().map(|face| face.id).collect();
    db.push_face_info(info);
    db.faces()
        .map(|face| face.id)
        .find(|id| !before.contains(id))
        .expect("push_face_info always inserts exactly one new face")
}

/// Load one page-provided font's bytes into `db`, using the process-wide
/// cache when this exact content has been seen before.
///
/// On a hit, every cached `FaceInfo` (a source can be a font collection, so
/// there can be more than one) is pushed into `db` without re-parsing. On a
/// miss, `db.load_font_source` parses as before and the resulting faces are
/// cloned into the cache (subject to the FIFO bound) for the next caller.
///
/// The whole miss path runs under the cache's lock: a second caller
/// requesting the exact same bytes while the first is still parsing blocks
/// on the mutex instead of racing to parse (and cache) the same content
/// twice. This serializes concurrent *first-time* parses of distinct
/// content too, trading a little concurrency for a simple, provably correct
/// cache; every parse other than the first, for any given content, is a
/// cheap hit.
pub(crate) fn load_cached_web_font(db: &mut fontdb::Database, data: &[u8]) -> Vec<fontdb::ID> {
    let hash = hash_font_bytes(data);
    let mut cache = font_byte_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(faces) = cache.get(hash) {
        return faces
            .into_iter()
            .map(|info| push_face_info_and_get_id(db, info))
            .collect();
    }
    #[cfg(test)]
    test_support::record_miss(hash);
    let ids: Vec<fontdb::ID> = db
        .load_font_source(fontdb::Source::Binary(std::sync::Arc::new(data.to_vec())))
        .into_iter()
        .collect();
    let faces: Vec<fontdb::FaceInfo> = ids.iter().filter_map(|id| db.face(*id).cloned()).collect();
    cache.insert(hash, faces, data.len());
    ids
}

static EXTRA_FONT_DIRECTORIES: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// Register extra font directories to fold into the base bundled-face
/// database (see `inline::base_font_database`) the first time it is built.
///
/// Must be called before the first render in this process: the base
/// database is itself cached in a process-wide `OnceLock` and is only ever
/// built once, so directories registered after that point have already
/// missed their only chance to be scanned. Calling this too late is a no-op;
/// a warning is logged so the misuse is visible rather than silently
/// ignored.
pub fn configure_extra_font_directories(dirs: Vec<PathBuf>) {
    if !set_extra_font_directories(&EXTRA_FONT_DIRECTORIES, dirs) {
        log::warn!(
            "configure_extra_font_directories called after the base font database was already \
             built; the requested directories will not be loaded in this process (call this \
             before the first render)"
        );
    }
}

/// Testable core of [`configure_extra_font_directories`]: attempt to set
/// `store` and report whether it took effect. Takes the `OnceLock` by
/// reference, rather than always the process-global one, so the "too late"
/// no-op path can be unit tested without depending on this test binary's
/// (unpredictable) test execution order.
pub(crate) fn set_extra_font_directories(
    store: &OnceLock<Vec<PathBuf>>,
    dirs: Vec<PathBuf>,
) -> bool {
    store.set(dirs).is_ok()
}

/// Directories configured via [`configure_extra_font_directories`], or empty
/// if none were (or if it was called too late to take effect).
pub(crate) fn configured_extra_font_directories() -> &'static [PathBuf] {
    EXTRA_FONT_DIRECTORIES.get_or_init(Vec::new)
}

/// Load every `.ttf`/`.otf`/`.ttc`/`.otc` face found (recursively) in `dir`
/// into `db`, returning the newly added ids. `fontdb::Database::load_fonts_dir`
/// already recursively scans and never errors (it logs and skips malformed
/// files), but silently no-ops for a directory that doesn't exist at all;
/// this wrapper adds a warning for that case too, so a misconfigured
/// `--font-dir` is never silent.
///
/// Deliberately takes `db`/`dirs` directly rather than reading the global
/// `OnceLock` config, so this can be unit tested without process-global
/// state or the "only built once" hazard `base_font_database` has.
pub(crate) fn load_extra_font_directory(db: &mut fontdb::Database, dir: &Path) -> Vec<fontdb::ID> {
    if !dir.is_dir() {
        log::warn!(
            "configured font directory {} does not exist or is not a directory; skipping",
            dir.display()
        );
        return Vec::new();
    }
    let before: HashSet<fontdb::ID> = db.faces().map(|face| face.id).collect();
    db.load_fonts_dir(dir);
    let added: Vec<fontdb::ID> = db
        .faces()
        .map(|face| face.id)
        .filter(|id| !before.contains(id))
        .collect();
    if added.is_empty() {
        log::warn!(
            "configured font directory {} contained no loadable fonts",
            dir.display()
        );
    }
    added
}

/// [`load_extra_font_directory`] over every configured directory.
pub(crate) fn load_extra_font_directories(
    db: &mut fontdb::Database,
    dirs: &[PathBuf],
) -> Vec<fontdb::ID> {
    dirs.iter()
        .flat_map(|dir| load_extra_font_directory(db, dir))
        .collect()
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    /// Number of real (non-cached) parses ever performed for a given content
    /// hash, for the life of this test binary's process.
    ///
    /// This is deliberately keyed by content hash rather than a single
    /// `AtomicUsize` total. `cargo test`'s default harness runs every
    /// `#[test]` in this binary concurrently on a shared thread pool, and
    /// any other test that builds a `TextEngine` with its own (different)
    /// `WebFont` bytes also causes a cache miss; a single shared counter
    /// would let that unrelated activity land in between one test's own
    /// calls, making an exact "+1" delta assertion flaky under parallel
    /// execution. Scoping by hash sidesteps this: unrelated tests use
    /// different bytes and so touch different buckets, while the *same*
    /// hash's count is still guaranteed to reach exactly 1 and never more,
    /// because the whole miss path in `load_cached_web_font` runs under the
    /// byte cache's mutex.
    static MISS_COUNTS: OnceLock<Mutex<HashMap<u64, usize>>> = OnceLock::new();

    fn counts() -> &'static Mutex<HashMap<u64, usize>> {
        MISS_COUNTS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(crate) fn record_miss(hash: u64) {
        *counts().lock().unwrap().entry(hash).or_insert(0) += 1;
    }

    pub(crate) fn miss_count(hash: u64) -> usize {
        counts().lock().unwrap().get(&hash).copied().unwrap_or(0)
    }
}
