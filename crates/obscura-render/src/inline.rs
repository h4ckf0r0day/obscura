//! Real inline text layout via cosmic-text (HarfBuzz-class shaping through
//! rustybuzz + UAX#14 line breaking), replacing the earlier approximation
//! that split each text node into one taffy flex item per word.
//!
//! The model matches how real browsers separate formatting contexts: taffy
//! lays out the *block/flex/grid* boxes, and an *inline formatting context*
//! (a box whose children are all inline-level text/spans) collapses to a
//! single taffy leaf whose measure function line-breaks its shaped text at
//! whatever width taffy offers. Line wrapping, alignment, and intrinsic
//! sizing then come from a real text engine instead of flexbox tricks.
//!
//! Fonts are loaded from embedded bytes only, never the OS, so layout is
//! byte-for-byte deterministic across hosts (the whole engine's guarantee).

use std::{collections::HashMap, sync::Arc};

use cosmic_text::{
    Align, Attrs, Buffer, CacheKey, CacheKeyFlags, Color, CssLineBreak, CssOverflowWrap,
    CssWordBreak, Cursor, Family, FeatureTag, FontFeatures, FontSystem, FontVariations, Metrics,
    Shaping, Style, SwashCache, SwashImage, VariationTag, Weight, Wrap,
};
use swash::scale::{image::Content as SwashContent, Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Angle, Format, Transform, Vector};

use obscura_dom::tree::{DomTree, NodeId};

use crate::{Dimension, Display, LayoutStyle, Rect, TextTransform};

// Bundled faces. Chrome on this class of host renders `sans-serif` and the
// ubiquitous Arial/Helvetica stacks as Liberation Sans, `system-ui` as DejaVu
// Sans, `serif` as Liberation Serif, and `monospace` as Liberation Mono.
// Matching those keeps text metrics (advance widths, wrapping, line positions)
// aligned with Chromium instead of drifting between unrelated host faces.
static SANS_R: &[u8] = include_bytes!("../assets/liberation-sans.ttf");
static SANS_B: &[u8] = include_bytes!("../assets/liberation-sans-bold.ttf");
static SANS_O: &[u8] = include_bytes!("../assets/liberation-sans-oblique.ttf");
static SANS_BO: &[u8] = include_bytes!("../assets/liberation-sans-boldoblique.ttf");
static SERIF_R: &[u8] = include_bytes!("../assets/liberation-serif.ttf");
static SERIF_B: &[u8] = include_bytes!("../assets/liberation-serif-bold.ttf");
static SERIF_O: &[u8] = include_bytes!("../assets/liberation-serif-oblique.ttf");
static SERIF_BO: &[u8] = include_bytes!("../assets/liberation-serif-boldoblique.ttf");
static MONO_R: &[u8] = include_bytes!("../assets/liberation-mono.ttf");
static MONO_B: &[u8] = include_bytes!("../assets/liberation-mono-bold.ttf");
static MONO_O: &[u8] = include_bytes!("../assets/liberation-mono-oblique.ttf");
static MONO_BO: &[u8] = include_bytes!("../assets/liberation-mono-boldoblique.ttf");
static SYSTEM_R: &[u8] = include_bytes!("../assets/dejavu-sans.ttf");
static SYSTEM_B: &[u8] = include_bytes!("../assets/dejavu-sans-bold.ttf");
static EMOJI_R: &[u8] = include_bytes!("../assets/noto-color-emoji.ttf");
#[cfg(test)]
static FALLBACK: &[u8] = SYSTEM_R;

const FAMILY: &str = "Liberation Sans";
const SERIF_FAMILY: &str = "Liberation Serif";
const MONO_FAMILY: &str = "Liberation Mono";
const SYSTEM_FAMILY: &str = "DejaVu Sans";

/// Whether text contains a code point that can request emoji presentation.
/// Keep the color face out of ordinary render passes: its bitmap table is
/// large, and loading it for every page would spend RSS and startup time even
/// when no emoji can be shaped.
pub(crate) fn text_may_need_emoji_font(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(
            ch,
            '\u{00A9}' | '\u{00AE}' | '\u{203C}' | '\u{2049}' | '\u{2122}' | '\u{2139}'
                | '\u{2194}'..='\u{2199}' | '\u{21A9}'..='\u{21AA}'
                | '\u{231A}'..='\u{231B}' | '\u{2328}' | '\u{23CF}'
                | '\u{23E9}'..='\u{23F3}' | '\u{23F8}'..='\u{23FA}' | '\u{24C2}'
                | '\u{25AA}'..='\u{25AB}' | '\u{25B6}' | '\u{25C0}'
                | '\u{25FB}'..='\u{25FE}' | '\u{2600}'..='\u{2604}' | '\u{2611}'
                | '\u{2614}'..='\u{2615}' | '\u{2618}' | '\u{261D}' | '\u{2620}'
                | '\u{2622}'..='\u{2623}' | '\u{2626}' | '\u{262A}'
                | '\u{262E}'..='\u{262F}' | '\u{2638}'..='\u{263A}' | '\u{2640}'
                | '\u{2642}' | '\u{2648}'..='\u{2653}' | '\u{265F}'..='\u{2660}'
                | '\u{2663}' | '\u{2665}'..='\u{2666}' | '\u{2668}' | '\u{267B}'
                | '\u{267E}'..='\u{267F}' | '\u{2692}'..='\u{2697}' | '\u{2699}'
                | '\u{269B}'..='\u{269C}' | '\u{26A0}'..='\u{26A1}' | '\u{26A7}'
                | '\u{26AA}'..='\u{26AB}' | '\u{26B0}'..='\u{26B1}'
                | '\u{26BD}'..='\u{26BE}' | '\u{26C4}'..='\u{26C5}' | '\u{26C8}'
                | '\u{26CE}'..='\u{26CF}' | '\u{26D1}' | '\u{26D3}'..='\u{26D4}'
                | '\u{26E9}'..='\u{26EA}' | '\u{26F0}'..='\u{26F5}'
                | '\u{26F7}'..='\u{26FA}' | '\u{26FD}' | '\u{2702}' | '\u{2705}'
                | '\u{2708}'..='\u{270D}' | '\u{270F}' | '\u{2712}' | '\u{2714}'
                | '\u{2716}' | '\u{271D}' | '\u{2721}' | '\u{2728}'
                | '\u{2733}'..='\u{2734}' | '\u{2744}' | '\u{2747}' | '\u{274C}'
                | '\u{274E}' | '\u{2753}'..='\u{2755}' | '\u{2757}'
                | '\u{2763}'..='\u{2764}' | '\u{2795}'..='\u{2797}' | '\u{27A1}'
                | '\u{27B0}' | '\u{27BF}' | '\u{2934}'..='\u{2935}'
                | '\u{2B05}'..='\u{2B07}' | '\u{2B1B}'..='\u{2B1C}' | '\u{2B50}'
                | '\u{2B55}' | '\u{3030}' | '\u{303D}' | '\u{3297}' | '\u{3299}'
                | '\u{FE0F}' | '\u{1F000}'..='\u{1FAFF}'
        )
    })
}

/// Map a CSS `font-family` list to a bundled face the way Chromium resolves the
/// generic families on this host. Chromium's Linux `system-ui` resolves to
/// DejaVu Sans, while `sans-serif`/Arial/Helvetica resolve to Liberation Sans.
/// The first recognizable family wins, matching CSS fallback order.
fn resolve_font_family(fam: Option<&str>) -> &'static str {
    let Some(f) = fam else { return FAMILY };
    for tok in f.split(',') {
        if let Some(family) = bundled_family_for_css_token(tok) {
            return family;
        }
        // Unrecognized named webfont: keep scanning for a generic fallback.
    }
    FAMILY
}

fn bundled_family_for_css_token(token: &str) -> Option<&'static str> {
    let token = token
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_ascii_lowercase();
    if token.is_empty() {
        return None;
    }
    if token == "system-ui" || token == "ui-sans-serif" {
        return Some(SYSTEM_FAMILY);
    }
    if token == "monospace"
        || token.contains("mono")
        || token.contains("courier")
        || token.contains("consol")
        || token == "menlo"
        || token == "monaco"
        || token == "code"
    {
        return Some(MONO_FAMILY);
    }
    if token == "serif"
        || token == "georgia"
        || token.contains("times")
        || token == "cambria"
        || token.contains("garamond")
        || token.contains("liberation serif")
        || token == "roman"
    {
        return Some(SERIF_FAMILY);
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
        return Some(FAMILY);
    }
    None
}

#[derive(Clone)]
struct LoadedFamily {
    faces: Vec<LoadedFace>,
}

#[derive(Clone)]
struct LoadedFace {
    name: Arc<str>,
    font_id: Option<cosmic_text::fontdb::ID>,
    metrics: FaceMetrics,
    min_weight: u16,
    max_weight: u16,
    italic: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FaceMetrics {
    ascent: f32,
    descent: f32,
    line_gap: f32,
    units_per_em: f32,
}

#[derive(Clone)]
struct ResolvedFont {
    family: Arc<str>,
    font_id: Option<cosmic_text::fontdb::ID>,
    metrics: FaceMetrics,
    synthetic_italic: bool,
}

#[derive(Clone)]
pub(crate) struct WebFont {
    pub data: Vec<u8>,
    pub family: Option<String>,
    pub weight: Option<(u16, u16)>,
    pub italic: Option<bool>,
}

fn resolve_loaded_font(
    fam: Option<&str>,
    requested_weight: u16,
    requested_italic: bool,
    loaded: &HashMap<String, LoadedFamily>,
) -> ResolvedFont {
    if let Some(stack) = fam {
        for token in stack.split(',') {
            let name = token.trim().trim_matches(|c| c == '"' || c == '\'').trim();
            let family = loaded.get(&name.to_ascii_lowercase()).or_else(|| {
                bundled_family_for_css_token(name)
                    .and_then(|family| loaded.get(&family.to_ascii_lowercase()))
            });
            if let Some(resolved) = family
                .and_then(|family| select_loaded_face(family, requested_weight, requested_italic))
            {
                return resolved;
            }
        }
    }
    let fallback = resolve_font_family(fam);
    ResolvedFont {
        family: Arc::from(fallback),
        font_id: None,
        metrics: bundled_face_metrics(fallback),
        synthetic_italic: false,
    }
}

fn select_loaded_face(
    family: &LoadedFamily,
    requested_weight: u16,
    requested_italic: bool,
) -> Option<ResolvedFont> {
    let exact_style: Vec<_> = family
        .faces
        .iter()
        .filter(|face| face.italic == requested_italic)
        .collect();
    let candidates: Vec<_> = if exact_style.is_empty() {
        family.faces.iter().collect()
    } else {
        exact_style
    };
    if let Some(face) = candidates
        .iter()
        .copied()
        .find(|face| (face.min_weight..=face.max_weight).contains(&requested_weight))
    {
        // The named-family matcher uses fontdb's default weight for this
        // resource, while a variable face commonly advertises `100 900` in
        // CSS. Preserve the descriptor-selected file and its database weight;
        // the authored coordinate enters the canonical axis tuple below.
        return Some(ResolvedFont {
            family: Arc::clone(&face.name),
            font_id: face.font_id,
            metrics: face.metrics,
            synthetic_italic: requested_italic && !face.italic,
        });
    }
    let available: Vec<_> = candidates.iter().map(|face| face.min_weight).collect();
    let matched = match_font_weight(requested_weight, &available);
    candidates
        .into_iter()
        .find(|face| face.min_weight == matched)
        .map(|face| ResolvedFont {
            family: Arc::clone(&face.name),
            font_id: face.font_id,
            metrics: face.metrics,
            synthetic_italic: requested_italic && !face.italic,
        })
}

/// CSS Fonts' asymmetric missing-weight search. In particular, 600 selects
/// 700 (not 400) when a family only provides regular and bold faces.
fn match_font_weight(requested: u16, available: &[u16]) -> u16 {
    if available.contains(&requested) {
        return requested;
    }
    let mut weights = available.to_vec();
    weights.sort_unstable();
    weights.dedup();
    if weights.is_empty() {
        return requested;
    }
    if (400..=500).contains(&requested) {
        weights
            .iter()
            .copied()
            .filter(|weight| *weight >= requested && *weight <= 500)
            .min()
            .or_else(|| {
                weights
                    .iter()
                    .copied()
                    .filter(|weight| *weight < requested)
                    .max()
            })
            .or_else(|| weights.iter().copied().filter(|weight| *weight > 500).min())
            .unwrap_or(requested)
    } else if requested < 400 {
        weights
            .iter()
            .copied()
            .filter(|weight| *weight <= requested)
            .max()
            .or_else(|| {
                weights
                    .iter()
                    .copied()
                    .filter(|weight| *weight > requested)
                    .min()
            })
            .unwrap_or(requested)
    } else {
        weights
            .iter()
            .copied()
            .filter(|weight| *weight >= requested)
            .min()
            .or_else(|| {
                weights
                    .iter()
                    .copied()
                    .filter(|weight| *weight < requested)
                    .max()
            })
            .unwrap_or(requested)
    }
}

/// Resolve `line-height: normal` from the selected face's horizontal header.
///
/// Chromium's FreeType-backed Linux path grid-fits the ascent, descent, and
/// line gap independently before adding them. Multiplying their sum by the
/// font size (or rounding the final line height) is observably different at
/// fractional and small sizes: Liberation Sans at 9.333px is 10px in
/// Chromium, not 11px. Keep these metrics beside the embedded faces so normal
/// line boxes follow the same device-pixel rhythm without consulting host
/// fonts.
fn bundled_face_metrics(family: &str) -> FaceMetrics {
    let (ascent, descent, line_gap) = match family {
        SERIF_FAMILY => (1825.0, 443.0, 87.0),
        MONO_FAMILY => (1705.0, 615.0, 0.0),
        SYSTEM_FAMILY => (1901.0, 483.0, 0.0),
        _ => (1854.0, 434.0, 67.0),
    };
    FaceMetrics {
        ascent,
        descent,
        line_gap,
        units_per_em: 2048.0,
    }
}

fn normal_line_height(font_size: f32, metrics: FaceMetrics) -> f32 {
    let scale = font_size / metrics.units_per_em.max(1.0);
    (metrics.ascent * scale).round()
        + (metrics.descent * scale).round()
        + (metrics.line_gap * scale).round()
}

/// Grid-fitted font ascent plus descent, excluding both the face line gap and
/// authored CSS `line-height`.
///
/// A non-replaced inline has two different vertical boxes. Its line
/// participation uses [`used_line_height_with_metrics`], while its painted
/// fragment/client rect uses this raw font box plus block-axis padding and
/// border. Chromium and Gecko both fit ascent and descent independently.
fn fitted_font_box_metrics(font_size: f32, metrics: FaceMetrics) -> (f32, f32) {
    let scale = font_size / metrics.units_per_em.max(1.0);
    (
        (metrics.ascent * scale).round(),
        (metrics.descent * scale).round(),
    )
}

fn font_metrics(
    db: &cosmic_text::fontdb::Database,
    id: cosmic_text::fontdb::ID,
) -> Option<FaceMetrics> {
    db.with_face_data(id, |data, face_index| {
        let face = cosmic_text::ttf_parser::Face::parse(data, face_index).ok()?;
        Some(FaceMetrics {
            ascent: face.ascender().max(0) as f32,
            descent: -(face.descender().min(0) as f32),
            line_gap: face.line_gap().max(0) as f32,
            units_per_em: face.units_per_em() as f32,
        })
    })
    .flatten()
}

fn used_line_height_for_font(style: &LayoutStyle, font: &ResolvedFont) -> f32 {
    used_line_height_with_metrics(style, font.metrics)
}

fn used_line_height_with_metrics(style: &LayoutStyle, metrics: FaceMetrics) -> f32 {
    let font_size = style.font_size.unwrap_or(16.0);
    match style.line_height {
        Some(crate::LineHeight::Px(px)) => px,
        Some(crate::LineHeight::Ratio(ratio)) => font_size * ratio,
        Some(crate::LineHeight::Relative(relative)) => match relative {
            crate::Dimension::Percent(percent) => font_size * percent,
            dimension => match dimension.resolve(font_size, 16.0, 0.0, 0.0) {
                crate::Dimension::Px(px) => px,
                _ => font_size,
            },
        },
        None | Some(crate::LineHeight::Normal) => normal_line_height(font_size, metrics),
    }
}

/// Computed used line-height shared by shaped inline runs and forced-break
/// sentinels that cannot join a run.
pub(crate) fn used_line_height(style: &LayoutStyle) -> f32 {
    let family = resolve_font_family(style.font_family.as_deref());
    used_line_height_with_metrics(style, bundled_face_metrics(family))
}

type ClipTextFill = (f32, Vec<([u8; 4], Option<f32>)>);

/// Per-glyph flags carried through cosmic-text's metadata. The common
/// non-variable path stays allocation-free: underline and fill remain packed
/// directly, while the upper half is a one-based index into an optional
/// variation-set table.
const META_UNDERLINE: usize = 1;
const META_FILL_SHIFT: usize = 1;
const META_VARIATION_BITS: usize = usize::BITS as usize / 2;
const META_VARIATION_SHIFT: usize = usize::BITS as usize - META_VARIATION_BITS;
const META_VARIATION_MASK: usize = ((1usize << META_VARIATION_BITS) - 1) << META_VARIATION_SHIFT;
const META_FILL_MASK: usize = ((1usize << META_VARIATION_SHIFT) - 1) & !META_UNDERLINE;

fn metadata_fill(metadata: usize) -> Option<usize> {
    ((metadata & META_FILL_MASK) >> META_FILL_SHIFT).checked_sub(1)
}

fn metadata_variation(metadata: usize) -> Option<usize> {
    ((metadata & META_VARIATION_MASK) >> META_VARIATION_SHIFT).checked_sub(1)
}

/// One inline formatting context: a shaped cosmic-text buffer plus where to
/// paint it (filled in after layout).
pub struct InlineItem {
    buffer: Buffer,
    /// Wrapping used for definite-width layout and final paint.
    layout_wrap: Wrap,
    /// Wrapping used only for an intrinsic min-content query. This differs
    /// from `layout_wrap` for `overflow-wrap: break-word`: emergency breaks
    /// are available during reflow but do not reduce min-content.
    min_content_wrap: Wrap,
    /// Unmodified shaped content retained only when `text-indent` is nonzero.
    /// Each Taffy measurement can then derive a first-line-only wrap boundary
    /// without accumulating synthetic hard breaks across repeated probes.
    source_buffer: Option<Buffer>,
    text_indent: Dimension,
    /// Used LTR paint offset for the first formatted line at the most recently
    /// shaped width. Later lines retain the ordinary content-box origin.
    first_line_offset: f32,
    /// Whether final shaping should tighten the wrap width while preserving
    /// the natural line count (`text-wrap-style: balance`).
    balance_wrap: bool,
    /// Alignment is applied against the original content width. Cosmic-text
    /// uses its buffer width for both wrapping and alignment, so a balanced
    /// (narrower) buffer needs a corresponding origin inset.
    align: Option<Align>,
    /// Minimum block-size contributed by explicit `<br>` breaks. cosmic-text
    /// omits the final empty run for a trailing newline, while CSS still gives
    /// a break-only or consecutive-break line the parent's used line-height.
    forced_min_height: f32,
    /// Content-box top-left in viewport coordinates, set by `finalize`.
    origin: (f32, f32),
    /// Ancestor `overflow: hidden` clip, set by `finalize`.
    clip: Option<Rect>,
    /// Per-span `-webkit-background-clip: text` fills. Glyph metadata selects
    /// one entry, allowing an inline accent span to own a gradient without
    /// recoloring the rest of its heading.
    clip_fills: Vec<ClipTextFill>,
    /// Canonical axis tuples, populated only when this IFC actually contains
    /// variable text. Glyph metadata stores a one-based index.
    variation_sets: Vec<Arc<FontVariations>>,
    /// Direct pure-text legacy clamp. Nested block descendants do not enter
    /// this IFC and are intentionally left to the future block-line iterator.
    line_clamp: Option<usize>,
    /// Ordinary single-value ellipsis is active only when inline-axis
    /// overflow is non-visible. The full buffer remains intact for natural
    /// overflow metrics and overflow-visible clamp painting.
    ellipsis_overflow: bool,
    /// Separately shaped marker using the same final text attributes. Keeping
    /// this distinct avoids mutating DOM text or the natural content buffer.
    marker_buffer: Option<Buffer>,
    marker: Option<MarkerPlacement>,
    /// Canonical collapsed/transformed text used to map shaped byte ranges back
    /// to ordinary inline DOM owners. Absent for the overwhelmingly common IFC
    /// with no nested inline owner, keeping that path allocation-free.
    owner_text: Option<String>,
    owner_ranges: Vec<OwnerTextRange>,
    owner_boxes: Vec<InlineOwnerBox>,
    boundary_events: Vec<InlineBoundaryEvent>,
    /// Nonzero used relative-position offsets, projected onto text ranges.
    /// Empty for ordinary IFCs and for nested inlines that remain at their
    /// normal-flow position, so paint pays no provenance cost on that path.
    relative_owner_ranges: Vec<RelativeOwnerTextRange>,
}

#[derive(Clone, Copy, Debug)]
struct OwnerTextRange {
    owner: NodeId,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct InlineEdge {
    margin: f32,
    border: f32,
    padding: f32,
}

impl InlineEdge {
    fn advance(self) -> f32 {
        self.margin + self.border + self.padding
    }

