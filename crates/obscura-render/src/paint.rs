//! Paint: rasterize the laid-out DOM into a [`tiny_skia::Pixmap`].
//!
//! Phase 5a. Fills each element's border box with its background color over a
//! white page. Text rendering arrives with the text step; borders and images
//! are later enhancements. Pure Rust (tiny-skia, CPU), deterministic, no system
//! dependencies, so a screenshot is reproducible across hosts.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use obscura_dom::tree::DomTree;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Paint, PathBuilder, Pattern,
    Pixmap, Point, RadialGradient, Rect, SpreadMode, Transform,
};

static FONT_BYTES: &[u8] = include_bytes!("../assets/liberation-sans.ttf");
static SYSTEM_FONT_BYTES: &[u8] = include_bytes!("../assets/dejavu-sans.ttf");
static SERIF_FONT_BYTES: &[u8] = include_bytes!("../assets/liberation-serif.ttf");
static MONO_FONT_BYTES: &[u8] = include_bytes!("../assets/liberation-mono.ttf");
static FONT_BOLD_BYTES: &[u8] = include_bytes!("../assets/liberation-sans-bold.ttf");
static FONT_OBLIQUE_BYTES: &[u8] = include_bytes!("../assets/liberation-sans-oblique.ttf");
static FONT_BOLD_OBLIQUE_BYTES: &[u8] = include_bytes!("../assets/liberation-sans-boldoblique.ttf");

use crate::dom::{
    layout_dom_with_web_fonts_and_retained_styles_with_animation_state,
    layout_dom_with_web_fonts_and_stylesheet_cache_for_media_with_animation_state,
    RetainedStyleMaps,
};

const DEFAULT_RESOURCE_CACHE_ENTRIES: usize = 512;
const DEFAULT_RESOURCE_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// CSS `content:url(...)` is only discoverable after cascade. Remember a
/// bounded set of successful selections so repeated prepares can seed their
/// intrinsic geometry before that cascade instead of always laying out twice.
const DEFAULT_CONTENT_IMAGE_INTRINSIC_ENTRIES: usize = 256;
const MISSING_RESOURCE_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(2);
/// Exact formats the renderer can decode. Do not use `image/*` or `*/*` here:
/// either wildcard permits a content-negotiating server to choose AVIF,
/// JPEG-XL, or another format that this build cannot rasterize.
const IMAGE_ACCEPT: &str = "image/webp,image/apng,image/svg+xml,image/png,image/jpeg,image/gif,image/bmp,image/x-icon,image/vnd.microsoft.icon";

/// Synchronous byte loader used by [`RenderResourceCache`]. The default
/// implementation uses Obscura's pooled image agent; tests and embedding
/// callers can provide a local loader without changing preparation or paint.
pub trait RenderResourceLoader {
    fn load(&mut self, url: &str) -> Option<Vec<u8>>;
}

/// One script-created `FontFace` registered with the document's
/// `FontFaceSet`. The JS runtime owns the registry and supplies this snapshot
/// alongside the DOM when preparing a render; keeping it out of the DOM avoids
/// exposing an implementation-only `<style>` element to page selectors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicFontFace {
    pub family: String,
    pub source: String,
    pub style: String,
    pub weight: String,
    pub unicode_range: String,
}

impl<F> RenderResourceLoader for F
where
    F: FnMut(&str) -> Option<Vec<u8>>,
{
    fn load(&mut self, url: &str) -> Option<Vec<u8>> {
        self(url)
    }
}

struct HttpResourceLoader;

impl RenderResourceLoader for HttpResourceLoader {
    fn load(&mut self, url: &str) -> Option<Vec<u8>> {
        http_get_bytes(url)
    }
}

enum CachedResource {
    Bytes(Arc<[u8]>),
    Missing(std::time::Instant),
}

/// Fetch credentials/CORS identity for an HTML image request. No-CORS uses
/// the ordinary URL cache key so CSS images can share the same response;
/// CORS variants use private keys and can never contaminate one another.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ImageRequestProfile {
    NoCorsInclude,
    CorsSameOrigin,
    CorsInclude,
}

fn image_request_profile(tree: &DomTree, id: obscura_dom::tree::NodeId) -> ImageRequestProfile {
    match tree
        .get_node(id)
        .and_then(|node| node.get_attribute("crossorigin").map(str::to_owned))
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("use-credentials") => ImageRequestProfile::CorsInclude,
        Some(_) => ImageRequestProfile::CorsSameOrigin,
        None => ImageRequestProfile::NoCorsInclude,
    }
}

fn image_resource_key(url: &str, profile: ImageRequestProfile) -> String {
    let url = network_resource_url(url);
    match profile {
        ImageRequestProfile::NoCorsInclude => url,
        ImageRequestProfile::CorsSameOrigin => format!("\0obscura-img-cors\0{url}"),
        ImageRequestProfile::CorsInclude => format!("\0obscura-img-credentials\0{url}"),
    }
}

fn network_resource_url(url: &str) -> String {
    if url.starts_with('\0') || url.starts_with("data:") {
        return url.to_string();
    }
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    parsed.set_fragment(None);
    parsed.to_string()
}

#[derive(Clone, Debug, PartialEq)]
struct RememberedContentImageIntrinsic {
    resolved_url: String,
    intrinsic: crate::ReplacedIntrinsic,
}

/// Page-scoped raw resource bytes shared by layout preparation and repeated
/// paints. Entries are FIFO-bounded by both count and retained byte size.
/// Successful bytes use `Arc` so consumers never clone an image/font body.
pub struct RenderResourceCache {
    entries: HashMap<String, CachedResource>,
    order: VecDeque<String>,
    retained_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
    content_image_intrinsics: HashMap<obscura_dom::tree::NodeId, RememberedContentImageIntrinsic>,
    content_image_intrinsic_order: VecDeque<obscura_dom::tree::NodeId>,
    max_content_image_intrinsics: usize,
    #[cfg(test)]
    content_image_layout_retries: usize,
    sync_loading_enabled: bool,
    loader: Box<dyn RenderResourceLoader>,
}

impl Default for RenderResourceCache {
    fn default() -> Self {
        Self::with_loader_and_limits(
            HttpResourceLoader,
            DEFAULT_RESOURCE_CACHE_ENTRIES,
            DEFAULT_RESOURCE_CACHE_BYTES,
        )
    }
}

impl RenderResourceCache {
    pub fn with_loader(loader: impl RenderResourceLoader + 'static) -> Self {
        Self::with_loader_and_limits(
            loader,
            DEFAULT_RESOURCE_CACHE_ENTRIES,
            DEFAULT_RESOURCE_CACHE_BYTES,
        )
    }

    pub fn with_loader_and_limits(
        loader: impl RenderResourceLoader + 'static,
        max_entries: usize,
        max_bytes: usize,
    ) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            retained_bytes: 0,
            max_entries,
            max_bytes,
            content_image_intrinsics: HashMap::new(),
            content_image_intrinsic_order: VecDeque::new(),
            max_content_image_intrinsics: max_entries.min(DEFAULT_CONTENT_IMAGE_INTRINSIC_ENTRIES),
            #[cfg(test)]
            content_image_layout_retries: 0,
            sync_loading_enabled: true,
            loader: Box::new(loader),
        }
    }

    /// Temporarily control the compatibility loader used by synchronous
    /// layout and paint. Capture callers disable it so a screenshot observes
    /// only bytes already prepared by the page transport. Unknown URLs remain
    /// unknown and can still be fetched by a later navigation/settle warmup.
    pub fn set_sync_loading_enabled(&mut self, enabled: bool) -> bool {
        std::mem::replace(&mut self.sync_loading_enabled, enabled)
    }

    pub fn retained_entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn retained_byte_len(&self) -> usize {
        self.retained_bytes
    }

    pub fn has_live_outcome(&self, url: &str) -> bool {
        match self.entries.get(&network_resource_url(url)) {
            Some(CachedResource::Bytes(_)) => true,
            Some(CachedResource::Missing(at)) => at.elapsed() < MISSING_RESOURCE_RETRY_AFTER,
            None => false,
        }
    }

    /// Whether this URL currently retains usable bytes (as opposed to a
    /// short-lived negative-cache entry). Profile-specific HTML image loads
    /// use this to avoid replacing paint bytes from another credential mode.
    pub fn has_cached_bytes(&self, url: &str) -> bool {
        matches!(
            self.entries.get(&network_resource_url(url)),
            Some(CachedResource::Bytes(_))
        )
    }

    pub fn has_live_image_outcome(&self, url: &str, profile: ImageRequestProfile) -> bool {
        self.has_live_outcome(&image_resource_key(url, profile))
    }

    pub fn seed_image(&mut self, url: String, profile: ImageRequestProfile, bytes: Vec<u8>) {
        self.seed(image_resource_key(&url, profile), bytes);
    }

    pub fn seed_image_missing(&mut self, url: String, profile: ImageRequestProfile) {
        self.seed_missing(image_resource_key(&url, profile));
    }

    /// Seed bytes fetched by the owning page's asynchronous browser transport.
    ///
    /// Layout and paint are synchronous, so letting them open their own HTTP
    /// requests serializes every image/font behind the capture call.  The page
    /// layer uses this entry point to fetch a bounded resource batch through
    /// its cookie/proxy/CORS-aware connection pool before entering layout.
    pub fn seed(&mut self, url: String, bytes: Vec<u8>) {
        let url = network_resource_url(&url);
        self.remove(&url);
        self.insert_bytes(url, Arc::from(bytes));
    }

    /// Retain a page-transport failure so capture does not immediately repeat
    /// the same slow request through the renderer's compatibility loader.
    pub fn seed_missing(&mut self, url: String) {
        let url = network_resource_url(&url);
        self.remove(&url);
        self.insert_missing(url);
    }

    /// Resolve, fetch, and inspect one image through the exact byte cache used
    /// by layout and paint. The JS image-element lifecycle calls this narrow
    /// bridge so `complete`/`naturalWidth` and the eventual screenshot are
    /// driven by one resource outcome instead of issuing an independent fetch.
    ///
    /// This intentionally accepts a plain `src`, not a DOM node: responsive
    /// `picture`/`srcset` selection remains owned by `prepare_dom`.
    pub fn image_metadata(
        &mut self,
        src: &str,
        base_url: Option<&str>,
    ) -> Option<(String, f32, f32)> {
        let resolved_url = resolve_resource_url(src, base_url)?;
        let bytes = fetch_bytes(&resolved_url, None, self)?;
        let (width, height) = image_metadata_from_bytes(&bytes)?;
        Some((resolved_url, width, height))
    }

    /// Inspect an image only when its renderer-cache outcome is already known.
    /// `None` means no live cache entry (the caller may queue a load);
    /// `Some(None)` means a retained load/decode failure; `Some(Some(...))`
    /// means success. `data:` sources have no network entry and are cheap to
    /// inspect directly.
    pub fn cached_image_metadata(
        &self,
        src: &str,
        base_url: Option<&str>,
    ) -> Option<Option<(String, f32, f32)>> {
        let resolved_url = resolve_resource_url(src, base_url)?;
        if resolved_url.starts_with("data:") {
            let mut scratch = RenderResourceCache::with_loader_and_limits(|_url: &str| None, 0, 0);
            return Some(
                fetch_bytes(&resolved_url, None, &mut scratch)
                    .and_then(|bytes| image_metadata_from_bytes(&bytes))
                    .map(|(width, height)| (resolved_url, width, height)),
            );
        }
        match self.entries.get(&network_resource_url(&resolved_url)) {
            Some(CachedResource::Bytes(bytes)) => Some(
                image_metadata_from_bytes(bytes)
                    .map(|(width, height)| (resolved_url, width, height)),
            ),
            Some(CachedResource::Missing(at)) if at.elapsed() < MISSING_RESOURCE_RETRY_AFTER => {
                Some(None)
            }
            _ => None,
        }
    }

    fn cached_profiled_image_metadata(
        &self,
        src: &str,
        base_url: Option<&str>,
        profile: ImageRequestProfile,
    ) -> Option<Option<(String, f32, f32)>> {
        let resolved_url = resolve_resource_url(src, base_url)?;
        if resolved_url.starts_with("data:") {
            return self.cached_image_metadata(&resolved_url, None);
        }
        let key = image_resource_key(&resolved_url, profile);
        match self.entries.get(&key) {
            Some(CachedResource::Bytes(bytes)) => Some(
                image_metadata_from_bytes(bytes)
                    .map(|(width, height)| (resolved_url, width, height)),
            ),
            Some(CachedResource::Missing(at)) if at.elapsed() < MISSING_RESOURCE_RETRY_AFTER => {
                Some(None)
            }
            _ => None,
        }
    }

    /// Select and inspect the resource for one live `<img>` using the same
    /// `picture`/`srcset`/`sizes` algorithm as `collect_image_intrinsics`.
    /// Dimensions are returned in CSS pixels after candidate-density scaling.
    /// A selected URL with `None` dimensions is an authoritative load/decode
    /// failure.
    pub fn image_element_metadata(
        &mut self,
        tree: &DomTree,
        id: obscura_dom::tree::NodeId,
        viewport: (f32, f32),
        base_url: Option<&str>,
    ) -> Option<(String, f32, Option<(f32, f32)>)> {
        let (src, density) = resolve_img_url(tree, id, viewport)?;
        let resolved_url = resolve_resource_url(&src, base_url).unwrap_or(src);
        let profile = image_request_profile(tree, id);
        let dimensions = fetch_profiled_image_bytes(&resolved_url, None, self, profile)
            .and_then(|bytes| image_metadata_from_bytes(&bytes))
            .map(|(width, height)| (width / density, height / density));
        Some((resolved_url, density, dimensions))
    }

    /// Cache-only counterpart to [`Self::image_element_metadata`]. The boolean
    /// is false only when the selected candidate has no live cache outcome;
    /// callers may queue the loading form without ever blocking a getter.
    pub fn cached_image_element_metadata(
        &self,
        tree: &DomTree,
        id: obscura_dom::tree::NodeId,
        viewport: (f32, f32),
        base_url: Option<&str>,
    ) -> Option<(String, f32, bool, Option<(f32, f32)>)> {
        let (src, density) = resolve_img_url(tree, id, viewport)?;
        let resolved_url = resolve_resource_url(&src, base_url).unwrap_or(src);
        let profile = image_request_profile(tree, id);
        match self.cached_profiled_image_metadata(&resolved_url, None, profile) {
            None => Some((resolved_url, density, false, None)),
            Some(dimensions) => Some((
                resolved_url,
                density,
                true,
                dimensions.map(|(_, width, height)| (width / density, height / density)),
            )),
        }
    }

    /// Cache-only poster selection for one live `<video>`. Poster images use
    /// the element's media CORS settings and the same page-owned image cache as
    /// ordinary replaced images; they do not imply that any video frame has
    /// loaded or decoded.
    pub fn cached_video_poster_metadata(
        &self,
        tree: &DomTree,
        id: obscura_dom::tree::NodeId,
        base_url: Option<&str>,
    ) -> Option<(String, ImageRequestProfile, bool, Option<(f32, f32)>)> {
        let node = tree.get_node(id)?;
        if node
            .as_element()
            .is_none_or(|element| element.local.as_ref() != "video")
        {
            return None;
        }
        let poster = node.get_attribute("poster")?.trim();
        if poster.is_empty() {
            return None;
        }
        let resolved_url = resolve_resource_url(poster, base_url)?;
        let profile = image_request_profile(tree, id);
        match self.cached_profiled_image_metadata(&resolved_url, None, profile) {
            None => Some((resolved_url, profile, false, None)),
            Some(dimensions) => Some((
                resolved_url,
                profile,
                true,
                dimensions.map(|(_, width, height)| (width, height)),
            )),
        }
    }

    fn get_or_load(&mut self, url: &str) -> Option<Arc<[u8]>> {
        let url = network_resource_url(url);
        if let Some(entry) = self.entries.get(&url) {
            match entry {
                CachedResource::Bytes(bytes) => return Some(Arc::clone(bytes)),
                CachedResource::Missing(at) if at.elapsed() < MISSING_RESOURCE_RETRY_AFTER => {
                    return None;
                }
                CachedResource::Missing(_) => {}
            }
        }
        if !self.sync_loading_enabled {
            return None;
        }
        self.remove(&url);

        let loaded = self.loader.load(&url).map(Arc::<[u8]>::from);
        match loaded {
            Some(bytes) => {
                self.insert_bytes(url, Arc::clone(&bytes));
                Some(bytes)
            }
            None => {
                self.insert_missing(url);
                None
            }
        }
    }

    fn get_or_load_image(
        &mut self,
        url: &str,
        profile: ImageRequestProfile,
    ) -> Option<Arc<[u8]>> {
        let key = image_resource_key(url, profile);
        if let Some(entry) = self.entries.get(&key) {
            match entry {
                CachedResource::Bytes(bytes) => return Some(Arc::clone(bytes)),
                CachedResource::Missing(at) if at.elapsed() < MISSING_RESOURCE_RETRY_AFTER => {
                    return None;
                }
                CachedResource::Missing(_) => {}
            }
        }
        if !self.sync_loading_enabled {
            return None;
        }
        self.remove(&key);
        // The private profile key is cache identity only and must never escape
        // into a network loader.
        let loaded = self
            .loader
            .load(&network_resource_url(url))
            .map(Arc::<[u8]>::from);
        match loaded {
            Some(bytes) => {
                self.insert_bytes(key, Arc::clone(&bytes));
                Some(bytes)
            }
            None => {
                self.insert_missing(key);
                None
            }
        }
    }

    fn insert_bytes(&mut self, url: String, bytes: Arc<[u8]>) {
        if self.max_entries == 0 || bytes.len() > self.max_bytes {
            return;
        }
        while self.entries.len() >= self.max_entries
            || self.retained_bytes.saturating_add(bytes.len()) > self.max_bytes
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.remove_entry(&oldest);
        }
        self.retained_bytes = self.retained_bytes.saturating_add(bytes.len());
        self.order.push_back(url.clone());
        self.entries.insert(url, CachedResource::Bytes(bytes));
    }

    fn insert_missing(&mut self, url: String) {
        if self.max_entries == 0 {
            return;
        }
        while self.entries.len() >= self.max_entries {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.remove_entry(&oldest);
        }
        self.order.push_back(url.clone());
        self.entries
            .insert(url, CachedResource::Missing(std::time::Instant::now()));
    }

    fn remove(&mut self, url: &str) {
        if self.entries.contains_key(url) {
            self.order.retain(|key| key != url);
            self.remove_entry(url);
        }
    }

    fn remove_entry(&mut self, url: &str) {
        if let Some(CachedResource::Bytes(bytes)) = self.entries.remove(url) {
            self.retained_bytes = self.retained_bytes.saturating_sub(bytes.len());
        }
    }

    /// Seed a prior stable `content:url(...)` selection into the ordinary
    /// intrinsic and selected-image maps before cascade. Node ids are scoped
    /// to this page cache; removed/non-image ids are pruned eagerly.
    fn seed_content_image_intrinsics(
        &mut self,
        tree: &DomTree,
        intrinsic: &mut HashMap<obscura_dom::tree::NodeId, crate::ReplacedIntrinsic>,
        selected: &mut HashMap<obscura_dom::tree::NodeId, SelectedImage>,
    ) -> HashSet<obscura_dom::tree::NodeId> {
        let remembered = self
            .content_image_intrinsics
            .iter()
            .map(|(&nid, value)| (nid, value.clone()))
            .collect::<Vec<_>>();
        let mut seeded = HashSet::with_capacity(remembered.len());
        for (nid, value) in remembered {
            let is_live_image = tree.get_node(nid).is_some_and(|node| {
                node.as_element()
                    .is_some_and(|element| element.local.as_ref() == "img")
            });
            if !is_live_image {
                self.forget_content_image_intrinsic(nid);
                continue;
            }
            intrinsic.insert(nid, value.intrinsic);
            selected.insert(
                nid,
                SelectedImage {
                    resolved_url: value.resolved_url,
                    density: 1.0,
                    profile: ImageRequestProfile::NoCorsInclude,
                },
            );
            seeded.insert(nid);
        }
        seeded
    }

    fn remember_content_image_intrinsic(
        &mut self,
        nid: obscura_dom::tree::NodeId,
        resolved_url: String,
        intrinsic: crate::ReplacedIntrinsic,
    ) {
        if self.max_content_image_intrinsics == 0 {
            return;
        }
        let replacing = self.content_image_intrinsics.contains_key(&nid);
        self.content_image_intrinsic_order
            .retain(|remembered| *remembered != nid);
        while !replacing && self.content_image_intrinsics.len() >= self.max_content_image_intrinsics
        {
            let Some(oldest) = self.content_image_intrinsic_order.pop_front() else {
                break;
            };
            self.content_image_intrinsics.remove(&oldest);
        }
        self.content_image_intrinsic_order.push_back(nid);
        self.content_image_intrinsics.insert(
            nid,
            RememberedContentImageIntrinsic {
                resolved_url,
                intrinsic,
            },
        );
    }

    fn forget_content_image_intrinsic(&mut self, nid: obscura_dom::tree::NodeId) {
        if self.content_image_intrinsics.remove(&nid).is_some() {
            self.content_image_intrinsic_order
                .retain(|remembered| *remembered != nid);
        }
    }
}

/// Inspect already-fetched image bytes without inserting them into a resource
/// cache. HTMLImageElement keeps request-profile-specific lifecycle outcomes
/// even though the renderer's ordinary paint cache is URL-keyed.
pub fn image_intrinsic_dimensions(bytes: &[u8]) -> Option<(f32, f32)> {
    image_metadata_from_bytes(bytes)
}

/// The exact responsive image candidate chosen during preparation.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectedImage {
    pub resolved_url: String,
    pub density: f32,
    pub profile: ImageRequestProfile,
}

/// A short-lived view of one JavaScript canvas backing store. The renderer
/// deliberately borrows raw RGBA only for a synchronous paint; retained
/// layout never owns VM-specific resources or pixel copies.
#[derive(Clone, Copy, Debug)]
pub struct CanvasSurface<'a> {
    width: u32,
    height: u32,
    rgba: &'a [u8],
}

impl<'a> CanvasSurface<'a> {
    pub fn from_rgba8(width: u32, height: u32, rgba: &'a [u8]) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (expected == rgba.len()).then_some(Self {
            width,
            height,
            rgba,
        })
    }
}

/// Paint-time lookup for dynamic canvas pixels. Implementations may retain a
/// VM backing store, but the returned slice cannot escape the capture call.
pub trait CanvasSurfaceSource {
    fn surface(&self, node: obscura_dom::tree::NodeId) -> Option<CanvasSurface<'_>>;
}

struct EmptyCanvasSurfaceSource;

impl CanvasSurfaceSource for EmptyCanvasSurfaceSource {
    fn surface(&self, _node: obscura_dom::tree::NodeId) -> Option<CanvasSurface<'_>> {
        None
    }
}

static EMPTY_CANVAS_SURFACES: EmptyCanvasSurfaceSource = EmptyCanvasSurfaceSource;

/// CSSOM scrolling metrics for one element box. Non-scroll containers expose
/// their padding-box size on both surfaces and retain a zero offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ElementScrollMetrics {
    pub client_size: (f32, f32),
    pub content_size: (f32, f32),
    pub offset: (f32, f32),
    pub max_offset: (f32, f32),
}

/// One fully resolved scrolling snapshot shared by geometry and paint. Node
/// movement and inherited clips are dense vectors indexed directly by
/// `NodeId`, so a capture pays one top-down resolution pass and no hot-path
/// ancestor walks or per-node scroll hash lookups.
#[derive(Clone, Debug)]
pub struct ResolvedScrollState {
    root_offset: (f32, f32),
    container_offsets: Vec<(f32, f32)>,
    node_movement: Vec<(f32, f32)>,
    inherited_clips: Vec<Option<crate::dom::OverflowClip>>,
}

/// A rectangle in immutable document CSS-pixel coordinates.
///
/// `scale` controls output pixels per CSS pixel without changing the prepared
/// layout viewport. This is the same separation Chromium makes between the
/// page-space clip and the screenshot surface scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptureRegion {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    output_size: Option<(u32, u32)>,
}

impl CaptureRegion {
    pub fn new(x: f32, y: f32, width: f32, height: f32, scale: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            scale,
            output_size: None,
        }
    }

    /// Construct a region whose transport protocol has already resolved the
    /// exact output pixel size. The CSS-space paint extent remains independent
    /// of that integer output surface.
    pub fn with_output_size(
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        scale: f32,
        output_width: u32,
        output_height: u32,
    ) -> Self {
        Self {
            x,
            y,
            width,
            height,
            scale,
            output_size: Some((output_width, output_height)),
        }
    }
}

/// Hard surface limits for one capture. The byte cap bounds both the native
/// CSS-pixel raster and the scaled output before either allocation occurs.
pub const MAX_CAPTURE_DIMENSION: u32 = 32_768;
pub const MAX_CAPTURE_PIXELS: u64 = 16 * 1024 * 1024;
pub const MAX_CAPTURE_SCALE: f32 = 16.0;
pub const MAX_CAPTURE_PEAK_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureError {
    InvalidRegion,
    AllocationLimitExceeded,
    PaintFailed,
    EncodeFailed,
}

fn checked_capture_dimensions(region: CaptureRegion) -> Result<(u32, u32, u32, u32), CaptureError> {
    if !region.x.is_finite()
        || !region.y.is_finite()
        || !region.width.is_finite()
        || !region.height.is_finite()
        || !region.scale.is_finite()
        || region.width <= 0.0
        || region.height <= 0.0
        || region.scale <= 0.0
    {
        return Err(CaptureError::InvalidRegion);
    }
    if region.scale > MAX_CAPTURE_SCALE {
        return Err(CaptureError::AllocationLimitExceeded);
    }
    let native_width = region.width.ceil();
    let native_height = region.height.ceil();
    let (output_width, output_height) = match region.output_size {
        Some((width, height)) => (f64::from(width), f64::from(height)),
        None => (
            f64::from(region.width) * f64::from(region.scale),
            f64::from(region.height) * f64::from(region.scale),
        ),
    };
    let output_width = output_width.ceil();
    let output_height = output_height.ceil();
    if !native_width.is_finite()
        || !native_height.is_finite()
        || !output_width.is_finite()
        || !output_height.is_finite()
        || output_width <= 0.0
        || output_height <= 0.0
        || native_width > MAX_CAPTURE_DIMENSION as f32
        || native_height > MAX_CAPTURE_DIMENSION as f32
        || output_width > f64::from(MAX_CAPTURE_DIMENSION)
        || output_height > f64::from(MAX_CAPTURE_DIMENSION)
    {
        return Err(CaptureError::AllocationLimitExceeded);
    }
    let dimensions = (
        native_width as u32,
        native_height as u32,
        output_width as u32,
        output_height as u32,
    );
    for (width, height) in [(dimensions.0, dimensions.1), (dimensions.2, dimensions.3)] {
        if u64::from(width).saturating_mul(u64::from(height)) > MAX_CAPTURE_PIXELS {
            return Err(CaptureError::AllocationLimitExceeded);
        }
    }
    let native_pixels = u64::from(dimensions.0).saturating_mul(u64::from(dimensions.1));
    let output_pixels = u64::from(dimensions.2).saturating_mul(u64::from(dimensions.3));
    let peak_pixels = if dimensions.0 == dimensions.2 && dimensions.1 == dimensions.3 {
        native_pixels
    } else {
        // Scaling owns the native RGBA surface and output RGBA surface at the
        // same time. Bound their combined live bytes before either allocation.
        native_pixels.saturating_add(output_pixels)
    };
    if peak_pixels.saturating_mul(4) > MAX_CAPTURE_PEAK_BYTES {
        return Err(CaptureError::AllocationLimitExceeded);
    }
    Ok(dimensions)
}

/// Validate every surface allocation implied by a capture before painting.
/// Protocol adapters use this on legacy viewport captures which preserve the
/// renderer's native PNG bytes but must obey the same limits as region capture.
pub fn validate_capture_region(region: CaptureRegion) -> Result<(), CaptureError> {
    checked_capture_dimensions(region).map(|_| ())
}

impl ResolvedScrollState {
    pub fn root_offset(&self) -> (f32, f32) {
        self.root_offset
    }

    fn movement_for(&self, id: obscura_dom::tree::NodeId) -> (f32, f32) {
        self.node_movement
            .get(id.index())
            .copied()
            .unwrap_or((0.0, 0.0))
    }

    fn inherited_clip_for(
        &self,
        id: obscura_dom::tree::NodeId,
    ) -> Option<crate::dom::OverflowClip> {
        self.inherited_clips.get(id.index()).cloned().flatten()
    }
}

/// A final image/font-aware document layout retained across viewport paints.
/// The DOM must not be mutated while this value is reused.
pub struct PreparedRender {
    viewport: (f32, f32),
    animation_sample: crate::AnimationSample,
    has_active_waapi_animations: bool,
    active_animation_impact: crate::AnimationEffectImpact,
    root_font_size: f32,
    base_url: Option<String>,
    has_dynamic_fonts: bool,
    content_size: (f32, f32),
    viewport_fixed: std::collections::HashSet<obscura_dom::tree::NodeId>,
    sticky: crate::StickyLayout,
    scroll_tree: crate::dom::ScrollTree,
    selected_images: HashMap<obscura_dom::tree::NodeId, SelectedImage>,
    svg_fonts: Arc<usvg::fontdb::Database>,
    layout: crate::DomLayout,
}

impl PreparedRender {
    pub fn viewport(&self) -> (f32, f32) {
        self.viewport
    }

    pub fn animation_sample_time(&self) -> crate::AnimationSampleTime {
        self.animation_sample.time
    }

    pub fn animation_sample(&self) -> crate::AnimationSample {
        self.animation_sample
    }

    /// Whether advancing the document timeline can still change a sampled CSS
    /// animation. Finite animations stop producing compositor damage after
    /// their active interval; paused and zero-duration animations never do.
    pub fn has_active_css_animations(&self) -> bool {
        self.has_active_waapi_animations
            || self
                .layout
                .styles
                .values()
                .any(css_animation_is_active)
    }

    fn has_active_declarative_css_animations(&self) -> bool {
        self.layout.styles.values().any(css_animation_is_active)
    }

    /// Advance opacity/transform Web Animations without rebuilding normal-flow
    /// layout. Eligibility is intentionally strict: every target is sampled
    /// from retained pre-WAAPI cascade provenance, and every update passes
    /// preflight before the prepared render is mutated.
    fn try_advance_visual_waapi_sample(
        &mut self,
        tree: &DomTree,
        sample: crate::AnimationSample,
        timeline: &crate::AnimationTimelineState,
    ) -> bool {
        if self.has_active_declarative_css_animations() {
            return false;
        }
        let connected = |node: obscura_dom::tree::NodeId| {
            tree.get_node(node).is_some()
                && (node == tree.document() || tree.ancestors(node).contains(&tree.document()))
        };
        let mut updates = Vec::new();
        let mut has_transform_effect = false;
        let mut has_opacity_effect = false;
        for node in timeline.waapi_nodes().into_iter().filter(|node| connected(*node)) {
            let Some(style) = self.layout.styles.get(&node) else {
                return false;
            };
            let Some(sampled) =
                crate::css::resample_visual_waapi(timeline, node, style, sample)
            else {
                return false;
            };
            let established_transform_cb =
                style.containing_block_triggers & crate::CB_TRIGGER_TRANSFORM != 0;
            if sampled.establishes_transform_cb != established_transform_cb {
                return false;
            }
            has_transform_effect |= sampled.has_transform_effect;
            has_opacity_effect |= sampled.has_opacity_effect;
            updates.push((node, sampled));
        }
        if updates.is_empty() {
            return false;
        }
        for (node, sampled) in updates {
            let style = self.layout
                .styles
                .get_mut(&node)
                .expect("WAAPI target passed preflight");
            style.transform_ops = sampled.transform_ops;
            style.opacity = sampled.opacity;
        }
        if has_opacity_effect {
            self.layout.refresh_effective_visibility(tree);
        }
        if has_transform_effect {
            self.layout.refresh_visual_geometry(tree, self.viewport);
            // A retained transform sample may change visual overflow, sticky
            // constraints, and scrolling ranges, but the preflight above has
            // already rejected any containing-block topology change. Reuse
            // the immutable viewport-fixed ownership instead of walking the
            // document again on every animation frame.
            let derived = self.layout.derived_geometry_with_fixed(
                tree,
                self.viewport,
                &self.viewport_fixed,
            );
            self.content_size = derived.content_size;
            self.sticky = derived.sticky;
            self.scroll_tree = derived.scroll_tree;
        }
        self.animation_sample = sample;
        self.has_active_waapi_animations = timeline.has_active_waapi(sample.time);
        self.active_animation_impact = timeline.active_waapi_effect_impact(sample.time);
        true
    }

    /// Whether a geometry-only consumer may retain this layout while moving
    /// to `sample`. The prepared style/paint sample deliberately stays
    /// unchanged; a later computed-style or paint consumer will materialize
    /// the exact requested sample through the normal retained-style path.
    pub fn can_reuse_geometry_for_animation_sample(
        &self,
        sample: crate::AnimationSample,
    ) -> bool {
        sample.mode == crate::AnimationSampleMode::DocumentTime
            && self.animation_sample.mode == crate::AnimationSampleMode::DocumentTime
            && sample.time.milliseconds >= self.animation_sample.time.milliseconds
            && self.active_animation_impact < crate::AnimationEffectImpact::Geometry
    }

    /// Advance a frame whose animation cascade is already time-invariant.
    /// This preserves retained layout for static pages and completed finite
    /// animations. Backward seeks are never eligible because they can re-enter
    /// an earlier active interval or undo forwards fill.
    pub fn advance_inactive_animation_sample_time(
        &mut self,
        sample: crate::AnimationSampleTime,
    ) -> bool {
        if self.animation_sample.mode != crate::AnimationSampleMode::DocumentTime
            || sample.milliseconds < self.animation_sample.time.milliseconds
            || self.has_active_css_animations()
        {
            return false;
        }
        self.animation_sample.time = sample;
        true
    }

    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    pub fn layout(&self) -> &crate::DomLayout {
        &self.layout
    }

    pub fn content_size(&self) -> (f32, f32) {
        self.content_size
    }

    pub fn viewport_fixed_nodes(&self) -> &std::collections::HashSet<obscura_dom::tree::NodeId> {
        &self.viewport_fixed
    }

    pub fn sticky_layout(&self) -> &crate::StickyLayout {
        &self.sticky
    }

    pub fn clamp_scroll(&self, requested: (f32, f32)) -> (f32, f32) {
        self.clamp_scroll_for_viewport(requested, self.viewport)
    }

    fn clamp_scroll_for_viewport(
        &self,
        requested: (f32, f32),
        viewport: (f32, f32),
    ) -> (f32, f32) {
        let clamp_axis = |requested: f32, content: f32, viewport: f32| {
            if requested.is_finite() {
                crate::quantize_scroll_value(requested, 1.0)
                    .clamp(0.0, crate::quantized_scroll_range(content, viewport, 1.0))
            } else {
                0.0
            }
        };
        (
            clamp_axis(requested.0, self.content_size.0, viewport.0),
            clamp_axis(requested.1, self.content_size.1, viewport.1),
        )
    }

    fn root_only_movement_for(
        &self,
        id: obscura_dom::tree::NodeId,
        requested_scroll: (f32, f32),
    ) -> (f32, f32) {
        let root = self.clamp_scroll(requested_scroll);
        let mut cumulative = vec![(0.0, 0.0); self.scroll_tree.containers.len()];
        cumulative[0] = (-root.0, -root.1);
        for index in 1..cumulative.len() {
            let parent = self.scroll_tree.containers[index].parent;
            cumulative[index] = parent
                .map(|parent| cumulative[parent.index()])
                .unwrap_or((0.0, 0.0));
        }
        let mut movement = self
            .scroll_tree
            .movement_owner
            .get(id.index())
            .copied()
            .flatten()
            .and_then(|owner| cumulative.get(owner.index()).copied())
            .unwrap_or((0.0, 0.0));
        let sticky = self.sticky.resolved_translation_for(
            id,
            self.viewport,
            &self.scroll_tree,
            &cumulative,
        );
        movement.0 += sticky.0;
        movement.1 += sticky.1;
        movement
    }

    /// Element scroll containers in this prepared layout, in stable DOM order.
    pub fn scroll_container_nodes(&self) -> impl Iterator<Item = obscura_dom::tree::NodeId> + '_ {
        self.scroll_tree
            .containers
            .iter()
            .skip(1)
            .filter_map(|container| container.node)
    }

    /// Resolve persistent NodeId-keyed offsets into this layout's dense scroll
    /// topology. Unknown/removed nodes are ignored; every retained offset is
    /// clamped against the final local scrolling overflow.
    pub fn resolve_scroll_state(
        &self,
        tree: &DomTree,
        requested_root: (f32, f32),
        requested_elements: &HashMap<obscura_dom::tree::NodeId, (f32, f32)>,
    ) -> ResolvedScrollState {
        self.resolve_scroll_state_for_viewport(
            tree,
            requested_root,
            requested_elements,
            self.viewport,
        )
    }

    /// Resolve scroll-time movement for a virtual capture viewport without
    /// mutating the live page scroll. Paginated raster consumers use each page
    /// slice as its viewport so fixed/sticky content is positioned at that
    /// page's origin, including a final slice shorter than the live viewport.
    pub fn resolve_scroll_state_for_viewport(
        &self,
        tree: &DomTree,
        requested_root: (f32, f32),
        requested_elements: &HashMap<obscura_dom::tree::NodeId, (f32, f32)>,
        viewport: (f32, f32),
    ) -> ResolvedScrollState {
        let root = self.clamp_scroll_for_viewport(requested_root, viewport);
        let mut offsets = vec![(0.0, 0.0); self.scroll_tree.containers.len()];
        offsets[0] = root;
        for (index, container) in self.scroll_tree.containers.iter().enumerate().skip(1) {
            let requested = container
                .node
                .and_then(|node| requested_elements.get(&node).copied())
                .unwrap_or((0.0, 0.0));
            let clamp = |value: f32, max: f32| {
                if value.is_finite() {
                    crate::quantize_scroll_value(value, 1.0).clamp(0.0, max)
                } else {
                    0.0
                }
            };
            offsets[index] = (
                clamp(requested.0, container.max_offset.0),
                clamp(requested.1, container.max_offset.1),
            );
        }

        let mut cumulative = vec![(0.0, 0.0); offsets.len()];
        cumulative[0] = (-root.0, -root.1);
        for index in 1..offsets.len() {
            let container = self.scroll_tree.containers[index];
            let inherited = container
                .parent
                .map(|parent| cumulative[parent.index()])
                .unwrap_or((0.0, 0.0));
            cumulative[index] = (
                inherited.0 - offsets[index].0,
                inherited.1 - offsets[index].1,
            );
        }

        let node_len = self.scroll_tree.movement_owner.len();
        let mut node_movement = vec![(0.0, 0.0); node_len];
        for (index, owner) in self.scroll_tree.movement_owner.iter().copied().enumerate() {
            if let Some(owner) = owner {
                node_movement[index] = cumulative[owner.index()];
            }
        }
        for (id, sticky) in
            self.sticky
                .resolved_translations(viewport, &self.scroll_tree, &cumulative)
        {
            if let Some(movement) = node_movement.get_mut(id.index()) {
                movement.0 += sticky.0;
                movement.1 += sticky.1;
            }
        }

        let mut inherited_clips = vec![None; node_len];
        fn resolve_clips(
            tree: &DomTree,
            laid: &crate::DomLayout,
            movement: &[(f32, f32)],
            viewport_fixed: &std::collections::HashSet<obscura_dom::tree::NodeId>,
            id: obscura_dom::tree::NodeId,
            inherited: Option<crate::dom::OverflowClip>,
            out: &mut [Option<crate::dom::OverflowClip>],
        ) {
            // A fixed-position box whose containing block is the viewport
            // escapes clips established by ancestors in document space. Only
            // reset at the boundary: descendants still inherit clips created
            // inside the fixed subtree, while transform/filter/contain-
            // captured fixed boxes are absent from `viewport_fixed` and retain
            // their ordinary ancestor clips.
            let starts_viewport_fixed = viewport_fixed.contains(&id)
                && crate::dom::rendered_parent(tree, id)
                    .is_none_or(|parent| !viewport_fixed.contains(&parent));
            let inherited = if starts_viewport_fixed {
                None
            } else {
                inherited
            };
            if let Some(slot) = out.get_mut(id.index()) {
                *slot = inherited.clone();
            }
            let next = match (laid.styles.get(&id), laid.rects.get(&id)) {
                (Some(style), Some(rect))
                    if style.overflow_hidden && !style.overflow_propagated_to_viewport =>
                {
                    let authored = laid.translates.get(&id).copied().unwrap_or((0.0, 0.0));
                    let scroll = movement.get(id.index()).copied().unwrap_or((0.0, 0.0));
                    let own = crate::dom::OverflowClip::for_box(
                        rect,
                        style,
                        authored.0 + scroll.0,
                        authored.1 + scroll.1,
                    );
                    Some(match inherited {
                        Some(clip) => clip.intersect(own),
                        None => own,
                    })
                }
                _ => inherited,
            };
            for child in crate::dom::rendered_children(tree, id) {
                resolve_clips(
                    tree,
                    laid,
                    movement,
                    viewport_fixed,
                    child,
                    next.clone(),
                    out,
                );
            }
        }
        if let Some(root_node) = tree
            .descendants(tree.document())
            .into_iter()
            .find(|id| tree.get_node(*id).is_some_and(|node| node.is_element()))
        {
            resolve_clips(
                tree,
                &self.layout,
                &node_movement,
                &self.viewport_fixed,
                root_node,
                None,
                &mut inherited_clips,
            );
        }

        ResolvedScrollState {
            root_offset: root,
            container_offsets: offsets,
            node_movement,
            inherited_clips,
        }
    }

    pub fn element_scroll_metrics(
        &self,
        id: obscura_dom::tree::NodeId,
        state: &ResolvedScrollState,
    ) -> Option<ElementScrollMetrics> {
        let client = self.client_size(id)?;
        let sid = self
            .scroll_tree
            .node_container
            .get(id.index())
            .copied()
            .flatten();
        let Some(sid) = sid else {
            let content = self
                .scroll_tree
                .node_content_size
                .get(id.index())
                .copied()
                .flatten()?;
            return Some(ElementScrollMetrics {
                client_size: client,
                content_size: content,
                offset: (0.0, 0.0),
                max_offset: (0.0, 0.0),
            });
        };
        let container = self.scroll_tree.containers[sid.index()];
        Some(ElementScrollMetrics {
            client_size: container.client_size,
            content_size: container.content_size,
            offset: state
                .container_offsets
                .get(sid.index())
                .copied()
                .unwrap_or((0.0, 0.0)),
            max_offset: container.max_offset,
        })
    }

    /// Axis-aligned bounds of the transformed border box in immutable document
    /// space, excluding root-scroll and sticky movement.
    pub fn document_rect(&self, id: obscura_dom::tree::NodeId) -> Option<crate::Rect> {
        let rect = *self.layout.rects.get(&id)?;
        Some(
            self.layout
                .transforms
                .get(&id)
                .copied()
                .map(|transform| transform.map_rect(rect))
                .unwrap_or(rect),
        )
    }

    /// Unscaled padding-box size used by CSSOM View's `clientWidth` and
    /// `clientHeight`. Layout rects are border boxes, so remove the resolved
    /// borders but retain padding. This deliberately ignores visual
    /// transforms: client metrics describe layout geometry, unlike
    /// `getBoundingClientRect()`.
    pub fn client_size(&self, id: obscura_dom::tree::NodeId) -> Option<(f32, f32)> {
        let rect = self.layout.rects.get(&id)?;
        let style = self.layout.styles.get(&id)?;
        if style.ignores_used_box_sizes() {
            return Some((0.0, 0.0));
        }
        Some((
            (rect.width - style.border.left - style.border.right).max(0.0),
            (rect.height - style.border.top - style.border.bottom).max(0.0),
        ))
    }

    /// A compact CSSOM snapshot derived from the same final cascade and
    /// layout used by paint and geometry. Keeping this on `PreparedRender`
    /// lets script fetch all high-traffic computed properties in one op,
    /// without rebuilding layout once per property access.
    pub fn computed_style(
        &self,
        id: obscura_dom::tree::NodeId,
    ) -> Option<HashMap<&'static str, String>> {
        let style = self.layout.styles.get(&id)?;
        let rect = self.layout.rects.get(&id);
        let mut out = HashMap::new();

        let active_webkit_clamp = style.webkit_box_display.is_some()
            && style.webkit_box_orient_vertical
            && style.webkit_line_clamp.is_some();
        let display = if style.display_contents {
            "contents"
        } else if style.display == crate::Display::None {
            "none"
        } else if active_webkit_clamp && style.webkit_box_display == Some(false) {
            "flow-root"
        } else if style.webkit_box_display == Some(false) && !active_webkit_clamp {
            "-webkit-box"
        } else if style.webkit_box_display == Some(true) && !active_webkit_clamp {
            "-webkit-inline-box"
        } else if style.internal_flex_container {
            "block"
        } else {
            match (style.display, style.is_inline_block) {
                (crate::Display::Flex, true) => "inline-flex",
                (crate::Display::Grid, true) => "inline-grid",
                (crate::Display::Block, true) => "inline-block",
                (crate::Display::Flex, false) => "flex",
                (crate::Display::Grid, false) => "grid",
                (crate::Display::Inline, true) => "inline-block",
                (crate::Display::Inline, false) => "inline",
                _ => "block",
            }
        };
        out.insert("display", display.to_string());
        out.insert(
            "float",
            match style.float {
                Some(crate::Float::Left) => "left",
                Some(crate::Float::Right) => "right",
                None => "none",
            }
            .to_string(),
        );
        out.insert(
            "clear",
            match style.clear {
                Some(crate::Clear::Left) => "left",
                Some(crate::Clear::Right) => "right",
                Some(crate::Clear::Both) => "both",
                None => "none",
            }
            .to_string(),
        );
        out.insert(
            "position",
            if style.position_fixed {
                "fixed"
            } else if style.position_sticky {
                "sticky"
            } else {
                match style.position {
                    Some(taffy::Position::Absolute) => "absolute",
                    Some(taffy::Position::Relative) => "relative",
                    _ => "static",
                }
            }
            .to_string(),
        );
        out.insert(
            "z-index",
            style
                .z_index
                .map_or_else(|| "auto".to_string(), |v| v.to_string()),
        );
        out.insert(
            "visibility",
            if style.visibility_hidden.unwrap_or(false) {
                "hidden"
            } else {
                "visible"
            }
            .to_string(),
        );
        out.insert("opacity", css_number(style.opacity.unwrap_or(1.0)));
        out.insert(
            "background-color",
            css_color(style.background_color.unwrap_or([0, 0, 0, 0])),
        );
        out.insert(
            "background-origin",
            match style.background_origin {
                crate::BackgroundOrigin::BorderBox => "border-box",
                crate::BackgroundOrigin::PaddingBox => "padding-box",
                crate::BackgroundOrigin::ContentBox => "content-box",
            }
            .to_string(),
        );
        out.insert(
            "background-clip",
            match style.background_clip {
                crate::BackgroundClip::BorderBox => "border-box",
                crate::BackgroundClip::PaddingBox => "padding-box",
                crate::BackgroundClip::ContentBox => "content-box",
                crate::BackgroundClip::Text => "text",
            }
            .to_string(),
        );
        out.insert("color", css_color(style.color.unwrap_or([0, 0, 0, 255])));
        out.insert("font-size", css_px(style.font_size.unwrap_or(16.0)));
        out.insert(
            "font-weight",
            style
                .font_weight
                .clone()
                .unwrap_or_else(|| "400".to_string()),
        );
        if let Some(family) = &style.font_family {
            out.insert("font-family", family.clone());
        }
        out.insert(
            "line-height",
            match style.line_height.unwrap_or(crate::LineHeight::Normal) {
                crate::LineHeight::Normal => "normal".to_string(),
                _ => css_px(self.layout.text_engine.selected_line_height(style)),
            },
        );
        out.insert(
            "letter-spacing",
            match style.letter_spacing.unwrap_or(0.0) {
                value if value == 0.0 => "normal".to_string(),
                value => css_px(value),
            },
        );
        out.insert(
            "white-space",
            match style.white_space.unwrap_or_default() {
                crate::WhiteSpace::Normal => "normal",
                crate::WhiteSpace::NoWrap => "nowrap",
                crate::WhiteSpace::Pre => "pre",
                crate::WhiteSpace::PreWrap => "pre-wrap",
                crate::WhiteSpace::PreLine => "pre-line",
                crate::WhiteSpace::BreakSpaces => "break-spaces",
            }
            .to_string(),
        );
        out.insert(
            "text-overflow",
            match style.text_overflow {
                crate::TextOverflow::Clip => "clip",
                crate::TextOverflow::Ellipsis => "ellipsis",
            }
            .to_string(),
        );
        out.insert(
            "-webkit-line-clamp",
            style
                .webkit_line_clamp
                .map_or_else(|| "none".to_string(), |lines| lines.to_string()),
        );
        out.insert(
            "-webkit-box-orient",
            if style.webkit_box_orient_vertical {
                "vertical"
            } else {
                "horizontal"
            }
            .to_string(),
        );
        let overflow_wrap = match style.overflow_wrap.unwrap_or_default() {
            crate::OverflowWrap::Normal => "normal",
            crate::OverflowWrap::BreakWord => "break-word",
            crate::OverflowWrap::Anywhere => "anywhere",
        }
        .to_string();
        out.insert("overflow-wrap", overflow_wrap.clone());
        // CSSOM retains the legacy alias as a separately addressable property.
        out.insert("word-wrap", overflow_wrap);
        out.insert(
            "word-break",
            match style.word_break.unwrap_or_default() {
                crate::WordBreak::Normal => "normal",
                crate::WordBreak::BreakAll => "break-all",
                crate::WordBreak::KeepAll => "keep-all",
                crate::WordBreak::BreakWord => "break-word",
            }
            .to_string(),
        );
        out.insert(
            "text-align",
            match style.text_align {
                Some(taffy::AlignItems::CENTER) => "center",
                Some(taffy::AlignItems::FLEX_END | taffy::AlignItems::END) => "end",
                _ => "start",
            }
            .to_string(),
        );

        if style.ignores_used_box_sizes() {
            out.insert("width", dimension_css(style.width, "auto"));
            out.insert("height", dimension_css(style.height, "auto"));
        } else if let Some(rect) = rect {
            let horizontal_non_content =
                style.border.left + style.border.right + style.padding.left + style.padding.right;
            let vertical_non_content =
                style.border.top + style.border.bottom + style.padding.top + style.padding.bottom;
            let (width, height) = match style.box_sizing {
                crate::BoxSizing::BorderBox => (rect.width, rect.height),
                _ => (
                    (rect.width - horizontal_non_content).max(0.0),
                    (rect.height - vertical_non_content).max(0.0),
                ),
            };
            out.insert("width", css_px(width));
            out.insert("height", css_px(height));
        } else {
            out.insert("width", dimension_css(style.width, "auto"));
            out.insert("height", dimension_css(style.height, "auto"));
        }
        out.insert("min-width", dimension_css(style.min_width, "auto"));
        out.insert("min-height", dimension_css(style.min_height, "auto"));
        out.insert("max-width", dimension_css(style.max_width, "none"));
        out.insert("max-height", dimension_css(style.max_height, "none"));
        out.insert(
            "box-sizing",
            if style.box_sizing == crate::BoxSizing::BorderBox {
                "border-box"
            } else {
                "content-box"
            }
            .to_string(),
        );

        let overflow_axis = |specified: u8, clipped: bool, scroll: bool| {
            if scroll {
                // The compact layout model intentionally merges
                // hidden/auto/scroll for clipping. `auto` is the least
                // surprising computed scroll-container value.
                "auto"
            } else if specified == 1 || clipped {
                "clip"
            } else {
                "visible"
            }
        };
        out.insert(
            "overflow-x",
            overflow_axis(
                style.overflow_specified_x,
                style.overflow_clip_x,
                style.overflow_scroll_x,
            )
            .to_string(),
        );
        out.insert(
            "overflow-y",
            overflow_axis(
                style.overflow_specified_y,
                style.overflow_clip_y,
                style.overflow_scroll_y,
            )
            .to_string(),
        );

        for (name, value, auto) in [
            ("margin-top", style.margin.top, style.margin_auto[0]),
            ("margin-right", style.margin.right, style.margin_auto[1]),
            ("margin-bottom", style.margin.bottom, style.margin_auto[2]),
            ("margin-left", style.margin.left, style.margin_auto[3]),
        ] {
            out.insert(
                name,
                if auto {
                    "auto".to_string()
                } else {
                    css_px(value)
                },
            );
        }
        for (name, value) in [
            ("padding-top", style.padding.top),
            ("padding-right", style.padding.right),
            ("padding-bottom", style.padding.bottom),
            ("padding-left", style.padding.left),
            ("border-top-width", style.border.top),
            ("border-right-width", style.border.right),
            ("border-bottom-width", style.border.bottom),
            ("border-left-width", style.border.left),
        ] {
            out.insert(name, css_px(value));
        }
        let current_color = style.color.unwrap_or([0, 0, 0, 255]);
        for (name, color) in [
            ("border-top-color", style.border_model.colors.top),
            ("border-right-color", style.border_model.colors.right),
            ("border-bottom-color", style.border_model.colors.bottom),
            ("border-left-color", style.border_model.colors.left),
        ] {
            out.insert(
                name,
                css_color(color.or(style.border_color).unwrap_or(current_color)),
            );
        }
        let effective_border_styles = effective_border_styles(style);
        for (name, line_style) in [
            ("border-top-style", effective_border_styles.top),
            ("border-right-style", effective_border_styles.right),
            ("border-bottom-style", effective_border_styles.bottom),
            ("border-left-style", effective_border_styles.left),
        ] {
            out.insert(name, line_style.css_name().to_string());
        }
        for (name, radius) in [
            ("border-top-left-radius", style.border_model.radii.top_left),
            (
                "border-top-right-radius",
                style.border_model.radii.top_right,
            ),
            (
                "border-bottom-right-radius",
                style.border_model.radii.bottom_right,
            ),
            (
                "border-bottom-left-radius",
                style.border_model.radii.bottom_left,
            ),
        ] {
            out.insert(name, corner_radius_css(radius));
        }
        out.insert("outline-width", css_px(style.outline.used_width()));
        out.insert("outline-style", style.outline.style.css_name().to_string());
        out.insert(
            "outline-color",
            css_color(style.outline.color.unwrap_or(current_color)),
        );
        out.insert("outline-offset", css_px(style.outline.offset));

        out.insert(
            "flex-direction",
            match style.flex_direction.unwrap_or(taffy::FlexDirection::Row) {
                taffy::FlexDirection::Row => "row",
                taffy::FlexDirection::RowReverse => "row-reverse",
                taffy::FlexDirection::Column => "column",
                taffy::FlexDirection::ColumnReverse => "column-reverse",
            }
            .to_string(),
        );
        out.insert(
            "flex-wrap",
            match style.flex_wrap.unwrap_or(taffy::FlexWrap::NoWrap) {
                taffy::FlexWrap::NoWrap => "nowrap",
                taffy::FlexWrap::Wrap => "wrap",
                taffy::FlexWrap::WrapReverse => "wrap-reverse",
            }
            .to_string(),
        );
        out.insert(
            "align-items",
            style
                .align_items
                .map_or_else(|| "normal".to_string(), align_items_css),
        );
        out.insert(
            "justify-items",
            style
                .justify_items
                .map_or_else(|| "normal".to_string(), align_items_css),
        );
        out.insert(
            "justify-content",
            style
                .justify_content
                .map_or_else(|| "normal".to_string(), align_content_css),
        );
        out.insert(
            "align-content",
            style
                .align_content
                .map_or_else(|| "normal".to_string(), align_content_css),
        );
        out.insert(
            "column-gap",
            style
                .column_gap
                .map_or_else(|| "normal".to_string(), css_px),
        );
        out.insert(
            "row-gap",
            style.row_gap.map_or_else(|| "normal".to_string(), css_px),
        );
        out.insert(
            "grid-auto-flow",
            match style.grid_auto_flow.unwrap_or(taffy::GridAutoFlow::Row) {
                taffy::GridAutoFlow::Row => "row",
                taffy::GridAutoFlow::Column => "column",
                taffy::GridAutoFlow::RowDense => "row dense",
                taffy::GridAutoFlow::ColumnDense => "column dense",
            }
            .to_string(),
        );

        out.insert(
            "transform",
            transform_css(style, rect, self.root_font_size, self.viewport),
        );
        out.insert("transform-origin", transform_origin_css(style, rect));
        out.insert(
            "translate",
            style.individual_translate.map_or_else(
                || "none".to_string(),
                |(x, y)| format!("{} {}", dimension_css(x, "0px"), dimension_css(y, "0px")),
            ),
        );
        out.insert(
            "rotate",
            style.individual_rotate.map_or_else(
                || "none".to_string(),
                |angle| format!("{}deg", css_number(angle)),
            ),
        );
        out.insert(
            "scale",
            style.individual_scale.map_or_else(
                || "none".to_string(),
                |(x, y)| format!("{} {}", css_number(x), css_number(y)),
            ),
        );
        Some(out)
    }

    /// Cascaded custom properties exposed by CSSOM alongside the compact
    /// fixed-property snapshot above. These maps are shared across unchanged
    /// inherited subtrees by `DomLayout`, so reading one does not require a
    /// second cascade or duplicate every design token for every descendant.
    pub fn computed_custom_properties(
        &self,
        id: obscura_dom::tree::NodeId,
    ) -> Option<HashMap<String, String>> {
        let properties = self.layout.custom_properties.get(&id)?;
        Some(properties.as_ref().clone())
    }

    /// Border box in the current root viewport. This is the read-only geometry
    /// path used by a later CSSOM integration and shares paint's clamped scroll,
    /// fixed-subtree, and sticky-positioning derivatives.
    pub fn viewport_rect(
        &self,
        id: obscura_dom::tree::NodeId,
        requested_scroll: (f32, f32),
    ) -> Option<crate::Rect> {
        let mut rect = self.document_rect(id)?;
        let movement = self.root_only_movement_for(id, requested_scroll);
        rect.x += movement.0;
        rect.y += movement.1;
        Some(rect)
    }

    /// Border box in the viewport using a pre-resolved root + element scroll
    /// snapshot. This is the production CSSOM path; the tuple-only method
    /// above remains for compatibility callers that cannot own element state.
    pub fn viewport_rect_with_scroll(
        &self,
        id: obscura_dom::tree::NodeId,
        scroll: &ResolvedScrollState,
    ) -> Option<crate::Rect> {
        let mut rect = self.document_rect(id)?;
        let movement = scroll.movement_for(id);
        rect.x += movement.0;
        rect.y += movement.1;
        Some(rect)
    }

    /// Every CSS border-box fragment in the current root viewport. Ordinary
    /// inlines can have one fragment per line; all other boxes expose their
    /// single border box. Keeping this separate from the bounding union is
    /// required by CSSOM View's `getClientRects()`.
    pub fn viewport_client_rects(
        &self,
        id: obscura_dom::tree::NodeId,
        requested_scroll: (f32, f32),
    ) -> Option<Vec<crate::Rect>> {
        let source = self
            .layout
            .inline_fragments
            .get(&id)
            .cloned()
            .or_else(|| self.layout.rects.get(&id).copied().map(|rect| vec![rect]))?;
        let movement = self.root_only_movement_for(id, requested_scroll);
        Some(
            source
                .into_iter()
                .map(|rect| {
                    let rect = self
                        .layout
                        .transforms
                        .get(&id)
                        .copied()
                        .map(|transform| transform.map_rect(rect))
                        .unwrap_or(rect);
                    crate::Rect {
                        x: rect.x + movement.0,
                        y: rect.y + movement.1,
                        ..rect
                    }
                })
                .collect(),
        )
    }

    pub fn viewport_client_rects_with_scroll(
        &self,
        id: obscura_dom::tree::NodeId,
        scroll: &ResolvedScrollState,
    ) -> Option<Vec<crate::Rect>> {
        let source = self
            .layout
            .inline_fragments
            .get(&id)
            .cloned()
            .or_else(|| self.layout.rects.get(&id).copied().map(|rect| vec![rect]))?;
        let movement = scroll.movement_for(id);
        Some(
            source
                .into_iter()
                .map(|rect| {
                    let rect = self
                        .layout
                        .transforms
                        .get(&id)
                        .copied()
                        .map(|transform| transform.map_rect(rect))
                        .unwrap_or(rect);
                    crate::Rect {
                        x: rect.x + movement.0,
                        y: rect.y + movement.1,
                        ..rect
                    }
                })
                .collect(),
        )
    }

    pub fn selected_image(&self, id: obscura_dom::tree::NodeId) -> Option<&SelectedImage> {
        self.selected_images.get(&id)
    }

    /// Whether newly available bytes for one selected image can change box
    /// geometry. A replaced image whose two used axes are authored lengths
    /// does not consult its natural dimensions; resource completion only
    /// changes the pixels painted inside the existing content box.
    ///
    /// Keep flex/grid items on the conservative path. Their intrinsic
    /// contribution participates in sizing algorithms even when the item has
    /// preferred dimensions, and browsers likewise propagate image-size
    /// invalidation through those formatting contexts.
    pub fn image_resource_needs_geometry(
        &self,
        tree: &DomTree,
        url: &str,
        profile: ImageRequestProfile,
    ) -> bool {
        self.selected_images.iter().any(|(id, selected)| {
            if selected.resolved_url != url || selected.profile != profile {
                return false;
            }
            if !tree.get_node(*id).is_some_and(|node| {
                node.as_element()
                    .is_some_and(|name| name.local.as_ref() == "img")
            }) {
                return true;
            }
            let Some(style) = self.layout.styles.get(id) else {
                return true;
            };
            // CSS replaced content has its own selected intrinsic metadata;
            // do not mistake an <img> owner for an ordinary fixed source
            // image merely because both selections share the element id.
            if style.content_image.is_some() {
                return true;
            }
            let fixed_box = matches!(style.width, crate::Dimension::Px(_))
                && matches!(style.height, crate::Dimension::Px(_))
                && matches!(style.min_width, crate::Dimension::Auto | crate::Dimension::Px(_))
                && matches!(style.min_height, crate::Dimension::Auto | crate::Dimension::Px(_))
                && matches!(style.max_width, crate::Dimension::Auto | crate::Dimension::Px(_))
                && matches!(style.max_height, crate::Dimension::Auto | crate::Dimension::Px(_))
                && !style.width_fit_content
                && style.size_expressions.iter().all(Option::is_none);
            if !fixed_box {
                return true;
            }

            let mut parent = crate::dom::rendered_parent(tree, *id);
            while let Some(parent_id) = parent {
                let Some(parent_style) = self.layout.styles.get(&parent_id) else {
                    parent = crate::dom::rendered_parent(tree, parent_id);
                    continue;
                };
                if parent_style.display_contents {
                    parent = crate::dom::rendered_parent(tree, parent_id);
                    continue;
                }
                return parent_style.display == crate::Display::Grid
                    || (parent_style.display == crate::Display::Flex
                        && !parent_style.internal_flex_container);
            }
            false
        })
    }
}

fn css_number(value: f32) -> String {
    if value == 0.0 {
        "0".to_string()
    } else {
        value.to_string()
    }
}

fn css_px(value: f32) -> String {
    format!("{}px", css_number(if value == 0.0 { 0.0 } else { value }))
}

fn dimension_css(value: crate::Dimension, auto: &str) -> String {
    match value {
        crate::Dimension::Auto => auto.to_string(),
        crate::Dimension::Px(v) => css_px(v),
        crate::Dimension::Percent(v) => format!("{}%", css_number(v * 100.0)),
        crate::Dimension::Em(v) => format!("{}em", css_number(v)),
        crate::Dimension::Ex(v) => format!("{}ex", css_number(v)),
        crate::Dimension::Rem(v) => format!("{}rem", css_number(v)),
        crate::Dimension::Vw(v) => format!("{}vw", css_number(v)),
        crate::Dimension::Vh(v) => format!("{}vh", css_number(v)),
        crate::Dimension::Vmin(v) => format!("{}vmin", css_number(v)),
        crate::Dimension::Vmax(v) => format!("{}vmax", css_number(v)),
    }
}

fn radius_value_css(value: crate::RadiusValue) -> String {
    match (value.length, value.percentage) {
        (length, 0.0) => css_px(length),
        (0.0, percentage) => format!("{}%", css_number(percentage * 100.0)),
        (length, percentage) => format!(
            "calc({} + {}%)",
            css_px(length),
            css_number(percentage * 100.0)
        ),
    }
}

fn corner_radius_css(radius: crate::CornerRadius) -> String {
    let x = radius_value_css(radius.x);
    let y = radius_value_css(radius.y);
    if x == y {
        x
    } else {
        format!("{x} {y}")
    }
}

fn css_color([r, g, b, a]: [u8; 4]) -> String {
    if a == 255 {
        format!("rgb({r}, {g}, {b})")
    } else {
        format!("rgba({r}, {g}, {b}, {})", css_number(a as f32 / 255.0))
    }
}

fn align_items_css(value: taffy::AlignItems) -> String {
    let keyword = match value.keyword {
        taffy::AlignItemsKeyword::Normal => "normal",
        taffy::AlignItemsKeyword::Start => "start",
        taffy::AlignItemsKeyword::End => "end",
        taffy::AlignItemsKeyword::FlexStart => "flex-start",
        taffy::AlignItemsKeyword::FlexEnd => "flex-end",
        taffy::AlignItemsKeyword::Center => "center",
        taffy::AlignItemsKeyword::Baseline => "baseline",
        taffy::AlignItemsKeyword::Stretch => "stretch",
    };
    if value.safety == taffy::AlignmentSafety::Safe {
        format!("safe {keyword}")
    } else {
        keyword.to_string()
    }
}

fn align_content_css(value: taffy::AlignContent) -> String {
    let keyword = match value.keyword {
        taffy::AlignContentKeyword::Start => "start",
        taffy::AlignContentKeyword::End => "end",
        taffy::AlignContentKeyword::FlexStart => "flex-start",
        taffy::AlignContentKeyword::FlexEnd => "flex-end",
        taffy::AlignContentKeyword::Center => "center",
        taffy::AlignContentKeyword::Stretch => "stretch",
        taffy::AlignContentKeyword::SpaceBetween => "space-between",
        taffy::AlignContentKeyword::SpaceEvenly => "space-evenly",
        taffy::AlignContentKeyword::SpaceAround => "space-around",
    };
    if value.safety == taffy::AlignmentSafety::Safe {
        format!("safe {keyword}")
    } else {
        keyword.to_string()
    }
}

fn transform_css(
    style: &crate::LayoutStyle,
    rect: Option<&crate::Rect>,
    root_font_size: f32,
    viewport: (f32, f32),
) -> String {
    if style.transform_ops.is_empty() {
        return "none".to_string();
    }
    let fallback = crate::Rect {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };
    let matrix = crate::dom::resolved_transform_property_matrix(
        style,
        rect.unwrap_or(&fallback),
        root_font_size,
        viewport,
    );
    format!(
        "matrix({}, {}, {}, {}, {}, {})",
        css_number(matrix.a),
        css_number(matrix.b),
        css_number(matrix.c),
        css_number(matrix.d),
        css_number(matrix.e),
        css_number(matrix.f),
    )
}

fn transform_origin_css(style: &crate::LayoutStyle, rect: Option<&crate::Rect>) -> String {
    let (x, y) = style.transform_origin.unwrap_or((
        crate::Dimension::Percent(0.5),
        crate::Dimension::Percent(0.5),
    ));
    let resolve = |value: crate::Dimension, axis: f32| match value {
        crate::Dimension::Percent(fraction) => css_px(fraction * axis),
        other => dimension_css(other, "0px"),
    };
    let (width, height) = rect.map_or((0.0, 0.0), |rect| (rect.width, rect.height));
    format!("{} {}", resolve(x, width), resolve(y, height))
}

/// Render `tree` at `viewport` (width, height) in CSS pixels to a Pixmap, or
/// None if the viewport is zero-sized. `base_url`, when given, resolves the
/// relative image URLs (`<img src="logo.svg">`) that make up the overwhelming
/// majority of real-world markup; without it only absolute and `data:` URLs
/// can be fetched.
pub fn paint_dom(tree: &DomTree, viewport: (f32, f32), base_url: Option<&str>) -> Option<Pixmap> {
    paint_dom_scrolled(tree, viewport, base_url, (0.0, 0.0))
}

/// Render the visible viewport after root scrolling. Normal document content
/// is translated by the clamped scroll offset while viewport-fixed subtrees
/// remain anchored to the initial containing block.
pub fn paint_dom_scrolled(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    scroll: (f32, f32),
) -> Option<Pixmap> {
    paint_dom_scrolled_at_animation_time(
        tree,
        viewport,
        base_url,
        scroll,
        crate::AnimationSampleTime::default(),
    )
}

pub fn paint_dom_scrolled_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    scroll: (f32, f32),
    animation_sample_time: crate::AnimationSampleTime,
) -> Option<Pixmap> {
    paint_dom_scrolled_at_animation_time_with_surface_color(
        tree,
        viewport,
        base_url,
        scroll,
        animation_sample_time,
        [255, 255, 255, 255],
    )
}

pub fn paint_dom_scrolled_at_animation_time_with_surface_color(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    scroll: (f32, f32),
    animation_sample_time: crate::AnimationSampleTime,
    surface_color: [u8; 4],
) -> Option<Pixmap> {
    let mut resources = RenderResourceCache::default();
    let mut prepared = prepare_dom_at_animation_time(
        tree,
        viewport,
        base_url,
        &mut resources,
        animation_sample_time,
    )?;
    paint_prepared_with_surface_color(tree, &mut prepared, &mut resources, scroll, surface_color)
}

/// Resolve image candidates and web fonts, then create the single final layout
/// shared by CSS geometry consumers and repeated paint.
pub fn prepare_dom(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
) -> Option<PreparedRender> {
    prepare_dom_at_animation_time(
        tree,
        viewport,
        base_url,
        resources,
        crate::AnimationSampleTime::default(),
    )
}

pub fn prepare_dom_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    animation_sample_time: crate::AnimationSampleTime,
) -> Option<PreparedRender> {
    prepare_dom_with_dynamic_fonts_at_animation_time(
        tree,
        viewport,
        base_url,
        resources,
        &[],
        animation_sample_time,
    )
}

/// Prepare a DOM with the document's script-created `FontFace` registrations.
///
/// Authored rules and dynamic faces deliberately share the same bounded
/// resource cache, decoder, descriptor matching, and shaping database.
pub fn prepare_dom_with_dynamic_fonts(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
) -> Option<PreparedRender> {
    prepare_dom_with_dynamic_fonts_at_animation_time(
        tree,
        viewport,
        base_url,
        resources,
        dynamic_fonts,
        crate::AnimationSampleTime::default(),
    )
}

pub fn prepare_dom_with_dynamic_fonts_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
    animation_sample_time: crate::AnimationSampleTime,
) -> Option<PreparedRender> {
    let mut stylesheet_cache = crate::css::StylesheetCache::default();
    prepare_dom_with_dynamic_fonts_and_stylesheet_cache_at_animation_time(
        tree,
        viewport,
        base_url,
        resources,
        dynamic_fonts,
        &mut stylesheet_cache,
        animation_sample_time,
    )
}

/// Prepare a DOM while retaining source parsing and selector indexing across
/// relayouts of the same document. Cascade, computed styles, and layout are
/// always rebuilt from the live DOM.
pub fn prepare_dom_with_dynamic_fonts_and_stylesheet_cache(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
    stylesheet_cache: &mut crate::css::StylesheetCache,
) -> Option<PreparedRender> {
    prepare_dom_with_dynamic_fonts_and_stylesheet_cache_at_animation_time(
        tree,
        viewport,
        base_url,
        resources,
        dynamic_fonts,
        stylesheet_cache,
        crate::AnimationSampleTime::default(),
    )
}

pub fn prepare_dom_with_dynamic_fonts_and_stylesheet_cache_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    animation_sample_time: crate::AnimationSampleTime,
) -> Option<PreparedRender> {
    let mut animation_timeline = crate::AnimationTimelineState::default();
    prepare_dom_with_dynamic_fonts_and_stylesheet_cache_with_animation_state(
        tree,
        viewport,
        base_url,
        resources,
        dynamic_fonts,
        stylesheet_cache,
        crate::AnimationSample {
            time: animation_sample_time,
            mode: crate::AnimationSampleMode::DocumentTime,
        },
        &mut animation_timeline,
    )
}

pub fn prepare_dom_with_dynamic_fonts_and_stylesheet_cache_with_animation_state(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> Option<PreparedRender> {
    prepare_dom_with_dynamic_fonts_and_stylesheet_cache_for_media_with_animation_state(
        tree,
        viewport,
        base_url,
        resources,
        dynamic_fonts,
        stylesheet_cache,
        crate::CssMediaType::Screen,
        animation_sample,
        animation_timeline,
    )
}

pub fn prepare_dom_with_dynamic_fonts_and_stylesheet_cache_for_media_with_animation_state(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    media_type: crate::CssMediaType,
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> Option<PreparedRender> {
    prepare_dom_with_dynamic_fonts_and_stylesheet_cache_internal(
        tree,
        viewport,
        base_url,
        resources,
        dynamic_fonts,
        stylesheet_cache,
        None,
        media_type,
        animation_sample,
        animation_timeline,
    )
}

/// Rebuild geometry while moving clean computed styles out of the previous
/// prepared render. The renderer validates every mutation against the cached
/// stylesheet's dependency map and automatically takes the full cascade path
/// whenever the retained subset is not provably sound.
pub fn prepare_dom_with_retained_attribute_styles(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    previous: PreparedRender,
    mutations: &[crate::dom::AttributeStyleMutation],
) -> Option<PreparedRender> {
    let mutations = mutations
        .iter()
        .cloned()
        .map(crate::dom::RetainedStyleMutation::Attribute)
        .collect::<Vec<_>>();
    prepare_dom_with_retained_styles(
        tree,
        viewport,
        base_url,
        resources,
        dynamic_fonts,
        stylesheet_cache,
        previous,
        &mutations,
    )
}

/// Rebuild geometry while retaining clean styles across both selector-key
/// attribute changes and conservatively scoped tree/text changes.
pub fn prepare_dom_with_retained_styles(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    previous: PreparedRender,
    mutations: &[crate::dom::RetainedStyleMutation],
) -> Option<PreparedRender> {
    prepare_dom_with_retained_styles_at_animation_time(
        tree,
        viewport,
        base_url,
        resources,
        dynamic_fonts,
        stylesheet_cache,
        previous,
        mutations,
        crate::AnimationSampleTime::default(),
    )
}

pub fn prepare_dom_with_retained_styles_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    previous: PreparedRender,
    mutations: &[crate::dom::RetainedStyleMutation],
    animation_sample_time: crate::AnimationSampleTime,
) -> Option<PreparedRender> {
    let mut animation_timeline = crate::AnimationTimelineState::default();
    prepare_dom_with_retained_styles_with_animation_state(
        tree,
        viewport,
        base_url,
        resources,
        dynamic_fonts,
        stylesheet_cache,
        previous,
        mutations,
        crate::AnimationSample {
            time: animation_sample_time,
            mode: crate::AnimationSampleMode::DocumentTime,
        },
        &mut animation_timeline,
    )
}

pub fn prepare_dom_with_retained_styles_with_animation_state(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    mut previous: PreparedRender,
    mutations: &[crate::dom::RetainedStyleMutation],
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> Option<PreparedRender> {
    let sample_changed = previous.animation_sample != animation_sample;
    let forward_document_sample = sample_changed
        && previous.animation_sample.mode == crate::AnimationSampleMode::DocumentTime
        && animation_sample.mode == crate::AnimationSampleMode::DocumentTime
        && animation_sample.time.milliseconds >= previous.animation_sample.time.milliseconds;
    if sample_changed && !forward_document_sample {
        drop(previous);
        return prepare_dom_with_dynamic_fonts_and_stylesheet_cache_with_animation_state(
            tree,
            viewport,
            base_url,
            resources,
            dynamic_fonts,
            stylesheet_cache,
            animation_sample,
            animation_timeline,
        );
    }
    if forward_document_sample
        && previous.viewport == viewport
        && previous.base_url.as_deref() == base_url
        && !previous.has_dynamic_fonts
        && dynamic_fonts.is_empty()
        && mutations.is_empty()
        && previous.try_advance_visual_waapi_sample(tree, animation_sample, animation_timeline)
    {
        return Some(previous);
    }
    let sampled_animation_mutations = sample_changed
        .then(|| {
            retained_animation_restyle_mutations(
                tree,
                &previous.layout.styles,
                animation_timeline,
            )
        })
        .unwrap_or_default();
    let animation_mutations;
    let mutations = if sampled_animation_mutations.is_empty() {
        mutations
    } else {
        animation_mutations = mutations
            .iter()
            .cloned()
            .chain(sampled_animation_mutations)
            .collect::<Vec<_>>();
        animation_mutations.as_slice()
    };
    let retained = RetainedStyleMaps {
        styles: std::mem::take(&mut previous.layout.styles),
        custom_properties: std::mem::take(&mut previous.layout.custom_properties),
    };
    drop(previous);
    prepare_dom_with_dynamic_fonts_and_stylesheet_cache_internal(
        tree,
        viewport,
        base_url,
        resources,
        dynamic_fonts,
        stylesheet_cache,
        Some((retained, mutations)),
        crate::CssMediaType::Screen,
        animation_sample,
        animation_timeline,
    )
}

fn retained_animation_restyle_mutations(
    tree: &DomTree,
    styles: &HashMap<obscura_dom::tree::NodeId, crate::LayoutStyle>,
    animation_timeline: &crate::AnimationTimelineState,
) -> Vec<crate::dom::RetainedStyleMutation> {
    let connected = |node: obscura_dom::tree::NodeId| {
        tree.get_node(node).is_some()
            && (node == tree.document() || tree.ancestors(node).contains(&tree.document()))
    };
    let mut css_nodes = styles
        .iter()
        .filter_map(|(node, style)| {
            (style.animation_name.is_some() && style.animation_has_render_effect)
                .then_some(*node)
        })
        .collect::<std::collections::HashSet<_>>();
    css_nodes.retain(|node| connected(*node));
    let mut mutations = css_nodes
        .into_iter()
        .map(|node| crate::dom::RetainedStyleMutation::Animation { node })
        .collect::<Vec<_>>();
    mutations.extend(
        animation_timeline
            .waapi_nodes()
            .into_iter()
            .filter(|node| connected(*node))
            .map(|node| crate::dom::RetainedStyleMutation::WaapiAnimation { node }),
    );
    mutations
}

fn prepare_dom_with_dynamic_fonts_and_stylesheet_cache_internal(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    resources: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    retained: Option<(RetainedStyleMaps, &[crate::dom::RetainedStyleMutation])>,
    media_type: crate::CssMediaType,
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> Option<PreparedRender> {
    if !viewport.0.is_finite() || !viewport.1.is_finite() || viewport.0 <= 0.0 || viewport.1 <= 0.0
    {
        return None;
    }
    // Fetch <img> bytes up front to learn intrinsic sizes for layout (a
    // CSS-sized image with no width/height attribute would otherwise be 0x0
    // and never paint). This seeds the same cache the paint pass reads, so
    // each URL is still fetched at most once.
    let (mut intrinsic, mut selected_images) =
        collect_image_intrinsics(tree, viewport, base_url, resources);
    // Preserve the HTML source fallback separately: a remembered CSS content
    // image temporarily overrides it, but a changed/removed/failed content
    // selection must restore the source before the correction layout.
    // Only remembered nodes can need their HTML fallback restored. Avoid
    // cloning the maps for every ordinary image on image-heavy pages.
    let remembered_content_nodes = resources
        .content_image_intrinsics
        .keys()
        .copied()
        .collect::<Vec<_>>();
    let source_intrinsic = remembered_content_nodes
        .iter()
        .filter_map(|nid| intrinsic.get(nid).copied().map(|value| (*nid, value)))
        .collect::<HashMap<_, _>>();
    let source_selected_images = remembered_content_nodes
        .iter()
        .filter_map(|nid| selected_images.get(nid).cloned().map(|value| (*nid, value)))
        .collect::<HashMap<_, _>>();
    let seeded_content_images =
        resources.seed_content_image_intrinsics(tree, &mut intrinsic, &mut selected_images);
    let fonts = collect_web_fonts(tree, base_url, resources, dynamic_fonts);
    // Most framework pages use web fonts and many decorative SVG icons, but
    // only SVG text needs the page font faces. Avoid cloning/loading the page
    // font database for ordinary icons and HTML-only text.
    let svg_fonts = if has_inline_svg_text(tree) {
        svg_font_database_with_web_fonts(&fonts)
    } else {
        svg_font_database()
    };
    let mut laid = match retained {
        Some((retained, mutations)) => layout_dom_with_web_fonts_and_retained_styles_with_animation_state(
            tree,
            viewport,
            &intrinsic,
            &fonts,
            stylesheet_cache,
            retained,
            mutations,
            animation_sample,
            animation_timeline,
        ),
        None => layout_dom_with_web_fonts_and_stylesheet_cache_for_media_with_animation_state(
            tree,
            viewport,
            &intrinsic,
            &fonts,
            stylesheet_cache,
            media_type,
            animation_sample,
            animation_timeline,
        ),
    };
    // `content:url(...)` is computed by the author cascade, whereas ordinary
    // HTML image sources are available before layout. Pay for a second layout
    // only on the uncommon pages that actually use a CSS image as replaced
    // content: its metadata then enters the same intrinsic-size map as `src`.
    if collect_content_image_intrinsics(
        tree,
        &laid.styles,
        base_url,
        resources,
        &mut intrinsic,
        &mut selected_images,
        &source_intrinsic,
        &source_selected_images,
        &seeded_content_images,
    ) {
        #[cfg(test)]
        {
            resources.content_image_layout_retries += 1;
        }
        laid = layout_dom_with_web_fonts_and_stylesheet_cache_for_media_with_animation_state(
            tree,
            viewport,
            &intrinsic,
            &fonts,
            stylesheet_cache,
            media_type,
            animation_sample,
            animation_timeline,
        );
    }
    let derived = laid.derived_layout_state(tree, viewport);
    let root_font_size = tree
        .query_selector("html")
        .ok()
        .flatten()
        .and_then(|root| laid.styles.get(&root))
        .and_then(|style| style.font_size)
        .unwrap_or(16.0);
    Some(PreparedRender {
        viewport,
        animation_sample,
        has_active_waapi_animations: animation_timeline.has_active_waapi(animation_sample.time),
        active_animation_impact: laid
            .styles
            .values()
            .filter(|style| css_animation_is_active(style))
            .map(|style| style.animation_effect_impact)
            .chain(std::iter::once(
                animation_timeline.active_waapi_effect_impact(animation_sample.time),
            ))
            .max()
            .unwrap_or_default(),
        root_font_size,
        base_url: base_url.map(str::to_string),
        has_dynamic_fonts: !dynamic_fonts.is_empty(),
        content_size: derived.content_size,
        viewport_fixed: derived.viewport_fixed,
        sticky: derived.sticky,
        scroll_tree: derived.scroll_tree,
        selected_images,
        svg_fonts,
        layout: laid,
    })
}

fn css_animation_is_active(style: &crate::LayoutStyle) -> bool {
    if style.animation_name.is_none()
        || !style.animation_has_render_effect
        || style.animation_timing.play_state == crate::AnimationPlayState::Paused
        || style.animation_timing.duration_ms <= 0.0
        || style.animation_timing.iteration_count <= 0.0
    {
        return false;
    }
    let end = style.animation_timing.delay_ms
        + style.animation_timing.duration_ms * style.animation_timing.iteration_count;
    end.is_infinite() || style.animation_local_time_ms < end.max(0.0)
}

/// Paint one root-scroll position from an already prepared resource-aware
/// layout. Resource bytes and glyph caches are reused across calls.
pub fn paint_prepared(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: (f32, f32),
) -> Option<Pixmap> {
    paint_prepared_with_surface_color(tree, prepared, resources, scroll, [255, 255, 255, 255])
}

fn paint_prepared_with_surface_color(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: (f32, f32),
    surface_color: [u8; 4],
) -> Option<Pixmap> {
    validate_capture_region(CaptureRegion::new(
        scroll.0,
        scroll.1,
        prepared.viewport.0,
        prepared.viewport.1,
        1.0,
    ))
    .ok()?;
    let (w, h) = (prepared.viewport.0 as u32, prepared.viewport.1 as u32);
    let mut pixmap = Pixmap::new(w, h)?;
    pixmap.fill(Color::from_rgba8(
        surface_color[0],
        surface_color[1],
        surface_color[2],
        surface_color[3],
    ));
    let canvas_background = canvas_background_source(tree, &prepared.layout);
    paint_laid_dom_scrolled(
        tree,
        prepared.viewport,
        prepared.base_url.as_deref(),
        scroll,
        None,
        None,
        pixmap,
        resources,
        &prepared.selected_images,
        &EMPTY_CANVAS_SURFACES,
        &prepared.svg_fonts,
        prepared.content_size,
        &prepared.viewport_fixed,
        &prepared.sticky,
        &prepared.scroll_tree,
        &mut prepared.layout,
        None,
        None,
        None,
        None,
        None,
        None,
        (0.0, 0.0),
        1.0,
        false,
        canvas_background,
    )
}

/// Paint from the same resolved root + element scroll snapshot used by CSSOM
/// geometry. No layout or scroll-tree work is performed by this call.
pub fn paint_prepared_with_scroll(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
) -> Option<Pixmap> {
    paint_prepared_with_scroll_and_surface_color(
        tree,
        prepared,
        resources,
        scroll,
        [255, 255, 255, 255],
    )
}

pub fn paint_prepared_with_scroll_and_surface_color(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    surface_color: [u8; 4],
) -> Option<Pixmap> {
    paint_prepared_with_scroll_and_surface_color_and_canvas_surfaces(
        tree,
        prepared,
        resources,
        scroll,
        surface_color,
        &EMPTY_CANVAS_SURFACES,
    )
}

pub fn paint_prepared_with_scroll_and_surface_color_and_canvas_surfaces(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    surface_color: [u8; 4],
    canvas_surfaces: &dyn CanvasSurfaceSource,
) -> Option<Pixmap> {
    validate_capture_region(CaptureRegion::new(
        scroll.root_offset().0,
        scroll.root_offset().1,
        prepared.viewport.0,
        prepared.viewport.1,
        1.0,
    ))
    .ok()?;
    let (w, h) = (prepared.viewport.0 as u32, prepared.viewport.1 as u32);
    let mut pixmap = Pixmap::new(w, h)?;
    pixmap.fill(Color::from_rgba8(
        surface_color[0],
        surface_color[1],
        surface_color[2],
        surface_color[3],
    ));
    let canvas_background = canvas_background_source(tree, &prepared.layout);
    paint_laid_dom_scrolled(
        tree,
        prepared.viewport,
        prepared.base_url.as_deref(),
        scroll.root_offset(),
        Some(scroll),
        None,
        pixmap,
        resources,
        &prepared.selected_images,
        canvas_surfaces,
        &prepared.svg_fonts,
        prepared.content_size,
        &prepared.viewport_fixed,
        &prepared.sticky,
        &prepared.scroll_tree,
        &mut prepared.layout,
        None,
        None,
        None,
        None,
        None,
        None,
        (0.0, 0.0),
        1.0,
        false,
        canvas_background,
    )
}

/// Paint an arbitrary document-space rectangle from one retained layout and
/// resolved scroll snapshot. The live root viewport remains the containing
/// block for fixed and sticky descendants, while ordinary document content is
/// translated directly to the requested page-space origin.
pub fn paint_prepared_region_with_scroll(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    region: CaptureRegion,
) -> Result<Pixmap, CaptureError> {
    paint_prepared_region_with_scroll_and_surface_color(
        tree,
        prepared,
        resources,
        scroll,
        region,
        [255, 255, 255, 255],
    )
}

pub fn paint_prepared_region_with_scroll_and_surface_color(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    region: CaptureRegion,
    surface_color: [u8; 4],
) -> Result<Pixmap, CaptureError> {
    paint_prepared_region_with_scroll_and_surface_color_and_canvas_surfaces(
        tree,
        prepared,
        resources,
        scroll,
        region,
        surface_color,
        &EMPTY_CANVAS_SURFACES,
    )
}

pub fn paint_prepared_region_with_scroll_and_surface_color_and_canvas_surfaces(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    region: CaptureRegion,
    surface_color: [u8; 4],
    canvas_surfaces: &dyn CanvasSurfaceSource,
) -> Result<Pixmap, CaptureError> {
    paint_prepared_region_with_scroll_policy(
        tree,
        prepared,
        resources,
        scroll,
        region,
        false,
        surface_color,
        canvas_surfaces,
    )
}

fn paint_prepared_region_with_scroll_with_print_economy(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    region: CaptureRegion,
    canvas_surfaces: &dyn CanvasSurfaceSource,
) -> Result<Pixmap, CaptureError> {
    paint_prepared_region_with_scroll_policy(
        tree,
        prepared,
        resources,
        scroll,
        region,
        true,
        [255, 255, 255, 255],
        canvas_surfaces,
    )
}

fn paint_prepared_region_with_scroll_policy(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    region: CaptureRegion,
    print_economy: bool,
    surface_color: [u8; 4],
    canvas_surfaces: &dyn CanvasSurfaceSource,
) -> Result<Pixmap, CaptureError> {
    let (native_width, native_height, output_width, output_height) =
        checked_capture_dimensions(region)?;
    let scale_matches_output =
        (output_width as f64 - f64::from(region.width) * f64::from(region.scale)).abs() <= 1.0
            && (output_height as f64 - f64::from(region.height) * f64::from(region.scale)).abs()
                <= 1.0;
    let native_scaled = (output_width != native_width || output_height != native_height)
        && scale_matches_output
        && native_raster_scale_supported(tree, &prepared.layout);
    let (paint_width, paint_height, raster_scale) = if native_scaled {
        (output_width, output_height, region.scale)
    } else {
        (native_width, native_height, 1.0)
    };
    let mut pixmap =
        Pixmap::new(paint_width, paint_height).ok_or(CaptureError::AllocationLimitExceeded)?;
    pixmap.fill(Color::from_rgba8(
        surface_color[0],
        surface_color[1],
        surface_color[2],
        surface_color[3],
    ));

    // Resolved node movement already contains `-root_scroll` for ordinary
    // document content and zero root movement for fixed content. Adding the
    // live root offset back before subtracting the capture origin therefore
    // maps ordinary nodes to page space while leaving fixed/sticky nodes at
    // their live viewport's document-space position.
    let root = scroll.root_offset();
    let surface_offset = (root.0 - region.x, root.1 - region.y);
    let canvas_background = canvas_background_source(tree, &prepared.layout);
    let pixmap = paint_laid_dom_scrolled(
        tree,
        prepared.viewport,
        prepared.base_url.as_deref(),
        root,
        Some(scroll),
        None,
        pixmap,
        resources,
        &prepared.selected_images,
        canvas_surfaces,
        &prepared.svg_fonts,
        prepared.content_size,
        &prepared.viewport_fixed,
        &prepared.sticky,
        &prepared.scroll_tree,
        &mut prepared.layout,
        None,
        None,
        None,
        None,
        None,
        Some((region.width, region.height)),
        surface_offset,
        raster_scale,
        print_economy,
        canvas_background,
    )
    .ok_or(CaptureError::PaintFailed)?;

    if native_scaled || (output_width == native_width && output_height == native_height) {
        return Ok(pixmap);
    }

    // The retained painter currently rasterizes glyphs and decoded images in
    // CSS-pixel space. Keep scale out of layout and scroll state, then use one
    // premultiplied-alpha high-quality surface transform. Vector primitives
    // still take the direct raster path whenever scale is 1.
    let source = image::RgbaImage::from_raw(native_width, native_height, pixmap.take())
        .ok_or(CaptureError::PaintFailed)?;
    let scaled = image::imageops::resize(
        &source,
        output_width,
        output_height,
        image::imageops::FilterType::Lanczos3,
    );
    let size = tiny_skia::IntSize::from_wh(output_width, output_height)
        .ok_or(CaptureError::AllocationLimitExceeded)?;
    Pixmap::from_vec(scaled.into_raw(), size).ok_or(CaptureError::PaintFailed)
}

/// Whether a retained display list can currently be rasterized directly at a
/// non-1 device scale. The native path deliberately starts with the ubiquitous
/// solid-box/text subset. Effects whose implementation creates logical-pixel
/// intermediate surfaces retain the proven CSS-raster + resample path until
/// those layers can carry an explicit source/device transform.
fn native_raster_scale_supported(tree: &DomTree, laid: &crate::DomLayout) -> bool {
    fn direct_gradient_geometry_supported(style: &crate::LayoutStyle) -> bool {
        if style.background_size.is_some()
            || style.background_size_expression.is_some()
            || style.background_size_fit.is_some()
        {
            return false;
        }

        // The direct vector painter is scale-safe only when no logical-pixel
        // tile surface is needed. Equal positioning and clipping boxes make
        // the implicit gradient tile exactly the painted path; position and
        // repeat then have no leftover space on which to operate.
        let geometry = background_geometry(
            &crate::Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            style,
        );
        (geometry.origin_rect.x - geometry.clip_rect.x).abs() <= 0.01
            && (geometry.origin_rect.y - geometry.clip_rect.y).abs() <= 0.01
            && (geometry.origin_rect.width - geometry.clip_rect.width).abs() <= 0.01
            && (geometry.origin_rect.height - geometry.clip_rect.height).abs() <= 0.01
    }

    fn style_supported(style: &crate::LayoutStyle) -> bool {
        let has_gradient = style.background_gradient.is_some()
            || style.background_radial_gradient.is_some()
            || !style.background_gradient_layers.is_empty();
        let gradients_supported = style.background_conic_gradient.is_none()
            && style
                .background_gradient_layers
                .iter()
                .all(|layer| match layer {
                    crate::BackgroundGradientLayer::Linear { repeating, .. } => !repeating,
                    crate::BackgroundGradientLayer::Radial { .. } => true,
                    crate::BackgroundGradientLayer::Conic { .. } => false,
                })
            && (!has_gradient || direct_gradient_geometry_supported(style));
        let simple = !has_authored_transform(style)
            && style.opacity.is_none_or(|opacity| opacity >= 1.0)
            && style.clip_path.is_none()
            && style.background_image.is_none()
            && style.mask_image.is_none()
            && gradients_supported
            && !style.background_clip_text
            && style.box_shadow.is_none()
            && style.content_image.is_none()
            && style.text_overflow == crate::TextOverflow::Clip;
        simple
            && style.before_pseudo.as_deref().is_none_or(style_supported)
            && style.after_pseudo.as_deref().is_none_or(style_supported)
    }

    if laid.clip_rects.values().any(|clip| clip.is_some())
        || laid.styles.values().any(|style| !style_supported(style))
    {
        return false;
    }
    !tree.descendants(tree.document()).into_iter().any(|id| {
        tree.get_node(id).is_some_and(|node| {
            node.as_element().is_some_and(|name| {
                matches!(
                    name.local.as_ref(),
                    "img" | "picture" | "svg" | "canvas" | "video"
                )
            })
        })
    })
}

/// The used `z-index` for an element which participates in stacking order.
///
/// A non-auto z-index applies not only to positioned boxes, but also to
/// otherwise-static flex and grid items. `display:contents` boxes are skipped
/// while finding the item's formatting-context parent because they generate no
/// box of their own.
fn stacking_z_index(
    tree: &DomTree,
    laid: &crate::DomLayout,
    id: obscura_dom::tree::NodeId,
) -> Option<i32> {
    let style = laid.styles.get(&id)?;
    if style.display == crate::Display::None || style.display_contents {
        return None;
    }
    // Fixed and sticky positioned boxes establish stacking contexts even
    // when their used z-index is `auto`. Treat that context as stack level
    // zero so a high-z descendant cannot escape above a later sibling layer.
    if style.position_fixed || style.position_sticky {
        return Some(style.z_index.unwrap_or(0));
    }
    let z = style.z_index?;
    if style.position.is_some() {
        return Some(z);
    }

    let mut parent = crate::dom::rendered_parent(tree, id);
    while let Some(parent_id) = parent {
        let Some(parent_style) = laid.styles.get(&parent_id) else {
            parent = crate::dom::rendered_parent(tree, parent_id);
            continue;
        };
        if parent_style.display_contents {
            parent = crate::dom::rendered_parent(tree, parent_id);
            continue;
        }
        let is_flex_or_grid = parent_style.display == crate::Display::Grid
            || (parent_style.display == crate::Display::Flex
                && !parent_style.internal_flex_container);
        return is_flex_or_grid.then_some(z);
    }
    None
}

/// Whether this box participates in the float painting band of its current
/// stacking context. A retained `float` declaration has no effect on an
/// absolutely positioned box or on a flex/grid item, matching layout's
/// blockification rules.
fn is_effective_float(
    tree: &DomTree,
    laid: &crate::DomLayout,
    id: obscura_dom::tree::NodeId,
) -> bool {
    let Some(style) = laid.styles.get(&id) else {
        return false;
    };
    if style.float.is_none()
        || style.display == crate::Display::None
        || style.display_contents
        || matches!(style.position, Some(taffy::Position::Absolute))
    {
        return false;
    }

    let mut parent = crate::dom::rendered_parent(tree, id);
    while let Some(parent_id) = parent {
        let Some(parent_style) = laid.styles.get(&parent_id) else {
            parent = crate::dom::rendered_parent(tree, parent_id);
            continue;
        };
        if parent_style.display_contents {
            parent = crate::dom::rendered_parent(tree, parent_id);
            continue;
        }
        return parent_style.display != crate::Display::Grid
            && !(parent_style.display == crate::Display::Flex
                && !parent_style.internal_flex_container);
    }
    true
}

fn has_authored_transform(style: &crate::LayoutStyle) -> bool {
    !style.transform_ops.is_empty()
        || style.individual_translate.is_some()
        || style.individual_rotate.is_some()
        || style.individual_scale.is_some()
}

/// Untransformed source bounds for one atomic transform layer, expressed in
/// the parent surface's coordinates. Keeping this tight both preserves source
/// pixels that begin outside the viewport and avoids a viewport-sized surface
/// for every transformed icon or badge.
fn transform_subtree_source_bounds(
    tree: &DomTree,
    laid: &crate::DomLayout,
    scroll_state: &ScrollPaintState<'_>,
    root: obscura_dom::tree::NodeId,
) -> Option<crate::Rect> {
    let mut bounds: Option<crate::Rect> = None;
    for id in std::iter::once(root).chain(crate::dom::rendered_descendants(tree, root)) {
        let Some(rect) = laid.rects.get(&id) else {
            continue;
        };
        let (x, y) = scroll_state.translation_for(laid, id);
        let mut visual = crate::Rect {
            x: rect.x + x,
            y: rect.y + y,
            width: rect.width,
            height: rect.height,
        };
        if let Some(style) = laid.styles.get(&id) {
            // The atomic source surface must retain every primitive that can
            // extend beyond the border box. In particular, a thick outline
            // is transformed with the element and cannot be reconstructed
            // after the tight source layer has already clipped it.
            visual = non_text_ink_bounds(&visual, style);
        }
        bounds = Some(match bounds {
            Some(current) => current.union(&visual),
            None => visual,
        });
    }
    bounds.map(|bounds| crate::Rect {
        x: bounds.x - 2.0,
        y: bounds.y - 2.0,
        width: bounds.width + 4.0,
        height: bounds.height + 4.0,
    })
}

/// Paint an already prepared layout without changing its document-space
/// geometry. Root scrolling and sticky positioning are per-shot visual state,
/// so alternating captures can safely reuse the same layout.
#[derive(Clone, Copy)]
struct CanvasBackground {
    root: obscura_dom::tree::NodeId,
    source: obscura_dom::tree::NodeId,
}

fn style_has_canvas_background(style: &crate::LayoutStyle) -> bool {
    style.background_color.is_some_and(|color| color[3] != 0)
        || style.background_image.is_some()
        || style.background_gradient.is_some()
        || style.background_radial_gradient.is_some()
        || style.background_conic_gradient.is_some()
        || !style.background_gradient_layers.is_empty()
}

fn canvas_background_source(tree: &DomTree, laid: &crate::DomLayout) -> Option<CanvasBackground> {
    let root = tree.query_selector("html").ok().flatten()?;
    let root_style = laid.styles.get(&root)?;
    let root_is_contained = root_style.containing_block_triggers & crate::CB_TRIGGER_CONTAIN != 0;
    if root_is_contained || style_has_canvas_background(root_style) {
        return Some(CanvasBackground { root, source: root });
    }
    let body = tree.query_selector("body").ok().flatten();
    let source = body
        .filter(|body| {
            laid.styles.get(body).is_some_and(|style| {
                style.containing_block_triggers & crate::CB_TRIGGER_CONTAIN == 0
            })
        })
        .unwrap_or(root);
    Some(CanvasBackground { root, source })
}

fn paint_canvas_background(
    pixmap: &mut Pixmap,
    style: &crate::LayoutStyle,
    origin_rect: &crate::Rect,
    surface_rect: &crate::Rect,
    root_font_size: f32,
    viewport: (f32, f32),
    base_url: Option<&str>,
    image_cache: &mut RenderResourceCache,
    raster_scale: f32,
) {
    let Some(surface) = Rect::from_xywh(
        surface_rect.x,
        surface_rect.y,
        surface_rect.width,
        surface_rect.height,
    ) else {
        return;
    };
    let mut builder = PathBuilder::new();
    builder.push_rect(surface);
    let Some(path) = builder.finish() else {
        return;
    };

    if let Some(color) = style.background_color {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(
            color[0], color[1], color[2], color[3],
        ));
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            raster_transform(raster_scale),
            None,
        );
    }

    if !style.background_gradient_layers.is_empty() {
        paint_background_gradient_layers(
            pixmap,
            &path,
            origin_rect,
            surface_rect,
            crate::ResolvedBorderRadii::default(),
            style,
            root_font_size,
            viewport,
            None,
            raster_scale,
        );
    } else {
        if let Some((center, stops)) = &style.background_radial_gradient {
            paint_radial_gradient(
                pixmap,
                &path,
                origin_rect,
                *center,
                stops,
                style.background_radial_gradient_geometry,
                style.font_size.unwrap_or(16.0),
                root_font_size,
                viewport,
                None,
                raster_scale,
            );
        }
        if let Some((angle, center, stops)) = &style.background_conic_gradient {
            paint_conic_gradient_sampled(
                pixmap,
                surface_rect,
                origin_rect,
                crate::ResolvedBorderRadii::default(),
                *angle,
                *center,
                stops,
                None,
            );
        }
        if let Some((angle, stops)) = &style.background_gradient {
            paint_linear_gradient(
                pixmap,
                &path,
                origin_rect,
                *angle,
                stops,
                None,
                raster_scale,
            );
        }
    }

    if let Some(url) = &style.background_image {
        if let Some(image_rect) = background_image_rect(
            url,
            base_url,
            origin_rect,
            style.background_size,
            style.background_size_expression.as_deref(),
            style.background_size_fit,
            style.background_position,
            style.font_size.unwrap_or(16.0),
            root_font_size,
            viewport,
            image_cache,
        ) {
            paint_image(
                url,
                base_url,
                &image_rect,
                surface_rect,
                crate::ObjectFit::Fill,
                crate::ObjectPosition::default(),
                pixmap,
                image_cache,
                None,
                None,
                crate::ResolvedBorderRadii::default(),
                None,
            );
        }
    }
}

fn paint_laid_dom_scrolled(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    scroll: (f32, f32),
    resolved_scroll: Option<&ResolvedScrollState>,
    shared_scroll_state: Option<&ScrollPaintState<'_>>,
    mut pixmap: Pixmap,
    image_cache: &mut RenderResourceCache,
    selected_images: &HashMap<obscura_dom::tree::NodeId, SelectedImage>,
    canvas_surfaces: &dyn CanvasSurfaceSource,
    svg_fonts: &Arc<usvg::fontdb::Database>,
    content_size: (f32, f32),
    viewport_fixed: &std::collections::HashSet<obscura_dom::tree::NodeId>,
    sticky: &crate::StickyLayout,
    scroll_tree: &crate::dom::ScrollTree,
    laid: &mut crate::DomLayout,
    paint_root: Option<obscura_dom::tree::NodeId>,
    suppress_opacity_for: Option<obscura_dom::tree::NodeId>,
    suppress_stacking_for: Option<obscura_dom::tree::NodeId>,
    suppress_transform_for: Option<obscura_dom::tree::NodeId>,
    clip_scope_root: Option<obscura_dom::tree::NodeId>,
    surface_extent: Option<(f32, f32)>,
    surface_offset: (f32, f32),
    raster_scale: f32,
    print_economy: bool,
    canvas_background: Option<CanvasBackground>,
) -> Option<Pixmap> {
    let scroll_state = match resolved_scroll {
        Some(resolved) => ScrollPaintState::from_resolved(
            tree,
            viewport,
            viewport_fixed,
            resolved,
            clip_scope_root,
            surface_extent,
            surface_offset,
        ),
        None => ScrollPaintState::new(
            tree,
            viewport,
            scroll,
            content_size,
            viewport_fixed,
            sticky,
            scroll_tree,
            laid,
            shared_scroll_state,
            clip_scope_root,
            surface_extent,
            surface_offset,
        ),
    };
    let root_font_size = tree
        .query_selector("html")
        .ok()
        .flatten()
        .and_then(|root| laid.styles.get(&root))
        .and_then(|style| style.font_size)
        .unwrap_or(16.0);
    if paint_root.is_none() {
        if let Some(canvas) = canvas_background {
            let root_visible = laid
                .styles
                .get(&canvas.root)
                .is_some_and(|style| !style.effectively_invisible);
            if root_visible {
                if let (Some(style), Some(source_rect)) = (
                    laid.styles.get(&canvas.source),
                    laid.rects.get(&canvas.source).copied(),
                ) {
                    let (x, y) = scroll_state.translation_for(laid, canvas.source);
                    let origin_rect = crate::Rect {
                        x: source_rect.x + x,
                        y: source_rect.y + y,
                        width: source_rect.width,
                        height: source_rect.height,
                    };
                    let surface_rect = paint_surface_rect(&pixmap, raster_scale);
                    paint_canvas_background(
                        &mut pixmap,
                        style,
                        &origin_rect,
                        &surface_rect,
                        root_font_size,
                        viewport,
                        base_url,
                        image_cache,
                        raster_scale,
                    );
                }
            }
        }
    }
    // Nodes that live inside an inline `<svg>` we rasterized as one document;
    // their painting is owned by that raster, so they are skipped in both the
    // box/text loop below and the inline-formatting loop after it (an svg
    // `<text>` element must not also paint its glyphs on top of the raster).
    let mut svg_subtree_skip: std::collections::HashSet<obscura_dom::tree::NodeId> =
        std::collections::HashSet::new();
    // Text is painted in a second pass, after the box-order loop. Remember
    // every subtree already painted into an opacity layer so its shaped text
    // is not drawn a second time at full opacity in the outer pass.
    let mut opacity_subtree_skip: std::collections::HashSet<obscura_dom::tree::NodeId> =
        std::collections::HashSet::new();
    // External sprite symbols, keyed by "url#id", extracted from a fetched
    // sprite file so a `<use href="url#id">` resolves. One sprite backs many
    // icons (a whole logo/icon band), so cache the parsed symbol across every
    // inline svg on the page rather than re-parsing the sprite per icon.
    let mut sprite_cache: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    // An overflow clip is inherited by every descendant until another clip
    // joins the chain. Rasterizing the same full-surface alpha mask per child
    // made clipped long pages scale as surface area times descendant count.
    // Cache immutable masks for this one paint surface and share them by Arc.
    let mut overflow_mask_cache: OverflowClipMaskCache = HashMap::new();
    // Whether any element carries a `transform: translate()`. When none does
    // (the overwhelmingly common case), every node's accumulated offset is
    // zero, so skip the per-node ancestor walk entirely and keep the paint
    // path free of any added cost.

    // Paint order follows CSS2's stacking bands: in-flow block backgrounds
    // and borders first, then non-positioned floats, then inline/text content
    // (painted by the later text pass). A float is an atomic paint unit in
    // this band, so its background, replaced content, and descendants cannot
    // be covered by a later full-width normal block background.
    //
    // A positioned element or a flex/grid item
    // with a non-auto z-index lifts its whole subtree into an atomic stacking
    // unit: negative layers paint under the normal flow, non-negative ones
    // above it, each sorted by z-index ascending (stable, so equal z keeps
    // tree order). The unit is recursively painted at its sorted position,
    // preventing its backgrounds, replaced content, and shaped text from
    // leaking into different global paint phases.
    let mut neg_layers: Vec<(i32, Vec<obscura_dom::tree::NodeId>)> = Vec::new();
    let mut pos_layers: Vec<(i32, Vec<obscura_dom::tree::NodeId>)> = Vec::new();
    let mut float_layers: Vec<obscura_dom::tree::NodeId> = Vec::new();
    let mut normal: Vec<obscura_dom::tree::NodeId> = Vec::new();
    let mut consumed: std::collections::HashSet<obscura_dom::tree::NodeId> =
        std::collections::HashSet::new();
    let mut paint_nodes = paint_root.into_iter().collect::<Vec<_>>();
    paint_nodes.extend(crate::dom::rendered_descendants(
        tree,
        paint_root.unwrap_or_else(|| tree.document()),
    ));
    for nid in paint_nodes.iter().copied() {
        if consumed.contains(&nid) {
            continue;
        }
        let is_opacity_root = suppress_opacity_for != Some(nid)
            && laid
                .styles
                .get(&nid)
                .and_then(|style| style.opacity)
                .is_some_and(|opacity| opacity.clamp(0.0, 1.0) < 1.0);
        let is_transform_root = suppress_transform_for != Some(nid)
            && laid.styles.get(&nid).is_some_and(has_authored_transform);
        let z = (suppress_stacking_for != Some(nid))
            .then(|| stacking_z_index(tree, laid, nid))
            .flatten();
        let is_float_root = paint_root != Some(nid) && is_effective_float(tree, laid, nid);
        if is_opacity_root || is_transform_root {
            let mut sub = vec![nid];
            sub.extend(crate::dom::rendered_descendants(tree, nid));
            for &member in &sub {
                consumed.insert(member);
            }
            // An opacity effect is one atomic paint-order unit. Its internal
            // z-order is resolved while painting its isolated surface.
            match z {
                Some(z) if z < 0 => neg_layers.push((z, vec![nid])),
                Some(z) => pos_layers.push((z, vec![nid])),
                None if is_float_root => float_layers.push(nid),
                None => normal.push(nid),
            }
        } else if let Some(z) = z {
            let mut sub = vec![nid];
            sub.extend(crate::dom::rendered_descendants(tree, nid));
            for &m in &sub {
                consumed.insert(m);
            }
            if z < 0 {
                neg_layers.push((z, vec![nid]));
            } else {
                pos_layers.push((z, vec![nid]));
            }
        } else if is_float_root {
            consumed.insert(nid);
            consumed.extend(crate::dom::rendered_descendants(tree, nid));
            float_layers.push(nid);
        } else {
            normal.push(nid);
        }
    }
    neg_layers.sort_by_key(|(z, _)| *z);
    pos_layers.sort_by_key(|(z, _)| *z);
    let paint_order: Vec<obscura_dom::tree::NodeId> = neg_layers
        .into_iter()
        .flat_map(|(_, sub)| sub)
        .chain(normal)
        .chain(float_layers)
        .chain(pos_layers.into_iter().flat_map(|(_, sub)| sub))
        .collect();

    // Generated boxes are anonymous layout children. ::before paints directly
    // after its host's own box; ::after paints after the host's last DOM
    // descendant in this paint order. Build the latter schedule only on pages
    // that actually materialized a generated box. A reverse DOM-preorder pass
    // propagates each node's paint index to its parent once, deriving subtree
    // endpoints in O(nodes + generated boxes), including reordered z layers.
    let mut generated_before: std::collections::HashMap<
        obscura_dom::tree::NodeId,
        Vec<crate::dom::GeneratedBox>,
    > = std::collections::HashMap::new();
    let mut generated_after_at: Vec<Vec<crate::dom::GeneratedBox>> =
        vec![Vec::new(); paint_order.len()];
    if !laid.generated_boxes.is_empty() {
        let paint_indices: std::collections::HashMap<obscura_dom::tree::NodeId, usize> =
            paint_order
                .iter()
                .enumerate()
                .map(|(index, &nid)| (nid, index))
                .collect();
        let mut last_index: std::collections::HashMap<obscura_dom::tree::NodeId, usize> =
            paint_indices.clone();
        let dom_preorder = paint_nodes.clone();
        for nid in dom_preorder.into_iter().rev() {
            let Some(index) = last_index.get(&nid).copied() else {
                continue;
            };
            if paint_root != Some(nid) {
                if let Some(parent) = crate::dom::rendered_parent(tree, nid) {
                    last_index
                        .entry(parent)
                        .and_modify(|last| *last = (*last).max(index))
                        .or_insert(index);
                }
            }
        }
        for generated in laid.generated_boxes.iter().copied() {
            match generated.kind {
                crate::dom::GeneratedBoxKind::Before => {
                    if paint_indices.contains_key(&generated.host) {
                        generated_before
                            .entry(generated.host)
                            .or_default()
                            .push(generated);
                    }
                }
                crate::dom::GeneratedBoxKind::After => {
                    if let Some(index) = last_index.get(&generated.host) {
                        generated_after_at[*index].push(generated);
                    }
                }
            }
        }
    }

    for (paint_index, nid) in paint_order.into_iter().enumerate() {
        if svg_subtree_skip.contains(&nid) {
            continue;
        }
        if suppress_stacking_for != Some(nid) && stacking_z_index(tree, laid, nid).is_some() {
            opacity_subtree_skip.insert(nid);
            opacity_subtree_skip.extend(crate::dom::rendered_descendants(tree, nid));
            // A stacking context is one structural paint item in its parent.
            // Re-enter the same display-list builder with this root suppressed
            // so all of its decorations, descendants, and shaped inline text
            // finish before the next sibling stacking unit starts. Passing the
            // existing pixmap avoids a viewport-sized intermediate surface;
            // opacity still takes the isolated-surface path below when needed.
            pixmap = paint_laid_dom_scrolled(
                tree,
                viewport,
                base_url,
                scroll,
                resolved_scroll,
                Some(&scroll_state),
                pixmap,
                image_cache,
                selected_images,
                canvas_surfaces,
                svg_fonts,
                content_size,
                viewport_fixed,
                sticky,
                scroll_tree,
                laid,
                Some(nid),
                suppress_opacity_for,
                Some(nid),
                suppress_transform_for,
                clip_scope_root,
                surface_extent,
                surface_offset,
                raster_scale,
                print_economy,
                canvas_background,
            )?;
            continue;
        }
        if paint_root != Some(nid) && is_effective_float(tree, laid, nid) {
            opacity_subtree_skip.insert(nid);
            opacity_subtree_skip.extend(crate::dom::rendered_descendants(tree, nid));
            // CSS paints each float as an atomic unit in the float band. The
            // recursive display-list build preserves the float's own stacking,
            // opacity, transforms, generated boxes, replaced content, and
            // inline text while keeping outer in-flow inline content above it.
            pixmap = paint_laid_dom_scrolled(
                tree,
                viewport,
                base_url,
                scroll,
                resolved_scroll,
                Some(&scroll_state),
                pixmap,
                image_cache,
                selected_images,
                canvas_surfaces,
                svg_fonts,
                content_size,
                viewport_fixed,
                sticky,
                scroll_tree,
                laid,
                Some(nid),
                suppress_opacity_for,
                suppress_stacking_for,
                suppress_transform_for,
                clip_scope_root,
                surface_extent,
                surface_offset,
                raster_scale,
                print_economy,
                canvas_background,
            )?;
            continue;
        }
        let node = match tree.get_node(nid) {
            Some(n) => n,
            None => continue,
        };

        if node.is_text() {
            if let Some(items) = laid.word_ifc_items.get(&nid) {
                let offset = scroll_state.translation_for(laid, nid);
                let overflow_clip = scroll_state.shaped_text_overflow_clip_for(laid, nid);
                let clip = overflow_clip.as_ref().map(|clip| {
                    clip.viewport_rect(scroll_state.surface_extent.unwrap_or(viewport))
                });
                let clip_mask = overflow_clip.as_ref().and_then(|clip| {
                    cached_overflow_clip_mask(
                        &mut overflow_mask_cache,
                        pixmap.width(),
                        pixmap.height(),
                        clip,
                        scroll_state.surface_extent.unwrap_or(viewport),
                    )
                });
                for &item in items {
                    laid.text_engine.paint_item_with_clip_mask_scaled_for_print(
                        item,
                        &mut pixmap,
                        offset,
                        clip,
                        clip_mask.as_deref(),
                        raster_scale,
                        print_economy,
                    );
                }
            } else {
                paint_text_node(tree, nid, laid, &scroll_state, &mut pixmap, raster_scale);
            }
            for generated in &generated_after_at[paint_index] {
                paint_in_flow_generated_box(
                    &mut pixmap,
                    generated,
                    laid,
                    &scroll_state,
                    viewport,
                    root_font_size,
                    base_url,
                    image_cache,
                    raster_scale,
                );
            }
            continue;
        }

        let name = match node.as_element() {
            Some(name) => name,
            None => continue,
        };
        let rect = match laid.rects.get(&nid) {
            Some(r) => *r,
            None => continue,
        };

        let style = match laid.styles.get(&nid) {
            Some(s) => s,
            None => continue,
        };

        if suppress_transform_for != Some(nid) && has_authored_transform(style) {
            opacity_subtree_skip.insert(nid);
            opacity_subtree_skip.extend(crate::dom::rendered_descendants(tree, nid));
            let transform =
                crate::dom::resolved_transform_matrix(style, &rect, root_font_size, viewport);
            if transform.is_translation() {
                // Translation-only transforms retain the direct offset path:
                // no surface allocation and no resampling for carousels and
                // centering utilities.
                pixmap = paint_laid_dom_scrolled(
                    tree,
                    viewport,
                    base_url,
                    scroll,
                    resolved_scroll,
                    Some(&scroll_state),
                    pixmap,
                    image_cache,
                    selected_images,
                    canvas_surfaces,
                    svg_fonts,
                    content_size,
                    viewport_fixed,
                    sticky,
                    scroll_tree,
                    laid,
                    Some(nid),
                    suppress_opacity_for,
                    suppress_stacking_for,
                    Some(nid),
                    clip_scope_root,
                    surface_extent,
                    surface_offset,
                    raster_scale,
                    print_economy,
                    canvas_background,
                )?;
                continue;
            }

            // A transform wraps the complete atomic child display list. Paint
            // that list in its untransformed coordinate space, with clips
            // established inside this subtree, then map the finished surface.
            // This makes text, borders, shadows, SVG, images, and pseudos share
            // one transform exactly like Gecko's nsDisplayTransform wrapper.
            let outside_overflow_clip = scroll_state.overflow_clip_for(laid, nid);
            let outside_clip = outside_overflow_clip
                .as_ref()
                .map(|clip| clip.viewport_rect(scroll_state.surface_extent.unwrap_or(viewport)));
            let movement = scroll_state.translation_for(laid, nid);
            let display_transform = crate::Affine2::translate(movement.0, movement.1)
                .then(transform)
                .then(crate::Affine2::translate(-movement.0, -movement.1));
            let target = crate::Rect {
                x: 0.0,
                y: 0.0,
                width: pixmap.width() as f32,
                height: pixmap.height() as f32,
            };
            let target = match outside_clip {
                Some(clip) => match target.intersect(&clip) {
                    Some(target) => target,
                    None => continue,
                },
                None => target,
            };
            let Some(inverse) = display_transform.inverse() else {
                // A singular transform has no two-dimensional painted area.
                continue;
            };
            let needed_source = inverse.map_rect(target);
            let Some(source_bounds) =
                transform_subtree_source_bounds(tree, laid, &scroll_state, nid)
                    .and_then(|bounds| bounds.intersect(&needed_source))
            else {
                continue;
            };
            let left = source_bounds.x.floor();
            let top = source_bounds.y.floor();
            let right = (source_bounds.x + source_bounds.width).ceil();
            let bottom = (source_bounds.y + source_bounds.height).ceil();
            let layer_width = (right - left).max(1.0) as u32;
            let layer_height = (bottom - top).max(1.0) as u32;
            let layer_delta = (-left, -top);
            let layer = Pixmap::new(layer_width, layer_height)?;
            let layer = paint_laid_dom_scrolled(
                tree,
                viewport,
                base_url,
                scroll,
                resolved_scroll,
                Some(&scroll_state),
                layer,
                image_cache,
                selected_images,
                canvas_surfaces,
                svg_fonts,
                content_size,
                viewport_fixed,
                sticky,
                scroll_tree,
                laid,
                Some(nid),
                suppress_opacity_for,
                suppress_stacking_for,
                Some(nid),
                Some(nid),
                surface_extent,
                (
                    surface_offset.0 + layer_delta.0,
                    surface_offset.1 + layer_delta.1,
                ),
                raster_scale,
                print_economy,
                canvas_background,
            )?;
            let transform =
                display_transform.then(crate::Affine2::translate(-layer_delta.0, -layer_delta.1));
            let transform = Transform::from_row(
                transform.a,
                transform.b,
                transform.c,
                transform.d,
                transform.e,
                transform.f,
            );
            let clip_mask = outside_overflow_clip.as_ref().and_then(|clip| {
                cached_overflow_clip_mask(
                    &mut overflow_mask_cache,
                    pixmap.width(),
                    pixmap.height(),
                    clip,
                    scroll_state.surface_extent.unwrap_or(viewport),
                )
            });
            pixmap.draw_pixmap(
                0,
                0,
                layer.as_ref(),
                &tiny_skia::PixmapPaint::default(),
                transform,
                clip_mask.as_deref(),
            );
            continue;
        }

        let own_opacity = laid
            .styles
            .get(&nid)
            .and_then(|style| style.opacity)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        if suppress_opacity_for != Some(nid) && own_opacity < 1.0 {
            opacity_subtree_skip.insert(nid);
            opacity_subtree_skip.extend(crate::dom::rendered_descendants(tree, nid));
            if own_opacity <= 0.0 {
                continue;
            }
            // Opacity is applied to the finished stacking context, never to
            // each primitive. Otherwise two opaque overlapping children at
            // opacity:.5 incorrectly become .75 alpha in their overlap.
            let layer = Pixmap::new(pixmap.width(), pixmap.height())?;
            let layer = paint_laid_dom_scrolled(
                tree,
                viewport,
                base_url,
                scroll,
                resolved_scroll,
                Some(&scroll_state),
                layer,
                image_cache,
                selected_images,
                canvas_surfaces,
                svg_fonts,
                content_size,
                viewport_fixed,
                sticky,
                scroll_tree,
                laid,
                Some(nid),
                Some(nid),
                suppress_stacking_for,
                suppress_transform_for,
                clip_scope_root,
                surface_extent,
                surface_offset,
                raster_scale,
                print_economy,
                canvas_background,
            )?;
            let group_paint = tiny_skia::PixmapPaint {
                opacity: own_opacity,
                ..tiny_skia::PixmapPaint::default()
            };
            pixmap.draw_pixmap(
                0,
                0,
                layer.as_ref(),
                &group_paint,
                Transform::identity(),
                None,
            );
            continue;
        }

        if style.effectively_invisible {
            continue;
        }
        let background_transfers_to_canvas = canvas_background
            .is_some_and(|canvas| nid == canvas.root || nid == canvas.source);

        // A `transform: translate()` on this element or any ancestor offsets
        // this element's whole painted box (and, applied per node, its whole
        // subtree). The box shifts into screen space. The inherited clip is
        // owner-shifted by layout and root-scroll/sticky-adjusted by the visual
        // state, but it must not move with this descendant: that is what lets a
        // clip cull a slide the carousel track translated out of its viewport.
        let (ox, oy) = scroll_state.translation_for(laid, nid);
        let rect = crate::Rect {
            x: rect.x + ox,
            y: rect.y + oy,
            width: rect.width,
            height: rect.height,
        };

        // Ancestor `overflow: hidden` clip, if any. Skip painting entirely
        // once the box has no visible overlap with it (this is what makes the
        // ubiquitous 1x1 clipped "visually hidden" accessibility pattern
        // actually invisible instead of painting text wherever it lands).
        let overflow_clip = scroll_state.overflow_clip_for(laid, nid);
        let clip = overflow_clip
            .as_ref()
            .map(|clip| clip.viewport_rect(scroll_state.surface_extent.unwrap_or(viewport)));
        let cull_rect = rect;
        let visible_rect = match clip {
            Some(c) => match cull_rect.intersect(&c) {
                Some(r) => r,
                None => continue,
            },
            None => cull_rect,
        };
        let box_rect = match Rect::from_xywh(
            visible_rect.x,
            visible_rect.y,
            visible_rect.width,
            visible_rect.height,
        ) {
            Some(rect) => rect,
            None => continue,
        };
        let surface = paint_surface_rect(&pixmap, raster_scale);
        let box_on_surface = visible_rect.intersect(&surface).is_some();
        let ink_bounds = non_text_ink_bounds(&rect, style);
        // Mask allocation follows the raw ink bounds, not the already-clipped
        // bounds. A distant box can cast a large offset shadow onto the paint
        // surface while its ancestor overflow clip remains offscreen. In that
        // case the primitive still needs the ancestor mask so it cannot leak
        // through the clip.
        let non_text_on_surface = ink_bounds.intersect(&surface).is_some();
        // A list marker or generated run may protrude modestly from the host
        // border box. Keep its clip masks available without letting a box
        // thousands of pixels away allocate full-surface masks.
        let text_guard = style.font_size.unwrap_or(16.0) * 4.0 + 4.0;
        let text_bounds = crate::Rect {
            x: rect.x - text_guard,
            y: rect.y - text_guard,
            width: rect.width + 2.0 * text_guard,
            height: rect.height + 2.0 * text_guard,
        };
        let text_on_surface = text_bounds.intersect(&surface).is_some();
        let needs_host_masks = non_text_on_surface || text_on_surface;
        let ancestor_clip_mask = needs_host_masks
            .then(|| {
                overflow_clip.as_ref().and_then(|clip| {
                    cached_overflow_clip_mask(
                        &mut overflow_mask_cache,
                        pixmap.width(),
                        pixmap.height(),
                        clip,
                        scroll_state.surface_extent.unwrap_or(viewport),
                    )
                })
            })
            .flatten();
        let clip_path_mask = needs_host_masks
            .then(|| {
                style.clip_path.as_ref().and_then(|polygon| {
                    polygon_clip_mask(
                        pixmap.width(),
                        pixmap.height(),
                        polygon,
                        &rect,
                        style.font_size.unwrap_or(16.0),
                        root_font_size,
                        viewport,
                    )
                })
            })
            .flatten();
        let has_background_box_paint = !style.background_clip_text
            && (style.background_color.is_some()
                || style.background_image.is_some()
                || style.mask_image.is_some()
                || style.background_gradient.is_some()
                || style.background_radial_gradient.is_some()
                || style.background_conic_gradient.is_some()
                || !style.background_gradient_layers.is_empty());
        let has_inline_box_paint = style.box_shadow.is_some()
            || has_background_box_paint
            || style.border != crate::Edges::default();
        let inline_pieces = (style.ignores_used_box_sizes() && has_inline_box_paint)
            .then(|| laid.inline_fragments.get(&nid))
            .flatten();
        let paints_inline_fragments = inline_pieces.is_some();
        if let Some(pieces) = inline_pieces {
            paint_inline_fragment_decorations(
                &mut pixmap,
                pieces,
                (ox, oy),
                &rect,
                style,
                clip,
                ancestor_clip_mask.as_deref(),
                root_font_size,
                viewport,
                base_url,
                image_cache,
                raster_scale,
            );
        }

        // Outset box-shadow paints behind this element's own background/border.
        // Geometry comes from the full (translate-adjusted) border box; the
        // ancestor overflow clip is reapplied inside so the shadow is clipped by
        // an ancestor exactly as the box itself is.
        if !paints_inline_fragments {
            if let Some(shadow) = style.box_shadow {
                paint_box_shadow(
                    &mut pixmap,
                    &shadow,
                    &rect,
                    style.border_model.radii,
                    ancestor_clip_mask.as_deref(),
                );
            }
        }

        // Background positioning and clipping are independent. Keep the
        // authored gradient/image coordinates tied to the origin box even
        // when the clip box or an ancestor overflow clip reveals only a
        // smaller portion.
        let radius = style.border_model.radii.resolve(rect.width, rect.height);
        let has_radius = !radius.is_zero();
        let background = background_geometry(&rect, style);
        let background_path = background_clip_path(background);
        let combined_clip_mask = clip_path_mask.as_ref().and_then(|polygon| {
            intersect_clip_masks(ancestor_clip_mask.as_deref().cloned(), Some(polygon))
        });
        let element_clip_mask = combined_clip_mask
            .as_ref()
            .or_else(|| ancestor_clip_mask.as_deref());
        let background_mask = element_clip_mask;
        // A linear-gradient background (heavily used by modern hero sections);
        // without this it paints white. Takes precedence over a solid color.
        // `background-clip: text` clips the background to the glyphs, so it must
        // not paint as a box here; the text paint path fills the glyphs instead.
        if box_on_surface
            && !paints_inline_fragments
            && !background_transfers_to_canvas
            && style.mask_image.is_none()
            && !style.background_clip_text
        {
            if let Some(bg) = style.background_color {
                if let Some(path) = background_path.as_ref() {
                    let mut paint = Paint::default();
                    paint.set_color(Color::from_rgba8(bg[0], bg[1], bg[2], bg[3]));
                    paint.anti_alias = !background.clip_radii.is_zero();
                    pixmap.fill_path(
                        path,
                        &paint,
                        FillRule::Winding,
                        raster_transform(raster_scale),
                        background_mask,
                    );
                }
            }
            if !style.background_gradient_layers.is_empty() {
                if let Some(path) = background_path.as_ref() {
                    paint_background_gradient_layers(
                        &mut pixmap,
                        path,
                        &background.origin_rect,
                        &background.clip_rect,
                        background.clip_radii,
                        style,
                        root_font_size,
                        viewport,
                        background_mask,
                        raster_scale,
                    );
                }
            } else {
                if let Some((center, stops)) = &style.background_radial_gradient {
                    if let Some(path) = background_path.as_ref() {
                        paint_radial_gradient(
                            &mut pixmap,
                            path,
                            &background.origin_rect,
                            *center,
                            stops,
                            style.background_radial_gradient_geometry,
                            style.font_size.unwrap_or(16.0),
                            root_font_size,
                            viewport,
                            background_mask,
                            raster_scale,
                        );
                    }
                }
                if let Some((angle, center, stops)) = &style.background_conic_gradient {
                    paint_conic_gradient_sampled(
                        &mut pixmap,
                        &background.clip_rect,
                        &background.origin_rect,
                        background.clip_radii,
                        *angle,
                        *center,
                        stops,
                        background_mask,
                    );
                }
                if let Some((angle, stops)) = &style.background_gradient {
                    if let Some(path) = background_path.as_ref() {
                        paint_linear_gradient(
                            &mut pixmap,
                            path,
                            &background.origin_rect,
                            *angle,
                            stops,
                            background_mask,
                            raster_scale,
                        );
                    }
                }
            }
        }

        if box_on_surface && !paints_inline_fragments && !background_transfers_to_canvas {
            if let Some(mask_url) = &style.mask_image {
                let fill = style
                    .background_color
                    .or(style.color)
                    .unwrap_or([0, 0, 0, 255]);
                paint_mask(
                    mask_url,
                    base_url,
                    &visible_rect,
                    radius,
                    fill,
                    style.background_radial_gradient.as_ref(),
                    style.background_radial_gradient_geometry,
                    style.font_size.unwrap_or(16.0),
                    root_font_size,
                    viewport,
                    style.background_gradient.as_ref(),
                    style.background_conic_gradient.as_ref(),
                    style.mask_size,
                    style.mask_repeat,
                    element_clip_mask,
                    &mut pixmap,
                    image_cache,
                );
            } else if let Some(bg_url) = &style.background_image {
                if let Some(img_rect) = background_image_rect(
                    bg_url,
                    base_url,
                    &background.origin_rect,
                    style.background_size,
                    style.background_size_expression.as_deref(),
                    style.background_size_fit,
                    style.background_position,
                    style.font_size.unwrap_or(16.0),
                    root_font_size,
                    viewport,
                    image_cache,
                ) {
                    // A background layer is always clipped to its owner's border
                    // box and then to inherited overflow. Keep its full destination
                    // rect separate from that clip: intersecting first and then
                    // scaling would resize a partially clipped image.
                    if background.clip_rect.width > 0.0 && background.clip_rect.height > 0.0 {
                        paint_image(
                            bg_url,
                            base_url,
                            &img_rect,
                            &background.clip_rect,
                            crate::ObjectFit::Fill,
                            crate::ObjectPosition::default(),
                            &mut pixmap,
                            image_cache,
                            None,
                            None,
                            background.clip_radii,
                            background_mask,
                        );
                    }
                }
            }
        }
        let has_positioned_pseudo = [
            style.before_pseudo.as_deref(),
            style.after_pseudo.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|pseudo| pseudo.position == Some(taffy::Position::Absolute));
        if has_positioned_pseudo {
            let positioned_pseudo_containing_block = {
                let mut ancestor = Some(nid);
                let mut found = None;
                while let Some(candidate) = ancestor {
                    let candidate_style = laid.styles.get(&candidate);
                    let establishes = candidate_style.is_some_and(|candidate_style| {
                        candidate_style.position.is_some()
                            || candidate_style.establishes_positioning_containing_block()
                    });
                    if establishes {
                        if let Some(candidate_rect) = laid.rects.get(&candidate).copied() {
                            let (candidate_x, candidate_y) =
                                scroll_state.translation_for(laid, candidate);
                            found = Some(crate::Rect {
                                x: candidate_rect.x + candidate_x,
                                y: candidate_rect.y + candidate_y,
                                width: candidate_rect.width,
                                height: candidate_rect.height,
                            });
                            break;
                        }
                        // A positioned inline can be flattened out of the taffy
                        // tree when its block children form the real layout
                        // boxes. It is still the CSS containing block, but has no
                        // usable rectangle in this representation. Keep walking
                        // to the nearest positioned ancestor that does have one.
                    }
                    ancestor = crate::dom::rendered_parent(tree, candidate);
                }
                found.unwrap_or(rect)
            };
            let positioned_pseudo_overflow_clip =
                scroll_state.descendant_overflow_clip_for(laid, nid);
            for pseudo in [
                style.before_pseudo.as_deref(),
                style.after_pseudo.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                paint_positioned_pseudo(
                    &mut laid.text_engine,
                    &mut pixmap,
                    pseudo,
                    &positioned_pseudo_containing_block,
                    &rect,
                    viewport,
                    root_font_size,
                    scroll_state.surface_extent.unwrap_or(viewport),
                    positioned_pseudo_overflow_clip.as_ref(),
                    base_url,
                    image_cache,
                    raster_scale,
                );
            }
        }

        if box_on_surface && name.local.as_ref() == "canvas" {
            if let Some(surface) = canvas_surfaces.surface(nid) {
                // A canvas bitmap is replaced content: CSS sizing and
                // object-fit operate on the content box, never the padding or
                // border box. Keep padding available for the element's own
                // background and inset the rounded clip to the same edge.
                let content_insets = crate::Sides {
                    top: style.border.top + style.padding.top,
                    right: style.border.right + style.padding.right,
                    bottom: style.border.bottom + style.padding.bottom,
                    left: style.border.left + style.padding.left,
                };
                let content_rect = crate::Rect {
                    x: rect.x + content_insets.left,
                    y: rect.y + content_insets.top,
                    width: (rect.width - content_insets.left - content_insets.right).max(0.0),
                    height: (rect.height - content_insets.top - content_insets.bottom).max(0.0),
                };
                let content_visible = content_rect.intersect(&visible_rect).unwrap_or_default();
                paint_canvas_surface(
                    surface,
                    &content_rect,
                    &content_visible,
                    style.object_fit,
                    style.object_position,
                    &mut pixmap,
                    radius.inset(content_insets),
                    element_clip_mask,
                );
            }
        }

        if !paints_inline_fragments {
            paint_css_border(
                &mut pixmap,
                &rect,
                style,
                element_clip_mask,
                raster_scale,
            );
        }
        paint_css_outline(
            &mut pixmap,
            &rect,
            style,
            element_clip_mask,
            raster_scale,
        );

        if box_on_surface && matches!(name.local.as_ref(), "img" | "video") {
            if let Some(source) = selected_images.get(&nid) {
                // `visible_rect` is the border box already intersected with the
                // ancestor overflow clip: the raster must not paint past it (a
                // half-scrolled carousel slide's image otherwise bleeds over
                // the viewport edge).
                let painted = paint_image(
                    &source.resolved_url,
                    None,
                    &rect,
                    &visible_rect,
                    style.object_fit,
                    style.object_position,
                    &mut pixmap,
                    image_cache,
                    Some(source.profile),
                    None,
                    radius,
                    element_clip_mask,
                );
                // Fall back when the image itself did not paint, following
                // what browsers show for a broken image: a non-empty alt
                // renders as text in place of the image (no placeholder box),
                // alt="" renders nothing at all (the author declared the
                // image decorative), and only a MISSING alt keeps the neutral
                // grey placeholder. box_rect/visible_rect are already
                // clip-intersected, so none of this paints outside an
                // overflow:hidden clip.
                if !painted && name.local.as_ref() == "img" {
                    match node.get_attribute("alt") {
                        Some(alt) if !alt.trim().is_empty() => {
                            draw_text(
                                &mut pixmap,
                                &alt,
                                rect.x,
                                rect.y,
                                [0, 0, 0, 255],
                                12.0,
                                false,
                                None,
                                0.0,
                                clip,
                                element_clip_mask,
                                raster_scale,
                            );
                        }
                        Some(_) => {}
                        None => {
                            if visible_rect.width >= 4.0 && visible_rect.height >= 4.0 {
                                let mut ph = Paint::default();
                                ph.set_color(Color::from_rgba8(0xE9, 0xEA, 0xEC, 0xFF));
                                pixmap.fill_rect(
                                    box_rect,
                                    &ph,
                                    raster_transform(raster_scale),
                                    None,
                                );
                            }
                        }
                    }
                }
            }
        }

        // Inline `<svg>...</svg>`: serialize the whole subtree back to one
        // standalone SVG document and rasterize it as a unit, so a
        // `<use href="#id">` resolves against the `<symbol>`/`<defs>` in the
        // same svg. The raster owns the subtree, so its DOM children are not
        // painted individually (they are added to `svg_subtree_skip`). The svg
        // is drawn at its full border-box size (undistorted) and clipped to the
        // overflow-visible region.
        if name.local.as_ref() == "svg" {
            if box_on_surface {
                let mut markup = serialize_svg_styled(
                    tree,
                    nid,
                    &laid.styles,
                    &laid.custom_properties,
                    (suppress_opacity_for == Some(nid)).then_some(nid),
                );
                // Resolve referenced symbols before carrying the host color into
                // the standalone document. A document-level/external symbol may
                // itself contain `currentColor`, and therefore has to be present
                // when the root color is established.
                inject_external_sprites(
                    tree,
                    nid,
                    Some(&laid.styles),
                    Some(&laid.custom_properties),
                    base_url,
                    &mut markup,
                    image_cache,
                    &mut sprite_cache,
                );
                // resvg parses the serialized subtree as a standalone SVG
                // document, outside the page's author stylesheet. Preserve the
                // host element's computed `color` so paths using `currentColor`
                // (the standard framework-logo/icon pattern) do not fall back to
                // black.
                if let Some(color) = style.color {
                    inject_svg_current_color(&mut markup, color);
                }
                // `<use href="url#id">` pointing at an EXTERNAL sprite file resolves
                // to nothing in resvg (the symbol lives in another document). Fetch
                // the sprite, splice the referenced `<symbol>` into a local `<defs>`,
                // and rewrite the href to a same-document `#id`. Same-document
                // `<use href="#id">` (empty url) is untouched.
                if let Some(content) = render_svg_with_font_database(
                    markup.as_bytes(),
                    rect.width as u32,
                    rect.height as u32,
                    &svg_fonts,
                ) {
                    let mut mask = element_clip_mask.cloned();
                    if has_radius {
                        let own = rounded_box_clip_mask_radii(
                            pixmap.width(),
                            pixmap.height(),
                            &visible_rect,
                            radius,
                        );
                        mask = intersect_clip_masks(mask, own.as_ref());
                    }
                    pixmap.draw_pixmap(
                        rect.x as i32,
                        rect.y as i32,
                        content.as_ref(),
                        &tiny_skia::PixmapPaint::default(),
                        Transform::identity(),
                        mask.as_ref(),
                    );
                }
            }
            for child in crate::dom::rendered_descendants(tree, nid) {
                svg_subtree_skip.insert(child);
            }
        }

        if let Some(generated) = generated_before.get(&nid) {
            for generated in generated {
                paint_in_flow_generated_box(
                    &mut pixmap,
                    generated,
                    laid,
                    &scroll_state,
                    viewport,
                    root_font_size,
                    base_url,
                    image_cache,
                    raster_scale,
                );
            }
        }

        // List-item marker (bullet or number), drawn in the indent to the left
        // of the item's content box. `list_style` is inherited and resolved,
        // so `None` (e.g. a nav `<ul style="list-style:none">`) suppresses it.
        if name.local.as_ref() == "li" {
            if let Some(marker) = list_marker_text(tree, nid, style.list_style) {
                let fsize = style.font_size.unwrap_or(16.0);
                let color = style.color.unwrap_or([0, 0, 0, 255]);
                let mw = measure_text(&marker, fsize, false, style.font_family.as_deref());
                let mx = rect.x + style.padding.left - mw - 6.0;
                let my = rect.y + style.border.top + style.padding.top;
                draw_text(
                    &mut pixmap,
                    &marker,
                    mx,
                    my,
                    color,
                    fsize,
                    false,
                    style.font_family.as_deref(),
                    style.letter_spacing.unwrap_or(0.0),
                    clip,
                    element_clip_mask,
                    raster_scale,
                );
            }
        }

        // `::before`/`::after` generated text has no DOM text node of its own.
        // Its shaped word items are registered under the host element, so
        // paint them here. The static path remains for layout-only builds and
        // for a face that cosmic-text cannot decode.
        if let Some(items) = laid.word_ifc_items.get(&nid) {
            let offset = scroll_state.translation_for(laid, nid);
            for &item in items {
                laid.text_engine.paint_item_with_clip_mask_scaled_for_print(
                    item,
                    &mut pixmap,
                    offset,
                    clip,
                    element_clip_mask,
                    raster_scale,
                    print_economy,
                );
            }
        } else if let Some(runs) = laid.text_runs.get(&nid) {
            let color = style.color.unwrap_or([0, 0, 0, 255]);
            let fsize = style.font_size.unwrap_or(16.0);
            let is_bold = crate::style::used_font_weight(style) >= 600;
            for (word_rect, word) in runs {
                draw_text(
                    &mut pixmap,
                    word,
                    word_rect.x + ox,
                    word_rect.y + oy,
                    color,
                    fsize,
                    is_bold,
                    style.font_family.as_deref(),
                    style.letter_spacing.unwrap_or(0.0),
                    clip,
                    element_clip_mask,
                    raster_scale,
                );
            }
        }

        // A closed native `<select>` paints only its selected option. Options
        // themselves are popup content (`display:none` in the layout tree),
        // so the label and disclosure arrow belong to the atomic control.
        if name.local.as_ref() == "select" {
            if let Some(label) = selected_option_label(tree, nid) {
                let fsize = style.font_size.unwrap_or(13.333_333);
                let line_height = crate::inline::used_line_height(style);
                let text_x = rect.x + style.border.left + style.padding.left;
                let text_y = rect.y + (rect.height - line_height) / 2.0;
                draw_text(
                    &mut pixmap,
                    &label,
                    text_x,
                    text_y,
                    style.color.unwrap_or([0, 0, 0, 255]),
                    fsize,
                    crate::style::used_font_weight(style) >= 600,
                    style.font_family.as_deref(),
                    style.letter_spacing.unwrap_or(0.0),
                    Some(visible_rect),
                    element_clip_mask,
                    raster_scale,
                );
            }
            if rect.width >= 12.0 && rect.height >= 8.0 {
                let center_x = rect.x + rect.width - style.border.right - 8.0;
                let center_y = rect.y + rect.height / 2.0;
                let mut arrow = PathBuilder::new();
                arrow.move_to(center_x - 3.5, center_y - 2.0);
                arrow.line_to(center_x + 3.5, center_y - 2.0);
                arrow.line_to(center_x, center_y + 2.5);
                arrow.close();
                if let Some(arrow) = arrow.finish() {
                    let mut arrow_paint = Paint::default();
                    let color = style.color.unwrap_or([0, 0, 0, 255]);
                    arrow_paint
                        .set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
                    pixmap.fill_path(
                        &arrow,
                        &arrow_paint,
                        FillRule::Winding,
                        raster_transform(raster_scale),
                        element_clip_mask,
                    );
                }
            }
        }

        // An empty text `<input>`/`<textarea>` shows its `placeholder`
        // attribute as muted text; there is no DOM text node for it (it is
        // not real content), so paint it directly from the attribute instead
        // of going through `paint_text_node`.
        if name.local.as_ref() == "input" || name.local.as_ref() == "textarea" {
            let has_value = node
                .get_attribute("value")
                .map(|v| !v.is_empty())
                .unwrap_or(false)
                || (name.local.as_ref() == "textarea"
                    && !tree.text_content(nid).is_empty());
            if !has_value {
                if let Some(placeholder) = node.get_attribute("placeholder") {
                    if !placeholder.is_empty() {
                        let fsize = style.font_size.unwrap_or(16.0);
                        let text_x = rect.x + style.padding.left + style.border.left;
                        let text_y = rect.y + style.padding.top + style.border.top;
                        let placeholder_style = style.placeholder_pseudo.as_deref();
                        let mut color = placeholder_style
                            .and_then(|pseudo| pseudo.color)
                            .unwrap_or([117, 117, 117, 255]);
                        let opacity = placeholder_style
                            .and_then(|pseudo| pseudo.opacity)
                            .unwrap_or(1.0)
                            .clamp(0.0, 1.0);
                        color[3] = ((color[3] as f32) * opacity).round() as u8;
                        if color[3] != 0 {
                            draw_text(
                                &mut pixmap,
                                placeholder,
                                text_x,
                                text_y,
                                color,
                                fsize,
                                false,
                                style.font_family.as_deref(),
                                style.letter_spacing.unwrap_or(0.0),
                                clip,
                                element_clip_mask,
                                raster_scale,
                            );
                        }
                    }
                }
            }
        }
        for generated in &generated_after_at[paint_index] {
            paint_in_flow_generated_box(
                &mut pixmap,
                generated,
                laid,
                &scroll_state,
                viewport,
                root_font_size,
                base_url,
                image_cache,
                raster_scale,
            );
        }
    }

    // Inline formatting contexts shaped by cosmic-text (paragraphs, headings,
    // cells, labels) draw last, in tree order, so their glyphs sit above the
    // box backgrounds/borders painted in the loop above. Each item already
    // carries its final origin and clip from `TextEngine::finalize`.
    for nid in paint_nodes {
        if svg_subtree_skip.contains(&nid) || opacity_subtree_skip.contains(&nid) {
            continue;
        }
        let whole = laid.ifc_items.get(&nid).copied();
        let run_items = laid.run_ifc_items.get(&nid).cloned();
        if whole.is_none() && run_items.is_none() {
            continue;
        }
        if laid
            .styles
            .get(&nid)
            .map(|s| s.effectively_invisible)
            .unwrap_or(false)
        {
            continue;
        }
        // Shift the shaped glyphs by the same accumulated translate as the
        // container's box so text under a transformed ancestor moves with
        // it. Computed before the mutable `paint_item` borrow.
        let off = scroll_state.translation_for(laid, nid);
        let overflow_clip = scroll_state.shaped_text_overflow_clip_for(laid, nid);
        let clip = overflow_clip
            .as_ref()
            .map(|clip| clip.viewport_rect(scroll_state.surface_extent.unwrap_or(viewport)));
        let clip_mask = overflow_clip.as_ref().and_then(|clip| {
            cached_overflow_clip_mask(
                &mut overflow_mask_cache,
                pixmap.width(),
                pixmap.height(),
                clip,
                scroll_state.surface_extent.unwrap_or(viewport),
            )
        });
        if let Some(idx) = whole {
            laid.text_engine.paint_item_with_clip_mask_scaled_for_print(
                idx,
                &mut pixmap,
                off,
                clip,
                clip_mask.as_deref(),
                raster_scale,
                print_economy,
            );
        }
        // Anonymous inline-run leaves of a mixed block (see
        // `build_mixed_block`), pinned to their own boxes at finalize.
        if let Some(items) = run_items {
            for idx in items {
                laid.text_engine.paint_item_with_clip_mask_scaled_for_print(
                    idx,
                    &mut pixmap,
                    off,
                    clip,
                    clip_mask.as_deref(),
                    raster_scale,
                    print_economy,
                );
            }
        }
    }

    Some(pixmap)
}

fn raster_transform(scale: f32) -> Transform {
    Transform::from_scale(scale, scale)
}

/// The raster target in the CSS-pixel coordinate space used by paint items.
/// Region captures can raster directly above 1x, so device dimensions alone
/// are not a valid cull rect.
fn paint_surface_rect(pixmap: &Pixmap, raster_scale: f32) -> crate::Rect {
    let scale = if raster_scale.is_finite() && raster_scale > 0.0 {
        raster_scale
    } else {
        1.0
    };
    crate::Rect {
        x: 0.0,
        y: 0.0,
        width: pixmap.width() as f32 / scale,
        height: pixmap.height() as f32 / scale,
    }
}

fn rect_intersects_paint_surface(
    rect: &crate::Rect,
    pixmap: &Pixmap,
    raster_scale: f32,
) -> bool {
    rect.width > 0.0
        && rect.height > 0.0
        && rect.intersect(&paint_surface_rect(pixmap, raster_scale)).is_some()
}

/// Conservative ink overflow for the non-text primitives emitted by one CSS
/// box. Gecko performs the analogous dirty-rect test against a frame's ink
/// overflow before constructing display items. A border box alone is not
/// sufficient because an outset shadow or outline can enter the capture while
/// the box itself remains outside it.
fn non_text_ink_bounds(rect: &crate::Rect, style: &crate::LayoutStyle) -> crate::Rect {
    let mut bounds = *rect;
    if let Some(shadow) = style
        .box_shadow
        .filter(|shadow| !shadow.inset && shadow.color[3] != 0)
    {
        let expansion = shadow.spread + shadow.blur.max(0.0);
        let shadow_bounds = crate::Rect {
            x: rect.x + shadow.offset_x - expansion,
            y: rect.y + shadow.offset_y - expansion,
            width: (rect.width + 2.0 * expansion).max(0.0),
            height: (rect.height + 2.0 * expansion).max(0.0),
        };
        if shadow_bounds.width > 0.0 && shadow_bounds.height > 0.0 {
            bounds = bounds.union(&shadow_bounds);
        }
    }
    let outline = (style.outline.offset + style.outline.used_width()).max(0.0);
    if outline > 0.0 {
        bounds = bounds.union(&crate::Rect {
            x: rect.x - outline,
            y: rect.y - outline,
            width: rect.width + 2.0 * outline,
            height: rect.height + 2.0 * outline,
        });
    }
    bounds
}

/// Paint an ordinary inline's sliced border boxes instead of its multiline
/// bounding union. Background coordinates stay anchored to the union so
/// gradients and images remain one joined `box-decoration-break:slice`
/// decoration, while left/right borders and radii appear only on the first
/// and last continuation respectively.
fn paint_inline_fragment_decorations(
    pixmap: &mut Pixmap,
    fragments: &[crate::Rect],
    offset: (f32, f32),
    union: &crate::Rect,
    style: &crate::LayoutStyle,
    clip: Option<crate::Rect>,
    ancestor_clip_mask: Option<&tiny_skia::Mask>,
    root_font_size: f32,
    viewport: (f32, f32),
    base_url: Option<&str>,
    image_cache: &mut RenderResourceCache,
    raster_scale: f32,
) {
    let union = crate::Rect {
        x: union.x,
        y: union.y,
        width: union.width,
        height: union.height,
    };
    let mut fragment_style = style.clone();
    for (index, fragment) in fragments.iter().enumerate() {
        let fragment = crate::Rect {
            x: fragment.x + offset.0,
            y: fragment.y + offset.1,
            ..*fragment
        };
        if clip.is_some_and(|clip| fragment.intersect(&clip).is_none()) {
            continue;
        }
        let first = index == 0;
        let last = index + 1 == fragments.len();
        fragment_style.border.left = style.border.left;
        fragment_style.border.right = style.border.right;
        fragment_style.border_model.radii = style.border_model.radii;
        if !first {
            fragment_style.border.left = 0.0;
            fragment_style.border_model.radii.top_left = crate::CornerRadius::default();
            fragment_style.border_model.radii.bottom_left = crate::CornerRadius::default();
        }
        if !last {
            fragment_style.border.right = 0.0;
            fragment_style.border_model.radii.top_right = crate::CornerRadius::default();
            fragment_style.border_model.radii.bottom_right = crate::CornerRadius::default();
        }
        let ink = non_text_ink_bounds(&fragment, &fragment_style);
        let visible_ink = match clip {
            Some(clip) => ink.intersect(&clip),
            None => Some(ink),
        };
        if !visible_ink
            .is_some_and(|ink| rect_intersects_paint_surface(&ink, pixmap, raster_scale))
        {
            continue;
        }
        let radius = fragment_style
            .border_model
            .radii
            .resolve(fragment.width, fragment.height);
        let clip_path_mask = fragment_style.clip_path.as_ref().and_then(|polygon| {
            polygon_clip_mask(
                pixmap.width(),
                pixmap.height(),
                polygon,
                &fragment,
                fragment_style.font_size.unwrap_or(16.0),
                root_font_size,
                viewport,
            )
        });
        let background = background_geometry(&fragment, &fragment_style);
        // Keep the positioning area stable across fragments.  The per-fragment
        // style has its sliced edge borders removed, which is correct for the
        // clip but must not move the shared background coordinate system.
        let background_origin = background_geometry(&union, style).origin_rect;
        let background_path = background_clip_path(background);
        let element_clip_mask = background_extra_clip(ancestor_clip_mask, clip_path_mask.as_ref());
        let background_mask = element_clip_mask.clone();

        if let Some(shadow) = fragment_style.box_shadow {
            paint_box_shadow(
                pixmap,
                &shadow,
                &fragment,
                fragment_style.border_model.radii,
                ancestor_clip_mask,
            );
        }
        if fragment_style.mask_image.is_none() && !fragment_style.background_clip_text {
            if let (Some(color), Some(path)) =
                (fragment_style.background_color, background_path.as_ref())
            {
                let mut paint = Paint::default();
                paint.set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
                paint.anti_alias = !background.clip_radii.is_zero();
                pixmap.fill_path(
                    path,
                    &paint,
                    FillRule::Winding,
                    raster_transform(raster_scale),
                    background_mask.as_ref(),
                );
            }
            if !fragment_style.background_gradient_layers.is_empty() {
                if let Some(path) = background_path.as_ref() {
                    paint_background_gradient_layers(
                        pixmap,
                        path,
                        &background_origin,
                        &background.clip_rect,
                        background.clip_radii,
                        &fragment_style,
                        root_font_size,
                        viewport,
                        background_mask.as_ref(),
                        raster_scale,
                    );
                }
            } else {
                if let (Some((center, stops)), Some(path)) = (
                    &fragment_style.background_radial_gradient,
                    background_path.as_ref(),
                ) {
                    paint_radial_gradient(
                        pixmap,
                        path,
                        &background_origin,
                        *center,
                        stops,
                        fragment_style.background_radial_gradient_geometry,
                        fragment_style.font_size.unwrap_or(16.0),
                        root_font_size,
                        viewport,
                        background_mask.as_ref(),
                        raster_scale,
                    );
                }
                if let Some((angle, center, stops)) = &fragment_style.background_conic_gradient {
                    paint_conic_gradient_sampled(
                        pixmap,
                        &background.clip_rect,
                        &background_origin,
                        background.clip_radii,
                        *angle,
                        *center,
                        stops,
                        background_mask.as_ref(),
                    );
                }
                if let (Some((angle, stops)), Some(path)) = (
                    &fragment_style.background_gradient,
                    background_path.as_ref(),
                ) {
                    paint_linear_gradient(
                        pixmap,
                        path,
                        &background_origin,
                        *angle,
                        stops,
                        background_mask.as_ref(),
                        raster_scale,
                    );
                }
            }
        }

        if let Some(mask_url) = &fragment_style.mask_image {
            let fill = fragment_style
                .background_color
                .or(fragment_style.color)
                .unwrap_or([0, 0, 0, 255]);
            paint_mask(
                mask_url,
                base_url,
                &fragment,
                radius,
                fill,
                fragment_style.background_radial_gradient.as_ref(),
                fragment_style.background_radial_gradient_geometry,
                fragment_style.font_size.unwrap_or(16.0),
                root_font_size,
                viewport,
                fragment_style.background_gradient.as_ref(),
                fragment_style.background_conic_gradient.as_ref(),
                fragment_style.mask_size,
                fragment_style.mask_repeat,
                element_clip_mask.as_ref(),
                pixmap,
                image_cache,
            );
        } else if let Some(background_url) = &fragment_style.background_image {
            if let Some(image_rect) = background_image_rect(
                background_url,
                base_url,
                &background_origin,
                fragment_style.background_size,
                fragment_style.background_size_expression.as_deref(),
                fragment_style.background_size_fit,
                fragment_style.background_position,
                fragment_style.font_size.unwrap_or(16.0),
                root_font_size,
                viewport,
                image_cache,
            ) {
                paint_image(
                    background_url,
                    base_url,
                    &image_rect,
                    &background.clip_rect,
                    crate::ObjectFit::Fill,
                    crate::ObjectPosition::default(),
                    pixmap,
                    image_cache,
                    None,
                    None,
                    background.clip_radii,
                    background_mask.as_ref(),
                );
            }
        }
        paint_css_border(
            pixmap,
            &fragment,
            &fragment_style,
            element_clip_mask.as_ref(),
            raster_scale,
        );
    }
}

/// Per-capture root-scroll and sticky offsets layered over an immutable
/// document-space [`DomLayout`]. Keeping these deltas out of the layout avoids
/// accumulating movement when the same prepared document paints more than one
/// frame.
struct ScrollPaintState<'a> {
    tree: &'a DomTree,
    viewport: (f32, f32),
    scroll: (f32, f32),
    viewport_fixed: &'a std::collections::HashSet<obscura_dom::tree::NodeId>,
    sticky: Arc<std::collections::HashMap<obscura_dom::tree::NodeId, (f32, f32)>>,
    sticky_clips: Arc<std::collections::HashMap<obscura_dom::tree::NodeId, (f32, f32)>>,
    viewport_fixed_clips: Arc<
        std::collections::HashMap<
            obscura_dom::tree::NodeId,
            Option<crate::dom::OverflowClip>,
        >,
    >,
    resolved: Option<&'a ResolvedScrollState>,
    clip_scope_root: Option<obscura_dom::tree::NodeId>,
    surface_extent: Option<(f32, f32)>,
    surface_offset: (f32, f32),
    active: bool,
}

fn viewport_fixed_clip_map(
    tree: &DomTree,
    laid: &crate::DomLayout,
    viewport_fixed: &std::collections::HashSet<obscura_dom::tree::NodeId>,
    sticky: &std::collections::HashMap<obscura_dom::tree::NodeId, (f32, f32)>,
) -> std::collections::HashMap<
    obscura_dom::tree::NodeId,
    Option<crate::dom::OverflowClip>,
> {
    fn walk(
        tree: &DomTree,
        laid: &crate::DomLayout,
        viewport_fixed: &std::collections::HashSet<obscura_dom::tree::NodeId>,
        sticky: &std::collections::HashMap<obscura_dom::tree::NodeId, (f32, f32)>,
        id: obscura_dom::tree::NodeId,
        inherited: Option<crate::dom::OverflowClip>,
        out: &mut std::collections::HashMap<
            obscura_dom::tree::NodeId,
            Option<crate::dom::OverflowClip>,
        >,
    ) {
        out.insert(id, inherited.clone());
        let next = match (laid.styles.get(&id), laid.rects.get(&id)) {
            (Some(style), Some(rect))
                if style.overflow_hidden && !style.overflow_propagated_to_viewport =>
            {
                let base = laid.translates.get(&id).copied().unwrap_or((0.0, 0.0));
                let movement = sticky.get(&id).copied().unwrap_or((0.0, 0.0));
                let own = crate::dom::OverflowClip::for_box(
                    rect,
                    style,
                    base.0 + movement.0,
                    base.1 + movement.1,
                );
                Some(match inherited {
                    Some(clip) => clip.intersect(own),
                    None => own,
                })
            }
            _ => inherited,
        };
        for child in crate::dom::rendered_children(tree, id) {
            if viewport_fixed.contains(&child) {
                walk(
                    tree,
                    laid,
                    viewport_fixed,
                    sticky,
                    child,
                    next.clone(),
                    out,
                );
            }
        }
    }

    let mut out = std::collections::HashMap::with_capacity(viewport_fixed.len());
    for &id in viewport_fixed {
        let starts_subtree = crate::dom::rendered_parent(tree, id)
            .is_none_or(|parent| !viewport_fixed.contains(&parent));
        if starts_subtree {
            walk(
                tree,
                laid,
                viewport_fixed,
                sticky,
                id,
                None,
                &mut out,
            );
        }
    }
    out
}

impl<'a> ScrollPaintState<'a> {
    fn new(
        tree: &'a DomTree,
        viewport: (f32, f32),
        requested: (f32, f32),
        content: (f32, f32),
        viewport_fixed: &'a std::collections::HashSet<obscura_dom::tree::NodeId>,
        sticky_layout: &crate::StickyLayout,
        scroll_tree: &crate::dom::ScrollTree,
        laid: &crate::DomLayout,
        shared: Option<&ScrollPaintState<'_>>,
        clip_scope_root: Option<obscura_dom::tree::NodeId>,
        surface_extent: Option<(f32, f32)>,
        surface_offset: (f32, f32),
    ) -> Self {
        let scroll_x = if requested.0.is_finite() {
            crate::quantize_scroll_value(requested.0, 1.0).clamp(
                0.0,
                crate::quantized_scroll_range(content.0, viewport.0, 1.0),
            )
        } else {
            0.0
        };
        let scroll_y = if requested.1.is_finite() {
            crate::quantize_scroll_value(requested.1, 1.0).clamp(
                0.0,
                crate::quantized_scroll_range(content.1, viewport.1, 1.0),
            )
        } else {
            0.0
        };
        let scroll = (scroll_x, scroll_y);
        let active = scroll != (0.0, 0.0) || !sticky_layout.is_empty();
        let (sticky, sticky_clips, viewport_fixed_clips) = if let Some(shared) = shared {
            (
                Arc::clone(&shared.sticky),
                Arc::clone(&shared.sticky_clips),
                Arc::clone(&shared.viewport_fixed_clips),
            )
        } else {
            let sticky = Arc::new(if active {
                sticky_layout.resolved_root_translations(viewport, scroll_tree, scroll)
            } else {
                std::collections::HashMap::new()
            });
            let sticky_clips = Arc::new(sticky_layout.clip_translations_from(&sticky));
            let viewport_fixed_clips = Arc::new(viewport_fixed_clip_map(
                tree,
                laid,
                viewport_fixed,
                &sticky,
            ));
            (sticky, sticky_clips, viewport_fixed_clips)
        };
        Self {
            tree,
            viewport,
            scroll,
            viewport_fixed,
            sticky,
            sticky_clips,
            viewport_fixed_clips,
            resolved: None,
            clip_scope_root,
            surface_extent,
            surface_offset,
            active,
        }
    }

    fn from_resolved(
        tree: &'a DomTree,
        viewport: (f32, f32),
        viewport_fixed: &'a std::collections::HashSet<obscura_dom::tree::NodeId>,
        resolved: &'a ResolvedScrollState,
        clip_scope_root: Option<obscura_dom::tree::NodeId>,
        surface_extent: Option<(f32, f32)>,
        surface_offset: (f32, f32),
    ) -> Self {
        Self {
            tree,
            viewport,
            scroll: resolved.root_offset(),
            viewport_fixed,
            sticky: Arc::new(std::collections::HashMap::new()),
            sticky_clips: Arc::new(std::collections::HashMap::new()),
            viewport_fixed_clips: Arc::new(std::collections::HashMap::new()),
            resolved: Some(resolved),
            clip_scope_root,
            surface_extent,
            surface_offset,
            active: true,
        }
    }

    fn translation_for(
        &self,
        laid: &crate::DomLayout,
        id: obscura_dom::tree::NodeId,
    ) -> (f32, f32) {
        let base = laid.translates.get(&id).copied().unwrap_or((0.0, 0.0));
        if let Some(resolved) = self.resolved {
            let movement = resolved.movement_for(id);
            return (
                base.0 + movement.0 + self.surface_offset.0,
                base.1 + movement.1 + self.surface_offset.1,
            );
        }
        if !self.active {
            return (
                base.0 + self.surface_offset.0,
                base.1 + self.surface_offset.1,
            );
        }
        let sticky = self.sticky.get(&id).copied().unwrap_or((0.0, 0.0));
        let root = if self.viewport_fixed.contains(&id) {
            (0.0, 0.0)
        } else {
            (-self.scroll.0, -self.scroll.1)
        };
        (
            base.0 + sticky.0 + root.0 + self.surface_offset.0,
            base.1 + sticky.1 + root.1 + self.surface_offset.1,
        )
    }

    fn overflow_clip_for(
        &self,
        laid: &crate::DomLayout,
        id: obscura_dom::tree::NodeId,
    ) -> Option<crate::dom::OverflowClip> {
        if self.clip_scope_root.is_none() {
            if let Some(resolved) = self.resolved {
                let mut clip = resolved.inherited_clip_for(id)?;
                clip.translate(self.surface_offset.0, self.surface_offset.1);
                return Some(clip);
            }
            if let Some(clip) = self.viewport_fixed_clips.get(&id) {
                let mut clip = clip.clone()?;
                clip.translate(self.surface_offset.0, self.surface_offset.1);
                return Some(clip);
            }
        }
        let in_viewport_fixed_subtree = self.viewport_fixed.contains(&id);
        if self.clip_scope_root.is_some() {
            if self.clip_scope_root == Some(id) {
                return None;
            }
            let mut owners = Vec::new();
            let mut current = crate::dom::rendered_parent(self.tree, id);
            let mut found_scope = self.clip_scope_root.is_none();
            let mut found_fixed_boundary = false;
            while let Some(owner) = current {
                // Crossing out of a viewport-fixed subtree would reintroduce
                // a document-space clip that fixed positioning escapes.
                if in_viewport_fixed_subtree && !self.viewport_fixed.contains(&owner) {
                    found_fixed_boundary = true;
                    break;
                }
                owners.push(owner);
                if self.clip_scope_root == Some(owner) {
                    found_scope = true;
                    break;
                }
                if in_viewport_fixed_subtree
                    && crate::dom::rendered_parent(self.tree, owner)
                        .is_none_or(|parent| !self.viewport_fixed.contains(&parent))
                {
                    found_fixed_boundary = true;
                    break;
                }
                current = crate::dom::rendered_parent(self.tree, owner);
            }
            if found_scope || found_fixed_boundary {
                let mut clip: Option<crate::dom::OverflowClip> = None;
                for owner in owners.into_iter().rev() {
                    let (Some(style), Some(rect)) =
                        (laid.styles.get(&owner), laid.rects.get(&owner))
                    else {
                        continue;
                    };
                    if !style.overflow_hidden || style.overflow_propagated_to_viewport {
                        continue;
                    }
                    let (x, y) = self.translation_for(laid, owner);
                    let own = crate::dom::OverflowClip::for_box(rect, style, x, y);
                    clip = Some(match clip {
                        Some(current) => current.intersect(own),
                        None => own,
                    });
                }
                return clip;
            }
        }
        if let Some(resolved) = self.resolved {
            let mut clip = resolved.inherited_clip_for(id)?;
            clip.translate(self.surface_offset.0, self.surface_offset.1);
            return Some(clip);
        }
        let mut clip = laid.clip_rects.get(&id).cloned().flatten()?;
        if self.active && !self.viewport_fixed.contains(&id) {
            let sticky = self.sticky_clips.get(&id).copied().unwrap_or((0.0, 0.0));
            clip.translate(sticky.0 - self.scroll.0, sticky.1 - self.scroll.1);
        }
        clip.translate(self.surface_offset.0, self.surface_offset.1);
        Some(clip)
    }

    fn descendant_overflow_clip_for(
        &self,
        laid: &crate::DomLayout,
        id: obscura_dom::tree::NodeId,
    ) -> Option<crate::dom::OverflowClip> {
        let inherited = self.overflow_clip_for(laid, id);
        let Some(style) = laid.styles.get(&id) else {
            return inherited;
        };
        if !style.overflow_hidden || style.overflow_propagated_to_viewport {
            return inherited;
        }
        let Some(rect) = laid.rects.get(&id) else {
            return inherited;
        };
        let (ox, oy) = self.translation_for(laid, id);
        let own = crate::dom::OverflowClip::for_box(rect, style, ox, oy);
        Some(match inherited {
            Some(clip) => clip.intersect(own),
            None => own,
        })
    }

    fn shaped_text_overflow_clip_for(
        &self,
        laid: &crate::DomLayout,
        id: obscura_dom::tree::NodeId,
    ) -> Option<crate::dom::OverflowClip> {
        self.descendant_overflow_clip_for(laid, id)
    }
}

/// A closed rounded-rectangle path, corners approximated by quadratic curves
/// (visually indistinguishable from true arcs at typical UI radii). The
/// horizontal and vertical radii are scaled together when necessary, matching
/// CSS's overlap rule while preserving percentage ellipses.
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) -> Option<tiny_skia::Path> {
    rounded_rect_path_radii(
        x,
        y,
        w,
        h,
        crate::ResolvedBorderRadii {
            top_left: (rx, ry),
            top_right: (rx, ry),
            bottom_right: (rx, ry),
            bottom_left: (rx, ry),
        },
    )
}

fn rounded_rect_path_radii(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radii: crate::ResolvedBorderRadii,
) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    if radii.is_zero() {
        let mut pb = PathBuilder::new();
        pb.push_rect(Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    let tl = radii.top_left;
    let tr = radii.top_right;
    let br = radii.bottom_right;
    let bl = radii.bottom_left;
    let mut pb = PathBuilder::new();
    pb.move_to(x + tl.0, y);
    pb.line_to(x + w - tr.0, y);
    pb.quad_to(x + w, y, x + w, y + tr.1);
    pb.line_to(x + w, y + h - br.1);
    pb.quad_to(x + w, y + h, x + w - br.0, y + h);
    pb.line_to(x + bl.0, y + h);
    pb.quad_to(x, y + h, x, y + h - bl.1);
    pb.line_to(x, y + tl.1);
    pb.quad_to(x, y, x + tl.0, y);
    pb.close();
    pb.finish()
}

#[derive(Clone, Copy)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

fn effective_border_styles(style: &crate::LayoutStyle) -> crate::Sides<crate::BorderStyle> {
    let mut styles = style.border_model.styles;
    // Preserve the public LayoutStyle contract for embedding code and older
    // renderer tests that construct used `border` edges directly. Cascaded
    // CSS never reaches this branch: none/hidden declarations synchronize the
    // used edges to zero before layout.
    if style.border.top > 0.0 && !styles.top.is_visible() {
        styles.top = crate::BorderStyle::Solid;
    }
    if style.border.right > 0.0 && !styles.right.is_visible() {
        styles.right = crate::BorderStyle::Solid;
    }
    if style.border.bottom > 0.0 && !styles.bottom.is_visible() {
        styles.bottom = crate::BorderStyle::Solid;
    }
    if style.border.left > 0.0 && !styles.left.is_visible() {
        styles.left = crate::BorderStyle::Solid;
    }
    styles
}

fn paint_css_border(
    pixmap: &mut Pixmap,
    rect: &crate::Rect,
    style: &crate::LayoutStyle,
    mask: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    let widths = crate::Sides {
        top: style.border.top,
        right: style.border.right,
        bottom: style.border.bottom,
        left: style.border.left,
    };
    if widths.as_array().iter().all(|width| *width <= 0.0) {
        return;
    }
    if !rect_intersects_paint_surface(rect, pixmap, raster_scale) {
        return;
    }
    let current = style.color.unwrap_or([0, 0, 0, 255]);
    let colors = style
        .border_model
        .colors
        .map(|color| color.or(style.border_color).unwrap_or(current));
    let styles = effective_border_styles(style);
    let radii = style.border_model.radii.resolve(rect.width, rect.height);
    let uniform = widths.top == widths.right
        && widths.right == widths.bottom
        && widths.bottom == widths.left
        && styles.top == styles.right
        && styles.right == styles.bottom
        && styles.bottom == styles.left
        && colors.top == colors.right
        && colors.right == colors.bottom
        && colors.bottom == colors.left;
    if uniform
        && !matches!(
            styles.top,
            crate::BorderStyle::Inset
                | crate::BorderStyle::Outset
                | crate::BorderStyle::Groove
                | crate::BorderStyle::Ridge
        )
    {
        paint_uniform_border(
            pixmap,
            rect,
            widths.top,
            styles.top,
            colors.top,
            radii,
            mask,
            raster_scale,
        );
        return;
    }

    for side in [Side::Top, Side::Right, Side::Bottom, Side::Left] {
        let (width, line_style, color) = match side {
            Side::Top => (widths.top, styles.top, colors.top),
            Side::Right => (widths.right, styles.right, colors.right),
            Side::Bottom => (widths.bottom, styles.bottom, colors.bottom),
            Side::Left => (widths.left, styles.left, colors.left),
        };
        if width <= 0.0 || !line_style.is_visible() {
            continue;
        }
        match line_style {
            crate::BorderStyle::Solid | crate::BorderStyle::Auto => {
                fill_solid_border_side(
                    pixmap,
                    rect,
                    widths,
                    radii,
                    side,
                    color,
                    mask,
                    raster_scale,
                );
            }
            crate::BorderStyle::Inset
            | crate::BorderStyle::Outset
            | crate::BorderStyle::Groove
            | crate::BorderStyle::Ridge => {
                // CSS's relief styles are two-tone. Preserve the directional
                // light source even in the compact painter; groove/ridge use
                // the same side polarity as inset/outset at narrow widths.
                let top_left = matches!(side, Side::Top | Side::Left);
                let dark_side = match line_style {
                    crate::BorderStyle::Inset | crate::BorderStyle::Groove => top_left,
                    crate::BorderStyle::Outset | crate::BorderStyle::Ridge => !top_left,
                    _ => false,
                };
                let color = shade_border_color(color, if dark_side { -0.28 } else { 0.28 });
                fill_solid_border_side(
                    pixmap,
                    rect,
                    widths,
                    radii,
                    side,
                    color,
                    mask,
                    raster_scale,
                );
            }
            crate::BorderStyle::Double => {
                if width < 3.0 {
                    fill_solid_border_side(
                        pixmap,
                        rect,
                        widths,
                        radii,
                        side,
                        color,
                        mask,
                        raster_scale,
                    );
                } else {
                    paint_straight_border_side(
                        pixmap,
                        rect,
                        side,
                        width / 3.0,
                        width / 6.0,
                        color,
                        None,
                        mask,
                        raster_scale,
                    );
                    paint_straight_border_side(
                        pixmap,
                        rect,
                        side,
                        width / 3.0,
                        width * 5.0 / 6.0,
                        color,
                        None,
                        mask,
                        raster_scale,
                    );
                }
            }
            crate::BorderStyle::Dashed | crate::BorderStyle::Dotted => {
                paint_straight_border_side(
                    pixmap,
                    rect,
                    side,
                    width,
                    width / 2.0,
                    color,
                    Some(line_style),
                    mask,
                    raster_scale,
                );
            }
            crate::BorderStyle::None | crate::BorderStyle::Hidden => {}
        }
    }
}

fn paint_css_outline(
    pixmap: &mut Pixmap,
    rect: &crate::Rect,
    style: &crate::LayoutStyle,
    mask: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    let width = style.outline.used_width();
    if width <= 0.0 {
        return;
    }
    let outer_expansion = (style.outline.offset + width).max(0.0);
    let outline_bounds = crate::Rect {
        x: rect.x - outer_expansion,
        y: rect.y - outer_expansion,
        width: rect.width + 2.0 * outer_expansion,
        height: rect.height + 2.0 * outer_expansion,
    };
    if !rect_intersects_paint_surface(&outline_bounds, pixmap, raster_scale) {
        return;
    }
    let center_expansion = style.outline.offset + width / 2.0;
    let center_rect = crate::Rect {
        x: rect.x - center_expansion,
        y: rect.y - center_expansion,
        width: rect.width + 2.0 * center_expansion,
        height: rect.height + 2.0 * center_expansion,
    };
    if center_rect.width <= 0.0 || center_rect.height <= 0.0 {
        return;
    }
    let radii = style
        .border_model
        .radii
        .resolve(rect.width, rect.height)
        .outset(crate::Sides::all(center_expansion));
    let color = style
        .outline
        .color
        .or(style.color)
        .unwrap_or([0, 0, 0, 255]);
    stroke_rounded_border_path(
        pixmap,
        &center_rect,
        width,
        style.outline.style,
        color,
        radii,
        mask,
        raster_scale,
    );
}

fn paint_uniform_border(
    pixmap: &mut Pixmap,
    rect: &crate::Rect,
    width: f32,
    line_style: crate::BorderStyle,
    color: [u8; 4],
    radii: crate::ResolvedBorderRadii,
    mask: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    if width <= 0.0 || !line_style.is_visible() {
        return;
    }
    if line_style == crate::BorderStyle::Double && width >= 3.0 {
        let stripe = width / 3.0;
        for inset in [stripe / 2.0, width - stripe / 2.0] {
            let stripe_rect = crate::Rect {
                x: rect.x + inset,
                y: rect.y + inset,
                width: rect.width - 2.0 * inset,
                height: rect.height - 2.0 * inset,
            };
            if stripe_rect.width > 0.0 && stripe_rect.height > 0.0 {
                stroke_rounded_border_path(
                    pixmap,
                    &stripe_rect,
                    stripe,
                    crate::BorderStyle::Solid,
                    color,
                    radii.inset(crate::Sides::all(inset)),
                    mask,
                    raster_scale,
                );
            }
        }
        return;
    }
    let center = width / 2.0;
    let center_rect = crate::Rect {
        x: rect.x + center,
        y: rect.y + center,
        width: rect.width - width,
        height: rect.height - width,
    };
    if center_rect.width <= 0.0 || center_rect.height <= 0.0 {
        return;
    }
    stroke_rounded_border_path(
        pixmap,
        &center_rect,
        width,
        line_style,
        color,
        radii.inset(crate::Sides::all(center)),
        mask,
        raster_scale,
    );
}

fn stroke_rounded_border_path(
    pixmap: &mut Pixmap,
    center_rect: &crate::Rect,
    width: f32,
    line_style: crate::BorderStyle,
    color: [u8; 4],
    radii: crate::ResolvedBorderRadii,
    mask: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    let Some(path) = rounded_rect_path_radii(
        center_rect.x,
        center_rect.y,
        center_rect.width,
        center_rect.height,
        radii,
    ) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
    paint.anti_alias = !radii.is_zero();
    let mut stroke = tiny_skia::Stroke {
        width,
        ..Default::default()
    };
    match line_style {
        crate::BorderStyle::Dashed => {
            stroke.dash = tiny_skia::StrokeDash::new(vec![width * 3.0, width * 3.0], 0.0);
        }
        crate::BorderStyle::Dotted => {
            stroke.dash = tiny_skia::StrokeDash::new(vec![0.0, width * 2.0], 0.0);
            stroke.line_cap = tiny_skia::LineCap::Round;
        }
        _ => {}
    }
    pixmap.stroke_path(&path, &paint, &stroke, raster_transform(raster_scale), mask);
}

fn fill_solid_border_side(
    pixmap: &mut Pixmap,
    rect: &crate::Rect,
    widths: crate::Sides<f32>,
    radii: crate::ResolvedBorderRadii,
    side: Side,
    color: [u8; 4],
    mask: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    let Some(path) = solid_border_side_path(rect, widths, radii, side) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
    paint.anti_alias = !radii.is_zero();
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        raster_transform(raster_scale),
        mask,
    );
}

fn arc_point(center: (f32, f32), radius: (f32, f32), degrees: f32) -> (f32, f32) {
    let angle = degrees.to_radians();
    (
        center.0 + radius.0 * angle.cos(),
        center.1 + radius.1 * angle.sin(),
    )
}

fn append_arc(
    path: &mut PathBuilder,
    center: (f32, f32),
    radius: (f32, f32),
    start: f32,
    end: f32,
) {
    for step in 0..=4 {
        let angle = start + (end - start) * step as f32 / 4.0;
        let point = arc_point(center, radius, angle);
        path.line_to(point.0, point.1);
    }
}

fn solid_border_side_path(
    rect: &crate::Rect,
    widths: crate::Sides<f32>,
    outer: crate::ResolvedBorderRadii,
    side: Side,
) -> Option<tiny_skia::Path> {
    let inner = outer.inset(widths);
    let x = rect.x;
    let y = rect.y;
    let right = x + rect.width;
    let bottom = y + rect.height;
    let ix = x + widths.left;
    let iy = y + widths.top;
    let iright = right - widths.right;
    let ibottom = bottom - widths.bottom;
    let outer_centers = [
        (x + outer.top_left.0, y + outer.top_left.1),
        (right - outer.top_right.0, y + outer.top_right.1),
        (right - outer.bottom_right.0, bottom - outer.bottom_right.1),
        (x + outer.bottom_left.0, bottom - outer.bottom_left.1),
    ];
    let inner_centers = [
        (ix + inner.top_left.0, iy + inner.top_left.1),
        (iright - inner.top_right.0, iy + inner.top_right.1),
        (
            iright - inner.bottom_right.0,
            ibottom - inner.bottom_right.1,
        ),
        (ix + inner.bottom_left.0, ibottom - inner.bottom_left.1),
    ];
    let outer_radii = [
        outer.top_left,
        outer.top_right,
        outer.bottom_right,
        outer.bottom_left,
    ];
    let inner_radii = [
        inner.top_left,
        inner.top_right,
        inner.bottom_right,
        inner.bottom_left,
    ];
    let (a, b, outer_a, outer_b, inner_b, inner_a) = match side {
        Side::Top => (225.0, 270.0, 0, 1, 1, 0),
        Side::Right => (315.0, 360.0, 1, 2, 2, 1),
        Side::Bottom => (45.0, 90.0, 2, 3, 3, 2),
        Side::Left => (135.0, 180.0, 3, 0, 0, 3),
    };
    let second_start = match side {
        Side::Top => 270.0,
        Side::Right => 0.0,
        Side::Bottom => 90.0,
        Side::Left => 180.0,
    };
    let second_end = match side {
        Side::Top => 315.0,
        Side::Right => 45.0,
        Side::Bottom => 135.0,
        Side::Left => 225.0,
    };
    let mut path = PathBuilder::new();
    let start = arc_point(outer_centers[outer_a], outer_radii[outer_a], a);
    path.move_to(start.0, start.1);
    append_arc(
        &mut path,
        outer_centers[outer_a],
        outer_radii[outer_a],
        a,
        b,
    );
    append_arc(
        &mut path,
        outer_centers[outer_b],
        outer_radii[outer_b],
        second_start,
        second_end,
    );
    append_arc(
        &mut path,
        inner_centers[inner_b],
        inner_radii[inner_b],
        second_end,
        second_start,
    );
    append_arc(
        &mut path,
        inner_centers[inner_a],
        inner_radii[inner_a],
        b,
        a,
    );
    path.close();
    path.finish()
}

fn paint_straight_border_side(
    pixmap: &mut Pixmap,
    rect: &crate::Rect,
    side: Side,
    stroke_width: f32,
    inward: f32,
    color: [u8; 4],
    pattern: Option<crate::BorderStyle>,
    mask: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    let mut path = PathBuilder::new();
    match side {
        Side::Top => {
            path.move_to(rect.x, rect.y + inward);
            path.line_to(rect.x + rect.width, rect.y + inward);
        }
        Side::Right => {
            path.move_to(rect.x + rect.width - inward, rect.y);
            path.line_to(rect.x + rect.width - inward, rect.y + rect.height);
        }
        Side::Bottom => {
            path.move_to(rect.x + rect.width, rect.y + rect.height - inward);
            path.line_to(rect.x, rect.y + rect.height - inward);
        }
        Side::Left => {
            path.move_to(rect.x + inward, rect.y + rect.height);
            path.line_to(rect.x + inward, rect.y);
        }
    }
    let Some(path) = path.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
    let mut stroke = tiny_skia::Stroke {
        width: stroke_width,
        ..Default::default()
    };
    if pattern == Some(crate::BorderStyle::Dashed) {
        stroke.dash = tiny_skia::StrokeDash::new(vec![stroke_width * 3.0, stroke_width * 3.0], 0.0);
    } else if pattern == Some(crate::BorderStyle::Dotted) {
        stroke.dash = tiny_skia::StrokeDash::new(vec![0.0, stroke_width * 2.0], 0.0);
        stroke.line_cap = tiny_skia::LineCap::Round;
    }
    pixmap.stroke_path(&path, &paint, &stroke, raster_transform(raster_scale), mask);
}

fn shade_border_color(color: [u8; 4], amount: f32) -> [u8; 4] {
    let target = if amount >= 0.0 { 255.0 } else { 0.0 };
    let factor = amount.abs().min(1.0);
    let channel = |value: u8| {
        (value as f32 + (target - value as f32) * factor)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [
        channel(color[0]),
        channel(color[1]),
        channel(color[2]),
        color[3],
    ]
}

/// Paint an outset `box-shadow` layer behind the element's own box. `rect` is
/// the element's (translate-adjusted) border box; the shadow is that box offset
/// by (offset_x, offset_y), expanded by `spread`, with a `blur`-wide soft edge.
/// tiny-skia has no gaussian blur, so the blur is approximated by nested
/// rounded rects from a solid core out to the blur radius, each at a fraction of
/// the shadow alpha so source-over accumulation ramps the coverage from full at
/// the core to near-zero at the outer edge. A shared mask removes the element's
/// original border box from every outset layer. `inset` shadows are parsed but
/// not painted. `clip`, when set, is the ancestor `overflow: hidden` region and
/// is intersected with the shadow mask.
fn paint_box_shadow(
    pixmap: &mut Pixmap,
    shadow: &crate::BoxShadow,
    rect: &crate::Rect,
    border_radius: crate::BorderRadii,
    ancestor_clip: Option<&tiny_skia::Mask>,
) {
    if shadow.inset || shadow.color[3] == 0 {
        return;
    }
    let spread = shadow.spread;
    let x0 = rect.x + shadow.offset_x - spread;
    let y0 = rect.y + shadow.offset_y - spread;
    let w0 = rect.width + 2.0 * spread;
    let h0 = rect.height + 2.0 * spread;
    if w0 <= 0.0 || h0 <= 0.0 {
        return;
    }
    let border_radii = border_radius.resolve(rect.width, rect.height);
    let radius = border_radii.top_left;
    let rx0 = (radius.0 + spread).max(0.0);
    let ry0 = (radius.1 + spread).max(0.0);
    let blur = shadow.blur.max(0.0);
    let shadow_bounds = crate::Rect {
        x: x0 - blur,
        y: y0 - blur,
        width: w0 + 2.0 * blur,
        height: h0 + 2.0 * blur,
    };
    // Shadows disable the native >1x path, so their paint coordinate space is
    // the pixmap's one-CSS-pixel-per-pixel surface.
    if !rect_intersects_paint_surface(&shadow_bounds, pixmap, 1.0) {
        return;
    }
    let left = shadow_bounds.x.floor().max(0.0) as i32;
    let top = shadow_bounds.y.floor().max(0.0) as i32;
    let right = (shadow_bounds.x + shadow_bounds.width)
        .ceil()
        .min(pixmap.width() as f32) as i32;
    let bottom = (shadow_bounds.y + shadow_bounds.height)
        .ceil()
        .min(pixmap.height() as f32) as i32;
    let Some(mut shadow_pixmap) = Pixmap::new((right - left) as u32, (bottom - top) as u32)
    else {
        return;
    };
    let local_rect = crate::Rect {
        x: rect.x - left as f32,
        y: rect.y - top as f32,
        ..*rect
    };
    let Some(mut shadow_mask) =
        rounded_box_clip_mask_radii(
            shadow_pixmap.width(),
            shadow_pixmap.height(),
            &local_rect,
            border_radii,
        )
    else {
        return;
    };
    shadow_mask.invert();
    let color = shadow.color;
    if blur < 0.5 {
        // No blur: a single crisp, offset (and spread) rounded rect.
        fill_shadow_rect(
            &mut shadow_pixmap,
            x0 - left as f32,
            y0 - top as f32,
            w0,
            h0,
            rx0,
            ry0,
            color,
            Some(&shadow_mask),
        );
    } else {
        let steps: u32 = (blur.ceil() as u32).clamp(2, 24);
        // Per-layer alpha chosen so `steps` source-over composites reach the target
        // alpha at the core: 1 - (1 - a)^steps == A  =>  a = 1 - (1 - A)^(1/steps).
        let a_frac = color[3] as f32 / 255.0;
        let per = 1.0 - (1.0 - a_frac).powf(1.0 / steps as f32);
        let layer_alpha = (per * 255.0).round().clamp(1.0, 255.0) as u8;
        let layer_color = [color[0], color[1], color[2], layer_alpha];
        for j in 0..steps {
            // j = 0 is the solid core (expansion 0); j = steps-1 reaches the blur
            // radius. Larger rects paint first, smaller (more-covered) ones on top.
            let e = blur * (j as f32) / ((steps - 1) as f32);
            fill_shadow_rect(
                &mut shadow_pixmap,
                x0 - e - left as f32,
                y0 - e - top as f32,
                w0 + 2.0 * e,
                h0 + 2.0 * e,
                rx0 + e,
                ry0 + e,
                layer_color,
                Some(&shadow_mask),
            );
        }
    }
    // Ancestor overflow clip is already the complete rect/rounded chain.
    pixmap.draw_pixmap(
        left,
        top,
        shadow_pixmap.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        Transform::identity(),
        ancestor_clip,
    );
}

/// Fill one (possibly rounded) shadow rectangle with a flat color, optionally
/// constrained by an alpha mask. A helper for `paint_box_shadow`'s layers.
fn fill_shadow_rect(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius_x: f32,
    radius_y: f32,
    color: [u8; 4],
    mask: Option<&tiny_skia::Mask>,
) {
    if w <= 0.0 || h <= 0.0 || color[3] == 0 {
        return;
    }
    let path = if radius_x > 0.5 && radius_y > 0.5 {
        match rounded_rect_path(x, y, w, h, radius_x, radius_y) {
            Some(p) => p,
            None => return,
        }
    } else {
        let r = match Rect::from_xywh(x, y, w, h) {
            Some(r) => r,
            None => return,
        };
        let mut pb = PathBuilder::new();
        pb.push_rect(r);
        match pb.finish() {
            Some(p) => p,
            None => return,
        }
    };
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
    paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        mask,
    );
}

/// The marker text for a list item, or `None` when markers are suppressed
/// (`list-style: none`). `Decimal` numbers the item by its position among
/// sibling list items so `<ol>`s count 1, 2, 3.
fn list_marker_text(
    tree: &DomTree,
    nid: obscura_dom::tree::NodeId,
    style: Option<crate::ListStyle>,
) -> Option<String> {
    match style {
        Some(crate::ListStyle::Disc) => Some("\u{2022}".to_string()),
        Some(crate::ListStyle::Circle) => Some("\u{25E6}".to_string()),
        Some(crate::ListStyle::Square) => Some("\u{25AA}".to_string()),
        Some(crate::ListStyle::Decimal) => {
            let mut n = 1usize;
            let mut cur = tree.get_node(nid).and_then(|node| node.prev_sibling);
            while let Some(sib) = cur {
                if tree
                    .get_node(sib)
                    .and_then(|s| s.as_element().map(|e| e.local.to_string()))
                    .as_deref()
                    == Some("li")
                {
                    n += 1;
                }
                cur = tree.get_node(sib).and_then(|s| s.prev_sibling);
            }
            Some(format!("{}.", n))
        }
        Some(crate::ListStyle::None) | None => None,
    }
}

fn selected_option_label(tree: &DomTree, select: obscura_dom::tree::NodeId) -> Option<String> {
    let mut first = None;
    for option_id in tree.descendants(select) {
        let Some(option) = tree.get_node(option_id) else {
            continue;
        };
        if option
            .as_element()
            .map_or(true, |name| name.local.as_ref() != "option")
        {
            continue;
        }
        let label = option
            .get_attribute("label")
            .map(str::to_owned)
            .unwrap_or_else(|| tree.text_content(option_id).trim().to_string());
        if first.is_none() {
            first = Some(label.clone());
        }
        if option.get_attribute("selected").is_some() {
            return Some(label);
        }
    }
    first
}

/// Render `tree` at `viewport` to PNG bytes (RGBA 8-bit). Returns None if the
/// viewport is zero-sized. Convenience over `paint_dom` + `encode_png`.
pub fn screenshot_png(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
) -> Option<Vec<u8>> {
    paint_dom(tree, viewport, base_url)?.encode_png().ok()
}

/// PNG convenience wrapper for a scrolled root viewport.
pub fn screenshot_png_scrolled(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    scroll: (f32, f32),
) -> Option<Vec<u8>> {
    paint_dom_scrolled(tree, viewport, base_url, scroll).and_then(|pixmap| pixmap.encode_png().ok())
}

pub fn screenshot_png_scrolled_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    scroll: (f32, f32),
    animation_sample_time: crate::AnimationSampleTime,
) -> Option<Vec<u8>> {
    paint_dom_scrolled_at_animation_time(
        tree,
        viewport,
        base_url,
        scroll,
        animation_sample_time,
    )
    .and_then(|pixmap| pixmap.encode_png().ok())
}

pub fn screenshot_png_scrolled_at_animation_time_with_surface_color(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    scroll: (f32, f32),
    animation_sample_time: crate::AnimationSampleTime,
    surface_color: [u8; 4],
) -> Option<Vec<u8>> {
    paint_dom_scrolled_at_animation_time_with_surface_color(
        tree,
        viewport,
        base_url,
        scroll,
        animation_sample_time,
        surface_color,
    )
    .and_then(|pixmap| pixmap.encode_png().ok())
}

/// PNG convenience wrapper for a retained resource-aware layout.
pub fn screenshot_prepared(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: (f32, f32),
) -> Option<Vec<u8>> {
    paint_prepared(tree, prepared, resources, scroll)?
        .encode_png()
        .ok()
}

pub fn screenshot_prepared_with_scroll(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
) -> Option<Vec<u8>> {
    paint_prepared_with_scroll(tree, prepared, resources, scroll)?
        .encode_png()
        .ok()
}

pub fn screenshot_prepared_with_scroll_and_surface_color(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    surface_color: [u8; 4],
) -> Option<Vec<u8>> {
    paint_prepared_with_scroll_and_surface_color(
        tree,
        prepared,
        resources,
        scroll,
        surface_color,
    )?
    .encode_png()
    .ok()
}

pub fn screenshot_prepared_with_scroll_and_surface_color_and_canvas_surfaces(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    surface_color: [u8; 4],
    canvas_surfaces: &dyn CanvasSurfaceSource,
) -> Option<Vec<u8>> {
    paint_prepared_with_scroll_and_surface_color_and_canvas_surfaces(
        tree,
        prepared,
        resources,
        scroll,
        surface_color,
        canvas_surfaces,
    )?
    .encode_png()
    .ok()
}

/// PNG convenience wrapper for [`paint_prepared_region_with_scroll`].
pub fn screenshot_prepared_region_with_scroll(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    region: CaptureRegion,
) -> Result<Vec<u8>, CaptureError> {
    paint_prepared_region_with_scroll(tree, prepared, resources, scroll, region)?
        .encode_png()
        .map_err(|_| CaptureError::EncodeFailed)
}

pub fn screenshot_prepared_region_with_scroll_and_surface_color(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    region: CaptureRegion,
    surface_color: [u8; 4],
) -> Result<Vec<u8>, CaptureError> {
    paint_prepared_region_with_scroll_and_surface_color(
        tree,
        prepared,
        resources,
        scroll,
        region,
        surface_color,
    )?
    .encode_png()
    .map_err(|_| CaptureError::EncodeFailed)
}

pub fn screenshot_prepared_region_with_scroll_and_surface_color_and_canvas_surfaces(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    region: CaptureRegion,
    surface_color: [u8; 4],
    canvas_surfaces: &dyn CanvasSurfaceSource,
) -> Result<Vec<u8>, CaptureError> {
    paint_prepared_region_with_scroll_and_surface_color_and_canvas_surfaces(
        tree,
        prepared,
        resources,
        scroll,
        region,
        surface_color,
        canvas_surfaces,
    )?
    .encode_png()
    .map_err(|_| CaptureError::EncodeFailed)
}

/// Capture a retained region using the PDF print-background policy. Ordinary
/// screenshots pass `true`. When false, authored fills are replaced with the
/// browser print-economy white fill and light text is darkened for legibility;
/// borders, shadows, replaced content, and masks remain paintable.
pub fn screenshot_prepared_region_with_scroll_and_backgrounds(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    region: CaptureRegion,
    paint_backgrounds: bool,
) -> Result<Vec<u8>, CaptureError> {
    screenshot_prepared_region_with_scroll_and_backgrounds_and_canvas_surfaces(
        tree,
        prepared,
        resources,
        scroll,
        region,
        paint_backgrounds,
        &EMPTY_CANVAS_SURFACES,
    )
}

pub fn screenshot_prepared_region_with_scroll_and_backgrounds_and_canvas_surfaces(
    tree: &DomTree,
    prepared: &mut PreparedRender,
    resources: &mut RenderResourceCache,
    scroll: &ResolvedScrollState,
    region: CaptureRegion,
    paint_backgrounds: bool,
    canvas_surfaces: &dyn CanvasSurfaceSource,
) -> Result<Vec<u8>, CaptureError> {
    if paint_backgrounds {
        return paint_prepared_region_with_scroll_and_surface_color_and_canvas_surfaces(
            tree,
            prepared,
            resources,
            scroll,
            region,
            [255, 255, 255, 255],
            canvas_surfaces,
        )?
        .encode_png()
        .map_err(|_| CaptureError::EncodeFailed);
    }

    let snapshots = prepared
        .layout
        .styles
        .iter_mut()
        .map(|(&node, style)| (node, PrintEconomyStyleSnapshot::apply(style)))
        .collect::<Vec<_>>();
    let result = paint_prepared_region_with_scroll_with_print_economy(
        tree,
        prepared,
        resources,
        scroll,
        region,
        canvas_surfaces,
    )
    .and_then(|pixmap| pixmap.encode_png().map_err(|_| CaptureError::EncodeFailed));
    for (node, snapshot) in snapshots {
        if let Some(style) = prepared.layout.styles.get_mut(&node) {
            snapshot.restore(style);
        }
    }
    result
}

struct PrintEconomyStyleSnapshot {
    background_color: Option<[u8; 4]>,
    background_gradient: Option<(f32, Vec<([u8; 4], Option<f32>)>)>,
    background_radial_gradient: Option<((f32, f32), Vec<([u8; 4], Option<f32>)>)>,
    background_conic_gradient: Option<(f32, (f32, f32), Vec<([u8; 4], Option<f32>)>)>,
    background_gradient_layers: Vec<crate::BackgroundGradientLayer>,
    background_image: Option<String>,
    color: Option<[u8; 4]>,
    before_pseudo: Option<Box<PrintEconomyStyleSnapshot>>,
    after_pseudo: Option<Box<PrintEconomyStyleSnapshot>>,
}

impl PrintEconomyStyleSnapshot {
    fn apply(style: &mut crate::LayoutStyle) -> Self {
        let background_color = style.background_color.take();
        let background_gradient = style.background_gradient.take();
        let background_radial_gradient = style.background_radial_gradient.take();
        let background_conic_gradient = style.background_conic_gradient.take();
        let background_gradient_layers = std::mem::take(&mut style.background_gradient_layers);
        let background_image = style.background_image.take();
        let had_background = background_color.is_some_and(|color| color[3] != 0)
            || background_gradient.is_some()
            || background_radial_gradient.is_some()
            || background_conic_gradient.is_some()
            || !background_gradient_layers.is_empty()
            || background_image.is_some();
        style.background_color = had_background.then_some([255, 255, 255, 255]);
        let color = style.color;
        style.color = color.map(print_economy_color);
        let before_pseudo = style
            .before_pseudo
            .as_deref_mut()
            .map(Self::apply)
            .map(Box::new);
        let after_pseudo = style
            .after_pseudo
            .as_deref_mut()
            .map(Self::apply)
            .map(Box::new);
        Self {
            background_color,
            background_gradient,
            background_radial_gradient,
            background_conic_gradient,
            background_gradient_layers,
            background_image,
            color,
            before_pseudo,
            after_pseudo,
        }
    }

    fn restore(self, style: &mut crate::LayoutStyle) {
        style.background_color = self.background_color;
        style.background_gradient = self.background_gradient;
        style.background_radial_gradient = self.background_radial_gradient;
        style.background_conic_gradient = self.background_conic_gradient;
        style.background_gradient_layers = self.background_gradient_layers;
        style.background_image = self.background_image;
        style.color = self.color;
        if let (Some(snapshot), Some(pseudo)) =
            (self.before_pseudo, style.before_pseudo.as_deref_mut())
        {
            snapshot.restore(pseudo);
        }
        if let (Some(snapshot), Some(pseudo)) =
            (self.after_pseudo, style.after_pseudo.as_deref_mut())
        {
            snapshot.restore(pseudo);
        }
    }
}

/// Blink's print-economy foreground correction: colors too close to white
/// move one third down the HSV value axis while preserving hue and alpha.
pub(crate) fn print_economy_color(color: [u8; 4]) -> [u8; 4] {
    const MIN_DIFFERENCE_SQUARED: i32 = 65_025;
    let difference = |target: u8| -> i32 {
        color[..3]
            .iter()
            .map(|component| {
                let delta = i32::from(*component) - i32::from(target);
                delta * delta
            })
            .sum()
    };
    if difference(255) > MIN_DIFFERENCE_SQUARED {
        return color;
    }
    let max = color[0].max(color[1]).max(color[2]) as f32 / 255.0;
    if max <= f32::EPSILON {
        return color;
    }
    let scale = (max - 0.33).max(0.0) / max;
    let adjusted =
        |component: u8| ((f32::from(component) / 255.0 * scale * 256.0) as u16).min(255) as u8;
    [
        adjusted(color[0]),
        adjusted(color[1]),
        adjusted(color[2]),
        color[3],
    ]
}

/// A representative visible color for `background-clip: text` text whose own
/// color is transparent, used on the word-split paint path (the cosmic-text IFC
/// path samples the gradient per glyph in `inline`). Returns the gradient's mid
/// stop or the background color so a transparent-colored label still paints;
/// `None` when the element is not a transparent-text clip-to-text box.
fn clip_text_fill_color(style: &crate::LayoutStyle) -> Option<[u8; 4]> {
    if !style.background_clip_text {
        return None;
    }
    if style.color.map(|c| c[3] != 0).unwrap_or(true) {
        return None;
    }
    if let Some((_, stops)) = &style.background_gradient {
        if !stops.is_empty() {
            let mid = stops[stops.len() / 2].0;
            return Some([mid[0], mid[1], mid[2], 255]);
        }
    }
    style
        .background_color
        .filter(|c| c[3] != 0)
        .map(|c| [c[0], c[1], c[2], 255])
}

/// Paint every word of a text node at its own laid-out position. A text node
/// lays out as one taffy leaf per word (see `dom::build_text_words`), each
/// wrapping independently, so its content is a list of (box, word) pairs
/// rather than one box for the whole node; color/font/clip come from the
/// parent element and are the same for every word.
fn paint_text_node(
    tree: &DomTree,
    nid: obscura_dom::tree::NodeId,
    laid: &crate::DomLayout,
    scroll_state: &ScrollPaintState,
    pixmap: &mut Pixmap,
    raster_scale: f32,
) -> Option<()> {
    let runs = laid.text_runs.get(&nid)?;
    let parent = crate::dom::rendered_parent(tree, nid)?;
    let style = laid.styles.get(&parent)?;
    if style.effectively_invisible {
        return Some(());
    }
    let color =
        clip_text_fill_color(style).unwrap_or_else(|| style.color.unwrap_or([0, 0, 0, 255]));
    let fsize = style.font_size.unwrap_or(16.0);
    let is_bold = crate::style::used_font_weight(style) >= 600;
    // A text node has no transform of its own, but any transformed element
    // ancestor offsets it (the accumulation covers text nodes too). The clip
    // receives root-scroll/sticky movement without following the descendant's
    // own transform.
    let (ox, oy) = scroll_state.translation_for(laid, nid);
    let overflow_clip = scroll_state.overflow_clip_for(laid, nid);
    let clip = overflow_clip.as_ref().map(|clip| {
        clip.viewport_rect(scroll_state.surface_extent.unwrap_or(scroll_state.viewport))
    });
    let clip_mask = overflow_clip.as_ref().and_then(|clip| {
        overflow_clip_mask(
            pixmap.width(),
            pixmap.height(),
            clip,
            scroll_state.surface_extent.unwrap_or(scroll_state.viewport),
        )
    });

    for (rect, word) in runs {
        draw_text(
            pixmap,
            word,
            rect.x + ox,
            rect.y + oy,
            color,
            fsize,
            is_bold,
            style.font_family.as_deref(),
            style.letter_spacing.unwrap_or(0.0),
            clip,
            clip_mask.as_ref(),
            raster_scale,
        );
    }
    Some(())
}

fn fallback_font_bytes(family: Option<&str>) -> &'static [u8] {
    let Some(family) = family else {
        return FONT_BYTES;
    };
    for token in family.split(',') {
        let token = token
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_ascii_lowercase();
        if token == "system-ui" || token == "ui-sans-serif" {
            return SYSTEM_FONT_BYTES;
        }
        if token == "monospace"
            || token.contains("mono")
            || token.contains("courier")
            || token.contains("consol")
            || token == "menlo"
            || token == "monaco"
            || token == "code"
        {
            return MONO_FONT_BYTES;
        }
        if token == "serif"
            || token == "georgia"
            || token.contains("times")
            || token == "cambria"
            || token.contains("garamond")
            || token.contains("liberation serif")
            || token == "roman"
        {
            return SERIF_FONT_BYTES;
        }
        if token == "sans-serif"
            || token.contains("sans")
            || token == "arial"
            || token == "helvetica"
            || token == "helvetica neue"
            || token == "-apple-system"
            || token == "roboto"
            || token == "segoe ui"
            || token == "inter"
            || token == "verdana"
            || token == "tahoma"
        {
            return FONT_BYTES;
        }
    }
    FONT_BYTES
}

pub fn measure_text(text: &str, size: f32, is_bold: bool, family: Option<&str>) -> f32 {
    let font = FontRef::try_from_slice(fallback_font_bytes(family)).unwrap();
    let scale = PxScale::from(size);
    let scaled_font = font.as_scaled(scale);
    let mut width = 0.0;
    for c in text.chars() {
        if c.is_control() {
            continue;
        }
        width += scaled_font.h_advance(font.glyph_id(c));
    }
    if is_bold {
        width += text.chars().filter(|c| !c.is_control()).count() as f32;
    }
    width
}

fn draw_text(
    pixmap: &mut Pixmap,
    text: &str,
    x: f32,
    y: f32,
    color: [u8; 4],
    size: f32,
    is_bold: bool,
    family: Option<&str>,
    letter_spacing: f32,
    clip: Option<crate::Rect>,
    clip_mask: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    // A fully clipped-away run (the common "visually hidden" accessibility
    // pattern: a 1x1 box with overflow: hidden) paints nothing at all.
    if let Some(c) = clip {
        if c.width <= 0.0 || c.height <= 0.0 {
            return;
        }
    }
    let font = FontRef::try_from_slice(fallback_font_bytes(family)).unwrap();
    let scale = PxScale::from(size * raster_scale);
    let scaled_font = font.as_scaled(scale);
    let mut caret = ab_glyph::point(x * raster_scale, y * raster_scale + scaled_font.ascent());

    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let clip_bounds = clip.map(|c| {
        (
            c.x * raster_scale,
            c.y * raster_scale,
            (c.x + c.width) * raster_scale,
            (c.y + c.height) * raster_scale,
        )
    });
    let pixels = pixmap.pixels_mut();
    let (r, g, b, a_full) = (color[0], color[1], color[2], color[3]);

    for c in text.chars() {
        if c.is_control() {
            continue;
        }
        let glyph_id = font.glyph_id(c);
        let id = glyph_id;
        let glyph = glyph_id.with_scale_and_position(scale, caret);
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, c| {
                let px = (bounds.min.x + gx as f32) as i32;
                let py = (bounds.min.y + gy as f32) as i32;
                if let Some((cx0, cy0, cx1, cy1)) = clip_bounds {
                    if (px as f32) < cx0
                        || (px as f32) >= cx1
                        || (py as f32) < cy0
                        || (py as f32) >= cy1
                    {
                        return;
                    }
                }
                if px >= 0 && px < width && py >= 0 && py < height {
                    let alpha = (a_full as f32 * c) as u8;
                    if alpha > 0 {
                        let mut px_indices = vec![(py * width + px) as usize];
                        if is_bold {
                            for dx in 1..raster_scale.ceil().max(1.0) as i32 {
                                if px + dx < width {
                                    px_indices.push((py * width + px + dx) as usize);
                                }
                            }
                        }
                        for idx in px_indices {
                            let mask_alpha = clip_mask
                                .and_then(|mask| mask.data().get(idx))
                                .copied()
                                .unwrap_or(255) as u32;
                            let alpha = alpha as u32 * mask_alpha / 255;
                            if alpha == 0 {
                                continue;
                            }
                            let dst = pixels[idx];

                            let src_a = alpha;
                            let src_r = (r as u32 * src_a) / 255;
                            let src_g = (g as u32 * src_a) / 255;
                            let src_b = (b as u32 * src_a) / 255;

                            let dst_a = dst.alpha() as u32;
                            let out_a = src_a + (dst_a * (255 - src_a) / 255);

                            if out_a > 0 {
                                let out_r = src_r + (dst.red() as u32 * (255 - src_a) / 255);
                                let out_g = src_g + (dst.green() as u32 * (255 - src_a) / 255);
                                let out_b = src_b + (dst.blue() as u32 * (255 - src_a) / 255);

                                pixels[idx] = tiny_skia::PremultipliedColorU8::from_rgba(
                                    out_r as u8,
                                    out_g as u8,
                                    out_b as u8,
                                    out_a as u8,
                                )
                                .unwrap_or_else(|| {
                                    tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 0).unwrap()
                                });
                            }
                        }
                    }
                }
            });
            // Matches measure_text's +1px-per-character bold compensation:
            // without it, a word's reserved layout width (from measure_text)
            // is wider than what draw_text actually advances through, and
            // the difference shows up as a visible gap after every word once
            // each word is its own independently-positioned box.
            caret.x += scaled_font.h_advance(id)
                + if is_bold { raster_scale } else { 0.0 }
                + letter_spacing * raster_scale;
        } else {
            caret.x += scaled_font.h_advance(id)
                + if is_bold { raster_scale } else { 0.0 }
                + letter_spacing * raster_scale;
        }
    }
}

/// Resolve `src` (a `data:` URI, or an absolute/relative URL against
/// `base_url`) to raw bytes, fetching over the network at most once per
/// distinct URL through the retained resource cache.
fn fetch_bytes(
    src: &str,
    base_url: Option<&str>,
    cache: &mut RenderResourceCache,
) -> Option<Arc<[u8]>> {
    if let Some(rest) = src.strip_prefix("data:") {
        let comma_idx = rest.find(',')?;
        let (meta, data) = (&rest[..comma_idx], &rest[comma_idx + 1..]);
        // Data-backed SVGs and web fonts may be base64 or percent-escaped.
        // Decode from the encoding label rather than assuming every data URI
        // is base64.
        let bytes = if meta.contains("base64") {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.decode(data).ok()
        } else {
            Some(percent_decode(data))
        }?;
        return Some(Arc::from(bytes));
    }
    let resolved = resolve_resource_url(src, base_url)?;
    cache.get_or_load(&resolved)
}

fn fetch_profiled_image_bytes(
    src: &str,
    base_url: Option<&str>,
    cache: &mut RenderResourceCache,
    profile: ImageRequestProfile,
) -> Option<Arc<[u8]>> {
    if src.starts_with("data:") {
        return fetch_bytes(src, base_url, cache);
    }
    let resolved = resolve_resource_url(src, base_url)?;
    cache.get_or_load_image(&resolved, profile)
}

fn resolve_resource_url(src: &str, base_url: Option<&str>) -> Option<String> {
    if src.starts_with("data:") {
        return Some(src.to_string());
    }
    // Resolve relative to the document's base URL: the overwhelming majority
    // of real markup uses relative image paths ("logo.svg", not
    // "https://example.com/logo.svg"), so without this every relative <img>
    // or mask/background reference silently fails to fetch.
    if src.starts_with("http://") || src.starts_with("https://") {
        Some(src.to_string())
    } else if let Some(rest) = src.strip_prefix("//") {
        // Protocol-relative URL (`//upload.wikimedia.org/...`, ubiquitous on
        // Wikipedia and CDN-hosted media): inherit the document scheme, but
        // never `file:`/other non-network schemes (a `file://` base would give
        // `file://host/...` and fail), so default to https for those.
        let scheme = base_url
            .and_then(|b| url::Url::parse(b).ok())
            .map(|u| u.scheme().to_string())
            .filter(|s| s == "http" || s == "https")
            .unwrap_or_else(|| "https".to_string());
        Some(format!("{scheme}://{rest}"))
    } else {
        base_url
            .and_then(|b| url::Url::parse(b).ok())
            .and_then(|base| base.join(src).ok())
            .map(|u| u.to_string())
    }
}

/// Fetch the Latin/ASCII face from each authored `@font-face` rule and decode
/// WOFF/WOFF2 into the sfnt bytes consumed by fontdb/cosmic-text. Unicode-range
/// filtering is load-bearing for performance: generated font packages commonly
/// emit six or seven script subsets per face, while an English page needs only
/// the subset containing ASCII.
fn collect_web_fonts(
    tree: &DomTree,
    base_url: Option<&str>,
    cache: &mut RenderResourceCache,
    dynamic_fonts: &[DynamicFontFace],
) -> Vec<crate::inline::WebFont> {
    struct FontRule {
        sources: Vec<(String, String)>,
        family: Option<String>,
        weight: Option<(u16, u16)>,
        italic: Option<bool>,
    }

    let mut seen = std::collections::HashSet::new();
    let mut fonts = Vec::new();
    let mut rules = Vec::new();

    for nid in crate::dom::rendered_descendants(tree, tree.document()) {
        let Some(node) = tree.get_node(nid) else {
            continue;
        };
        if node
            .as_element()
            .map(|element| element.local.as_ref() != "style")
            .unwrap_or(true)
        {
            continue;
        }
        let css = tree.text_content(nid);
        for face in font_face_blocks(&css) {
            if !font_face_covers_ascii(face) {
                continue;
            }
            let sources: Vec<_> = font_face_urls(face)
                .into_iter()
                .filter(|src| font_source_may_be_supported(src))
                .map(|src| (font_resource_key(&src, base_url), src))
                .collect();
            if sources.is_empty() {
                continue;
            }
            rules.push(FontRule {
                sources,
                family: font_face_family(face),
                weight: font_face_weight(face),
                italic: font_face_italic(face),
            });
        }
    }
    for face in dynamic_fonts {
        let descriptor_block = format!(
            "src:{};font-weight:{};font-style:{};unicode-range:{}",
            face.source, face.weight, face.style, face.unicode_range
        );
        if !font_face_covers_ascii(&descriptor_block) {
            continue;
        }
        let sources: Vec<_> = font_face_urls(&descriptor_block)
            .into_iter()
            .filter(|src| font_source_may_be_supported(src))
            .map(|src| (font_resource_key(&src, base_url), src))
            .collect();
        if sources.is_empty() {
            continue;
        }
        rules.push(FontRule {
            sources,
            family: (!face.family.is_empty()).then(|| face.family.clone()),
            weight: font_face_weight(&descriptor_block),
            italic: font_face_italic(&descriptor_block),
        });
    }

    // Critical web fonts are normally preloaded from the document with a URL
    // already resolved relative to the HTML. Fetch those first, while retaining
    // the matching @font-face descriptors needed for CSS family/weight lookup.
    let mut preloads = Vec::new();
    for nid in crate::dom::rendered_descendants(tree, tree.document()) {
        let Some(node) = tree.get_node(nid) else {
            continue;
        };
        if node
            .as_element()
            .map(|element| element.local.as_ref() != "link")
            .unwrap_or(true)
        {
            continue;
        }
        let rel = node.get_attribute("rel").unwrap_or("");
        let as_value = node.get_attribute("as").unwrap_or("");
        if rel
            .split_ascii_whitespace()
            .any(|token| token.eq_ignore_ascii_case("preload"))
            && as_value.eq_ignore_ascii_case("font")
        {
            if let Some(href) = node.get_attribute("href") {
                preloads.push(href.to_string());
            }
        }
    }
    for src in preloads.iter().take(16) {
        let key = font_resource_key(src, base_url);
        if !seen.insert(key.clone()) {
            continue;
        }
        if let Some(decoded) = fetch_and_decode_font(src, base_url, cache) {
            let metadata = rules.iter().find(|rule| {
                rule.sources
                    .iter()
                    .any(|(source_key, _)| *source_key == key)
            });
            fonts.push(crate::inline::WebFont {
                data: decoded,
                family: metadata.and_then(|rule| rule.family.clone()),
                weight: metadata.and_then(|rule| rule.weight),
                italic: metadata.and_then(|rule| rule.italic),
            });
        }
    }

    for rule in rules {
        if fonts.len() >= 16 {
            break;
        }
        for (key, src) in rule.sources {
            if !seen.insert(key) {
                continue;
            }
            if let Some(decoded) = fetch_and_decode_font(&src, base_url, cache) {
                fonts.push(crate::inline::WebFont {
                    data: decoded,
                    family: rule.family,
                    weight: rule.weight,
                    italic: rule.italic,
                });
                break;
            }
        }
    }
    fonts
}

/// Exclude source formats that fontdb cannot consume before issuing a request.
/// Unknown and extensionless URLs are retained because their response bytes can
/// still identify a supported sfnt, WOFF, or WOFF2 resource.
fn font_source_may_be_supported(src: &str) -> bool {
    let path = src
        .split(['?', '#'])
        .next()
        .unwrap_or(src)
        .to_ascii_lowercase();
    !path.ends_with(".eot") && !path.ends_with(".svg")
}

fn font_resource_key(src: &str, base_url: Option<&str>) -> String {
    url::Url::parse(src)
        .ok()
        .or_else(|| {
            base_url
                .and_then(|base| url::Url::parse(base).ok())
                .and_then(|base| base.join(src).ok())
        })
        .map(|url| url.to_string())
        .unwrap_or_else(|| src.to_string())
}

fn fetch_and_decode_font(
    src: &str,
    base_url: Option<&str>,
    cache: &mut RenderResourceCache,
) -> Option<Vec<u8>> {
    let compressed = fetch_bytes(src, base_url, cache)?;
    if compressed.len() > 8 * 1024 * 1024 {
        return None;
    }
    let decoded = match compressed.get(..4) {
        Some(b"wOF2") => wuff::decompress_woff2(&compressed).ok(),
        Some(b"wOFF") => wuff::decompress_woff1(&compressed).ok(),
        // TrueType/OpenType collections and raw sfnt fonts already have the
        // representation fontdb expects.
        Some(b"\0\x01\0\0" | b"OTTO" | b"ttcf") => Some(compressed.as_ref().to_vec()),
        _ => None,
    }?;
    (decoded.len() <= 32 * 1024 * 1024).then_some(decoded)
}

fn font_face_blocks(css: &str) -> Vec<&str> {
    let lower = css.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find("@font-face") {
        let at = cursor + relative;
        let Some(open_relative) = lower[at..].find('{') else {
            break;
        };
        let open = at + open_relative;
        let mut depth = 1i32;
        let mut quote = None;
        let mut escaped = false;
        let mut close = None;
        for (offset, ch) in css[open + 1..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if let Some(active) = quote {
                if ch == active {
                    quote = None;
                }
                continue;
            }
            if matches!(ch, '"' | '\'') {
                quote = Some(ch);
                continue;
            }
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + 1 + offset);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { break };
        out.push(&css[open + 1..close]);
        cursor = close + 1;
    }
    out
}

fn font_face_declaration<'a>(face: &'a str, name: &str) -> Option<&'a str> {
    split_css_top_level(face, ';')
        .into_iter()
        .filter_map(|declaration| {
            let (property, value) = declaration.split_once(':')?;
            property
                .trim()
                .eq_ignore_ascii_case(name)
                .then_some(value.trim())
        })
        .last()
}

fn font_face_family(face: &str) -> Option<String> {
    font_face_declaration(face, "font-family")
        .map(|family| {
            family
                .trim()
                .trim_matches(|ch| matches!(ch, '"' | '\''))
                .to_string()
        })
        .filter(|family| !family.is_empty())
}

fn font_face_weight(face: &str) -> Option<(u16, u16)> {
    fn parse(value: &str) -> Option<u16> {
        match value.to_ascii_lowercase().as_str() {
            "normal" => Some(400),
            "bold" => Some(700),
            value => value
                .parse::<f32>()
                .ok()
                .filter(|weight| weight.is_finite() && (1.0..=1000.0).contains(weight))
                .map(|weight| weight.round() as u16),
        }
    }
    let mut values = font_face_declaration(face, "font-weight")?
        .split_ascii_whitespace()
        .filter_map(parse);
    let first = values.next()?;
    let second = values.next().unwrap_or(first);
    Some((first.min(second), first.max(second)))
}

fn font_face_italic(face: &str) -> Option<bool> {
    font_face_declaration(face, "font-style").and_then(|style| {
        let style = style.trim().to_ascii_lowercase();
        if style == "normal" {
            Some(false)
        } else if style == "italic" || style.starts_with("oblique") {
            Some(true)
        } else {
            None
        }
    })
}

fn font_face_covers_ascii(face: &str) -> bool {
    let Some(range) = font_face_declaration(face, "unicode-range") else {
        return true;
    };
    range.split(',').any(|part| {
        let token = part.trim().to_ascii_lowercase();
        let Some(value) = token.strip_prefix("u+") else {
            return false;
        };
        let (start, end) = if value.contains('?') {
            (
                u32::from_str_radix(&value.replace('?', "0"), 16).ok(),
                u32::from_str_radix(&value.replace('?', "f"), 16).ok(),
            )
        } else if let Some((start, end)) = value.split_once('-') {
            (
                u32::from_str_radix(start, 16).ok(),
                u32::from_str_radix(end, 16).ok(),
            )
        } else {
            let point = u32::from_str_radix(value, 16).ok();
            (point, point)
        };
        matches!((start, end), (Some(start), Some(end)) if start <= 0x7e && end >= 0x20)
    })
}

fn font_face_urls(face: &str) -> Vec<String> {
    let Some(src) = font_face_declaration(face, "src") else {
        return Vec::new();
    };
    let lower = src.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find("url(") {
        let start = cursor + relative + 4;
        let Some(end_relative) = src[start..].find(')') else {
            break;
        };
        let end = start + end_relative;
        let value = src[start..end]
            .trim()
            .trim_matches(|ch| ch == '"' || ch == '\'')
            .trim();
        if !value.is_empty() {
            out.push(value.to_string());
        }
        cursor = end + 1;
    }
    out
}

fn split_css_top_level(value: &str, separator: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '"' | '\'') {
            quote = Some(ch);
            continue;
        }
        match ch {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            ch if ch == separator && depth == 0 => {
                out.push(&value[start..index]);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&value[start..]);
    out
}

/// Fetch `url` with a descriptive User-Agent and a bounded timeout, retrying on
/// rate-limit / transient errors with backoff. Real pages pull dozens of images
/// from one CDN in a burst (a Wikipedia article references ~60); hosts like
/// Wikimedia answer a rapid burst with HTTP 429 after ~10 requests. Without a
/// retry the rate-limited images (e.g. an infobox photo montage fetched late in
/// the burst) came back blank, and the failure was cached permanently. The
/// backoff both recovers them and paces the burst back under the limit.
fn http_get_bytes(url: &str) -> Option<Vec<u8>> {
    let mut backoff = std::time::Duration::from_millis(200);
    for attempt in 0..3 {
        // Advertise only formats that this build can decode. Content-negotiating
        // CDNs otherwise commonly choose AVIF and leave the image blank.
        let res = image_agent().get(url).set("Accept", IMAGE_ACCEPT).call();
        match res {
            Ok(resp) => {
                let mut buf = Vec::new();
                use std::io::Read;
                return resp.into_reader().read_to_end(&mut buf).ok().map(|_| buf);
            }
            // 429 (rate limit) and 5xx are transient: a short backoff clears a
            // brief blip. A sustained limit (Wikimedia 429s a 60-image burst
            // from a datacenter IP hard, with `Retry-After: 1`) is NOT worth
            // waiting out here: honoring the hint stalls the whole render for
            // minutes, so fast-fail to the grey placeholder instead. Real
            // fidelity for that case needs an HTTP/2 image client (multiplexing
            // like Chrome), not blocking retries.
            Err(ureq::Error::Status(code, _))
                if matches!(code, 429 | 500 | 502 | 503 | 504) && attempt < 2 =>
            {
                std::thread::sleep(backoff);
                backoff *= 2;
            }
            Err(ureq::Error::Transport(_)) if attempt < 2 => {
                std::thread::sleep(backoff);
                backoff *= 2;
            }
            Err(_) => return None,
        }
    }
    None
}

/// One shared HTTP agent for all image fetches in the process, with a browser
/// User-Agent and keep-alive connection pooling. A CDN's bot rate-limiter keys
/// on connection churn as much as on rate: a fresh TLS handshake per image (the
/// old per-call `ureq::get`) reads as a burst and gets 429'd, whereas reusing
/// one pooled connection to the same host (as a browser does) both avoids most
/// throttling and is much faster on an image-heavy page.
fn image_agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            // Present the same normal browser identity the engine uses for the
            // document. A bot-identifying UA got image requests filtered by CDNs
            // that gate on User-Agent (Akamai/Cloudflare image endpoints on
            // cnbc, techcrunch, arstechnica), so the images Chrome loads came
            // back blank; a real browser UA loads the same bytes Chrome does.
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36")
            .build()
    })
}

/// Decode a percent-escaped data: URI payload (`%23` -> `#`, etc). Bytes that
/// are not part of a `%XX` escape pass through unchanged, which is exactly
/// right for the inline-SVG case: only the characters that would otherwise be
/// ambiguous in a URI (`#`, `"`, ...) get escaped, everything else is literal
/// UTF-8 text.
fn percent_decode(s: &str) -> Vec<u8> {
    // Operates on raw bytes throughout (never slices `s` as a string): a
    // stray '%' followed by non-hex bytes could otherwise land a string
    // slice in the middle of a multi-byte UTF-8 character and panic.
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Decode raster image bytes (GIF/JPEG/PNG/WebP) to a premultiplied-alpha pixmap
/// resized to `w`x`h`.
fn raster_to_pixmap(bytes: &[u8], w: u32, h: u32) -> Option<Pixmap> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let resized = image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle);
    let mut raw = resized.into_raw();
    for pixel in raw.chunks_exact_mut(4) {
        let a = pixel[3] as u32;
        pixel[0] = ((pixel[0] as u32 * a) / 255) as u8;
        pixel[1] = ((pixel[1] as u32 * a) / 255) as u8;
        pixel[2] = ((pixel[2] as u32 * a) / 255) as u8;
    }
    let size = tiny_skia::IntSize::from_wh(w, h)?;
    Pixmap::from_vec(raw, size)
}

/// Read an image's intrinsic pixel dimensions from its header only, without
/// decoding the whole thing. Returns None for formats the raster decoder does
/// not recognize (e.g. SVG, which is sized elsewhere).
/// Fill `path` with a CSS `linear-gradient`. `angle` is degrees clockwise from
/// 12 o'clock (0 = to top). The gradient line length uses the CSS formula so
/// the stops land where a browser puts them. Positionless stops are spread
/// evenly; positions are clamped monotonic (tiny-skia requires ascending).
fn paint_linear_gradient(
    pixmap: &mut Pixmap,
    path: &tiny_skia::Path,
    rect: &crate::Rect,
    angle: f32,
    stops: &[([u8; 4], Option<f32>)],
    clip: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    if stops.len() < 2 {
        return;
    }
    let rad = angle.to_radians();
    let dx = rad.sin();
    let dy = -rad.cos();
    let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let half = (dx.abs() * rect.width + dy.abs() * rect.height) / 2.0;
    let start = Point::from_xy(cx - dx * half, cy - dy * half);
    let end = Point::from_xy(cx + dx * half, cy + dy * half);
    let n = stops.len();
    let mut gs: Vec<GradientStop> = Vec::with_capacity(n);
    let mut last = 0.0f32;
    for (i, (_, pos)) in stops.iter().enumerate() {
        let c = gradient_stop_color(stops, i);
        let p = pos
            .unwrap_or(i as f32 / (n - 1) as f32)
            .clamp(0.0, 1.0)
            .max(last);
        last = p;
        gs.push(GradientStop::new(
            p,
            Color::from_rgba8(c[0], c[1], c[2], c[3]),
        ));
    }
    if let Some(shader) =
        LinearGradient::new(start, end, gs, SpreadMode::Pad, Transform::identity())
    {
        let mut paint = Paint::default();
        paint.shader = shader;
        paint.anti_alias = true;
        pixmap.fill_path(
            path,
            &paint,
            FillRule::Winding,
            raster_transform(raster_scale),
            clip,
        );
    }
}

fn paint_linear_gradient_layer(
    pixmap: &mut Pixmap,
    path: &tiny_skia::Path,
    rect: &crate::Rect,
    angle: f32,
    stops: &[([u8; 4], Option<f32>)],
    stop_positions: &[Option<String>],
    repeating: bool,
    em: f32,
    rem: f32,
    viewport: (f32, f32),
    clip: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    if stops.len() < 2 {
        return;
    }
    let rad = angle.to_radians();
    let dx = rad.sin();
    let dy = -rad.cos();
    let (cx, cy) = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let half = (dx.abs() * rect.width + dy.abs() * rect.height) / 2.0;
    let line_length = half * 2.0;
    if line_length <= f32::EPSILON {
        return;
    }
    let base_start = Point::from_xy(cx - dx * half, cy - dy * half);
    let base_end = Point::from_xy(cx + dx * half, cy + dy * half);
    let mut positions: Vec<Option<f32>> = stops
        .iter()
        .enumerate()
        .map(|(index, (_, legacy))| {
            stop_positions
                .get(index)
                .and_then(Option::as_deref)
                .and_then(|value| {
                    crate::style::resolve_contextual_length(
                        value,
                        em,
                        rem,
                        viewport.0 / 100.0,
                        viewport.1 / 100.0,
                        line_length,
                    )
                    .map(|pixels| pixels / line_length)
                })
                .or(*legacy)
        })
        .collect();
    if positions.first().is_some_and(Option::is_none) {
        positions[0] = Some(0.0);
    }
    let last_index = positions.len() - 1;
    if positions[last_index].is_none() {
        positions[last_index] = Some(1.0);
    }
    let mut previous = 0usize;
    for index in 1..positions.len() {
        let Some(mut position) = positions[index] else {
            continue;
        };
        let previous_position = positions[previous].unwrap_or(0.0);
        position = position.max(previous_position);
        positions[index] = Some(position);
        let gap = index - previous;
        for offset in 1..gap {
            positions[previous + offset] = Some(
                previous_position + (position - previous_position) * offset as f32 / gap as f32,
            );
        }
        previous = index;
    }
    let first = positions[0].unwrap_or(0.0);
    let last = positions[last_index].unwrap_or(first);
    if repeating && last - first <= 1e-6 {
        let color = gradient_stop_color(stops, last_index);
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
        pixmap.fill_path(
            path,
            &paint,
            FillRule::Winding,
            raster_transform(raster_scale),
            clip,
        );
        return;
    }
    let (start, end, spread, normalize) = if repeating {
        (
            Point::from_xy(
                base_start.x + (base_end.x - base_start.x) * first,
                base_start.y + (base_end.y - base_start.y) * first,
            ),
            Point::from_xy(
                base_start.x + (base_end.x - base_start.x) * last,
                base_start.y + (base_end.y - base_start.y) * last,
            ),
            SpreadMode::Repeat,
            Some((first, last - first)),
        )
    } else {
        (base_start, base_end, SpreadMode::Pad, None)
    };
    let mut gradient_stops = Vec::with_capacity(stops.len());
    let mut monotonic = 0.0f32;
    for (index, _) in stops.iter().enumerate() {
        let color = gradient_stop_color(stops, index);
        let position = match normalize {
            Some((origin, span)) => {
                ((positions[index].unwrap_or(origin) - origin) / span).clamp(0.0, 1.0)
            }
            None => positions[index].unwrap_or(0.0).clamp(0.0, 1.0),
        }
        .max(monotonic);
        monotonic = position;
        gradient_stops.push(GradientStop::new(
            position,
            Color::from_rgba8(color[0], color[1], color[2], color[3]),
        ));
    }
    if let Some(shader) =
        LinearGradient::new(start, end, gradient_stops, spread, Transform::identity())
    {
        let mut paint = Paint::default();
        paint.shader = shader;
        paint.anti_alias = true;
        pixmap.fill_path(
            path,
            &paint,
            FillRule::Winding,
            raster_transform(raster_scale),
            clip,
        );
    }
}

#[derive(Clone, Copy)]
struct BackgroundGeometry {
    origin_rect: crate::Rect,
    clip_rect: crate::Rect,
    clip_radii: crate::ResolvedBorderRadii,
}

fn inset_rect(rect: &crate::Rect, insets: crate::Sides<f32>) -> crate::Rect {
    crate::Rect {
        x: rect.x + insets.left,
        y: rect.y + insets.top,
        width: (rect.width - insets.left - insets.right).max(0.0),
        height: (rect.height - insets.top - insets.bottom).max(0.0),
    }
}

fn add_sides(a: crate::Sides<f32>, b: crate::Sides<f32>) -> crate::Sides<f32> {
    crate::Sides {
        top: a.top + b.top,
        right: a.right + b.right,
        bottom: a.bottom + b.bottom,
        left: a.left + b.left,
    }
}

fn background_geometry(rect: &crate::Rect, style: &crate::LayoutStyle) -> BackgroundGeometry {
    let border = crate::Sides {
        top: style.border.top,
        right: style.border.right,
        bottom: style.border.bottom,
        left: style.border.left,
    };
    let padding = crate::Sides {
        top: style.padding.top,
        right: style.padding.right,
        bottom: style.padding.bottom,
        left: style.padding.left,
    };
    let content = add_sides(border, padding);
    let origin_insets = match style.background_origin {
        crate::BackgroundOrigin::BorderBox => crate::Sides::all(0.0),
        crate::BackgroundOrigin::PaddingBox => border,
        crate::BackgroundOrigin::ContentBox => content,
    };
    let clip_insets = match style.background_clip {
        crate::BackgroundClip::BorderBox | crate::BackgroundClip::Text => crate::Sides::all(0.0),
        crate::BackgroundClip::PaddingBox => border,
        crate::BackgroundClip::ContentBox => content,
    };
    let outer_radii = style.border_model.radii.resolve(rect.width, rect.height);
    BackgroundGeometry {
        origin_rect: inset_rect(rect, origin_insets),
        clip_rect: inset_rect(rect, clip_insets),
        clip_radii: outer_radii.inset(clip_insets),
    }
}

fn background_clip_path(geometry: BackgroundGeometry) -> Option<tiny_skia::Path> {
    if geometry.clip_rect.width <= 0.0 || geometry.clip_rect.height <= 0.0 {
        return None;
    }
    if !geometry.clip_radii.is_zero() {
        return rounded_rect_path_radii(
            geometry.clip_rect.x,
            geometry.clip_rect.y,
            geometry.clip_rect.width,
            geometry.clip_rect.height,
            geometry.clip_radii,
        );
    }
    Rect::from_xywh(
        geometry.clip_rect.x,
        geometry.clip_rect.y,
        geometry.clip_rect.width,
        geometry.clip_rect.height,
    )
    .and_then(|rect| {
        let mut builder = PathBuilder::new();
        builder.push_rect(rect);
        builder.finish()
    })
}

fn background_extra_clip(
    ancestor_clip: Option<&tiny_skia::Mask>,
    polygon_clip: Option<&tiny_skia::Mask>,
) -> Option<tiny_skia::Mask> {
    intersect_clip_masks(ancestor_clip.cloned(), polygon_clip)
}

fn paint_radial_gradient(
    pixmap: &mut Pixmap,
    path: &tiny_skia::Path,
    rect: &crate::Rect,
    center: (f32, f32),
    stops: &[([u8; 4], Option<f32>)],
    geometry: Option<crate::RadialGradientGeometry>,
    em: f32,
    root_font_size: f32,
    viewport: (f32, f32),
    clip: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    if stops.len() < 2 {
        return;
    }
    let center = Point::from_xy(
        rect.x + rect.width * center.0,
        rect.y + rect.height * center.1,
    );
    let Some((radius_x, radius_y)) =
        resolve_radial_gradient_radii(rect, center, geometry, em, root_font_size, viewport)
    else {
        return;
    };
    let normalized = normalized_stops(stops);
    let gradient_stops = normalized
        .into_iter()
        .map(|(position, color)| {
            GradientStop::new(
                position,
                Color::from_rgba8(color[0], color[1], color[2], color[3]),
            )
        })
        .collect();
    // tiny-skia's native radial shader is circular. Keep the established CSS
    // coordinate-space shader and transform that circle around its center into
    // the authored ellipse. The paint transform later handles device scale.
    let ellipse_scale = radius_y / radius_x;
    let gradient_transform = Transform::from_row(
        1.0,
        0.0,
        0.0,
        ellipse_scale,
        0.0,
        center.y * (1.0 - ellipse_scale),
    );
    if let Some(shader) = RadialGradient::new(
        center,
        0.0,
        center,
        radius_x,
        gradient_stops,
        SpreadMode::Pad,
        gradient_transform,
    ) {
        let mut paint = Paint::default();
        paint.shader = shader;
        paint.anti_alias = true;
        pixmap.fill_path(
            path,
            &paint,
            FillRule::Winding,
            raster_transform(raster_scale),
            clip,
        );
    }
}

fn resolve_radial_gradient_radii(
    rect: &crate::Rect,
    center: Point,
    geometry: Option<crate::RadialGradientGeometry>,
    em: f32,
    root_font_size: f32,
    viewport: (f32, f32),
) -> Option<(f32, f32)> {
    use crate::{RadialGradientShape as Shape, RadialGradientSize as Size};

    let left = (center.x - rect.x).abs();
    let right = (rect.x + rect.width - center.x).abs();
    let top = (center.y - rect.y).abs();
    let bottom = (rect.y + rect.height - center.y).abs();

    // Programmatically constructed legacy `BackgroundGradientLayer::Radial`
    // values have no sidecar geometry. Preserve their former circular
    // farthest-corner behavior exactly.
    let Some(geometry) = geometry else {
        let radius = [
            left.hypot(top),
            right.hypot(top),
            left.hypot(bottom),
            right.hypot(bottom),
        ]
        .into_iter()
        .fold(0.0, f32::max);
        return (radius > f32::EPSILON).then_some((radius, radius));
    };

    let (mut radius_x, mut radius_y) = match geometry.size {
        Size::ClosestSide => (left.min(right), top.min(bottom)),
        Size::FarthestSide => (left.max(right), top.max(bottom)),
        Size::ClosestCorner => {
            let sqrt_two = 2.0f32.sqrt();
            (left.min(right) * sqrt_two, top.min(bottom) * sqrt_two)
        }
        Size::FarthestCorner => {
            let sqrt_two = 2.0f32.sqrt();
            (left.max(right) * sqrt_two, top.max(bottom) * sqrt_two)
        }
        Size::Explicit(x, y) => (
            resolve_radial_radius(x, rect.width, em, root_font_size, viewport)?,
            resolve_radial_radius(y, rect.height, em, root_font_size, viewport)?,
        ),
    };
    if geometry.shape == Shape::Circle && !matches!(geometry.size, Size::Explicit(..)) {
        let radius = match geometry.size {
            Size::ClosestSide => radius_x.min(radius_y),
            Size::FarthestSide => radius_x.max(radius_y),
            Size::ClosestCorner => left.min(right).hypot(top.min(bottom)),
            Size::FarthestCorner => left.max(right).hypot(top.max(bottom)),
            Size::Explicit(..) => unreachable!(),
        };
        radius_x = radius;
        radius_y = radius;
    }
    (radius_x > f32::EPSILON && radius_y > f32::EPSILON).then_some((radius_x, radius_y))
}

fn resolve_radial_radius(
    radius: crate::Dimension,
    percentage_basis: f32,
    em: f32,
    root_font_size: f32,
    viewport: (f32, f32),
) -> Option<f32> {
    match radius.resolve(em, root_font_size, viewport.0 / 100.0, viewport.1 / 100.0) {
        crate::Dimension::Px(value) => Some(value),
        crate::Dimension::Percent(value) => Some(value * percentage_basis),
        _ => None,
    }
    .filter(|value| value.is_finite() && *value >= 0.0)
}

fn paint_background_gradient_layers(
    pixmap: &mut Pixmap,
    path: &tiny_skia::Path,
    origin_rect: &crate::Rect,
    clip_rect: &crate::Rect,
    clip_radii: crate::ResolvedBorderRadii,
    style: &crate::LayoutStyle,
    root_font_size: f32,
    viewport: (f32, f32),
    clip: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    let layers = &style.background_gradient_layers;
    let em = style.font_size.unwrap_or(16.0);
    let tile_size = background_gradient_tile_size(style, origin_rect, em, root_font_size, viewport);
    let clip_differs_from_origin = (clip_rect.x - origin_rect.x).abs() > 0.01
        || (clip_rect.y - origin_rect.y).abs() > 0.01
        || (clip_rect.width - origin_rect.width).abs() > 0.01
        || (clip_rect.height - origin_rect.height).abs() > 0.01;
    let needs_tile = (tile_size.0 - origin_rect.width).abs() > 0.01
        || (tile_size.1 - origin_rect.height).abs() > 0.01
        // Even a default-sized image must be treated as a tile when the
        // painting area extends beyond its positioning area.  Filling the
        // clip directly would incorrectly stretch/pad the authored gradient
        // coordinates and would also ignore `background-repeat: no-repeat`.
        || clip_differs_from_origin;
    if needs_tile && tile_size.0 > 0.0 && tile_size.1 > 0.0 {
        debug_assert_eq!(raster_scale, 1.0);
        let width = tile_size.0.ceil().clamp(1.0, 4096.0) as u32;
        let height = tile_size.1.ceil().clamp(1.0, 4096.0) as u32;
        if let Some(mut tile) = Pixmap::new(width, height) {
            let tile_rect = crate::Rect {
                x: 0.0,
                y: 0.0,
                width: width as f32,
                height: height as f32,
            };
            if let Some(tile_path) = Rect::from_xywh(0.0, 0.0, tile_rect.width, tile_rect.height)
                .and_then(|rect| {
                    let mut builder = PathBuilder::new();
                    builder.push_rect(rect);
                    builder.finish()
                })
            {
                paint_gradient_layer_stack(
                    &mut tile,
                    &tile_path,
                    &tile_rect,
                    &tile_rect,
                    crate::ResolvedBorderRadii::default(),
                    layers,
                    &style.background_gradient_layer_radial_geometries,
                    em,
                    root_font_size,
                    viewport,
                    None,
                    1.0,
                );
                let tile_x = origin_rect.x
                    + style
                        .background_position
                        .x
                        .resolve(origin_rect.width - tile_size.0);
                let tile_y = origin_rect.y
                    + style
                        .background_position
                        .y
                        .resolve(origin_rect.height - tile_size.1);
                let repeats = style.background_repeat.unwrap_or((true, true));
                if repeats == (true, true) {
                    let mut paint = Paint::default();
                    paint.shader = Pattern::new(
                        tile.as_ref(),
                        SpreadMode::Repeat,
                        FilterQuality::Nearest,
                        1.0,
                        Transform::from_translate(tile_x, tile_y),
                    );
                    pixmap.fill_path(path, &paint, FillRule::Winding, Transform::identity(), clip);
                    return;
                }

                let mut owner_clip = clip.cloned().or_else(|| {
                    let mut mask = tiny_skia::Mask::new(pixmap.width(), pixmap.height())?;
                    mask.fill_path(path, FillRule::Winding, true, Transform::identity());
                    Some(mask)
                });
                if clip.is_some() {
                    if let Some(mask) = owner_clip.as_mut() {
                        mask.intersect_path(path, FillRule::Winding, true, Transform::identity());
                    }
                }
                let start_x = if repeats.0 {
                    tile_x - ((tile_x - clip_rect.x) / width as f32).ceil() * width as f32
                } else {
                    tile_x
                };
                let start_y = if repeats.1 {
                    tile_y - ((tile_y - clip_rect.y) / height as f32).ceil() * height as f32
                } else {
                    tile_y
                };
                let end_x = if repeats.0 {
                    clip_rect.x + clip_rect.width
                } else {
                    tile_x + 0.5
                };
                let end_y = if repeats.1 {
                    clip_rect.y + clip_rect.height
                } else {
                    tile_y + 0.5
                };
                let mut y = start_y;
                while y < end_y {
                    let mut x = start_x;
                    while x < end_x {
                        pixmap.draw_pixmap(
                            x.floor() as i32,
                            y.floor() as i32,
                            tile.as_ref(),
                            &tiny_skia::PixmapPaint::default(),
                            Transform::identity(),
                            owner_clip.as_ref(),
                        );
                        if !repeats.0 {
                            break;
                        }
                        x += width as f32;
                    }
                    if !repeats.1 {
                        break;
                    }
                    y += height as f32;
                }
                return;
            }
        }
    }

    paint_gradient_layer_stack(
        pixmap,
        path,
        origin_rect,
        clip_rect,
        clip_radii,
        layers,
        &style.background_gradient_layer_radial_geometries,
        em,
        root_font_size,
        viewport,
        clip,
        raster_scale,
    );
}

fn paint_gradient_layer_stack(
    pixmap: &mut Pixmap,
    path: &tiny_skia::Path,
    sampling_rect: &crate::Rect,
    clip_rect: &crate::Rect,
    clip_radii: crate::ResolvedBorderRadii,
    layers: &[crate::BackgroundGradientLayer],
    radial_geometries: &[Option<crate::RadialGradientGeometry>],
    em: f32,
    root_font_size: f32,
    viewport: (f32, f32),
    clip: Option<&tiny_skia::Mask>,
    raster_scale: f32,
) {
    // CSS lists the topmost background first. Paint back-to-front so every
    // translucent layer composites over the layers authored after it.
    for (index, layer) in layers.iter().enumerate().rev() {
        match layer {
            crate::BackgroundGradientLayer::Linear {
                angle,
                stops,
                stop_positions,
                repeating,
            } => {
                paint_linear_gradient_layer(
                    pixmap,
                    path,
                    sampling_rect,
                    *angle,
                    stops,
                    stop_positions,
                    *repeating,
                    em,
                    root_font_size,
                    viewport,
                    clip,
                    raster_scale,
                );
            }
            crate::BackgroundGradientLayer::Radial { center, stops } => {
                paint_radial_gradient(
                    pixmap,
                    path,
                    sampling_rect,
                    *center,
                    stops,
                    radial_geometries.get(index).copied().flatten(),
                    em,
                    root_font_size,
                    viewport,
                    clip,
                    raster_scale,
                );
            }
            crate::BackgroundGradientLayer::Conic {
                angle,
                center,
                stops,
            } => {
                paint_conic_gradient_sampled(
                    pixmap,
                    clip_rect,
                    sampling_rect,
                    clip_radii,
                    *angle,
                    *center,
                    stops,
                    clip,
                );
            }
        }
    }
}

fn background_gradient_tile_size(
    style: &crate::LayoutStyle,
    rect: &crate::Rect,
    em: f32,
    rem: f32,
    viewport: (f32, f32),
) -> (f32, f32) {
    if matches!(
        style.background_size_fit,
        Some(crate::ObjectFit::Cover | crate::ObjectFit::Contain)
    ) {
        return (rect.width, rect.height);
    }
    if let Some(expression) = style.background_size_expression.as_deref() {
        let components = split_background_size_components(expression);
        let resolve = |value: &&str, basis: f32| {
            (!value.eq_ignore_ascii_case("auto"))
                .then(|| {
                    crate::style::resolve_contextual_length(
                        value,
                        em,
                        rem,
                        viewport.0 / 100.0,
                        viewport.1 / 100.0,
                        basis,
                    )
                })
                .flatten()
        };
        let width = components
            .first()
            .and_then(|value| resolve(value, rect.width))
            .unwrap_or(rect.width);
        let height = components
            .get(1)
            .and_then(|value| resolve(value, rect.height))
            .unwrap_or(rect.height);
        return (width, height);
    }
    style.background_size.unwrap_or((rect.width, rect.height))
}

fn paint_conic_gradient_sampled(
    pixmap: &mut Pixmap,
    rect: &crate::Rect,
    sampling_rect: &crate::Rect,
    border_radius: crate::ResolvedBorderRadii,
    angle: f32,
    center: (f32, f32),
    stops: &[([u8; 4], Option<f32>)],
    extra_clip: Option<&tiny_skia::Mask>,
) {
    if rect.width <= 0.0 || rect.height <= 0.0 || stops.len() < 2 {
        return;
    }
    let width = rect.width.ceil() as u32;
    let height = rect.height.ceil() as u32;
    let Some(mut layer) = Pixmap::new(width, height) else {
        return;
    };
    let normalized = normalized_stops(stops);
    for y in 0..height {
        for x in 0..width {
            let color = conic_color_at(
                sampling_rect,
                angle,
                center,
                &normalized,
                rect.x + x as f32 + 0.5,
                rect.y + y as f32 + 0.5,
            );
            layer.pixels_mut()[(y * width + x) as usize] = premultiplied(color);
        }
    }
    let mut clip = extra_clip.cloned();
    if !border_radius.is_zero() {
        let path = rounded_rect_path_radii(rect.x, rect.y, rect.width, rect.height, border_radius);
        match (clip.as_mut(), path) {
            (Some(mask), Some(path)) => {
                mask.intersect_path(&path, FillRule::Winding, true, Transform::identity());
            }
            (None, _) => {
                clip = rounded_box_clip_mask_radii(
                    pixmap.width(),
                    pixmap.height(),
                    rect,
                    border_radius,
                );
            }
            _ => {}
        }
    }
    pixmap.draw_pixmap(
        rect.x.floor() as i32,
        rect.y.floor() as i32,
        layer.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        Transform::identity(),
        clip.as_ref(),
    );
}

fn normalized_stops(stops: &[([u8; 4], Option<f32>)]) -> Vec<(f32, [u8; 4])> {
    let count = stops.len();
    let mut normalized = Vec::with_capacity(count);
    let mut last = 0.0f32;
    for (index, (_, position)) in stops.iter().enumerate() {
        let color = gradient_stop_color(stops, index);
        let position = position
            .unwrap_or_else(|| {
                if count <= 1 {
                    0.0
                } else {
                    index as f32 / (count - 1) as f32
                }
            })
            .clamp(0.0, 1.0)
            .max(last);
        last = position;
        normalized.push((position, color));
    }
    normalized
}

fn gradient_stop_color(stops: &[([u8; 4], Option<f32>)], index: usize) -> [u8; 4] {
    let color = stops[index].0;
    if color[3] != 0 {
        return color;
    }
    let neighbor = stops[index + 1..]
        .iter()
        .find(|(candidate, _)| candidate[3] != 0)
        .or_else(|| {
            stops[..index]
                .iter()
                .rev()
                .find(|(candidate, _)| candidate[3] != 0)
        });
    neighbor
        .map(|(neighbor, _)| [neighbor[0], neighbor[1], neighbor[2], 0])
        .unwrap_or(color)
}

fn sample_normalized_stops(stops: &[(f32, [u8; 4])], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    let Some(&(first_position, first_color)) = stops.first() else {
        return [0, 0, 0, 0];
    };
    if t <= first_position {
        return first_color;
    }
    for pair in stops.windows(2) {
        let (start_position, start_color) = pair[0];
        let (end_position, end_color) = pair[1];
        if t <= end_position {
            let span = end_position - start_position;
            let fraction = if span <= f32::EPSILON {
                1.0
            } else {
                ((t - start_position) / span).clamp(0.0, 1.0)
            };
            let interpolate = |start: u8, end: u8| {
                (start as f32 + (end as f32 - start as f32) * fraction)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            return [
                interpolate(start_color[0], end_color[0]),
                interpolate(start_color[1], end_color[1]),
                interpolate(start_color[2], end_color[2]),
                interpolate(start_color[3], end_color[3]),
            ];
        }
    }
    stops.last().map(|(_, color)| *color).unwrap_or(first_color)
}

fn conic_color_at(
    rect: &crate::Rect,
    angle: f32,
    center: (f32, f32),
    stops: &[(f32, [u8; 4])],
    x: f32,
    y: f32,
) -> [u8; 4] {
    let center_x = rect.x + rect.width * center.0;
    let center_y = rect.y + rect.height * center.1;
    let point_angle = (x - center_x)
        .atan2(-(y - center_y))
        .to_degrees()
        .rem_euclid(360.0);
    let position = (point_angle - angle).rem_euclid(360.0) / 360.0;
    sample_normalized_stops(stops, position)
}

fn linear_color_at(
    rect: &crate::Rect,
    angle: f32,
    stops: &[(f32, [u8; 4])],
    x: f32,
    y: f32,
) -> [u8; 4] {
    let radians = angle.to_radians();
    let dx = radians.sin();
    let dy = -radians.cos();
    let center_x = rect.x + rect.width / 2.0;
    let center_y = rect.y + rect.height / 2.0;
    let half = (dx.abs() * rect.width + dy.abs() * rect.height) / 2.0;
    if half <= f32::EPSILON {
        return sample_normalized_stops(stops, 0.5);
    }
    let start_x = center_x - dx * half;
    let start_y = center_y - dy * half;
    let position = ((x - start_x) * dx + (y - start_y) * dy) / (2.0 * half);
    sample_normalized_stops(stops, position)
}

fn radial_color_at(
    rect: &crate::Rect,
    center: (f32, f32),
    stops: &[(f32, [u8; 4])],
    geometry: Option<crate::RadialGradientGeometry>,
    em: f32,
    root_font_size: f32,
    viewport: (f32, f32),
    x: f32,
    y: f32,
) -> [u8; 4] {
    let center_x = rect.x + rect.width * center.0;
    let center_y = rect.y + rect.height * center.1;
    let radii = resolve_radial_gradient_radii(
        rect,
        Point::from_xy(center_x, center_y),
        geometry,
        em,
        root_font_size,
        viewport,
    );
    let position = radii.map_or(0.0, |(radius_x, radius_y)| {
        (((x - center_x) / radius_x).powi(2) + ((y - center_y) / radius_y).powi(2)).sqrt()
    });
    sample_normalized_stops(stops, position)
}

fn premultiplied(color: [u8; 4]) -> tiny_skia::PremultipliedColorU8 {
    let alpha = color[3] as u32;
    tiny_skia::PremultipliedColorU8::from_rgba(
        ((color[0] as u32 * alpha) / 255) as u8,
        ((color[1] as u32 * alpha) / 255) as u8,
        ((color[2] as u32 * alpha) / 255) as u8,
        color[3],
    )
    .unwrap_or_else(|| {
        tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 0)
            .expect("transparent premultiplied color")
    })
}

fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

fn image_metadata_from_bytes(bytes: &[u8]) -> Option<(f32, f32)> {
    image_intrinsic_metadata(bytes)?.natural_size()
}

fn image_intrinsic_metadata(bytes: &[u8]) -> Option<crate::ReplacedIntrinsic> {
    let metadata = image_dimensions(bytes)
        .map(|(width, height)| {
            crate::ReplacedIntrinsic::from_dimensions(width as f32, height as f32)
        })
        .or_else(|| svg_image_intrinsic_metadata(bytes))?;
    let (width, height) = metadata.natural_size()?;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some(metadata)
}

fn background_image_rect(
    src: &str,
    base_url: Option<&str>,
    box_rect: &crate::Rect,
    explicit_size: Option<(f32, f32)>,
    size_expression: Option<&str>,
    fit: Option<crate::ObjectFit>,
    position: crate::BackgroundPosition,
    em: f32,
    rem: f32,
    viewport: (f32, f32),
    cache: &mut RenderResourceCache,
) -> Option<crate::Rect> {
    let bytes = fetch_bytes(src, base_url, cache)?;
    let intrinsic = if is_svg(&bytes) {
        svg_intrinsic(&bytes)
    } else {
        image_dimensions(&bytes).map(|(width, height)| (width as f32, height as f32))
    };
    let expression_size = size_expression.and_then(|expression| {
        let components = split_background_size_components(expression);
        let width = components.first().and_then(|value| {
            (!value.eq_ignore_ascii_case("auto")).then(|| {
                crate::style::resolve_contextual_length(
                    value,
                    em,
                    rem,
                    viewport.0 / 100.0,
                    viewport.1 / 100.0,
                    box_rect.width,
                )
            })?
        });
        let height = components.get(1).and_then(|value| {
            (!value.eq_ignore_ascii_case("auto")).then(|| {
                crate::style::resolve_contextual_length(
                    value,
                    em,
                    rem,
                    viewport.0 / 100.0,
                    viewport.1 / 100.0,
                    box_rect.height,
                )
            })?
        });
        match (width, height, intrinsic) {
            (Some(width), Some(height), _) => Some((width, height)),
            (Some(width), None, Some((iw, ih))) => Some((width, width * ih / iw)),
            (None, Some(height), Some((iw, ih))) => Some((height * iw / ih, height)),
            (None, None, Some(intrinsic)) => Some(intrinsic),
            _ => None,
        }
    });
    // `cover` and `contain` are complete sizing algorithms, not contextual
    // lengths. The style layer deliberately retains the authored token string
    // for CSS math, so a keyword can also appear in `size_expression`; run the
    // typed fit first or the unresolved token path falls back to the intrinsic
    // size and silently bypasses fitting.
    let (width, height) = if let Some(fit) = fit {
        let (iw, ih) = intrinsic?;
        let scale = match fit {
            crate::ObjectFit::Cover => (box_rect.width / iw).max(box_rect.height / ih),
            crate::ObjectFit::Contain => (box_rect.width / iw).min(box_rect.height / ih),
            _ => 1.0,
        };
        (iw * scale, ih * scale)
    } else if let Some(size) = expression_size {
        size
    } else if let Some(size) = explicit_size {
        size
    } else {
        intrinsic.unwrap_or((box_rect.width, box_rect.height))
    };
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some(crate::Rect {
        x: box_rect.x + position.x.resolve(box_rect.width - width),
        y: box_rect.y + position.y.resolve(box_rect.height - height),
        width,
        height,
    })
}

fn split_background_size_components(value: &str) -> Vec<&str> {
    let mut components = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    for (index, ch) in value.char_indices() {
        match ch {
            '(' => {
                depth += 1;
                start.get_or_insert(index);
            }
            ')' => depth = (depth - 1).max(0),
            ch if ch.is_whitespace() && depth == 0 => {
                if let Some(start) = start.take() {
                    components.push(value[start..index].trim());
                }
            }
            _ => {
                start.get_or_insert(index);
            }
        }
    }
    if let Some(start) = start {
        components.push(value[start..].trim());
    }
    components
}

fn paint_in_flow_generated_box(
    pixmap: &mut Pixmap,
    generated: &crate::dom::GeneratedBox,
    laid: &crate::dom::DomLayout,
    scroll_state: &ScrollPaintState,
    viewport: (f32, f32),
    root_font_size: f32,
    base_url: Option<&str>,
    image_cache: &mut RenderResourceCache,
    raster_scale: f32,
) {
    let Some(host_style) = laid.styles.get(&generated.host) else {
        return;
    };
    let style = match generated.kind {
        crate::dom::GeneratedBoxKind::Before => host_style.before_pseudo.as_deref(),
        crate::dom::GeneratedBoxKind::After => host_style.after_pseudo.as_deref(),
    };
    let Some(style) = style else { return };
    if style.effectively_invisible {
        return;
    }

    let (ox, oy) = scroll_state.translation_for(laid, generated.host);
    let rect = crate::Rect {
        x: generated.rect.x + ox,
        y: generated.rect.y + oy,
        width: generated.rect.width,
        height: generated.rect.height,
    };
    let overflow_clip = scroll_state.descendant_overflow_clip_for(laid, generated.host);
    let clip = overflow_clip
        .as_ref()
        .map(|clip| clip.viewport_rect(scroll_state.surface_extent.unwrap_or(viewport)));
    let visible = match clip {
        Some(clip) => rect.intersect(&clip),
        None => Some(rect),
    };
    let Some(visible) = visible else { return };
    if visible.width <= 0.0 || visible.height <= 0.0 {
        return;
    }
    let ink = non_text_ink_bounds(&rect, style);
    let visible_ink = match clip {
        Some(clip) => ink.intersect(&clip),
        None => Some(ink),
    };
    if !visible_ink
        .is_some_and(|ink| rect_intersects_paint_surface(&ink, pixmap, raster_scale))
    {
        return;
    }
    let ancestor_clip_mask = overflow_clip.as_ref().and_then(|clip| {
        overflow_clip_mask(
            pixmap.width(),
            pixmap.height(),
            clip,
            scroll_state.surface_extent.unwrap_or(viewport),
        )
    });

    if let Some(shadow) = style.box_shadow {
        paint_box_shadow(
            pixmap,
            &shadow,
            &rect,
            style.border_model.radii,
            ancestor_clip_mask.as_ref(),
        );
    }
    let radius = style.border_model.radii.resolve(rect.width, rect.height);
    let clip_path_mask = style.clip_path.as_ref().and_then(|polygon| {
        polygon_clip_mask(
            pixmap.width(),
            pixmap.height(),
            polygon,
            &rect,
            style.font_size.unwrap_or(16.0),
            root_font_size,
            viewport,
        )
    });
    let background = background_geometry(&rect, style);
    let background_path = background_clip_path(background);
    let element_clip_mask =
        background_extra_clip(ancestor_clip_mask.as_ref(), clip_path_mask.as_ref());
    let background_mask = element_clip_mask.clone();
    if style.mask_image.is_none() && !style.background_clip_text {
        if let (Some(color), Some(path)) = (style.background_color, background_path.as_ref()) {
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
            paint.anti_alias = !background.clip_radii.is_zero();
            pixmap.fill_path(
                path,
                &paint,
                FillRule::Winding,
                raster_transform(raster_scale),
                background_mask.as_ref(),
            );
        }
        if !style.background_gradient_layers.is_empty() {
            if let Some(path) = background_path.as_ref() {
                paint_background_gradient_layers(
                    pixmap,
                    path,
                    &background.origin_rect,
                    &background.clip_rect,
                    background.clip_radii,
                    style,
                    root_font_size,
                    viewport,
                    background_mask.as_ref(),
                    raster_scale,
                );
            }
        } else {
            if let Some((center, stops)) = &style.background_radial_gradient {
                if let Some(path) = background_path.as_ref() {
                    paint_radial_gradient(
                        pixmap,
                        path,
                        &background.origin_rect,
                        *center,
                        stops,
                        style.background_radial_gradient_geometry,
                        style.font_size.unwrap_or(16.0),
                        root_font_size,
                        viewport,
                        background_mask.as_ref(),
                        raster_scale,
                    );
                }
            }
            if let Some((angle, center, stops)) = &style.background_conic_gradient {
                paint_conic_gradient_sampled(
                    pixmap,
                    &background.clip_rect,
                    &background.origin_rect,
                    background.clip_radii,
                    *angle,
                    *center,
                    stops,
                    background_mask.as_ref(),
                );
            }
            if let Some((angle, stops)) = &style.background_gradient {
                if let Some(path) = background_path.as_ref() {
                    paint_linear_gradient(
                        pixmap,
                        path,
                        &background.origin_rect,
                        *angle,
                        stops,
                        background_mask.as_ref(),
                        raster_scale,
                    );
                }
            }
        }
    }
    if let Some(mask_url) = &style.mask_image {
        let fill = style
            .background_color
            .or(style.color)
            .unwrap_or([0, 0, 0, 255]);
        paint_mask(
            mask_url,
            base_url,
            &visible,
            radius,
            fill,
            style.background_radial_gradient.as_ref(),
            style.background_radial_gradient_geometry,
            style.font_size.unwrap_or(16.0),
            root_font_size,
            viewport,
            style.background_gradient.as_ref(),
            style.background_conic_gradient.as_ref(),
            style.mask_size,
            style.mask_repeat,
            element_clip_mask.as_ref(),
            pixmap,
            image_cache,
        );
    } else if let Some(background_url) = &style.background_image {
        if let Some(image_rect) = background_image_rect(
            background_url,
            base_url,
            &background.origin_rect,
            style.background_size,
            style.background_size_expression.as_deref(),
            style.background_size_fit,
            style.background_position,
            style.font_size.unwrap_or(16.0),
            root_font_size,
            viewport,
            image_cache,
        ) {
            paint_image(
                background_url,
                base_url,
                &image_rect,
                &background.clip_rect,
                crate::ObjectFit::Fill,
                crate::ObjectPosition::default(),
                pixmap,
                image_cache,
                None,
                None,
                background.clip_radii,
                background_mask.as_ref(),
            );
        }
    }

    paint_css_border(
        pixmap,
        &rect,
        style,
        element_clip_mask.as_ref(),
        raster_scale,
    );
    paint_css_outline(
        pixmap,
        &rect,
        style,
        element_clip_mask.as_ref(),
        raster_scale,
    );
}

fn paint_positioned_pseudo(
    text_engine: &mut crate::inline::TextEngine,
    pixmap: &mut Pixmap,
    style: &crate::LayoutStyle,
    containing_block: &crate::Rect,
    static_position_rect: &crate::Rect,
    viewport: (f32, f32),
    root_font_size: f32,
    clip_extent: (f32, f32),
    ancestor_overflow_clip: Option<&crate::dom::OverflowClip>,
    base_url: Option<&str>,
    image_cache: &mut RenderResourceCache,
    raster_scale: f32,
) {
    if style.position != Some(taffy::Position::Absolute) {
        return;
    }
    let em = style.font_size.unwrap_or(16.0);
    let resolve = |dimension: crate::Dimension, basis: f32| match dimension.resolve(
        em,
        root_font_size,
        viewport.0 / 100.0,
        viewport.1 / 100.0,
    ) {
        crate::Dimension::Px(value) => Some(value),
        crate::Dimension::Percent(value) => Some(value * basis),
        _ => None,
    };
    let top = style.inset[0].and_then(|value| resolve(value, containing_block.height));
    let right = style.inset[1].and_then(|value| resolve(value, containing_block.width));
    let bottom = style.inset[2].and_then(|value| resolve(value, containing_block.height));
    let left = style.inset[3].and_then(|value| resolve(value, containing_block.width));
    // Generated text supplies the shrink-to-fit dimensions of an absolutely
    // positioned pseudo whose width and/or height is auto. Tailwind's code
    // gutters use exactly this shape (`width` plus auto height); requiring two
    // opposing insets previously discarded the pseudo before text painting.
    let generated_item = style
        .before_content
        .as_deref()
        .filter(|content| !content.is_empty())
        .and_then(|content| text_engine.push_generated_text(content, style));
    let generated_intrinsic = generated_item.map(|item| text_engine.measure(item, None));
    let width = resolve(style.width, containing_block.width)
        .or_else(|| Some(containing_block.width - left? - right?))
        .or_else(|| generated_intrinsic.map(|size| size.0));
    let height = resolve(style.height, containing_block.height)
        .or_else(|| Some(containing_block.height - top? - bottom?))
        .or_else(|| generated_intrinsic.map(|size| size.1));
    let (Some(width), Some(height)) = (width, height) else {
        return;
    };
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let x = left
        .map(|value| containing_block.x + value)
        .or_else(|| right.map(|value| containing_block.x + containing_block.width - value - width))
        .unwrap_or(static_position_rect.x);
    let y = top
        .map(|value| containing_block.y + value)
        .or_else(|| {
            bottom.map(|value| containing_block.y + containing_block.height - value - height)
        })
        .unwrap_or(static_position_rect.y);
    let rect = crate::Rect {
        x,
        y,
        width,
        height,
    };
    let ancestor_clip = ancestor_overflow_clip
        .map(|clip| clip.viewport_rect(clip_extent));
    let visible = match ancestor_clip {
        Some(clip) => rect.intersect(&clip),
        None => Some(rect),
    };
    let Some(visible) = visible else { return };
    if !rect_intersects_paint_surface(&non_text_ink_bounds(&rect, style), pixmap, raster_scale) {
        return;
    }
    let ancestor_clip_mask = ancestor_overflow_clip.and_then(|clip| {
        overflow_clip_mask(pixmap.width(), pixmap.height(), clip, clip_extent)
    });
    let radius = style.border_model.radii.resolve(rect.width, rect.height);
    let clip_path_mask = style.clip_path.as_ref().and_then(|polygon| {
        polygon_clip_mask(
            pixmap.width(),
            pixmap.height(),
            polygon,
            &rect,
            em,
            root_font_size,
            viewport,
        )
    });
    let background = background_geometry(&rect, style);
    let background_path = background_clip_path(background);
    let element_clip_mask =
        background_extra_clip(ancestor_clip_mask.as_ref(), clip_path_mask.as_ref());
    let background_mask = element_clip_mask.clone();
    if style.mask_image.is_none() && !style.background_clip_text {
        if let (Some(color), Some(path)) = (style.background_color, background_path.as_ref()) {
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
            pixmap.fill_path(
                path,
                &paint,
                FillRule::Winding,
                raster_transform(raster_scale),
                background_mask.as_ref(),
            );
        }
        if !style.background_gradient_layers.is_empty() {
            if let Some(path) = background_path.as_ref() {
                paint_background_gradient_layers(
                    pixmap,
                    path,
                    &background.origin_rect,
                    &background.clip_rect,
                    background.clip_radii,
                    style,
                    root_font_size,
                    viewport,
                    background_mask.as_ref(),
                    raster_scale,
                );
            }
        } else {
            if let Some((center, stops)) = &style.background_radial_gradient {
                if let Some(path) = background_path.as_ref() {
                    paint_radial_gradient(
                        pixmap,
                        path,
                        &background.origin_rect,
                        *center,
                        stops,
                        style.background_radial_gradient_geometry,
                        em,
                        root_font_size,
                        viewport,
                        background_mask.as_ref(),
                        raster_scale,
                    );
                }
            }
            if let Some((angle, center, stops)) = &style.background_conic_gradient {
                paint_conic_gradient_sampled(
                    pixmap,
                    &background.clip_rect,
                    &background.origin_rect,
                    background.clip_radii,
                    *angle,
                    *center,
                    stops,
                    background_mask.as_ref(),
                );
            }
            if let Some((angle, stops)) = &style.background_gradient {
                if let Some(path) = background_path.as_ref() {
                    paint_linear_gradient(
                        pixmap,
                        path,
                        &background.origin_rect,
                        *angle,
                        stops,
                        background_mask.as_ref(),
                        raster_scale,
                    );
                }
            }
        }
    }
    if let Some(mask_url) = &style.mask_image {
        let fill = style
            .background_color
            .or(style.color)
            .unwrap_or([0, 0, 0, 255]);
        paint_mask(
            mask_url,
            base_url,
            &visible,
            radius,
            fill,
            style.background_radial_gradient.as_ref(),
            style.background_radial_gradient_geometry,
            em,
            root_font_size,
            viewport,
            style.background_gradient.as_ref(),
            style.background_conic_gradient.as_ref(),
            style.mask_size,
            style.mask_repeat,
            element_clip_mask.as_ref(),
            pixmap,
            image_cache,
        );
    } else if let Some(bg_url) = &style.background_image {
        if let Some(image_rect) = background_image_rect(
            bg_url,
            base_url,
            &background.origin_rect,
            style.background_size,
            style.background_size_expression.as_deref(),
            style.background_size_fit,
            style.background_position,
            em,
            root_font_size,
            viewport,
            image_cache,
        ) {
            paint_image(
                bg_url,
                base_url,
                &image_rect,
                &background.clip_rect,
                crate::ObjectFit::Fill,
                crate::ObjectPosition::default(),
                pixmap,
                image_cache,
                None,
                None,
                background.clip_radii,
                background_mask.as_ref(),
            );
        }
    }
    paint_css_border(
        pixmap,
        &rect,
        style,
        element_clip_mask.as_ref(),
        raster_scale,
    );
    paint_css_outline(
        pixmap,
        &rect,
        style,
        element_clip_mask.as_ref(),
        raster_scale,
    );
    if let Some(item) = generated_item {
        let (text_width, text_height) = generated_intrinsic.unwrap_or_default();
        // `text-align` positions inline content within the pseudo's content
        // box. It is independent of flex/grid `justify-content`: Tailwind's
        // absolutely positioned line-number gutters are inline-block pseudos,
        // so consulting only `justify-content` left every counter at the
        // gutter's start edge even though its inherited computed alignment
        // was `right`.
        let x = if style.display == crate::Display::Flex {
            match style.justify_content {
                Some(taffy::JustifyContent::CENTER) => rect.x + (rect.width - text_width) / 2.0,
                Some(taffy::JustifyContent::FLEX_END | taffy::JustifyContent::END) => {
                    rect.x + rect.width - style.padding.right - text_width
                }
                _ => rect.x + style.padding.left,
            }
        } else {
            match style.text_align {
                Some(taffy::AlignItems::CENTER) => rect.x + (rect.width - text_width) / 2.0,
                Some(taffy::AlignItems::FLEX_END | taffy::AlignItems::END) => {
                    rect.x + rect.width - style.padding.right - text_width
                }
                _ => rect.x + style.padding.left,
            }
        };
        let y = match style.align_items {
            Some(taffy::AlignItems::CENTER) => rect.y + (rect.height - text_height) / 2.0,
            Some(taffy::AlignItems::FLEX_END | taffy::AlignItems::END) => {
                rect.y + rect.height - style.padding.bottom - text_height
            }
            _ => rect.y + style.padding.top,
        };
        text_engine.finalize(item, (x, y), text_width, Some(visible));
        text_engine.paint_item_with_clip_mask_scaled(
            item,
            pixmap,
            (0.0, 0.0),
            Some(visible),
            element_clip_mask.as_ref(),
            raster_scale,
        );
    }
}

/// Fetch every `<img>` once (seeding `cache` for the paint pass) and record its
/// intrinsic (width, height) so layout can size replaced elements that have no
/// explicit dimensions. Video posters are image resources too: before a
/// decoded frame exists, their intrinsic dimensions and pixels are the
/// replaced content Chromium paints for `<video>`. Keyed by the element's
/// NodeId.
fn collect_image_intrinsics(
    tree: &DomTree,
    viewport: (f32, f32),
    base_url: Option<&str>,
    cache: &mut RenderResourceCache,
) -> (
    HashMap<obscura_dom::tree::NodeId, crate::ReplacedIntrinsic>,
    HashMap<obscura_dom::tree::NodeId, SelectedImage>,
) {
    let mut out = std::collections::HashMap::new();
    let mut selected = HashMap::new();
    for nid in crate::dom::rendered_descendants(tree, tree.document()) {
        let Some(node) = tree.get_node(nid) else {
            continue;
        };
        let Some(element) = node.as_element() else {
            continue;
        };
        let (url, density) = match element.local.as_ref() {
            "img" => {
                let Some(candidate) = resolve_img_url(tree, nid, viewport) else {
                    continue;
                };
                candidate
            }
            "video" => {
                let Some(poster) = node.get_attribute("poster") else {
                    continue;
                };
                let poster = poster.trim();
                if poster.is_empty() {
                    continue;
                }
                (poster.to_string(), 1.0)
            }
            _ => continue,
        };
        let resolved_url = resolve_resource_url(&url, base_url).unwrap_or(url);
        let profile = image_request_profile(tree, nid);
        selected.insert(
            nid,
            SelectedImage {
                resolved_url: resolved_url.clone(),
                density,
                profile,
            },
        );
        let Some(bytes) = fetch_profiled_image_bytes(&resolved_url, None, cache, profile) else {
            continue;
        };
        if let Some(mut intrinsic) = image_intrinsic_metadata(&bytes) {
            // A 2x (or w-descriptor) candidate's raw pixels are density times
            // its CSS size. A ratio is dimensionless and remains unchanged.
            intrinsic.width = intrinsic.width.map(|width| width / density);
            intrinsic.height = intrinsic.height.map(|height| height / density);
            out.insert(nid, intrinsic);
        }
    }
    (out, selected)
}

/// Add intrinsic metadata for CSS `content:url(...)` images after the first
/// cascade has exposed their computed URL. Returns true when a new intrinsic
/// entry was added and layout therefore needs one resource-aware retry.
fn collect_content_image_intrinsics(
    tree: &DomTree,
    styles: &std::collections::HashMap<obscura_dom::tree::NodeId, crate::LayoutStyle>,
    base_url: Option<&str>,
    cache: &mut RenderResourceCache,
    out: &mut std::collections::HashMap<obscura_dom::tree::NodeId, crate::ReplacedIntrinsic>,
    selected: &mut HashMap<obscura_dom::tree::NodeId, SelectedImage>,
    source_intrinsic: &HashMap<obscura_dom::tree::NodeId, crate::ReplacedIntrinsic>,
    source_selected: &HashMap<obscura_dom::tree::NodeId, SelectedImage>,
    seeded: &HashSet<obscura_dom::tree::NodeId>,
) -> bool {
    let mut changed = false;
    let mut active = HashSet::new();
    for (&nid, style) in styles {
        let Some(node) = tree.get_node(nid) else {
            continue;
        };
        if node
            .as_element()
            .map_or(true, |name| name.local.as_ref() != "img")
        {
            continue;
        }
        let Some(url) = style.content_image.as_deref() else {
            continue;
        };
        active.insert(nid);
        let resolved_url = resolve_resource_url(url, base_url).unwrap_or_else(|| url.to_string());
        let remembered_url_changed = cache
            .content_image_intrinsics
            .get(&nid)
            .is_some_and(|remembered| remembered.resolved_url != resolved_url);
        selected.insert(
            nid,
            SelectedImage {
                resolved_url: resolved_url.clone(),
                density: 1.0,
                profile: ImageRequestProfile::NoCorsInclude,
            },
        );
        // CSS content replaces the element's ordinary source. Remove the src
        // intrinsic before inspecting content so a failed content image cannot
        // accidentally retain src geometry.
        let previous_dimensions = out.remove(&nid);
        let Some(bytes) = fetch_bytes(&resolved_url, None, cache) else {
            changed |= previous_dimensions.is_some() || remembered_url_changed;
            cache.forget_content_image_intrinsic(nid);
            continue;
        };
        let Some(intrinsic) = image_intrinsic_metadata(&bytes) else {
            changed |= previous_dimensions.is_some() || remembered_url_changed;
            cache.forget_content_image_intrinsic(nid);
            continue;
        };
        changed |= previous_dimensions != Some(intrinsic) || remembered_url_changed;
        out.insert(nid, intrinsic);
        cache.remember_content_image_intrinsic(nid, resolved_url, intrinsic);
    }

    // A remembered selection whose computed content disappeared must stop
    // overriding the element's ordinary source. Even equal fallback dimensions
    // get one correction pass: the first layout consumed a stale CSS selection,
    // and URL/selection state is part of the prepared result too.
    for nid in seeded.iter().copied().collect::<Vec<_>>() {
        if active.contains(&nid) {
            continue;
        }
        cache.forget_content_image_intrinsic(nid);
        match source_intrinsic.get(&nid).copied() {
            Some(dimensions) => {
                out.insert(nid, dimensions);
            }
            None => {
                out.remove(&nid);
            }
        }
        match source_selected.get(&nid).cloned() {
            Some(source) => {
                selected.insert(nid, source);
            }
            None => {
                selected.remove(&nid);
            }
        }
        changed = true;
    }
    changed
}

/// Choose the URL to paint for an `<img>`. Browsers do not use `src` alone:
/// a wrapping `<picture>`'s `<source>`s, `srcset`, and `sizes` select by
/// type/media/viewport/density. Non-standard lazy-loader attributes such as
/// `data-src` are deliberately ignored until page script copies them into the
/// real `src`/`srcset`; promoting them here changes `currentSrc`, lifecycle
/// events, and resource timing relative to browsers.
fn resolve_img_url(
    tree: &DomTree,
    nid: obscura_dom::tree::NodeId,
    viewport: (f32, f32),
) -> Option<(String, f32)> {
    let node = tree.get_node(nid)?;
    // A <picture>'s preceding, type/media-matching <source> wins over the
    // <img>'s own attributes (HTML "update the source set").
    if let Some(pick) = picture_source_url(tree, nid, viewport) {
        return Some(pick);
    }
    let sizes = node.get_attribute("sizes");
    if let Some(v) = node.get_attribute("srcset") {
        if let Some(pick) = best_srcset_candidate(v, sizes, viewport) {
            return Some(pick);
        }
    }
    if let Some(v) = node.get_attribute("src") {
        let v = v.trim();
        if !v.is_empty() {
            return Some((v.to_string(), 1.0));
        }
    }
    None
}

/// When `img_nid` is an `<img>` inside a `<picture>`, walk its preceding
/// `<source>` siblings in document order and return the selected URL of the
/// first supported one (matching `type` and `media`), per WebKit's
/// `HTMLImageElement::bestFitSourceFromPictureElement`. `None` means no source
/// applied and the caller should fall back to the `<img>`'s own attributes.
fn picture_source_url(
    tree: &DomTree,
    img_nid: obscura_dom::tree::NodeId,
    viewport: (f32, f32),
) -> Option<(String, f32)> {
    let img = tree.get_node(img_nid)?;
    let parent = img.parent?;
    let is_picture = tree
        .get_node(parent)
        .and_then(|p| p.as_element().map(|e| e.local.as_ref() == "picture"))
        .unwrap_or(false);
    if !is_picture {
        return None;
    }
    for cid in tree.children(parent) {
        // Only sources that precede the <img> contribute.
        if cid == img_nid {
            break;
        }
        let Some(child) = tree.get_node(cid) else {
            continue;
        };
        if child
            .as_element()
            .map(|e| e.local.as_ref() != "source")
            .unwrap_or(true)
        {
            continue;
        }
        let Some(srcset) = child.get_attribute("srcset") else {
            continue;
        };
        if srcset.trim().is_empty() {
            continue;
        }
        if let Some(t) = child.get_attribute("type") {
            if !crate::source_type_supported(t) {
                continue;
            }
        }
        if let Some(m) = child.get_attribute("media") {
            if !m.trim().is_empty() && !crate::css::media_query_applies_for_viewport(m, viewport) {
                continue;
            }
        }
        let sizes = child.get_attribute("sizes");
        if let Some(u) = best_srcset_candidate(srcset, sizes, viewport) {
            return Some(u);
        }
    }
    None
}

/// Pick one URL from a `srcset` list, matching the WebKit/Blink selection:
/// normalize each `w` descriptor to an effective density (`w / source-size`,
/// with the source-size taken from `sizes` or falling back to the viewport
/// width), treat `x` descriptors as-is and a bare candidate as `1x`, then pick
/// the smallest density at least the device pixel ratio (1 at DPR 1), else the
/// largest available.
/// Returns the picked candidate URL and its pixel density. The density is the
/// x-descriptor (or, for w-descriptors, width / source-size): the factor the
/// file's raw pixels must be divided by to get CSS px. Laying out with raw
/// pixels made every 2x responsive image occupy twice its design size.
fn best_srcset_candidate(
    srcset: &str,
    sizes: Option<&str>,
    viewport: (f32, f32),
) -> Option<(String, f32)> {
    const DPR: f32 = 1.0;
    let source_size = source_size_px(sizes, viewport);
    let mut cands: Vec<(f32, String)> = Vec::new();
    // Parse candidates WHATWG-style: a URL is a run of non-whitespace (so a
    // data: URI's internal commas stay part of it, unlike a naive split on
    // ','), optionally followed by a descriptor up to the next comma.
    let is_ws = |c: char| c.is_whitespace();
    let mut rest = srcset.trim_start_matches(|c: char| is_ws(c) || c == ',');
    while !rest.is_empty() {
        let url_end = rest.find(is_ws).unwrap_or(rest.len());
        let raw_url = &rest[..url_end];
        rest = &rest[url_end..];
        // Trailing commas on the URL mean the candidate had no descriptor.
        let url = raw_url.trim_end_matches(',');
        let no_desc = url.len() != raw_url.len();
        rest = rest.trim_start_matches(is_ws);
        let desc = if no_desc {
            ""
        } else {
            let d_end = rest.find(',').unwrap_or(rest.len());
            let d = rest[..d_end].trim();
            rest = &rest[d_end..];
            d
        };
        rest = rest.trim_start_matches(|c: char| c == ',' || is_ws(c));
        if url.is_empty() {
            continue;
        }
        let density = if desc.is_empty() {
            1.0
        } else if let Some(w) = desc.strip_suffix('w').and_then(|s| s.parse::<f32>().ok()) {
            if source_size > 0.0 {
                w / source_size
            } else {
                continue;
            }
        } else if let Some(x) = desc.strip_suffix('x').and_then(|s| s.parse::<f32>().ok()) {
            x
        } else {
            // An `h` (height) descriptor or malformed token: skip the candidate.
            continue;
        };
        cands.push((density, url.to_string()));
    }
    if cands.is_empty() {
        return None;
    }
    cands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let pick = cands
        .iter()
        .find(|(d, _)| *d >= DPR)
        .map(|(d, u)| (u.clone(), *d))
        .unwrap_or_else(|| {
            let (d, u) = cands.last().unwrap();
            (u.clone(), *d)
        });
    Some((pick.0, pick.1.max(0.01)))
}

/// Approximate the CSS px size an image will be displayed at, from its `sizes`
/// attribute: the first entry whose media condition holds at our assumed
/// desktop viewport (a bare entry always holds), else the viewport width. Used
/// only to convert `w` descriptors to densities, so a coarse value is fine.
fn source_size_px(sizes: Option<&str>, viewport: (f32, f32)) -> f32 {
    let Some(sizes) = sizes else {
        return viewport.0;
    };
    for entry in sizes.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (cond, len) = split_size_entry(entry);
        if let Some(cond) = cond {
            if !crate::css::media_query_applies_for_viewport(&cond, viewport) {
                continue;
            }
        }
        if let Some(px) = length_to_px(&len, viewport.0) {
            return px;
        }
    }
    viewport.0
}

/// Split one `sizes` entry into its optional leading media condition and its
/// trailing `<length>`. Tokenizes on whitespace at paren depth 0 so a
/// `calc(...)` length or a parenthesized condition stays intact; the last
/// token is the length, anything before it is the condition.
fn split_size_entry(entry: &str) -> (Option<String>, String) {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in entry.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            c if c.is_whitespace() && depth == 0 => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let len = tokens.pop().unwrap_or_default();
    let cond = if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    };
    (cond, len)
}

/// Resolve a `sizes` length to px against the assumed viewport. `vw`/`%` scale
/// by the viewport width; `px` is literal; `em`/`rem` use the 16px root.
/// `calc()` and other forms return `None` (the caller tries the next entry).
fn length_to_px(len: &str, viewport_width: f32) -> Option<f32> {
    let t = len.trim().to_ascii_lowercase();
    let num = |s: &str| s.trim().parse::<f32>().ok();
    if let Some(v) = t.strip_suffix("vw").and_then(num) {
        return Some(v / 100.0 * viewport_width);
    }
    if let Some(v) = t.strip_suffix('%').and_then(num) {
        return Some(v / 100.0 * viewport_width);
    }
    if let Some(v) = t.strip_suffix("px").and_then(num) {
        return Some(v);
    }
    if let Some(v) = t.strip_suffix("rem").and_then(num) {
        return Some(v * 16.0);
    }
    if let Some(v) = t.strip_suffix("em").and_then(num) {
        return Some(v * 16.0);
    }
    num(&t)
}

fn paint_canvas_surface(
    surface: CanvasSurface<'_>,
    rect: &crate::Rect,
    visible_rect: &crate::Rect,
    object_fit: crate::ObjectFit,
    object_position: crate::ObjectPosition,
    pixmap: &mut Pixmap,
    clip_radius: crate::ResolvedBorderRadii,
    extra_clip: Option<&tiny_skia::Mask>,
) -> bool {
    if surface.width == 0
        || surface.height == 0
        || rect.width <= 0.0
        || rect.height <= 0.0
        || !rect_intersects_paint_surface(visible_rect, pixmap, 1.0)
    {
        return false;
    }

    // Canvas ImageData is straight-alpha RGBA; tiny-skia consumes
    // premultiplied RGBA. Convert once per actual canvas paint while borrowing
    // the live V8 backing directly—there is no JS-to-Rust surface copy on each
    // Canvas2D operation.
    let mut premultiplied = surface.rgba.to_vec();
    for pixel in premultiplied.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        pixel[0] = ((pixel[0] as u32 * alpha + 127) / 255) as u8;
        pixel[1] = ((pixel[1] as u32 * alpha + 127) / 255) as u8;
        pixel[2] = ((pixel[2] as u32 * alpha + 127) / 255) as u8;
    }
    let Some(content) = tiny_skia::PixmapRef::from_bytes(
        &premultiplied,
        surface.width,
        surface.height,
    ) else {
        return false;
    };

    let dest = object_fit_dest_positioned(
        rect,
        surface.width as f32,
        surface.height as f32,
        object_fit,
        object_position,
    );
    if dest.width <= 0.0 || dest.height <= 0.0 {
        return false;
    }

    let has_radius = !clip_radius.is_zero();
    let needs_box_clip = has_radius
        || dest.width > visible_rect.width + 0.5
        || dest.height > visible_rect.height + 0.5
        || dest.x < visible_rect.x - 0.5
        || dest.y < visible_rect.y - 0.5;
    let mut clip = extra_clip.cloned();
    if needs_box_clip {
        let path = if has_radius {
            rounded_rect_path_radii(
                visible_rect.x,
                visible_rect.y,
                visible_rect.width,
                visible_rect.height,
                clip_radius,
            )
        } else {
            Rect::from_xywh(
                visible_rect.x,
                visible_rect.y,
                visible_rect.width,
                visible_rect.height,
            )
            .and_then(|rect| {
                let mut builder = PathBuilder::new();
                builder.push_rect(rect);
                builder.finish()
            })
        };
        match (clip.as_mut(), path) {
            (Some(mask), Some(path)) => {
                mask.intersect_path(&path, FillRule::Winding, true, Transform::identity())
            }
            (None, _) => {
                clip = rounded_box_clip_mask_radii(
                    pixmap.width(),
                    pixmap.height(),
                    visible_rect,
                    clip_radius,
                );
            }
            _ => {}
        }
    }

    let transform = Transform::from_row(
        dest.width / surface.width as f32,
        0.0,
        0.0,
        dest.height / surface.height as f32,
        dest.x,
        dest.y,
    );
    pixmap.draw_pixmap(
        0,
        0,
        content,
        &tiny_skia::PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..tiny_skia::PixmapPaint::default()
        },
        transform,
        clip.as_ref(),
    );
    true
}

fn paint_image(
    src: &str,
    base_url: Option<&str>,
    rect: &crate::Rect,
    visible_rect: &crate::Rect,
    object_fit: crate::ObjectFit,
    object_position: crate::ObjectPosition,
    pixmap: &mut Pixmap,
    cache: &mut RenderResourceCache,
    profile: Option<ImageRequestProfile>,
    transform: Option<crate::Affine2>,
    clip_radius: crate::ResolvedBorderRadii,
    extra_clip: Option<&tiny_skia::Mask>,
) -> bool {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return false;
    }
    // Image-bearing display lists currently use the proven CSS-pixel raster
    // path (`native_raster_scale_supported` rejects them). Cull before cache
    // lookup, SVG parsing, or bitmap resizing.
    if !rect_intersects_paint_surface(visible_rect, pixmap, 1.0) {
        return false;
    }
    let bytes = match profile {
        Some(profile) => fetch_profiled_image_bytes(src, base_url, cache, profile),
        None => fetch_bytes(src, base_url, cache),
    };
    let Some(bytes) = bytes else {
        return false;
    };
    let svg = is_svg(&bytes);

    // Destination sub-rect within the element box. `Fill` keeps the historical
    // behavior (stretch the image to the whole box); the other modes need the
    // image's intrinsic size to preserve its aspect ratio, and fall back to
    // fill when it cannot be read.
    let dest = if object_fit == crate::ObjectFit::Fill {
        *rect
    } else {
        let intrinsic = if svg {
            svg_intrinsic(&bytes)
        } else {
            image_dimensions(&bytes).map(|(w, h)| (w as f32, h as f32))
        };
        match intrinsic {
            Some((iw, ih)) => {
                object_fit_dest_positioned(rect, iw, ih, object_fit, object_position)
            }
            None => *rect,
        }
    };

    let (dw, dh) = (
        dest.width.round().max(1.0) as u32,
        dest.height.round().max(1.0) as u32,
    );
    let content = if svg {
        render_svg(&bytes, dw, dh)
    } else {
        raster_to_pixmap(&bytes, dw, dh)
    };
    let Some(content) = content else { return false };

    // The raster may not paint past `visible_rect` (the border box already
    // intersected with the ancestor overflow clip): `Cover`/`None` can size
    // the image past the box, and an ancestor clip can cut into the box
    // itself. Only the fully-inside case takes the unmasked fast path.
    let has_radius = !clip_radius.is_zero();
    let needs_box_clip = has_radius
        || dest.width > visible_rect.width + 0.5
        || dest.height > visible_rect.height + 0.5
        || dest.x < visible_rect.x - 0.5
        || dest.y < visible_rect.y - 0.5;
    let mut clip = extra_clip.cloned();
    if needs_box_clip {
        let path = if has_radius {
            rounded_rect_path_radii(
                visible_rect.x,
                visible_rect.y,
                visible_rect.width,
                visible_rect.height,
                clip_radius,
            )
        } else {
            Rect::from_xywh(
                visible_rect.x,
                visible_rect.y,
                visible_rect.width,
                visible_rect.height,
            )
            .and_then(|rect| {
                let mut builder = PathBuilder::new();
                builder.push_rect(rect);
                builder.finish()
            })
        };
        match (clip.as_mut(), path) {
            (Some(mask), Some(path)) => {
                mask.intersect_path(&path, FillRule::Winding, true, Transform::identity())
            }
            (None, _) => {
                clip = rounded_box_clip_mask_radii(
                    pixmap.width(),
                    pixmap.height(),
                    visible_rect,
                    clip_radius,
                );
            }
            _ => {}
        }
    }
    pixmap.draw_pixmap(
        dest.x as i32,
        dest.y as i32,
        content.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        transform
            .map(|transform| {
                Transform::from_row(
                    transform.a,
                    transform.b,
                    transform.c,
                    transform.d,
                    transform.e,
                    transform.f,
                )
            })
            .unwrap_or_else(Transform::identity),
        clip.as_ref(),
    );
    true
}

/// The destination sub-rect for replaced image content within its box. The
/// position resolves against the leftover space after `object-fit`; for
/// `Cover`/`None` that space can be negative and the caller clips the result.
#[cfg(test)]
fn object_fit_dest(box_rect: &crate::Rect, iw: f32, ih: f32, fit: crate::ObjectFit) -> crate::Rect {
    object_fit_dest_positioned(box_rect, iw, ih, fit, crate::ObjectPosition::default())
}

fn object_fit_dest_positioned(
    box_rect: &crate::Rect,
    iw: f32,
    ih: f32,
    fit: crate::ObjectFit,
    position: crate::ObjectPosition,
) -> crate::Rect {
    let (bw, bh) = (box_rect.width, box_rect.height);
    if iw <= 0.0 || ih <= 0.0 {
        return *box_rect;
    }
    let (dw, dh) = match fit {
        crate::ObjectFit::Fill => (bw, bh),
        crate::ObjectFit::Contain => {
            let s = (bw / iw).min(bh / ih);
            (iw * s, ih * s)
        }
        crate::ObjectFit::Cover => {
            let s = (bw / iw).max(bh / ih);
            (iw * s, ih * s)
        }
        crate::ObjectFit::None => (iw, ih),
        crate::ObjectFit::ScaleDown => {
            // min(Contain-size, intrinsic-size): the Contain fit, but never
            // scaled up past the image's own pixels.
            let s = (bw / iw).min(bh / ih).min(1.0);
            (iw * s, ih * s)
        }
    };
    crate::Rect {
        x: box_rect.x + position.x.resolve(bw - dw),
        y: box_rect.y + position.y.resolve(bh - dh),
        width: dw,
        height: dh,
    }
}

/// The intrinsic `(width, height)` of an SVG image from its size/`viewBox`,
/// used to preserve aspect ratio under `object-fit`. Parses the SVG once; the
/// eventual raster re-parses in `render_svg` (only reached for a non-`fill`
/// object-fit on an SVG image, which is rare).
fn svg_intrinsic(bytes: &[u8]) -> Option<(f32, f32)> {
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let size = tree.size();
    if size.width() > 0.0 && size.height() > 0.0 {
        Some((size.width(), size.height()))
    } else {
        None
    }
}

/// Read intrinsic SVG dimensions without treating `viewBox` user-space
/// coordinates as CSS-pixel dimensions. `usvg::Tree::size()` necessarily
/// resolves percentage/missing root dimensions to its rendering viewport, so
/// it cannot preserve the distinction required by CSS replaced sizing.
fn svg_image_intrinsic_metadata(bytes: &[u8]) -> Option<crate::ReplacedIntrinsic> {
    // The lightweight attribute pass below preserves missing/percentage axes,
    // which `Tree::size()` necessarily resolves. It must not, however, turn a
    // malformed XML prefix that merely resembles an SVG root into successful
    // image metadata. Use the same parser as paint as the validity gate first.
    let _validated = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let source = std::str::from_utf8(bytes).ok()?;
    let mut remaining = source.trim_start_matches('\u{feff}').trim_start();
    let tail = loop {
        if let Some(after) = remaining.strip_prefix("<!--") {
            let end = after.find("-->")?;
            remaining = after[end + 3..].trim_start();
            continue;
        }
        if let Some(after) = remaining.strip_prefix("<?") {
            let end = after.find("?>")?;
            remaining = after[end + 2..].trim_start();
            continue;
        }
        if remaining.starts_with("<!") {
            // Skip a validated DOCTYPE/declaration, including an internal
            // subset whose quoted declarations may themselves contain `>`.
            let mut quote = None;
            let mut subset_depth = 0usize;
            let end = remaining.char_indices().find_map(|(index, ch)| match quote {
                Some(open) if ch == open => {
                    quote = None;
                    None
                }
                Some(_) => None,
                None if ch == '\'' || ch == '"' => {
                    quote = Some(ch);
                    None
                }
                None if ch == '[' => {
                    subset_depth += 1;
                    None
                }
                None if ch == ']' => {
                    subset_depth = subset_depth.saturating_sub(1);
                    None
                }
                None if ch == '>' && subset_depth == 0 => Some(index),
                None => None,
            })?;
            remaining = remaining[end + 1..].trim_start();
            continue;
        }
        let after_open = remaining.strip_prefix('<')?;
        let name_end = after_open
            .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '/' | '>'))
            .unwrap_or(after_open.len());
        let root_name = &after_open[..name_end];
        if root_name.rsplit(':').next()? != "svg" {
            return None;
        }
        break &after_open[name_end..];
    };
    let mut quote = None;
    let end = tail.char_indices().find_map(|(index, ch)| match quote {
        Some(open) if ch == open => {
            quote = None;
            None
        }
        Some(_) => None,
        None if ch == '\'' || ch == '"' => {
            quote = Some(ch);
            None
        }
        None if ch == '>' => Some(index),
        None => None,
    })?;
    let attributes = &tail[..end];
    let attribute = |name: &str| -> Option<&str> {
        let bytes = attributes.as_bytes();
        let mut cursor = 0;
        while cursor < bytes.len() {
            while cursor < bytes.len()
                && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b'/')
            {
                cursor += 1;
            }
            let name_start = cursor;
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && bytes[cursor] != b'='
            {
                cursor += 1;
            }
            let found = &attributes[name_start..cursor];
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() || bytes[cursor] != b'=' {
                continue;
            }
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() || !matches!(bytes[cursor], b'\'' | b'"') {
                continue;
            }
            let quote = bytes[cursor];
            cursor += 1;
            let value_start = cursor;
            while cursor < bytes.len() && bytes[cursor] != quote {
                cursor += 1;
            }
            let value = &attributes[value_start..cursor];
            cursor = cursor.saturating_add(1);
            if found == name {
                return Some(value);
            }
        }
        None
    };
    let length = |value: &str| -> Option<f32> {
        let value = value.trim();
        if value.is_empty() || value.ends_with('%') || value.eq_ignore_ascii_case("auto") {
            return None;
        }
        let split = value
            .find(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.' | 'e' | 'E')))
            .unwrap_or(value.len());
        let number = value[..split].parse::<f32>().ok()?;
        let factor = match value[split..].trim().to_ascii_lowercase().as_str() {
            "" | "px" => 1.0,
            "in" => 96.0,
            "cm" => 96.0 / 2.54,
            "mm" => 96.0 / 25.4,
            "q" => 96.0 / 101.6,
            "pt" => 96.0 / 72.0,
            "pc" => 16.0,
            _ => return None,
        };
        let result = number * factor;
        (result.is_finite() && result > 0.0).then_some(result)
    };
    let width = attribute("width").and_then(length);
    let height = attribute("height").and_then(length);
    let view_box_ratio = attribute("viewBox").and_then(|value| {
        let values = value
            .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
            .filter(|value| !value.is_empty())
            .map(str::parse::<f32>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        (values.len() == 4 && values[2].is_finite() && values[3].is_finite()
            && values[2] > 0.0 && values[3] > 0.0)
            .then_some(values[2] / values[3])
    });
    let ratio = match (width, height) {
        (Some(width), Some(height)) => Some(width / height),
        _ => view_box_ratio,
    };
    Some(crate::ReplacedIntrinsic {
        width,
        height,
        ratio,
    })
}

/// A full-pixmap clip mask admitting only the pixels inside `rect`, used to
/// crop an `object-fit: cover|none` image to its element box.
fn box_clip_mask(pw: u32, ph: u32, rect: &crate::Rect) -> Option<tiny_skia::Mask> {
    let mut mask = tiny_skia::Mask::new(pw, ph)?;
    let r = Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)?;
    let mut pb = PathBuilder::new();
    pb.push_rect(r);
    let path = pb.finish()?;
    mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
    Some(mask)
}

/// Rasterize the complete inherited overflow clip chain. Rectangular axis
/// bounds provide the cheap culling envelope; every rounded padding-box node
/// remains an independent path intersection so nested rounded scrollers do
/// not collapse to one bounding rectangle and leak through either corner.
fn overflow_clip_mask(
    pw: u32,
    ph: u32,
    clip: &crate::dom::OverflowClip,
    viewport: (f32, f32),
) -> Option<tiny_skia::Mask> {
    let bounds = clip.viewport_rect(viewport);
    let mut mask = box_clip_mask(pw, ph, &bounds)?;
    let offset = clip.rounded_offset();
    let mut current = clip.rounded_chain().map(AsRef::as_ref);
    while let Some(node) = current {
        let rect = crate::Rect {
            x: node.clip.rect.x + offset.0,
            y: node.clip.rect.y + offset.1,
            ..node.clip.rect
        };
        if let Some(path) =
            rounded_rect_path_radii(rect.x, rect.y, rect.width, rect.height, node.clip.radii)
        {
            mask.intersect_path(&path, FillRule::Winding, true, Transform::identity());
        }
        current = node.parent.as_deref();
    }
    Some(mask)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct OverflowClipMaskKey {
    bounds: [u32; 4],
    rounded_chain: usize,
    rounded_offset: [u32; 2],
}

type OverflowClipMaskCache = HashMap<
    OverflowClipMaskKey,
    (
        Option<Arc<crate::dom::RoundedOverflowClipChain>>,
        Arc<tiny_skia::Mask>,
    ),
>;

const MAX_OVERFLOW_CLIP_MASK_CACHE_ENTRIES: usize = 2;

fn overflow_clip_mask_key(
    clip: &crate::dom::OverflowClip,
    viewport: (f32, f32),
) -> OverflowClipMaskKey {
    let bounds = clip.viewport_rect(viewport);
    let rounded_offset = clip.rounded_offset();
    OverflowClipMaskKey {
        bounds: [
            bounds.x.to_bits(),
            bounds.y.to_bits(),
            bounds.width.to_bits(),
            bounds.height.to_bits(),
        ],
        rounded_chain: clip
            .rounded_chain()
            .map_or(0, |chain| Arc::as_ptr(chain) as usize),
        rounded_offset: [rounded_offset.0.to_bits(), rounded_offset.1.to_bits()],
    }
}

fn cached_overflow_clip_mask(
    cache: &mut OverflowClipMaskCache,
    pw: u32,
    ph: u32,
    clip: &crate::dom::OverflowClip,
    viewport: (f32, f32),
) -> Option<Arc<tiny_skia::Mask>> {
    let key = overflow_clip_mask_key(clip, viewport);
    if let Some((_, mask)) = cache.get(&key) {
        return Some(Arc::clone(mask));
    }
    let mask = Arc::new(overflow_clip_mask(pw, ph, clip, viewport)?);
    // A mask is one byte per output pixel. Two entries keep the ubiquitous
    // shared scrollport fast without turning a page with many distinct clips
    // into an unbounded surface-sized memory cache. DOM traversal is locally
    // ordered, so clearing at the bound retains the useful descendant burst.
    if cache.len() >= MAX_OVERFLOW_CLIP_MASK_CACHE_ENTRIES {
        cache.clear();
    }
    // `clip_scope_root` can synthesize a temporary rounded chain. Keeping its
    // root Arc alongside the pointer-based key prevents allocator reuse from
    // making a later, different chain alias this entry during the same paint.
    cache.insert(
        key,
        (clip.rounded_chain().cloned(), Arc::clone(&mask)),
    );
    Some(mask)
}

fn intersect_clip_masks(
    mut first: Option<tiny_skia::Mask>,
    second: Option<&tiny_skia::Mask>,
) -> Option<tiny_skia::Mask> {
    match (first.as_mut(), second) {
        (Some(first_mask), Some(second)) => {
            for (a, b) in first_mask.data_mut().iter_mut().zip(second.data()) {
                *a = ((*a as u16 * *b as u16 + 127) / 255) as u8;
            }
            first
        }
        (None, Some(second)) => Some(second.clone()),
        _ => first,
    }
}

fn rounded_box_clip_mask_radii(
    pw: u32,
    ph: u32,
    rect: &crate::Rect,
    radii: crate::ResolvedBorderRadii,
) -> Option<tiny_skia::Mask> {
    if radii.is_zero() {
        return box_clip_mask(pw, ph, rect);
    }
    let mut mask = tiny_skia::Mask::new(pw, ph)?;
    let path = rounded_rect_path_radii(rect.x, rect.y, rect.width, rect.height, radii)?;
    mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
    Some(mask)
}

/// Resolve a CSS polygon against its border-box reference and rasterize it as
/// a full-surface alpha clip. Gecko's basic-shape path builder likewise
/// resolves each x percentage against the reference width and each y
/// percentage against its height before closing the path.
fn polygon_clip_mask(
    pw: u32,
    ph: u32,
    polygon: &crate::ClipPathPolygon,
    rect: &crate::Rect,
    em: f32,
    rem: f32,
    viewport: (f32, f32),
) -> Option<tiny_skia::Mask> {
    let resolve = |coordinate: crate::Dimension, basis: f32| match coordinate.resolve(
        em,
        rem,
        viewport.0 / 100.0,
        viewport.1 / 100.0,
    ) {
        crate::Dimension::Px(value) => Some(value),
        crate::Dimension::Percent(value) => Some(value * basis),
        _ => None,
    };
    let mut builder = PathBuilder::new();
    for (index, &(x, y)) in polygon.points.iter().enumerate() {
        let x = rect.x + resolve(x, rect.width)?;
        let y = rect.y + resolve(y, rect.height)?;
        if index == 0 {
            builder.move_to(x, y);
        } else {
            builder.line_to(x, y);
        }
    }
    builder.close();
    let path = builder.finish();
    let fill_rule = match polygon.fill_rule {
        crate::ClipPathFillRule::Nonzero => FillRule::Winding,
        crate::ClipPathFillRule::Evenodd => FillRule::EvenOdd,
    };
    let mut mask = tiny_skia::Mask::new(pw, ph)?;
    // Degenerate polygons are valid CSS basic shapes but have no enclosed
    // area. Keep the newly zeroed mask in that case: treating it as "no mask"
    // would incorrectly reveal the whole element.
    if let Some(path) = path {
        mask.fill_path(&path, fill_rule, true, Transform::identity());
    }
    Some(mask)
}

/// Sniff SVG content: either an XML/SVG prolog, or a bare `<svg` root tag
/// (both are valid, and image responses commonly omit the XML declaration).
fn is_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(256)];
    let text = String::from_utf8_lossy(head);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with("<?xml") || trimmed.starts_with("<svg")
}

/// Rasterize SVG bytes to a `width` x `height` pixmap, scaled to fit (matching
/// how a replaced element like `<img>` sizes its intrinsic content).
fn render_svg(bytes: &[u8], width: u32, height: u32) -> Option<Pixmap> {
    let fonts = svg_font_database();
    render_svg_with_font_database(bytes, width, height, &fonts)
}

fn render_svg_with_font_database(
    bytes: &[u8],
    width: u32,
    height: u32,
    fonts: &std::sync::Arc<usvg::fontdb::Database>,
) -> Option<Pixmap> {
    if width == 0 || height == 0 {
        return None;
    }
    let mut opts = usvg::Options::default();
    // The outer replaced element supplies the SVG document viewport. Force
    // that used CSS size onto the root before usvg resolves `viewBox`:
    // a missing height is represented as 100%, which usvg otherwise resolves
    // against the viewBox height itself. `<svg width=32 viewBox="0 0 223
    // 236">` would therefore become a 32x236 viewport; its artwork is fitted
    // into a thin centered strip and then the whole strip is scaled to 32x34.
    // Author `preserveAspectRatio` still controls fitting inside this viewport.
    // usvg resolves root dimensions before an injected stylesheet can
    // override them, so provide the used viewport as actual root attributes.
    let viewport_svg = svg_with_root_viewport(bytes, width, height)?;
    opts.default_size = usvg::Size::from_wh(width as f32, height as f32)?;
    opts.font_family = "Liberation Serif".to_string();
    opts.fontdb = std::sync::Arc::clone(fonts);
    let tree = usvg::Tree::from_data(&viewport_svg, &opts).ok()?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        return None;
    }
    let mut svg_pixmap = Pixmap::new(width, height)?;
    let transform =
        Transform::from_scale(width as f32 / size.width(), height as f32 / size.height());
    resvg::render(&tree, transform, &mut svg_pixmap.as_mut());
    Some(svg_pixmap)
}

/// A deterministic font database shared by every SVG raster in the process.
/// Constructing/scanning a database per icon is far too expensive for pages
/// with SVG-heavy navigation and would be prohibitive for future repeated
/// frame capture. The embedded faces are the same stable browser-generic
/// families used by the HTML text engine.
fn svg_font_database() -> std::sync::Arc<usvg::fontdb::Database> {
    static DATABASE: std::sync::OnceLock<std::sync::Arc<usvg::fontdb::Database>> =
        std::sync::OnceLock::new();
    std::sync::Arc::clone(DATABASE.get_or_init(|| {
        let mut database = usvg::fontdb::Database::new();
        for bytes in [
            FONT_BYTES,
            FONT_BOLD_BYTES,
            FONT_OBLIQUE_BYTES,
            FONT_BOLD_OBLIQUE_BYTES,
            SERIF_FONT_BYTES,
            MONO_FONT_BYTES,
        ] {
            database.load_font_data(bytes.to_vec());
        }
        database.set_sans_serif_family("Liberation Sans");
        database.set_serif_family("Liberation Serif");
        database.set_monospace_family("Liberation Mono");
        std::sync::Arc::new(database)
    }))
}

fn svg_font_database_with_web_fonts(
    web_fonts: &[crate::inline::WebFont],
) -> std::sync::Arc<usvg::fontdb::Database> {
    let base = svg_font_database();
    if web_fonts.is_empty() {
        return base;
    }
    // `fontdb::Database::clone` shares each binary source; only the page's
    // already-decoded user fonts allocate here, once per page rather than once
    // per SVG. HTML shaping needs the same bytes, so retaining both databases
    // is the cost of keeping the rasterizer and layout engine deterministic.
    let mut database = (*base).clone();
    for font in web_fonts {
        database.load_font_data(font.data.clone());
    }
    std::sync::Arc::new(database)
}

fn has_inline_svg_text(tree: &DomTree) -> bool {
    crate::dom::rendered_descendants(tree, tree.document())
        .into_iter()
        .any(|nid| {
        tree.get_node(nid).is_some_and(|node| {
            node.as_element()
                .is_some_and(|name| matches!(name.local.as_ref(), "text" | "tspan" | "textPath"))
        })
        })
}

/// Return SVG XML whose root `width`/`height` are the resolved CSS viewport.
///
/// This is deliberately a narrow XML start-tag rewrite rather than a DOM
/// reserialization: all namespaces, styles, definitions, and source order
/// remain byte-for-byte intact. Existing attribute values are replaced;
/// missing ones are appended. Quoted `>` characters are respected while
/// finding the end of the root tag.
fn svg_with_root_viewport(bytes: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let source = std::str::from_utf8(bytes).ok()?;
    let start = source.find("<svg")?;
    let tail = &source[start..];
    let mut quote = None;
    let mut tag_end = None;
    for (offset, ch) in tail.char_indices() {
        match (quote, ch) {
            (Some(open), close) if close == open => quote = None,
            (None, '"' | '\'') => quote = Some(ch),
            (None, '>') => {
                tag_end = Some(start + offset);
                break;
            }
            _ => {}
        }
    }
    let tag_end = tag_end?;
    let mut root = source[start..=tag_end].to_string();
    for (name, value) in [("width", width), ("height", height)] {
        if let Some((value_start, value_end)) = svg_root_attr_value_range(&root, name) {
            root.replace_range(value_start..value_end, &value.to_string());
        } else {
            root.insert_str(root.len() - 1, &format!(" {name}=\"{value}\""));
        }
    }

    let mut output = String::with_capacity(source.len() + 32);
    output.push_str(&source[..start]);
    output.push_str(&root);
    output.push_str(&source[tag_end + 1..]);
    Some(output.into_bytes())
}

/// Value byte range for one attribute in an `<svg ...>` start tag.
fn svg_root_attr_value_range(tag: &str, wanted: &str) -> Option<(usize, usize)> {
    let bytes = tag.as_bytes();
    let mut index = "<svg".len();
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] == b'>' || bytes[index] == b'/' {
            return None;
        }
        let name_start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && bytes[index] != b'='
            && bytes[index] != b'>'
            && bytes[index] != b'/'
        {
            index += 1;
        }
        let name_end = index;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            continue;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let (value_start, value_end) =
            if index < bytes.len() && matches!(bytes[index], b'"' | b'\'') {
                let delimiter = bytes[index];
                index += 1;
                let start = index;
                while index < bytes.len() && bytes[index] != delimiter {
                    index += 1;
                }
                let end = index;
                index = (index + 1).min(bytes.len());
                (start, end)
            } else {
                let start = index;
                while index < bytes.len()
                    && !bytes[index].is_ascii_whitespace()
                    && bytes[index] != b'>'
                    && bytes[index] != b'/'
                {
                    index += 1;
                }
                (start, index)
            };
        if &tag[name_start..name_end] == wanted {
            return Some((value_start, value_end));
        }
    }
    None
}

/// Serialize an inline `<svg>` subtree (rooted at `root`) back to a standalone
/// SVG document string. Emits `<tag attr="v">children</tag>` for the element
/// and every descendant, preserving the root's `viewBox`/`width`/`height` and
/// all `<defs>`/`<symbol>`/`<use>`/`<path>` structure so resvg can rasterize it
/// as a self-contained document. SVG is XML-clean, so there are no HTML
/// void-element or optional-close rules to apply; every element gets an
/// explicit closing tag. The root gains an `xmlns` declaration when it lacks
/// one (common for inline svg, whose namespace is implied by the HTML parser
/// but required for usvg to parse the string on its own).
#[cfg(test)]
fn serialize_svg(tree: &DomTree, root: obscura_dom::tree::NodeId) -> String {
    let mut buf = String::new();
    serialize_svg_node(tree, root, true, None, None, None, &mut buf);
    buf
}

/// Serialize an inline SVG while carrying the page's computed author styling
/// into the standalone document consumed by resvg. External page stylesheets
/// are otherwise outside that document, so class-driven SVG text and icons
/// silently fall back to presentation attributes or SVG defaults.
fn serialize_svg_styled(
    tree: &DomTree,
    root: obscura_dom::tree::NodeId,
    styles: &std::collections::HashMap<obscura_dom::tree::NodeId, crate::LayoutStyle>,
    custom_properties: &std::collections::HashMap<
        obscura_dom::tree::NodeId,
        std::rc::Rc<std::collections::HashMap<String, String>>,
    >,
    suppress_opacity_for: Option<obscura_dom::tree::NodeId>,
) -> String {
    let mut buf = String::new();
    serialize_svg_node(
        tree,
        root,
        true,
        Some(styles),
        Some(custom_properties),
        suppress_opacity_for,
        &mut buf,
    );
    buf
}

fn inject_svg_current_color(markup: &mut String, color: [u8; 4]) {
    let Some(start) = markup.find("<svg") else {
        return;
    };
    let Some(end) = markup[start..].find('>').map(|offset| start + offset) else {
        return;
    };
    let root = &markup[start..end];
    // An explicit presentation attribute already survives serialization and
    // is the correct local currentColor source.
    if root.contains(" color=") {
        return;
    }
    let attribute = format!(
        " color=\"#{:02x}{:02x}{:02x}\"",
        color[0], color[1], color[2]
    );
    markup.insert_str(start + "<svg".len(), &attribute);
}

fn serialize_svg_node(
    tree: &DomTree,
    nid: obscura_dom::tree::NodeId,
    is_root: bool,
    styles: Option<&std::collections::HashMap<obscura_dom::tree::NodeId, crate::LayoutStyle>>,
    custom_properties: Option<
        &std::collections::HashMap<
            obscura_dom::tree::NodeId,
            std::rc::Rc<std::collections::HashMap<String, String>>,
        >,
    >,
    suppress_opacity_for: Option<obscura_dom::tree::NodeId>,
    buf: &mut String,
) {
    let node = match tree.get_node(nid) {
        Some(n) => n,
        None => return,
    };
    if let Some(text) = node.text_content_of_text_node() {
        svg_escape_text(text, buf);
        return;
    }
    let name = match node.as_element() {
        Some(n) => n,
        // Document/comment/PI: no tag of its own, emit only element children.
        None => {
            for child in tree.children(nid) {
                serialize_svg_node(
                    tree,
                    child,
                    false,
                    styles,
                    custom_properties,
                    suppress_opacity_for,
                    buf,
                );
            }
            return;
        }
    };
    let tag = name.local.as_ref();
    buf.push('<');
    buf.push_str(tag);
    let mut has_xmlns = false;
    let mut source_style: Option<String> = None;
    if let Some(attrs) = node.attrs() {
        for attr in attrs {
            // Emit the local name only, dropping any prefix (`xlink:href` ->
            // `href`): resvg reads both, and a bare local avoids needing an
            // `xmlns:xlink` declaration in the standalone document.
            let aname = attr.name.local.as_ref();
            // HTML frameworks commonly stamp hydration attributes such as
            // `q:id` onto inline SVG. In an HTML document that name is fine,
            // but our standalone XML serialization has no matching `xmlns:q`,
            // so one irrelevant attribute makes usvg reject the entire logo.
            // Namespace-aware attributes arrive with a clean local name;
            // discard only literal, unbound colon names from the HTML parser.
            if aname.contains(':') {
                continue;
            }
            if aname == "xmlns" {
                has_xmlns = true;
            }
            if styles.is_some() && aname == "style" {
                source_style = Some(attr.value.to_string());
                continue;
            }
            let value = if styles.is_some() && svg_css_presentation_attribute(aname) {
                let empty = std::collections::HashMap::new();
                let properties = custom_properties
                    .and_then(|all| all.get(&nid))
                    .map_or(&empty, std::rc::Rc::as_ref);
                let Some(resolved) = resolve_svg_presentation_value(
                    aname,
                    attr.value.as_ref(),
                    properties,
                ) else {
                    // A var() failure makes the declaration invalid at
                    // computed value time. Omitting this low-specificity
                    // presentation attribute lets usvg apply the property's
                    // inherited or initial value, matching the browser
                    // cascade.
                    continue;
                };
                resolved
            } else {
                std::borrow::Cow::Borrowed(attr.value.as_ref())
            };
            buf.push(' ');
            buf.push_str(aname);
            buf.push_str("=\"");
            svg_escape_attr(&value, buf);
            buf.push('"');
        }
    }
    if styles.is_some() {
        let mut declarations = String::new();
        if let Some(source) = source_style.as_deref() {
            declarations.push_str(source.trim());
            if !declarations.is_empty() && !declarations.ends_with(';') {
                declarations.push(';');
            }
        }
        let mut append = |name: &str, value: &str| {
            if value.trim().is_empty() {
                return;
            }
            declarations.push_str(name);
            declarations.push(':');
            declarations.push_str(value);
            declarations.push_str("!important;");
        };
        if let Some(computed) = styles.and_then(|all| all.get(&nid)) {
            let empty = std::collections::HashMap::new();
            let properties = custom_properties
                .and_then(|all| all.get(&nid))
                .map_or(&empty, std::rc::Rc::as_ref);
            if let Some(value) = computed.svg_fill.as_deref() {
                if let Some(value) = resolve_svg_presentation_value("fill", value, properties) {
                    append(
                        "fill",
                        if value.eq_ignore_ascii_case("currentcolor") {
                            "currentColor"
                        } else {
                            value.as_ref()
                        },
                    );
                }
            }
            if let Some(value) = computed.svg_stroke.as_deref() {
                if let Some(value) = resolve_svg_presentation_value("stroke", value, properties) {
                    append(
                        "stroke",
                        if value.eq_ignore_ascii_case("currentcolor") {
                            "currentColor"
                        } else {
                            value.as_ref()
                        },
                    );
                }
            }
            if let Some(value) = computed.svg_stroke_width.as_deref() {
                if let Some(value) =
                    resolve_svg_presentation_value("stroke-width", value, properties)
                {
                    append("stroke-width", value.as_ref());
                }
            }
            if matches!(tag, "svg" | "text" | "textPath" | "textpath" | "tspan") {
                if let Some(value) = computed.font_size {
                    append("font-size", &format!("{value}px"));
                }
                if let Some(value) = computed.font_weight.as_deref() {
                    append("font-weight", value);
                }
                if let Some(value) = computed.font_family.as_deref() {
                    append("font-family", value);
                }
                if computed.font_style_italic == Some(true) {
                    append("font-style", "italic");
                }
            }
            if let Some(color) = computed.color {
                append(
                    "color",
                    &format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]),
                );
            }
            if suppress_opacity_for == Some(nid) {
                // The HTML paint layer applies this SVG root's opacity after
                // resvg returns. Override both a source style declaration and
                // the computed value here so the group is not faded twice.
                append("opacity", "1");
            } else {
                if let Some(opacity) = computed.opacity {
                    append("opacity", &opacity.to_string());
                }
            }
        }
        if !declarations.is_empty() {
            buf.push_str(" style=\"");
            svg_escape_attr(&declarations, buf);
            buf.push('"');
        }
    }
    if is_root && !has_xmlns {
        buf.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
    }
    buf.push('>');
    for child in tree.children(nid) {
        serialize_svg_node(
            tree,
            child,
            false,
            styles,
            custom_properties,
            suppress_opacity_for,
            buf,
        );
    }
    buf.push_str("</");
    buf.push_str(tag);
    buf.push('>');
}

/// SVG attributes which participate in the CSS cascade. Blink exposes the
/// first group through `SVGElement::CssPropertyIdForSVGAttributeName`; the
/// final geometry group is backed by per-element SVG animated properties with
/// CSS property IDs. XML-only attributes such as `d`, `viewBox`, and `id` must
/// remain literal even when their text happens to contain `var()`.
fn svg_css_presentation_attribute(name: &str) -> bool {
    matches!(
        name,
        "alignment-baseline"
            | "baseline-shift"
            | "buffered-rendering"
            | "clip"
            | "clip-path"
            | "clip-rule"
            | "color"
            | "color-interpolation"
            | "color-interpolation-filters"
            | "color-rendering"
            | "cursor"
            | "direction"
            | "display"
            | "dominant-baseline"
            | "fill"
            | "fill-opacity"
            | "fill-rule"
            | "filter"
            | "flood-color"
            | "flood-opacity"
            | "font-family"
            | "font-size"
            | "font-stretch"
            | "font-style"
            | "font-variant"
            | "font-weight"
            | "image-rendering"
            | "letter-spacing"
            | "lighting-color"
            | "marker-end"
            | "marker-mid"
            | "marker-start"
            | "mask"
            | "mask-type"
            | "opacity"
            | "overflow"
            | "paint-order"
            | "pointer-events"
            | "shape-rendering"
            | "stop-color"
            | "stop-opacity"
            | "stroke"
            | "stroke-dasharray"
            | "stroke-dashoffset"
            | "stroke-linecap"
            | "stroke-linejoin"
            | "stroke-miterlimit"
            | "stroke-opacity"
            | "stroke-width"
            | "text-anchor"
            | "text-decoration"
            | "text-rendering"
            | "transform-origin"
            | "unicode-bidi"
            | "vector-effect"
            | "visibility"
            | "word-spacing"
            | "writing-mode"
            | "x"
            | "y"
            | "cx"
            | "cy"
            | "r"
            | "rx"
            | "ry"
            | "width"
            | "height"
    )
}

fn resolve_svg_presentation_value<'a>(
    name: &str,
    value: &'a str,
    properties: &std::collections::HashMap<String, String>,
) -> Option<std::borrow::Cow<'a, str>> {
    if !value.contains("var(") {
        return Some(std::borrow::Cow::Borrowed(value));
    }
    let resolved = crate::css::substitute_var_value(value, properties, 0)?;
    if resolved.trim().is_empty()
        || svg_presentation_substitution_is_guaranteed_invalid(name, &resolved)
    {
        return None;
    }
    Some(std::borrow::Cow::Owned(resolved))
}

/// Detect only values whose post-substitution grammar is unambiguously
/// invalid. Functional and escaped values are left to usvg because its SVG
/// paint grammar is broader than our HTML color parser. An unknown bare
/// identifier, however, cannot be a color/paint and must compute to
/// inherit/initial instead of triggering usvg's black error fallback.
fn svg_presentation_substitution_is_guaranteed_invalid(name: &str, value: &str) -> bool {
    if !matches!(
        name,
        "color" | "fill" | "flood-color" | "lighting-color" | "stop-color" | "stroke"
    ) {
        return false;
    }
    let value = value.trim();
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || crate::style::parse_color(value).is_some()
    {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    if matches!(name, "fill" | "stroke")
        && matches!(lower.as_str(), "none" | "context-fill" | "context-stroke")
    {
        return false;
    }
    !matches!(
        lower.as_str(),
        "currentcolor"
            | "inherit"
            | "initial"
            | "unset"
            | "revert"
            | "revert-layer"
            | "accentcolor"
            | "accentcolortext"
            | "activetext"
            | "buttonborder"
            | "buttonface"
            | "buttontext"
            | "canvas"
            | "canvastext"
            | "field"
            | "fieldtext"
            | "graytext"
            | "highlight"
            | "highlighttext"
            | "linktext"
            | "mark"
            | "marktext"
            | "selecteditem"
            | "selecteditemtext"
            | "visitedtext"
    )
}

fn svg_escape_text(s: &str, buf: &mut String) {
    for c in s.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            _ => buf.push(c),
        }
    }
}

fn svg_escape_attr(s: &str, buf: &mut String) {
    for c in s.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '<' => buf.push_str("&lt;"),
            '"' => buf.push_str("&quot;"),
            _ => buf.push(c),
        }
    }
}

/// Resolve `<use>` elements in an inline `<svg>` subtree against either a
/// document-level symbol sprite (`href="#id"`) or an external sprite file
/// (`href="url#id"`), splicing the referenced symbol into the standalone SVG
/// handed to resvg. Symbols already inside `root` need no injection.
fn inject_external_sprites(
    tree: &DomTree,
    root: obscura_dom::tree::NodeId,
    styles: Option<&std::collections::HashMap<obscura_dom::tree::NodeId, crate::LayoutStyle>>,
    custom_properties: Option<
        &std::collections::HashMap<
            obscura_dom::tree::NodeId,
            std::rc::Rc<std::collections::HashMap<String, String>>,
        >,
    >,
    base_url: Option<&str>,
    markup: &mut String,
    cache: &mut RenderResourceCache,
    sprite_cache: &mut std::collections::HashMap<String, Option<String>>,
) {
    // Distinct external references (full href, url, fragment id), in first-seen
    // order. Dedupe so one symbol referenced by several `<use>` is fetched and
    // injected once (the rewrite below still fixes every occurrence).
    let root_descendants = tree.descendants(root);
    let mut refs: Vec<(String, String, String)> = Vec::new();
    let mut local_fragments = Vec::new();
    for nid in tree.descendants(root) {
        let Some(node) = tree.get_node(nid) else {
            continue;
        };
        let Some(el) = node.as_element() else {
            continue;
        };
        if el.local.as_ref() != "use" {
            continue;
        }
        // `get_attribute` matches by local name, so a single "href" lookup
        // already covers both `href` and `xlink:href`; check the prefixed form
        // too for completeness.
        let Some(href) = node
            .get_attribute("href")
            .or_else(|| node.get_attribute("xlink:href"))
        else {
            continue;
        };
        let Some(hash) = href.find('#') else { continue };
        let (url, frag) = (&href[..hash], &href[hash + 1..]);
        if frag.is_empty() {
            continue;
        }
        if url.is_empty() {
            if !local_fragments.iter().any(|existing| existing == frag) {
                local_fragments.push(frag.to_string());
            }
            continue;
        }
        let entry = (href.to_string(), url.to_string(), frag.to_string());
        if !refs.contains(&entry) {
            refs.push(entry);
        }
    }
    let mut defs = String::new();
    let mut rewrites: Vec<(String, String)> = Vec::new();
    let wanted_local: std::collections::HashSet<&str> =
        local_fragments.iter().map(String::as_str).collect();
    let mut local_nodes = std::collections::HashMap::new();
    if !wanted_local.is_empty() {
        for nid in tree.descendants(tree.document()) {
            let Some(node) = tree.get_node(nid) else {
                continue;
            };
            let Some(id) = node.get_attribute("id") else {
                continue;
            };
            if wanted_local.contains(id) {
                local_nodes.entry(id.to_string()).or_insert(nid);
            }
        }
    }
    for frag in local_fragments {
        let Some(&symbol_id) = local_nodes.get(&frag) else {
            continue;
        };
        if symbol_id == root || root_descendants.contains(&symbol_id) {
            continue;
        }
        serialize_svg_node(
            tree,
            symbol_id,
            false,
            styles,
            custom_properties,
            None,
            &mut defs,
        );
    }
    for (href, url, frag) in &refs {
        let key = format!("{url}#{frag}");
        let symbol = sprite_cache
            .entry(key)
            .or_insert_with(|| {
                let bytes = fetch_bytes(url, base_url, cache)?;
                let text = String::from_utf8_lossy(&bytes);
                // Drop `xlink:` prefixes in the fetched fragment (resvg reads a
                // bare `href`), matching how the local subtree is serialized and
                // avoiding an undeclared-namespace parse error in the standalone
                // document.
                extract_svg_element_by_id(&text, frag).map(|s| s.replace("xlink:href", "href"))
            })
            .clone();
        let Some(symbol) = symbol else { continue };
        let empty = std::collections::HashMap::new();
        let properties = custom_properties
            .and_then(|all| all.get(&root))
            .map_or(&empty, std::rc::Rc::as_ref);
        defs.push_str(&resolve_svg_markup_presentation_vars(
            &symbol,
            properties,
        ));
        rewrites.push((href.clone(), format!("#{frag}")));
    }
    if defs.is_empty() {
        return;
    }

    // Splice the fetched symbols into a `<defs>` immediately after the opening
    // `<svg ...>` tag (the first `>` in the serialized document).
    if let Some(gt) = markup.find('>') {
        markup.insert_str(gt + 1, &format!("<defs>{defs}</defs>"));
    }
    // Point each external `<use>` at the injected local symbol. The serialized
    // href is attribute-escaped, so match against the escaped form.
    for (href, local) in rewrites {
        let from = format!("href=\"{}\"", svg_escape_attr_str(&href));
        let to = format!("href=\"{}\"", svg_escape_attr_str(&local));
        *markup = markup.replace(&from, &to);
    }
}

/// External sprite fragments are not part of the page DOM, so they have no
/// `LayoutStyle` entry to carry into the standalone resvg document. Resolve
/// CSS-variable presentation attributes against the referencing SVG's
/// inherited custom properties (or their authored fallbacks) while preserving
/// XML-only attributes byte-for-byte.
fn resolve_svg_markup_presentation_vars(
    markup: &str,
    properties: &std::collections::HashMap<String, String>,
) -> String {
    if !markup.contains("var(") {
        return markup.to_string();
    }
    let mut output = String::with_capacity(markup.len());
    let mut cursor = 0;
    while let Some(relative_start) = markup[cursor..].find('<') {
        let start = cursor + relative_start;
        output.push_str(&markup[cursor..start]);
        let Some(end) = svg_markup_tag_end(markup, start) else {
            output.push_str(&markup[start..]);
            return output;
        };
        output.push_str(&resolve_svg_tag_presentation_vars(
            &markup[start..=end],
            properties,
        ));
        cursor = end + 1;
    }
    output.push_str(&markup[cursor..]);
    output
}

fn svg_markup_tag_end(markup: &str, start: usize) -> Option<usize> {
    let bytes = markup.as_bytes();
    let mut quote = None;
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(active) = quote {
            if byte == active {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if byte == b'>' {
            return Some(cursor);
        }
        cursor += 1;
    }
    None
}

fn resolve_svg_tag_presentation_vars(
    tag: &str,
    properties: &std::collections::HashMap<String, String>,
) -> String {
    let bytes = tag.as_bytes();
    if bytes.len() < 3
        || bytes[0] != b'<'
        || matches!(bytes[1], b'/' | b'!' | b'?')
    {
        return tag.to_string();
    }
    let mut cursor = 1;
    while cursor < bytes.len()
        && !bytes[cursor].is_ascii_whitespace()
        && !matches!(bytes[cursor], b'/' | b'>')
    {
        cursor += 1;
    }
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    while cursor < bytes.len() {
        let attribute_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || matches!(bytes[cursor], b'/' | b'>') {
            break;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && !matches!(bytes[cursor], b'=' | b'/' | b'>')
        {
            cursor += 1;
        }
        let name_end = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b'=' {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }
        let quote = matches!(bytes[cursor], b'\'' | b'"').then_some(bytes[cursor]);
        if quote.is_some() {
            cursor += 1;
        }
        let value_start = cursor;
        while cursor < bytes.len()
            && if let Some(quote) = quote {
                bytes[cursor] != quote
            } else {
                !bytes[cursor].is_ascii_whitespace() && !matches!(bytes[cursor], b'/' | b'>')
            }
        {
            cursor += 1;
        }
        let value_end = cursor;
        if quote.is_some() && cursor < bytes.len() {
            cursor += 1;
        }
        let name = &tag[name_start..name_end];
        let value = &tag[value_start..value_end];
        if svg_css_presentation_attribute(name) && value.contains("var(") {
            match resolve_svg_presentation_value(name, value, properties) {
                Some(std::borrow::Cow::Owned(resolved)) => {
                    replacements.push((value_start, value_end, resolved));
                }
                Some(std::borrow::Cow::Borrowed(_)) => {}
                None => replacements.push((attribute_start, cursor, String::new())),
            }
        }
    }
    if replacements.is_empty() {
        return tag.to_string();
    }
    let mut resolved = tag.to_string();
    for (start, end, replacement) in replacements.into_iter().rev() {
        resolved.replace_range(start..end, &replacement);
    }
    resolved
}

/// Escape a string for use as an SVG attribute value (`&`, `<`, `"`), returning
/// it as an owned `String` (the buffer-writing `svg_escape_attr` in one call).
fn svg_escape_attr_str(s: &str) -> String {
    let mut buf = String::new();
    svg_escape_attr(s, &mut buf);
    buf
}

/// Pull the element carrying `id="id"` (a `<symbol>`, `<g>`, `<path>`, ...) out
/// of an external sprite document, returned as a verbatim serialized substring
/// (its start tag through the matching end tag, or the self-closing tag alone).
/// A lightweight namespace-agnostic XML scan, not a full parse: usvg would
/// flatten `<symbol>`/`<use>` structure, and we want to re-inject the element
/// unchanged. Returns None when no element has that id.
fn extract_svg_element_by_id(sprite: &str, id: &str) -> Option<String> {
    let mut i = 0usize;
    while i < sprite.len() {
        let rest = &sprite[i..];
        if !rest.starts_with('<') {
            // Advance to the next tag (skips text/whitespace between elements).
            i += rest.find('<')?;
            continue;
        }
        if rest.starts_with("<!--") {
            i += rest.find("-->").map(|p| p + 3)?;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i += rest.find("]]>").map(|p| p + 3)?;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") || rest.starts_with("</") {
            i += rest.find('>').map(|p| p + 1)?;
            continue;
        }
        // A start tag: inner spans between '<' and '>'.
        let gt = i + rest.find('>')?;
        let inner = &sprite[i + 1..gt];
        if tag_attr(inner, "id") == Some(id) {
            if inner.trim_end().ends_with('/') {
                return Some(sprite[i..=gt].to_string());
            }
            let name = tag_name(inner);
            let end = element_end(sprite, gt + 1, name)?;
            return Some(sprite[i..end].to_string());
        }
        i = gt + 1;
    }
    None
}

/// The tag name from a tag's inner text (the bytes between `<` and `>`),
/// dropping any leading `/` of an end tag and stopping at the first whitespace
/// or self-close slash.
fn tag_name(inner: &str) -> &str {
    let inner = inner.trim_start().trim_start_matches('/');
    let end = inner
        .find(|c: char| c.is_ascii_whitespace() || c == '/')
        .unwrap_or(inner.len());
    &inner[..end]
}

/// The value of attribute `want` in a tag's inner text, or None if absent.
/// Matches attribute names whole (so `id` does not match `data-id`/`xml:id`)
/// and handles single/double quoted and bare values.
fn tag_attr<'a>(inner: &'a str, want: &str) -> Option<&'a str> {
    let b = inner.as_bytes();
    let mut i = 0usize;
    // Skip the tag name.
    while i < b.len() && !b[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] == b'/' {
            break;
        }
        let name_start = i;
        while i < b.len() && b[i] != b'=' && !b[i].is_ascii_whitespace() && b[i] != b'/' {
            i += 1;
        }
        let name = &inner[name_start..i];
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let value = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let quote = b[i];
                i += 1;
                let vstart = i;
                while i < b.len() && b[i] != quote {
                    i += 1;
                }
                let v = &inner[vstart..i.min(b.len())];
                if i < b.len() {
                    i += 1;
                }
                v
            } else {
                let vstart = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'/' {
                    i += 1;
                }
                &inner[vstart..i]
            };
            if name == want {
                return Some(value);
            }
        } else if name == want {
            // Valueless (boolean) attribute.
            return Some("");
        }
    }
    None
}

/// The byte offset just past the `</name>` that closes an element whose content
/// starts at `start`, tracking nesting of same-named tags (e.g. `<g>` inside
/// `<g>`). None if the document ends without a matching close.
fn element_end(sprite: &str, start: usize, name: &str) -> Option<usize> {
    let mut i = start;
    let mut depth = 1usize;
    while i < sprite.len() {
        let rest = &sprite[i..];
        if !rest.starts_with('<') {
            i += rest.find('<')?;
            continue;
        }
        if rest.starts_with("<!--") {
            i += rest.find("-->").map(|p| p + 3)?;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i += rest.find("]]>").map(|p| p + 3)?;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            i += rest.find('>').map(|p| p + 1)?;
            continue;
        }
        let gt = i + rest.find('>')?;
        let inner = &sprite[i + 1..gt];
        if rest.starts_with("</") {
            if tag_name(inner) == name {
                depth -= 1;
                if depth == 0 {
                    return Some(gt + 1);
                }
            }
        } else if tag_name(inner) == name && !inner.trim_end().ends_with('/') {
            depth += 1;
        }
        i = gt + 1;
    }
    None
}

/// Paint a `mask-image`: the ubiquitous "colored, scalable icon" pattern,
/// where an SVG shape is used purely as a stencil and tinted by
/// `background-color`/`color` rather than carrying its own colors. Fetches
/// and rasterizes the mask the same way as an ordinary image, then repaints
/// every pixel it covers as `fill`, weighted by the mask's own alpha there
/// (its "coverage"), instead of drawing the mask's own pixel colors.
fn paint_mask(
    src: &str,
    base_url: Option<&str>,
    rect: &crate::Rect,
    border_radius: crate::ResolvedBorderRadii,
    fill: [u8; 4],
    radial_gradient: Option<&((f32, f32), Vec<([u8; 4], Option<f32>)>)>,
    radial_geometry: Option<crate::RadialGradientGeometry>,
    em: f32,
    root_font_size: f32,
    viewport: (f32, f32),
    linear_gradient: Option<&(f32, Vec<([u8; 4], Option<f32>)>)>,
    conic_gradient: Option<&(f32, (f32, f32), Vec<([u8; 4], Option<f32>)>)>,
    mask_size: Option<(f32, f32)>,
    mask_repeat: Option<(bool, bool)>,
    extra_clip: Option<&tiny_skia::Mask>,
    pixmap: &mut Pixmap,
    cache: &mut RenderResourceCache,
) -> bool {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return false;
    }
    // Masks likewise force the CSS-pixel raster path. Avoid decoding and the
    // O(box area) recolor loop when their destination misses this capture.
    if !rect_intersects_paint_surface(rect, pixmap, 1.0) {
        return false;
    }
    let Some(bytes) = fetch_bytes(src, base_url, cache) else {
        return false;
    };
    let (box_width, box_height) = (rect.width.ceil() as u32, rect.height.ceil() as u32);
    let (tile_width, tile_height) = mask_size
        .map(|(width, height)| (width.max(1.0).ceil() as u32, height.max(1.0).ceil() as u32))
        .unwrap_or((box_width, box_height));
    let mask = if is_svg(&bytes) {
        render_svg(&bytes, tile_width, tile_height)
    } else {
        raster_to_pixmap(&bytes, tile_width, tile_height)
    };
    let Some(mask) = mask else { return false };

    let repeat = if mask_size.is_some() {
        mask_repeat.unwrap_or((true, true))
    } else {
        mask_repeat.unwrap_or((false, false))
    };
    let normalized_linear = linear_gradient.map(|(_, stops)| normalized_stops(stops));
    let normalized_conic = conic_gradient.map(|(_, _, stops)| normalized_stops(stops));
    let normalized_radial = radial_gradient.map(|(_, stops)| normalized_stops(stops));
    let Some(mut recolored) = Pixmap::new(box_width, box_height) else {
        return false;
    };
    for y in 0..box_height {
        if !repeat.1 && y >= tile_height {
            continue;
        }
        let tile_y = if repeat.1 { y % tile_height } else { y };
        for x in 0..box_width {
            if !repeat.0 && x >= tile_width {
                continue;
            }
            let tile_x = if repeat.0 { x % tile_width } else { x };
            let coverage = mask.pixels()[(tile_y * tile_width + tile_x) as usize].alpha() as u32;
            if coverage == 0 {
                continue;
            }
            let sample_x = rect.x + x as f32 + 0.5;
            let sample_y = rect.y + y as f32 + 0.5;
            let mut color = if let (Some((angle, center, _)), Some(stops)) =
                (conic_gradient, normalized_conic.as_deref())
            {
                conic_color_at(rect, *angle, *center, stops, sample_x, sample_y)
            } else if let (Some((angle, _)), Some(stops)) =
                (linear_gradient, normalized_linear.as_deref())
            {
                linear_color_at(rect, *angle, stops, sample_x, sample_y)
            } else if let (Some((center, _)), Some(stops)) =
                (radial_gradient, normalized_radial.as_deref())
            {
                radial_color_at(
                    rect,
                    *center,
                    stops,
                    radial_geometry,
                    em,
                    root_font_size,
                    viewport,
                    sample_x,
                    sample_y,
                )
            } else {
                fill
            };
            color[3] = ((color[3] as u32 * coverage) / 255) as u8;
            recolored.pixels_mut()[(y * box_width + x) as usize] = premultiplied(color);
        }
    }
    let mut clip = extra_clip.cloned();
    if !border_radius.is_zero() {
        let path = rounded_rect_path_radii(rect.x, rect.y, rect.width, rect.height, border_radius);
        match (clip.as_mut(), path) {
            (Some(mask), Some(path)) => {
                mask.intersect_path(&path, FillRule::Winding, true, Transform::identity())
            }
            (None, _) => {
                clip = rounded_box_clip_mask_radii(
                    pixmap.width(),
                    pixmap.height(),
                    rect,
                    border_radius,
                );
            }
            _ => {}
        }
    }
    pixmap.draw_pixmap(
        rect.x.floor() as i32,
        rect.y.floor() as i32,
        recolored.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        Transform::identity(),
        clip.as_ref(),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::layout_dom_with_web_fonts;
    use obscura_dom::tree::ShadowRootMode;
    use obscura_dom::tree_sink::parse_html;

    #[test]
    fn native_shadow_flat_tree_paints_shadow_and_slotted_content_only() {
        let tree = parse_html(
            r#"<html style="background:white"><body style="margin:0"><x-card id="host" style="display:block;width:30px"><span id="light" style="display:block;height:10px;background:rgb(255,0,0)"></span><span id="unslotted" slot="missing" style="display:block;height:10px;background:rgb(0,255,0)"></span></x-card><div id="source"><div style="height:10px;background:rgb(0,0,255)"></div><slot></slot><div style="height:10px;background:rgb(255,255,0)"></div></div></body></html>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let source = tree.get_element_by_id("source").unwrap();
        let root = tree
            .attach_shadow_root(host, ShadowRootMode::Open)
            .expect("attach native shadow root");
        for child in tree.children(source) {
            tree.append_child(root, child);
        }
        tree.remove(source);

        let pixmap = paint_dom(&tree, (30.0, 30.0), None).expect("shadow paint");
        let blue = pixmap.pixel(5, 5).unwrap();
        let red = pixmap.pixel(5, 15).unwrap();
        let yellow = pixmap.pixel(5, 25).unwrap();
        assert!(blue.blue() > 240 && blue.red() < 20 && blue.green() < 20);
        assert!(red.red() > 240 && red.green() < 20 && red.blue() < 20);
        assert!(yellow.red() > 240 && yellow.green() > 240 && yellow.blue() < 20);
    }

    #[test]
    fn outset_box_shadow_stays_outside_transparent_and_opaque_border_boxes() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0;background:white">
                <div style="position:absolute;left:20px;top:20px;width:40px;height:30px;
                            box-shadow:4px 4px 0 black"></div>
                <div style="position:absolute;left:100px;top:20px;width:40px;height:30px;
                            background:rgb(0,255,0);box-shadow:4px 4px 0 black"></div>
                <div style="position:absolute;left:20px;top:70px;width:40px;height:30px;
                            border-radius:12px;box-shadow:0 0 0 4px black"></div>
                <div style="position:absolute;left:100px;top:70px;width:40px;height:30px;
                            box-shadow:2px 2px 3px rgb(51,51,51)"></div>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (160.0, 120.0), None).expect("box shadow paint");

        let transparent_center = pixmap.pixel(35, 35).expect("transparent center");
        assert_eq!(
            (
                transparent_center.red(),
                transparent_center.green(),
                transparent_center.blue(),
            ),
            (255, 255, 255),
            "an outset shadow must not cover a transparent border box"
        );
        let offset_gap = pixmap.pixel(21, 35).expect("offset gap");
        assert_eq!(
            (offset_gap.red(), offset_gap.green(), offset_gap.blue()),
            (255, 255, 255),
            "the clip must not turn the shadow and border-box paths into a symmetric difference"
        );
        let shadow_edge = pixmap.pixel(62, 35).expect("shadow edge");
        assert!(
            shadow_edge.red() < 10 && shadow_edge.green() < 10 && shadow_edge.blue() < 10,
            "shadow ink must remain outside the border box: {shadow_edge:?}"
        );
        let opaque_center = pixmap.pixel(115, 35).expect("opaque center");
        assert!(
            opaque_center.green() > 245
                && opaque_center.red() < 10
                && opaque_center.blue() < 10,
            "opaque backgrounds must continue to cover the shadow: {opaque_center:?}"
        );
        let rounded_corner = pixmap.pixel(21, 71).expect("rounded corner shadow");
        assert!(
            rounded_corner.red() < 10
                && rounded_corner.green() < 10
                && rounded_corner.blue() < 10,
            "the hole must follow the rounded border box rather than its rectangular bounds: {rounded_corner:?}"
        );
        let blurred_center = pixmap.pixel(115, 85).expect("blurred center");
        assert_eq!(
            (
                blurred_center.red(),
                blurred_center.green(),
                blurred_center.blue(),
            ),
            (255, 255, 255),
            "blurred outset shadows must keep the border-box interior transparent"
        );
        let blurred_edge = pixmap.pixel(142, 85).expect("blurred edge");
        assert!(
            blurred_edge.red() < 240
                && blurred_edge.green() < 240
                && blurred_edge.blue() < 240,
            "the issue's 2px 2px 3px shadow must retain ink outside the box: {blurred_edge:?}"
        );
    }

    #[test]
    fn svg_image_metadata_keeps_view_box_as_ratio_only() {
        let ratio_only = svg_image_intrinsic_metadata(
            br#"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 576 576'/>"#,
        )
        .expect("ratio-only SVG metadata");
        assert_eq!(ratio_only.width, None);
        assert_eq!(ratio_only.height, None);
        assert_eq!(ratio_only.ratio, Some(1.0));
        assert_eq!(ratio_only.natural_size(), Some((150.0, 150.0)));

        let explicit = svg_image_intrinsic_metadata(
            br#"<svg xmlns='http://www.w3.org/2000/svg' width='120' height='80' viewBox='0 0 200 100'/>"#,
        )
        .expect("explicit SVG metadata");
        assert_eq!(explicit.width, Some(120.0));
        assert_eq!(explicit.height, Some(80.0));
        assert_eq!(explicit.ratio, Some(1.5));
        assert_eq!(explicit.natural_size(), Some((120.0, 80.0)));

        let commented = svg_image_intrinsic_metadata(
            br#"<!-- <svg width='999' height='999'/> --><svg xmlns='http://www.w3.org/2000/svg' width='12' height='8'/>"#,
        )
        .expect("the real root after a comment");
        assert_eq!(commented.natural_size(), Some((12.0, 8.0)));

        assert!(svg_image_intrinsic_metadata(
            br#"<svg xmlns='http://www.w3.org/2000/svg' width='10' height='20'><g>"#,
        )
        .is_none());
    }

    #[test]
    fn view_box_only_svg_transfers_definite_css_width_without_using_view_box_units() {
        let square = br#"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'/>"#;
        assert_eq!(
            image_metadata_from_bytes(square),
            Some((150.0, 150.0)),
            "natural dimensions contain the ratio in the 300x150 default object"
        );
        let tree = parse_html(
            r#"<html><head><style>
                html,body { margin:0 }
                #host { width:360px }
                img { display:block }
                #ratio { width:100%; height:auto }
            </style></head><body><div id="host">
                <img id="auto" src="data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20viewBox='0%200%20100%20100'/%3E">
                <img id="ratio" src="data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20viewBox='0%200%20200%20100'/%3E">
                <img id="explicit" src="data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='120'%20height='80'%20viewBox='0%200%20200%20100'/%3E">
            </div></body></html>"#,
        );
        let mut resources = RenderResourceCache::default();
        let prepared = prepare_dom(&tree, (500.0, 400.0), None, &mut resources).expect("layout");
        let rect = |selector| {
            let id = tree.query_selector(selector).unwrap().unwrap();
            prepared.layout.rects[&id]
        };
        let auto = rect("#auto");
        assert_eq!((auto.width, auto.height), (360.0, 360.0));
        let ratio = rect("#ratio");
        assert_eq!((ratio.width, ratio.height), (360.0, 180.0));
        let explicit = rect("#explicit");
        assert_eq!((explicit.width, explicit.height), (120.0, 80.0));
    }

    #[test]
    fn ratio_only_inline_images_stretch_fit_the_definite_line_width() {
        let source = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20viewBox='0%200%20576%20576'%3E%3Crect%20width='576'%20height='576'%20fill='%23ff80ab'/%3E%3C/svg%3E";
        let raster = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAADCAYAAAC56t6BAAAAFklEQVR4nGP8z8Dwn4GBgYGJAQrgDAAxOwIE7x6DkQAAAABJRU5ErkJggg==";
        let tree = parse_html(&format!(
            r#"<html><head><style>
                html,body {{ margin:0 }}
                .case {{ width:370px; line-height:0 }}
                #authored-inline {{ display:inline }}
                #authored-block {{ display:block }}
                #authored-inline-block {{ display:inline-block }}
                #positioned {{ position:absolute; left:0; top:1900px; width:340px;
                    min-width:360px; max-width:380px; box-sizing:border-box;
                    padding:10px; border:10px solid transparent }}
                #float-row {{ width:500px }}
                #float-column {{ float:left; width:40%; box-sizing:border-box;
                    padding-left:10px; padding-right:10px }}
            </style></head><body>
                <div class="case"><img id="direct" src="{source}"></div>
                <div class="case"><a><img id="anchored" src="{source}"></a></div>
                <div class="case"><img id="authored-inline" src="{source}"></div>
                <div class="case"><img id="authored-block" src="{source}"></div>
                <div class="case"><img id="authored-inline-block" src="{source}"></div>
                <div class="case"><img id="intrinsic-raster" src="{raster}"></div>
                <div id="positioned"><img id="positioned-image" src="{source}"></div>
                <div id="float-row"><div id="float-column"><img id="float-image" src="{source}"></div></div>
            </body></html>"#
        ));
        let mut resources = RenderResourceCache::default();
        let prepared =
            prepare_dom(&tree, (500.0, 2400.0), None, &mut resources).expect("image matrix");
        let node = |selector| tree.query_selector(selector).unwrap().unwrap();
        let rect = |selector| prepared.layout.rects[&node(selector)];

        for selector in [
            "#direct",
            "#anchored",
            "#authored-inline",
            "#authored-block",
            "#authored-inline-block",
        ] {
            let image = rect(selector);
            assert_eq!(
                (image.width, image.height),
                (370.0, 370.0),
                "ratio-only auto/auto sizing for {selector}: {image:?}"
            );
        }
        assert_eq!(
            (rect("#intrinsic-raster").width, rect("#intrinsic-raster").height),
            (2.0, 3.0),
            "a decoded image with real intrinsic axes must not stretch-fit"
        );
        assert_eq!(
            (rect("#positioned-image").width, rect("#positioned-image").height),
            (320.0, 320.0),
            "a definite positioned border-box honors edges and min/max widths"
        );
        assert_eq!(
            (rect("#float-image").width, rect("#float-image").height),
            (180.0, 180.0),
            "a floated percentage column resolves against its reliable containing width"
        );

        let display = |selector| {
            prepared
                .computed_style(node(selector))
                .unwrap()["display"]
                .clone()
        };
        assert_eq!(display("#direct"), "inline");
        assert_eq!(display("#anchored"), "inline");
        assert_eq!(display("#authored-inline"), "inline");
        assert_eq!(display("#authored-block"), "block");
        assert_eq!(display("#authored-inline-block"), "inline-block");
    }

    #[test]
    fn native_placeholders_honor_default_author_color_opacity_and_value_state() {
        let tree = parse_html(
            r#"<html><head><style>
                html,body { margin:0; background:white }
                input { display:block; box-sizing:border-box; width:180px; height:30px;
                        padding:0; border:0; font-size:20px; background:white }
                #colored::placeholder { color:rgb(255,0,0) }
                #hidden::placeholder { opacity:0 }
                #inherited { color:rgb(0,0,255) }
                #inherited::placeholder { color:inherit; opacity:.5 }
            </style></head><body>
                <input id="default" placeholder="default">
                <input id="colored" placeholder="colored">
                <input id="hidden" placeholder="hidden">
                <input id="filled" placeholder="must not paint" value="actual">
                <input id="inherited" placeholder="inherited">
            </body></html>"#,
        );
        let mut resources = RenderResourceCache::default();
        let mut prepared = prepare_dom(&tree, (200.0, 130.0), None, &mut resources)
            .expect("placeholder layout");
        let node = |selector| tree.query_selector(selector).unwrap().unwrap();

        assert!(
            prepared.layout().styles[&node("#default")]
                .placeholder_pseudo
                .is_none(),
            "the native default must not require an authored pseudo rule"
        );
        assert_eq!(
            prepared.layout().styles[&node("#colored")]
                .placeholder_pseudo
                .as_deref()
                .and_then(|style| style.color),
            Some([255, 0, 0, 255])
        );
        assert_eq!(
            prepared.layout().styles[&node("#hidden")]
                .placeholder_pseudo
                .as_deref()
                .and_then(|style| style.opacity),
            Some(0.0)
        );
        let inherited = prepared.layout().styles[&node("#inherited")]
            .placeholder_pseudo
            .as_deref()
            .expect("inherited placeholder style");
        assert_eq!(inherited.color, Some([0, 0, 255, 255]));
        assert_eq!(inherited.opacity, Some(0.5));

        let pixmap = paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 0.0))
            .expect("placeholder paint");
        let non_white = |top: u32| {
            (top..top + 30)
                .flat_map(|y| (0..180).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let pixel = pixmap.pixel(x, y).unwrap();
                    pixel.red() < 245 || pixel.green() < 245 || pixel.blue() < 245
                })
                .count()
        };
        assert!(non_white(0) > 10, "the native default placeholder must paint");
        assert!(
            (30..60).any(|y| (0..180).any(|x| {
                let pixel = pixmap.pixel(x, y).unwrap();
                pixel.red() > 200 && pixel.green() < 80 && pixel.blue() < 80
            })),
            "the authored placeholder color must reach glyph paint"
        );
        assert_eq!(non_white(60), 0, "opacity:0 must suppress placeholder glyphs");
        assert_eq!(
            non_white(90),
            0,
            "a non-empty control value must suppress placeholder glyphs"
        );
    }

    #[test]
    fn cache_only_mode_does_not_load_or_negative_cache_unknown_urls() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let mut cache = RenderResourceCache::with_loader(move |_url: &str| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(vec![1, 2, 3])
        });
        let url = "https://example.test/dynamic.png";

        let previous = cache.set_sync_loading_enabled(false);
        assert!(previous);
        assert!(cache.get_or_load(url).is_none());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(
            !cache.has_live_outcome(url),
            "a cache-only miss must remain eligible for later preparation"
        );

        cache.set_sync_loading_enabled(previous);
        cache.seed(url.to_string(), vec![9, 8, 7]);
        assert_eq!(cache.get_or_load(url).as_deref(), Some([9, 8, 7].as_slice()));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn html_image_profiles_keep_intrinsic_geometry_and_paint_separate() {
        let network_url = "https://assets.test/shared.svg";
        let url = "https://assets.test/shared.svg#icon";
        let tree = parse_html(&format!(
            r#"<html style="margin:0"><body style="margin:0">
                <img id="plain" src="{url}" style="display:block">
                <img id="anonymous" crossorigin="anonymous" src="{url}" style="display:block">
                <img id="credentialed" crossorigin="use-credentials" alt="" src="{url}"
                     style="display:block;width:20px;height:10px">
            </body></html>"#
        ));
        let mut resources = RenderResourceCache::with_loader(|_url: &str| {
            panic!("seeded profile resources must not reach the loader")
        });
        resources.seed_image(
            network_url.to_string(),
            ImageRequestProfile::NoCorsInclude,
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="#f00"/></svg>"##.to_vec(),
        );
        resources.seed_image(
            network_url.to_string(),
            ImageRequestProfile::CorsSameOrigin,
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30"><rect width="40" height="30" fill="#0f0"/></svg>"##.to_vec(),
        );
        resources.seed_image_missing(network_url.to_string(), ImageRequestProfile::CorsInclude);

        let mut prepared =
            prepare_dom(&tree, (80.0, 60.0), None, &mut resources).expect("profiled layout");
        let node = |selector| tree.query_selector(selector).unwrap().unwrap();
        let plain = prepared.layout().rects[&node("#plain")];
        let anonymous = prepared.layout().rects[&node("#anonymous")];
        assert_eq!(prepared.selected_image(node("#plain")).unwrap().resolved_url, url);
        assert_eq!((plain.width, plain.height), (20.0, 10.0));
        assert_eq!((anonymous.width, anonymous.height), (40.0, 30.0));

        let pixmap = paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 0.0))
            .expect("profiled paint");
        let red = pixmap.pixel(5, 5).unwrap();
        assert!(red.red() > 240 && red.green() < 20, "{red:?}");
        let green = pixmap.pixel(5, 20).unwrap();
        assert!(green.green() > 240 && green.red() < 20, "{green:?}");
        let failed = pixmap.pixel(5, 45).unwrap();
        assert_eq!(
            (failed.red(), failed.green(), failed.blue(), failed.alpha()),
            (255, 255, 255, 255),
            "failed credentialed image must not paint bytes from another profile"
        );
    }

    #[test]
    fn image_accept_advertises_exactly_decodable_mime_types() {
        assert!(!IMAGE_ACCEPT.contains('*'));
        assert!(!IMAGE_ACCEPT.to_ascii_lowercase().contains("avif"));
        for mime in IMAGE_ACCEPT.split(',') {
            assert!(
                crate::source_type_supported(mime),
                "advertised MIME type must be decodable: {mime}"
            );
        }
        for required in [
            "image/webp",
            "image/apng",
            "image/svg+xml",
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/bmp",
            "image/x-icon",
            "image/vnd.microsoft.icon",
        ] {
            assert!(
                IMAGE_ACCEPT.split(',').any(|mime| mime == required),
                "missing supported MIME type {required}"
            );
        }
    }

    #[test]
    fn picture_type_filter_skips_avif_and_accepts_parameterized_mime_essence() {
        let tree = parse_html(
            r#"<picture>
                 <source type="IMAGE/AVIF; codecs=av01" srcset="unsupported.avif">
                 <source type=" Image/WebP ; codecs=lossless " srcset="supported.webp">
                 <img id="hero" src="fallback.png">
               </picture>"#,
        );
        let hero = tree.get_element_by_id("hero").expect("hero");
        assert_eq!(
            picture_source_url(&tree, hero, (800.0, 600.0)),
            Some(("supported.webp".to_string(), 1.0))
        );
    }

    #[test]
    fn data_source_attributes_are_not_image_candidates_or_fetches() {
        let tree = parse_html(
            r#"<img id="lazy" data-src="real.png"
                     data-srcset="small.png 1x, large.png 2x"
                     data-lazy-src="other.png" data-original="original.png">"#,
        );
        let lazy = tree.get_element_by_id("lazy").expect("lazy image");
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_calls = calls.clone();
        let mut resources = RenderResourceCache::with_loader(move |_url: &str| {
            loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            None
        });

        assert_eq!(resolve_img_url(&tree, lazy, (800.0, 600.0)), None);
        assert_eq!(
            resources.image_element_metadata(
                &tree,
                lazy,
                (800.0, 600.0),
                Some("https://example.test/page"),
            ),
            None
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn data_uri_src_remains_selected_when_data_src_is_present() {
        const PLACEHOLDER: &str = "data:image/svg+xml,%3Csvg%20xmlns=%22http://www.w3.org/2000/svg%22%20width=%221%22%20height=%221%22/%3E";
        let tree = parse_html(&format!(
            r#"<img id="lazy" src="{PLACEHOLDER}" data-src="real.png">"#
        ));
        let lazy = tree.get_element_by_id("lazy").expect("lazy image");
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_calls = calls.clone();
        let mut resources = RenderResourceCache::with_loader(move |_url: &str| {
            loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            None
        });

        assert_eq!(
            resolve_img_url(&tree, lazy, (800.0, 600.0)),
            Some((PLACEHOLDER.to_string(), 1.0))
        );
        let metadata = resources
            .image_element_metadata(
                &tree,
                lazy,
                (800.0, 600.0),
                Some("https://example.test/page"),
            )
            .expect("selected placeholder");
        assert_eq!(metadata.0, PLACEHOLDER);
        assert_eq!(metadata.2, Some((1.0, 1.0)));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn enabled_bmp_and_ico_formats_have_metadata_and_full_raster_decode() {
        use std::io::Cursor;

        let source = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            3,
            image::Rgba([20, 40, 60, 255]),
        ));
        for format in [image::ImageFormat::Bmp, image::ImageFormat::Ico] {
            let mut encoded = Cursor::new(Vec::new());
            source
                .write_to(&mut encoded, format)
                .expect("encode fixture");
            let encoded = encoded.into_inner();
            assert_eq!(image_dimensions(&encoded), Some((2, 3)), "{format:?}");
            let raster = raster_to_pixmap(&encoded, 2, 3).expect("decode raster");
            assert_eq!((raster.width(), raster.height()), (2, 3));
        }
    }

    #[test]
    fn gif_placeholders_have_intrinsic_pixels_and_valid_gifs_decode() {
        // Apple's lazy-picture system (and many generic lazy loaders) uses a
        // transparent 1x1 GIF as the selected source until the real candidate
        // enters its preload range. It is still a successfully decoded image:
        // Chromium reports complete=true and naturalWidth=1.
        const TRANSPARENT_GIF: &[u8] = &[
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x70, 0x00, 0x00, 0x21,
            0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
        ];

        // Apple's deliberately palette-less placeholder is accepted by
        // browsers for source selection. Header metadata is sufficient for
        // complete/naturalWidth; visually it contributes no pixels.
        assert_eq!(image_dimensions(TRANSPARENT_GIF), Some((1, 1)));

        const VISIBLE_GIF: &[u8] = &[
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00,
            0x00, 0x00, 0xff, 0xff, 0xff, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00,
            0x00, 0x02, 0x01, 0x4c, 0x00, 0x3b,
        ];
        let decoded = raster_to_pixmap(VISIBLE_GIF, 1, 1).expect("decode GIF");
        assert_eq!(decoded.width(), 1);
        assert_eq!(decoded.height(), 1);
        assert_eq!(decoded.pixel(0, 0).expect("pixel").alpha(), 255);
    }

    #[test]
    fn scrolled_viewport_moves_document_content_but_not_fixed_subtrees() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:80px;background:#ff0000"></div>
                <div style="height:80px;background:#0000ff"></div>
                <div style="position:fixed;z-index:10;left:0;top:0;width:20px;height:20px;background:#00ff00">
                    <span style="color:#00ff00">x</span>
                </div>
            </body></html>"#,
        );
        let top = paint_dom_scrolled(&tree, (100.0, 80.0), None, (0.0, 0.0)).expect("top viewport");
        let scrolled =
            paint_dom_scrolled(&tree, (100.0, 80.0), None, (0.0, 80.0)).expect("scrolled viewport");

        let top_content = top.pixel(50, 10).expect("top content");
        assert!(
            top_content.red() > 240 && top_content.blue() < 15,
            "top viewport should show first red block: {top_content:?}"
        );
        let scrolled_content = scrolled.pixel(50, 10).expect("scrolled content");
        assert!(
            scrolled_content.blue() > 240 && scrolled_content.red() < 15,
            "scrolled viewport should show second blue block: {scrolled_content:?}"
        );
        for (name, pixmap) in [("top", &top), ("scrolled", &scrolled)] {
            let fixed = pixmap.pixel(5, 5).expect("fixed content");
            assert!(
                fixed.green() > 240 && fixed.red() < 15 && fixed.blue() < 15,
                "{name} viewport should keep fixed subtree at the viewport origin: {fixed:?}"
            );
        }
    }

    #[test]
    fn repeated_scroll_capture_moves_shaped_text_and_its_overflow_clip_together() {
        let tree = parse_html(
            r#"<html style="margin:0;overflow:auto"><body style="margin:0">
               <div style="height:1000px;background:#ff0000"></div>
               <section style="height:160px;overflow:hidden;background:#000000;color:#ffffff">
                 <h2 style="margin:0;font-size:32px;line-height:40px">VISIBLE SCROLLED TEXT</h2>
                 <p style="margin:0;font-size:20px;line-height:28px">SECOND SHAPED LINE</p>
                 <div style="position:absolute;left:260px;top:1100px;width:20px;height:20px;background:#00ff00"></div>
                 <svg style="position:absolute;left:260px;top:1040px;width:20px;height:20px"
                      viewBox="0 0 20 20"><rect width="20" height="20" fill="cyan"/></svg>
                 <div style="position:absolute;left:20px;top:1150px;width:20px;height:30px;background:#ff00ff"></div>
               </section>
               <div style="height:200px"></div>
               </body></html>"#,
        );
        let viewport = (300.0, 180.0);
        let mut resources = RenderResourceCache::default();
        let mut prepared =
            prepare_dom(&tree, viewport, None, &mut resources).expect("prepared render");
        let top =
            paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 0.0)).expect("top capture");
        let scrolled = paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 1000.0))
            .expect("scrolled capture");
        let top_repeat = paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 0.0))
            .expect("repeated top capture");
        let scrolled_repeat = paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 1000.0))
            .expect("repeated scrolled capture");

        assert_eq!(
            top, top_repeat,
            "returning to the top must not accumulate scroll"
        );
        assert_eq!(
            scrolled, scrolled_repeat,
            "repeated bottom paint must reuse immutable document geometry"
        );
        let white_ink = (0..90)
            .flat_map(|y| (0..250).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let pixel = scrolled.pixel(x, y).expect("text pixel");
                pixel.red() > 220 && pixel.green() > 220 && pixel.blue() > 220
            })
            .count();
        assert!(
            white_ink > 100,
            "visible shaped text must share the viewport-space overflow clip, found {white_ink} white pixels"
        );
        let marker = scrolled.pixel(270, 110).expect("marker pixel");
        assert!(
            marker.green() > 220 && marker.red() < 40 && marker.blue() < 40,
            "a non-text box in the same clipped section must remain visible: {marker:?}"
        );
        let svg_marker = scrolled.pixel(270, 50).expect("svg marker pixel");
        assert!(
            svg_marker.green() > 220 && svg_marker.blue() > 220 && svg_marker.red() < 40,
            "an inline svg in the same clipped section must remain visible: {svg_marker:?}"
        );
        let clipped_marker = scrolled.pixel(30, 165).expect("clipped marker pixel");
        assert!(
            clipped_marker.red() > 240
                && clipped_marker.green() > 240
                && clipped_marker.blue() > 240,
            "nested overflow must still clip content below its document-space padding box: {clipped_marker:?}"
        );
    }

    #[test]
    fn body_overflow_stays_a_content_clip_when_html_owns_root_overflow() {
        let tree = parse_html(
            r#"<html style="margin:0;overflow:auto">
               <body style="margin:0;width:100px;height:50px;overflow:hidden">
                 <div style="position:absolute;left:10px;top:60px;width:20px;height:20px;background:red"></div>
               </body>
               </html>"#,
        );
        let output = paint_dom(&tree, (100.0, 100.0), None).expect("paint");
        let below_body = output.pixel(15, 65).expect("pixel");
        assert!(
            below_body.red() > 240
                && below_body.green() > 240
                && below_body.blue() > 240,
            "body overflow must not be mistaken for a viewport clip when html already owns overflow: {below_body:?}"
        );
    }

    #[test]
    fn repeated_scroll_captures_reuse_immutable_layout_geometry() {
        let tree = parse_html(
            r#"<html style="margin:0"><head><style>
                @font-face { font-family: Fixture; src: url("https://assets.test/font.ttf"); }
                body { font-family: Fixture; }
            </style></head><body style="margin:0">
                <img id="hero" src="https://assets.test/fallback.svg"
                     srcset="https://assets.test/hero.svg 2x"
                     style="display:block;width:100px;height:auto">
                <div style="position:sticky;top:0;height:10px;background:#00ff00"></div>
                <div style="height:60px;background:#ff0000"></div>
                <div style="height:180px;overflow:hidden;background:#0000ff;
                            background-image:url('https://assets.test/background.svg')">
                    <div style="position:sticky;top:5px;height:20px;background:#00ff00"></div>
                    <div style="transform:translate(3px,4px);height:80px;color:#ffffff">stable text</div>
                </div>
                <div id="fixed" style="position:fixed;top:2px;left:2px;width:8px;height:8px"></div>
            </body></html>"#,
        );
        let viewport = (100.0, 80.0);
        let counts = Arc::new(std::sync::Mutex::new(HashMap::<String, usize>::new()));
        let loader_counts = Arc::clone(&counts);
        let mut resources = RenderResourceCache::with_loader(move |url: &str| {
            *loader_counts
                .lock()
                .expect("loader counts")
                .entry(url.to_string())
                .or_default() += 1;
            match url {
                "https://assets.test/font.ttf" => Some(FONT_BYTES.to_vec()),
                "https://assets.test/hero.svg" => Some(
                    br##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
                        <rect width="200" height="100" fill="#ffff00"/>
                    </svg>"##
                        .to_vec(),
                ),
                "https://assets.test/background.svg" => Some(
                    br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">
                        <rect width="20" height="20" fill="#0000ff"/>
                    </svg>"##
                        .to_vec(),
                ),
                _ => None,
            }
        });
        let mut prepared =
            prepare_dom(&tree, viewport, None, &mut resources).expect("prepared render");
        let hero = tree
            .query_selector("#hero")
            .expect("valid selector")
            .expect("hero");
        assert_eq!(
            prepared.selected_image(hero),
            Some(&SelectedImage {
                resolved_url: "https://assets.test/hero.svg".to_string(),
                density: 2.0,
                profile: ImageRequestProfile::NoCorsInclude,
            })
        );
        let hero_rect = prepared.layout().rects.get(&hero).expect("hero rect");
        assert!((hero_rect.width - 100.0).abs() < 0.1);
        assert!((hero_rect.height - 50.0).abs() < 0.1);
        assert!(prepared.content_size().1 > viewport.1);
        assert!(!prepared.sticky_layout().is_empty());
        assert_eq!(
            prepared.viewport_rect(hero, (0.0, 20.0)).unwrap().y,
            prepared.document_rect(hero).unwrap().y - 20.0
        );
        let fixed = tree
            .query_selector("#fixed")
            .expect("valid selector")
            .expect("fixed");
        assert!(prepared.viewport_fixed_nodes().contains(&fixed));
        assert_eq!(
            prepared.viewport_rect(fixed, (0.0, 100.0)),
            prepared.document_rect(fixed)
        );
        let base_rects = prepared.layout().rects.clone();
        let base_translates = prepared.layout().translates.clone();
        let base_clips = prepared.layout().clip_rects.clone();

        let near = screenshot_prepared(&tree, &mut prepared, &mut resources, (0.0, 20.0))
            .expect("near capture");
        let far = screenshot_prepared(&tree, &mut prepared, &mut resources, (0.0, 100.0))
            .expect("far capture");
        let far_repeat = screenshot_prepared(&tree, &mut prepared, &mut resources, (0.0, 100.0))
            .expect("repeated far capture");
        let near_after_far = screenshot_prepared(&tree, &mut prepared, &mut resources, (0.0, 20.0))
            .expect("repeated near capture");

        assert_ne!(
            near, far,
            "distinct scroll positions must paint distinct frames"
        );
        assert_eq!(far, far_repeat, "the same scroll position must be stable");
        assert_eq!(
            near, near_after_far,
            "an intervening capture must not accumulate scroll movement"
        );
        assert_eq!(prepared.layout().rects, base_rects);
        assert_eq!(prepared.layout().translates, base_translates);
        assert_eq!(prepared.layout().clip_rects, base_clips);
        let counts = counts.lock().expect("final loader counts");
        for url in [
            "https://assets.test/font.ttf",
            "https://assets.test/hero.svg",
            "https://assets.test/background.svg",
        ] {
            assert_eq!(counts.get(url), Some(&1), "{url} must load exactly once");
        }
        assert!(!counts.contains_key("https://assets.test/fallback.svg"));
        assert_eq!(resources.retained_entry_count(), 3);
        assert!(resources.retained_byte_len() > FONT_BYTES.len());
    }

    /// Portable pixel counterpart to the Chromium root-sticky geometry probe
    /// in obscura-js. It checks that sticky backgrounds and descendants move
    /// as one painted subtree, bottom sticking is visible, fixed remains
    /// viewport-anchored, and the top sticky eventually exits at its
    /// containing-block boundary.
    #[test]
    fn root_scroll_sticky_paints_subtrees_and_respects_bottom_boundary() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0;background:#220022">
                <div style="height:40px;background:#ffffff"></div>
                <div style="box-sizing:border-box;height:900px;padding:10px 12px;border:4px solid #333;background:#dddddd">
                    <div style="box-sizing:border-box;position:sticky;top:20px;height:60px;margin:6px;background:#ff0000">
                        <div style="height:12px;background:#0000ff"></div>
                    </div>
                    <div style="height:500px"></div>
                    <div style="box-sizing:border-box;position:sticky;bottom:15px;height:50px;margin:5px;background:#ff8800"></div>
                </div>
                <div style="height:700px;background:#220022"></div>
                <div style="position:fixed;z-index:10;left:600px;top:20px;width:60px;height:60px;background:#00ff00"></div>
            </body></html>"#,
        );
        let viewport = (800.0, 513.0);
        let top = paint_dom_scrolled(&tree, viewport, None, (0.0, 0.0)).unwrap();
        let stuck = paint_dom_scrolled(&tree, viewport, None, (0.0, 100.0)).unwrap();
        let bottom_normal = paint_dom_scrolled(&tree, viewport, None, (0.0, 400.0)).unwrap();
        let boundary = paint_dom_scrolled(&tree, viewport, None, (0.0, 9999.0)).unwrap();

        let is_color = |pixel: tiny_skia::PremultipliedColorU8, rgb: [u8; 3]| {
            (pixel.red() as i16 - rgb[0] as i16).abs() < 8
                && (pixel.green() as i16 - rgb[1] as i16).abs() < 8
                && (pixel.blue() as i16 - rgb[2] as i16).abs() < 8
        };
        assert!(is_color(top.pixel(100, 80).unwrap(), [255, 0, 0]));
        assert!(is_color(top.pixel(100, 460).unwrap(), [255, 136, 0]));
        assert!(is_color(stuck.pixel(100, 25).unwrap(), [0, 0, 255]));
        assert!(is_color(stuck.pixel(100, 50).unwrap(), [255, 0, 0]));
        assert!(is_color(stuck.pixel(100, 460).unwrap(), [255, 136, 0]));
        assert!(is_color(
            bottom_normal.pixel(100, 240).unwrap(),
            [255, 136, 0]
        ));
        assert!(is_color(boundary.pixel(100, 20).unwrap(), [34, 0, 34]));
        for pixmap in [&top, &stuck, &bottom_normal, &boundary] {
            assert!(is_color(pixmap.pixel(620, 30).unwrap(), [0, 255, 0]));
        }
    }

    #[test]
    fn object_fit_contain_and_cover_center_and_preserve_aspect() {
        // A 200x100 box (2:1) with a square 100x100 image, offset so centering
        // is checked against the box origin, not (0,0).
        let box_rect = crate::Rect {
            x: 10.0,
            y: 20.0,
            width: 200.0,
            height: 100.0,
        };
        let (iw, ih) = (100.0f32, 100.0f32);

        // Contain: the largest square fitting inside 200x100 is 100x100,
        // letterboxed horizontally and centered.
        let c = object_fit_dest(&box_rect, iw, ih, crate::ObjectFit::Contain);
        assert!(
            (c.width - 100.0).abs() < 0.01 && (c.height - 100.0).abs() < 0.01,
            "contain size {:?}",
            c
        );
        assert!(
            (c.width / c.height - iw / ih).abs() < 1e-3,
            "contain preserves aspect: {:?}",
            c
        );
        assert!(
            (c.x - 60.0).abs() < 0.01,
            "contain centered x (10 + (200-100)/2): {}",
            c.x
        );
        assert!(
            (c.y - 20.0).abs() < 0.01,
            "contain centered y (20 + (100-100)/2): {}",
            c.y
        );
        // Contain always fits inside the box.
        assert!(c.x >= box_rect.x - 0.01 && c.x + c.width <= box_rect.x + box_rect.width + 0.01);
        assert!(c.y >= box_rect.y - 0.01 && c.y + c.height <= box_rect.y + box_rect.height + 0.01);

        // Cover: the smallest square covering 200x100 is 200x200, centered so
        // it overflows the box vertically (the paint path clips it).
        let v = object_fit_dest(&box_rect, iw, ih, crate::ObjectFit::Cover);
        assert!(
            (v.width - 200.0).abs() < 0.01 && (v.height - 200.0).abs() < 0.01,
            "cover size {:?}",
            v
        );
        assert!(
            (v.width / v.height - iw / ih).abs() < 1e-3,
            "cover preserves aspect: {:?}",
            v
        );
        assert!(
            (v.x - 10.0).abs() < 0.01,
            "cover centered x (10 + (200-200)/2): {}",
            v.x
        );
        assert!(
            (v.y + 30.0).abs() < 0.01,
            "cover centered y (20 + (100-200)/2 = -30): {}",
            v.y
        );
        // Cover fully covers the box on both axes.
        assert!(v.x <= box_rect.x + 0.01 && v.x + v.width >= box_rect.x + box_rect.width - 0.01);
        assert!(v.y <= box_rect.y + 0.01 && v.y + v.height >= box_rect.y + box_rect.height - 0.01);

        // scale-down never upscales: a 100x100 image in a 200x200 box stays
        // 100x100 (Contain would grow it to 200x200), centered.
        let box2 = crate::Rect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 200.0,
        };
        let sd = object_fit_dest(&box2, iw, ih, crate::ObjectFit::ScaleDown);
        assert!(
            (sd.width - 100.0).abs() < 0.01 && (sd.height - 100.0).abs() < 0.01,
            "scale-down no upscale: {:?}",
            sd
        );
        assert!(
            (sd.x - 50.0).abs() < 0.01 && (sd.y - 50.0).abs() < 0.01,
            "scale-down centered: {:?}",
            sd
        );
        let cn = object_fit_dest(&box2, iw, ih, crate::ObjectFit::Contain);
        assert!(
            (cn.width - 200.0).abs() < 0.01,
            "contain upscales into the box: {:?}",
            cn
        );

        // None uses the intrinsic size regardless of box, centered.
        let n = object_fit_dest(&box2, iw, ih, crate::ObjectFit::None);
        assert!(
            (n.width - 100.0).abs() < 0.01 && (n.height - 100.0).abs() < 0.01,
            "none intrinsic size: {:?}",
            n
        );
        assert!(
            (n.x - 50.0).abs() < 0.01 && (n.y - 50.0).abs() < 0.01,
            "none centered: {:?}",
            n
        );

        // Fill stretches to exactly the box.
        let f = object_fit_dest(&box_rect, iw, ih, crate::ObjectFit::Fill);
        assert!(
            (f.width - box_rect.width).abs() < 0.01 && (f.height - box_rect.height).abs() < 0.01,
            "fill: {:?}",
            f
        );
        assert!(
            (f.x - box_rect.x).abs() < 0.01 && (f.y - box_rect.y).abs() < 0.01,
            "fill origin: {:?}",
            f
        );
    }

    #[test]
    fn paints_background_color() {
        let tree = parse_html(
            "<html><body><div style=\"background-color: #ff0000; width: 100px; height: 80px\"></div></body></html>",
        );
        let pixmap = paint_dom(&tree, (200.0, 200.0), None).expect("pixmap");
        assert_eq!(pixmap.width(), 200);
        // The red div is laid out at the origin; sample inside it.
        let inside = pixmap.pixel(10, 10).expect("pixel");
        assert!(inside.red() > 200, "expected red bg, got {:?}", inside);
        assert!(inside.green() < 60);
        assert!(inside.blue() < 60);
        // Outside the 100x80 div the page background is white.
        let outside = pixmap.pixel(150, 150).expect("pixel");
        assert_eq!(outside.red(), 255);
        assert_eq!(outside.green(), 255);
        assert_eq!(outside.blue(), 255);
    }

    #[test]
    fn body_background_transfers_to_canvas_once_over_base_surface() {
        let tree = parse_html(
            r#"<html style="margin:0;background:transparent">
               <body style="margin:0;width:20px;height:20px;background:rgba(255,0,0,.5)"></body>
               </html>"#,
        );
        let pixmap = paint_dom_scrolled_at_animation_time_with_surface_color(
            &tree,
            (100.0, 80.0),
            None,
            (0.0, 0.0),
            crate::AnimationSampleTime::default(),
            [0, 0, 255, 255],
        )
        .expect("canvas paint");
        let inside = pixmap.pixel(10, 10).expect("inside body box");
        let outside = pixmap.pixel(90, 70).expect("outside body box");
        assert_eq!(inside, outside, "transferred body background must not paint twice");
        assert!((127..=128).contains(&inside.red()), "red blend: {inside:?}");
        assert_eq!(inside.green(), 0);
        assert!((127..=128).contains(&inside.blue()), "blue blend: {inside:?}");
        assert_eq!(inside.alpha(), 255);
    }

    #[test]
    fn authored_html_background_owns_canvas_and_body_keeps_its_box_background() {
        let tree = parse_html(
            r#"<html style="margin:0;background:rgb(10,20,30)">
               <body style="margin:0;width:20px;height:20px;background:rgb(200,100,50)"></body>
               </html>"#,
        );
        let pixmap = paint_dom(&tree, (100.0, 80.0), None).expect("canvas paint");
        let inside = pixmap.pixel(10, 10).expect("body pixel");
        let outside = pixmap.pixel(90, 70).expect("canvas pixel");
        assert_eq!(
            (inside.red(), inside.green(), inside.blue()),
            (200, 100, 50)
        );
        assert_eq!(
            (outside.red(), outside.green(), outside.blue()),
            (10, 20, 30)
        );
    }

    #[test]
    fn transparent_html_and_body_leave_base_surface_unchanged() {
        let tree = parse_html(
            r#"<html style="margin:0;background:transparent">
               <body style="margin:0;background:transparent"></body></html>"#,
        );
        let pixmap = paint_dom_scrolled_at_animation_time_with_surface_color(
            &tree,
            (40.0, 30.0),
            None,
            (0.0, 0.0),
            crate::AnimationSampleTime::default(),
            [4, 8, 12, 255],
        )
        .expect("transparent canvas paint");
        for pixel in pixmap.pixels() {
            assert_eq!(
                (pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()),
                (4, 8, 12, 255)
            );
        }
    }

    #[test]
    fn transferred_canvas_background_paints_below_negative_z_content() {
        let tree = parse_html(
            r#"<html style="margin:0;background:transparent">
               <body style="margin:0;background:red">
                 <div style="position:absolute;z-index:-1;left:0;top:0;width:20px;height:20px;background:blue"></div>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (60.0, 40.0), None).expect("canvas paint");
        let negative = pixmap.pixel(10, 10).expect("negative layer pixel");
        let canvas = pixmap.pixel(40, 30).expect("canvas pixel");
        assert_eq!(
            (negative.red(), negative.green(), negative.blue()),
            (0, 0, 255)
        );
        assert_eq!((canvas.red(), canvas.green(), canvas.blue()), (255, 0, 0));
    }

    #[test]
    fn float_paints_above_normal_block_backgrounds_and_below_later_flow() {
        // Chromium 145: ordinary block border boxes remain full-width beneath
        // the float, but their backgrounds are in the lower block-background
        // paint band. The right float therefore stays visible through its
        // 80px height; the following block becomes visible across the full
        // width once it starts below the float.
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #bfc{display:flow-root;width:200px}
                #float{float:right;width:50px;height:80px;background:#1971c2}
                #lead{height:50px;background:#087f5b}
                #heading{height:10px;background:#e8590c}
                #beside{height:20px;background:#7048e8}
                #after{height:20px;background:#a61e4d}
            </style>
            <main id="bfc">
              <aside id="float"></aside>
              <div id="lead"></div>
              <div id="heading"></div>
              <div id="beside"></div>
              <div id="after"></div>
            </main>"#,
        );
        let pixmap = paint_dom(&tree, (200.0, 120.0), None).expect("float paint");
        let rgb = |x, y| {
            let pixel = pixmap.pixel(x, y).expect("sample pixel");
            (pixel.red(), pixel.green(), pixel.blue())
        };

        assert_eq!(rgb(25, 25), (8, 127, 91), "left side keeps lead background");
        assert_eq!(rgb(25, 55), (232, 89, 12), "left side keeps heading background");
        assert_eq!(rgb(25, 70), (112, 72, 232), "left side keeps beside background");
        for y in [25, 55, 70] {
            assert_eq!(
                rgb(175, y),
                (25, 113, 194),
                "float must overlay the full-width block background at y={y}"
            );
        }
        assert_eq!(
            rgb(175, 90),
            (166, 30, 77),
            "normal flow below the float paints across the full width"
        );
    }

    /// Chromium 150 oracle for CSS Backgrounds box geometry. Transparent
    /// borders make each clip edge directly observable, asymmetric insets
    /// distinguish authored gradient coordinates from the visible clip, and
    /// the rounded content box verifies that inner radii shrink per side.
    #[test]
    fn background_origin_clip_boxes_radii_and_sampling_match_chromium() {
        let tree = parse_html(
            r#"<html style="margin:0;background:white"><body style="margin:0;background:white">
              <style>
                .box { position:absolute;top:0;width:60px;height:40px;padding:10px;
                       border:10px solid transparent;background:#00aa00 }
                #border { left:0;background-clip:border-box }
                #padding { left:110px;background-clip:padding-box }
                #content { left:220px;background-clip:content-box }
                #radius { left:330px;border-radius:40px;background-clip:content-box }
                #gradient { position:absolute;left:0;top:100px;width:100px;height:40px;
                            border-left:20px solid transparent;padding-left:20px;
                            background-image:linear-gradient(90deg,red 0 50%,blue 50%);
                            background-origin:border-box;background-clip:content-box;
                            background-repeat:no-repeat }
                #wrapper { position:absolute;left:170px;top:100px;width:60px;height:40px;
                           overflow:hidden }
                #wide { width:140px;height:40px;
                        background:linear-gradient(90deg,red 0 50%,blue 50%) }
                #origin { position:absolute;left:280px;top:100px;width:100px;height:40px;
                          border-left:20px solid transparent;padding-left:20px;
                          background-image:linear-gradient(90deg,red,blue);
                          background-size:20px 20px;background-repeat:no-repeat;
                          background-origin:content-box;background-clip:border-box }
              </style>
              <div id="border" class="box"></div>
              <div id="padding" class="box"></div>
              <div id="content" class="box"></div>
              <div id="radius" class="box"></div>
              <div id="gradient"></div>
              <div id="wrapper"><div id="wide"></div></div>
              <div id="origin"></div>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (500.0, 220.0), None).expect("background geometry");
        let is_white = |x, y| {
            let pixel = pixmap.pixel(x, y).expect("pixel");
            pixel.red() > 240 && pixel.green() > 240 && pixel.blue() > 240
        };
        let is_green = |x, y| {
            let pixel = pixmap.pixel(x, y).expect("pixel");
            pixel.green() > 140 && pixel.red() < 30 && pixel.blue() < 30
        };
        let is_red = |x, y| {
            let pixel = pixmap.pixel(x, y).expect("pixel");
            pixel.red() > 220 && pixel.blue() < 40
        };
        let is_blue = |x, y| {
            let pixel = pixmap.pixel(x, y).expect("pixel");
            pixel.blue() > 220 && pixel.red() < 40
        };

        assert!(
            is_green(5, 30),
            "border-box clip paints beneath transparent border"
        );
        assert!(
            is_white(115, 30) && is_green(125, 30),
            "padding-box excludes border"
        );
        assert!(is_white(225, 30) && is_white(235, 30) && is_green(245, 30));
        assert!(
            is_white(351, 21) && is_green(370, 40),
            "content radius must inset to 20px"
        );
        assert!(
            is_red(60, 120) && is_blue(80, 120),
            "clip must not rebase gradient line"
        );
        assert!(
            is_red(220, 120),
            "ancestor clipping must not resize gradient coordinates"
        );
        assert!(
            is_white(300, 120) && !is_white(325, 110),
            "content origin anchors no-repeat tile"
        );
    }

    #[test]
    fn rounded_overflow_clips_descendants_at_the_padding_edge() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0;background:white">
                <div id="outer" style="position:absolute;left:10px;top:10px;box-sizing:border-box;
                     width:100px;height:100px;border:10px solid black;border-radius:30px;
                     overflow:hidden">
                  <div id="child" style="position:absolute;left:0;top:0;width:100px;height:100px;
                       background:red;transform:translate(-10px,-10px)"></div>
                </div>
            </body></html>"#,
        );
        let child = tree.get_element_by_id("child").expect("child");
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared = prepare_dom(&tree, (130.0, 130.0), None, &mut resources)
            .expect("prepared rounded overflow");
        let child_clip = prepared
            .layout
            .clip_rects
            .get(&child)
            .and_then(Option::as_ref)
            .expect("child clip");
        let rounded = child_clip
            .rounded_chain()
            .expect("child must retain the owner's rounded clip node");
        assert_eq!(
            rounded.clip.rect,
            crate::Rect {
                x: 20.0,
                y: 20.0,
                width: 80.0,
                height: 80.0
            }
        );
        assert_eq!(rounded.clip.radii.top_left, (20.0, 20.0));
        let direct_mask =
            overflow_clip_mask(130, 130, child_clip, (130.0, 130.0)).expect("rounded mask");
        assert_eq!(direct_mask.data()[(23 * 130 + 23) as usize], 0);
        let pixmap = paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 0.0))
            .expect("rounded overflow paint");
        let rgb = |x, y| {
            let pixel = pixmap.pixel(x, y).expect("pixel");
            (pixel.red(), pixel.green(), pixel.blue())
        };

        assert_eq!(rgb(23, 23), (0, 0, 0));
        assert_eq!(rgb(40, 25), (255, 0, 0));
        assert_eq!(rgb(50, 50), (255, 0, 0));
        assert_eq!(rgb(15, 50), (0, 0, 0));
    }

    #[test]
    fn nested_rounded_overflow_keeps_every_clip_chain_node() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0;background:white">
                <div id="outer" style="position:absolute;left:10px;top:10px;width:100px;height:100px;
                     border-radius:40px;overflow:hidden;background:blue">
                  <div style="position:absolute;left:30px;top:0;width:80px;height:80px;
                       border-radius:20px;overflow:hidden">
                    <div id="child" style="width:80px;height:80px;background:lime"></div>
                  </div>
                </div>
            </body></html>"#,
        );
        let child = tree.get_element_by_id("child").expect("child");
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared = prepare_dom(&tree, (140.0, 130.0), None, &mut resources)
            .expect("prepared nested clips");
        let mut chain = prepared
            .layout
            .clip_rects
            .get(&child)
            .and_then(Option::as_ref)
            .and_then(crate::dom::OverflowClip::rounded_chain)
            .map(AsRef::as_ref);
        let mut chain_len = 0;
        while let Some(node) = chain {
            chain_len += 1;
            chain = node.parent.as_deref();
        }
        assert_eq!(chain_len, 2, "both rounded owners must survive the chain");
        let pixmap = paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 0.0))
            .expect("nested clip paint");
        let rgb = |x, y| {
            let pixel = pixmap.pixel(x, y).expect("pixel");
            (pixel.red(), pixel.green(), pixel.blue())
        };

        assert_eq!(rgb(70, 30), (0, 255, 0), "inside both rounded clips");
        assert_eq!(
            rgb(43, 13),
            (0, 0, 255),
            "inner rounded corner must reveal the outer background"
        );
        assert_eq!(
            rgb(105, 15),
            (255, 255, 255),
            "outer rounded corner must still constrain an inner-visible point"
        );
    }

    #[test]
    fn paints_border_when_var_is_adjacent_to_border_style_token() {
        let tree = parse_html(
            r##"<html><head><style>
                body { margin:0 }
                #target {
                    --stroke:2px;
                    --ink:#e11d48;
                    width:30px;
                    height:30px;
                    border:var(--stroke)solid var(--ink);
                    background:#fff;
                }
            </style></head><body><div id="target"></div></body></html>"##,
        );
        let pixmap = paint_dom(&tree, (60.0, 60.0), None).expect("pixmap");
        let border = pixmap.pixel(1, 1).expect("border pixel");
        assert!(
            border.red() > 200 && border.green() < 70 && border.blue() < 100,
            "adjacent var() substitution must retain a painted red border: {border:?}"
        );
        let interior = pixmap.pixel(5, 5).expect("interior pixel");
        assert!(
            interior.red() > 245 && interior.green() > 245 && interior.blue() > 245,
            "the border must not consume the content box: {interior:?}"
        );
    }

    #[test]
    fn border_outline_computed_geometry_and_pixels_share_one_used_model() {
        let tree = parse_html(
            r##"<html><body style="margin:0;background:white">
              <div id="none" style="width:100px;height:50px;border-width:10px;border-style:none"></div>
              <div id="decorated" style="position:absolute;left:30px;top:90px;width:100px;height:50px;
                   border-width:8px;border-style:solid;border-color:red green blue purple;
                   border-radius:30px 20px 14px 8px/20px 12px 10px 6px;
                   outline:4px dashed black;outline-offset:3px;background:#ffff00"></div>
            </body></html>"##,
        );
        let none = tree.get_element_by_id("none").expect("none");
        let decorated = tree.get_element_by_id("decorated").expect("decorated");
        let mut resources = RenderResourceCache::default();
        let mut prepared =
            prepare_dom(&tree, (180.0, 180.0), None, &mut resources).expect("prepare");

        let none_rect = prepared.document_rect(none).expect("none rect");
        assert_eq!((none_rect.width, none_rect.height), (100.0, 50.0));
        let none_style = prepared.computed_style(none).expect("none style");
        assert_eq!(none_style["border-top-width"], "0px");
        assert_eq!(none_style["border-top-style"], "none");

        let rect = prepared.document_rect(decorated).expect("decorated rect");
        assert_eq!((rect.width, rect.height), (116.0, 66.0));
        let computed = prepared.computed_style(decorated).expect("computed style");
        assert_eq!(computed["border-top-color"], "rgb(255, 0, 0)");
        assert_eq!(computed["border-right-color"], "rgb(0, 128, 0)");
        assert_eq!(computed["border-bottom-color"], "rgb(0, 0, 255)");
        assert_eq!(computed["border-left-color"], "rgb(128, 0, 128)");
        assert_eq!(computed["border-top-left-radius"], "30px 20px");
        assert_eq!(computed["border-bottom-left-radius"], "8px 6px");
        assert_eq!(computed["outline-width"], "4px");
        assert_eq!(computed["outline-style"], "dashed");
        assert_eq!(computed["outline-offset"], "3px");

        let pixmap =
            paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 0.0)).expect("paint");
        let sample = |x: u32, y: u32| pixmap.pixel(x, y).expect("pixel");
        let top = sample((rect.x + rect.width / 2.0) as u32, (rect.y + 2.0) as u32);
        let right = sample(
            (rect.x + rect.width - 2.0) as u32,
            (rect.y + rect.height / 2.0) as u32,
        );
        let bottom = sample(
            (rect.x + rect.width / 2.0) as u32,
            (rect.y + rect.height - 2.0) as u32,
        );
        let left = sample((rect.x + 2.0) as u32, (rect.y + rect.height / 2.0) as u32);
        assert!(
            top.red() > 220 && top.green() < 40 && top.blue() < 40,
            "{top:?}"
        );
        assert!(
            right.green() > 90 && right.red() < 40 && right.blue() < 40,
            "{right:?}"
        );
        assert!(
            bottom.blue() > 220 && bottom.red() < 40 && bottom.green() < 40,
            "{bottom:?}"
        );
        assert!(
            left.red() > 90 && left.blue() > 90 && left.green() < 40,
            "{left:?}"
        );

        let rounded_corner = sample((rect.x + 1.0) as u32, (rect.y + 1.0) as u32);
        assert!(
            !(rounded_corner.red() > 220
                && rounded_corner.green() > 220
                && rounded_corner.blue() < 40),
            "asymmetric elliptical corner must stay outside the yellow background: {rounded_corner:?}"
        );
        let mut outline_pixels = 0;
        let outer_x = (rect.x - 8.0).max(0.0) as u32;
        let outer_y = (rect.y - 8.0).max(0.0) as u32;
        let outer_right = (rect.x + rect.width + 8.0) as u32;
        let outer_bottom = (rect.y + rect.height + 8.0) as u32;
        for y in outer_y..outer_bottom.min(pixmap.height()) {
            for x in outer_x..outer_right.min(pixmap.width()) {
                let outside = (x as f32) < rect.x
                    || (x as f32) >= rect.x + rect.width
                    || (y as f32) < rect.y
                    || (y as f32) >= rect.y + rect.height;
                let pixel = sample(x, y);
                if outside && pixel.red() < 30 && pixel.green() < 30 && pixel.blue() < 30 {
                    outline_pixels += 1;
                }
            }
        }
        assert!(
            outline_pixels > 40,
            "dashed outline should paint outside layout: {outline_pixels}"
        );
    }

    #[test]
    fn outline_none_retains_width_but_does_not_change_geometry_or_computed_used_width() {
        let tree = parse_html(
            r#"<html><body style="margin:0"><div id="box" style="width:40px;height:20px;
                outline-width:9px;outline-style:none"></div></body></html>"#,
        );
        let id = tree.get_element_by_id("box").unwrap();
        let mut resources = RenderResourceCache::default();
        let prepared = prepare_dom(&tree, (80.0, 50.0), None, &mut resources).unwrap();
        let rect = prepared.document_rect(id).unwrap();
        assert_eq!((rect.width, rect.height), (40.0, 20.0));
        assert_eq!(prepared.layout.styles[&id].outline.specified_width, 9.0);
        assert_eq!(prepared.computed_style(id).unwrap()["outline-width"], "0px");
    }

    #[test]
    fn direct_pure_text_webkit_clamp_matches_line_geometry_and_computed_values() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
              <div id="clamped" style="width:120px;font:16px/20px Arial;
                display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:2;
                overflow:hidden">one<br>two<br>three<br>four<br>five</div>
              <div id="exact" style="width:120px;font:16px/20px Arial;
                display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:2;
                overflow:hidden">one<br>two</div>
              <div id="inactive" style="width:120px;font:16px/20px Arial;
                -webkit-box-orient:vertical;-webkit-line-clamp:2;overflow:hidden">
                one<br>two<br>three<br>four<br>five</div>
              <div id="nested" style="width:120px;font:16px/20px Arial;
                display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:2;
                overflow:hidden"><div>one</div><div>two</div><div>three</div></div>
            </body></html>"#,
        );
        let clamped = tree.get_element_by_id("clamped").unwrap();
        let exact = tree.get_element_by_id("exact").unwrap();
        let inactive = tree.get_element_by_id("inactive").unwrap();
        let nested = tree.get_element_by_id("nested").unwrap();
        let mut resources = RenderResourceCache::default();
        let prepared = prepare_dom(&tree, (300.0, 300.0), None, &mut resources).unwrap();

        assert_eq!(prepared.document_rect(clamped).unwrap().height, 40.0);
        assert_eq!(prepared.document_rect(exact).unwrap().height, 40.0);
        assert_eq!(prepared.document_rect(inactive).unwrap().height, 100.0);
        assert_eq!(
            prepared.document_rect(nested).unwrap().height,
            60.0,
            "this slice must not fake descendant block-line counting by clipping children"
        );
        let computed = prepared.computed_style(clamped).unwrap();
        assert_eq!(computed["display"], "flow-root");
        assert_eq!(computed["-webkit-line-clamp"], "2");
        assert_eq!(computed["-webkit-box-orient"], "vertical");
        assert_eq!(computed["text-overflow"], "clip");
    }

    #[test]
    fn truncation_markers_are_shaped_and_painted_without_changing_dom_text() {
        let ellipsis_tree = parse_html(
            r#"<html><body style="margin:0"><div id="nowrap" style="position:absolute;top:0;width:120px;font:16px/20px Arial;
              white-space:nowrap;overflow:hidden;text-overflow:ellipsis">ABCDEFGHIJKLMNO</div>
              <div id="clamp" style="position:absolute;top:20px;width:120px;font:16px/20px Arial;display:-webkit-box;
              -webkit-box-orient:vertical;-webkit-line-clamp:2;overflow:hidden">one<br>two<br>three</div>
              <div id="exact" style="position:absolute;top:80px;width:120px;font:16px/20px Arial;display:-webkit-box;
              -webkit-box-orient:vertical;-webkit-line-clamp:2;overflow:hidden">one<br>two</div>
              <div id="visible" style="position:absolute;top:120px;width:120px;font:16px/20px Arial;
              white-space:nowrap;overflow:visible;text-overflow:ellipsis">ABCDEFGHIJKLMNO</div>
              </body></html>"#,
        );
        let clip_tree = parse_html(
            r#"<html><body style="margin:0"><div id="nowrap" style="position:absolute;top:0;width:120px;font:16px/20px Arial;
              white-space:nowrap;overflow:hidden;text-overflow:clip">ABCDEFGHIJKLMNO</div>
              <div id="clamp" style="position:absolute;top:20px;width:120px;font:16px/20px Arial;overflow:hidden">one<br>two<br>three</div>
              <div id="exact" style="position:absolute;top:80px;width:120px;font:16px/20px Arial;overflow:hidden">one<br>two</div>
              <div id="visible" style="position:absolute;top:120px;width:120px;font:16px/20px Arial;
              white-space:nowrap;overflow:visible;text-overflow:clip">ABCDEFGHIJKLMNO</div>
              </body></html>"#,
        );
        let ellipsis = paint_dom(&ellipsis_tree, (180.0, 160.0), None).unwrap();
        let clip = paint_dom(&clip_tree, (180.0, 160.0), None).unwrap();

        let nowrap_edge_difference = (85..120)
            .flat_map(|x| (0..20).map(move |y| (x, y)))
            .filter(|&(x, y)| ellipsis.pixel(x, y) != clip.pixel(x, y))
            .count();
        assert!(
            nowrap_edge_difference > 8,
            "ellipsis must replace end glyph pixels: {nowrap_edge_difference}"
        );
        let clamp_marker_difference = (20..55)
            .flat_map(|x| (40..60).map(move |y| (x, y)))
            .filter(|&(x, y)| ellipsis.pixel(x, y) != clip.pixel(x, y))
            .count();
        assert!(
            clamp_marker_difference > 4,
            "line clamp must paint a separately shaped marker: {clamp_marker_difference}"
        );
        let exact_line_difference = (0..140)
            .flat_map(|x| (80..120).map(move |y| (x, y)))
            .filter(|&(x, y)| ellipsis.pixel(x, y) != clip.pixel(x, y))
            .count();
        assert_eq!(
            exact_line_difference, 0,
            "an exactly-full clamp must not synthesize an overflow marker"
        );
        let visible_overflow_difference = (0..180)
            .flat_map(|x| (120..140).map(move |y| (x, y)))
            .filter(|&(x, y)| ellipsis.pixel(x, y) != clip.pixel(x, y))
            .count();
        assert_eq!(
            visible_overflow_difference, 0,
            "text-overflow must not synthesize a marker when overflow is visible"
        );
        for id in ["nowrap", "clamp", "exact", "visible"] {
            assert_eq!(
                ellipsis_tree.text_content(ellipsis_tree.get_element_by_id(id).unwrap()),
                clip_tree.text_content(clip_tree.get_element_by_id(id).unwrap()),
            );
        }
    }

    #[test]
    fn percentage_border_radius_paints_circles_ellipses_and_replaced_clips() {
        let tree = parse_html(
            r##"<html><body style="margin:0">
               <div id="circle" style="position:absolute;left:0;top:0;width:40px;height:40px;
                    border-radius:50%;background:#ff0000"></div>
               <div id="ellipse" style="position:absolute;left:50px;top:0;width:80px;height:40px;
                    border-radius:50%;background:#0000ff"></div>
               <div id="pill" style="position:absolute;left:140px;top:0;width:80px;height:40px;
                    border-radius:20px;background:#00aa00"></div>
               <img alt="" src="data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='40'%20height='40'%3E%3Crect%20width='40'%20height='40'%20fill='%23800080'/%3E%3C/svg%3E"
                    style="position:absolute;left:230px;top:0;width:40px;height:40px;border-radius:50%">
               </body></html>"##,
        );
        let pixmap = paint_dom(&tree, (280.0, 50.0), None).expect("pixmap");
        let is_white = |x, y| {
            let pixel = pixmap.pixel(x, y).expect("pixel");
            pixel.red() > 245 && pixel.green() > 245 && pixel.blue() > 245
        };
        assert!(is_white(1, 1), "a square 50% radius must clear its corner");
        let circle_top = pixmap.pixel(20, 1).expect("circle top");
        assert!(circle_top.red() > 200 && circle_top.green() < 40 && circle_top.blue() < 40);

        assert!(
            is_white(60, 3),
            "a rectangular 50% radius must use a 40x20 elliptical corner"
        );
        let ellipse_top = pixmap.pixel(90, 1).expect("ellipse top");
        let ellipse_left = pixmap.pixel(51, 20).expect("ellipse left");
        for pixel in [ellipse_top, ellipse_left] {
            assert!(pixel.blue() > 200 && pixel.red() < 40 && pixel.green() < 40);
        }

        let pill_corner = pixmap.pixel(150, 3).expect("pixel radius pill corner");
        assert!(
            pill_corner.green() > 100 && pill_corner.red() < 40 && pill_corner.blue() < 40,
            "a 20px radius must remain circular rather than resolving like 50%: {pill_corner:?}"
        );

        assert!(
            is_white(231, 1),
            "a circular replaced image must clip its raster corner"
        );
        let image_center = pixmap.pixel(250, 20).expect("image center");
        assert!(image_center.red() > 80 && image_center.blue() > 80 && image_center.green() < 40);
    }

    #[test]
    fn auto_background_size_uses_intrinsic_dimensions_and_position() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
               <div style="width:100px;height:100px;background-color:red;
                 background-image:url(&quot;data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='20'%20height='10'%3E%3Crect%20width='20'%20height='10'%20fill='blue'/%3E%3C/svg%3E&quot;);
                 background-position:right bottom;background-repeat:no-repeat"></div>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (120.0, 120.0), None).expect("pixmap");
        let background = pixmap.pixel(10, 10).expect("pixel");
        assert!(
            background.red() > 200 && background.blue() < 60,
            "the intrinsic image must not stretch across the owner"
        );
        let image = pixmap.pixel(90, 95).expect("pixel");
        assert!(
            image.blue() > 200 && image.red() < 60,
            "the 20x10 intrinsic image must anchor at bottom right"
        );
    }

    #[test]
    fn cover_and_contain_background_sizes_fit_the_positioning_area() {
        let image = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='200'%20height='100'%3E%3Crect%20width='200'%20height='100'%20fill='blue'/%3E%3C/svg%3E";
        let tree = parse_html(&format!(
            r#"<html><body style="margin:0;background:white">
               <div style="position:absolute;left:0;top:0;width:100px;height:200px;
                 background:red url(&quot;{image}&quot;) center/cover no-repeat"></div>
               <div style="position:absolute;left:100px;top:0;width:100px;height:200px;
                 background-color:red;background-image:url(&quot;{image}&quot;);
                 background-position:center;background-size:contain;background-repeat:no-repeat"></div>
               </body></html>"#
        ));
        let pixmap = paint_dom(&tree, (200.0, 200.0), None).expect("pixmap");

        let cover_edge = pixmap.pixel(50, 10).expect("cover edge");
        assert!(
            cover_edge.blue() > 200 && cover_edge.red() < 40,
            "cover must scale the 2:1 image to the owner's 200px block axis: {cover_edge:?}"
        );

        let contain_outside = pixmap.pixel(150, 60).expect("contain letterbox");
        assert!(
            contain_outside.red() > 200 && contain_outside.blue() < 40,
            "contain must leave the area outside its centered 100x50 image unpainted: {contain_outside:?}"
        );
        let contain_center = pixmap.pixel(150, 100).expect("contain center");
        assert!(
            contain_center.blue() > 200 && contain_center.red() < 40,
            "contain must paint the fitted image through the center: {contain_center:?}"
        );
    }

    #[test]
    fn ordered_background_gradients_paint_every_translucent_layer() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
               <div style="width:100px;height:100px;background-color:white;
                 background-image:
                   linear-gradient(180deg,transparent,white 85%),
                   radial-gradient(circle at top left,rgba(255,0,0,.9),transparent 50%),
                   radial-gradient(circle at top right,rgba(0,0,255,.9),transparent 50%)">
               </div>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (100.0, 100.0), None).expect("pixmap");
        let left = pixmap.pixel(4, 4).expect("top-left gradient");
        let right = pixmap.pixel(95, 4).expect("top-right gradient");
        assert!(
            left.red() > left.blue() + 80,
            "the top-left radial layer must tint its own corner: {left:?}"
        );
        assert!(
            right.blue() > right.red() + 80,
            "the later top-right radial layer must remain beneath the authored top layer: {right:?}"
        );
    }

    #[test]
    fn repeating_linear_gradient_tiles_at_background_size_over_ordered_layer() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
               <div style="width:40px;height:40px;background-color:white;
                 background-image:
                   repeating-linear-gradient(315deg,
                     rgba(0,0,0,.65) 0,rgba(0,0,0,.65) 1px,
                     transparent 0,transparent 50%),
                   linear-gradient(red,red);
                 background-size:10px 10px">
               </div>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (40.0, 40.0), None).expect("pixmap");
        let mut dark = 0usize;
        let mut red = 0usize;
        for y in 0..10 {
            for x in 0..10 {
                let first = pixmap.pixel(x, y).expect("first tile");
                let repeated = pixmap.pixel(x + 20, y + 20).expect("repeated tile");
                assert_eq!(
                    first, repeated,
                    "background-size must establish a stable 10px tile at ({x},{y})"
                );
                if first.red() < 150 {
                    dark += 1;
                } else if first.red() > 220 && first.green() < 40 && first.blue() < 40 {
                    red += 1;
                }
            }
        }
        assert!(dark > 4, "the repeating hatch must paint dark stripes");
        assert!(
            red > 20,
            "transparent hatch gaps must reveal the ordered red layer"
        );
    }

    #[test]
    fn length_background_positions_select_sprite_frames() {
        let sprite = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='48'%20height='24'%3E%3Crect%20width='24'%20height='24'%20fill='%23ff0000'/%3E%3Crect%20x='24'%20width='24'%20height='24'%20fill='%230000ff'/%3E%3C/svg%3E";
        let tree = parse_html(&format!(
            r#"<html><body style="margin:0;background:white">
               <div style="position:absolute;left:0;top:0;width:24px;height:42px;
                 background-color:black;background-image:url(&quot;{sprite}&quot;);
                 background-size:48px 24px;background-position:0;background-repeat:no-repeat"></div>
               <div style="position:absolute;left:30px;top:0;width:24px;height:42px;
                 background-color:black;background-image:url(&quot;{sprite}&quot;);
                 background-size:48px 24px;background-position:-24px;background-repeat:no-repeat"></div>
               </body></html>"#
        ));
        let pixmap = paint_dom(&tree, (60.0, 45.0), None).expect("pixmap");

        let first = pixmap.pixel(12, 21).expect("first sprite frame");
        assert!(
            first.red() > 200 && first.green() < 40 && first.blue() < 40,
            "`background-position:0` must anchor the first frame at the start edge: {first:?}"
        );
        let second = pixmap.pixel(42, 21).expect("second sprite frame");
        assert!(
            second.blue() > 200 && second.red() < 40 && second.green() < 40,
            "a negative pixel position must select the next sprite frame: {second:?}"
        );
        let above = pixmap.pixel(12, 2).expect("vertical centering");
        assert!(
            above.red() < 30 && above.green() < 30 && above.blue() < 30,
            "a one-value horizontal position must default the vertical axis to center: {above:?}"
        );
    }

    #[test]
    fn negative_text_indent_clips_label_without_hiding_background_icon() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
               <a style="display:block;width:24px;height:24px;overflow:hidden;
                 white-space:nowrap;text-indent:-9999px;color:black;background-color:blue;
                 background-image:url(&quot;data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='16'%20height='16'%3E%3Crect%20width='16'%20height='16'%20fill='red'/%3E%3C/svg%3E&quot;);
                 background-position:center;background-size:16px 16px;background-repeat:no-repeat">Bluesky (@mozilla.org)</a>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (32.0, 32.0), None).expect("pixmap");
        let corner = pixmap.pixel(1, 1).expect("anchor background");
        assert!(
            corner.blue() > 200 && corner.red() < 40 && corner.green() < 40,
            "the anchor background must remain visible: {corner:?}"
        );
        let icon = pixmap.pixel(12, 12).expect("centered SVG icon");
        assert!(
            icon.red() > 200 && icon.green() < 40 && icon.blue() < 40,
            "the background SVG must paint independently of indented text: {icon:?}"
        );
        for y in 0..24 {
            for x in 0..24 {
                let pixel = pixmap.pixel(x, y).expect("anchor pixel");
                assert!(
                    pixel.red() > 40 || pixel.green() > 40 || pixel.blue() > 40,
                    "black label glyph leaked through the 24px overflow clip at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn contextual_background_size_preserves_auto_axis_ratio() {
        let source = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='200'%20height='50'%3E%3C/svg%3E";
        let owner = crate::Rect {
            x: 0.0,
            y: 0.0,
            width: 132.0,
            height: 60.0,
        };
        let mut cache = RenderResourceCache::default();
        let image = background_image_rect(
            source,
            None,
            &owner,
            None,
            Some("calc(100% - 2rem) auto"),
            None,
            crate::BackgroundPosition::new(
                crate::BackgroundPositionAxis::percentage(0.0),
                crate::BackgroundPositionAxis::percentage(0.5),
            ),
            10.0,
            10.0,
            (1280.0, 720.0),
            &mut cache,
        )
        .unwrap();
        assert_eq!(image.width, 112.0);
        assert_eq!(image.height, 28.0);
        assert_eq!(image.y, 16.0);
    }

    #[test]
    fn paints_positioned_empty_pseudo_background_box() {
        let tree = parse_html(
            r#"<html><head><style>
               body { margin:0 }
               #host { position:relative; width:100px; height:50px }
               #host::before {
                 content:"";
                 position:absolute;
                 top:10px;
                 left:20px;
                 width:40px;
                 height:30px;
                 background:
                   linear-gradient(to bottom, transparent, #ffffff),
                   radial-gradient(circle at 50% 50%, #ebf3f9, #d6dee4);
               }
               </style></head><body><div id="host"></div></body></html>"#,
        );
        let pixmap = paint_dom(&tree, (120.0, 80.0), None).expect("pixmap");
        let center = pixmap.pixel(40, 25).expect("pixel");
        assert!(
            center.red() >= 214 && center.green() >= 222 && center.blue() >= 228,
            "transparent-to-white over a light radial layer must not darken it: {center:?}"
        );
        let outside = pixmap.pixel(5, 5).expect("pixel");
        assert_eq!(
            (outside.red(), outside.green(), outside.blue()),
            (255, 255, 255)
        );
    }

    #[test]
    fn polygon_clip_path_paints_responsive_geometry_on_elements_and_pseudos() {
        let tree = parse_html(
            r#"<html><head><style>
               html, body { margin:0 }
               @supports (clip-path:polygon(0 0,100% 0,50% 100%)) {
                 .triangle, #in-flow::before, #positioned::after {
                   clip-path:polygon(0 0,100% 0,50% 100%);
                 }
               }
               .triangle { position:absolute; left:0; top:0;
                 width:80px; height:80px; background:#f00 }
               #in-flow { position:absolute; left:100px; top:0 }
               #in-flow::before { content:""; display:block;
                 width:80px; height:80px; background:#0a0 }
               #positioned { position:absolute; left:200px; top:0;
                 width:80px; height:80px }
               #positioned::after { content:""; position:absolute; inset:0;
                 background:#00f }
               </style></head><body>
                 <div class="triangle"></div>
                 <div id="in-flow"></div>
                 <div id="positioned"></div>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (300.0, 100.0), None).expect("pixmap");
        for (name, left, channel) in [
            ("ordinary element", 0, 0),
            ("in-flow pseudo", 100, 1),
            ("positioned pseudo", 200, 2),
        ] {
            let inside = pixmap.pixel(left + 40, 65).expect("inside pixel");
            let colored = match channel {
                0 => inside.red() > 180 && inside.green() < 80 && inside.blue() < 80,
                1 => inside.green() > 100 && inside.red() < 80 && inside.blue() < 80,
                _ => inside.blue() > 180 && inside.red() < 80 && inside.green() < 80,
            };
            assert!(
                colored,
                "{name} must paint inside its percentage polygon: {inside:?}"
            );
            let outside = pixmap.pixel(left + 4, 65).expect("outside pixel");
            assert_eq!(
                (outside.red(), outside.green(), outside.blue()),
                (255, 255, 255),
                "{name} must be clipped outside its triangle: {outside:?}"
            );
        }
    }

    #[test]
    fn polygon_clip_path_honors_evenodd_fill_rule() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
               <div style="width:100px;height:100px;background:#e00;
                 clip-path:polygon(evenodd,
                   0 0,100% 0,100% 100%,0 100%,0 25%,
                   75% 25%,75% 75%,25% 75%,25% 25%,0 25%)"></div>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (120.0, 120.0), None).expect("pixmap");
        let shell = pixmap.pixel(10, 50).expect("outer shell");
        assert!(
            shell.red() > 180 && shell.green() < 80,
            "the outer contour must remain painted: {shell:?}"
        );
        let hole = pixmap.pixel(50, 50).expect("inner hole");
        assert_eq!(
            (hole.red(), hole.green(), hole.blue()),
            (255, 255, 255),
            "evenodd must cut the inner winding out of the clip: {hole:?}"
        );
    }

    #[test]
    fn degenerate_polygon_clip_path_clips_the_element_away() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
               <div style="width:80px;height:80px;background:red;
                 clip-path:polygon(20px 20px)"></div>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (100.0, 100.0), None).expect("pixmap");
        for (x, y) in [(10, 10), (20, 20), (40, 40)] {
            let pixel = pixmap.pixel(x, y).expect("pixel");
            assert_eq!(
                (pixel.red(), pixel.green(), pixel.blue()),
                (255, 255, 255),
                "a zero-area polygon must not fall back to an unclipped box: {pixel:?}"
            );
        }
    }

    #[test]
    fn paints_generated_style_images_and_sizes_content_url_as_replaced_content() {
        let tree = parse_html(
            r#"<html><head><style>
               body { margin:0 }
               #host { position:relative; width:100px; height:40px }
               #host::before {
                 content:""; position:absolute; left:0; top:0;
                 width:40px; height:40px;
                 background-image:url("data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='40'%20height='40'%3E%3Crect%20width='40'%20height='40'%20fill='blue'/%3E%3C/svg%3E");
                 background-size:100% 100%;
               }
               #host::after {
                 content:""; position:absolute; left:50px; top:0;
                 width:40px; height:40px; background-color:red;
                 mask-image:url("data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='40'%20height='40'%3E%3Ccircle%20cx='20'%20cy='20'%20r='16'%20fill='white'/%3E%3C/svg%3E");
                 mask-size:40px 40px; mask-repeat:no-repeat;
               }
               #content-image {
                 display:block;
                 content:url("data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='30'%20height='20'%3E%3Crect%20width='30'%20height='20'%20fill='lime'/%3E%3C/svg%3E");
               }
               </style></head><body>
                 <div id="host"></div><img id="content-image" alt="">
               </body></html>"#,
        );

        // The first cascade exposes the style image. Feeding its decoded
        // dimensions back through the ordinary intrinsic map must give the
        // source-less replaced element its 30x20 CSS box.
        let mut intrinsic = std::collections::HashMap::new();
        let first = layout_dom_with_web_fonts(&tree, (120.0, 80.0), &intrinsic, &[]);
        let host_id = tree
            .query_selector("#host")
            .expect("selector")
            .expect("host");
        let host_style = &first.styles[&host_id];
        assert!(
            host_style
                .before_pseudo
                .as_deref()
                .and_then(|style| style.background_image.as_deref())
                .is_some(),
            "the positioned pseudo must retain its parsed URL background"
        );
        assert!(
            host_style
                .after_pseudo
                .as_deref()
                .and_then(|style| style.mask_image.as_deref())
                .is_some(),
            "the positioned pseudo must retain its parsed URL mask"
        );
        let host_rect = first.rects[&host_id];
        assert_eq!(
            (host_rect.x, host_rect.y, host_rect.width, host_rect.height),
            (0.0, 0.0, 100.0, 40.0)
        );
        let mut cache = RenderResourceCache::default();
        let mut selected = HashMap::new();
        assert!(collect_content_image_intrinsics(
            &tree,
            &first.styles,
            None,
            &mut cache,
            &mut intrinsic,
            &mut selected,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        ));
        let laid = layout_dom_with_web_fonts(&tree, (120.0, 80.0), &intrinsic, &[]);
        let image_id = tree
            .query_selector("#content-image")
            .expect("selector")
            .expect("content image");
        let image_rect = laid.rects[&image_id];
        assert_eq!(
            (
                image_rect.x,
                image_rect.y,
                image_rect.width,
                image_rect.height
            ),
            (0.0, 40.0, 30.0, 20.0),
            "content:url must use ordinary replaced-element geometry"
        );

        let pixmap = paint_dom(&tree, (120.0, 80.0), None).expect("pixmap");
        let blue = pixmap.pixel(20, 20).expect("blue pseudo");
        assert!(
            blue.blue() > 220 && blue.red() < 40 && blue.green() < 80,
            "positioned pseudo background-image must paint: {blue:?}"
        );
        let red = pixmap.pixel(70, 20).expect("masked pseudo center");
        assert!(
            red.red() > 220 && red.green() < 40 && red.blue() < 40,
            "positioned pseudo mask center must use the authored fill: {red:?}"
        );
        let transparent_corner = pixmap.pixel(51, 1).expect("mask corner");
        assert_eq!(
            (
                transparent_corner.red(),
                transparent_corner.green(),
                transparent_corner.blue(),
            ),
            (255, 255, 255),
            "transparent mask corners must not paint the pseudo's solid box"
        );
        let green = pixmap.pixel(15, 50).expect("content image");
        assert!(
            green.green() > 220 && green.red() < 40 && green.blue() < 40,
            "content:url image must paint through the replaced-image path: {green:?}"
        );
    }

    #[test]
    fn repeated_content_image_prepare_reuses_intrinsic_and_corrects_changes() {
        const SOURCE: &str = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='12'%20height='8'%3E%3C/svg%3E";
        const CONTENT_A: &str = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='30'%20height='20'%3E%3C/svg%3E";
        const CONTENT_B: &str = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='50'%20height='10'%3E%3C/svg%3E";
        let make_tree = |content: Option<&str>| {
            let content = content
                .map(|url| format!("content:url('{url}');"))
                .unwrap_or_default();
            parse_html(&format!(
                r#"<html><body style="margin:0"><img id="target" src="{SOURCE}" style="display:block;{content}"></body></html>"#
            ))
        };
        let rect_for = |tree: &DomTree, prepared: &PreparedRender| {
            let id = tree
                .query_selector("#target")
                .expect("selector")
                .expect("target");
            prepared.layout.rects[&id]
        };

        let mut resources = RenderResourceCache::default();
        let first_tree = make_tree(Some(CONTENT_A));
        let first =
            prepare_dom(&first_tree, (100.0, 80.0), None, &mut resources).expect("first prepare");
        let first_rect = rect_for(&first_tree, &first);
        assert_eq!((first_rect.width, first_rect.height), (30.0, 20.0));
        assert_eq!(resources.content_image_layout_retries, 1);

        // Stable node + resolved URL + dimensions seed the first layout. The
        // retry counter must not move on a repeated prepare.
        let repeated = prepare_dom(&first_tree, (100.0, 80.0), None, &mut resources)
            .expect("repeated prepare");
        let repeated_rect = rect_for(&first_tree, &repeated);
        assert_eq!((repeated_rect.width, repeated_rect.height), (30.0, 20.0));
        assert_eq!(resources.content_image_layout_retries, 1);

        // A changed computed URL initially receives the remembered geometry,
        // then must correct it from the newly selected resource.
        let changed_tree = make_tree(Some(CONTENT_B));
        let changed = prepare_dom(&changed_tree, (100.0, 80.0), None, &mut resources)
            .expect("changed prepare");
        let changed_rect = rect_for(&changed_tree, &changed);
        assert_eq!((changed_rect.width, changed_rect.height), (50.0, 10.0));
        assert_eq!(resources.content_image_layout_retries, 2);

        // Removing CSS content restores the ordinary src selection and its
        // intrinsic geometry rather than leaking the remembered CSS image.
        let removed_tree = make_tree(None);
        let removed = prepare_dom(&removed_tree, (100.0, 80.0), None, &mut resources)
            .expect("removed prepare");
        let removed_id = removed_tree
            .query_selector("#target")
            .expect("selector")
            .expect("target");
        let removed_rect = removed.layout.rects[&removed_id];
        assert_eq!((removed_rect.width, removed_rect.height), (12.0, 8.0));
        assert_eq!(resources.content_image_layout_retries, 3);
        assert!(resources.content_image_intrinsics.is_empty());
        assert_eq!(
            removed.selected_images[&removed_id].resolved_url, SOURCE,
            "the HTML source must regain painting ownership"
        );
    }

    #[test]
    fn content_image_intrinsic_memory_is_bounded_and_refreshes_recency() {
        let mut resources = RenderResourceCache::with_loader_and_limits(|_url: &str| None, 2, 1024);
        let tree = parse_html(r#"<html><body><img id="a"><img id="b"><img id="c"></body></html>"#);
        let ids = ["#a", "#b", "#c"].map(|selector| {
            tree.query_selector(selector)
                .expect("selector")
                .expect("image")
        });
        resources.remember_content_image_intrinsic(
            ids[0],
            "a".into(),
            crate::ReplacedIntrinsic::from_dimensions(1.0, 1.0),
        );
        resources.remember_content_image_intrinsic(
            ids[1],
            "b".into(),
            crate::ReplacedIntrinsic::from_dimensions(2.0, 2.0),
        );
        // Refreshing id 1 makes id 2 the oldest entry.
        resources.remember_content_image_intrinsic(
            ids[0],
            "a2".into(),
            crate::ReplacedIntrinsic::from_dimensions(3.0, 3.0),
        );
        resources.remember_content_image_intrinsic(
            ids[2],
            "c".into(),
            crate::ReplacedIntrinsic::from_dimensions(4.0, 4.0),
        );

        assert_eq!(resources.content_image_intrinsics.len(), 2);
        assert!(resources.content_image_intrinsics.contains_key(&ids[0]));
        assert!(!resources.content_image_intrinsics.contains_key(&ids[1]));
        assert!(resources.content_image_intrinsics.contains_key(&ids[2]));
        assert_eq!(
            resources.content_image_intrinsics[&ids[0]].resolved_url,
            "a2"
        );
    }

    #[test]
    fn repeated_data_svg_masks_sample_radial_sources_on_every_box_path() {
        let tree = parse_html(
            r##"<html><head><style>
               html, body { margin:0 }
               .mask {
                 width:88px; height:66px;
               }
               .mask-source, #in-flow::before, #positioned::after {
                 mask-image:url("data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' width='72' height='72' viewBox='0 0 72 72'><defs><pattern id='p' patternUnits='userSpaceOnUse' width='72' height='72'><g transform='translate(36 36) rotate(-60)'><line x1='-10' y1='0' x2='10' y2='0' stroke='white' stroke-width='3' stroke-linecap='round'/></g></pattern></defs><rect width='100%' height='100%' fill='url(%23p)'/></svg>");
                 mask-size:22px 22px; mask-repeat:repeat;
               }
               #ordinary { position:absolute; left:0; top:0 }
               #in-flow { position:absolute; left:100px; top:0 }
               #in-flow::before {
                 content:""; display:block;
                 width:88px; height:66px;
                 background:radial-gradient(circle at 50% 125%,transparent 20%,#f627e3 35%,#6911d2 55%,transparent 75%);
               }
               #positioned { position:absolute; left:200px; top:0 }
               #positioned::after {
                 content:""; position:absolute; inset:0;
                 background:radial-gradient(circle at 50% 125%,transparent 20%,#f627e3 35%,#6911d2 55%,transparent 75%);
               }
               #ordinary {
                 background:radial-gradient(circle at 50% 125%,transparent 20%,#f627e3 35%,#6911d2 55%,transparent 75%);
               }
               #solid-source { position:absolute; left:0; top:80px; background-color:#00aa00 }
               #linear-source { position:absolute; left:100px; top:80px; background:linear-gradient(90deg,#ff0000,#0000ff) }
               #conic-source { position:absolute; left:200px; top:80px; background:conic-gradient(from 0deg at 50% 50%,#ff0000,#0000ff,#ff0000) }
               </style></head><body>
                 <div id="ordinary" class="mask mask-source"></div>
                 <div id="in-flow" class="mask"></div>
                 <div id="positioned" class="mask"></div>
                 <div id="solid-source" class="mask mask-source"></div>
                 <div id="linear-source" class="mask mask-source"></div>
                 <div id="conic-source" class="mask mask-source"></div>
               </body></html>"##,
        );
        let pixmap = paint_dom(&tree, (300.0, 160.0), None).expect("pixmap");
        let count_pixels = |left: u32, top: u32, predicate: fn(u8, u8, u8) -> bool| {
            (top..top + 66)
                .flat_map(|y| (left..left + 88).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let pixel = pixmap.pixel(x, y).expect("pixel");
                    predicate(pixel.red(), pixel.green(), pixel.blue())
                })
                .count()
        };
        let is_radial_color =
            |red, green, blue| red > 70 && blue > 100 && blue as u16 > green as u16 * 2;
        for (name, left) in [
            ("ordinary element", 0),
            ("in-flow pseudo", 100),
            ("positioned pseudo", 200),
        ] {
            let colored = count_pixels(left, 0, is_radial_color);
            assert!(
                colored > 20,
                "{name} must sample the radial source through the repeated SVG mask, found {colored} colored pixels"
            );
            let black = count_pixels(left, 0, |red, green, blue| {
                red < 20 && green < 20 && blue < 20
            });
            assert_eq!(
                black, 0,
                "{name} must not fall back to the default black mask fill"
            );
        }
        assert!(
            count_pixels(0, 80, |red, green, blue| green > 100
                && red < 40
                && blue < 40)
                > 20,
            "solid mask sources must keep painting"
        );
        assert!(
            count_pixels(100, 80, |red, green, blue| (red > 100 || blue > 100)
                && green < 80)
                > 20,
            "linear-gradient mask sources must keep painting"
        );
        assert!(
            count_pixels(200, 80, |red, green, blue| (red > 100 || blue > 100)
                && green < 80)
                > 20,
            "conic-gradient mask sources must keep painting"
        );
    }

    #[test]
    fn paints_empty_in_flow_generated_block_at_its_layout_rect() {
        let tree = parse_html(
            r#"<html><head><style>
               html, body { margin:0 }
               body { font-size:20px; line-height:20px }
               #host { width:200px }
               #host::before {
                 content:""; display:block; width:80px; height:40px;
                 margin-bottom:10px; background:#0066cc;
               }
               #next { width:20px; height:10px; background:#00aa00 }
               </style></head><body>
                 <div id="host">TEXT</div><div id="next"></div>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (240.0, 100.0), None).expect("pixmap");
        let generated = pixmap.pixel(40, 20).expect("generated block pixel");
        assert!(
            generated.blue() > 180 && generated.green() > 70 && generated.red() < 30,
            "the anonymous generated box must paint its own background: {generated:?}"
        );
        let following = pixmap.pixel(10, 75).expect("following block pixel");
        assert!(
            following.green() > 120 && following.red() < 30 && following.blue() < 30,
            "the following block must paint below the generated geometry: {following:?}"
        );
    }

    #[test]
    fn paints_positioned_attr_content_over_the_host_background() {
        let tree = parse_html(
            r#"<html><head><style>
               body { margin:0 }
               #cta {
                 position:relative; width:120px; height:40px; border:0;
                 padding:0; color:transparent; background:red;
               }
               #cta::before {
                 content:attr(data-label);
                 position:absolute; inset:1px;
                 display:flex; align-items:center; justify-content:center;
                 border-radius:4px; color:black; background:white;
               }
               </style></head><body>
               <button id="cta" data-label="Get Started">Get Started</button>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (140.0, 60.0), None).expect("pixmap");
        let inner = pixmap.pixel(5, 5).expect("inner pixel");
        assert_eq!(
            (inner.red(), inner.green(), inner.blue()),
            (255, 255, 255),
            "the generated box must cover the red host background"
        );
        let dark_pixels = (35..85)
            .flat_map(|x| (8..32).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                let pixel = pixmap.pixel(x, y).unwrap();
                pixel.red() < 100 && pixel.green() < 100 && pixel.blue() < 100
            })
            .count();
        assert!(dark_pixels > 10, "generated attr() text must be painted");
    }

    #[test]
    fn later_positioned_pseudo_opaquely_covers_the_earlier_one() {
        let tree = parse_html(
            r#"<html><head><style>
               body { margin:0 }
               #cta {
                 position:relative; width:120px; height:40px; padding:0;
                 color:transparent; background:black;
               }
               #cta::before {
                 content:"before";
                 position:absolute; inset:1px;
                 display:flex; align-items:center; justify-content:center;
                 color:red; background:red;
               }
               #cta::after {
                 content:"after";
                 position:absolute; inset:1px;
                 display:flex; align-items:center; justify-content:center;
                 color:blue; background:white;
               }
               </style></head><body><button id="cta">host</button></body></html>"#,
        );
        let pixmap = paint_dom(&tree, (140.0, 60.0), None).expect("pixmap");
        let inner = pixmap.pixel(5, 5).expect("inner pixel");
        assert_eq!(
            (inner.red(), inner.green(), inner.blue()),
            (255, 255, 255),
            "::after's opaque background must cover ::before"
        );
        let red_pixels = (1..119)
            .flat_map(|x| (1..39).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                let pixel = pixmap.pixel(x, y).unwrap();
                pixel.red() > 180 && pixel.green() < 80 && pixel.blue() < 80
            })
            .count();
        assert_eq!(red_pixels, 0, "::before must not bleed through ::after");
    }

    #[test]
    fn angular_absolute_primary_button_paints_one_generated_label() {
        let tree = parse_html(
            r#"<html><head><style>
               :root {
                 --orange-red:#f00; --vivid-pink:#f0f; --electric-violet:#70f;
                 --page-bg-radial-gradient:radial-gradient(circle,#fff 0%,#fff 100%);
                 --page-background:#fff; --primary-contrast:#111;
               }
               html, body { margin:0 }
               .section { position:relative; width:500px; height:300px }
               .content button { position:absolute; bottom:48px }
               .docs-primary-btn {
                 cursor:pointer; border:none; outline:none; position:relative;
                 border-radius:4px; padding:12px 24px; width:max-content;
                 color:transparent; font-size:14px; font-weight:600;
                 background:linear-gradient(90deg,var(--orange-red) 0%,
                   var(--vivid-pink) 50%,var(--electric-violet) 100%);
               }
               .docs-primary-btn::before {
                 content:attr(text); position:absolute; inset:1px;
                 background:var(--page-bg-radial-gradient); border-radius:3px;
                 display:flex; align-items:center; justify-content:center;
                 color:var(--primary-contrast);
               }
               .docs-primary-btn::after {
                 content:attr(text); position:absolute; inset:1px;
                 background:var(--page-background); border-radius:3px;
                 display:flex; align-items:center; justify-content:center;
                 color:var(--primary-contrast);
               }
               </style></head><body>
                 <section class="section"><div class="content">
                   <button id="cta" class="docs-primary-btn" text="Learn more">Learn more</button>
                 </div></section>
               </body></html>"#,
        );
        let laid = crate::layout_dom(&tree, (500.0, 300.0));
        let cta = tree.get_element_by_id("cta").unwrap();
        let rect = laid.rects[&cta];
        let style = &laid.styles[&cta];
        assert_eq!(style.color, Some([0, 0, 0, 0]));
        let pixmap = paint_dom(&tree, (500.0, 300.0), None).expect("pixmap");
        let rows = (rect.y.floor() as u32..(rect.y + rect.height).ceil() as u32)
            .map(|y| {
                (rect.x.floor() as u32..(rect.x + rect.width).ceil() as u32)
                    .filter(|&x| {
                        let pixel = pixmap.pixel(x, y).unwrap();
                        pixel.red() < 100 && pixel.green() < 100 && pixel.blue() < 100
                    })
                    .count()
            })
            .collect::<Vec<_>>();
        let ink_rows = rows
            .iter()
            .enumerate()
            .filter_map(|(row, &ink)| (ink > 0).then_some(row))
            .collect::<Vec<_>>();
        assert!(
            ink_rows.last().unwrap() - ink_rows.first().unwrap() <= 14,
            "only the generated label may paint; transparent host text leaked across rows {rows:?}"
        );
    }

    #[test]
    fn native_select_paints_only_the_selected_label_and_arrow() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
                <select id="theme">
                    <option>Light</option>
                    <option selected>Dark</option>
                </select>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (160.0, 60.0), None).expect("pixmap");
        let dark_pixels = (0..120)
            .flat_map(|x| (0..30).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                let pixel = pixmap.pixel(x, y).unwrap();
                pixel.red() < 100 && pixel.green() < 100 && pixel.blue() < 100
            })
            .count();
        assert!(
            dark_pixels > 20,
            "selected label, border, and disclosure arrow should paint"
        );
        let select = tree.get_element_by_id("theme").unwrap();
        assert_eq!(
            selected_option_label(&tree, select).as_deref(),
            Some("Dark")
        );
    }

    #[test]
    fn authored_combobox_button_paints_css_math_border() {
        let tree = parse_html(
            r#"<html style="margin:0"><head><style>
                :root {
                    --stroke-standard: calc(1 * 1px);
                    --control-height: calc(4px * 10);
                    --control-radius: calc(4px * 2);
                }
                #host { width:260px }
                #language {
                    display:flex;
                    align-items:center;
                    justify-content:space-between;
                    width:100%;
                    height:var(--control-height);
                    box-sizing:border-box;
                    border-style:solid;
                    border-width:var(--stroke-standard);
                    border-color:rgba(208,217,251,.4);
                    border-radius:var(--control-radius);
                    padding:1px 12px;
                }
            </style></head><body style="margin:0">
                <div id="host">
                    <button id="language" role="combobox">
                        <span>English (United States)</span><span>▼</span>
                    </button>
                </div>
            </body></html>"#,
        );
        let laid = crate::layout_dom(&tree, (320.0, 80.0));
        let language = tree.get_element_by_id("language").unwrap();
        let rect = laid.rects[&language];
        let style = &laid.styles[&language];
        assert_eq!((rect.width, rect.height), (260.0, 40.0));
        assert_eq!(
            style.border,
            crate::Edges {
                top: 1.0,
                right: 1.0,
                bottom: 1.0,
                left: 1.0,
            }
        );

        let pixmap = paint_dom(&tree, (320.0, 80.0), None).expect("pixmap");
        let painted_top_edge = (20..240)
            .filter(|&x| pixmap.pixel(x, 0).is_some_and(|pixel| pixel.alpha() > 0))
            .count();
        assert!(
            painted_top_edge > 200,
            "the rounded authored border should paint across the control: {painted_top_edge}"
        );
    }

    #[test]
    fn fractional_opacity_composites_boxes_and_inline_svg_once() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
                <div style="width:20px;height:20px;background:black;opacity:.05"></div>
                <svg style="position:absolute;left:30px;top:0;opacity:.05"
                     width="20" height="20" viewBox="0 0 20 20">
                    <rect width="20" height="20" fill="black"/>
                </svg>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (60.0, 30.0), None).expect("pixmap");
        for (label, x) in [("box", 10), ("svg", 40)] {
            let pixel = pixmap.pixel(x, 10).expect("pixel");
            assert!(
                (240..=244).contains(&pixel.red())
                    && pixel.red() == pixel.green()
                    && pixel.green() == pixel.blue(),
                "{label} opacity:.05 should composite black once over white: {pixel:?}"
            );
        }
    }

    #[test]
    fn opacity_is_applied_to_overlapping_children_as_one_group() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
                <div style="position:relative;width:30px;height:20px;opacity:.5">
                    <div style="position:absolute;left:0;width:20px;height:20px;background:black"></div>
                    <div style="position:absolute;left:10px;width:20px;height:20px;background:black"></div>
                </div>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (40.0, 30.0), None).expect("pixmap");
        let samples = [5, 15, 25].map(|x| pixmap.pixel(x, 10).unwrap().red());
        assert!(
            samples.iter().all(|channel| (126..=129).contains(channel)),
            "single-child and overlap regions must receive the same group alpha: {samples:?}"
        );
    }

    #[test]
    fn nested_opacity_groups_multiply_at_composite_boundaries() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
                <div style="width:20px;height:20px;opacity:.5">
                    <div style="width:20px;height:20px;background:black;opacity:.5"></div>
                </div>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (30.0, 30.0), None).expect("pixmap");
        let pixel = pixmap.pixel(10, 10).expect("pixel");
        assert!(
            (190..=193).contains(&pixel.red())
                && pixel.red() == pixel.green()
                && pixel.green() == pixel.blue(),
            "nested .5 groups should produce .25 black over white: {pixel:?}"
        );
    }

    #[test]
    fn opacity_group_contains_z_order_and_preserves_clip_transforms() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
                <div style="position:relative;width:20px;height:20px;
                            overflow:hidden;opacity:.5">
                    <div style="position:absolute;z-index:999;left:0;top:0;
                                transform:translate(10px,0);width:20px;height:20px;
                                background:red"></div>
                </div>
                <div style="position:absolute;left:30px;top:0;width:20px;height:20px;
                            opacity:.5">
                    <div style="position:absolute;z-index:999;width:20px;height:20px;
                                background:red"></div>
                </div>
                <div style="position:absolute;left:30px;top:0;width:20px;height:20px;
                            background:blue"></div>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (60.0, 30.0), None).expect("pixmap");
        let clipped_out = pixmap.pixel(5, 10).unwrap();
        let clipped_in = pixmap.pixel(15, 10).unwrap();
        let outside = pixmap.pixel(25, 10).unwrap();
        assert_eq!(
            (clipped_out.red(), clipped_out.green(), clipped_out.blue()),
            (255, 255, 255)
        );
        assert!(
            clipped_in.red() > 240 && (126..=129).contains(&clipped_in.green()),
            "translated child should be clipped then group-composited: {clipped_in:?}"
        );
        assert_eq!(
            (outside.red(), outside.green(), outside.blue()),
            (255, 255, 255)
        );
        let covered = pixmap.pixel(40, 10).unwrap();
        assert!(
            covered.blue() > 240 && covered.red() < 20,
            "high-z descendant must stay inside the earlier opacity group: {covered:?}"
        );
    }

    #[test]
    fn fixed_and_sticky_auto_z_index_contain_descendant_stacking() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
                <div id="fixed" style="position:fixed;left:0;top:0;width:20px;height:20px;background:red">
                    <div style="position:absolute;z-index:999;inset:0;background:lime"></div>
                </div>
                <div style="position:absolute;z-index:1;left:0;top:0;width:20px;height:20px;background:blue"></div>
                <div style="position:absolute;left:0;top:30px">
                    <div id="sticky" style="position:sticky;top:0;width:20px;height:20px;background:red">
                        <div style="position:absolute;z-index:999;inset:0;background:lime"></div>
                    </div>
                </div>
                <div style="position:absolute;z-index:1;left:0;top:30px;width:20px;height:20px;background:blue"></div>
            </body></html>"#,
        );
        let fixed = tree.get_element_by_id("fixed").unwrap();
        let sticky = tree.get_element_by_id("sticky").unwrap();
        let laid = crate::layout_dom(&tree, (40.0, 60.0));
        assert_eq!(stacking_z_index(&tree, &laid, fixed), Some(0));
        assert_eq!(stacking_z_index(&tree, &laid, sticky), Some(0));

        let pixmap = paint_dom(&tree, (40.0, 60.0), None).expect("pixmap");
        for (label, y) in [("fixed", 10), ("sticky", 40)] {
            let pixel = pixmap.pixel(10, y).unwrap();
            assert!(
                pixel.blue() > 240 && pixel.red() < 20 && pixel.green() < 20,
                "{label} high-z descendant escaped its auto-z stacking context: {pixel:?}",
            );
        }
    }

    #[test]
    fn static_flex_and_grid_item_z_index_paints_each_subtree_atomically() {
        let blue_image = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='100'%20height='60'%3E%3Crect%20width='100'%20height='60'%20fill='blue'/%3E%3C/svg%3E";
        let tree = parse_html(&format!(
            r#"<html><head><style>
                 body {{ margin:0 }}
                 .row {{
                   position:relative; width:120px; height:70px;
                   display:flex; align-items:flex-start;
                 }}
                 .grid {{
                   width:120px; height:70px; display:grid;
                   grid-template-columns:100px;
                   grid-template-rows:60px;
                 }}
                 .front {{
                   box-sizing:border-box; width:100px; height:60px;
                   z-index:2; background:#ff0000; border:5px solid #00ff00;
                   color:#000000; font-size:24px; line-height:30px;
                 }}
                 .cover {{
                   position:absolute; z-index:auto; left:0; top:0;
                   width:100px; height:60px; background:#0000ff;
                 }}
                 .cell {{ grid-area:1 / 1; }}
                 img.cell {{ width:100px; height:60px; }}
               </style></head><body>
                 <div class="row">
                   <div class="front">FLEX</div>
                   <div class="cover"></div>
                 </div>
                 <div class="grid">
                   <div class="front cell">GRID</div>
                   <img class="cell" alt="" src="{blue_image}">
                 </div>
                 <div class="row" style="height:60px">
                   <div class="front" style="z-index:1">LOW</div>
                   <div class="cover" style="z-index:3"></div>
                 </div>
               </body></html>"#
        ));
        let pixmap = paint_dom(&tree, (140.0, 210.0), None).expect("pixmap");

        let is_green = |x, y| {
            let pixel = pixmap.pixel(x, y).expect("border pixel");
            pixel.green() > 230 && pixel.red() < 30 && pixel.blue() < 30
        };
        let is_red = |x, y| {
            let pixel = pixmap.pixel(x, y).expect("background pixel");
            pixel.red() > 230 && pixel.green() < 30 && pixel.blue() < 30
        };
        assert!(
            is_green(2, 30) && is_red(80, 50),
            "the static flex item's border/background must paint above its later absolute sibling"
        );
        assert!(
            is_green(2, 100) && is_red(80, 120),
            "the static grid item's border/background must paint above its later image sibling"
        );

        for (label, y0) in [("flex", 0), ("grid", 70)] {
            let dark_ink = (8..92)
                .flat_map(|x| (y0 + 8..y0 + 42).map(move |y| (x, y)))
                .filter(|&(x, y)| {
                    let pixel = pixmap.pixel(x, y).expect("text pixel");
                    pixel.red() < 40 && pixel.green() < 40 && pixel.blue() < 40
                })
                .count();
            assert!(
                dark_ink > 20,
                "{label} item text must remain inside the raised atomic subtree, found {dark_ink} dark pixels"
            );
        }

        assert!(
            (0..60).all(|dy| (0..100).all(|x| {
                let pixel = pixmap.pixel(x, 140 + dy).expect("covered pixel");
                pixel.blue() > 230 && pixel.red() < 30 && pixel.green() < 30
            })),
            "a higher-z sibling must cover the lower item's background, border, and shaped text as one unit"
        );
    }

    #[test]
    fn wrapped_inline_background_and_slice_borders_paint_per_continuation() {
        let tree = parse_html(
            r#"<style>
                html,body,p { margin:0 }
                p { width:70px; font:16px/24px monospace }
                #token {
                    color:transparent; background:#ff0000; padding:0 10px;
                    border-left:2px solid #0000ff;
                    border-right:2px solid #0000ff
                }
            </style>
            <p><span id="token">aaaa aaaa aaaa</span></p>"#,
        );
        let token = tree.get_element_by_id("token").unwrap();
        let mut resources = RenderResourceCache::default();
        let mut prepared = prepare_dom(&tree, (120.0, 100.0), None, &mut resources).unwrap();
        let fragments = prepared.layout.inline_fragments[&token].clone();
        assert_eq!(fragments.len(), 3, "{fragments:?}");
        let pixmap = paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 0.0)).unwrap();
        let rgb = |x: f32, y: f32| {
            let pixel = pixmap
                .pixel(x.floor().max(0.0) as u32, y.floor().max(0.0) as u32)
                .unwrap();
            (pixel.red(), pixel.green(), pixel.blue())
        };
        let first = fragments[0];
        let middle = fragments[1];
        let last = fragments[2];
        assert_eq!(rgb(first.x + 0.5, first.y + 1.0), (0, 0, 255));
        assert_eq!(rgb(middle.x + 0.5, middle.y + 1.0), (255, 0, 0));
        assert_eq!(rgb(last.x + last.width - 0.5, last.y + 1.0), (0, 0, 255));
        let gap_y = (first.y + first.height + middle.y) * 0.5;
        assert_eq!(
            rgb(5.0, gap_y),
            (255, 255, 255),
            "the multiline bounding union must not fill the inter-line gap"
        );
    }

    #[test]
    fn overflow_clipping_keeps_physical_axes_independent() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
                <div style="position:relative;width:20px;height:20px;
                            overflow-x:clip">
                    <div style="position:absolute;left:10px;top:30px;
                                width:20px;height:10px;background:red"></div>
                </div>
                <div style="position:absolute;left:40px;top:0;width:20px;height:20px;
                            overflow-y:clip">
                    <div style="position:absolute;left:30px;top:10px;
                                width:10px;height:20px;background:blue"></div>
                </div>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (90.0, 50.0), None).expect("pixmap");

        let x_inside_y_outside = pixmap.pixel(15, 35).unwrap();
        assert!(
            x_inside_y_outside.red() > 240 && x_inside_y_outside.blue() < 20,
            "overflow-x:clip must not clip a Brave-shaped absolute child on Y: \
             {x_inside_y_outside:?}"
        );
        assert_eq!(
            pixmap.pixel(25, 35).map(|p| (p.red(), p.green(), p.blue())),
            Some((255, 255, 255)),
            "overflow-x:clip must still clip X"
        );

        let y_inside_x_outside = pixmap.pixel(75, 15).unwrap();
        assert!(
            y_inside_x_outside.blue() > 240 && y_inside_x_outside.red() < 20,
            "overflow-y:clip must not clip the reciprocal X overflow: \
             {y_inside_x_outside:?}"
        );
        assert_eq!(
            pixmap.pixel(75, 25).map(|p| (p.red(), p.green(), p.blue())),
            Some((255, 255, 255)),
            "overflow-y:clip must still clip Y"
        );
    }

    #[test]
    fn generated_boxes_use_the_hosts_overflow_on_only_the_authored_axis() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
                <div id="x"></div><div id="y"></div>
                <style>
                  #x { position:relative;width:20px;height:20px;overflow-x:clip }
                  #x::before { content:"";position:absolute;left:10px;top:30px;
                               width:20px;height:10px;background:red }
                  #y { position:absolute;left:40px;top:0;width:20px;height:20px;
                       overflow-y:clip }
                  #y::after { content:"";display:block;width:10px;height:30px;
                              margin-left:30px;background:blue }
                </style>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (90.0, 50.0), None).expect("pixmap");

        let positioned_y_overflow = pixmap.pixel(15, 35).unwrap();
        assert!(
            positioned_y_overflow.red() > 240 && positioned_y_overflow.blue() < 20,
            "positioned pseudo must remain visible outside Y for overflow-x:clip: \
             {positioned_y_overflow:?}"
        );
        assert_eq!(
            pixmap.pixel(25, 35).map(|p| (p.red(), p.green(), p.blue())),
            Some((255, 255, 255)),
            "positioned pseudo must be clipped on X"
        );

        let in_flow_x_overflow = pixmap.pixel(75, 10).unwrap();
        assert!(
            in_flow_x_overflow.blue() > 240 && in_flow_x_overflow.red() < 20,
            "in-flow pseudo must remain visible outside X for overflow-y:clip: \
             {in_flow_x_overflow:?}"
        );
        assert_eq!(
            pixmap.pixel(75, 25).map(|p| (p.red(), p.green(), p.blue())),
            Some((255, 255, 255)),
            "in-flow pseudo must be clipped on Y"
        );
    }

    #[test]
    fn later_element_paints_over_earlier() {
        // A blue div nested inside a red one: both cover the origin, and blue
        // (a descendant, later in tree order) paints over red.
        let tree = parse_html(
            "<html><body>\
             <div style=\"background-color:red; width:100px; height:100px\">\
               <div style=\"background-color:blue; width:50px; height:50px\"></div>\
             </div>\
             </body></html>",
        );
        let pixmap = paint_dom(&tree, (200.0, 200.0), None).expect("pixmap");
        let p = pixmap.pixel(5, 5).expect("pixel");
        assert!(
            p.blue() > 200,
            "expected blue to paint over red, got {:?}",
            p
        );
    }

    #[test]
    fn nested_translate_accumulates_through_subtree() {
        // Parent red box (position:absolute at 0,0, 20x20) translated by
        // (50,60). Child blue box (10x10, in-flow at the red box's origin)
        // translated by an additional (30,0). The child's painted position must
        // be the SUM of both translates, (50+30, 60+0) = (80,60), proving an
        // ancestor's translate offsets the whole subtree on top of the node's
        // own translate.
        let tree = parse_html(
            "<html><body style=\"margin:0\">\
             <div style=\"position:relative; width:200px; height:200px\">\
               <div style=\"position:absolute; top:0; left:0; width:20px; height:20px; \
                            background:#ff0000; transform:translate(50px,60px)\">\
                 <div style=\"width:10px; height:10px; background:#0000ff; \
                              transform:translate(30px,0)\"></div>\
               </div>\
             </div>\
             </body></html>",
        );
        let pixmap = paint_dom(&tree, (200.0, 200.0), None).expect("pixmap");
        // Child blue lands at (80..90, 60..70).
        let blue = pixmap.pixel(85, 65).expect("pixel");
        assert!(
            blue.blue() > 200 && blue.red() < 60,
            "expected blue child at accumulated offset (80,60), got {:?}",
            blue
        );
        // Parent red lands at (50..70, 60..80); sample where the blue child does
        // not cover.
        let red = pixmap.pixel(55, 75).expect("pixel");
        assert!(
            red.red() > 200 && red.blue() < 60,
            "expected red parent at its own translate (50,60), got {:?}",
            red
        );
        // Nothing painted at the pre-transform origin: both boxes moved away.
        let origin = pixmap.pixel(5, 5).expect("pixel");
        assert_eq!(
            (origin.red(), origin.green(), origin.blue()),
            (255, 255, 255)
        );
    }

    #[test]
    fn rotate_and_scale_paint_complete_mixed_subtrees() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
              <div style="position:absolute;left:20px;top:20px;width:60px;height:40px;
                          background:#ff0000;transform-origin:0 0;transform:scale(2)">
                <span style="color:#0000ff;font:12px sans-serif">MMMM</span>
                <img alt="" src="data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='10'%20height='10'%3E%3Crect%20width='10'%20height='10'%20fill='%2300ff00'/%3E%3C/svg%3E"
                     style="position:absolute;left:40px;top:20px;width:10px;height:10px">
              </div>
              <div style="position:absolute;left:180px;top:20px;width:30px;height:20px;
                          background:#ff00ff;transform-origin:0 0;transform:rotate(90deg)">
                <span style="color:#0000ff;font:10px sans-serif">M</span>
                <img alt="" src="data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='10'%20height='10'%3E%3Crect%20width='10'%20height='10'%20fill='%2300ffff'/%3E%3C/svg%3E"
                     style="position:absolute;left:20px;top:0;width:10px;height:10px">
              </div>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (240.0, 120.0), None).expect("pixmap");

        let scaled_box = pixmap.pixel(30, 70).expect("scaled box");
        assert!(scaled_box.red() > 220 && scaled_box.green() < 40);
        let scaled_image = pixmap.pixel(110, 70).expect("scaled image");
        assert!(scaled_image.green() > 220 && scaled_image.red() < 40);
        assert!(
            (20..100).any(|x| (20..60).any(|y| {
                let pixel = pixmap.pixel(x, y).expect("scaled text region");
                pixel.red() < 80 && pixel.green() < 80 && pixel.blue() < 80
            })),
            "text must be rasterized inside the scaled atomic subtree"
        );

        let rotated_box = pixmap.pixel(165, 25).expect("rotated box");
        assert!(rotated_box.red() > 220 && rotated_box.blue() > 220);
        let rotated_image = pixmap.pixel(175, 45).expect("rotated image");
        assert!(rotated_image.green() > 220 && rotated_image.blue() > 220);
    }

    #[test]
    fn transform_function_order_nested_world_matrices_and_cssom_aabbs() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
              <div id="translate-rotate" style="position:absolute;left:50px;top:50px;
                   width:20px;height:10px;transform-origin:0 0;
                   transform:translateX(100px) rotate(90deg)"></div>
              <div id="rotate-translate" style="position:absolute;left:50px;top:50px;
                   width:20px;height:10px;transform-origin:0 0;
                   transform:rotate(90deg) translateX(100px)"></div>
              <div id="parent" style="position:absolute;left:50px;top:200px;width:100px;
                   height:100px;transform-origin:0 0;transform:scale(2)">
                <div id="child" style="position:absolute;left:10px;top:20px;width:10px;
                     height:20px;transform-origin:0 0;transform:rotate(90deg)"></div>
                <div id="captured-fixed" style="position:fixed;left:0;top:0;width:10px;
                     height:10px"></div>
              </div>
              <div id="center-origin" style="position:absolute;left:100px;top:400px;
                   width:20px;height:10px;transform:rotate(90deg)"></div>
              <div id="cssom-transform" style="position:absolute;left:200px;top:400px;
                   width:20px;height:10px;transform:translateX(50%);translate:7px;
                   rotate:30deg;scale:2"></div>
            </body></html>"#,
        );
        let mut resources = RenderResourceCache::default();
        let prepared = prepare_dom(&tree, (500.0, 600.0), None, &mut resources)
            .expect("prepared transform geometry");
        let assert_rect = |actual: crate::Rect, expected: crate::Rect| {
            assert!((actual.x - expected.x).abs() < 0.02, "x: {actual:?}");
            assert!((actual.y - expected.y).abs() < 0.02, "y: {actual:?}");
            assert!(
                (actual.width - expected.width).abs() < 0.02,
                "width: {actual:?}"
            );
            assert!(
                (actual.height - expected.height).abs() < 0.02,
                "height: {actual:?}"
            );
        };
        let first = tree.get_element_by_id("translate-rotate").unwrap();
        let second = tree.get_element_by_id("rotate-translate").unwrap();
        assert_rect(
            prepared.document_rect(first).unwrap(),
            crate::Rect {
                x: 140.0,
                y: 50.0,
                width: 10.0,
                height: 20.0,
            },
        );
        assert_rect(
            prepared.document_rect(second).unwrap(),
            crate::Rect {
                x: 40.0,
                y: 150.0,
                width: 10.0,
                height: 20.0,
            },
        );
        let child = tree.get_element_by_id("child").unwrap();
        let child_rect = crate::Rect {
            x: 30.0,
            y: 240.0,
            width: 40.0,
            height: 20.0,
        };
        assert_rect(prepared.document_rect(child).unwrap(), child_rect);
        assert_rect(
            prepared.viewport_client_rects(child, (0.0, 0.0)).unwrap()[0],
            child_rect,
        );
        let fixed = tree.get_element_by_id("captured-fixed").unwrap();
        assert_rect(
            prepared.document_rect(fixed).unwrap(),
            crate::Rect {
                x: 50.0,
                y: 200.0,
                width: 20.0,
                height: 20.0,
            },
        );
        let centered = tree.get_element_by_id("center-origin").unwrap();
        assert_rect(
            prepared.document_rect(centered).unwrap(),
            crate::Rect {
                x: 105.0,
                y: 395.0,
                width: 10.0,
                height: 20.0,
            },
        );
        let cssom = tree.get_element_by_id("cssom-transform").unwrap();
        let computed = prepared.computed_style(cssom).unwrap();
        assert_eq!(computed["transform"], "matrix(1, 0, 0, 1, 10, 0)");
        assert_eq!(computed["translate"], "7px 0px");
        assert_eq!(computed["rotate"], "30deg");
        assert_eq!(computed["scale"], "2 2");
    }

    #[test]
    fn transformed_subtree_is_clipped_by_outside_ancestor() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
              <div style="position:absolute;left:20px;top:20px;width:40px;height:40px;
                          overflow:hidden">
                <div style="position:absolute;left:30px;top:10px;width:20px;height:20px;
                            background:#00aa00;transform-origin:0 0;transform:scale(2)"></div>
              </div>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (100.0, 80.0), None).expect("pixmap");
        let inside = pixmap.pixel(55, 40).expect("inside ancestor clip");
        assert!(inside.green() > 120 && inside.red() < 40);
        let outside = pixmap.pixel(65, 40).expect("outside ancestor clip");
        assert_eq!(
            (outside.red(), outside.green(), outside.blue()),
            (255, 255, 255)
        );
    }

    #[test]
    fn no_transform_keeps_identity_geometry_and_baseline_paint() {
        let tree = parse_html(
            r#"<html><body style="margin:0"><div id="plain" style="position:absolute;
               left:10px;top:12px;width:20px;height:15px;background:#ff0000"></div>
               </body></html>"#,
        );
        let mut resources = RenderResourceCache::default();
        let prepared = prepare_dom(&tree, (80.0, 60.0), None, &mut resources).unwrap();
        let plain = tree.get_element_by_id("plain").unwrap();
        assert!(prepared.layout().transforms.is_empty());
        assert_eq!(
            prepared.document_rect(plain),
            Some(crate::Rect {
                x: 10.0,
                y: 12.0,
                width: 20.0,
                height: 15.0
            })
        );
        let pixmap = paint_dom(&tree, (80.0, 60.0), None).unwrap();
        let painted = pixmap.pixel(15, 15).unwrap();
        assert!(painted.red() > 220 && painted.green() < 40);
        let empty = pixmap.pixel(5, 5).unwrap();
        assert_eq!((empty.red(), empty.green(), empty.blue()), (255, 255, 255));
    }

    #[test]
    fn transformed_image_is_clipped_inside_overflow_border() {
        // CSS overflow clipping belongs to the owner's padding box. The
        // translated images paint after the viewport's border in tree order,
        // so a border-box clip would let them overwrite the border.
        let tree = parse_html(
            r#"<html><body style="margin:0">
               <div style="width:100px;height:60px;overflow:hidden;
                           border:4px solid red">
                 <div style="display:flex;transform:translate(-50px,0)">
                   <img alt="" src="data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='100'%20height='60'%3E%3Crect%20width='100'%20height='60'%20fill='blue'/%3E%3C/svg%3E"
                        style="width:100px;height:60px;object-fit:cover;flex-shrink:0">
                   <img alt="" src="data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='100'%20height='60'%3E%3Crect%20width='100'%20height='60'%20fill='blue'/%3E%3C/svg%3E"
                        style="width:100px;height:60px;object-fit:cover;flex-shrink:0">
                 </div>
               </div>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (140.0, 90.0), None).expect("pixmap");

        for &(x, y) in &[(0, 30), (3, 30), (104, 30), (107, 30), (50, 0), (50, 67)] {
            let pixel = pixmap.pixel(x, y).expect("border pixel");
            assert!(
                pixel.red() > 220 && pixel.green() < 40 && pixel.blue() < 40,
                "translated image must not overwrite border pixel ({x},{y}): {pixel:?}"
            );
        }
        let content = pixmap.pixel(50, 30).expect("content pixel");
        assert!(
            content.blue() > 220 && content.red() < 40,
            "translated cover image must remain visible inside padding box: {content:?}"
        );
    }

    #[test]
    fn video_poster_supplies_intrinsic_size_and_paints_as_replaced_content() {
        const POSTER: &str = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='300'%20height='100'%3E%3Crect%20width='100'%20height='100'%20fill='%23ff0000'/%3E%3Crect%20x='100'%20width='100'%20height='100'%20fill='%2300ff00'/%3E%3Crect%20x='200'%20width='100'%20height='100'%20fill='%230000ff'/%3E%3C/svg%3E";
        let tree = parse_html(&format!(
            r#"<html><body style="margin:0;background:white">
                <video id="poster" style="position:absolute;left:0;top:0" poster="{POSTER}"></video>
                <video id="positioned" style="position:absolute;left:0;top:110px;width:100px;height:100px;object-fit:cover;object-position:right center;border-radius:20px;opacity:.5" poster="{POSTER}"></video>
            </body></html>"#
        ));
        let mut resources = RenderResourceCache::default();
        let mut prepared = prepare_dom(&tree, (360.0, 220.0), None, &mut resources)
            .expect("poster-aware layout");
        let video = tree.get_element_by_id("poster").expect("video");
        let rect = prepared.document_rect(video).expect("video rect");
        assert_eq!((rect.width, rect.height), (300.0, 100.0));

        let pixmap = paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 0.0))
            .expect("poster paint");
        let red = pixmap.pixel(50, 50).expect("red stripe");
        let green = pixmap.pixel(150, 50).expect("green stripe");
        let blue = pixmap.pixel(250, 50).expect("blue stripe");
        assert!(red.red() > 240 && red.green() < 20 && red.blue() < 20);
        assert!(green.green() > 240 && green.red() < 20 && green.blue() < 20);
        assert!(blue.blue() > 240 && blue.red() < 20 && blue.green() < 20);

        let rounded_corner = pixmap.pixel(0, 110).expect("rounded corner");
        assert!(
            rounded_corner.red() > 245
                && rounded_corner.green() > 245
                && rounded_corner.blue() > 245,
            "poster must be clipped by the video radius: {rounded_corner:?}"
        );
        let positioned = pixmap.pixel(50, 160).expect("positioned poster center");
        assert!(
            positioned.red() > 100
                && positioned.red() < 160
                && positioned.green() > 100
                && positioned.green() < 160
                && positioned.blue() > 240,
            "right object-position must select the blue stripe and opacity must composite it: {positioned:?}"
        );
    }

    #[test]
    fn projected_image_transform_enters_overflow_clip() {
        // Chromium reduction: without the projected rotate/scale, this image is
        // wholly left of the clip. Its transformed right edge reaches x=53.14.
        let tree = parse_html(
            r#"<html><body style="margin:0">
               <div style="position:relative;width:120px;height:100px;overflow:hidden">
                 <div style="position:absolute;left:-60px;top:50px;transform-origin:0 0;
                             transform:rotateX(60deg) rotateZ(-45deg);scale:200%">
                   <img alt="" src="data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='40'%20height='40'%3E%3Crect%20width='40'%20height='40'%20fill='red'/%3E%3C/svg%3E"
                        style="display:block;width:40px;height:40px">
                 </div>
               </div>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (160.0, 120.0), None).expect("pixmap");

        assert!(
            (0..100).any(|y| (0..120).any(|x| {
                let pixel = pixmap.pixel(x, y).expect("inside viewport");
                pixel.red() > 220 && pixel.green() < 40 && pixel.blue() < 40
            })),
            "projected image should enter the overflow clip"
        );
        assert!(
            (0..120).all(|y| (120..160).all(|x| {
                let pixel = pixmap.pixel(x, y).expect("outside clip");
                pixel.red() > 240 && pixel.green() > 240 && pixel.blue() > 240
            })),
            "projected image must remain clipped to its overflow ancestor"
        );
    }

    #[test]
    fn translate_offscreen_box_is_not_painted() {
        // translate(-10000px,0) shoves the box far off the left edge (the old
        // hidden skip-link idiom); it must not paint anywhere on the canvas.
        let tree = parse_html(
            "<html><body>\
             <div style=\"position:absolute; top:0; left:0; width:50px; height:50px; \
                          background:#ff0000; transform:translate(-10000px,0)\"></div>\
             </body></html>",
        );
        let pixmap = paint_dom(&tree, (200.0, 200.0), None).expect("pixmap");
        let mut any_red = false;
        'scan: for y in 0..200 {
            for x in 0..200 {
                let p = pixmap.pixel(x, y).expect("pixel");
                if p.red() > 200 && p.green() < 60 && p.blue() < 60 {
                    any_red = true;
                    break 'scan;
                }
            }
        }
        assert!(
            !any_red,
            "translate(-10000px,0) box should be off-screen and unpainted"
        );
    }

    #[test]
    fn translate_percent_centers_absolute_box() {
        // The canonical centering idiom: an absolutely-positioned box at
        // top:50%/left:50% of its containing block pulled back by
        // translate(-50%,-50%) of its own size centers within it. In a 200x200
        // container a 40x40 box centers at (100,100), so its border box (with
        // top-left at 100,100 before the transform) becomes (80..120, 80..120).
        let tree = parse_html(
            "<html><body style=\"margin:0\">\
             <div style=\"position:relative; width:200px; height:200px\">\
               <div style=\"position:absolute; top:50%; left:50%; width:40px; height:40px; \
                            background:#ff0000; transform:translate(-50%,-50%)\"></div>\
             </div>\
             </body></html>",
        );
        let pixmap = paint_dom(&tree, (200.0, 200.0), None).expect("pixmap");
        let center = pixmap.pixel(100, 100).expect("pixel");
        assert!(
            center.red() > 200 && center.blue() < 60,
            "expected centered red box, got {:?}",
            center
        );
        // Just outside the centered box stays white.
        let outside = pixmap.pixel(70, 70).expect("pixel");
        assert_eq!(
            (outside.red(), outside.green(), outside.blue()),
            (255, 255, 255)
        );
    }

    #[test]
    fn paints_text_color() {
        let tree = parse_html(
            "<html><body><div style=\"color: #00ff00; width: 100px; height: 100px\">Hello</div></body></html>",
        );
        let pixmap = paint_dom(&tree, (200.0, 200.0), None).expect("pixmap");
        let mut found_green = false;
        for y in 0..200 {
            for x in 0..200 {
                let p = pixmap.pixel(x, y).expect("pixel");
                if p.green() > 200 && p.red() < 50 && p.blue() < 50 {
                    found_green = true;
                    break;
                }
            }
            if found_green {
                break;
            }
        }
        assert!(found_green, "expected green text to be painted");
    }

    #[test]
    fn word_measurement_honors_generic_font_family() {
        let sans = measure_text("iiiiiiii", 16.0, false, Some("sans-serif"));
        let mono = measure_text("iiiiiiii", 16.0, false, Some("monospace"));
        assert!(
            mono > sans * 1.5,
            "monospace advances must be used for code text: sans={sans}, mono={mono}"
        );

        let sample = "Build fast, responsive sites with Bootstrap";
        let system = measure_text(sample, 64.0, false, Some("system-ui, sans-serif"));
        let arial = measure_text(sample, 64.0, false, Some("Arial, sans-serif"));
        assert!(
            system > arial * 1.08,
            "Chromium's Linux system-ui face is DejaVu Sans, not the narrower \
             Liberation Sans used for Arial: system={system}, arial={arial}"
        );
    }

    #[test]
    fn paints_vendor_gradient_on_inline_text_span() {
        // Vue and many other framework sites put the gradient on an inline
        // accent span, not on the whole heading. The surrounding text must
        // keep its normal color while this span samples both gradient ends.
        let tree = parse_html(
            r#"<html><head><style>
               h1 { color:#17233c; font-size:50px; margin:0 }
               html:not(.dark) .accent[data-v-x] {
                 -webkit-text-fill-color:transparent;
                 background:-webkit-linear-gradient(315deg,#42d392 25%,#647eff);
                 -webkit-background-clip:text;
                 background-clip:text
               }
               </style></head><body style="margin:0">
               <h1>The <span class="accent" data-v-x>Progressive</span></h1>
               </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (500.0, 100.0), None).expect("pixmap");
        let mut green = false;
        let mut blue = false;
        let mut normal = false;
        for pixel in pixmap.pixels() {
            let (r, g, b) = (pixel.red(), pixel.green(), pixel.blue());
            green |= g > r.saturating_add(20) && g > b.saturating_add(10);
            blue |= b > r.saturating_add(20) && b > g.saturating_add(5);
            normal |= b > g.saturating_add(10) && r < 80 && g < 100;
        }
        assert!(
            normal,
            "surrounding heading text should retain its normal color"
        );
        assert!(
            green && blue,
            "inline accent should contain both gradient colors"
        );
    }

    #[test]
    fn serializes_inline_svg_subtree() {
        // A sprite-style svg: a <use> that references a <symbol> in the same
        // document must survive serialization so resvg can resolve it.
        let tree = parse_html(
            r##"<html><body><svg viewBox="0 0 10 10"><use href="#a"/><symbol id="a"><path d="M0 0h10v10z"/></symbol></svg></body></html>"##,
        );
        let svg = tree.query_selector("svg").unwrap().unwrap();
        let out = serialize_svg(&tree, svg);
        assert!(out.starts_with("<svg"), "root svg tag: {out}");
        assert!(
            out.contains(r#"viewBox="0 0 10 10""#),
            "viewBox preserved: {out}"
        );
        assert!(
            out.contains(r#"xmlns="http://www.w3.org/2000/svg""#),
            "xmlns injected: {out}"
        );
        assert!(
            out.contains("<use") && out.contains(r##"href="#a""##),
            "use + href: {out}"
        );
        assert!(
            out.contains("<symbol") && out.contains(r#"id="a""#),
            "symbol id: {out}"
        );
        assert!(
            out.contains("<path") && out.contains("</path>"),
            "path opened + closed: {out}"
        );
        assert!(out.trim_end().ends_with("</svg>"), "root closed: {out}");
        // The serialized string parses as a standalone SVG document.
        let opts = usvg::Options::default();
        assert!(
            usvg::Tree::from_data(out.as_bytes(), &opts).is_ok(),
            "usvg should parse serialized svg: {out}",
        );
    }

    #[test]
    fn inline_svg_keeps_author_css_and_embedded_text_fonts() {
        // Inline SVG is parsed in the HTML document and styled by the page's
        // author sheet, then serialized into a standalone document for resvg.
        // The standalone boundary must not erase a CSS fill/font and inherit
        // the root's `fill:none`, which made text-only illustrations blank.
        let tree = parse_html(
            r##"<html><head><style>
                .art > text {
                    fill:#00cc55;
                    font-family:sans-serif;
                    font-size:24px;
                    font-weight:400
                }
                </style></head><body style="margin:0">
                <svg class="art" width="120" height="50"
                     viewBox="0 0 120 50" fill="none">
                    <text x="4" y="32">SVG text</text>
                </svg>
                </body></html>"##,
        );
        let svg = tree.query_selector("svg").unwrap().unwrap();
        let layout = crate::dom::layout_dom(&tree, (160.0, 80.0));
        let markup = serialize_svg_styled(
            &tree,
            svg,
            &layout.styles,
            &layout.custom_properties,
            None,
        );
        assert!(
            markup.contains("fill:#00cc55!important"),
            "computed author fill must cross the standalone boundary: {markup}"
        );
        assert!(
            markup.contains("font-size:24px!important"),
            "computed SVG font size must cross the standalone boundary: {markup}"
        );

        let pixmap = paint_dom(&tree, (160.0, 80.0), None).expect("pixmap");
        let painted_green = pixmap
            .pixels()
            .iter()
            .any(|pixel| pixel.green() > 150 && pixel.red() < 80 && pixel.blue() < 120);
        assert!(
            painted_green,
            "author-styled SVG text should rasterize with embedded fonts"
        );
    }

    #[test]
    fn injects_xmlns_only_when_absent() {
        let tree = parse_html(
            r#"<html><body><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4"><rect width="4" height="4"/></svg></body></html>"#,
        );
        let svg = tree.query_selector("svg").unwrap().unwrap();
        let out = serialize_svg(&tree, svg);
        assert_eq!(
            out.matches("xmlns=").count(),
            1,
            "no duplicate xmlns: {out}"
        );
    }

    #[test]
    fn paints_inline_svg() {
        // The <rect> inside an inline svg must rasterize (it is not an <img>).
        let tree = parse_html(
            r##"<html><body><svg width="40" height="40" viewBox="0 0 40 40"><rect x="0" y="0" width="40" height="40" fill="#ff0000"/></svg></body></html>"##,
        );
        let pixmap = paint_dom(&tree, (200.0, 200.0), None).expect("pixmap");
        let mut found_red = false;
        'outer: for y in 0..80 {
            for x in 0..80 {
                let p = pixmap.pixel(x, y).expect("pixel");
                if p.red() > 200 && p.green() < 60 && p.blue() < 60 {
                    found_red = true;
                    break 'outer;
                }
            }
        }
        assert!(found_red, "expected inline svg <rect> to paint red");
    }

    #[test]
    fn svg_missing_root_height_uses_the_final_css_viewport() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg"
            width="32" viewBox="0 0 223 236">
            <rect width="223" height="236" fill="#ed174c"/>
        </svg>"##;
        let pixmap = render_svg(svg, 32, 34).expect("svg raster");
        let mut min_y = 34u32;
        let mut max_y = 0u32;
        for y in 0..34 {
            for x in 0..32 {
                let pixel = pixmap.pixel(x, y).unwrap();
                if pixel.alpha() > 0 {
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        assert!(
            max_y.saturating_sub(min_y) >= 30,
            "viewBox artwork should fill the resolved 32x34 viewport, got rows {min_y}..{max_y}"
        );
    }

    #[test]
    fn paints_inline_svg_current_color_from_computed_style() {
        let tree = parse_html(
            r##"<html><body><svg style="color:#0784aa" width="40" height="40" viewBox="0 0 40 40"><circle cx="20" cy="20" r="18" fill="currentColor"/></svg></body></html>"##,
        );
        let pixmap = paint_dom(&tree, (80.0, 80.0), None).expect("pixmap");
        let mut found = false;
        for pixel in pixmap.pixels() {
            found |= pixel.blue() > 120 && pixel.green() > 80 && pixel.red() < 40;
        }
        assert!(
            found,
            "computed color should resolve currentColor in inline svg"
        );
    }

    #[test]
    fn inline_svg_resolves_custom_property_presentation_attributes_like_chromium() {
        assert!(svg_css_presentation_attribute("transform-origin"));
        assert!(!svg_css_presentation_attribute("transform"));
        assert!(!svg_css_presentation_attribute("d"));
        assert!(!svg_css_presentation_attribute("viewBox"));
        assert!(!svg_css_presentation_attribute("id"));
        let tree = parse_html(
            r##"<html><head><style>#winner{fill:#a030c0}</style></head>
            <body style="margin:0">
              <svg id="icon" width="70" height="10" viewBox="0 0 70 10"
                   fill="#176b75"
                   style="display:block;--direct:#dc1e28;--nested:#c02020;
                          --paint:currentColor;--x:10;--w:10;--stroke-width:2;
                          --cycle-a:var(--cycle-b);--cycle-b:var(--cycle-a);
                          color:#e09020">
                <defs>
                  <linearGradient id="gradient">
                    <stop id="stop" offset="0" stop-color="var(--nested)"/>
                  </linearGradient>
                  <path id="probe" d="var(--path)" fill="none"
                        stroke="var(--direct)"
                        stroke-width="var(--stroke-width,4)"/>
                </defs>
                <rect id="direct" x="0" width="10" height="10"
                      fill="var(--direct,rgb(255,255,255))"/>
                <rect id="nested" x="var(--x)" width="var(--w,5)" height="10"
                      style="--nested:#20c060"
                      fill="var(--missing,var(--nested,#ffffff))"/>
                <rect id="cycle" x="20" width="10" height="10"
                      fill="var(--cycle-a,#3040e0)"/>
                <rect id="current" x="30" width="10" height="10"
                      fill="var(--paint,#000000)"/>
                <rect id="winner" x="40" width="10" height="10"
                      fill="var(--direct,#ffffff)"/>
                <rect id="invalid" x="50" width="10" height="10"
                      fill="var(--missing)"/>
                <rect id="invalid-fallback" x="60" width="10" height="10"
                      fill="var(--missing,definitely-not-a-paint)"/>
              </svg>
            </body></html>"##,
        );
        let svg = tree.query_selector("#icon").unwrap().unwrap();
        let laid = crate::dom::layout_dom(&tree, (70.0, 10.0));
        let markup = serialize_svg_styled(
            &tree,
            svg,
            &laid.styles,
            &laid.custom_properties,
            None,
        );

        assert!(
            markup.contains(r##"fill="#dc1e28""##)
                && markup.contains(r##"stop-color="#c02020""##)
                && markup.contains(r##"stroke="#dc1e28""##)
                && markup.contains(r#"stroke-width="2""#)
                && markup.contains(r#"x="10""#)
                && markup.contains(r#"width="10""#),
            "computed custom properties must cross the resvg boundary: {markup}"
        );
        assert!(
            markup.contains(r#"d="var(--path)""#),
            "non-CSS XML attributes must remain literal: {markup}"
        );
        assert!(
            !markup.contains("fill=\"var(")
                && !markup.contains("stroke=\"var(")
                && !markup.contains("stop-color=\"var("),
            "supported presentation attributes must not reach usvg unresolved: {markup}"
        );
        let invalid = markup
            .split("<rect id=\"invalid\"")
            .nth(1)
            .and_then(|tail| tail.split_once('>'))
            .map(|(tag, _)| tag)
            .expect("serialized invalid rect");
        assert!(
            !invalid.contains("fill="),
            "guaranteed-invalid var() must become inherited/initial, not usvg black: {invalid}"
        );
        let invalid_fallback = markup
            .split("<rect id=\"invalid-fallback\"")
            .nth(1)
            .and_then(|tail| tail.split_once('>'))
            .map(|(tag, _)| tag)
            .expect("serialized invalid-fallback rect");
        assert!(
            !invalid_fallback.contains("fill="),
            "invalid fallback grammar must inherit instead of becoming usvg black: {invalid_fallback}"
        );
        assert!(
            markup.contains("fill:#a030c0!important"),
            "author CSS must keep precedence over the presentation attribute: {markup}"
        );

        let pixmap = paint_dom(&tree, (70.0, 10.0), None).expect("paint custom-property svg");
        let rgb = |x| {
            let pixel = pixmap.pixel(x, 5).expect("SVG sample");
            (pixel.red(), pixel.green(), pixel.blue())
        };
        assert_eq!(rgb(5), (0xdc, 0x1e, 0x28), "defined token");
        assert_eq!(rgb(15), (0x20, 0xc0, 0x60), "nested fallback");
        assert_eq!(rgb(25), (0x30, 0x40, 0xe0), "cycle fallback");
        assert_eq!(rgb(35), (0xe0, 0x90, 0x20), "currentColor token");
        assert_eq!(rgb(45), (0xa0, 0x30, 0xc0), "author CSS winner");
        assert_eq!(rgb(55), (0x17, 0x6b, 0x75), "invalid var inherits fill");
        assert_eq!(
            rgb(65),
            (0x17, 0x6b, 0x75),
            "invalid fallback inherits fill"
        );
    }

    #[test]
    fn computed_style_exposes_text_break_longhands_and_alias() {
        let tree = parse_html(
            r#"<style>#copy{overflow-wrap:anywhere;word-break:keep-all}</style>
               <p id="copy">copy</p>"#,
        );
        let copy = tree.get_element_by_id("copy").unwrap();
        let mut resources = RenderResourceCache::default();
        let prepared =
            prepare_dom(&tree, (320.0, 200.0), None, &mut resources).expect("prepared render");
        let computed = prepared.computed_style(copy).expect("computed style");

        assert_eq!(
            computed.get("overflow-wrap").map(String::as_str),
            Some("anywhere")
        );
        assert_eq!(
            computed.get("word-wrap").map(String::as_str),
            Some("anywhere")
        );
        assert_eq!(
            computed.get("word-break").map(String::as_str),
            Some("keep-all")
        );
    }

    #[test]
    fn computed_style_exposes_float_and_clear() {
        let tree = parse_html(
            r#"<style>#left{float:left} #right{float:right;clear:both}</style>
               <div id="left"></div><div id="right"></div><div id="plain"></div>"#,
        );
        let mut resources = RenderResourceCache::default();
        let prepared =
            prepare_dom(&tree, (320.0, 200.0), None, &mut resources).expect("prepared render");
        let computed = |id| {
            prepared
                .computed_style(tree.get_element_by_id(id).unwrap())
                .expect("computed style")
        };

        assert_eq!(computed("left")["float"], "left");
        assert_eq!(computed("left")["clear"], "none");
        assert_eq!(computed("right")["float"], "right");
        assert_eq!(computed("right")["clear"], "both");
        assert_eq!(computed("plain")["float"], "none");
        assert_eq!(computed("plain")["clear"], "none");
    }

    #[test]
    fn paints_inline_svg_with_framework_colon_attribute() {
        let tree = obscura_dom::parse_html(
            r##"<html><body><svg q:id="f" width="40" height="40" viewBox="0 0 40 40"><rect width="40" height="40" fill="#18b6f6"/></svg></body></html>"##,
        );
        let output = paint_dom(&tree, (80.0, 80.0), None).expect("pixmap");
        let found_blue = (0..80).any(|y| {
            (0..80).any(|x| {
                let pixel = output.pixel(x, y).expect("pixel");
                pixel.blue() > 200 && pixel.green() > 120 && pixel.red() < 80
            })
        });
        assert!(
            found_blue,
            "framework hydration attributes must not invalidate inline SVG XML"
        );
    }

    #[test]
    fn paints_inline_svg_use_reference() {
        // The icon-sprite pattern: <use href="#id"> resolves against a <defs>
        // element in the same svg only because the whole subtree is serialized
        // and handed to resvg as one document.
        let tree = parse_html(
            r##"<html><body><svg width="40" height="40" viewBox="0 0 40 40"><defs><rect id="a" width="40" height="40" fill="#0000ff"/></defs><use href="#a"/></svg></body></html>"##,
        );
        let pixmap = paint_dom(&tree, (200.0, 200.0), None).expect("pixmap");
        let mut found_blue = false;
        'outer: for y in 0..80 {
            for x in 0..80 {
                let p = pixmap.pixel(x, y).expect("pixel");
                if p.blue() > 200 && p.red() < 60 && p.green() < 60 {
                    found_blue = true;
                    break 'outer;
                }
            }
        }
        assert!(
            found_blue,
            "expected <use> to instantiate the referenced <rect>"
        );
    }

    #[test]
    fn extracts_symbol_by_id_from_sprite() {
        // The external-sprite core: given a fetched sprite, pull out just the
        // referenced <symbol> verbatim so it can be spliced into the local svg.
        let sprite = r##"<svg xmlns="http://www.w3.org/2000/svg"><defs><symbol id="a" viewBox="0 0 10 10"><path d="M0 0h10v10z"/></symbol><symbol id="b"><rect width="4" height="4"/></symbol></defs></svg>"##;
        let out = extract_svg_element_by_id(sprite, "a").expect("symbol a found");
        assert!(
            out.starts_with("<symbol"),
            "starts at the symbol tag: {out}"
        );
        assert!(out.contains(r#"id="a""#), "keeps the id: {out}");
        assert!(
            out.contains("<path") && out.contains("h10v10z"),
            "keeps children: {out}"
        );
        assert!(
            out.trim_end().ends_with("</symbol>"),
            "closed at matching end: {out}"
        );
        assert!(
            !out.contains(r#"id="b""#),
            "stops before the sibling symbol: {out}"
        );
        assert!(!out.contains("<rect"), "no sibling content leaks in: {out}");
    }

    #[test]
    fn extract_handles_self_closing_nesting_and_absent() {
        // A self-closing element carrying the id returns just that tag.
        let s1 = r#"<svg><rect id="x" width="4" height="4"/></svg>"#;
        assert_eq!(
            extract_svg_element_by_id(s1, "x").as_deref(),
            Some(r#"<rect id="x" width="4" height="4"/>"#),
        );
        // Same-name nesting: the matching close is the outer one, not the inner.
        let s2 = r#"<svg><g id="grp"><g><path/></g></g></svg>"#;
        assert_eq!(
            extract_svg_element_by_id(s2, "grp").as_deref(),
            Some(r#"<g id="grp"><g><path/></g></g>"#),
        );
        // `data-id` / a missing id must not be mistaken for `id`.
        let s3 = r#"<svg><symbol data-id="a"><path/></symbol></svg>"#;
        assert!(
            extract_svg_element_by_id(s3, "a").is_none(),
            "data-id is not id"
        );
        assert!(extract_svg_element_by_id(s2, "nope").is_none(), "absent id");
    }

    #[test]
    fn same_document_use_left_unchanged_by_inject() {
        // A same-document symbol already inside the target SVG needs no
        // injection and leaves the serialized markup byte-for-byte unchanged.
        let tree = parse_html(
            r##"<html><body><svg viewBox="0 0 10 10"><use href="#a"/><symbol id="a"><path d="M0 0h10v10z"/></symbol></svg></body></html>"##,
        );
        let svg = tree.query_selector("svg").unwrap().unwrap();
        let mut markup = serialize_svg(&tree, svg);
        let before = markup.clone();
        let mut cache = RenderResourceCache::default();
        let mut sprite_cache = std::collections::HashMap::new();
        inject_external_sprites(
            &tree,
            svg,
            None,
            None,
            None,
            &mut markup,
            &mut cache,
            &mut sprite_cache,
        );
        assert_eq!(markup, before, "same-document use must be untouched");
    }

    #[test]
    fn injects_document_level_symbol_into_target_svg() {
        // Frameworks commonly keep one hidden sprite beside the application
        // root and reference it from otherwise independent inline SVGs.
        let tree = parse_html(
            r##"<html><body>
                <svg style="display:none"><symbol id="arrow" viewBox="0 0 10 10"><path d="M0 0h10v10z"/></symbol></svg>
                <svg id="icon" viewBox="0 0 10 10"><use href="#arrow"/></svg>
            </body></html>"##,
        );
        let svg = tree.query_selector("#icon").unwrap().unwrap();
        let mut markup = serialize_svg(&tree, svg);
        let mut cache = RenderResourceCache::default();
        let mut sprite_cache = std::collections::HashMap::new();
        inject_external_sprites(
            &tree,
            svg,
            None,
            None,
            None,
            &mut markup,
            &mut cache,
            &mut sprite_cache,
        );
        assert!(
            markup.contains(r#"<defs><symbol id="arrow""#),
            "document-level symbol must be copied into target SVG: {markup}"
        );
        assert!(
            markup.contains(r##"<use href="#arrow""##),
            "local use reference must remain intact: {markup}"
        );
    }

    #[test]
    fn injected_document_symbol_carries_resolved_presentation_style() {
        let tree = parse_html(
            r##"<html><head><style>
                body { --icon-fill: #20c997; }
                #sprite rect { stroke: var(--icon-stroke, #114433); }
            </style></head><body>
                <svg id="sprite" style="display:none"><symbol id="badge" viewBox="0 0 10 10"><rect width="10" height="10" fill="var(--icon-fill, #ff0000)"/></symbol></svg>
                <svg id="icon" width="10" height="10" viewBox="0 0 10 10"><use href="#badge"/></svg>
            </body></html>"##,
        );
        let laid = crate::dom::layout_dom(&tree, (100.0, 100.0));
        let svg = tree.query_selector("#icon").unwrap().unwrap();
        let mut markup = serialize_svg_styled(
            &tree,
            svg,
            &laid.styles,
            &laid.custom_properties,
            None,
        );
        let mut cache = RenderResourceCache::default();
        let mut sprite_cache = std::collections::HashMap::new();
        inject_external_sprites(
            &tree,
            svg,
            Some(&laid.styles),
            Some(&laid.custom_properties),
            None,
            &mut markup,
            &mut cache,
            &mut sprite_cache,
        );
        assert!(
            !markup.contains("var("),
            "injected local symbol must not lose computed custom properties: {markup}"
        );
        assert!(
            markup.contains("#20c997") && markup.contains("#114433"),
            "injected local symbol must carry attribute and author-CSS colors: {markup}"
        );
        let pixmap = render_svg(markup.as_bytes(), 10, 10).expect("injected svg renders");
        let center = pixmap.pixel(5, 5).expect("center pixel");
        assert!(
            center.green() > 150 && center.red() < 80 && center.blue() > 90,
            "resolved injected-symbol fill must paint instead of usvg black: {center:?}"
        );
    }

    #[test]
    fn injected_external_symbol_resolves_host_custom_properties_and_fallbacks() {
        let tree = parse_html(
            r##"<html><body><svg id="icon" style="--icon-fill:#e83e8c" width="10" height="10" viewBox="0 0 10 10"><use href="icons.svg#badge"/></svg></body></html>"##,
        );
        let laid = crate::dom::layout_dom(&tree, (100.0, 100.0));
        let svg = tree.query_selector("#icon").unwrap().unwrap();
        let mut markup = serialize_svg_styled(
            &tree,
            svg,
            &laid.styles,
            &laid.custom_properties,
            None,
        );
        let mut cache = RenderResourceCache::default();
        let mut sprite_cache = std::collections::HashMap::from([(
            "icons.svg#badge".to_string(),
            Some(
                r##"<symbol id="badge" viewBox="0 0 10 10"><rect width="10" height="10" fill="var(--icon-fill, #ff0000)" stroke="var(--missing, #224466)"/><path d="var(--xml-only)"/></symbol>"##
                    .to_string(),
            ),
        )]);
        inject_external_sprites(
            &tree,
            svg,
            Some(&laid.styles),
            Some(&laid.custom_properties),
            None,
            &mut markup,
            &mut cache,
            &mut sprite_cache,
        );
        assert!(
            markup.contains(r##"href="#badge""##),
            "external use reference must be localized: {markup}"
        );
        assert!(
            markup.contains(r##"fill="#e83e8c""##)
                && markup.contains(r##"stroke="#224466""##),
            "external symbol presentation values must use host properties and fallbacks: {markup}"
        );
        assert!(
            markup.contains(r##"d="var(--xml-only)""##),
            "XML-only attributes must remain literal: {markup}"
        );
    }

    #[test]
    fn injected_document_symbol_inherits_target_current_color() {
        let tree = parse_html(
            r##"<html><body>
                <svg style="display:none"><symbol id="arrow" viewBox="0 0 10 10"><rect width="10" height="10" fill="currentColor"/></symbol></svg>
                <svg id="icon" viewBox="0 0 10 10"><use href="#arrow"/></svg>
            </body></html>"##,
        );
        let svg = tree.query_selector("#icon").unwrap().unwrap();
        let mut markup = serialize_svg(&tree, svg);
        let mut cache = RenderResourceCache::default();
        let mut sprite_cache = std::collections::HashMap::new();
        inject_external_sprites(
            &tree,
            svg,
            None,
            None,
            None,
            &mut markup,
            &mut cache,
            &mut sprite_cache,
        );
        inject_svg_current_color(&mut markup, [220, 20, 60, 255]);
        let pixmap = render_svg(markup.as_bytes(), 20, 20).expect("injected svg renders");
        assert!(
            pixmap
                .pixels()
                .iter()
                .any(|pixel| pixel.red() > 180 && pixel.green() < 60 && pixel.blue() < 100),
            "injected currentColor symbol should inherit target SVG color: {markup}",
        );
    }

    #[test]
    fn svg_light_dark_presentation_is_resolved_before_usvg() {
        let tree = parse_html(
            r#"<style>
               #dark { color-scheme:dark }
               #dark rect {
                 fill:light-dark(#c3c7cb,#51565d);
                 stroke:light-dark(#ffffff,#000000);
               }
               </style>
               <div id="dark">
                 <svg id="icon" width="10" height="10" viewBox="0 0 10 10">
                   <rect width="10" height="10"/>
                 </svg>
               </div>"#,
        );
        let laid = crate::dom::layout_dom(&tree, (100.0, 100.0));
        let svg = tree.query_selector("#icon").unwrap().unwrap();
        let markup = serialize_svg_styled(
            &tree,
            svg,
            &laid.styles,
            &laid.custom_properties,
            None,
        );
        assert!(
            !markup.to_ascii_lowercase().contains("light-dark("),
            "unsupported CSS Color 5 syntax must not reach usvg: {markup}"
        );
        assert!(
            markup.contains("fill:#51565dff!important")
                && markup.contains("stroke:#000000ff!important"),
            "serialized presentation colors must use the dark subtree scheme: {markup}"
        );
        let pixmap = render_svg(markup.as_bytes(), 10, 10).expect("resolved SVG renders");
        let center = pixmap.pixel(5, 5).expect("center pixel");
        assert!(
            center.red() > 60
                && center.red() < 110
                && center.green() > 60
                && center.green() < 120
                && center.blue() > 70
                && center.blue() < 130,
            "resolved dark fill must survive usvg rasterization: {center:?}"
        );
    }

    #[test]
    fn font_face_parser_selects_ascii_subset_and_preserves_functional_src() {
        let css = r#"
            @font-face {
                font-family: "Example";
                src: local("Example"), url("./example-cyrillic.woff2") format("woff2");
                unicode-range: U+0400-04FF;
            }
            @font-face {
                font-family: "Example";
                font-style: italic;
                font-weight: 350 650;
                src: url(data:font/woff2;base64,d09GMg==) format("woff2"),
                     url("./example-latin.woff") format("woff");
                unicode-range: U+??, U+2000-206F;
            }
        "#;
        let faces = font_face_blocks(css);
        assert_eq!(faces.len(), 2);
        assert!(!font_face_covers_ascii(faces[0]));
        assert!(font_face_covers_ascii(faces[1]));
        assert_eq!(font_face_family(faces[1]).as_deref(), Some("Example"));
        assert_eq!(font_face_weight(faces[1]), Some((350, 650)));
        assert_eq!(font_face_italic(faces[1]), Some(true));
        assert_eq!(
            font_face_urls(faces[1]),
            vec![
                "data:font/woff2;base64,d09GMg==".to_string(),
                "./example-latin.woff".to_string(),
            ]
        );
    }

    #[test]
    fn font_face_without_unicode_range_is_general_purpose() {
        let css = r#"@font-face{font-family:Example;src:url(example.otf)}"#;
        let face = font_face_blocks(css)[0];
        assert!(font_face_covers_ascii(face));
        assert_eq!(font_face_urls(face), vec!["example.otf"]);
    }

    #[test]
    fn font_face_uses_the_first_decodable_source() {
        let tree = parse_html(
            r#"<html><head><style>
                @font-face {
                    font-family: Fixture;
                    src: url("fixture.eot") format("embedded-opentype"),
                         url("fixture.woff2") format("woff2"),
                         url("fixture.ttf") format("truetype");
                }
            </style></head><body></body></html>"#,
        );
        let loads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let loader_loads = Arc::clone(&loads);
        let mut resources = RenderResourceCache::with_loader(move |url: &str| {
            loader_loads
                .lock()
                .expect("font loads")
                .push(url.to_string());
            match url {
                // A server can return malformed bytes for a preferred source;
                // CSS Fonts requires trying the next candidate in the list.
                "https://example.test/fixture.woff2" => Some(b"not a font".to_vec()),
                "https://example.test/fixture.ttf" => Some(SERIF_FONT_BYTES.to_vec()),
                _ => None,
            }
        });

        let fonts = collect_web_fonts(
            &tree,
            Some("https://example.test/page.html"),
            &mut resources,
            &[],
        );

        assert_eq!(fonts.len(), 1);
        assert_eq!(fonts[0].family.as_deref(), Some("Fixture"));
        assert_eq!(fonts[0].data, SERIF_FONT_BYTES);
        assert_eq!(
            *loads.lock().expect("font loads"),
            vec![
                "https://example.test/fixture.woff2".to_string(),
                "https://example.test/fixture.ttf".to_string(),
            ],
            "unsupported EOT must be skipped without a request"
        );
    }

    #[test]
    fn font_face_uses_the_last_duplicate_src_descriptor() {
        let css = r#"@font-face {
            font-family: FontAwesome;
            src: url("legacy.eot");
            src: url("legacy.eot?#iefix") format("embedded-opentype"),
                 url("icons.woff2") format("woff2"),
                 url("icons.ttf") format("truetype");
        }"#;
        let face = font_face_blocks(css)[0];

        assert_eq!(
            font_face_urls(face),
            vec![
                "legacy.eot?#iefix".to_string(),
                "icons.woff2".to_string(),
                "icons.ttf".to_string(),
            ],
            "CSS keeps the final duplicate @font-face descriptor"
        );
    }

    #[test]
    fn dynamic_font_face_uses_shared_fetch_decode_and_layout_path() {
        let tree = parse_html(
            r#"<html><body><span id="sample" style="display:inline-block;width:max-content;
                font:40px DynamicFixture;white-space:nowrap">WWWWiiii</span></body></html>"#,
        );
        let sample = tree
            .query_selector("#sample")
            .expect("valid selector")
            .expect("sample");
        let fallback = prepare_dom(
            &tree,
            (400.0, 100.0),
            Some("https://example.test/page/index.html"),
            &mut RenderResourceCache::default(),
        )
        .expect("fallback render")
        .document_rect(sample)
        .expect("fallback geometry")
        .width;
        let loads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let loader_loads = Arc::clone(&loads);
        let mut resources = RenderResourceCache::with_loader(move |url: &str| {
            loader_loads
                .lock()
                .expect("font loads")
                .push(url.to_string());
            (url == "https://example.test/fonts/fixture.ttf").then(|| SERIF_FONT_BYTES.to_vec())
        });
        let prepared = prepare_dom_with_dynamic_fonts(
            &tree,
            (400.0, 100.0),
            Some("https://example.test/page/index.html"),
            &mut resources,
            &[DynamicFontFace {
                family: "DynamicFixture".to_string(),
                source: "url('../../fonts/fixture.ttf') format('truetype')".to_string(),
                style: "normal".to_string(),
                weight: "400".to_string(),
                unicode_range: "U+20-7E".to_string(),
            }],
        )
        .expect("dynamic font render");
        let dynamic = prepared
            .document_rect(sample)
            .expect("dynamic geometry")
            .width;
        assert_ne!(
            fallback, dynamic,
            "dynamic face must participate in shaping"
        );
        assert_eq!(
            *loads.lock().expect("dynamic font loads"),
            vec!["https://example.test/fonts/fixture.ttf".to_string()]
        );
    }

    #[test]
    fn resolved_nested_scroll_keeps_owner_and_clip_stationary_while_pixels_move() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="outer" style="box-sizing:border-box;width:120px;height:100px;
                     border:4px solid red;overflow:hidden;position:relative;background:red">
                  <div id="inner" style="width:220px;height:200px;overflow:hidden;
                       position:relative;background:blue">
                    <div id="target" style="position:absolute;left:300px;top:280px;
                         width:30px;height:20px;background:lime"></div>
                  </div>
                </div>
            </body></html>"#,
        );
        let outer = tree.get_element_by_id("outer").expect("outer");
        let inner = tree.get_element_by_id("inner").expect("inner");
        let target = tree.get_element_by_id("target").expect("target");
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared =
            prepare_dom(&tree, (360.0, 240.0), None, &mut resources).expect("prepared render");
        let top_state = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &HashMap::new());
        let top_outer = prepared
            .viewport_rect_with_scroll(outer, &top_state)
            .expect("top outer");
        let top_target = prepared
            .viewport_rect_with_scroll(target, &top_state)
            .expect("top target");
        let top = paint_prepared_with_scroll(&tree, &mut prepared, &mut resources, &top_state)
            .expect("top paint");

        let offsets = HashMap::from([(outer, (9999.0, 9999.0)), (inner, (9999.0, 9999.0))]);
        let scrolled_state = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &offsets);
        let outer_metrics = prepared
            .element_scroll_metrics(outer, &scrolled_state)
            .expect("outer metrics");
        let inner_metrics = prepared
            .element_scroll_metrics(inner, &scrolled_state)
            .expect("inner metrics");
        assert_eq!(outer_metrics.client_size, (112.0, 92.0));
        assert_eq!(outer_metrics.content_size, (220.0, 200.0));
        assert_eq!(outer_metrics.offset, (108.0, 108.0));
        assert_eq!(inner_metrics.content_size, (330.0, 300.0));
        assert_eq!(inner_metrics.offset, (110.0, 100.0));

        let scrolled_outer = prepared
            .viewport_rect_with_scroll(outer, &scrolled_state)
            .expect("scrolled outer");
        let scrolled_target = prepared
            .viewport_rect_with_scroll(target, &scrolled_state)
            .expect("scrolled target");
        assert_eq!(
            scrolled_outer, top_outer,
            "scroller chrome must not move with its content"
        );
        assert_eq!(scrolled_target.x, top_target.x - 218.0);
        assert_eq!(scrolled_target.y, top_target.y - 208.0);

        let scrolled =
            paint_prepared_with_scroll(&tree, &mut prepared, &mut resources, &scrolled_state)
                .expect("scrolled paint");
        let repeated =
            paint_prepared_with_scroll(&tree, &mut prepared, &mut resources, &scrolled_state)
                .expect("repeated paint");
        assert_ne!(top.data(), scrolled.data());
        assert_eq!(
            scrolled.data(),
            repeated.data(),
            "scroll deltas must not accumulate"
        );
        let pixel =
            |pixmap: &Pixmap, x: u32, y: u32| pixmap.pixels()[(y * pixmap.width() + x) as usize];
        assert_eq!(
            pixel(&top, 2, 2),
            pixel(&scrolled, 2, 2),
            "outer border moved"
        );
        assert_eq!(
            pixel(&top, 125, 50),
            pixel(&scrolled, 125, 50),
            "content escaped the stationary outer clip"
        );
        assert_ne!(
            pixel(&top, 95, 85),
            pixel(&scrolled, 95, 85),
            "newly visible nested content did not move into the scrollport"
        );
    }

    #[test]
    fn nested_scroll_sticky_geometry_pixels_and_percentage_basis_share_one_state() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="plain" style="position:relative;width:100px;height:80px;
                     overflow:hidden;background:red">
                    <div style="height:40px"></div>
                    <div id="plain-sticky" style="position:sticky;top:5px;
                         width:30px;height:20px;background:lime">
                        <div id="fixed-child" style="position:fixed;right:0;top:0;
                             width:5px;height:5px;background:black"></div>
                    </div>
                    <div id="plain-tail" style="height:160px;background:blue"></div>
                </div>
                <div id="padded" style="position:absolute;left:120px;top:0;
                     width:100px;height:80px;padding-top:20px;overflow:hidden;background:red">
                    <div style="height:70px"></div>
                    <div id="percent-sticky" style="position:sticky;top:50%;
                         width:30px;height:20px;background:lime"></div>
                    <div style="height:160px;background:blue"></div>
                </div>
                <div id="fixed-scroller" style="position:fixed;left:240px;top:0;
                     width:100px;height:80px;overflow:hidden;background:red">
                    <div style="height:40px"></div>
                    <div id="fixed-sticky" style="position:sticky;top:5px;
                         width:30px;height:20px;background:lime"></div>
                    <div style="height:160px;background:blue"></div>
                </div>
                <div id="calc-scroller" style="position:absolute;left:360px;top:0;
                     width:100px;height:80px;padding-top:20px;overflow:hidden;background:red">
                    <div style="height:70px"></div>
                    <div id="calc-sticky" style="position:sticky;top:calc(50% + 1px);
                         width:30px;height:20px;background:lime"></div>
                    <div style="height:160px;background:blue"></div>
                </div>
            </body></html>"#,
        );
        let plain = tree.get_element_by_id("plain").unwrap();
        let plain_sticky = tree.get_element_by_id("plain-sticky").unwrap();
        let plain_tail = tree.get_element_by_id("plain-tail").unwrap();
        let padded = tree.get_element_by_id("padded").unwrap();
        let percent_sticky = tree.get_element_by_id("percent-sticky").unwrap();
        let fixed_child = tree.get_element_by_id("fixed-child").unwrap();
        let fixed_scroller = tree.get_element_by_id("fixed-scroller").unwrap();
        let fixed_sticky = tree.get_element_by_id("fixed-sticky").unwrap();
        let calc_scroller = tree.get_element_by_id("calc-scroller").unwrap();
        let calc_sticky = tree.get_element_by_id("calc-sticky").unwrap();
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared =
            prepare_dom(&tree, (480.0, 120.0), None, &mut resources).expect("prepared");

        let top = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &HashMap::new());
        assert_eq!(
            prepared.viewport_rect_with_scroll(plain_sticky, &top).unwrap().y,
            40.0,
        );
        let offsets = HashMap::from([
            (plain, (0.0, 60.0)),
            (padded, (0.0, 80.0)),
            (fixed_scroller, (0.0, 60.0)),
            (calc_scroller, (0.0, 80.0)),
        ]);
        let scrolled = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &offsets);
        assert_eq!(
            prepared.element_scroll_metrics(plain, &scrolled).unwrap().offset,
            (0.0, 60.0),
        );
        assert_eq!(
            prepared.viewport_rect_with_scroll(plain, &scrolled).unwrap().y,
            0.0,
            "scroll-container chrome must remain stationary",
        );
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(plain_sticky, &scrolled)
                .unwrap()
                .y,
            5.0,
        );
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(plain_tail, &scrolled)
                .unwrap()
                .y,
            0.0,
            "ordinary content must retain the element scroll movement",
        );
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(percent_sticky, &scrolled)
                .unwrap()
                .y,
            60.0,
            "50% sticky inset must use the 80px content box after 20px padding",
        );
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(fixed_sticky, &scrolled)
                .unwrap()
                .y,
            5.0,
            "an element scroller inside a fixed subtree must still drive sticky positioning",
        );
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(fixed_child, &scrolled)
                .unwrap()
                .y,
            0.0,
            "a viewport-fixed descendant must not inherit its sticky ancestor's movement",
        );
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(calc_sticky, &scrolled)
                .unwrap()
                .y,
            61.0,
            "calc() percentage inset must use the nested content-box basis",
        );

        let pixels = paint_prepared_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scrolled,
        )
        .expect("paint");
        let plain_pixel = pixels.pixel(10, 10).unwrap();
        let percent_pixel = pixels.pixel(130, 65).unwrap();
        let fixed_pixel = pixels.pixel(250, 10).unwrap();
        let calc_pixel = pixels.pixel(370, 65).unwrap();
        assert!(plain_pixel.green() > 240 && plain_pixel.red() < 20);
        assert!(percent_pixel.green() > 240 && percent_pixel.red() < 20);
        assert!(fixed_pixel.green() > 240 && fixed_pixel.red() < 20);
        assert!(calc_pixel.green() > 240 && calc_pixel.red() < 20);
    }

    #[test]
    fn viewport_fixed_descendant_escapes_nested_scrollport_clip_in_all_paint_paths() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="scroller" style="position:relative;width:100px;height:80px;
                     overflow:hidden;background:red">
                    <div style="height:40px"></div>
                    <div style="position:sticky;top:5px;width:30px;height:20px;background:blue">
                        <div id="fixed" style="position:fixed;z-index:10;left:150px;top:10px;
                             width:20px;height:20px;background:lime"></div>
                    </div>
                    <div style="height:160px"></div>
                </div>
                <div id="captured-host" style="position:absolute;left:0;top:50px;
                     width:100px;height:30px;overflow:hidden;transform:translateX(0);background:red">
                    <div id="captured-fixed" style="position:fixed;left:150px;top:0;
                         width:20px;height:20px;background:blue"></div>
                </div>
                <div id="fixed-clip" style="position:fixed;left:100px;top:75px;
                     width:40px;height:25px;overflow:hidden;background:red">
                    <div style="position:absolute;left:50px;top:0;
                         width:20px;height:20px;background:blue"></div>
                </div>
            </body></html>"#,
        );
        let scroller = tree.get_element_by_id("scroller").unwrap();
        let fixed = tree.get_element_by_id("fixed").unwrap();
        let captured_fixed = tree.get_element_by_id("captured-fixed").unwrap();
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared =
            prepare_dom(&tree, (220.0, 100.0), None, &mut resources).expect("prepared");
        assert!(prepared.viewport_fixed_nodes().contains(&fixed));
        assert!(!prepared.viewport_fixed_nodes().contains(&captured_fixed));

        let tuple = paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 0.0))
            .expect("tuple paint");
        let tuple_pixel = tuple.pixel(155, 15).unwrap();
        assert!(
            tuple_pixel.green() > 240 && tuple_pixel.red() < 20 && tuple_pixel.blue() < 20,
            "the default paint path retained the ancestor scroller clip: {tuple_pixel:?}",
        );
        let captured_pixel = tuple.pixel(155, 55).unwrap();
        assert!(
            captured_pixel.red() > 240
                && captured_pixel.green() > 240
                && captured_pixel.blue() > 240,
            "a containing-block-captured fixed descendant must remain clipped: {captured_pixel:?}",
        );
        let internal_clip_pixel = tuple.pixel(155, 80).unwrap();
        assert!(
            internal_clip_pixel.red() > 240
                && internal_clip_pixel.green() > 240
                && internal_clip_pixel.blue() > 240,
            "clips created inside a viewport-fixed subtree must still apply: {internal_clip_pixel:?}",
        );

        let offsets = HashMap::from([(scroller, (0.0, 60.0))]);
        let scroll = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &offsets);
        assert!(
            scroll.inherited_clip_for(fixed).is_none(),
            "a viewport-fixed boundary must clear document-space overflow clips",
        );
        let resolved =
            paint_prepared_with_scroll(&tree, &mut prepared, &mut resources, &scroll)
                .expect("resolved paint");
        let resolved_pixel = resolved.pixel(155, 15).unwrap();
        assert!(
            resolved_pixel.green() > 240
                && resolved_pixel.red() < 20
                && resolved_pixel.blue() < 20,
            "the resolved paint path retained the ancestor scroller clip: {resolved_pixel:?}",
        );
        let captured_pixel = resolved.pixel(155, 55).unwrap();
        assert!(captured_pixel.red() > 240 && captured_pixel.green() > 240 && captured_pixel.blue() > 240);
        let internal_clip_pixel = resolved.pixel(155, 80).unwrap();
        assert!(
            internal_clip_pixel.red() > 240
                && internal_clip_pixel.green() > 240
                && internal_clip_pixel.blue() > 240
        );
    }

    #[test]
    fn virtual_viewport_re_resolves_functional_and_viewport_sticky_insets() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:600px">
                <div style="height:150px"></div>
                <div id="calc" style="position:sticky;top:calc(50% + 1px);
                     width:20px;height:20px"></div>
                <div id="vh" style="position:sticky;top:10vh;width:20px;height:20px"></div>
            </body></html>"#,
        );
        let calc = tree.get_element_by_id("calc").unwrap();
        let vh = tree.get_element_by_id("vh").unwrap();
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let prepared =
            prepare_dom(&tree, (200.0, 200.0), None, &mut resources).expect("prepared");
        let live = prepared.resolve_scroll_state_for_viewport(
            &tree,
            (0.0, 200.0),
            &HashMap::new(),
            (200.0, 200.0),
        );
        let page = prepared.resolve_scroll_state_for_viewport(
            &tree,
            (0.0, 200.0),
            &HashMap::new(),
            (200.0, 100.0),
        );

        assert_eq!(prepared.viewport_rect_with_scroll(calc, &live).unwrap().y, 101.0);
        assert_eq!(prepared.viewport_rect_with_scroll(calc, &page).unwrap().y, 51.0);
        assert_eq!(prepared.viewport_rect_with_scroll(vh, &live).unwrap().y, 20.0);
        assert_eq!(prepared.viewport_rect_with_scroll(vh, &page).unwrap().y, 10.0);
    }

    #[test]
    fn hidden_scroller_captures_sticky_while_overflow_clip_does_not() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:400px">
                <div id="hidden" style="position:absolute;left:0;top:50px;
                     width:100px;height:80px;overflow:hidden">
                    <div id="hidden-sticky" style="position:sticky;top:0;
                         width:20px;height:20px;background:lime"></div>
                </div>
                <div id="clip" style="position:absolute;left:120px;top:50px;
                     width:100px;height:80px;overflow:clip">
                    <div id="clip-sticky" style="position:sticky;top:0;
                         width:20px;height:20px;background:lime"></div>
                </div>
            </body></html>"#,
        );
        let hidden = tree.get_element_by_id("hidden").unwrap();
        let hidden_sticky = tree.get_element_by_id("hidden-sticky").unwrap();
        let clip = tree.get_element_by_id("clip").unwrap();
        let clip_sticky = tree.get_element_by_id("clip-sticky").unwrap();
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let prepared =
            prepare_dom(&tree, (240.0, 120.0), None, &mut resources).expect("prepared");
        assert!(prepared.scroll_container_nodes().any(|node| node == hidden));
        assert!(!prepared.scroll_container_nodes().any(|node| node == clip));

        let scrolled = prepared.resolve_scroll_state(&tree, (0.0, 100.0), &HashMap::new());
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(hidden_sticky, &scrolled)
                .unwrap()
                .y,
            -50.0,
            "overflow:hidden must capture sticky even without local overflow",
        );
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(clip_sticky, &scrolled)
                .unwrap()
                .y,
            0.0,
            "overflow:clip must leave sticky owned by the root viewport",
        );
    }

    #[test]
    fn outer_sticky_motion_cancels_from_inner_scrollport_constraints() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:400px">
                <div style="height:40px"></div>
                <div id="outer-sticky" style="position:sticky;top:0;
                     width:100px;height:80px;background:red">
                    <div id="inner-scroller" style="width:100px;height:60px;
                         overflow:hidden;background:blue">
                        <div style="height:30px"></div>
                        <div id="inner-sticky" style="position:sticky;top:5px;
                             width:30px;height:10px;background:lime"></div>
                        <div style="height:100px"></div>
                    </div>
                </div>
            </body></html>"#,
        );
        let outer_sticky = tree.get_element_by_id("outer-sticky").unwrap();
        let inner_scroller = tree.get_element_by_id("inner-scroller").unwrap();
        let inner_sticky = tree.get_element_by_id("inner-sticky").unwrap();
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared =
            prepare_dom(&tree, (120.0, 100.0), None, &mut resources).expect("prepared");
        let offsets = HashMap::from([(inner_scroller, (0.0, 40.0))]);
        let scrolled = prepared.resolve_scroll_state(&tree, (0.0, 60.0), &offsets);

        assert_eq!(
            prepared
                .viewport_rect_with_scroll(outer_sticky, &scrolled)
                .unwrap()
                .y,
            0.0,
        );
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(inner_scroller, &scrolled)
                .unwrap()
                .y,
            0.0,
        );
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(inner_sticky, &scrolled)
                .unwrap()
                .y,
            5.0,
            "outer sticky movement must affect the inner port and its content equally",
        );

        let pixels = paint_prepared_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scrolled,
        )
        .expect("paint");
        let pixel = pixels.pixel(10, 7).unwrap();
        assert!(pixel.green() > 240 && pixel.red() < 20 && pixel.blue() < 20);
    }

    #[test]
    fn tuple_capture_and_geometry_resolve_nested_sticky_at_zero_element_scroll() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="scroller" style="width:100px;height:80px;overflow:hidden;background:red">
                    <div style="height:100px"></div>
                    <div id="sticky" style="position:sticky;bottom:0;
                         width:30px;height:20px;background:lime"></div>
                    <div style="height:20px"></div>
                </div>
                <div style="position:fixed;left:120px;top:0;width:100px;height:80px;
                     overflow:hidden;background:red">
                    <div style="height:100px"></div>
                    <div id="fixed-sticky" style="position:sticky;bottom:0;
                         width:30px;height:20px;background:lime"></div>
                    <div style="height:20px"></div>
                </div>
            </body></html>"#,
        );
        let sticky = tree.get_element_by_id("sticky").unwrap();
        let fixed_sticky = tree.get_element_by_id("fixed-sticky").unwrap();
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared =
            prepare_dom(&tree, (240.0, 100.0), None, &mut resources).expect("prepared");
        let resolved = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &HashMap::new());
        assert_eq!(prepared.viewport_rect(sticky, (0.0, 0.0)).unwrap().y, 60.0);
        assert_eq!(
            prepared.viewport_rect(fixed_sticky, (0.0, 0.0)).unwrap().y,
            60.0,
        );
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(sticky, &resolved)
                .unwrap()
                .y,
            60.0,
        );

        let pixels = paint_prepared(&tree, &mut prepared, &mut resources, (0.0, 0.0))
            .expect("tuple paint");
        let pixel = pixels.pixel(10, 65).unwrap();
        let fixed_pixel = pixels.pixel(130, 65).unwrap();
        assert!(pixel.green() > 240 && pixel.red() < 20 && pixel.blue() < 20);
        assert!(
            fixed_pixel.green() > 240 && fixed_pixel.red() < 20 && fixed_pixel.blue() < 20,
            "tuple paint dropped nested sticky movement in a fixed subtree: {fixed_pixel:?}",
        );
    }

    #[test]
    fn document_region_capture_reuses_layout_scroll_and_resources() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:300px;background:white">
                <div style="height:80px;background:red"></div>
                <div id="sticky" style="position:sticky;z-index:1;top:0;margin-left:20px;
                     width:10px;height:10px;background:yellow"></div>
                <div style="height:210px;background:green"></div>
                <div id="offscreen" style="position:absolute;left:40px;top:150px;
                     width:10px;height:10px;background:magenta"></div>
                <img src="fixture.svg" style="position:absolute;left:60px;top:200px;
                     width:10px;height:10px">
                <div id="fixed" style="position:fixed;left:0;top:0;width:10px;
                     height:10px;background:blue"></div>
            </body></html>"#,
        );
        let loads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_loads = loads.clone();
        let mut resources = RenderResourceCache::with_loader(move |url: &str| {
            assert_eq!(url, "https://example.test/fixture.svg");
            loader_loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(
                br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
                    <rect width="10" height="10" fill="#00ffff"/>
                </svg>"##
                    .to_vec(),
            )
        });
        let mut prepared = prepare_dom(
            &tree,
            (80.0, 60.0),
            Some("https://example.test/page"),
            &mut resources,
        )
        .expect("prepared render");
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1);
        let scroll = prepared.resolve_scroll_state(&tree, (0.0, 100.0), &HashMap::new());
        let fixed = tree.get_element_by_id("fixed").expect("fixed");
        let before_fixed = prepared
            .viewport_rect_with_scroll(fixed, &scroll)
            .expect("fixed geometry");

        let offscreen = paint_prepared_region_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scroll,
            CaptureRegion::new(0.0, 100.0, 80.0, 80.0, 1.0),
        )
        .expect("offscreen region");
        assert_eq!((offscreen.width(), offscreen.height()), (80, 80));
        let fixed_pixel = offscreen.pixel(5, 5).expect("fixed pixel");
        assert!(fixed_pixel.blue() > 240 && fixed_pixel.red() < 20);
        let sticky_pixel = offscreen.pixel(25, 5).expect("sticky pixel");
        assert!(sticky_pixel.red() > 240 && sticky_pixel.green() > 240);
        let target_pixel = offscreen.pixel(45, 55).expect("offscreen target");
        assert!(target_pixel.red() > 240 && target_pixel.blue() > 240);

        let full_height = prepared.content_size().1;
        let full = paint_prepared_region_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scroll,
            CaptureRegion::new(0.0, 0.0, 80.0, full_height, 1.0),
        )
        .expect("full-content region");
        assert_eq!(full.height(), full_height.ceil() as u32);
        let fixed_at_live_viewport = full.pixel(5, 105).expect("full fixed pixel");
        assert!(fixed_at_live_viewport.blue() > 240 && fixed_at_live_viewport.red() < 20);
        let fixed_not_duplicated = full.pixel(5, 5).expect("document top pixel");
        assert!(fixed_not_duplicated.red() > 240 && fixed_not_duplicated.blue() < 20);

        let scaled = paint_prepared_region_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scroll,
            CaptureRegion::new(40.0, 150.0, 10.0, 10.0, 2.0),
        )
        .expect("scaled region");
        assert_eq!((scaled.width(), scaled.height()), (20, 20));
        let scaled_center = scaled.pixel(10, 10).expect("scaled center");
        assert!(scaled_center.red() > 240 && scaled_center.blue() > 240);

        let protocol_sized = paint_prepared_region_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scroll,
            CaptureRegion::with_output_size(40.0, 150.0, 10.0, 9.0, 1.1, 11, 10),
        )
        .expect("protocol-sized region");
        assert_eq!((protocol_sized.width(), protocol_sized.height()), (11, 10));

        assert_eq!(scroll.root_offset(), (0.0, 100.0));
        assert_eq!(
            prepared
                .viewport_rect_with_scroll(fixed, &scroll)
                .expect("unchanged fixed geometry"),
            before_fixed
        );
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn print_background_policy_matches_chromium_economy_and_restores_screen_paint() {
        assert_eq!(
            print_economy_color([255, 255, 255, 255]),
            [171, 171, 171, 255]
        );
        assert_eq!(
            print_economy_color([105, 105, 105, 255]),
            [105, 105, 105, 255]
        );
        assert_eq!(print_economy_color([110, 110, 110, 255]), [25, 25, 25, 255]);
        assert_eq!(print_economy_color([255, 0, 0, 255]), [255, 0, 0, 255]);
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0;background:#123456">
                <div style="box-sizing:border-box;width:100px;height:100px;
                     background:#ff0000;border:10px solid #0000ff;
                     color:#ffffff;font:40px sans-serif">X</div>
                <div style="position:relative;width:120px;height:120px">
                    <img src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='120' height='120'%3E%3Crect width='120' height='120' fill='orange'/%3E%3C/svg%3E"
                         style="position:absolute;inset:0;width:120px;height:120px">
                    <div style="position:absolute;left:20px;top:20px;width:80px;height:80px;
                         background-image:linear-gradient(#00ff00,#00ff00)"></div>
                </div>
            </body></html>"#,
        );
        let mut resources = RenderResourceCache::default();
        let mut prepared = prepare_dom(&tree, (120.0, 220.0), None, &mut resources)
            .expect("prepared print fixture");
        let scroll = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &HashMap::new());
        let region = CaptureRegion::new(0.0, 0.0, 120.0, 220.0, 1.0);

        let screen_before = screenshot_prepared_region_with_scroll_and_backgrounds(
            &tree, &mut prepared, &mut resources, &scroll, region, true,
        )
        .expect("screen capture before print");
        let economy = screenshot_prepared_region_with_scroll_and_backgrounds(
            &tree, &mut prepared, &mut resources, &scroll, region, false,
        )
        .expect("print economy capture");
        let screen_after = screenshot_prepared_region_with_scroll_and_backgrounds(
            &tree, &mut prepared, &mut resources, &scroll, region, true,
        )
        .expect("screen capture after print");
        assert_eq!(screen_before, screen_after, "print paint must not mutate retained style");

        let screen = image::load_from_memory_with_format(&screen_before, image::ImageFormat::Png)
            .unwrap()
            .into_rgb8();
        let economy = image::load_from_memory_with_format(&economy, image::ImageFormat::Png)
            .unwrap()
            .into_rgb8();
        assert_eq!(screen.get_pixel(80, 80).0, [255, 0, 0]);
        assert_eq!(economy.get_pixel(80, 80).0, [255, 255, 255]);
        assert_eq!(economy.get_pixel(5, 50).0, [0, 0, 255]);
        assert_eq!(screen.get_pixel(50, 150).0, [0, 255, 0]);
        assert_eq!(economy.get_pixel(5, 105).0, [255, 165, 0]);
        assert_eq!(economy.get_pixel(50, 150).0, [255, 255, 255]);
        assert!((10..90).any(|y| (10..90).any(|x| {
            let [r, g, b] = economy.get_pixel(x, y).0;
            (140..=200).contains(&r) && r.abs_diff(g) <= 2 && r.abs_diff(b) <= 2
        })));
    }

    #[test]
    fn document_region_capture_rejects_invalid_and_oversized_surfaces_before_paint() {
        let tree = parse_html("<div style='width:10px;height:10px;background:red'></div>");
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared =
            prepare_dom(&tree, (20.0, 20.0), None, &mut resources).expect("prepared render");
        let scroll = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &HashMap::new());

        for invalid in [
            CaptureRegion::new(0.0, 0.0, 0.0, 10.0, 1.0),
            CaptureRegion::new(f32::NAN, 0.0, 10.0, 10.0, 1.0),
            CaptureRegion::new(0.0, 0.0, 10.0, 10.0, f32::INFINITY),
        ] {
            assert_eq!(
                paint_prepared_region_with_scroll(
                    &tree,
                    &mut prepared,
                    &mut resources,
                    &scroll,
                    invalid,
                )
                .unwrap_err(),
                CaptureError::InvalidRegion
            );
        }
        assert_eq!(
            paint_prepared_region_with_scroll(
                &tree,
                &mut prepared,
                &mut resources,
                &scroll,
                CaptureRegion::new(0.0, 0.0, MAX_CAPTURE_DIMENSION as f32, 10.0, 2.0),
            )
            .unwrap_err(),
            CaptureError::AllocationLimitExceeded
        );
        assert_eq!(
            paint_prepared_region_with_scroll(
                &tree,
                &mut prepared,
                &mut resources,
                &scroll,
                CaptureRegion::new(0.0, 0.0, 10.0, 10.0, MAX_CAPTURE_SCALE + 1.0),
            )
            .unwrap_err(),
            CaptureError::AllocationLimitExceeded
        );
        assert_eq!(
            paint_prepared_region_with_scroll(
                &tree,
                &mut prepared,
                &mut resources,
                &scroll,
                // Each surface is below the legacy 64M-pixel per-surface
                // bound, but their simultaneous RGBA peak exceeds 256 MiB.
                CaptureRegion::new(0.0, 0.0, 6000.0, 6000.0, 1.2),
            )
            .unwrap_err(),
            CaptureError::AllocationLimitExceeded
        );
        assert_eq!(
            paint_prepared_region_with_scroll(
                &tree,
                &mut prepared,
                &mut resources,
                &scroll,
                CaptureRegion::new(0.0, 0.0, 9000.0, 9000.0, 1.0),
            )
            .unwrap_err(),
            CaptureError::AllocationLimitExceeded
        );
    }

    #[test]
    fn simple_region_capture_rasterizes_geometry_and_text_natively_at_two_x() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="width:8px;height:8px;background:#f00"></div>
                <p style="margin:0;color:#000;font:12px sans-serif">Hi</p>
            </body></html>"#,
        );
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared =
            prepare_dom(&tree, (40.0, 30.0), None, &mut resources).expect("prepared render");
        assert!(
            native_raster_scale_supported(&tree, &prepared.layout),
            "solid boxes and shaped text should use direct device-scale paint"
        );
        let scroll = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &HashMap::new());
        let one_x = paint_prepared_region_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scroll,
            CaptureRegion::new(0.0, 0.0, 40.0, 30.0, 1.0),
        )
        .unwrap();
        let two_x = paint_prepared_region_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scroll,
            CaptureRegion::new(0.0, 0.0, 40.0, 30.0, 2.0),
        )
        .unwrap();
        assert_eq!((two_x.width(), two_x.height()), (80, 60));

        let red = |x, y| {
            let pixel = two_x.pixel(x, y).unwrap();
            pixel.red() > 245 && pixel.green() < 10 && pixel.blue() < 10
        };
        assert!(red(15, 7), "the 8 CSS-px box must cover 16 device pixels");
        assert!(
            !red(16, 7),
            "the adjacent CSS pixel must remain outside the box"
        );

        let source =
            image::RgbaImage::from_raw(one_x.width(), one_x.height(), one_x.data().to_vec())
                .unwrap();
        let post_scaled = image::imageops::resize(
            &source,
            two_x.width(),
            two_x.height(),
            image::imageops::FilterType::Lanczos3,
        );
        let differing = two_x
            .data()
            .iter()
            .zip(post_scaled.as_raw())
            .filter(|(native, post)| native != post)
            .count();
        assert!(
            differing > 100,
            "2x glyph outlines and box edges must be rerasterized, not resize-equivalent ({differing} differing channels)"
        );
        let dark_text_pixels = (16..60)
            .flat_map(|y| (0..50).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let pixel = two_x.pixel(x, y).unwrap();
                pixel.red() < 100 && pixel.green() < 100 && pixel.blue() < 100
            })
            .count();
        assert!(dark_text_pixels > 10, "native 2x text must remain painted");
    }

    #[test]
    fn direct_vector_gradients_rasterize_natively_at_two_x() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="width:20px;height:16px;background:linear-gradient(90deg,#f00 0%,#00f 100%)"></div>
                <div style="width:20px;height:16px;background:radial-gradient(circle at 50% 50%,#fff 0%,#000 100%)"></div>
            </body></html>"#,
        );
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared =
            prepare_dom(&tree, (30.0, 40.0), None, &mut resources).expect("prepared render");
        assert!(
            native_raster_scale_supported(&tree, &prepared.layout),
            "direct non-repeating linear/radial gradients should use device-scale paint"
        );
        let scroll = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &HashMap::new());
        let one_x = paint_prepared_region_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scroll,
            CaptureRegion::new(0.0, 0.0, 30.0, 40.0, 1.0),
        )
        .unwrap();
        let two_x = paint_prepared_region_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scroll,
            CaptureRegion::new(0.0, 0.0, 30.0, 40.0, 2.0),
        )
        .unwrap();
        assert_eq!((one_x.width(), one_x.height()), (30, 40));
        assert_eq!((two_x.width(), two_x.height()), (60, 80));

        let linear_left = two_x.pixel(1, 8).unwrap();
        let linear_right = two_x.pixel(38, 8).unwrap();
        assert!(
            linear_left.red() > 220 && linear_left.blue() < 40,
            "linear gradient start must remain red at 2x: {linear_left:?}"
        );
        assert!(
            linear_right.blue() > 220 && linear_right.red() < 40,
            "linear gradient end must remain blue at 2x: {linear_right:?}"
        );
        let radial_center = two_x.pixel(20, 48).unwrap();
        let radial_edge = two_x.pixel(1, 48).unwrap();
        assert!(
            radial_center.red() > 230,
            "radial center must remain light at 2x: {radial_center:?}"
        );
        assert!(
            radial_edge.red() < 100,
            "radial edge must remain dark at 2x: {radial_edge:?}"
        );

        let source =
            image::RgbaImage::from_raw(one_x.width(), one_x.height(), one_x.data().to_vec())
                .unwrap();
        let post_scaled = image::imageops::resize(
            &source,
            two_x.width(),
            two_x.height(),
            image::imageops::FilterType::Lanczos3,
        );
        let differing = two_x
            .data()
            .iter()
            .zip(post_scaled.as_raw())
            .filter(|(native, post)| native != post)
            .count();
        assert!(
            differing > 200,
            "2x vector gradient samples must be rerasterized, not resize-equivalent ({differing} differing channels)"
        );
    }

    #[test]
    fn explicit_radial_ellipse_matches_chromium_axis_samples() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="width:200px;height:100px;
                    background:radial-gradient(50% 25% at 50% 50%,#fff 0%,#000 100%)">
                </div>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (200.0, 100.0), None).expect("pixmap");
        // Chromium 144 at DPR 1 yields R values 249, 126, 2, 127, and 5 at
        // these pixel centers. Allow a small raster-backend interpolation
        // tolerance while requiring both authored radii to control geometry.
        for ((x, y), chromium) in [
            ((100, 50), 249i16),
            ((150, 50), 126),
            ((199, 50), 2),
            ((100, 62), 127),
            ((100, 74), 5),
        ] {
            let pixel = pixmap.pixel(x, y).expect("sample pixel");
            let actual = pixel.red() as i16;
            assert!(
                (actual - chromium).abs() <= 5,
                "ellipse sample ({x},{y}) was {actual}, Chromium was {chromium}: {pixel:?}"
            );
        }
    }

    #[test]
    fn radial_extent_keywords_keep_circle_and_ellipse_geometry_distinct() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0;display:flex">
                <div style="width:200px;height:100px;
                    background:radial-gradient(circle farthest-side at 25% 50%,#fff,#000)">
                </div>
                <div style="width:200px;height:100px;
                    background:radial-gradient(ellipse farthest-side at 25% 50%,#fff,#000)">
                </div>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (400.0, 100.0), None).expect("pixmap");
        let circle_bottom = pixmap.pixel(50, 99).expect("circle sample").red();
        let ellipse_bottom = pixmap.pixel(250, 99).expect("ellipse sample").red();
        assert!(
            circle_bottom > 150,
            "farthest-side circle radius is 150px, so its bottom remains light: {circle_bottom}"
        );
        assert!(
            ellipse_bottom < 10,
            "farthest-side ellipse vertical radius is 50px: {ellipse_bottom}"
        );
    }

    #[test]
    fn effectful_region_capture_keeps_the_proven_bounded_resample_fallback() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="width:20px;height:20px;background:conic-gradient(red,blue,red)"></div>
            </body></html>"#,
        );
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared =
            prepare_dom(&tree, (30.0, 30.0), None, &mut resources).expect("prepared render");
        assert!(
            !native_raster_scale_supported(&tree, &prepared.layout),
            "sampled conic gradients must retain the logical-pixel fallback"
        );
        let scroll = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &HashMap::new());
        let one_x = paint_prepared_region_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scroll,
            CaptureRegion::new(0.0, 0.0, 30.0, 30.0, 1.0),
        )
        .unwrap();
        let two_x = paint_prepared_region_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scroll,
            CaptureRegion::new(0.0, 0.0, 30.0, 30.0, 2.0),
        )
        .unwrap();
        let source = image::RgbaImage::from_raw(30, 30, one_x.data().to_vec()).unwrap();
        let expected =
            image::imageops::resize(&source, 60, 60, image::imageops::FilterType::Lanczos3);
        assert_eq!(two_x.data(), expected.as_raw());
    }

    #[test]
    fn paint_surface_culling_uses_css_coordinates_and_ink_overflow() {
        let pixmap = Pixmap::new(200, 160).expect("surface");
        assert!(rect_intersects_paint_surface(
            &crate::Rect {
                x: 90.0,
                y: 70.0,
                width: 20.0,
                height: 20.0,
            },
            &pixmap,
            2.0,
        ));
        assert!(!rect_intersects_paint_surface(
            &crate::Rect {
                x: 0.0,
                y: 81.0,
                width: 20.0,
                height: 20.0,
            },
            &pixmap,
            2.0,
        ));

        let mut style = crate::LayoutStyle::default();
        style.box_shadow = Some(crate::BoxShadow {
            offset_x: -24.0,
            offset_y: 0.0,
            blur: 8.0,
            spread: 2.0,
            color: [0, 0, 0, 255],
            inset: false,
        });
        style.outline.style = crate::BorderStyle::Solid;
        style.outline.specified_width = 4.0;
        style.outline.offset = 3.0;
        let ink = non_text_ink_bounds(
            &crate::Rect {
                x: 110.0,
                y: 10.0,
                width: 10.0,
                height: 10.0,
            },
            &style,
        );
        assert_eq!(ink.x, 76.0, "outset shadow must widen the cull rect");
        assert_eq!(ink.y, 0.0, "shadow blur must widen the cull rect vertically");
        assert_eq!(ink.x + ink.width, 127.0);
    }

    #[test]
    fn offscreen_overflow_clip_suppresses_offset_shadow_and_outline_ink() {
        let paint = |overflow: &str| {
            let html = format!(
                r#"<html style="margin:0"><body style="margin:0">
                <div style="position:absolute;left:200px;top:0;width:100px;height:120px;
                            overflow:{overflow}">
                    <div style="position:absolute;left:50px;top:20px;width:20px;height:20px;
                                box-shadow:-210px 0 0 0 black"></div>
                    <div style="position:absolute;left:50px;top:75px;width:20px;height:20px;
                                outline:210px solid red"></div>
                </div>
            </body></html>"#
            );
            let tree = parse_html(&html);
            paint_dom(&tree, (100.0, 120.0), None).expect("offscreen ink")
        };

        let unclipped = paint("visible");
        for (x, y, label) in [(45, 25, "shadow"), (45, 85, "outline")] {
            let pixel = unclipped.pixel(x, y).expect(label);
            assert_ne!(
                (pixel.red(), pixel.green(), pixel.blue()),
                (255, 255, 255),
                "control must place {label} ink at the regression sample"
            );
        }

        let pixmap = paint("hidden");
        for (x, y, label) in [(45, 25, "shadow"), (45, 85, "outline")] {
            let pixel = pixmap.pixel(x, y).expect(label);
            assert_eq!(
                (pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()),
                (255, 255, 255, 255),
                "offscreen ancestor overflow clip must suppress {label}: {pixel:?}"
            );
        }
    }

    #[test]
    fn transformed_outline_survives_tight_atomic_source_bounds() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="position:absolute;left:40px;top:40px;width:20px;height:20px;
                            outline:10px solid red;transform-origin:0 0;transform:scale(2)">
                </div>
            </body></html>"#,
        );
        let pixmap = paint_dom(&tree, (120.0, 120.0), None).expect("transformed outline");
        let outer_outline = pixmap.pixel(25, 50).expect("transformed outer outline");
        assert!(
            outer_outline.red() > 220
                && outer_outline.green() < 40
                && outer_outline.blue() < 40,
            "outline ink outside the border-box source bounds must survive the transform: {outer_outline:?}"
        );
    }

    #[test]
    fn offscreen_image_is_rejected_before_resource_lookup_and_decode() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_calls = Arc::clone(&calls);
        let mut resources = RenderResourceCache::with_loader(move |_url: &str| {
            loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(vec![0; 1024])
        });
        let mut pixmap = Pixmap::new(100, 100).expect("surface");
        let rect = crate::Rect {
            x: 0.0,
            y: 10_000.0,
            width: 80.0,
            height: 80.0,
        };
        assert!(!paint_image(
            "https://example.test/offscreen.svg",
            None,
            &rect,
            &rect,
            crate::ObjectFit::Fill,
            crate::ObjectPosition::default(),
            &mut pixmap,
            &mut resources,
            None,
            None,
            crate::ResolvedBorderRadii::default(),
            None,
        ));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "offscreen culling must happen before any resource work"
        );
    }

    #[test]
    fn viewport_cull_uses_post_translate_fixed_and_sticky_coordinates() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:1400px">
                <div style="position:absolute;left:10px;top:1000px;width:20px;height:20px;
                     transform:translateY(-990px);background:red"></div>
                <div style="position:fixed;left:40px;top:10px;width:20px;height:20px;
                     background:lime"></div>
                <div style="height:1000px"></div>
                <div style="position:sticky;left:70px;top:10px;width:20px;height:20px;
                     background:blue"></div>
            </body></html>"#,
        );
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut prepared =
            prepare_dom(&tree, (120.0, 80.0), None, &mut resources).expect("prepared render");
        let top = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &HashMap::new());
        let top_pixmap = paint_prepared_with_scroll(&tree, &mut prepared, &mut resources, &top)
            .expect("top paint");
        let translated = top_pixmap.pixel(15, 15).expect("translated pixel");
        assert!(translated.red() > 240 && translated.green() < 20);

        let scroll = prepared.resolve_scroll_state(&tree, (0.0, 990.0), &HashMap::new());
        let pixmap = paint_prepared_with_scroll(&tree, &mut prepared, &mut resources, &scroll)
            .expect("scrolled paint");
        let fixed = pixmap.pixel(45, 15).expect("fixed pixel");
        assert!(fixed.green() > 240 && fixed.red() < 20);
        let sticky = pixmap.pixel(75, 15).expect("sticky pixel");
        assert!(sticky.blue() > 240 && sticky.red() < 20);
    }

    #[test]
    fn animation_restyle_mutations_deduplicate_waapi_and_filter_disconnected_targets() {
        let tree =
            parse_html("<main><div id=connected></div><div id=detached></div></main>");
        let connected = tree.get_element_by_id("connected").unwrap();
        let detached = tree.get_element_by_id("detached").unwrap();
        tree.remove_child(detached);
        let animation = |id, node| crate::WaapiAnimation {
            id,
            node,
            keyframes: vec![crate::WaapiKeyframe {
                offset: 0.0,
                opacity: Some(0.0),
                transform: None,
            }],
            timing: crate::AnimationTiming {
                duration_ms: 1_000.0,
                ..crate::AnimationTiming::default()
            },
            easing: None,
            linear_easing: None,
            start_time_ms: 0.0,
            hold_time_ms: None,
            play_state: crate::WaapiPlayState::Running,
        };
        let mut timeline = crate::AnimationTimelineState::default();
        timeline.register_waapi(animation(1, connected));
        timeline.register_waapi(animation(2, connected));
        timeline.register_waapi(animation(3, detached));
        assert_eq!(
            timeline.waapi_nodes(),
            std::collections::HashSet::from([connected, detached]),
            "the timeline accessor must return the exact deduplicated target set"
        );

        let mutations = retained_animation_restyle_mutations(&tree, &HashMap::new(), &timeline);
        assert_eq!(
            mutations,
            vec![crate::dom::RetainedStyleMutation::WaapiAnimation {
                node: connected
            }],
            "only connected WAAPI targets may enter retained style damage"
        );
    }

    #[test]
    fn active_waapi_transform_is_conservatively_geometry_affecting() {
        let tree = parse_html("<div id=target></div>");
        let target = tree.get_element_by_id("target").unwrap();
        let animation = |id, opacity, transform| crate::WaapiAnimation {
            id,
            node: target,
            keyframes: vec![crate::WaapiKeyframe {
                offset: 0.0,
                opacity,
                transform,
            }],
            timing: crate::AnimationTiming {
                duration_ms: 1_000.0,
                ..crate::AnimationTiming::default()
            },
            easing: None,
            linear_easing: None,
            start_time_ms: 0.0,
            hold_time_ms: None,
            play_state: crate::WaapiPlayState::Running,
        };
        let sample = crate::AnimationSampleTime { milliseconds: 100.0 };
        let mut timeline = crate::AnimationTimelineState::default();
        timeline.register_waapi(animation(1, Some(0.5), None));
        assert_eq!(
            timeline.active_waapi_effect_impact(sample),
            crate::AnimationEffectImpact::Paint,
        );
        timeline.register_waapi(animation(
            2,
            None,
            Some("future-transform(1)".to_string()),
        ));
        assert_eq!(
            timeline.active_waapi_effect_impact(sample),
            crate::AnimationEffectImpact::Geometry,
        );
    }

    #[test]
    fn visual_waapi_refresh_matches_forced_full_geometry_and_pixels() {
        let tree = parse_html(
            r#"<html style="margin:0;background:white"><body style="margin:0">
                <div id="moving" style="position:absolute;left:20px;top:20px;width:80px;height:60px;
                     overflow:hidden;border-radius:12px;background:red;transform:translateX(0px)">
                    <div style="width:130px;height:60px;background:lime"></div>
                    <div id="captured-fixed" style="position:fixed;left:4px;top:4px;
                         width:12px;height:12px;background:yellow"></div>
                </div>
                <div style="position:absolute;left:145px;top:20px;width:75px;height:75px;overflow:hidden">
                    <div id="affine" style="width:60px;height:60px;background:blue;
                         transform-origin:30px 30px;transform:rotate(0deg)"></div>
                </div>
                <div id="overflow" style="position:absolute;left:10px;top:100px;width:10px;height:10px;
                     background:black;transform:translateX(0px)"></div>
                <div id="scroller" style="position:absolute;left:150px;top:96px;width:70px;height:22px;
                     overflow:auto;background:purple">
                    <div id="scroll-target" style="width:150px;height:80px;background:orange"></div>
                </div>
                <div id="flow" style="width:24px;height:320px">
                    <div style="height:65px"></div>
                    <div id="sticky" style="position:sticky;top:3px;width:24px;height:12px;background:cyan">
                        <div id="sticky-child" style="width:8px;height:8px;background:black"></div>
                    </div>
                </div>
                <div id="viewport-fixed" style="position:fixed;right:0;top:0;
                     width:8px;height:8px;background:magenta"></div>
            </body></html>"#,
        );
        let moving = tree.get_element_by_id("moving").unwrap();
        let affine = tree.get_element_by_id("affine").unwrap();
        let overflow = tree.get_element_by_id("overflow").unwrap();
        let captured_fixed = tree.get_element_by_id("captured-fixed").unwrap();
        let viewport_fixed = tree.get_element_by_id("viewport-fixed").unwrap();
        let scroller = tree.get_element_by_id("scroller").unwrap();
        let scroll_target = tree.get_element_by_id("scroll-target").unwrap();
        let sticky = tree.get_element_by_id("sticky").unwrap();
        let sticky_child = tree.get_element_by_id("sticky-child").unwrap();
        let make_timeline = || {
            let mut timeline = crate::AnimationTimelineState::default();
            for (id, node, from, to) in [
                (1, moving, "translateX(0px)", "translateX(36px)"),
                (2, affine, "rotate(0deg)", "rotate(28deg)"),
                (3, overflow, "translateX(0px)", "translateX(500px)"),
            ] {
                timeline.register_waapi(crate::WaapiAnimation {
                    id,
                    node,
                    keyframes: vec![
                        crate::WaapiKeyframe {
                            offset: 0.0,
                            opacity: None,
                            transform: Some(from.to_string()),
                        },
                        crate::WaapiKeyframe {
                            offset: 1.0,
                            opacity: None,
                            transform: Some(to.to_string()),
                        },
                    ],
                    timing: crate::AnimationTiming {
                        duration_ms: 1_000.0,
                        fill_mode: crate::AnimationFillMode::Both,
                        ..Default::default()
                    },
                    easing: None,
                    linear_easing: None,
                    start_time_ms: 0.0,
                    hold_time_ms: None,
                    play_state: crate::WaapiPlayState::Running,
                });
            }
            timeline.register_waapi(crate::WaapiAnimation {
                id: 4,
                node: moving,
                keyframes: vec![
                    crate::WaapiKeyframe {
                        offset: 0.0,
                        opacity: Some(0.0),
                        transform: None,
                    },
                    crate::WaapiKeyframe {
                        offset: 1.0,
                        opacity: Some(1.0),
                        transform: None,
                    },
                ],
                timing: crate::AnimationTiming {
                    duration_ms: 1_000.0,
                    fill_mode: crate::AnimationFillMode::Both,
                    ..Default::default()
                },
                easing: None,
                linear_easing: None,
                start_time_ms: 0.0,
                hold_time_ms: None,
                play_state: crate::WaapiPlayState::Running,
            });
            timeline
        };
        let viewport = (240.0, 120.0);
        let mut candidate_resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut candidate_cache = crate::css::StylesheetCache::default();
        let mut candidate_timeline = make_timeline();
        let mut candidate =
            prepare_dom_with_dynamic_fonts_and_stylesheet_cache_with_animation_state(
                &tree,
                viewport,
                None,
                &mut candidate_resources,
                &[],
                &mut candidate_cache,
                crate::AnimationSample::document(0.0),
                &mut candidate_timeline,
            )
            .expect("initial candidate");
        let initial_transforms = candidate.layout.transforms.clone();
        assert!(candidate.try_advance_visual_waapi_sample(
            &tree,
            crate::AnimationSample::document(500.0),
            &candidate_timeline,
        ));
        assert_ne!(candidate.layout.transforms, initial_transforms);

        let mut oracle_resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut oracle_cache = crate::css::StylesheetCache::default();
        let mut oracle_timeline = make_timeline();
        let mut oracle =
            prepare_dom_with_dynamic_fonts_and_stylesheet_cache_with_animation_state(
                &tree,
                viewport,
                None,
                &mut oracle_resources,
                &[],
                &mut oracle_cache,
                crate::AnimationSample::document(500.0),
                &mut oracle_timeline,
            )
            .expect("forced-full oracle");

        assert_eq!(candidate.layout.rects, oracle.layout.rects);
        assert_eq!(candidate.layout.inline_fragments, oracle.layout.inline_fragments);
        assert_eq!(candidate.layout.translates, oracle.layout.translates);
        assert_eq!(candidate.layout.transforms, oracle.layout.transforms);
        assert_eq!(candidate.layout.clip_rects, oracle.layout.clip_rects);
        assert_eq!(candidate.content_size, oracle.content_size);
        assert!(candidate.content_size.0 > viewport.0);
        assert!(candidate.content_size.1 > viewport.1);
        assert_eq!(candidate.viewport_fixed, oracle.viewport_fixed);
        assert!(candidate.viewport_fixed.contains(&viewport_fixed));
        assert!(!candidate.viewport_fixed.contains(&captured_fixed));
        assert_eq!(
            candidate.scroll_container_nodes().collect::<Vec<_>>(),
            oracle.scroll_container_nodes().collect::<Vec<_>>(),
        );
        assert!(candidate.scroll_container_nodes().any(|node| node == scroller));
        for node in [moving, affine, overflow] {
            assert_eq!(
                format!("{:?}", candidate.layout.styles[&node]),
                format!("{:?}", oracle.layout.styles[&node]),
            );
        }
        let element_offsets = HashMap::from([(scroller, (9999.0, 9999.0))]);
        let candidate_scroll = candidate.resolve_scroll_state(&tree, (0.0, 80.0), &element_offsets);
        let oracle_scroll = oracle.resolve_scroll_state(&tree, (0.0, 80.0), &element_offsets);
        assert_eq!(candidate_scroll.root_offset, oracle_scroll.root_offset);
        assert_eq!(
            candidate_scroll.container_offsets,
            oracle_scroll.container_offsets
        );
        assert_eq!(candidate_scroll.node_movement, oracle_scroll.node_movement);
        assert_eq!(
            candidate_scroll.inherited_clips,
            oracle_scroll.inherited_clips
        );
        assert_eq!(
            candidate.element_scroll_metrics(scroller, &candidate_scroll),
            oracle.element_scroll_metrics(scroller, &oracle_scroll),
        );
        assert_eq!(
            candidate.viewport_rect_with_scroll(sticky, &candidate_scroll),
            oracle.viewport_rect_with_scroll(sticky, &oracle_scroll),
        );
        assert_eq!(
            candidate.viewport_rect_with_scroll(sticky_child, &candidate_scroll),
            oracle.viewport_rect_with_scroll(sticky_child, &oracle_scroll),
        );
        assert_eq!(
            candidate.viewport_rect_with_scroll(captured_fixed, &candidate_scroll),
            oracle.viewport_rect_with_scroll(captured_fixed, &oracle_scroll),
        );
        assert_eq!(
            candidate.viewport_rect_with_scroll(scroll_target, &candidate_scroll),
            oracle.viewport_rect_with_scroll(scroll_target, &oracle_scroll),
        );
        assert_ne!(
            candidate.sticky.translations(viewport, candidate_scroll.root_offset),
            HashMap::new(),
        );
        let candidate_pixels = paint_prepared_with_scroll(
            &tree,
            &mut candidate,
            &mut candidate_resources,
            &candidate_scroll,
        )
        .expect("candidate pixels");
        let oracle_pixels = paint_prepared_with_scroll(
            &tree,
            &mut oracle,
            &mut oracle_resources,
            &oracle_scroll,
        )
        .expect("oracle pixels");
        assert_eq!(candidate_pixels.data(), oracle_pixels.data());
    }

    #[test]
    fn visual_waapi_refresh_rejects_unsupported_effect_atomically() {
        let tree = parse_html(
            r#"<div id="pure" style="transform:translateX(0px)"></div>
                <div id="mixed" style="transform:translateX(0px)"></div>"#,
        );
        let pure = tree.get_element_by_id("pure").unwrap();
        let mixed = tree.get_element_by_id("mixed").unwrap();
        let mut timeline = crate::AnimationTimelineState::default();
        for (id, node, opacity, transform) in [
            (1, pure, None, Some("translateX(40px)".to_string())),
            (2, mixed, None, None),
        ] {
            timeline.register_waapi(crate::WaapiAnimation {
                id,
                node,
                keyframes: vec![crate::WaapiKeyframe {
                    offset: 1.0,
                    opacity,
                    transform,
                }],
                timing: crate::AnimationTiming {
                    duration_ms: 1_000.0,
                    fill_mode: crate::AnimationFillMode::Both,
                    ..Default::default()
                },
                easing: None,
                linear_easing: None,
                start_time_ms: 0.0,
                hold_time_ms: None,
                play_state: crate::WaapiPlayState::Running,
            });
        }
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut cache = crate::css::StylesheetCache::default();
        let mut prepared =
            prepare_dom_with_dynamic_fonts_and_stylesheet_cache_with_animation_state(
                &tree,
                (100.0, 100.0),
                None,
                &mut resources,
                &[],
                &mut cache,
                crate::AnimationSample::document(0.0),
                &mut timeline,
            )
            .expect("initial render");
        let sample_before = prepared.animation_sample;
        let pure_before = prepared.layout.styles[&pure].transform_ops.clone();
        assert!(!prepared.try_advance_visual_waapi_sample(
            &tree,
            crate::AnimationSample::document(500.0),
            &timeline,
        ));
        assert_eq!(prepared.animation_sample, sample_before);
        assert_eq!(
            format!("{:?}", prepared.layout.styles[&pure].transform_ops),
            format!("{pure_before:?}"),
        );
    }

    #[test]
    fn visual_waapi_refresh_rejects_containing_block_topology_change() {
        let tree = parse_html(
            r#"<div id="target"><div style="position:fixed;left:10px;top:10px"></div></div>"#,
        );
        let target = tree.get_element_by_id("target").unwrap();
        let mut timeline = crate::AnimationTimelineState::default();
        timeline.register_waapi(crate::WaapiAnimation {
            id: 1,
            node: target,
            keyframes: vec![crate::WaapiKeyframe {
                offset: 1.0,
                opacity: None,
                transform: Some("translateX(40px)".into()),
            }],
            timing: crate::AnimationTiming {
                delay_ms: 100.0,
                duration_ms: 1_000.0,
                ..Default::default()
            },
            easing: None,
            linear_easing: None,
            start_time_ms: 0.0,
            hold_time_ms: None,
            play_state: crate::WaapiPlayState::Running,
        });
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut cache = crate::css::StylesheetCache::default();
        let mut prepared =
            prepare_dom_with_dynamic_fonts_and_stylesheet_cache_with_animation_state(
                &tree,
                (100.0, 100.0),
                None,
                &mut resources,
                &[],
                &mut cache,
                crate::AnimationSample::document(0.0),
                &mut timeline,
            )
            .expect("initial render before delayed effect");
        assert_eq!(
            prepared.layout.styles[&target].containing_block_triggers
                & crate::CB_TRIGGER_TRANSFORM,
            0,
        );
        assert!(!prepared.try_advance_visual_waapi_sample(
            &tree,
            crate::AnimationSample::document(500.0),
            &timeline,
        ));
        assert_eq!(prepared.animation_sample, crate::AnimationSample::document(0.0));
    }

    #[test]
    fn prepared_render_samples_explicit_animation_time_and_reports_live_damage() {
        let tree = parse_html(
            r#"<style>
                @keyframes dismiss {
                    from { opacity:1; visibility:visible }
                    to { opacity:0; visibility:hidden }
                }
                #overlay { opacity:1; animation:dismiss 600ms linear forwards }
            </style><div id="overlay" style="width:20px;height:20px;background:red"></div>"#,
        );
        let overlay = tree.query_selector("#overlay").unwrap().unwrap();
        let mut resources = RenderResourceCache::with_loader(|_url: &str| None);
        let mut stylesheets = crate::css::StylesheetCache::default();
        let at_zero = prepare_dom_with_dynamic_fonts_and_stylesheet_cache_at_animation_time(
            &tree,
            (40.0, 40.0),
            None,
            &mut resources,
            &[],
            &mut stylesheets,
            crate::AnimationSampleTime { milliseconds: 0.0 },
        )
        .expect("T=0 render");
        assert_eq!(at_zero.animation_sample_time().milliseconds, 0.0);
        assert_eq!(at_zero.layout.styles[&overlay].opacity, Some(1.0));
        assert_ne!(at_zero.layout.styles[&overlay].visibility_hidden, Some(true));
        assert!(at_zero.has_active_css_animations());

        let at_end = prepare_dom_with_dynamic_fonts_and_stylesheet_cache_at_animation_time(
            &tree,
            (40.0, 40.0),
            None,
            &mut resources,
            &[],
            &mut stylesheets,
            crate::AnimationSampleTime { milliseconds: 600.0 },
        )
        .expect("animation-end render");
        assert_eq!(at_end.layout.styles[&overlay].opacity, Some(0.0));
        assert_eq!(at_end.layout.styles[&overlay].visibility_hidden, Some(true));
        assert!(at_end.layout.styles[&overlay].effectively_invisible);
        assert!(!at_end.has_active_css_animations());
    }
}