    fn border_padding(self) -> f32 {
        self.border + self.padding
    }
}

#[derive(Clone, Copy, Debug)]
struct InlineOwnerBox {
    owner: NodeId,
    start: usize,
    end: usize,
    start_edge: InlineEdge,
    end_edge: InlineEdge,
    start_event: usize,
    end_event: usize,
}

#[derive(Clone, Copy, Debug)]
struct InlineBoundaryEvent {
    owner: NodeId,
    position: usize,
    is_start: bool,
    edge: InlineEdge,
}

#[derive(Clone, Copy, Debug)]
struct ActiveInlineOwner {
    owner: NodeId,
    start: usize,
    start_edge: InlineEdge,
    end_edge: InlineEdge,
    start_event: usize,
}

#[derive(Clone, Copy, Debug)]
struct RelativeOwnerTextRange {
    start: usize,
    end: usize,
    offset: (f32, f32),
}

/// One ordinary inline owner's horizontal continuation on a finalized visual
/// line. DOM layout supplies the owner's font box and decorations around this
/// baseline-relative shaped extent.
#[derive(Clone, Copy, Debug)]
pub(crate) struct InlineOwnerLineFragment {
    pub owner: NodeId,
    pub item_index: usize,
    pub line_index: usize,
    pub x: f32,
    pub baseline_y: f32,
    pub width: f32,
}

#[derive(Clone, Copy)]
struct MarkerPlacement {
    line_index: usize,
    x: f32,
    y: f32,
    content_end: f32,
}

/// Owns the font set and shaping caches for one render pass, plus every
/// inline formatting context discovered while building the tree. Lives in
/// [`crate::DomLayout`] so paint can rasterize the shaped glyphs.
pub struct TextEngine {
    font_system: FontSystem,
    loaded_families: HashMap<String, LoadedFamily>,
    swash: SwashCache,
    variable_swash: VariableSwashCache,
    items: Vec<InlineItem>,
    replaced: Vec<ReplacedItem>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct VariableCacheKey {
    glyph: CacheKey,
    variations: Arc<FontVariations>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct VariationIntentKey {
    font_id: cosmic_text::fontdb::ID,
    weight_bits: Option<u32>,
    optical_size_bits: Option<u32>,
    italic: bool,
    explicit: Option<Arc<FontVariations>>,
}

/// Swash's ordinary cache key intentionally has no variation coordinates.
/// Keep variable instances in a separate cache so different axis tuples can
/// never share an outline and repeated paint remains O(1) after first raster.
struct VariableSwashCache {
    context: ScaleContext,
    images: HashMap<VariableCacheKey, Option<SwashImage>>,
    /// Canonical coordinates supported by the face actually selected for a
    /// glyph. This is deliberately separate from authored intent: unsupported
    /// axes and values that clamp to the same endpoint share raster entries.
    instances: HashMap<VariationIntentKey, Option<Arc<FontVariations>>>,
}

impl VariableSwashCache {
    fn new() -> Self {
        Self {
            context: ScaleContext::new(),
            images: HashMap::new(),
            instances: HashMap::new(),
        }
    }

    fn effective_variations(
        &mut self,
        font_system: &mut FontSystem,
        font_id: cosmic_text::fontdb::ID,
        weight: Option<f32>,
        optical_size: Option<f32>,
        italic: bool,
        explicit: Option<Arc<FontVariations>>,
    ) -> Option<Arc<FontVariations>> {
        let key = VariationIntentKey {
            font_id,
            weight_bits: weight
                .filter(|value| value.is_finite())
                .map(|value| (value + 0.0).to_bits()),
            optical_size_bits: optical_size
                .filter(|value| value.is_finite())
                .map(|value| (value + 0.0).to_bits()),
            italic,
            explicit,
        };
        if let Some(cached) = self.instances.get(&key) {
            return cached.clone();
        }

        let resolved = font_system.get_font(font_id).and_then(|font| {
            let swash_font = font.as_swash();
            let has_ital = swash_font
                .variations()
                .any(|axis| axis.tag() == swash::tag_from_bytes(b"ital"));
            let mut variations = FontVariations::new();
            for axis in swash_font.variations() {
                let tag = axis.tag();
                let explicit_value = key.explicit.as_ref().and_then(|settings| {
                    settings
                        .iter()
                        .find(|setting| swash::tag_from_bytes(setting.tag.as_bytes()) == tag)
                        .map(|setting| setting.value.0)
                });
                let automatic_value = if tag == swash::tag_from_bytes(b"wght") {
                    key.weight_bits.map(f32::from_bits)
                } else if tag == swash::tag_from_bytes(b"opsz") {
                    key.optical_size_bits.map(f32::from_bits)
                } else if italic && tag == swash::tag_from_bytes(b"ital") {
                    Some(1.0)
                } else if italic && !has_ital && tag == swash::tag_from_bytes(b"slnt") {
                    Some(-14.0)
                } else {
                    None
                };
                let Some(value) = explicit_value.or(automatic_value) else {
                    continue;
                };
                if !value.is_finite() {
                    continue;
                }
                let value = value.clamp(axis.min_value(), axis.max_value()) + 0.0;
                variations.set(VariationTag::new(&tag.to_be_bytes()), value);
            }
            (!variations.is_empty()).then(|| Arc::new(variations))
        });
        self.instances.insert(key, resolved.clone());
        resolved
    }

    fn with_pixels<F: FnMut(i32, i32, Color)>(
        &mut self,
        font_system: &mut FontSystem,
        cache_key: CacheKey,
        variations: Arc<FontVariations>,
        base: Color,
        mut f: F,
    ) {
        let render_variations = Arc::clone(&variations);
        let key = VariableCacheKey {
            glyph: cache_key,
            variations,
        };
        let image = self.images.entry(key).or_insert_with(|| {
            let font = font_system.get_font(cache_key.font_id)?;
            let settings = render_variations.iter().map(|variation| {
                (
                    swash::tag_from_bytes(variation.tag.as_bytes()),
                    variation.value.0,
                )
            });
            let mut scaler = self
                .context
                .builder(font.as_swash())
                .size(f32::from_bits(cache_key.font_size_bits))
                .hint(true)
                .variations(settings)
                .build();
            let offset = Vector::new(cache_key.x_bin.as_float(), cache_key.y_bin.as_float());
            Render::new(&[
                Source::ColorOutline(0),
                Source::ColorBitmap(StrikeWith::BestFit),
                Source::Outline,
            ])
            .format(Format::Alpha)
            .offset(offset)
            .transform(
                cache_key
                    .flags
                    .contains(CacheKeyFlags::FAKE_ITALIC)
                    .then(|| Transform::skew(Angle::from_degrees(14.0), Angle::from_degrees(0.0))),
            )
            .render(&mut scaler, cache_key.glyph_id)
        });
        let Some(image) = image else { return };
        let left = image.placement.left;
        let top = -image.placement.top;
        match image.content {
            SwashContent::Mask => {
                for (index, alpha) in image.data.iter().copied().enumerate() {
                    let x = index as i32 % image.placement.width as i32;
                    let y = index as i32 / image.placement.width as i32;
                    f(
                        left + x,
                        top + y,
                        Color(((alpha as u32) << 24) | base.0 & 0x00FF_FFFF),
                    );
                }
            }
            SwashContent::Color => {
                for (index, rgba) in image.data.chunks_exact(4).enumerate() {
                    let x = index as i32 % image.placement.width as i32;
                    let y = index as i32 / image.placement.width as i32;
                    f(
                        left + x,
                        top + y,
                        Color::rgba(rgba[0], rgba[1], rgba[2], rgba[3]),
                    );
                }
            }
            SwashContent::SubpixelMask => {}
        }
    }
}

const REPLACED_CONTEXT_BIT: usize = 1usize << (usize::BITS - 1);

#[derive(Clone, Copy)]
struct ReplacedItem {
    intrinsic_width: Option<f32>,
    intrinsic_height: Option<f32>,
    preferred_width: Option<f32>,
    preferred_height: Option<f32>,
    preferred_ratio: f32,
    min_width: Option<f32>,
    min_height: Option<f32>,
    max_width: Option<f32>,
    max_height: Option<f32>,
    /// Both intrinsic axes are absent but a real preferred ratio exists.
    /// CSS replaced sizing stretch-fits this case to a definite available
    /// inline size instead of using the 300x150 default-object contribution.
    ratio_only: bool,
    /// Definite normal-flow containing width captured before Taffy's intrinsic
    /// flex-item contribution pass. Only populated for ratio-only auto/auto
    /// replaced boxes; decoded raster dimensions never use it.
    ratio_only_available_width: Option<f32>,
    /// CSS Sizing's cyclic-percentage rule makes a proper replaced element's
    /// inline min-content contribution zero when its preferred or maximum
    /// inline size contains a percentage. The natural size still participates
    /// in max-content sizing and in the final definite layout.
    zero_inline_min_content: bool,
}

impl ReplacedItem {
    fn from_style(width: f32, height: f32, style: &LayoutStyle) -> Self {
        Self::from_intrinsic(crate::ReplacedIntrinsic::from_dimensions(width, height), style)
    }

    fn from_intrinsic(intrinsic: crate::ReplacedIntrinsic, style: &LayoutStyle) -> Self {
        let px = |dimension| match dimension {
            Dimension::Px(value) => Some(value.max(0.0)),
            _ => None,
        };
        let expression_has_percentage = |index: usize| {
            style.size_expressions[index]
                .as_deref()
                .map_or(false, |expression| expression.contains('%'))
        };
        let explicit_ratio = style
            .aspect_ratio
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
            .or_else(|| {
                intrinsic
                    .ratio
                    .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
            });
        let intrinsic_ratio = intrinsic
            .ratio
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
            .or_else(|| {
                let (width, height) = intrinsic.natural_size()?;
                (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
                    .then_some(width / height)
            })
            .unwrap_or(2.0);
        ReplacedItem {
            intrinsic_width: intrinsic
                .width
                .filter(|width| width.is_finite() && *width > 0.0),
            intrinsic_height: intrinsic
                .height
                .filter(|height| height.is_finite() && *height > 0.0),
            preferred_width: px(style.width),
            preferred_height: px(style.height),
            preferred_ratio: explicit_ratio.unwrap_or(intrinsic_ratio),
            min_width: px(style.min_width),
            min_height: px(style.min_height),
            max_width: px(style.max_width),
            max_height: px(style.max_height),
            ratio_only: intrinsic.width.is_none()
                && intrinsic.height.is_none()
                && explicit_ratio.is_some(),
            ratio_only_available_width: style.ratio_only_available_width,
            zero_inline_min_content: matches!(style.width, Dimension::Percent(_))
                || matches!(style.max_width, Dimension::Percent(_))
                || expression_has_percentage(0)
                || expression_has_percentage(4),
        }
    }

    fn clamp(value: f32, min: Option<f32>, max: Option<f32>) -> f32 {
        // CSS sizing gives the minimum precedence when min > max.
        let value = max.map_or(value, |max| value.min(max));
        min.map_or(value, |min| value.max(min))
    }

    /// Apply the CSS 2.1 10.4/10.7 constraint table for a replaced element
    /// whose preferred width and height are both auto. Unlike independently
    /// clamping the two axes, this transfers a one-axis min/max constraint
    /// through the preferred aspect ratio whenever the constraints allow it.
    fn constrain_auto_size(self, tentative: taffy::Size<f32>) -> taffy::Size<f32> {
        let min_width = self.min_width.unwrap_or(0.0);
        let min_height = self.min_height.unwrap_or(0.0);
        let max_width = self.max_width.unwrap_or(f32::INFINITY).max(min_width);
        let max_height = self.max_height.unwrap_or(f32::INFINITY).max(min_height);
        let width = tentative.width;
        let height = tentative.height;

        let height_at_max_width = (max_width / self.preferred_ratio).max(min_height);
        let height_at_min_width = (min_width / self.preferred_ratio).min(max_height);
        let width_at_max_height = (max_height * self.preferred_ratio).max(min_width);
        let width_at_min_height = (min_height * self.preferred_ratio).min(max_width);

        let (width, height) = if width > max_width {
            if height > max_height {
                if max_width * height <= max_height * width {
                    (max_width, height_at_max_width)
                } else {
                    (width_at_max_height, max_height)
                }
            } else {
                (max_width, height_at_max_width)
            }
        } else if width < min_width {
            if height < min_height {
                if min_width * height <= min_height * width {
                    (width_at_min_height, min_height)
                } else {
                    (min_width, height_at_min_width)
                }
            } else {
                (min_width, height_at_min_width)
            }
        } else if height > max_height {
            (width_at_max_height, max_height)
        } else if height < min_height {
            (width_at_min_height, min_height)
        } else {
            (width, height)
        };

        taffy::Size { width, height }
    }

    fn size(self, known: taffy::Size<Option<f32>>) -> taffy::Size<f32> {
        let (width, height) = match (known.width, known.height) {
            (Some(width), Some(height)) => (width, height),
            (Some(width), None) => (width, width / self.preferred_ratio),
            (None, Some(height)) => (height * self.preferred_ratio, height),
            (None, None) => match (self.preferred_width, self.preferred_height) {
                (Some(width), Some(height)) => (width, height),
                (Some(width), None) => (width, width / self.preferred_ratio),
                (None, Some(height)) => (height * self.preferred_ratio, height),
                (None, None) => match (self.intrinsic_width, self.intrinsic_height) {
                    (Some(width), Some(height)) => (width, height),
                    (Some(width), None) => (width, width / self.preferred_ratio),
                    (None, Some(height)) => (height * self.preferred_ratio, height),
                    // Contain the intrinsic ratio inside CSS Images' 300x150
                    // default object size. A definite authored/known axis is
                    // handled above and transfers through the same ratio.
                    (None, None) => {
                        let width = if self.preferred_ratio >= 2.0 {
                            300.0
                        } else {
                            150.0 * self.preferred_ratio
                        };
                        (width, width / self.preferred_ratio)
                    }
                },
            },
        };
        let tentative = taffy::Size { width, height };
        if self.preferred_width.is_none()
            && self.preferred_height.is_none()
            && (known.width.is_none() || known.height.is_none())
        {
            self.constrain_auto_size(tentative)
        } else {
            taffy::Size {
                width: Self::clamp(width, self.min_width, self.max_width),
                height: Self::clamp(height, self.min_height, self.max_height),
            }
        }
    }
}

pub(crate) fn constrained_auto_replaced_size(
    width: f32,
    height: f32,
    style: &LayoutStyle,
) -> taffy::Size<f32> {
    ReplacedItem::from_style(width, height, style).size(taffy::Size {
        width: None,
        height: None,
    })
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEngine {
    pub fn new() -> Self {
        Self::new_with_fonts(&[])
    }

    pub fn new_with_fonts(fonts: &[Vec<u8>]) -> Self {
        let fonts: Vec<_> = fonts
            .iter()
            .map(|data| WebFont {
                data: data.clone(),
                family: None,
                weight: None,
                italic: None,
            })
            .collect();
        Self::new_with_web_fonts(&fonts)
    }

    pub(crate) fn new_with_web_fonts(fonts: &[WebFont]) -> Self {
        Self::new_with_web_fonts_and_emoji(fonts, false)
    }

    pub(crate) fn new_with_web_fonts_and_emoji(fonts: &[WebFont], load_emoji: bool) -> Self {
        // Build a database from embedded and page-provided faces. Never call
        // load_system_fonts: a host's font set would make layout differ
        // machine to machine and add a multi-millisecond startup scan.
        let mut db = cosmic_text::fontdb::Database::new();
        let mut declarations = Vec::new();
        for bytes in [
            SANS_R, SANS_B, SANS_O, SANS_BO, SERIF_R, SERIF_B, SERIF_O, SERIF_BO, MONO_R, MONO_B,
            MONO_O, MONO_BO, SYSTEM_R, SYSTEM_B,
        ] {
            for id in db.load_font_source(cosmic_text::fontdb::Source::Binary(Arc::new(bytes))) {
                declarations.push((id, None, None, None));
            }
        }
        if load_emoji {
            for id in db.load_font_source(cosmic_text::fontdb::Source::Binary(Arc::new(EMOJI_R))) {
                declarations.push((id, None, None, None));
            }
        }
        for font in fonts {
            for id in db.load_font_source(cosmic_text::fontdb::Source::Binary(Arc::new(
                font.data.clone(),
            ))) {
                declarations.push((id, font.family.clone(), font.weight, font.italic));
            }
        }
        let mut loaded_families = HashMap::new();
        for (id, declared_family, declared_weight, declared_italic) in declarations {
            let Some(face) = db.face(id) else { continue };
            let names = face.families.clone();
            let internal_name = names
                .first()
                .map(|(name, _)| Arc::<str>::from(name.as_str()))
                .unwrap_or_else(|| Arc::from(FAMILY));
            let shape_weight = face.weight.0;
            let metrics = font_metrics(&db, id)
                .unwrap_or_else(|| bundled_face_metrics(internal_name.as_ref()));
            let italic = declared_italic
                .unwrap_or(!matches!(face.style, cosmic_text::fontdb::Style::Normal));
            let weight = declared_weight.unwrap_or((shape_weight, shape_weight));
            let declared_names: Vec<String> = declared_family
                .map(|name| vec![name])
                .unwrap_or_else(|| names.into_iter().map(|(name, _)| name).collect());
            for name in declared_names {
                let family = loaded_families
                    .entry(name.to_ascii_lowercase())
                    .or_insert_with(|| LoadedFamily { faces: Vec::new() });
                family.faces.push(LoadedFace {
                    name: Arc::clone(&internal_name),
                    font_id: Some(id),
                    metrics,
                    min_weight: weight.0,
                    max_weight: weight.1,
                    italic,
                });
            }
        }
        db.set_sans_serif_family(FAMILY);
        let font_system = FontSystem::new_with_locale_and_db("en-US".to_string(), db);
        TextEngine {
            font_system,
            loaded_families,
            swash: SwashCache::new(),
            variable_swash: VariableSwashCache::new(),
            items: Vec::new(),
            replaced: Vec::new(),
        }
    }

    /// Number of inline formatting contexts collected (for debug/stats).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Raw selected-font box used by ordinary inline fragments. This is
    /// deliberately not CSS `line-height`: leading belongs to the containing
    /// line and must not enlarge backgrounds, borders, or DOM client rects.
    pub(crate) fn inline_font_box_height(&self, style: &LayoutStyle) -> f32 {
        let (ascent, descent) = self.inline_font_box_metrics(style);
        ascent + descent
    }

    pub(crate) fn inline_font_box_metrics(&self, style: &LayoutStyle) -> (f32, f32) {
        let font = resolve_loaded_font(
            style.font_family.as_deref(),
            crate::style::used_font_weight(style),
            style.font_style_italic.unwrap_or(false),
            &self.loaded_families,
        );
        fitted_font_box_metrics(style.font_size.unwrap_or(16.0), font.metrics)
    }

    /// Used line-height for the same selected face. Kept beside
    /// [`inline_font_box_height`](Self::inline_font_box_height) so layout can
    /// distribute leading around the raw fragment using one font decision.
    pub(crate) fn selected_line_height(&self, style: &LayoutStyle) -> f32 {
        let font = resolve_loaded_font(
            style.font_family.as_deref(),
            crate::style::used_font_weight(style),
            style.font_style_italic.unwrap_or(false),
            &self.loaded_families,
        );
        used_line_height_for_font(style, &font)
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn item_text(&self, idx: usize) -> String {
        self.items[idx]
            .buffer
            .lines
            .iter()
            .map(|line| line.text())
            .collect()
    }

    /// Build an inline formatting context for `id`'s subtree if it is one we
    /// can collapse to shaped text (see [`is_pure_text_ifc`]); returns the
    /// item index to store as the leaf's taffy context. `None` means the
    /// container is not a pure-text IFC and should build through the normal
    /// (block / flex / word-split) path.
    pub fn try_build(
        &mut self,
        tree: &DomTree,
        id: NodeId,
        styles: &std::collections::HashMap<NodeId, LayoutStyle>,
    ) -> Option<usize> {
        if !is_pure_text_ifc(tree, id, styles) {
            return None;
        }
        let base = styles.get(&id)?;
        let mut collector = Collector::new();
        let font = resolve_loaded_font(
            base.font_family.as_deref(),
            crate::style::used_font_weight(base),
            base.font_style_italic.unwrap_or(false),
            &self.loaded_families,
        );
        let ctx = base_span_ctx(base, font, &mut collector);
        let line_height = ctx.line_height;
        let mut spans: Vec<(String, SpanAttrs)> = Vec::new();
        collect_spans(
            tree,
            id,
            styles,
            ctx,
            &mut spans,
            &mut collector,
            &self.loaded_families,
        );
        self.push_shaped_item(
            base,
            line_height,
            spans,
            collector.clip_fills,
            collector.owner_ranges,
            collector.owner_boxes,
            collector.boundary_events,
        )
    }

    /// Build an inline formatting context from a *run* of consecutive
    /// inline-level siblings inside `parent` (a mixed-content block whose
    /// other children are block-level). The run folds to one shaped buffer
    /// exactly like a whole-container IFC, using the parent's style as the
    /// base. Returns `None` when any node in the run cannot fold (atomic
    /// inline, replaced element, ...) or the run has no visible text; the
    /// caller then falls back to the flex-wrap wrapper for that run.
    pub fn try_build_run(
        &mut self,
        tree: &DomTree,
        parent: NodeId,
        run: &[NodeId],
        styles: &std::collections::HashMap<NodeId, LayoutStyle>,
    ) -> Option<usize> {
        let mut has_text = false;
        for &cid in run {
            if !inline_child_ok(tree, cid, styles, &mut has_text) {
                return None;
            }
        }
        if !has_text {
            return None;
        }
        let base = styles.get(&parent)?;
        let mut collector = Collector::new();
        let font = resolve_loaded_font(
            base.font_family.as_deref(),
            crate::style::used_font_weight(base),
            base.font_style_italic.unwrap_or(false),
            &self.loaded_families,
        );
        let ctx = base_span_ctx(base, font, &mut collector);
        let line_height = ctx.line_height;
        let mut spans: Vec<(String, SpanAttrs)> = Vec::new();
        for &cid in run {
            collect_node_spans(
                tree,
                cid,
                styles,
                ctx.clone(),
                &mut spans,
                &mut collector,
                &self.loaded_families,
            );
        }
        self.push_shaped_item(
            base,
            line_height,
            spans,
            collector.clip_fills,
            collector.owner_ranges,
            collector.owner_boxes,
            collector.boundary_events,
        )
    }

    /// Shape generated text that owns a positioned pseudo box.
    ///
    /// Positioned `::before`/`::after` boxes do not participate in the taffy
    /// tree, but their text still uses the same authored webfonts, variable
    /// weight selection, transformations, and glyph rasterizer as an ordinary
    /// inline formatting context. The caller measures/finalizes/paints the
    /// returned item immediately against the pseudo's resolved content box.
    pub(crate) fn push_generated_text(&mut self, text: &str, style: &LayoutStyle) -> Option<usize> {
        let mut collector = Collector::new();
        let font = resolve_loaded_font(
            style.font_family.as_deref(),
            crate::style::used_font_weight(style),
            style.font_style_italic.unwrap_or(false),
            &self.loaded_families,
        );
        let context = base_span_ctx(style, font, &mut collector);
        let line_height = context.line_height;
        let attrs = SpanAttrs {
            font_size: context.font_size,
            line_height: context.line_height,
            letter_spacing: context.letter_spacing,
            letter_spacing_non_normal: context.letter_spacing_non_normal,
            weight: context.weight,
            optical_sizing: context.optical_sizing,
            font_id: context.font_id,
            variations: context.variations.clone(),
            italic: context.italic,
            synthetic_italic: context.synthetic_italic,
            underline: context.underline,
            color: context.color,
            family: context.family,
            clip_fill: context.clip_fill,
            white_space: context.white_space,
            overflow_wrap: context.overflow_wrap,
            word_break: context.word_break,
        };
        let mut spans = Vec::new();
        push_text(
            text,
            context.transform,
            context.white_space,
            &attrs,
            &mut spans,
            &mut collector,
        );
        self.push_shaped_item(
            style,
            line_height,
            spans,
            collector.clip_fills,
            collector.owner_ranges,
            collector.owner_boxes,
            collector.boundary_events,
        )
    }

    /// Shared tail of [`try_build`] / [`try_build_run`]: shape the collected
    /// spans into a cosmic-text buffer under `base`'s font metrics and
    /// alignment, and store it as a new inline item.
    fn push_shaped_item(
        &mut self,
        base: &LayoutStyle,
        line_h: f32,
        mut spans: Vec<(String, SpanAttrs)>,
        clip_fills: Vec<ClipTextFill>,
        mut owner_ranges: Vec<OwnerTextRange>,
        mut owner_boxes: Vec<InlineOwnerBox>,
        mut boundary_events: Vec<InlineBoundaryEvent>,
    ) -> Option<usize> {
        let white_space = base.white_space.unwrap_or_default();
        let layout_wrap = if spans
            .iter()
            .any(|(_, attrs)| attrs.has_layout_emergency_breaks())
        {
            Wrap::WordOrGlyph
        } else {
            Wrap::Word
        };
        let min_content_wrap = if spans
            .iter()
            .any(|(_, attrs)| attrs.has_min_content_emergency_breaks())
        {
            Wrap::WordOrGlyphMinContent
        } else {
            Wrap::Word
        };
        // Collapsible trailing whitespace does not widen the last line.
        if matches!(
            white_space,
            crate::WhiteSpace::Normal | crate::WhiteSpace::NoWrap | crate::WhiteSpace::PreLine
        ) {
            if let Some((text, _)) = spans.last_mut() {
                if text.ends_with(' ') {
                    text.pop();
                }
            }
        }
        if owner_boxes.is_empty()
            && spans.iter().all(|(text, _)| {
                text.is_empty()
                    || (matches!(
                        white_space,
                        crate::WhiteSpace::Normal
                            | crate::WhiteSpace::NoWrap
                            | crate::WhiteSpace::PreLine
                    ) && text.trim().is_empty()
                        && !text.contains('\n'))
            })
        {
            return None;
        }
        let text_len = spans.iter().map(|(text, _)| text.len()).sum::<usize>();
        owner_ranges.retain_mut(|range| {
            range.start = range.start.min(text_len);
            range.end = range.end.min(text_len);
            range.start < range.end
        });
        for owner in &mut owner_boxes {
            owner.start = owner.start.min(text_len);
            owner.end = owner.end.min(text_len);
        }
        for event in &mut boundary_events {
            event.position = event.position.min(text_len);
        }
        let owner_text = (!owner_boxes.is_empty()).then(|| {
            let mut text = String::with_capacity(text_len);
            for (span, _) in &spans {
                text.push_str(span);
            }
            text
        });

        let base_size = base.font_size.unwrap_or(16.0);
        // Explicit line-height stays fractional. `normal` is derived from the
        // embedded face metrics with the same per-component pixel fitting as
        // Chromium's Linux font path.
        let mut forced_breaks = 0usize;
        let mut visible_after_last_break = false;
        for (text, _) in &spans {
            for ch in text.chars() {
                if ch == '\n' {
                    forced_breaks += 1;
                    visible_after_last_break = false;
                } else if !ch.is_whitespace() {
                    visible_after_last_break = true;
                }
            }
        }
        let forced_lines =
            forced_breaks + usize::from(forced_breaks > 0 && visible_after_last_break);
        let forced_lines = if owner_boxes.is_empty() {
            forced_lines
        } else {
            forced_lines.max(1)
        };
        let forced_min_height = forced_lines as f32 * line_h.max(1.0);
        // cosmic-text asserts (an uncatchable process abort) if font size OR
        // line height is 0. `font-size:0` is a common whitespace-collapse trick
        // and drives both to 0, so floor both at 1px here. The glyphs stay
        // ~invisible, matching the intent, and one page can never abort a worker.
        let cosmic_size = base_size.max(1.0);
        let metrics = Metrics::new(cosmic_size, line_h.max(1.0));
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        // Install the used-layout mode now; intrinsic measurement temporarily
        // swaps to `min_content_wrap` below. Keeping those modes separate is
        // what prevents `overflow-wrap: break-word` from shrinking a
        // table/flex min-content contribution while still permitting an
        // emergency break once the actual line width is known.
        buffer.set_wrap(&mut self.font_system, layout_wrap);
        // Always Advanced shaping: Basic mis-maps per-span attribute
        // boundaries in the shaping backend (a multi-color run like body text
        // with links ends up coloring the wrong glyphs), and shaping is a
        // small fraction of a page's CSS-cascade-dominated render time.
        let mut variation_sets = Vec::<Arc<FontVariations>>::new();
        for (_, attrs) in &spans {
            if let Some(variations) = attrs.variations.as_ref() {
                if !variation_sets
                    .iter()
                    .any(|existing| **existing == **variations)
                {
                    variation_sets.push(Arc::clone(variations));
                }
            }
        }
        let line_clamp = (base.webkit_box_display.is_some() && base.webkit_box_orient_vertical)
            .then_some(base.webkit_line_clamp)
            .flatten()
            .map(|lines| lines as usize);
        let ellipsis_overflow =
            base.text_overflow == crate::TextOverflow::Ellipsis && base.clips_overflow_x();
        let marker_attrs = (line_clamp.is_some() || ellipsis_overflow)
            .then(|| spans.last().map(|(_, attrs)| attrs.clone()))
            .flatten();
        let rich = spans.iter().map(|(text, attrs)| {
            let variation_index = attrs
                .variations
                .as_ref()
                .map(|variations| {
                    variation_sets
                        .iter()
                        .position(|existing| **existing == **variations)
                        .expect("collected span variation set")
                        + 1
                })
                .unwrap_or(0);
            (text.as_str(), attrs.to_attrs(variation_index))
        });
        let defaults = Attrs::new().family(Family::Name(FAMILY));
        buffer.set_rich_text(
            &mut self.font_system,
            rich,
            &defaults,
            Shaping::Advanced,
            None,
        );

        let marker_buffer = marker_attrs.map(|attrs| {
            let variation_index = attrs
                .variations
                .as_ref()
                .and_then(|variations| {
                    variation_sets
                        .iter()
                        .position(|existing| **existing == **variations)
                })
                .map_or(0, |index| index + 1);
            let mut marker = Buffer::new(&mut self.font_system, metrics);
            marker.set_wrap(&mut self.font_system, Wrap::None);
            let marker_attrs = attrs.to_attrs(variation_index);
            marker.set_rich_text(
                &mut self.font_system,
                [("…", marker_attrs)],
                &Attrs::new().family(Family::Name(FAMILY)),
                Shaping::Advanced,
                None,
            );
            marker.set_size(&mut self.font_system, None, None);
            marker.shape_until_scroll(&mut self.font_system, false);
            marker
        });

        let align = match base.text_align {
            Some(taffy::AlignItems::CENTER) => Some(Align::Center),
            Some(taffy::AlignItems::FLEX_END) => Some(Align::End),
            _ => None,
        };
        if let Some(a) = align {
            for line in buffer.lines.iter_mut() {
                line.set_align(Some(a));
            }
        }

        let idx = self.items.len();
        let text_indent = base.text_indent.unwrap_or(Dimension::Px(0.0));
        let source_buffer = (!matches!(text_indent, Dimension::Px(value) if value == 0.0)
            || !boundary_events.is_empty())
        .then(|| buffer.clone());
        self.items.push(InlineItem {
            buffer,
            layout_wrap,
            min_content_wrap,
            source_buffer,
            text_indent,
            first_line_offset: 0.0,
            balance_wrap: base.text_wrap_style == Some(crate::TextWrapStyle::Balance),
            align,
            forced_min_height,
            origin: (0.0, 0.0),
            clip: None,
            clip_fills,
            variation_sets,
            line_clamp,
            ellipsis_overflow,
            marker_buffer,
            marker: None,
            owner_text,
            owner_ranges,
            owner_boxes,
            boundary_events,
            relative_owner_ranges: Vec::new(),
        });
        Some(idx)
    }

    /// Measure the inline context `idx` at `width` (content-box width, or
    /// None for max-content), returning its shaped (width, height). Called by
    /// taffy's measure function during layout.
    pub fn measure(&mut self, idx: usize, width: Option<f32>) -> (f32, f32) {
        if idx & REPLACED_CONTEXT_BIT != 0 {
            let size = self.replaced[idx & !REPLACED_CONTEXT_BIT].size(taffy::Size {
                width,
                height: None,
            });
            return (size.width, size.height);
        }
        let wrap = self.items[idx].layout_wrap;
        self.measure_text_with_wrap(idx, width, wrap)
    }

    fn measure_text_with_wrap(&mut self, idx: usize, width: Option<f32>, wrap: Wrap) -> (f32, f32) {
        let TextEngine {
            font_system, items, ..
        } = self;
        let item = &mut items[idx];
        shape_with_text_indent(font_system, item, width, wrap);
        let (width, height, clamped) = buffer_size(item);
        (
            width,
            if clamped {
                height
            } else {
                height.max(item.forced_min_height)
            },
        )
    }

    /// Exact max-content size for one fallback word item. Paragraph IFCs keep
    /// their historical integer-ceiled intrinsic width, but word boxes need
    /// the selected webfont's fractional advance or every token accumulates a
    /// pixel of horizontal drift against browser geometry.
    pub(crate) fn measure_word(&mut self, idx: usize) -> (f32, f32) {
        let TextEngine {
            font_system, items, ..
        } = self;
        let Some(item) = items.get_mut(idx) else {
            return (0.0, 0.0);
        };
        let wrap = item.layout_wrap;
        shape_with_text_indent(font_system, item, None, wrap);
        let mut width = 0.0f32;
        let mut height = 0.0f32;
        let starts = item
            .owner_text
            .as_deref()
            .map(|source| source_line_starts(&item.buffer, source))
            .unwrap_or_default();
        for (line_index, run) in item.buffer.layout_runs().enumerate() {
            let offset = if line_index == 0 {
                item.first_line_offset
            } else {
                0.0
            };
            let line_start = starts.get(run.line_i).copied().unwrap_or(0);
            let line_end = line_start + run.text.len();
            width = width.max(
                (run.line_w + offset + line_edge_advance(item, line_start, line_end)).max(0.0),
            );
            height = height.max(run.line_top + run.line_height);
        }
        (width, height.max(item.forced_min_height))
    }

    /// Register a replaced element's intrinsic size as a taffy measure
    /// context. Percentage-sized image leaves still need their intrinsic
    /// max-content contribution while an auto-sized ancestor is measured.
    pub fn register_replaced(&mut self, width: f32, height: f32, style: &LayoutStyle) -> usize {
        self.register_replaced_intrinsic(
            crate::ReplacedIntrinsic::from_dimensions(width, height),
            style,
        )
    }

    pub(crate) fn register_replaced_intrinsic(
        &mut self,
        intrinsic: crate::ReplacedIntrinsic,
        style: &LayoutStyle,
    ) -> usize {
        let index = self.replaced.len();
        self.replaced
            .push(ReplacedItem::from_intrinsic(intrinsic, style));
        REPLACED_CONTEXT_BIT | index
    }

    /// Measure either a shaped text context or an intrinsic replaced element.
    /// Replaced boxes transfer a definite axis through their intrinsic ratio;
    /// with neither axis definite they contribute their natural size.
    pub fn measure_taffy(
        &mut self,
        idx: usize,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
    ) -> taffy::Size<f32> {
        if idx & REPLACED_CONTEXT_BIT != 0 {
            let replaced = self.replaced[idx & !REPLACED_CONTEXT_BIT];
            let stretch_width = (replaced.ratio_only
                && replaced.preferred_width.is_none()
                && replaced.preferred_height.is_none()
                && known.width.is_none())
            .then(|| {
                match available.width {
                    taffy::AvailableSpace::Definite(width) => Some(width.max(0.0)),
                    _ => replaced.ratio_only_available_width,
                }
            })
            .flatten();
            let mut size = replaced.size(taffy::Size {
                width: known.width.or(stretch_width),
                height: known.height,
            });
            if replaced.zero_inline_min_content
                && known.width.is_none()
                && matches!(available.width, taffy::AvailableSpace::MinContent)
            {
                size.width = 0.0;
            }
            return size;
        }
        let min_content_query =
            known.width.is_none() && matches!(available.width, taffy::AvailableSpace::MinContent);
        let width = known.width.or(match available.width {
            taffy::AvailableSpace::Definite(width) => Some(width),
            taffy::AvailableSpace::MinContent => Some(0.0),
            taffy::AvailableSpace::MaxContent => None,
        });
        let wrap = if min_content_query {
            self.items[idx].min_content_wrap
        } else {
            self.items[idx].layout_wrap
        };
        let (width, height) = self.measure_text_with_wrap(idx, width, wrap);
        taffy::Size { width, height }
    }

    /// After layout, pin each context to its final content-box origin and
    /// clip, reshaping once at the resolved width so paint draws the same
    /// line breaks the box was sized for.
    pub fn finalize(
        &mut self,
        idx: usize,
        content_origin: (f32, f32),
        content_width: f32,
        clip: Option<Rect>,
    ) {
        let TextEngine {
            font_system, items, ..
        } = self;
        let item = &mut items[idx];
        let content_width = content_width.max(0.0);
        let wrap = item.layout_wrap;
        shape_with_text_indent(font_system, item, Some(content_width), wrap);
        let balanced_width = item
            .balance_wrap
            .then(|| balance_wrap_width(font_system, &mut item.buffer, content_width))
            .flatten()
            .unwrap_or(content_width);
        let alignment_inset = (content_width - balanced_width).max(0.0)
            * match item.align {
                Some(Align::Center) => 0.5,
                Some(Align::End | Align::Right) => 1.0,
                _ => 0.0,
            };
        item.marker = None;
        let marker_width = item
            .marker_buffer
            .as_ref()
            .and_then(|buffer| buffer.layout_runs().next().map(|run| run.line_w));
        if let Some(marker_width) = marker_width {
            let mut nonempty = 0usize;
            let mut clamp_target = None;
            let mut clamp_has_following_line = false;
            let mut overflow_target = None;
            for (line_index, run) in item.buffer.layout_runs().enumerate() {
                if run.glyphs.is_empty() {
                    continue;
                }
                nonempty += 1;
                let line_offset = if line_index == 0 {
                    item.first_line_offset
                } else {
                    0.0
                };
                let content_end = run
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.x + glyph.w + line_offset)
                    .fold(0.0f32, f32::max);
                if item.line_clamp == Some(nonempty) {
                    clamp_target = Some((line_index, run.line_y, content_end));
                } else if item.line_clamp.is_some_and(|limit| nonempty > limit) {
                    clamp_has_following_line = true;
                }
                if overflow_target.is_none()
                    && item.ellipsis_overflow
                    && content_end > content_width + 0.01
                {
                    overflow_target = Some((line_index, run.line_y, content_end));
                }
            }
            let target = if clamp_has_following_line {
                clamp_target.map(|target| (target, true))
            } else {
                overflow_target.map(|target| (target, false))
            };
            if let Some(((line_index, line_y, natural_end), is_clamp)) = target {
                let available_marker_start = (content_width - marker_width).max(0.0);
                let marker_x = if is_clamp {
                    natural_end.min(available_marker_start)
                } else {
                    available_marker_start
                };
                let marker_line_y = item
                    .marker_buffer
                    .as_ref()
                    .and_then(|buffer| buffer.layout_runs().next().map(|run| run.line_y))
                    .unwrap_or(0.0);
                item.marker = Some(MarkerPlacement {
                    line_index,
                    x: marker_x,
                    y: line_y - marker_line_y,
                    content_end: marker_x,
                });
            }
        }
        item.origin = (content_origin.0 + alignment_inset, content_origin.1);
        item.clip = clip;
    }

    /// Replace only the finalized clip without reshaping. Used when canonical
    /// inline fragment rects make a second clip/transform tree walk necessary.
    pub(crate) fn set_clip(&mut self, idx: usize, clip: Option<Rect>) {
        if let Some(item) = self.items.get_mut(idx) {
            item.clip = clip;
        }
    }

    pub(crate) fn has_inline_owners(&self) -> bool {
        self.items.iter().any(|item| !item.owner_boxes.is_empty())
    }

    /// Install cumulative used offsets for relative ordinary-inline owners.
    /// DOM resolves percentages against the real containing block. Selecting
    /// the deepest matching range at paint time is therefore sufficient even
    /// for nested relative inlines: its offset already includes its ancestors.
    pub(crate) fn set_inline_owner_offsets(&mut self, offsets: &HashMap<NodeId, (f32, f32)>) {
        for item in &mut self.items {
            item.relative_owner_ranges = item
                .owner_ranges
                .iter()
                .filter_map(|range| {
                    let offset = offsets.get(&range.owner).copied()?;
                    (offset != (0.0, 0.0)).then_some(RelativeOwnerTextRange {
                        start: range.start,
                        end: range.end,
                        offset,
                    })
                })
                .collect();
        }
    }

    /// Derive ordinary inline continuation extents from finalized shaping.
    /// Provenance is a sidecar rather than glyph metadata, so changing DOM
    /// owners never creates a font-shaping boundary or disables ligatures.
    pub(crate) fn inline_owner_line_fragments(&self) -> Vec<InlineOwnerLineFragment> {
        let mut out: Vec<InlineOwnerLineFragment> = Vec::new();
        for (item_index, item) in self.items.iter().enumerate() {
            let Some(source) = item.owner_text.as_deref() else {
                continue;
            };
            let line_starts = source_line_starts(&item.buffer, source);

            for (line_index, run) in item.buffer.layout_runs().enumerate() {
                let Some(&line_start) = line_starts.get(run.line_i) else {
                    continue;
                };
                let line_end = line_start.saturating_add(run.text.len());
                let first_line_offset = if line_index == 0 {
                    item.first_line_offset
                } else {
                    0.0
                };
                let alignment_shift = line_edge_alignment_shift(item, line_start, line_end);
                for owner in &item.owner_boxes {
                    let empty = owner.start == owner.end;
                    let intersects = if empty {
                        (owner.start >= line_start && owner.start < line_end)
                            || (line_end == source.len() && owner.start == line_end)
                    } else {
                        owner.start < line_end && owner.end > line_start
                    };
                    if !intersects {
                        continue;
                    }
                    let first = owner.start >= line_start && owner.start < line_end || empty;
                    let last = owner.end > line_start && owner.end <= line_end || empty;
                    let raw_left = if first {
                        run_cursor_x(&run, owner.start.saturating_sub(line_start))
                            + line_advance_before_event(
                                item,
                                owner.start_event,
                                line_start,
                                line_end,
                            )
                            + owner.start_edge.margin
                    } else {
                        run_cursor_x(&run, 0)
                    };
                    let raw_right = if last {
                        run_cursor_x(&run, owner.end.saturating_sub(line_start))
                            + line_advance_before_event(item, owner.end_event, line_start, line_end)
                            + owner.end_edge.border_padding()
                    } else {
                        run.line_w + line_edge_advance(item, line_start, line_end)
                    };
                    let x = item.origin.0 + first_line_offset + alignment_shift + raw_left;
                    let width = (raw_right - raw_left).max(0.0);
                    let baseline_y = item.origin.1 + run.line_y;
                    if let Some(existing) = out.iter_mut().find(|fragment| {
                        fragment.owner == owner.owner
                            && fragment.item_index == item_index
                            && fragment.line_index == line_index
                    }) {
                        let left = existing.x.min(x);
                        let right = (existing.x + existing.width).max(x + width);
                        existing.x = left;
                        existing.width = (right - left).max(0.0);
                    } else {
                        out.push(InlineOwnerLineFragment {
                            owner: owner.owner,
                            item_index,
                            line_index,
                            x,
                            baseline_y,
                            width: width.max(0.0),
                        });
                    }
                }
            }
        }
        out
    }
}

/// A run of same-styled inline text.
#[derive(Clone, PartialEq)]
struct SpanAttrs {
    font_size: f32,
    line_height: f32,
    letter_spacing: f32,
    letter_spacing_non_normal: bool,
    weight: u16,
    optical_sizing: crate::FontOpticalSizing,
    font_id: Option<cosmic_text::fontdb::ID>,
    variations: Option<Arc<FontVariations>>,
    italic: bool,
    synthetic_italic: bool,
    underline: bool,
    color: [u8; 4],
    family: Arc<str>,
    clip_fill: Option<usize>,
    white_space: crate::WhiteSpace,
    overflow_wrap: crate::OverflowWrap,
    word_break: crate::WordBreak,
}

impl SpanAttrs {
    fn wrapping_enabled(&self) -> bool {
        !matches!(
            self.white_space,
            crate::WhiteSpace::NoWrap | crate::WhiteSpace::Pre
        )
    }

    fn has_layout_emergency_breaks(&self) -> bool {
        self.wrapping_enabled()
            && (self.word_break == crate::WordBreak::BreakWord
                || self.overflow_wrap != crate::OverflowWrap::Normal)
    }

    fn has_min_content_emergency_breaks(&self) -> bool {
        self.wrapping_enabled()
            && (self.word_break == crate::WordBreak::BreakWord
                || self.overflow_wrap == crate::OverflowWrap::Anywhere)
    }

    fn to_attrs(&self, variation_index: usize) -> Attrs<'_> {
        let mut a = Attrs::new().family(Family::Name(self.family.as_ref()));
        // Inline descendants keep their own computed font metrics inside the
        // enclosing line box. Without per-span metrics, an `<a>`/`<span>`
        // with a relative font-size was shaped at the block container's size
        // even though cascade had resolved the descendant correctly.
        a = a.metrics(Metrics::new(
            self.font_size.max(1.0),
            self.line_height.max(1.0),
        ));
        a = a.weight(Weight(self.weight));
        a = a.font_weight_axis(self.weight as f32);
        if self.optical_sizing == crate::FontOpticalSizing::Auto {
            a = a.font_optical_size(self.font_size);
        }
        a = a.font_italic_axis(self.italic);
        if let Some(font_id) = self.font_id {
            a = a.font_id(font_id);
        }
        a = a.style(if self.italic {
            Style::Italic
        } else {
            Style::Normal
        });
        if self.synthetic_italic {
            a = a.cache_key_flags(CacheKeyFlags::FAKE_ITALIC);
        }
        if self.letter_spacing.is_finite() && self.letter_spacing != 0.0 {
            a = a.letter_spacing(self.letter_spacing / self.font_size.max(1.0));
        }
        if self.letter_spacing_non_normal
            && self.letter_spacing.is_finite()
            && self.letter_spacing != 0.0
        {
            let mut features = FontFeatures::new();
            features
                .disable(FeatureTag::STANDARD_LIGATURES)
                .disable(FeatureTag::CONTEXTUAL_LIGATURES);
            a = a.font_features(features);
        }
        // Clip-text glyphs must be shaped with an opaque fill so their coverage
        // reaches paint; the real gradient is selected through metadata.
        let color = if self.clip_fill.is_some() {
            [255, 255, 255, 255]
        } else {
            self.color
        };
        a = a.color(Color::rgba(color[0], color[1], color[2], color[3]));
        let fill = self
            .clip_fill
            .map_or(0, |index| (index + 1) << META_FILL_SHIFT);
        debug_assert_eq!(fill & !META_FILL_MASK, 0);
        let variation = variation_index << META_VARIATION_SHIFT;
        assert_eq!(variation & !META_VARIATION_MASK, 0);
        if let Some(variations) = self.variations.as_ref() {
            a = a.font_variations((**variations).clone());
        }
        let word_break = match self.word_break {
            crate::WordBreak::Normal => CssWordBreak::Normal,
            crate::WordBreak::BreakAll => CssWordBreak::BreakAll,
            crate::WordBreak::KeepAll => CssWordBreak::KeepAll,
            crate::WordBreak::BreakWord => CssWordBreak::BreakWord,
        };
        let overflow_wrap = match self.overflow_wrap {
            crate::OverflowWrap::Normal => CssOverflowWrap::Normal,
            crate::OverflowWrap::BreakWord => CssOverflowWrap::BreakWord,
            crate::OverflowWrap::Anywhere => CssOverflowWrap::Anywhere,
        };
        a = a.css_line_break(CssLineBreak {
            wrap: self.wrapping_enabled(),
            word_break,
            overflow_wrap,
        });
        a = a.metadata(fill | variation | usize::from(self.underline));
        a
    }
}

/// Inherited inline context threaded down the subtree while collecting spans.
#[derive(Clone)]
struct SpanCtx {
    font_size: f32,
    line_height: f32,
    letter_spacing: f32,
    letter_spacing_non_normal: bool,
    color: [u8; 4],
    weight: u16,
    optical_sizing: crate::FontOpticalSizing,
    font_id: Option<cosmic_text::fontdb::ID>,
    font_metrics: FaceMetrics,
    variations: Option<Arc<FontVariations>>,
    italic: bool,
    synthetic_italic: bool,
    underline: bool,
    transform: TextTransform,
    white_space: crate::WhiteSpace,
    overflow_wrap: crate::OverflowWrap,
    word_break: crate::WordBreak,
    family: Arc<str>,
    clip_fill: Option<usize>,
}

struct Collector {
    last_was_space: bool,
    clip_fills: Vec<ClipTextFill>,
    owners: Vec<ActiveInlineOwner>,
    owner_ranges: Vec<OwnerTextRange>,
    owner_boxes: Vec<InlineOwnerBox>,
    boundary_events: Vec<InlineBoundaryEvent>,
    text_len: usize,
}

impl Collector {
    fn new() -> Self {
        Self {
            last_was_space: true,
            clip_fills: Vec::new(),
            owners: Vec::new(),
            owner_ranges: Vec::new(),
            owner_boxes: Vec::new(),
            boundary_events: Vec::new(),
            text_len: 0,
        }
    }

    fn record_text(&mut self, byte_len: usize) {
        let start = self.text_len;
        self.text_len = self.text_len.saturating_add(byte_len);
        for owner in &self.owners {
            self.owner_ranges.push(OwnerTextRange {
                owner: owner.owner,
                start,
                end: self.text_len,
            });
        }
    }

    fn begin_owner(&mut self, owner: NodeId, style: &LayoutStyle) {
        let start_edge = InlineEdge {
            margin: style.margin.left,
            border: style.border.left,
            padding: style.padding.left,
        };
        let end_edge = InlineEdge {
            margin: style.margin.right,
            border: style.border.right,
            padding: style.padding.right,
        };
        let start_event = self.boundary_events.len();
        self.boundary_events.push(InlineBoundaryEvent {
            owner,
            position: self.text_len,
            is_start: true,
            edge: start_edge,
        });
        self.owners.push(ActiveInlineOwner {
            owner,
            start: self.text_len,
            start_edge,
            end_edge,
            start_event,
        });
    }

    fn end_owner(&mut self, owner: NodeId) {
        let active = self.owners.pop().expect("balanced inline owner stack");
        debug_assert_eq!(active.owner, owner);
        let end_event = self.boundary_events.len();
        self.boundary_events.push(InlineBoundaryEvent {
            owner,
            position: self.text_len,
            is_start: false,
            edge: active.end_edge,
        });
        self.owner_boxes.push(InlineOwnerBox {
            owner,
            start: active.start,
            end: self.text_len,
            start_edge: active.start_edge,
            end_edge: active.end_edge,
            start_event: active.start_event,
            end_event,
        });
    }
}

/// DFS the inline subtree, appending whitespace-collapsed text runs. Adjacent
/// runs with identical attributes are merged so cosmic-text sees the fewest
/// spans. Collapsing spans HTML's insignificant whitespace (runs of spaces,
/// tabs, and newlines fold to one space; leading space at the start of the
/// context is dropped) exactly as `white-space: normal` requires.
/// Root span context (and background-clip-text fill, when active) for an IFC
/// whose base style is `base`.
///
/// `-webkit-background-clip: text` on a transparent-colored element paints
/// its background *through* the glyphs (gradient/solid text). When active,
/// shape the glyphs in opaque white so their coverage renders, then recolor
/// them from the background at paint time; otherwise transparent text stays
/// transparent (and invisible), unchanged.
fn resolved_font_variations(
    style: &LayoutStyle,
    _font: &ResolvedFont,
) -> Option<Arc<FontVariations>> {
    let mut variations = FontVariations::new();
    // Automatic high-level axes travel separately with every glyph so they
    // can be resolved against the face actually selected during fallback.
    // Only low-level authored settings need an allocated tuple here.
    for setting in style.font_variation_settings.as_deref().unwrap_or(&[]) {
        if setting.value.is_finite() {
            variations.set(VariationTag::new(&setting.tag), setting.value);
        }
    }
    (!variations.is_empty()).then(|| Arc::new(variations))
}

fn base_span_ctx(base: &LayoutStyle, font: ResolvedFont, collector: &mut Collector) -> SpanCtx {
    let clip_fill = clip_text_fill(base).map(|fill| {
        let index = collector.clip_fills.len();
        collector.clip_fills.push(fill);
        index
    });
    let variations = resolved_font_variations(base, &font);
    let line_height = used_line_height_for_font(base, &font);
    SpanCtx {
        font_size: base.font_size.unwrap_or(16.0),
        line_height,
        letter_spacing: base.letter_spacing.unwrap_or(0.0),
        letter_spacing_non_normal: base.letter_spacing_non_normal.unwrap_or(false),
        color: base.color.unwrap_or([0, 0, 0, 255]),
        weight: crate::style::used_font_weight(base),
        optical_sizing: base.font_optical_sizing.unwrap_or_default(),
        font_id: font.font_id,
        font_metrics: font.metrics,
        variations,
        italic: base.font_style_italic.unwrap_or(false),
        synthetic_italic: font.synthetic_italic,
        underline: base.underline.unwrap_or(false),
        transform: base.text_transform.unwrap_or(TextTransform::None),
        white_space: base.white_space.unwrap_or_default(),
        overflow_wrap: base.overflow_wrap.unwrap_or_default(),
        word_break: base.word_break.unwrap_or_default(),
        family: font.family,
        clip_fill,
    }
}

fn collect_spans(
    tree: &DomTree,
    id: NodeId,
    styles: &std::collections::HashMap<NodeId, LayoutStyle>,
    ctx: SpanCtx,
    out: &mut Vec<(String, SpanAttrs)>,
    c: &mut Collector,
    loaded_families: &HashMap<String, LoadedFamily>,
) {
    for cid in crate::dom::rendered_children(tree, id) {
        collect_node_spans(tree, cid, styles, ctx.clone(), out, c, loaded_families);
    }
}

/// Collect the spans contributed by one node (a text node's runs, or an
/// element's whole subtree with its style threaded through). Split out of
/// [`collect_spans`] so an inline *run* (a slice of siblings, not a whole
/// container) can also be folded into one shaped buffer.
fn collect_node_spans(
    tree: &DomTree,
    cid: NodeId,
    styles: &std::collections::HashMap<NodeId, LayoutStyle>,
    ctx: SpanCtx,
    out: &mut Vec<(String, SpanAttrs)>,
    c: &mut Collector,
    loaded_families: &HashMap<String, LoadedFamily>,
) {
    let Some(node) = tree.get_node(cid) else {
        return;
    };
    match &node.data {
        obscura_dom::tree::NodeData::Text { contents } => {
            let attrs = SpanAttrs {
                font_size: ctx.font_size,
                line_height: ctx.line_height,
                letter_spacing: ctx.letter_spacing,
                letter_spacing_non_normal: ctx.letter_spacing_non_normal,
                weight: ctx.weight,
                optical_sizing: ctx.optical_sizing,
                font_id: ctx.font_id,
                variations: ctx.variations.clone(),
                italic: ctx.italic,
                synthetic_italic: ctx.synthetic_italic,
                underline: ctx.underline,
                color: ctx.color,
                family: Arc::clone(&ctx.family),
                clip_fill: ctx.clip_fill,
                white_space: ctx.white_space,
                overflow_wrap: ctx.overflow_wrap,
                word_break: ctx.word_break,
            };
            push_text(contents, ctx.transform, ctx.white_space, &attrs, out, c);
        }
        _ => {
            let Some(elem) = node.as_element() else {
                return;
            };
            let style = styles.get(&cid);
            if style.map(|s| s.display == Display::None).unwrap_or(false) {
                return;
            }
            if elem.local.as_ref() == "br" {
                out.push((
                    "\n".to_string(),
                    SpanAttrs {
                        font_size: ctx.font_size,
                        line_height: ctx.line_height,
                        letter_spacing: ctx.letter_spacing,
                        letter_spacing_non_normal: ctx.letter_spacing_non_normal,
                        weight: ctx.weight,
                        optical_sizing: ctx.optical_sizing,
                        font_id: ctx.font_id,
                        variations: ctx.variations.clone(),
                        italic: ctx.italic,
                        synthetic_italic: ctx.synthetic_italic,
                        underline: ctx.underline,
                        color: ctx.color,
                        family: Arc::clone(&ctx.family),
                        clip_fill: ctx.clip_fill,
                        white_space: ctx.white_space,
                        overflow_wrap: ctx.overflow_wrap,
                        word_break: ctx.word_break,
                    },
                ));
                c.text_len = c.text_len.saturating_add(1);
                c.last_was_space = true;
                return;
            }
            let owns_inline_fragment = style
                .is_some_and(|style| style.ignores_used_box_sizes() && !style.display_contents);
            if owns_inline_fragment {
                c.begin_owner(cid, style.expect("inline owner style"));
            }
            let own_clip_fill = style.and_then(clip_text_fill).map(|fill| {
                let index = c.clip_fills.len();
                c.clip_fills.push(fill);
                index
            });
            let color = style.and_then(|s| s.color).unwrap_or(ctx.color);
            // A descendant with its own clip-text background replaces the
            // inherited fill. Transparent descendants otherwise continue an
            // ancestor's fill; an opaque text color paints normally.
            let clip_fill =
                own_clip_fill.or_else(|| if color[3] == 0 { ctx.clip_fill } else { None });
            let requested_weight = style
                .map(crate::style::used_font_weight)
                .unwrap_or(ctx.weight);
            let font = style
                .and_then(|style| style.font_family.as_deref())
                .map(|family| {
                    resolve_loaded_font(
                        Some(family),
                        requested_weight,
                        style
                            .and_then(|style| style.font_style_italic)
                            .unwrap_or(ctx.italic),
                        loaded_families,
                    )
                })
                .unwrap_or_else(|| ResolvedFont {
                    family: Arc::clone(&ctx.family),
                    font_id: ctx.font_id,
                    metrics: ctx.font_metrics,
                    synthetic_italic: ctx.synthetic_italic,
                });
            let variations = style
                .map(|style| resolved_font_variations(style, &font))
                .unwrap_or_else(|| ctx.variations.clone());
            let child = SpanCtx {
                font_size: style
                    .and_then(|style| style.font_size)
                    .unwrap_or(ctx.font_size),
                line_height: style
                    .map(|style| used_line_height_for_font(style, &font))
                    .unwrap_or(ctx.line_height),
                letter_spacing: style
                    .and_then(|style| style.letter_spacing)
                    .unwrap_or(ctx.letter_spacing),
                letter_spacing_non_normal: style
                    .and_then(|style| style.letter_spacing_non_normal)
                    .unwrap_or(ctx.letter_spacing_non_normal),
                color,
                weight: requested_weight,
                optical_sizing: style
                    .and_then(|style| style.font_optical_sizing)
                    .unwrap_or(ctx.optical_sizing),
                font_id: font.font_id,
                font_metrics: font.metrics,
                variations,
                italic: ctx.italic || style.and_then(|s| s.font_style_italic).unwrap_or(false),
                synthetic_italic: font.synthetic_italic,
                // Underline propagates in: an ancestor's underline covers
                // descendant text; an element only sets its own via CSS.
                underline: ctx.underline || style.and_then(|s| s.underline).unwrap_or(false),
                transform: style
                    .and_then(|s| s.text_transform)
                    .unwrap_or(ctx.transform),
                white_space: style.and_then(|s| s.white_space).unwrap_or(ctx.white_space),
                overflow_wrap: style
                    .and_then(|s| s.overflow_wrap)
                    .unwrap_or(ctx.overflow_wrap),
                word_break: style.and_then(|s| s.word_break).unwrap_or(ctx.word_break),
                family: font.family,
                clip_fill,
            };
            collect_spans(tree, cid, styles, child, out, c, loaded_families);
            if owns_inline_fragment {
                c.end_owner(cid);
            }
        }
    }
}

fn push_text(
    raw: &str,
    transform: TextTransform,
    white_space: crate::WhiteSpace,
    attrs: &SpanAttrs,
    out: &mut Vec<(String, SpanAttrs)>,
    c: &mut Collector,
) {
    let mut buf = String::new();
    let mut at_word_start = c.last_was_space;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            match white_space {
                crate::WhiteSpace::Pre
                | crate::WhiteSpace::PreWrap
                | crate::WhiteSpace::BreakSpaces => buf.push(ch),
                crate::WhiteSpace::PreLine if ch == '\n' => {
                    if buf.ends_with(' ') {
                        buf.pop();
                    }
                    buf.push('\n');
                }
                _ if !c.last_was_space => buf.push(' '),
                _ => {}
            }
            c.last_was_space = true;
            at_word_start = true;
        } else {
            match transform {
                TextTransform::Uppercase => buf.extend(ch.to_uppercase()),
                TextTransform::Lowercase => buf.extend(ch.to_lowercase()),
                TextTransform::Capitalize if at_word_start => buf.extend(ch.to_uppercase()),
                _ => buf.push(ch),
            }
            c.last_was_space = false;
            at_word_start = false;
        }
    }
    if buf.is_empty() {
        return;
    }
    c.record_text(buf.len());
    if let Some((last_text, last_attrs)) = out.last_mut() {
        if last_attrs == attrs {
            last_text.push_str(&buf);
            return;
        }
    }
    out.push((buf, attrs.clone()));
}

fn boundary_event_on_line(
    item: &InlineItem,
    event: &InlineBoundaryEvent,
    line_start: usize,
    line_end: usize,
) -> bool {
    let source_end = item.owner_text.as_ref().map_or(0, String::len);
    let empty = item
        .owner_boxes
        .iter()
        .find(|owner| owner.owner == event.owner)
        .is_some_and(|owner| owner.start == owner.end);
    if empty {
        (event.position >= line_start && event.position < line_end)
            || (line_end == source_end && event.position == line_end)
    } else if event.is_start {
        event.position >= line_start && event.position < line_end
    } else {
        event.position > line_start && event.position <= line_end
    }
}

fn line_edge_advance(item: &InlineItem, line_start: usize, line_end: usize) -> f32 {
    item.boundary_events
        .iter()
        .filter(|event| boundary_event_on_line(item, event, line_start, line_end))
        .map(|event| event.edge.advance())
        .sum()
}

fn line_edge_alignment_shift(item: &InlineItem, line_start: usize, line_end: usize) -> f32 {
    -line_edge_advance(item, line_start, line_end)
        * match item.align {
            Some(Align::Center) => 0.5,
            Some(Align::End | Align::Right) => 1.0,
            _ => 0.0,
        }
}

fn line_advance_before_event(
    item: &InlineItem,
    event_index: usize,
    line_start: usize,
    line_end: usize,
) -> f32 {
    item.boundary_events[..event_index]
        .iter()
        .filter(|event| boundary_event_on_line(item, event, line_start, line_end))
        .map(|event| event.edge.advance())
        .sum()
}

fn line_advance_before_text(
    item: &InlineItem,
    global_position: usize,
    line_start: usize,
    line_end: usize,
) -> f32 {
    item.boundary_events
        .iter()
        .filter(|event| {
            boundary_event_on_line(item, event, line_start, line_end)
                && event.position <= global_position
        })
        .map(|event| event.edge.advance())
        .sum()
}

/// Total shaped size of a buffer: widest line, and the bottom of the last line.
fn buffer_size(item: &InlineItem) -> (f32, f32, bool) {
    let mut w = 0.0f32;
    let mut h = 0.0f32;
    let mut nonempty_lines = 0usize;
    let mut clamp_height = None;
    let line_starts = item
        .owner_text
        .as_deref()
        .map(|source| source_line_starts(&item.buffer, source))
        .unwrap_or_default();
    for (line_index, run) in item.buffer.layout_runs().enumerate() {
        let offset = if line_index == 0 {
            item.first_line_offset
        } else {
            0.0
        };
        let line_start = line_starts.get(run.line_i).copied().unwrap_or(0);
        let line_end = line_start + run.text.len();
        let edges = line_edge_advance(item, line_start, line_end);
        w = w.max((run.line_w + offset + edges).max(0.0));
        h = h.max(run.line_top + run.line_height);
        if !run.glyphs.is_empty() {
            nonempty_lines += 1;
            if item.line_clamp == Some(nonempty_lines) {
                clamp_height = Some(run.line_top + run.line_height);
            }
        }
    }
    let clamped = item.line_clamp.is_some_and(|limit| nonempty_lines > limit);
    (
        w.ceil(),
        if clamped {
            clamp_height.unwrap_or(h)
        } else {
            h
        },
        clamped,
    )
}

/// Map each cosmic-text BufferLine back to its byte start in the canonical
/// collapsed source. Buffer lines normally correspond to authored hard
/// breaks; text-indent can also split the first line synthetically.
fn source_line_starts(buffer: &Buffer, source: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(buffer.lines.len());
    let mut source_offset = 0usize;
    for line in &buffer.lines {
        while source.as_bytes().get(source_offset) == Some(&b'\n') {
            source_offset += 1;
        }
        let text = line.text();
        let start = if source
            .get(source_offset..)
            .is_some_and(|tail| tail.starts_with(text))
        {
            source_offset
        } else {
            source
                .get(source_offset..)
                .and_then(|tail| tail.find(text))
                .map_or(source_offset, |relative| source_offset + relative)
        };
        starts.push(start);
        source_offset = start.saturating_add(text.len()).min(source.len());
    }
    starts
}

fn run_cursor_x(run: &cosmic_text::LayoutRun<'_>, byte: usize) -> f32 {
    let cursor = Cursor::new(run.line_i, byte.min(run.text.len()));
    if let Some((x, _)) = run.highlight(cursor, cursor) {
        return x;
    }
    if byte == 0 {
        run.glyphs.first().map_or(0.0, |glyph| glyph.x)
    } else {
        run.glyphs
            .last()
            .map_or(run.line_w, |glyph| glyph.x + glyph.w)
    }
}

fn glyph_relative_offset(
    ranges: &[RelativeOwnerTextRange],
    line_start: usize,
    glyph_start: usize,
    glyph_end: usize,
) -> (f32, f32) {
    if ranges.is_empty() {
        return (0.0, 0.0);
    }
    let start = line_start.saturating_add(glyph_start);
    let end = line_start.saturating_add(glyph_end);
    ranges
        .iter()
        .rev()
        .find(|range| range.start < end && range.end > start)
        .map_or((0.0, 0.0), |range| range.offset)
}

fn used_text_indent(value: Dimension, width: Option<f32>) -> f32 {
    let indent = match value {
        Dimension::Px(pixels) => pixels,
        Dimension::Percent(fraction) => width.unwrap_or(0.0) * fraction,
        _ => 0.0,
    };
    if indent.is_finite() {
        indent
    } else {
        0.0
    }
}

/// Shape one IFC with first-line indent and ordinary-inline boundary advances.
///
/// The pristine buffer remains one shaping stream across DOM owners. For a
/// definite width we probe cosmic-text's own UAX/CSS wrap boundary, subtract
/// the ordered margin/border/padding events that fall on that candidate line,
/// and monotonically retry if those edges force an earlier break. Only final
/// visual-line boundaries split BufferLines, so ligatures and kerning are not
/// broken merely because an inline element starts or ends.
fn shape_with_text_indent(
    font_system: &mut FontSystem,
    item: &mut InlineItem,
    width: Option<f32>,
    wrap: Wrap,
) {
    let indent = used_text_indent(item.text_indent, width);
    item.first_line_offset = indent
        * match item.align {
            Some(Align::Center) => 0.5,
            Some(Align::End | Align::Right) => 0.0,
            _ => 1.0,
        };

    let Some(source) = item.source_buffer.as_ref() else {
        item.buffer.set_wrap(font_system, wrap);
        item.buffer
            .set_size(font_system, width.map(|value| value.max(0.0)), None);
        item.buffer.shape_until_scroll(font_system, false);
        return;
    };
    item.buffer = source.clone();
    item.buffer.set_wrap(font_system, wrap);

    if let (Some(full_width), Some(source_text)) = (width, item.owner_text.as_deref()) {
        let metrics = item.buffer.metrics();
        let mono = item.buffer.monospace_width();
        let tab_width = item.buffer.tab_width();
        let mut line_index = 0usize;
        while line_index < item.buffer.lines.len() {
            let starts = source_line_starts(&item.buffer, source_text);
            let global_start = starts.get(line_index).copied().unwrap_or(0);
            let first_indent = if line_index == 0 { indent } else { 0.0 };
            let base_available = (full_width - first_indent).max(0.0);
            // Negative inline margins can admit content that would not fit in
            // the text-only probe. Begin at the widest possible candidate and
            // retain the same monotonic-decrease convergence used for positive
            // padding/border advances.
            let line_source_end = global_start + item.buffer.lines[line_index].text().len();
            let negative_edges = item
                .boundary_events
                .iter()
                .filter(|event| event.position >= global_start && event.position <= line_source_end)
                .map(|event| event.edge.advance().min(0.0))
                .sum::<f32>();
            let mut available = (base_available - negative_edges).max(0.0);
            let mut split = None;

            for _ in 0..=item.boundary_events.len() {
                let (candidate, line_width) = {
                    let line = &mut item.buffer.lines[line_index];
                    let layouts = line.layout(
                        font_system,
                        metrics.font_size,
                        Some(available),
                        wrap,
                        mono,
                        tab_width,
                    );
                    let Some(first) = layouts.first() else {
                        break;
                    };
                    (first.glyphs.iter().map(|glyph| glyph.end).max(), first.w)
                };
                let Some(candidate) = candidate else {
                    break;
                };
                let candidate_end = global_start.saturating_add(candidate);
                let edges = line_edge_advance(item, global_start, candidate_end);
                let required_available = (base_available - edges).max(0.0);
                split = Some(candidate);
                if line_width + edges <= base_available + 0.01
                    || required_available + 0.01 >= available
                {
                    break;
                }
                available = required_available;
                item.buffer.lines[line_index].reset_layout();
            }

            let Some(mut split) = split else {
                line_index += 1;
                continue;
            };
            let text = item.buffer.lines[line_index].text();
            while split < text.len() {
                let Some(ch) = text[split..].chars().next() else {
                    break;
                };
                if !ch.is_whitespace() {
                    break;
                }
                split += ch.len_utf8();
            }
            if split > 0 && split < text.len() {
                let tail = item.buffer.lines[line_index].split_off(split);
                item.buffer.lines.insert(line_index + 1, tail);
            }
            line_index += 1;
        }
    } else if let Some(full_width) = width {
        item.buffer
            .set_size(font_system, Some((full_width - indent).max(0.0)), None);
        item.buffer.shape_until_scroll(font_system, false);
        let first_break = item
            .buffer
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.iter().map(|glyph| glyph.end).max());
        item.buffer = source.clone();
        item.buffer.set_wrap(font_system, wrap);
        if let (Some(mut split), Some(first_line)) = (first_break, item.buffer.lines.first()) {
            let text = first_line.text();
            while split < text.len() {
                let Some(ch) = text[split..].chars().next() else {
                    break;
                };
                if !ch.is_whitespace() {
                    break;
                }
                split += ch.len_utf8();
            }
            if split > 0 && split < text.len() {
                let tail = item.buffer.lines[0].split_off(split);
                item.buffer.lines.insert(1, tail);
            }
        }
    }
    item.buffer
        .set_size(font_system, width.map(|value| value.max(0.0)), None);
    item.buffer.shape_until_scroll(font_system, false);
}

/// Chromium's bisection implementation only balances paragraphs of at most
/// six lines. Keeping the same cap bounds repeated shaping on long body copy
/// and confines this work to the heading-sized content the property targets.
const MAX_BALANCED_LINES: usize = 6;

/// Tighten `buffer` to the narrowest (within one CSS pixel) wrapping width
/// that retains its natural line count. This is deliberately a final-shaping
/// operation: intrinsic measurement and the block's used geometry remain
/// unchanged, while the line grouping changes.
fn balance_wrap_width(
    font_system: &mut FontSystem,
    buffer: &mut Buffer,
    available_width: f32,
) -> Option<f32> {
    let natural = buffer.layout_runs().collect::<Vec<_>>();
    let line_count = natural.len();
    if !(2..=MAX_BALANCED_LINES).contains(&line_count) {
        return None;
    }
    // Blink's bisection path is inapplicable to forced breaks; balancing each
    // hard-break-delimited paragraph needs separate segment constraints.
    if buffer.lines.len() > 1 {
        return None;
    }

    let average_line_width = natural.iter().map(|run| run.line_w).sum::<f32>() / line_count as f32;
    drop(natural);
    let mut lower = (average_line_width * 0.8).clamp(0.0, available_width);
    let mut upper = available_width;
    while lower + 1.0 < upper {
        let middle = (lower + upper) * 0.5;
        buffer.set_size(font_system, Some(middle), None);
        buffer.shape_until_scroll(font_system, false);
        if buffer.layout_runs().count() == line_count {
            upper = middle;
        } else {
            lower = middle;
        }
    }
    buffer.set_size(font_system, Some(upper), None);
    buffer.shape_until_scroll(font_system, false);
    Some(upper)
}

/// Does `id` establish an inline formatting context made purely of text and
/// plain inline formatting (no atomic inline boxes, no block children, no
/// inline element carrying its own box)? Such a container collapses cleanly
/// to one shaped buffer; anything else keeps the general build path.
fn is_pure_text_ifc(
    tree: &DomTree,
    id: NodeId,
    styles: &std::collections::HashMap<NodeId, LayoutStyle>,
) -> bool {
    let Some(style) = styles.get(&id) else {
        return false;
    };
    // Out-of-flow generated boxes neither contribute to nor interrupt the
    // host's inline formatting context. Keeping a positioned decorative
    // pseudo must not demote an otherwise text-only shrink-to-fit control to
    // approximate per-word flex leaves: that path can disagree with shaped
    // space advances and wrap text at its own max-content width.
    let has_in_flow_pseudo = |pseudo: Option<&LayoutStyle>| {
        pseudo.is_some_and(|pseudo| {
            pseudo.display != Display::None
                && !matches!(pseudo.position, Some(taffy::Position::Absolute))
        })
    };
    if has_in_flow_pseudo(style.before_pseudo.as_deref())
        || has_in_flow_pseudo(style.after_pseudo.as_deref())
    {
        return false;
    }
    // Only containers that lay their children out in normal flow (block, or
    // the flex-column stand-ins our UA sheet uses for td/th/center). A real
    // flex/grid row with inline children is rare and better left to taffy.
    let flow = style.display == Display::Block
        || style.is_inline_block
        || (style.display == Display::Flex
            && style.flex_direction == Some(taffy::FlexDirection::Column));
    if !flow {
        return false;
    }
    let mut has_text = false;
    let children = crate::dom::rendered_children(tree, id);
    if children.is_empty() {
        return false;
    }
    for cid in &children {
        if !inline_child_ok(tree, *cid, styles, &mut has_text) {
            return false;
        }
    }
    has_text
}

/// Replaced / atomic-inline tags: their box does not contain text, so folding
/// one into a shaped buffer would drop its content entirely. A subtree that
/// contains one is never a pure-text IFC (it keeps the general build path,
/// where the element gets a real taffy box). This is by tag, not display, so a
/// stylesheet setting `img{display:inline}` cannot trick us into folding it.
pub(crate) fn is_replaced(local: &str) -> bool {
    matches!(
        local,
        "img"
            | "svg"
            | "canvas"
            | "video"
            | "audio"
            | "iframe"
            | "embed"
            | "object"
            | "input"
            | "textarea"
            | "select"
            | "button"
            | "progress"
            | "meter"
    )
}

/// Elements that use CSS's intrinsic replaced-size algorithm. This is
/// deliberately narrower than `is_replaced`: ordinary form controls are
/// atomic inline boxes, but grid `normal` stretches them like non-replaced
/// boxes in Chromium and Gecko.
pub(crate) fn has_replaced_sizing(local: &str) -> bool {
    matches!(
        local,
        "img" | "canvas" | "video" | "audio" | "iframe" | "embed" | "object" | "progress" | "meter"
    )
}

/// HTML's default object size for replaced media whose intrinsic metadata is
/// not available yet. Canvas dimensions and decoded video metadata can replace
/// these defaults before layout when present.
pub(crate) fn default_replaced_intrinsic_size(
    local: &str,
    font_size: f32,
    has_controls: bool,
    has_resource: bool,
) -> Option<(f32, f32)> {
    match local {
        "canvas" | "video" | "iframe" | "object" => Some((300.0, 150.0)),
        "embed" if has_resource => Some((300.0, 150.0)),
        "audio" if has_controls => Some((300.0, 54.0)),
        "progress" => Some((font_size * 10.0, font_size)),
        "meter" => Some((font_size * 5.0, font_size)),
        _ => None,
    }
}

/// Is `cid` (and its whole subtree) inline-level, in-flow content safe to fold
/// into a shaped buffer? Sets `has_text` if it contributes any non-whitespace
/// text. Inline wrappers (`<a>`, `<span>`, `<b>`, `<code>`, `<sup>`, ...) are
/// accepted and recursed into even when they carry a background or border of
/// their own: keeping the whole paragraph as one shaped run (correct wrapping
/// plus per-span color/weight/style/underline that `collect_spans` threads) is
/// worth losing an inline decoration, and it avoids the taffy-discouraged
/// flex word-promotion path that breaks real prose wrapping. Only boxes that
/// genuinely cannot fold are rejected: replaced/atomic elements, block-level
/// children, floats, out-of-flow positioned boxes, and elements with generated
/// content (which would be lost).
fn inline_child_ok(
    tree: &DomTree,
    cid: NodeId,
    styles: &std::collections::HashMap<NodeId, LayoutStyle>,
    has_text: &mut bool,
) -> bool {
    let Some(node) = tree.get_node(cid) else {
        return true;
    };
    match &node.data {
        obscura_dom::tree::NodeData::Text { contents } => {
            if !contents.trim().is_empty() {
                *has_text = true;
            }
            true
        }
        obscura_dom::tree::NodeData::Element { .. } => {
            let Some(elem) = node.as_element() else {
                return true;
            };
            let Some(style) = styles.get(&cid) else {
                return false;
            };
            if style.display == Display::None {
                return true; // removed from flow; ignore its subtree
            }
            if elem.local.as_ref() == "br" {
                *has_text = true;
                return true;
            }
            // A replaced element or an atomic inline-block has its own box with
            // non-text content; it must stay a real taffy box (keep flex path).
            if is_replaced(elem.local.as_ref()) || style.is_inline_block {
                return false;
            }
            // Only genuinely inline-level, in-flow boxes fold. A block-level
            // child, a float, or an out-of-flow positioned box each needs the
            // general path; so does an element with generated ::before/::after
            // content (lost if folded) or an overflow clip of its own.
            let foldable_inline = style.display == Display::Inline
                && !matches!(style.position, Some(taffy::Position::Absolute))
                && style.float.is_none()
                && !style.overflow_hidden
                && style.before_pseudo.is_none()
                && style.after_pseudo.is_none();
            if !foldable_inline {
                return false;
            }
            if style.margin != crate::Edges::default()
                || style.padding != crate::Edges::default()
                || style.border != crate::Edges::default()
            {
                *has_text = true;
            }
            for gc in crate::dom::rendered_children(tree, cid) {
                if !inline_child_ok(tree, gc, styles, has_text) {
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}

/// Content-box origin for a container whose border box is `rect`, i.e. inside
/// its border and padding: where inline text actually starts.
pub fn content_origin(rect: &Rect, style: &LayoutStyle) -> (f32, f32) {
    (
        rect.x + style.border.left + style.padding.left,
        rect.y + style.border.top + style.padding.top,
    )
}

pub fn content_width(rect: &Rect, style: &LayoutStyle) -> f32 {
    (rect.width - style.border.left - style.border.right - style.padding.left - style.padding.right)
        .max(0.0)
}

/// The background to paint through the glyphs for `-webkit-background-clip: text`
/// when the element's own text color is transparent (the common gradient-text
/// technique on hero headings and buttons). Returns the background gradient as
/// is, or a solid background color as a flat two-stop gradient. `None` when the
/// element is not a transparent-text clip-to-text box, so ordinary transparent
/// text still renders invisibly.
fn clip_text_fill(style: &LayoutStyle) -> Option<(f32, Vec<([u8; 4], Option<f32>)>)> {
    if !style.background_clip_text {
        return None;
    }
    // Only when the text itself is transparent: an opaque color paints normally
    // and the clip is a no-op we would otherwise recolor incorrectly.
    if style.color.map(|c| c[3] != 0).unwrap_or(true) {
        return None;
    }
    if let Some(g) = &style.background_gradient {
        if g.1.len() >= 2 {
            return Some(g.clone());
        }
    }
    let bg = style.background_color.filter(|c| c[3] != 0)?;
    Some((180.0, vec![(bg, Some(0.0)), (bg, Some(1.0))]))
}

/// Sample a CSS linear gradient at point `(x, y)` inside a `w` x `h` text box,
/// returning an rgba color. `angle` is CSS degrees clockwise from 12 o'clock
/// (0 = to top, 90 = to right, 180 = to bottom), matching `parse_linear_gradient`
/// and `paint::paint_linear_gradient`. Positionless stops are spread evenly.
fn sample_gradient(
    fill: &(f32, Vec<([u8; 4], Option<f32>)>),
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> [u8; 4] {
    let (angle, stops) = fill;
    match stops.len() {
        0 => return [0, 0, 0, 255],
        1 => return stops[0].0,
        _ => {}
    }
    let rad = angle.to_radians();
    let (dx, dy) = (rad.sin(), -rad.cos());
    let (w, h) = (w.max(1.0), h.max(1.0));
    // Full extent of the box along the gradient direction (the CSS gradient-line
    // length), so the endpoints land at the box's projected corners.
    let len = (w * dx).abs() + (h * dy).abs();
    let t = if len <= 0.0 {
        0.5
    } else {
        (((x - w / 2.0) * dx + (y - h / 2.0) * dy) / len + 0.5).clamp(0.0, 1.0)
    };
    let n = stops.len();
    let pos = |i: usize| {
        stops[i]
            .1
            .unwrap_or(i as f32 / (n as f32 - 1.0))
            .clamp(0.0, 1.0)
    };
    // Walk to the pair of stops surrounding t, then interpolate between them.
    let mut lo = 0usize;
    while lo + 1 < n && pos(lo + 1) < t {
        lo += 1;
    }
    let hi = (lo + 1).min(n - 1);
    let (p0, p1) = (pos(lo), pos(hi));
    let f = if (p1 - p0).abs() < 1e-6 {
        0.0
    } else {
        ((t - p0) / (p1 - p0)).clamp(0.0, 1.0)
    };
    let (c0, c1) = (stops[lo].0, stops[hi].0);
    let lerp = |a: u8, b: u8| {
        (a as f32 + (b as f32 - a as f32) * f)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [
        lerp(c0[0], c1[0]),
        lerp(c0[1], c1[1]),
        lerp(c0[2], c1[2]),
        lerp(c0[3], c1[3]),
    ]
}

/// Whether one shaped inline item can contribute ink to the destination
/// surface. `SwashCache::with_pixels` rasterizes a glyph before its callback
/// can reject out-of-bounds pixels, so letting a long document reach that
/// callback for every offscreen glyph makes viewport screenshots scale with
/// the full page height. Keep this test deliberately conservative: line boxes
/// are expanded by four ems for unusual font ink bounds, and every authored
/// relative-inline offset participates in the envelope. Translation and
/// isolated transform layers arrive through `offset`, in the same coordinate
/// space as the destination pixmap.
fn inline_item_may_intersect_surface(
    item: &InlineItem,
    offset: (f32, f32),
    clip: Option<Rect>,
    surface_height: u32,
    raster_scale: f32,
) -> bool {
    if !raster_scale.is_finite() || raster_scale <= 0.0 || surface_height == 0 {
        return false;
    }
    let surface_bottom = surface_height as f32 / raster_scale;
    if let Some(clip) = clip {
        if clip.y >= surface_bottom || clip.y + clip.height <= 0.0 {
            return false;
        }
    }

    let mut line_top = f32::INFINITY;
    let mut line_bottom = f32::NEG_INFINITY;
    let mut max_font_size = 0.0f32;
    for run in item.buffer.layout_runs() {
        line_top = line_top.min(run.line_top);
        line_bottom = line_bottom.max(run.line_top + run.line_height);
        for glyph in run.glyphs {
            if glyph.font_size.is_finite() {
                max_font_size = max_font_size.max(glyph.font_size.max(0.0));
            }
        }
    }
    if !line_top.is_finite() || !line_bottom.is_finite() {
        return false;
    }

    // Some fonts have ink well outside their ascender/descender metrics. Four
    // ems on each side is intentionally much larger than normal glyph ink,
    // while still culling text that is genuinely pages away from the viewport.
    let ink_guard = max_font_size.mul_add(4.0, 4.0);
    let mut relative_top = 0.0f32;
    let mut relative_bottom = 0.0f32;
    for range in &item.relative_owner_ranges {
        if range.offset.1.is_finite() {
            relative_top = relative_top.min(range.offset.1);
            relative_bottom = relative_bottom.max(range.offset.1);
        }
    }
    let origin_y = item.origin.1 + offset.1;
    let visual_top = origin_y + line_top + relative_top - ink_guard;
    let visual_bottom = origin_y + line_bottom + relative_bottom + ink_guard;
    visual_top < surface_bottom && visual_bottom > 0.0
}

impl TextEngine {
    /// Rasterize inline context `idx` into `pixmap`, honoring its finalized
    /// clip. Tests and generated items whose coordinates do not change between
    /// layout and paint use this path.
    pub fn paint_item(&mut self, idx: usize, pixmap: &mut tiny_skia::Pixmap, offset: (f32, f32)) {
        self.paint_item_with_clip(idx, pixmap, offset, None);
    }

    /// Rasterize an inline context with a capture-space clip override. Root
    /// scrolling is layered over immutable document layout, so the glyph
    /// origin and any overflow clip must both be sampled in the current
    /// viewport rather than mixing viewport and document coordinates.
    pub fn paint_item_with_clip(
        &mut self,
        idx: usize,
        pixmap: &mut tiny_skia::Pixmap,
        offset: (f32, f32),
        clip_override: Option<Rect>,
    ) {
        self.paint_item_with_clip_mask(idx, pixmap, offset, clip_override, None);
    }

    /// Rasterize shaped text against the rectangular culling envelope and an
    /// optional full descendant clip-chain mask. The latter preserves rounded
    /// overflow corners without moving glyph coordinates into the clip
    /// owner's space.
    pub fn paint_item_with_clip_mask(
        &mut self,
        idx: usize,
        pixmap: &mut tiny_skia::Pixmap,
        offset: (f32, f32),
        clip_override: Option<Rect>,
        clip_mask: Option<&tiny_skia::Mask>,
    ) {
        self.paint_item_with_clip_mask_scaled(idx, pixmap, offset, clip_override, clip_mask, 1.0);
    }

    /// Rasterize already-shaped CSS-pixel glyph positions directly into a
    /// device-pixel surface. Shaping and line breaking stay immutable; only
    /// glyph outline sampling, clipping, and compositing use `raster_scale`.
    pub fn paint_item_with_clip_mask_scaled(
        &mut self,
        idx: usize,
        pixmap: &mut tiny_skia::Pixmap,
        offset: (f32, f32),
        clip_override: Option<Rect>,
        clip_mask: Option<&tiny_skia::Mask>,
        raster_scale: f32,
    ) {
        self.paint_item_with_clip_mask_scaled_for_print(
            idx,
            pixmap,
            offset,
            clip_override,
            clip_mask,
            raster_scale,
            false,
        );
    }

    pub(crate) fn paint_item_with_clip_mask_scaled_for_print(
        &mut self,
        idx: usize,
        pixmap: &mut tiny_skia::Pixmap,
        offset: (f32, f32),
        clip_override: Option<Rect>,
        clip_mask: Option<&tiny_skia::Mask>,
        raster_scale: f32,
        print_economy: bool,
    ) {
        let TextEngine {
            font_system,
            swash,
            variable_swash,
            items,
            ..
        } = self;
        let Some(item) = items.get_mut(idx) else {
            return;
        };
        let clip = clip_override.or(item.clip);
        if !inline_item_may_intersect_surface(
            item,
            offset,
            clip,
            pixmap.height(),
            raster_scale,
        ) {
            return;
        }
        let (ox, oy) = (
            (item.origin.0 + offset.0) * raster_scale,
            (item.origin.1 + offset.1) * raster_scale,
        );
        // The glyph origin shifts by the container's accumulated translate,
        // but the clip is already in screen space (owner-shifted at
        // `resolve_clip_rects`) and must not move with the container, or a
        // translated slide would drag its viewport's clip along with it.
        let pw = pixmap.width() as i32;
        let ph = pixmap.height() as i32;
        let clip_bounds = clip.map(|c| {
            (
                c.x * raster_scale,
                c.y * raster_scale,
                (c.x + c.width) * raster_scale,
                (c.y + c.height) * raster_scale,
            )
        });
        let line_source_starts = item
            .owner_text
            .as_deref()
            .filter(|_| !item.relative_owner_ranges.is_empty() || !item.boundary_events.is_empty())
            .map(|source| source_line_starts(&item.buffer, source))
            .unwrap_or_default();

        // Collect underline segments before drawing glyphs (both borrow the
        // buffer). Underline is carried per glyph via metadata; group runs of
        // consecutive underlined glyphs on a line into one stroke below the
        // baseline. Done first so the draw() mutable borrow does not overlap.
        let mut underlines: Vec<(f32, f32, f32, f32, [u8; 4])> = Vec::new(); // x0, x1, y, thickness, color
        let mut fill_bounds: Vec<Option<(f32, f32, f32, f32)>> = vec![None; item.clip_fills.len()];
        for (line_index, run) in item.buffer.layout_runs().enumerate() {
            let line_offset = if line_index == 0 {
                item.first_line_offset
            } else {
                0.0
            };
            let line_source_start = line_source_starts.get(run.line_i).copied().unwrap_or(0);
            let line_source_end = line_source_start + run.text.len();
            let inline_alignment =
                line_edge_alignment_shift(item, line_source_start, line_source_end);
            let base_y = run.line_y;
            let mut seg: Option<(f32, f32, f32, [u8; 4], (f32, f32))> = None;
            for g in run.glyphs {
                // Keep decoration and background-clip bounds in lockstep with
                // glyph painting. A truncated glyph must not leave an
                // underline tail or expand a gradient's sampling bounds past
                // the separately painted ellipsis marker.
                if item.marker.is_some_and(|marker| {
                    marker.line_index == line_index && g.x + line_offset + g.w > marker.content_end
                }) {
                    continue;
                }
                let mut relative = glyph_relative_offset(
                    &item.relative_owner_ranges,
                    line_source_start,
                    g.start,
                    g.end,
                );
                relative.0 += inline_alignment
                    + line_advance_before_text(
                        item,
                        line_source_start + g.start,
                        line_source_start,
                        line_source_end,
                    );
                let underlined = g.metadata & META_UNDERLINE != 0;
                if let Some(fill_index) = metadata_fill(g.metadata) {
                    if let Some(bounds) = fill_bounds.get_mut(fill_index) {
                        let glyph_bounds = (
                            g.x + line_offset + relative.0,
                            run.line_top + relative.1,
                            g.x + line_offset + g.w + relative.0,
                            run.line_top + run.line_height + relative.1,
                        );
                        *bounds = Some(match *bounds {
                            Some((x0, y0, x1, y1)) => (
                                x0.min(glyph_bounds.0),
                                y0.min(glyph_bounds.1),
                                x1.max(glyph_bounds.2),
                                y1.max(glyph_bounds.3),
                            ),
                            None => glyph_bounds,
                        });
                    }
                }
                let mut col = g
                    .color_opt
                    .map(|c| [c.r(), c.g(), c.b(), c.a()])
                    .unwrap_or([0, 0, 0, 255]);
                if print_economy {
                    col = crate::paint::print_economy_color(col);
                }
                if underlined {
                    match &mut seg {
                        Some((_, x1, fs, c, prior_relative))
                            if *c == col && *prior_relative == relative =>
                        {
                            *x1 = g.x + line_offset + g.w + relative.0;
                            *fs = fs.max(g.font_size);
                        }
                        _ => {
                            if let Some((x0, x1, fs, c, prior_relative)) = seg.take() {
                                underlines.push((
                                    x0,
                                    x1,
                                    base_y + prior_relative.1 + (fs * 0.12).max(1.0),
                                    (fs / 14.0).max(1.0),
                                    c,
                                ));
                            }
                            seg = Some((
                                g.x + line_offset + relative.0,
                                g.x + line_offset + g.w + relative.0,
                                g.font_size,
                                col,
                                relative,
                            ));
                        }
                    }
                } else if let Some((x0, x1, fs, c, relative)) = seg.take() {
                    underlines.push((
                        x0,
                        x1,
                        base_y + relative.1 + (fs * 0.12).max(1.0),
                        (fs / 14.0).max(1.0),
                        c,
                    ));
                }
            }
            if let Some((x0, x1, fs, c, relative)) = seg.take() {
                underlines.push((
                    x0,
                    x1,
                    base_y + relative.1 + (fs * 0.12).max(1.0),
                    (fs / 14.0).max(1.0),
                    c,
                ));
            }
        }

        // Fallback color if a glyph carries none (shouldn't happen: every span
        // sets one), black.
        let default = Color::rgba(0, 0, 0, 255);
        // `-webkit-background-clip: text`: rasterize each glyph while its
        // metadata is still available, then sample that span's background
        // gradient across the span's own shaped bounds. Buffer::draw omits
        // metadata from its callback, so using it here would force one fill
        // over the entire heading and lose inline accent gradients.
        let clip_fills = item.clip_fills.clone();
        let pixels = pixmap.pixels_mut();
        for (line_index, run) in item.buffer.layout_runs().enumerate() {
            let line_offset = if line_index == 0 {
                item.first_line_offset
            } else {
                0.0
            };
            let line_source_start = line_source_starts.get(run.line_i).copied().unwrap_or(0);
            let line_source_end = line_source_start + run.text.len();
            let inline_alignment =
                line_edge_alignment_shift(item, line_source_start, line_source_end);
            for glyph in run.glyphs {
                if item.marker.is_some_and(|marker| {
                    marker.line_index == line_index
                        && glyph.x + line_offset + glyph.w > marker.content_end
                }) {
                    continue;
                }
                let mut relative = glyph_relative_offset(
                    &item.relative_owner_ranges,
                    line_source_start,
                    glyph.start,
                    glyph.end,
                );
                relative.0 += inline_alignment
                    + line_advance_before_text(
                        item,
                        line_source_start + glyph.start,
                        line_source_start,
                        line_source_end,
                    );
                let physical = glyph.physical((line_offset + relative.0, relative.1), raster_scale);
                let glyph_color = glyph.color_opt.unwrap_or(default);
                let fill_index = metadata_fill(glyph.metadata);
                if print_economy && fill_index.is_some() {
                    continue;
                }
                let mut draw_pixel = |x, y, color: Color| {
                    // cosmic-text's mask rasterizer replaces the authored
                    // alpha with glyph coverage (see its `blend base alpha?`
                    // TODO). Reapply the span alpha here so transparent and
                    // translucent CSS text do not become opaque at paint.
                    let a = color.a() as u32 * glyph_color.a() as u32 / 255;
                    if a == 0 {
                        return;
                    }
                    let gx = physical.x + x;
                    let gy = (run.line_y * raster_scale) as i32 + physical.y + y;
                    let (mut r, mut g, mut b) = fill_index
                        .and_then(|index| {
                            let fill = clip_fills.get(index)?;
                            let (x0, y0, x1, y1) = fill_bounds.get(index).copied().flatten()?;
                            let sampled = sample_gradient(
                                fill,
                                (gx as f32 + 0.5) / raster_scale - x0,
                                (gy as f32 + 0.5) / raster_scale - y0,
                                x1 - x0,
                                y1 - y0,
                            );
                            Some((sampled[0], sampled[1], sampled[2]))
                        })
                        .unwrap_or_else(|| (color.r(), color.g(), color.b()));
                    if print_economy {
                        let adjusted = crate::paint::print_economy_color([r, g, b, 255]);
                        (r, g, b) = (adjusted[0], adjusted[1], adjusted[2]);
                    }
                    let px = ox as i32 + gx;
                    let py = oy as i32 + gy;
                    if let Some((cx0, cy0, cx1, cy1)) = clip_bounds {
                        if (px as f32) < cx0
                            || (px as f32) >= cx1
                            || (py as f32) < cy0
                            || (py as f32) >= cy1
                        {
                            return;
                        }
                    }
                    if px < 0 || px >= pw || py < 0 || py >= ph {
                        return;
                    }
                    let idx = (py * pw + px) as usize;
                    let mask_alpha = clip_mask
                        .and_then(|mask| mask.data().get(idx))
                        .copied()
                        .unwrap_or(255) as u32;
                    let a = a * mask_alpha / 255;
                    if a == 0 {
                        return;
                    }
                    let dst = pixels[idx];
                    let sa = a;
                    let sr = (r as u32 * sa) / 255;
                    let sg = (g as u32 * sa) / 255;
                    let sb = (b as u32 * sa) / 255;
                    let inv = 255 - sa;
                    let out_a = sa + (dst.alpha() as u32 * inv / 255);
                    if out_a == 0 {
                        return;
                    }
                    let out_r = sr + (dst.red() as u32 * inv / 255);
                    let out_g = sg + (dst.green() as u32 * inv / 255);
                    let out_b = sb + (dst.blue() as u32 * inv / 255);
                    pixels[idx] = tiny_skia::PremultipliedColorU8::from_rgba(
                        out_r as u8,
                        out_g as u8,
                        out_b as u8,
                        out_a as u8,
                    )
                    .unwrap_or(dst);
                };
                let explicit_variations = metadata_variation(glyph.metadata)
                    .and_then(|index| item.variation_sets.get(index))
                    .map(Arc::clone);
                let effective_variations = glyph
                    .font_is_variable
                    .then(|| {
                        variable_swash.effective_variations(
                            font_system,
                            physical.cache_key.font_id,
                            glyph.font_weight_axis_opt,
                            glyph.font_optical_size_opt,
                            glyph.font_italic_axis,
                            explicit_variations,
                        )
                    })
                    .flatten();
                if let Some(variations) = effective_variations {
                    variable_swash.with_pixels(
                        font_system,
                        physical.cache_key,
                        variations,
                        glyph_color,
                        &mut draw_pixel,
                    );
                } else {
                    swash.with_pixels(
                        font_system,
                        physical.cache_key,
                        glyph_color,
                        &mut draw_pixel,
                    );
                }
            }
        }

        // The marker owns its own shaped buffer so it uses a real U+2026
        // glyph from the selected font without changing DOM text or the
        // natural content buffer. The direct pure-text slice is LTR-only;
        // logical-start marker placement arrives with the bidi foundation.
        if let (Some(marker), Some(marker_buffer)) = (item.marker, item.marker_buffer.as_ref()) {
            marker_buffer.draw(font_system, swash, default, |x, y, _, _, color| {
                let px = ox as i32 + marker.x.round() as i32 + x;
                let py = oy as i32 + marker.y.round() as i32 + y;
                if let Some((cx0, cy0, cx1, cy1)) = clip_bounds {
                    if (px as f32) < cx0
                        || (px as f32) >= cx1
                        || (py as f32) < cy0
                        || (py as f32) >= cy1
                    {
                        return;
                    }
                }
                if px < 0 || px >= pw || py < 0 || py >= ph {
                    return;
                }
                let alpha = color.a() as u32;
                if alpha == 0 {
                    return;
                }
                let index = (py * pw + px) as usize;
                let dst = pixels[index];
                let inverse = 255 - alpha;
                let out_alpha = alpha + dst.alpha() as u32 * inverse / 255;
                let out_red = color.r() as u32 * alpha / 255 + dst.red() as u32 * inverse / 255;
                let out_green = color.g() as u32 * alpha / 255 + dst.green() as u32 * inverse / 255;
                let out_blue = color.b() as u32 * alpha / 255 + dst.blue() as u32 * inverse / 255;
                pixels[index] = tiny_skia::PremultipliedColorU8::from_rgba(
                    out_red as u8,
                    out_green as u8,
                    out_blue as u8,
                    out_alpha as u8,
                )
                .unwrap_or(dst);
            });
        }

        // Stroke underline segments with the same authored alpha as their
        // glyphs. Transparent text decorations are transparent too.
        for (x0, x1, y, thick, col) in underlines {
            if col[3] == 0 {
                continue;
            }
            let t = (thick.max(1.0) * raster_scale).round().max(1.0) as i32;
            for dt in 0..t {
                let py = oy as i32 + (y * raster_scale) as i32 + dt;
                if py < 0 || py >= ph {
                    continue;
                }
                if let Some((_, cy0, _, cy1)) = clip_bounds {
                    if (py as f32) < cy0 || (py as f32) >= cy1 {
                        continue;
                    }
                }
                for px in (ox + x0 * raster_scale) as i32..(ox + x1 * raster_scale) as i32 {
                    if px < 0 || px >= pw {
                        continue;
                    }
                    if let Some((cx0, _, cx1, _)) = clip_bounds {
                        if (px as f32) < cx0 || (px as f32) >= cx1 {
                            continue;
                        }
                    }
                    let i = (py * pw + px) as usize;
                    let dst = pixels[i];
                    let sa = col[3] as u32;
                    let inv = 255 - sa;
                    let out_a = sa + dst.alpha() as u32 * inv / 255;
                    let out_r = col[0] as u32 * sa / 255 + dst.red() as u32 * inv / 255;
                    let out_g = col[1] as u32 * sa / 255 + dst.green() as u32 * inv / 255;
                    let out_b = col[2] as u32 * sa / 255 + dst.blue() as u32 * inv / 255;
                    pixels[i] = tiny_skia::PremultipliedColorU8::from_rgba(
                        out_r as u8,
                        out_g as u8,
                        out_b as u8,
                        out_a as u8,
                    )
                    .unwrap_or(dst);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    fn surface_cull_fixture() -> (TextEngine, usize) {
        let tree = obscura_dom::parse_html("<p id='copy'>Visible text</p>");
        let copy = tree.get_element_by_id("copy").unwrap();
        let style = LayoutStyle {
            display: Display::Block,
            font_size: Some(16.0),
            line_height: Some(crate::LineHeight::Px(20.0)),
            ..Default::default()
        };
        let mut engine = TextEngine::new();
        let item = engine
            .try_build(&tree, copy, &HashMap::from([(copy, style)]))
            .unwrap();
        engine.finalize(item, (0.0, 10.0), 200.0, None);
        (engine, item)
    }

    #[test]
    fn text_surface_cull_is_conservative_for_offsets_and_ink_overhang() {
        let (mut engine, item) = surface_cull_fixture();
        assert!(inline_item_may_intersect_surface(
            &engine.items[item],
            (0.0, 0.0),
            None,
            100,
            1.0,
        ));

        engine.items[item].origin.1 = -60.0;
        assert!(
            inline_item_may_intersect_surface(
                &engine.items[item],
                (0.0, 0.0),
                None,
                100,
                1.0,
            ),
            "the four-em ink guard must preserve unusual glyph overhang"
        );
        engine.items[item].origin.1 = -500.0;
        assert!(!inline_item_may_intersect_surface(
            &engine.items[item],
            (0.0, 0.0),
            None,
            100,
            1.0,
        ));
        engine.items[item].origin.1 = 500.0;
        assert!(!inline_item_may_intersect_surface(
            &engine.items[item],
            (0.0, 0.0),
            None,
            100,
            1.0,
        ));
        assert!(
            inline_item_may_intersect_surface(
                &engine.items[item],
                (0.0, -490.0),
                None,
                100,
                1.0,
            ),
            "the transform/layer translation must be applied before culling"
        );

        engine.items[item]
            .relative_owner_ranges
            .push(RelativeOwnerTextRange {
                start: 0,
                end: 1,
                offset: (0.0, -490.0),
            });
        assert!(
            inline_item_may_intersect_surface(
                &engine.items[item],
                (0.0, 0.0),
                None,
                100,
                1.0,
            ),
            "a relatively positioned inline may move visible ink into the surface"
        );
        assert!(!inline_item_may_intersect_surface(
            &engine.items[item],
            (0.0, 0.0),
            Some(Rect {
                x: 0.0,
                y: 200.0,
                width: 100.0,
                height: 20.0,
            }),
            100,
            1.0,
        ));
    }

    #[test]
    fn system_ui_resolves_in_stack_order_to_chromium_linux_face() {
        let stack = "system-ui, -apple-system, \"Segoe UI\", Roboto, \
                     \"Helvetica Neue\", \"Noto Sans\", \"Liberation Sans\", \
                     Arial, sans-serif";
        assert_eq!(resolve_font_family(Some(stack)), SYSTEM_FAMILY);

        let mut engine = TextEngine::new();
        let system = resolve_loaded_font(Some(stack), 600, false, &engine.loaded_families);
        assert_eq!(system.family.as_ref(), SYSTEM_FAMILY);
        let face = engine
            .font_system
            .db()
            .face(system.font_id.expect("bundled system-ui face"))
            .expect("system-ui database face");
        assert_eq!(
            face.weight.0, 700,
            "CSS weight 600 selects DejaVu Sans Bold just as Chromium does"
        );

        let italic = resolve_loaded_font(Some("system-ui"), 400, true, &engine.loaded_families);
        let italic_face = engine
            .font_system
            .db()
            .face(italic.font_id.expect("bundled system-ui italic fallback"))
            .expect("system-ui italic fallback database face");
        assert_eq!(italic_face.weight.0, 400);
        assert_eq!(
            italic_face.style,
            cosmic_text::fontdb::Style::Normal,
            "Chromium synthesizes system-ui italic from DejaVu Sans regular on this host"
        );

        let bold_italic =
            resolve_loaded_font(Some("system-ui"), 700, true, &engine.loaded_families);
        let bold_italic_face = engine
            .font_system
            .db()
            .face(
                bold_italic
                    .font_id
                    .expect("bundled system-ui bold italic fallback"),
            )
            .expect("system-ui bold italic fallback database face");
        assert_eq!(bold_italic_face.weight.0, 700);
        assert_eq!(
            bold_italic_face.style,
            cosmic_text::fontdb::Style::Normal,
            "Chromium synthesizes system-ui bold italic from DejaVu Sans Bold on this host"
        );

        let tree = obscura_dom::parse_html("<p id='copy'>Italic system UI</p>");
        let copy = tree.get_element_by_id("copy").unwrap();
        let mut style = LayoutStyle::default();
        style.display = Display::Block;
        style.font_family = Some("system-ui".to_string());
        style.font_style_italic = Some(true);
        style.font_size = Some(32.0);
        let item = engine
            .try_build(&tree, copy, &HashMap::from([(copy, style)]))
            .expect("system-ui italic shapes");
        engine.measure(item, Some(400.0));
        let glyph = engine.items[item]
            .buffer
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first())
            .expect("system-ui italic glyph");
        assert!(
            glyph
                .physical((0.0, 0.0), 1.0)
                .cache_key
                .flags
                .contains(CacheKeyFlags::FAKE_ITALIC),
            "the normal DejaVu resource must retain the requested synthetic slant"
        );

        let arial = resolve_loaded_font(
            Some("Arial, sans-serif"),
            600,
            false,
            &engine.loaded_families,
        );
        assert_eq!(arial.family.as_ref(), FAMILY);
    }

    #[test]
    fn normal_line_height_grid_fits_each_face_metric() {
        // Values measured from Chromium 145 using the same bundled Linux
        // platform faces. Small fractional sizes expose the difference from
        // rounding a single 1.15 multiplier.
        assert_eq!(
            normal_line_height(9.3333, bundled_face_metrics(FAMILY)),
            10.0
        );
        assert_eq!(normal_line_height(12.0, bundled_face_metrics(FAMILY)), 14.0);
        assert_eq!(
            normal_line_height(13.0, bundled_face_metrics(SERIF_FAMILY)),
            16.0
        );
        assert_eq!(
            normal_line_height(13.0, bundled_face_metrics(MONO_FAMILY)),
            15.0
        );
        // Poppins's 1000-unit hhea metrics are 1050/-350 with a 100-unit
        // gap. A 64px normal line is therefore 67 + 22 + 6 = 95px, rather
        // than the 74px produced by the old generic-sans constants.
        assert_eq!(
            normal_line_height(
                64.0,
                FaceMetrics {
                    ascent: 1050.0,
                    descent: 350.0,
                    line_gap: 100.0,
                    units_per_em: 1000.0,
                },
            ),
            95.0
        );
    }

    #[test]
    fn declared_web_family_keeps_the_loaded_faces_line_metrics() {
        let engine = TextEngine::new_with_web_fonts(&[WebFont {
            data: FALLBACK.to_vec(),
            family: Some("Page Face".to_string()),
            weight: Some((400, 400)),
            italic: Some(false),
        }]);
        let resolved = resolve_loaded_font(Some("Page Face"), 400, false, &engine.loaded_families);
        let expected = font_metrics(&engine.font_system.db(), resolved.font_id.unwrap()).unwrap();
        assert_eq!(resolved.metrics, expected);

        let style = LayoutStyle {
            font_family: Some("Page Face".to_string()),
            font_size: Some(64.0),
            line_height: Some(crate::LineHeight::Normal),
            ..Default::default()
        };
        assert_eq!(
            used_line_height_for_font(&style, &resolved),
            normal_line_height(64.0, expected)
        );
    }

    #[test]
    fn replaced_percentage_math_only_zeroes_inline_min_content() {
        for expression_index in [0, 4] {
            let mut engine = TextEngine::new();
            let mut style = LayoutStyle::default();
            style.size_expressions[expression_index] = Some("calc(100% - 1px)".to_string());
            let item = engine.register_replaced(800.0, 400.0, &style);
            let unknown = taffy::Size {
                width: None,
                height: None,
            };
            let min_content = engine.measure_taffy(
                item,
                unknown,
                taffy::Size {
                    width: taffy::AvailableSpace::MinContent,
                    height: taffy::AvailableSpace::MaxContent,
                },
            );
            let max_content = engine.measure_taffy(
                item,
                unknown,
                taffy::Size {
                    width: taffy::AvailableSpace::MaxContent,
                    height: taffy::AvailableSpace::MaxContent,
                },
            );
            let final_size = engine.measure_taffy(
                item,
                taffy::Size {
                    width: Some(300.0),
                    height: None,
                },
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(300.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
            );

            assert_eq!(min_content.width, 0.0);
            assert_eq!(max_content.width, 800.0);
            assert_eq!(max_content.height, 400.0);
            assert_eq!(final_size.width, 300.0);
            assert_eq!(final_size.height, 150.0);
        }
    }

    #[test]
    fn partial_replaced_intrinsics_use_the_default_object_axis() {
        let unknown = taffy::Size {
            width: None,
            height: None,
        };
        let style = LayoutStyle::default();
        let width_only = ReplacedItem::from_intrinsic(
            crate::ReplacedIntrinsic {
                width: Some(100.0),
                height: None,
                ratio: None,
            },
            &style,
        )
        .size(unknown);
        assert_eq!((width_only.width, width_only.height), (100.0, 150.0));

        let height_only = ReplacedItem::from_intrinsic(
            crate::ReplacedIntrinsic {
                width: None,
                height: Some(100.0),
                ratio: None,
            },
            &style,
        )
        .size(unknown);
        assert_eq!((height_only.width, height_only.height), (300.0, 100.0));
    }

    #[test]
    fn both_auto_replaced_constraints_transfer_through_preferred_ratio() {
        let unknown = taffy::Size {
            width: None,
            height: None,
        };
        let measure =
            |style: &LayoutStyle| {
                ReplacedItem::from_style(512.0, 323.0, style).size(unknown)
            };

        let max_height = LayoutStyle {
            max_height: Dimension::Px(128.0),
            ..Default::default()
        };
        let size = measure(&max_height);
        assert!((size.width - 202.89783).abs() < 0.001, "{size:?}");
        assert!((size.height - 128.0).abs() < 0.001, "{size:?}");

        let max_width = LayoutStyle {
            max_width: Dimension::Px(256.0),
            ..Default::default()
        };
        let size = measure(&max_width);
        assert!((size.width - 256.0).abs() < 0.001, "{size:?}");
        assert!((size.height - 161.5).abs() < 0.001, "{size:?}");

        let min_width = LayoutStyle {
            min_width: Dimension::Px(1024.0),
            ..Default::default()
        };
        let size = measure(&min_width);
        assert!((size.width - 1024.0).abs() < 0.001, "{size:?}");
        assert!((size.height - 646.0).abs() < 0.001, "{size:?}");

        let min_height = LayoutStyle {
            min_height: Dimension::Px(646.0),
            ..Default::default()
        };
        let size = measure(&min_height);
        assert!((size.width - 1024.0).abs() < 0.001, "{size:?}");
        assert!((size.height - 646.0).abs() < 0.001, "{size:?}");

        // A non-intrinsic authored ratio is the preferred ratio used for
        // transfer, rather than the decoded resource's natural ratio.
        let authored_ratio = LayoutStyle {
            max_height: Dimension::Px(128.0),
            aspect_ratio: Some(4.0),
            ..Default::default()
        };
        let size = measure(&authored_ratio);
        assert!((size.width - 512.0).abs() < 0.001, "{size:?}");
        assert!((size.height - 128.0).abs() < 0.001, "{size:?}");
    }

    #[test]
    fn missing_font_weights_follow_css_search_order() {
        assert_eq!(match_font_weight(500, &[400, 700]), 400);
        assert_eq!(match_font_weight(600, &[400, 500, 700]), 700);
        assert_eq!(match_font_weight(300, &[100, 400, 700]), 100);
        assert_eq!(match_font_weight(800, &[400, 700]), 700);
        assert_eq!(match_font_weight(500, &[400, 500, 700]), 500);
    }

    #[test]
    fn declared_family_selects_a_face_with_a_different_internal_name() {
        let family = LoadedFamily {
            faces: vec![
                LoadedFace {
                    name: Arc::from("Poppins"),
                    font_id: None,
                    metrics: bundled_face_metrics(FAMILY),
                    min_weight: 400,
                    max_weight: 400,
                    italic: false,
                },
                LoadedFace {
                    name: Arc::from("Poppins Medium"),
                    font_id: None,
                    metrics: bundled_face_metrics(FAMILY),
                    min_weight: 500,
                    max_weight: 500,
                    italic: false,
                },
                LoadedFace {
                    name: Arc::from("Poppins"),
                    font_id: None,
                    metrics: bundled_face_metrics(FAMILY),
                    min_weight: 700,
                    max_weight: 700,
                    italic: false,
                },
            ],
        };
        let loaded = HashMap::from([("poppins".to_string(), family)]);
        let medium = resolve_loaded_font(Some("Poppins, sans-serif"), 500, false, &loaded);
        assert_eq!(medium.family.as_ref(), "Poppins Medium");
        let semibold = resolve_loaded_font(Some("Poppins"), 600, false, &loaded);
        assert_eq!(semibold.family.as_ref(), "Poppins");
    }

    #[test]
    fn ranged_face_shapes_at_its_font_database_weight() {
        let tree = obscura_dom::parse_html("<p id='copy'>Variable family</p>");
        let copy = tree.get_element_by_id("copy").unwrap();
        let mut style = LayoutStyle::default();
        style.display = Display::Block;
        style.font_family = Some("test variable".to_string());
        style.font_weight = Some("700".to_string());
        style.font_size = Some(32.0);
        let styles = HashMap::from([(copy, style)]);

        let mut engine = TextEngine::new();
        let regular_id = engine
            .font_system
            .db()
            .faces()
            .find(|face| {
                face.weight.0 == 400
                    && matches!(face.style, cosmic_text::fontdb::Style::Normal)
                    && face.families.iter().any(|(name, _)| name == FAMILY)
            })
            .map(|face| face.id)
            .unwrap();
        engine.loaded_families.insert(
            "test variable".to_string(),
            LoadedFamily {
                faces: vec![LoadedFace {
                    name: Arc::from(FAMILY),
                    font_id: Some(regular_id),
                    metrics: bundled_face_metrics(FAMILY),
                    min_weight: 100,
                    max_weight: 900,
                    italic: false,
                }],
            },
        );

        let item = engine.try_build(&tree, copy, &styles).unwrap();
        engine.measure(item, None);
        let font_id = engine.items[item]
            .buffer
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first())
            .map(|glyph| glyph.font_id)
            .unwrap();
        let face = engine.font_system.db().face(font_id).unwrap();
        assert!(
            face.families.iter().any(|(name, _)| name == FAMILY),
            "authored family must not fall through to an unrelated exact-weight face"
        );
        assert_eq!(face.weight.0, 400);
    }

    #[test]
    fn descriptor_selected_resource_pins_the_exact_font_face() {
        let data = SANS_R.to_vec();
        let mut engine = TextEngine::new_with_web_fonts(&[
            WebFont {
                data: data.clone(),
                family: Some("Pinned Variable".to_string()),
                weight: Some((100, 400)),
                italic: Some(false),
            },
            WebFont {
                data,
                family: Some("Pinned Variable".to_string()),
                weight: Some((500, 900)),
                italic: Some(false),
            },
        ]);
        let loaded = &engine.loaded_families["pinned variable"];
        assert_eq!(loaded.faces.len(), 2);
        let first_id = loaded.faces[0].font_id.unwrap();
        let selected =
            resolve_loaded_font(Some("Pinned Variable"), 700, false, &engine.loaded_families);
        let selected_id = selected.font_id.unwrap();
        assert_ne!(
            first_id, selected_id,
            "the fixture must contain two distinct database resources"
        );
        let selected_font = engine
            .font_system
            .get_font(selected_id)
            .expect("the descriptor-selected database face must be loadable");
        assert_eq!(selected_font.id(), selected_id);

        let tree = obscura_dom::parse_html(
            "<p id='copy'>Build whatever you want, without touching your CSS file.</p>",
        );
        let copy = tree.get_element_by_id("copy").unwrap();
        let style = LayoutStyle {
            display: Display::Block,
            font_family: Some("Pinned Variable".to_string()),
            font_weight: Some("700".to_string()),
            font_size: Some(32.0),
            ..Default::default()
        };
        let item = engine
            .try_build(&tree, copy, &HashMap::from([(copy, style)]))
            .unwrap();
        assert_eq!(
            engine.items[item].buffer.lines[0]
                .attrs_list()
                .get_span(0)
                .font_id_opt,
            Some(selected_id),
            "the resolved face must survive rich-text span construction"
        );
        engine.measure(item, None);
        let shaped_id = engine.items[item]
            .buffer
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first())
            .map(|glyph| glyph.font_id)
            .unwrap();
        assert_eq!(
            shaped_id, selected_id,
            "family/default-weight rematching must not replace the descriptor-selected resource"
        );
    }

    #[test]
    fn ranged_face_keeps_requested_weight_as_fallback_axis_intent() {
        let loaded = HashMap::from([(
            "inter".to_string(),
            LoadedFamily {
                faces: vec![LoadedFace {
                    name: Arc::from("Inter"),
                    font_id: None,
                    metrics: bundled_face_metrics(FAMILY),
                    min_weight: 100,
                    max_weight: 900,
                    italic: false,
                }],
            },
        )]);
        let resolved = resolve_loaded_font(Some("Inter, sans-serif"), 725, false, &loaded);
        assert_eq!(resolved.family.as_ref(), "Inter");
        let style = LayoutStyle {
            font_weight: Some("725".to_string()),
            ..Default::default()
        };
        let mut collector = Collector::new();
        let context = base_span_ctx(&style, resolved, &mut collector);
        assert_eq!(context.weight, 725);
    }

    #[test]
    fn glyph_metadata_keeps_fill_and_variations_independent() {
        let mut variations = FontVariations::new();
        variations.set(VariationTag::new(b"wght"), 725.0);
        let attrs = SpanAttrs {
            font_size: 16.0,
            line_height: 18.0,
            letter_spacing: 0.0,
            letter_spacing_non_normal: false,
            weight: 400,
            optical_sizing: crate::FontOpticalSizing::Auto,
            font_id: None,
            variations: Some(Arc::new(variations)),
            italic: false,
            synthetic_italic: false,
            underline: true,
            color: [1, 2, 3, 255],
            family: Arc::from(FAMILY),
            clip_fill: Some(37),
            white_space: crate::WhiteSpace::Normal,
            overflow_wrap: crate::OverflowWrap::Normal,
            word_break: crate::WordBreak::Normal,
        };
        let shaped = attrs.to_attrs(42);
        assert_ne!(shaped.metadata & META_UNDERLINE, 0);
        assert_eq!(metadata_fill(shaped.metadata), Some(37));
        assert_eq!(metadata_variation(shaped.metadata), Some(41));
        assert_eq!(
            shaped
                .font_variations
                .iter()
                .find(|variation| variation.tag == VariationTag::new(b"wght"))
                .map(|variation| variation.value.0),
            Some(725.0)
        );
    }

    #[test]
    fn static_text_keeps_the_zero_allocation_variation_path() {
        let tree = obscura_dom::parse_html("<p id='copy'>ordinary text</p>");
        let copy = tree.get_element_by_id("copy").unwrap();
        let mut style = LayoutStyle::default();
        style.display = Display::Block;
        let static_font = ResolvedFont {
            family: Arc::from(FAMILY),
            font_id: None,
            metrics: bundled_face_metrics(FAMILY),
            synthetic_italic: false,
        };
        assert!(resolved_font_variations(&style, &static_font).is_none());
        let styles = HashMap::from([(copy, style)]);
        let mut engine = TextEngine::new();
        let item = engine.try_build(&tree, copy, &styles).unwrap();

        assert!(engine.items[item].variation_sets.is_empty());
        assert_eq!(engine.items[item].variation_sets.capacity(), 0);
    }

    #[test]
    fn programmatic_nonfinite_variations_never_reach_shape_or_raster() {
        let style = LayoutStyle {
            font_variation_settings: Some(vec![
                crate::FontVariationSetting {
                    tag: *b"opsz",
                    value: f32::NAN,
                },
                crate::FontVariationSetting {
                    tag: *b"wght",
                    value: f32::INFINITY,
                },
            ]),
            ..Default::default()
        };
        let font = ResolvedFont {
            family: Arc::from(FAMILY),
            font_id: None,
            metrics: bundled_face_metrics(FAMILY),
            synthetic_italic: false,
        };
        assert!(resolved_font_variations(&style, &font).is_none());
    }

    #[test]
    fn non_normal_letter_spacing_reaches_shaping_features() {
        let span = SpanAttrs {
            font_size: 20.0,
            line_height: 24.0,
            letter_spacing: 2.0,
            letter_spacing_non_normal: true,
            weight: 400,
            optical_sizing: crate::FontOpticalSizing::Auto,
            font_id: None,
            variations: None,
            italic: false,
            synthetic_italic: false,
            underline: false,
            color: [0, 0, 0, 255],
            family: Arc::from(FAMILY),
            clip_fill: None,
            white_space: crate::WhiteSpace::Normal,
            overflow_wrap: crate::OverflowWrap::Normal,
            word_break: crate::WordBreak::Normal,
        };
        let attrs = span.to_attrs(1);
        assert_eq!(attrs.letter_spacing_opt.unwrap().0, 0.1);
        assert!(attrs.font_features.features.iter().any(|feature| {
            feature.tag == FeatureTag::STANDARD_LIGATURES && feature.value == 0
        }));
        assert!(attrs.font_features.features.iter().any(|feature| {
            feature.tag == FeatureTag::CONTEXTUAL_LIGATURES && feature.value == 0
        }));

        let mut zero = span;
        zero.letter_spacing = 0.0;
        assert!(zero.to_attrs(1).font_features.features.is_empty());
    }

    #[test]
    fn keep_all_never_inserts_controls_inside_grapheme_clusters() {
        let source = "각각 か\u{3099}か\u{3099} 👨‍👩‍👧‍👦 👍🏽";
        let tree = obscura_dom::parse_html(&format!("<p id='copy'>{source}</p>"));
        let copy = tree.get_element_by_id("copy").unwrap();
        let base = LayoutStyle {
            display: Display::Block,
            font_size: Some(40.0),
            line_height: Some(crate::LineHeight::Px(48.0)),
            ..Default::default()
        };
        let mut keep = base.clone();
        keep.word_break = Some(crate::WordBreak::KeepAll);

        let mut normal_engine = TextEngine::new();
        let normal_item = normal_engine
            .try_build(&tree, copy, &HashMap::from([(copy, base)]))
            .unwrap();
        let normal_size = normal_engine.measure(normal_item, None);

        let mut keep_engine = TextEngine::new();
        let keep_item = keep_engine
            .try_build(&tree, copy, &HashMap::from([(copy, keep)]))
            .unwrap();
        let keep_size = keep_engine.measure(keep_item, None);

        assert_eq!(keep_engine.item_text(keep_item), source);
        assert_eq!(keep_size, normal_size);
    }

    fn variable_font_fixture() -> Vec<u8> {
        let encoded: String = include_str!("../tests/fonts/obscura-vf-test.woff2.b64")
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        let compressed = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("decode variable-font fixture");
        wuff::decompress_woff2(&compressed).expect("decompress variable-font fixture")
    }

    #[test]
    fn variable_font_multi_axis_shaping_preserves_space_advance() {
        let mut engine = TextEngine::new_with_web_fonts(&[WebFont {
            data: variable_font_fixture(),
            family: Some("Obscura VF Test".into()),
            weight: Some((100, 900)),
            italic: Some(false),
        }]);
        let tree = obscura_dom::parse_html("<p id='copy'>Tools and</p>");
        let copy = tree.get_element_by_id("copy").unwrap();
        let mut style = LayoutStyle::default();
        style.display = Display::Block;
        style.font_family = Some("Obscura VF Test".into());
        style.font_weight = Some("700".into());
        style.font_size = Some(32.0);
        style.font_optical_sizing = Some(crate::FontOpticalSizing::Auto);
        let item = engine
            .try_build(&tree, copy, &HashMap::from([(copy, style)]))
            .unwrap();
        engine.finalize(item, (0.0, 0.0), 600.0, None);
        let shaped_text = engine.item_text(item).to_string();
        let space_advance = engine.items[item]
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .find(|glyph| &shaped_text[glyph.start..glyph.end] == " ")
            .map(|glyph| glyph.w)
            .expect("shaped space glyph");
        assert!(
            space_advance > 5.0,
            "setting wght and opsz must not repeatedly remap avar coordinates and collapse the space advance: {space_advance}"
        );
    }

    fn isolated_fallback_engine() -> (TextEngine, cosmic_text::fontdb::ID, cosmic_text::fontdb::ID)
    {
        let mut engine = TextEngine::new_with_web_fonts(&[
            WebFont {
                data: include_bytes!("../../../vendor/cosmic-text/fonts/NotoSansArabic.ttf")
                    .to_vec(),
                family: Some("Static Primary".to_string()),
                weight: Some((400, 400)),
                italic: Some(false),
            },
            WebFont {
                data: variable_font_fixture(),
                family: Some("Variable Fallback".to_string()),
                weight: Some((100, 900)),
                italic: Some(false),
            },
        ]);
        let static_id = engine.loaded_families["static primary"].faces[0]
            .font_id
            .unwrap();
        let variable_id = engine.loaded_families["variable fallback"].faces[0]
            .font_id
            .unwrap();
        let remove = engine
            .font_system
            .db()
            .faces()
            .map(|face| face.id)
            .filter(|id| *id != static_id && *id != variable_id)
            .collect::<Vec<_>>();
        let db = engine.font_system.db_mut();
        for id in remove {
            db.remove_face(id);
        }
        (engine, static_id, variable_id)
    }

    fn render_variable_fallback(
        optical_sizing: crate::FontOpticalSizing,
    ) -> (
        cosmic_text::fontdb::ID,
        std::collections::HashMap<[u8; 4], std::collections::HashSet<u32>>,
    ) {
        let (mut engine, _, variable_id) = isolated_fallback_engine();
        let tree = obscura_dom::parse_html("<p id='copy'>A</p>");
        let copy = tree.get_element_by_id("copy").unwrap();
        let mut style = LayoutStyle::default();
        style.display = Display::Block;
        style.font_family = Some("Static Primary".to_string());
        style.font_weight = Some("900".to_string());
        style.font_size = Some(40.0);
        style.font_optical_sizing = Some(optical_sizing);
        let item = engine
            .try_build(&tree, copy, &HashMap::from([(copy, style)]))
            .unwrap();
        engine.measure(item, Some(100.0));
        engine.finalize(item, (0.0, 0.0), 100.0, None);
        let shaped_id = engine.items[item]
            .buffer
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first())
            .map(|glyph| glyph.font_id)
            .unwrap();
        assert_eq!(shaped_id, variable_id);

        let mut pixmap = tiny_skia::Pixmap::new(100, 80).unwrap();
        engine.paint_item(item, &mut pixmap, (0.0, 0.0));
        let mut axes = std::collections::HashMap::<[u8; 4], std::collections::HashSet<u32>>::new();
        for key in engine.variable_swash.images.keys() {
            for variation in key.variations.iter() {
                axes.entry(*variation.tag.as_bytes())
                    .or_default()
                    .insert(variation.value.0.to_bits());
            }
        }
        (shaped_id, axes)
    }

    #[test]
    fn automatic_axes_follow_the_actual_variable_fallback_face() {
        let (_, auto_axes) = render_variable_fallback(crate::FontOpticalSizing::Auto);
        let (_, none_axes) = render_variable_fallback(crate::FontOpticalSizing::None);

        assert_eq!(
            auto_axes.get(b"wght"),
            Some(&std::collections::HashSet::from([900.0f32.to_bits()]))
        );
        assert_eq!(
            none_axes.get(b"wght"),
            Some(&std::collections::HashSet::from([900.0f32.to_bits()]))
        );
        assert_eq!(
            auto_axes.get(b"opsz"),
            // The fixture's opsz range ends at 32; CSS's automatic 40px
            // coordinate must be clamped against the actual fallback face.
            Some(&std::collections::HashSet::from([32.0f32.to_bits()]))
        );
        assert!(!none_axes.contains_key(b"opsz"));
    }

    #[test]
    fn unsupported_and_clamped_axes_share_effective_raster_identity() {
        let (mut engine, _, variable_id) = isolated_fallback_engine();
        let first = {
            let mut settings = FontVariations::new();
            settings
                .set(VariationTag::new(b"NOPE"), 1.0)
                .set(VariationTag::new(b"wght"), 5_000.0);
            Arc::new(settings)
        };
        let second = {
            let mut settings = FontVariations::new();
            settings
                .set(VariationTag::new(b"NOPE"), 999.0)
                .set(VariationTag::new(b"wght"), 900.0);
            Arc::new(settings)
        };
        let first = engine
            .variable_swash
            .effective_variations(
                &mut engine.font_system,
                variable_id,
                Some(400.0),
                Some(40.0),
                false,
                Some(first),
            )
            .unwrap();
        let second = engine
            .variable_swash
            .effective_variations(
                &mut engine.font_system,
                variable_id,
                Some(400.0),
                Some(40.0),
                false,
                Some(second),
            )
            .unwrap();

        assert_eq!(first, second);
        assert!(first
            .iter()
            .all(|variation| variation.tag != VariationTag::new(b"NOPE")));
        assert_eq!(
            first
                .iter()
                .find(|variation| variation.tag == VariationTag::new(b"wght"))
                .map(|variation| variation.value.0),
            Some(900.0)
        );
    }

    fn render_variable_weight(weight: u16) -> ((f32, f32), u64, Vec<u16>) {
        let mut engine = TextEngine::new_with_web_fonts(&[WebFont {
            data: variable_font_fixture(),
            family: Some("Obscura VF Test".to_string()),
            weight: Some((100, 900)),
            italic: Some(false),
        }]);
        let tree = obscura_dom::parse_html("<p id='copy'>MMMMMMMM</p>");
        let copy = tree.get_element_by_id("copy").unwrap();
        let mut style = LayoutStyle::default();
        style.display = Display::Block;
        style.font_family = Some("Obscura VF Test".to_string());
        style.font_weight = Some(weight.to_string());
        style.font_size = Some(64.0);
        style.line_height = Some(crate::LineHeight::Px(80.0));
        let styles = HashMap::from([(copy, style)]);
        let item = engine.try_build(&tree, copy, &styles).unwrap();
        let geometry = engine.measure(item, Some(600.0));
        engine.finalize(item, (0.0, 0.0), 600.0, None);
        let mut pixmap = tiny_skia::Pixmap::new(600, 100).unwrap();
        engine.paint_item(item, &mut pixmap, (0.0, 0.0));
        let ink = pixmap
            .pixels()
            .iter()
            .map(|pixel| u64::from(pixel.alpha()))
            .sum();
        let mut cached_weights: Vec<_> = engine
            .variable_swash
            .images
            .keys()
            .filter_map(|key| {
                key.variations
                    .iter()
                    .find(|variation| variation.tag == VariationTag::new(b"wght"))
                    .map(|variation| variation.value.0 as u16)
            })
            .collect();
        cached_weights.sort_unstable();
        cached_weights.dedup();
        (geometry, ink, cached_weights)
    }

    #[test]
    fn shaped_text_preserves_authored_alpha_through_mask_rasterization() {
        let render_alpha = |alpha: u8, underline: bool| {
            let tree = obscura_dom::parse_html("<p id='copy'>Alpha</p>");
            let copy = tree.get_element_by_id("copy").unwrap();
            let mut style = LayoutStyle::default();
            style.display = Display::Block;
            style.font_size = Some(32.0);
            style.line_height = Some(crate::LineHeight::Px(40.0));
            style.color = Some([0, 0, 0, alpha]);
            style.underline = Some(underline);
            let styles = HashMap::from([(copy, style)]);
            let mut engine = TextEngine::new();
            let item = engine.try_build(&tree, copy, &styles).unwrap();
            engine.finalize(item, (0.0, 0.0), 160.0, None);
            let mut pixmap = tiny_skia::Pixmap::new(160, 48).unwrap();
            engine.paint_item(item, &mut pixmap, (0.0, 0.0));
            pixmap
                .pixels()
                .iter()
                .map(|pixel| u64::from(pixel.alpha()))
                .sum::<u64>()
        };

        assert_eq!(
            render_alpha(0, true),
            0,
            "transparent glyphs and decorations must not paint"
        );
        let half = render_alpha(128, false);
        let opaque = render_alpha(255, false);
        let ratio = half * 100 / opaque;
        assert!(
            (48..=52).contains(&ratio),
            "50% CSS alpha must retain half the opaque coverage: half={half}, opaque={opaque}"
        );
    }

    #[test]
    fn variable_wght_changes_shape_and_true_outline() {
        let (regular_geometry, regular_ink, regular_cache) = render_variable_weight(400);
        let (black_geometry, black_ink, black_cache) = render_variable_weight(900);
        assert!(regular_geometry.0 > 0.0 && black_geometry.0 > 0.0);
        assert!(
            black_ink > regular_ink * 13 / 10,
            "wght=900 should carry substantially more raster ink: {regular_ink} vs {black_ink}"
        );
        assert_eq!(regular_cache, vec![400]);
        assert_eq!(black_cache, vec![900]);
    }

    #[test]
    fn variable_glyph_cache_keys_include_weight_axis() {
        let mut engine = TextEngine::new_with_web_fonts(&[WebFont {
            data: variable_font_fixture(),
            family: Some("Obscura VF Test".to_string()),
            weight: Some((100, 900)),
            italic: Some(false),
        }]);
        let tree = obscura_dom::parse_html(
            "<p id='copy'><span id='regular'>M</span><span id='black'>M</span></p>",
        );
        let copy = tree.get_element_by_id("copy").unwrap();
        let regular = tree.get_element_by_id("regular").unwrap();
        let black = tree.get_element_by_id("black").unwrap();
        let mut base = LayoutStyle::default();
        base.display = Display::Block;
        base.font_family = Some("Obscura VF Test".to_string());
        base.font_size = Some(64.0);
        base.line_height = Some(crate::LineHeight::Px(80.0));
        let mut regular_style = base.clone();
        regular_style.display = Display::Inline;
        regular_style.font_weight = Some("400".to_string());
        let mut black_style = regular_style.clone();
        black_style.font_weight = Some("900".to_string());
        let styles = HashMap::from([(copy, base), (regular, regular_style), (black, black_style)]);
        let item = engine.try_build(&tree, copy, &styles).unwrap();
        engine.measure(item, Some(200.0));
        engine.finalize(item, (0.0, 0.0), 200.0, None);
        let mut pixmap = tiny_skia::Pixmap::new(200, 100).unwrap();
        engine.paint_item(item, &mut pixmap, (0.0, 0.0));
        let weights: std::collections::HashSet<_> = engine
            .variable_swash
            .images
            .keys()
            .filter_map(|key| {
                key.variations
                    .iter()
                    .find(|variation| variation.tag == VariationTag::new(b"wght"))
                    .map(|variation| variation.value.0 as u16)
            })
            .collect();
        assert_eq!(weights, std::collections::HashSet::from([400, 900]));
    }

    fn render_tailwind_optical_heading(
        optical_sizing: crate::FontOpticalSizing,
        explicit_opsz: Option<f32>,
        width: f32,
    ) -> ((f32, f32), usize, std::collections::HashSet<u32>) {
        let mut engine = TextEngine::new_with_web_fonts(&[WebFont {
            data: variable_font_fixture(),
            family: Some("Obscura VF Test".to_string()),
            weight: Some((100, 900)),
            italic: Some(false),
        }]);
        let tree = obscura_dom::parse_html(
            "<h2 id='copy'>Build whatever you want, without touching your CSS file.</h2>",
        );
        let copy = tree.get_element_by_id("copy").unwrap();
        let mut style = LayoutStyle::default();
        style.display = Display::Block;
        style.font_family = Some("Obscura VF Test".to_string());
        style.font_weight = Some("500".to_string());
        style.font_size = Some(40.0);
        style.line_height = Some(crate::LineHeight::Px(40.0));
        style.letter_spacing = Some(-2.0);
        style.letter_spacing_non_normal = Some(true);
        style.font_optical_sizing = Some(optical_sizing);
        style.font_variation_settings = Some(
            explicit_opsz
                .map(|value| {
                    vec![crate::FontVariationSetting {
                        tag: *b"opsz",
                        value,
                    }]
                })
                .unwrap_or_default(),
        );
        let styles = HashMap::from([(copy, style)]);
        let item = engine.try_build(&tree, copy, &styles).unwrap();
        let geometry = engine.measure(item, Some(width));
        let lines = engine.items[item].buffer.layout_runs().count();
        engine.finalize(item, (0.0, 0.0), width, None);
        let mut pixmap = tiny_skia::Pixmap::new(512, 140).unwrap();
        engine.paint_item(item, &mut pixmap, (0.0, 0.0));
        let optical_values = engine
            .variable_swash
            .images
            .keys()
            .filter_map(|key| {
                key.variations
                    .iter()
                    .find(|variation| variation.tag == VariationTag::new(b"opsz"))
                    .map(|variation| variation.value.0.to_bits())
            })
            .collect();
        (geometry, lines, optical_values)
    }

    #[test]
    fn optical_sizing_changes_tailwind_heading_shape_and_raster_axes() {
        let (auto_geometry, auto_lines, auto_axes) =
            render_tailwind_optical_heading(crate::FontOpticalSizing::Auto, None, 496.0);
        let (none_geometry, none_lines, none_axes) =
            render_tailwind_optical_heading(crate::FontOpticalSizing::None, None, 496.0);
        let (explicit_geometry, explicit_lines, explicit_axes) =
            render_tailwind_optical_heading(crate::FontOpticalSizing::Auto, Some(14.0), 496.0);

        assert_ne!(
            auto_geometry, none_geometry,
            "automatic opsz must affect the shaped heading advances"
        );
        assert_eq!(auto_lines, none_lines);
        assert_eq!(explicit_lines, none_lines);
        assert_ne!(
            explicit_geometry, auto_geometry,
            "an explicit low-level opsz coordinate must override automatic sizing"
        );
        assert_eq!(
            auto_axes,
            std::collections::HashSet::from([32.0f32.to_bits()])
        );
        assert!(none_axes.is_empty());
        assert_eq!(
            explicit_axes,
            std::collections::HashSet::from([14.0f32.to_bits()])
        );
    }

    #[test]
    fn text_only_inline_block_keeps_an_internal_shaping_context() {
        let tree = obscura_dom::parse_html("<span id='icon'>ligature_name</span>");
        let icon = tree.get_element_by_id("icon").unwrap();
        let mut style = LayoutStyle::default();
        style.display = Display::Inline;
        style.is_inline_block = true;
        let styles = HashMap::from([(icon, style)]);

        assert!(
            is_pure_text_ifc(&tree, icon, &styles),
            "atomic inline participation must not disable shaping inside the box"
        );
    }

    #[test]
    fn inline_descendant_keeps_its_computed_font_metrics() {
        let tree = obscura_dom::parse_html(
            r#"<style>
                #copy { font-size:16px; line-height:20px }
                #big { font-size:2em; line-height:1.5 }
            </style>
            <p id="copy">small <a id="big">large</a></p>"#,
        );
        let copy = tree.get_element_by_id("copy").unwrap();
        let big = tree.get_element_by_id("big").unwrap();
        let laid = crate::dom::layout_dom(&tree, (500.0, 200.0));

        assert_eq!(laid.styles[&big].font_size, Some(32.0));
        let item = laid.ifc_items[&copy];
        let glyph_sizes = laid.text_engine.items[item]
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.font_size))
            .collect::<Vec<_>>();
        assert!(
            glyph_sizes.iter().any(|size| (*size - 16.0).abs() < 0.01),
            "base text should shape at 16px: {glyph_sizes:?}"
        );
        assert!(
            glyph_sizes.iter().any(|size| (*size - 32.0).abs() < 0.01),
            "inline descendant should shape at 32px: {glyph_sizes:?}"
        );
    }

    #[test]
    fn preformatted_newlines_preserve_authored_line_count() {
        let tree = obscura_dom::parse_html(
            r#"<style>
                html,body { margin:0 }
                .box { margin:0; width:200px; font:16px/24px monospace }
                #explicit { white-space:pre-wrap }
                #normal { white-space:normal }
            </style>
            <pre id="ua" class="box"><code>alpha
beta
gamma</code></pre>
            <div id="explicit" class="box">alpha
beta
gamma</div>
            <div id="normal" class="box">alpha
beta
gamma</div>"#,
        );
        let laid = crate::dom::layout_dom(&tree, (400.0, 300.0));
        let rect = |id| laid.rects[&tree.get_element_by_id(id).unwrap()];

        assert_eq!(
            laid.styles[&tree.get_element_by_id("ua").unwrap()].white_space,
            Some(crate::WhiteSpace::Pre)
        );
        assert_eq!(
            laid.styles[&tree.get_element_by_id("explicit").unwrap()].white_space,
            Some(crate::WhiteSpace::PreWrap)
        );
        assert!((rect("ua").height - 72.0).abs() < 0.01, "{:?}", rect("ua"));
        assert!(
            (rect("explicit").height - 72.0).abs() < 0.01,
            "{:?}",
            rect("explicit")
        );
        assert!(
            (rect("normal").height - 24.0).abs() < 0.01,
            "{:?}",
            rect("normal")
        );
    }

    fn shaped_line_texts(buffer: &Buffer) -> Vec<String> {
        buffer
            .layout_runs()
            .map(|run| {
                let start = run
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.start)
                    .min()
                    .unwrap_or(0);
                let end = run
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.end)
                    .max()
                    .unwrap_or(start);
                run.text[start..end].trim().to_string()
            })
            .collect()
    }

    #[test]
    fn text_indent_changes_only_the_first_formatted_line() {
        let tree = obscura_dom::parse_html(
            "<p id='copy'>alpha beta gamma delta epsilon zeta eta theta</p>",
        );
        let copy = tree.get_element_by_id("copy").unwrap();
        let style = LayoutStyle {
            display: Display::Block,
            font_size: Some(16.0),
            line_height: Some(crate::LineHeight::Px(20.0)),
            text_indent: Some(Dimension::Px(40.0)),
            ..Default::default()
        };
        let mut engine = TextEngine::new();
        let item = engine
            .try_build(&tree, copy, &HashMap::from([(copy, style)]))
            .unwrap();
        engine.finalize(item, (0.0, 0.0), 150.0, None);

        let lines = shaped_line_texts(&engine.items[item].buffer);
        assert!(lines.len() >= 2, "fixture must wrap: {lines:?}");
        assert_eq!(engine.items[item].first_line_offset, 40.0);
        let runs: Vec<_> = engine.items[item].buffer.layout_runs().collect();
        let first_x = runs[0].glyphs.first().unwrap().x + engine.items[item].first_line_offset;
        let second_x = runs[1].glyphs.first().unwrap().x;
        assert!(
            (first_x - 40.0).abs() < 0.01,
            "first line must start at the authored indent: {first_x}"
        );
        assert!(
            second_x.abs() < 0.01,
            "continuation lines must return to the content edge: {second_x}"
        );

        let percentage = LayoutStyle {
            display: Display::Block,
            text_indent: Some(Dimension::Percent(0.25)),
            ..Default::default()
        };
        let percent_item = engine
            .try_build(&tree, copy, &HashMap::from([(copy, percentage)]))
            .unwrap();
        engine.finalize(percent_item, (0.0, 0.0), 200.0, None);
        assert_eq!(engine.items[percent_item].first_line_offset, 50.0);
    }

    #[test]
    fn text_wrap_balance_changes_line_grouping_without_changing_line_count() {
        let tree = obscura_dom::parse_html("<h1 id='hero'>Welcome to Mozilla</h1>");
        let hero = tree.get_element_by_id("hero").unwrap();
        let style = LayoutStyle {
            display: Display::Block,
            font_size: Some(128.0),
            line_height: Some(crate::LineHeight::Px(128.0)),
            text_wrap_style: Some(crate::TextWrapStyle::Balance),
            ..Default::default()
        };
        let mut engine = TextEngine::new();
        let item = engine
            .try_build(&tree, hero, &HashMap::from([(hero, style)]))
            .unwrap();
        let width = 912.0;
        engine.measure(item, Some(width));
        let natural_lines = shaped_line_texts(&engine.items[item].buffer);

        engine.finalize(item, (0.0, 0.0), width, None);
        let balanced_lines = shaped_line_texts(&engine.items[item].buffer);
        let balanced_width = engine.items[item].buffer.size().0.unwrap();

        assert_eq!(natural_lines, ["Welcome to", "Mozilla"]);
        assert_eq!(balanced_lines, ["Welcome", "to Mozilla"]);
        assert!(
            balanced_width < width - 1.0,
            "balance should tighten the effective wrap width: {balanced_width}"
        );
    }

    #[test]
    fn text_wrap_style_is_inherited_and_can_be_reset() {
        let tree = obscura_dom::parse_html(
            r#"<style>
                #outer { text-wrap:balance }
                #reset { text-wrap-style:auto }
            </style>
            <div id="outer">
                <h1 id="inherited">balanced heading words</h1>
                <h1 id="reset">ordinary heading words</h1>
            </div>"#,
        );
        let inherited = tree.get_element_by_id("inherited").unwrap();
        let reset = tree.get_element_by_id("reset").unwrap();
        let laid = crate::dom::layout_dom(&tree, (500.0, 300.0));

        assert_eq!(
            laid.styles[&inherited].text_wrap_style,
            Some(crate::TextWrapStyle::Balance)
        );
        assert_eq!(
            laid.styles[&reset].text_wrap_style,
            Some(crate::TextWrapStyle::Auto)
        );
    }

    #[test]
    fn clip_fill_only_for_transparent_clip_text() {
        let mut s = LayoutStyle::default();
        // Gradient + clip-to-text + transparent color: fills through the glyphs.
        s.background_clip_text = true;
        s.color = Some([0, 0, 0, 0]);
        s.background_gradient = Some((90.0, vec![(RED, None), (BLUE, None)]));
        assert!(clip_text_fill(&s).is_some());

        // Same, but opaque text: paints normally, no clip fill.
        s.color = Some([10, 20, 30, 255]);
        assert!(clip_text_fill(&s).is_none());

        // Clip-to-text off: ordinary transparent text stays invisible.
        s.color = Some([0, 0, 0, 0]);
        s.background_clip_text = false;
        assert!(clip_text_fill(&s).is_none());

        // Solid background color becomes a flat two-stop gradient.
        s.background_clip_text = true;
        s.background_gradient = None;
        s.background_color = Some([12, 34, 56, 255]);
        let fill = clip_text_fill(&s).expect("solid bg clip fill");
        assert_eq!(fill.1.len(), 2);
        assert_eq!(fill.1[0].0, [12, 34, 56, 255]);
    }

    #[test]
    fn sample_gradient_tints_left_to_right() {
        // 90deg (to right): left edge is the first stop, right edge the last.
        let fill = (90.0f32, vec![(RED, None), (BLUE, None)]);
        let left = sample_gradient(&fill, 0.0, 5.0, 100.0, 10.0);
        let right = sample_gradient(&fill, 100.0, 5.0, 100.0, 10.0);
        assert!(left[0] > left[2], "left end should be reddish: {left:?}");
        assert!(right[2] > right[0], "right end should be bluish: {right:?}");
        // A single-color list samples to that color everywhere.
        let flat = (0.0f32, vec![([7, 8, 9, 255], None)]);
        assert_eq!(sample_gradient(&flat, 3.0, 3.0, 20.0, 20.0), [7, 8, 9, 255]);
    }

    #[test]
    fn emoji_font_is_loaded_only_for_emoji_documents() {
        assert!(!text_may_need_emoji_font("Plain text and arrows ->"));
        assert!(text_may_need_emoji_font("Add ➕ or remove ➖"));
        assert!(text_may_need_emoji_font("Launch 🚀"));

        let plain = TextEngine::new_with_web_fonts_and_emoji(&[], false);
        assert!(!plain.loaded_families.contains_key("noto color emoji"));

        let mut engine = TextEngine::new_with_web_fonts_and_emoji(&[], true);
        let emoji_ids: std::collections::HashSet<_> = engine.loaded_families["noto color emoji"]
            .faces
            .iter()
            .filter_map(|face| face.font_id)
            .collect();
        let tree = obscura_dom::parse_html("<p id='copy'>➕ 😀 🚀</p>");
        let copy = tree.get_element_by_id("copy").unwrap();
        let style = LayoutStyle {
            display: Display::Block,
            font_size: Some(48.0),
            line_height: Some(crate::LineHeight::Px(64.0)),
            ..Default::default()
        };
        let item = engine
            .try_build(&tree, copy, &HashMap::from([(copy, style)]))
            .unwrap();
        engine.finalize(item, (0.0, 0.0), 300.0, None);
        let font_ids: std::collections::HashSet<_> = engine.items[item]
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .map(|glyph| glyph.font_id)
            .collect();
        assert!(
            !font_ids.is_disjoint(&emoji_ids),
            "emoji clusters must shape with the bundled color face"
        );
        let mut pixmap = tiny_skia::Pixmap::new(300, 80).unwrap();
        engine.paint_item(item, &mut pixmap, (0.0, 0.0));
        let colors: std::collections::HashSet<_> = pixmap
            .pixels()
            .iter()
            .filter(|pixel| pixel.alpha() != 0)
            .map(|pixel| (pixel.red(), pixel.green(), pixel.blue()))
            .collect();
        assert!(
            colors.len() > 20,
            "the bundled CBDT face must rasterize as color, not a monochrome mask"
        );
    }
}
