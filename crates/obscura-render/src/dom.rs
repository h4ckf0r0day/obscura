//! DOM integration: build a taffy layout tree from a live [`DomTree`], run
//! layout, and return border-box geometry keyed by [`NodeId`].
//!
//! Phase 3. Text nodes do not yet contribute to size (no inline/text layout
//! until the text/paint phase), so a leaf element with only text may have zero
//! height. Block and flex structure, plus explicit sizes and box model, are
//! correct.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use obscura_dom::tree::{DomTree, NodeId};
use taffy::prelude::*;

use crate::{to_taffy_style, Rect};

/// Text width for layout. With the `paint` feature this is exact (real glyph
/// metrics from the embedded font, shared with rasterization). Without it
/// (layout-only builds, e.g. for `getBoundingClientRect`), fall back to a
/// standard average-character-width heuristic for proportional fonts so
/// `obscura-render` compiles and gives reasonable geometry with its default
/// (lightest-weight) feature set, matching RENDER.md's documented "layout
/// (default)" mode.
#[cfg(feature = "paint")]
fn text_width(
    text: &str,
    size: f32,
    is_bold: bool,
    family: Option<&str>,
    letter_spacing: f32,
) -> f32 {
    crate::paint::measure_text(text, size, is_bold, family)
        + text.chars().filter(|c| !c.is_control()).count() as f32 * letter_spacing
}

#[cfg(not(feature = "paint"))]
fn text_width(
    text: &str,
    size: f32,
    is_bold: bool,
    _family: Option<&str>,
    letter_spacing: f32,
) -> f32 {
    const AVG_CHAR_WIDTH_EM: f32 = 0.55;
    let chars = text.chars().filter(|c| !c.is_control()).count() as f32;
    let glyph_width = chars * size * AVG_CHAR_WIDTH_EM;
    (if is_bold {
        glyph_width * 1.08
    } else {
        glyph_width
    }) + chars * letter_spacing
}

#[derive(Default)]
struct NativeButtonIntrinsicContent {
    text: String,
    atomic_width: f32,
}

/// Collect the normal-flow content that contributes to an auto-sized
/// `<button>`'s intrinsic inline size.
///
/// DOM `textContent` is deliberately the wrong abstraction here: it includes
/// text below `display:none` boxes and absolutely positioned accessibility
/// labels. Gecko gives `<button>` an ordinary block/flex/grid frame, so those
/// descendants either have no frame or are out of flow and cannot enlarge its
/// intrinsic inline size. Atomic replaced descendants (most commonly an SVG
/// icon) do contribute their definite outer inline size.
fn native_button_intrinsic_content(
    tree: &DomTree,
    root: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    font_size: f32,
) -> NativeButtonIntrinsicContent {
    fn definite_inline_size(dimension: crate::Dimension, font_size: f32) -> Option<f32> {
        match dimension {
            crate::Dimension::Px(px) => Some(px),
            crate::Dimension::Em(value) | crate::Dimension::Rem(value) => Some(value * font_size),
            crate::Dimension::Ex(value) => Some(value * font_size * 0.528_320_3),
            _ => None,
        }
        .map(|value| value.max(0.0))
    }

    fn walk(
        tree: &DomTree,
        id: NodeId,
        styles: &HashMap<NodeId, crate::LayoutStyle>,
        font_size: f32,
        content: &mut NativeButtonIntrinsicContent,
    ) {
        let Some(node) = tree.get_node(id) else {
            return;
        };
        if let Some(text) = node.text_content_of_text_node() {
            content.text.push_str(text);
            return;
        }
        let Some(element) = node.as_element() else {
            return;
        };
        let style = styles.get(&id);
        if style.is_some_and(|style| {
            style.display == crate::Display::None
                || matches!(style.position, Some(taffy::Position::Absolute))
        }) {
            return;
        }

        let is_atomic = matches!(
            element.local.as_ref(),
            "svg" | "img" | "video" | "canvas" | "iframe" | "embed" | "object"
        );
        if is_atomic {
            if let Some(style) = style {
                if let Some(width) = definite_inline_size(style.width, font_size) {
                    let horizontal_edges = style.padding.left
                        + style.padding.right
                        + style.border.left
                        + style.border.right;
                    let border_box = if style.box_sizing == crate::BoxSizing::ContentBox {
                        width + horizontal_edges
                    } else {
                        width.max(horizontal_edges)
                    };
                    content.atomic_width +=
                        border_box + style.margin.left.max(0.0) + style.margin.right.max(0.0);
                }
            }
            return;
        }

        for child in rendered_children(tree, id) {
            walk(tree, child, styles, font_size, content);
        }
    }

    let mut content = NativeButtonIntrinsicContent::default();
    for child in rendered_children(tree, root) {
        walk(tree, child, styles, font_size, &mut content);
    }
    content
}

/// One accumulated overflow clip with independent physical axes.
///
/// `None` on an axis means unbounded on that axis. This avoids representing
/// `overflow-x:clip` with an artificial enormous Y rectangle, which can
/// corrupt scrolling overflow and transformed clip intersections.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RoundedOverflowClip {
    pub rect: Rect,
    pub radii: crate::ResolvedBorderRadii,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RoundedOverflowClipChain {
    pub clip: RoundedOverflowClip,
    pub parent: Option<Arc<RoundedOverflowClipChain>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverflowClip {
    x: Option<(f32, f32)>,
    y: Option<(f32, f32)>,
    rounded: Option<Arc<RoundedOverflowClipChain>>,
    rounded_offset: (f32, f32),
}

impl OverflowClip {
    pub(crate) fn for_box(rect: &Rect, style: &crate::LayoutStyle, tx: f32, ty: f32) -> Self {
        let left = rect.x + tx + style.border.left;
        let top = rect.y + ty + style.border.top;
        let right = (rect.x + tx + rect.width - style.border.right).max(left);
        let bottom = (rect.y + ty + rect.height - style.border.bottom).max(top);
        let clips_x = style.clips_overflow_x();
        let clips_y = style.clips_overflow_y();
        let padding_rect = Rect {
            x: left,
            y: top,
            width: (right - left).max(0.0),
            height: (bottom - top).max(0.0),
        };
        let radii = style
            .border_model
            .radii
            .resolve(rect.width, rect.height)
            .inset(crate::Sides {
                top: style.border.top,
                right: style.border.right,
                bottom: style.border.bottom,
                left: style.border.left,
            });
        // A rounded corner constrains both axes. Keep the existing independent
        // rectangular representation for one-axis overflow clips; applying a
        // closed rounded path there would incorrectly bound the visible axis.
        let rounded = (clips_x && clips_y && !radii.is_zero()).then(|| {
            Arc::new(RoundedOverflowClipChain {
                clip: RoundedOverflowClip {
                    rect: padding_rect,
                    radii,
                },
                parent: None,
            })
        });
        Self {
            x: clips_x.then_some((left, right)),
            y: clips_y.then_some((top, bottom)),
            rounded,
            rounded_offset: (0.0, 0.0),
        }
    }

    pub(crate) fn intersect(self, other: Self) -> Self {
        let axis = |a: Option<(f32, f32)>, b: Option<(f32, f32)>| match (a, b) {
            (Some((a0, a1)), Some((b0, b1))) => {
                let start = a0.max(b0);
                Some((start, a1.min(b1).max(start)))
            }
            (Some(bounds), None) | (None, Some(bounds)) => Some(bounds),
            (None, None) => None,
        };
        let mut rounded = self.rounded;
        let mut other_clips = Vec::new();
        let mut current = other.rounded.as_deref();
        while let Some(node) = current {
            let mut clip = node.clip;
            clip.rect.x += other.rounded_offset.0 - self.rounded_offset.0;
            clip.rect.y += other.rounded_offset.1 - self.rounded_offset.1;
            other_clips.push(clip);
            current = node.parent.as_deref();
        }
        for clip in other_clips.into_iter().rev() {
            rounded = Some(Arc::new(RoundedOverflowClipChain {
                clip,
                parent: rounded,
            }));
        }
        Self {
            x: axis(self.x, other.x),
            y: axis(self.y, other.y),
            rounded,
            rounded_offset: self.rounded_offset,
        }
    }

    pub(crate) fn intersect_rect(&self, rect: &Rect) -> Option<Rect> {
        let left = self.x.map_or(rect.x, |(start, _)| rect.x.max(start));
        let right = self.x.map_or(rect.x + rect.width, |(_, end)| {
            (rect.x + rect.width).min(end)
        });
        let top = self.y.map_or(rect.y, |(start, _)| rect.y.max(start));
        let bottom = self.y.map_or(rect.y + rect.height, |(_, end)| {
            (rect.y + rect.height).min(end)
        });
        (right > left && bottom > top).then_some(Rect {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }

    pub(crate) fn translate(&mut self, dx: f32, dy: f32) {
        if let Some((start, end)) = &mut self.x {
            *start += dx;
            *end += dx;
        }
        if let Some((start, end)) = &mut self.y {
            *start += dy;
            *end += dy;
        }
        self.rounded_offset.0 += dx;
        self.rounded_offset.1 += dy;
    }

    pub(crate) fn rounded_chain(&self) -> Option<&Arc<RoundedOverflowClipChain>> {
        self.rounded.as_ref()
    }

    pub(crate) fn rounded_offset(&self) -> (f32, f32) {
        self.rounded_offset
    }

    pub(crate) fn viewport_rect(&self, viewport: (f32, f32)) -> Rect {
        let (left, right) = self.x.unwrap_or((0.0, viewport.0));
        let (top, bottom) = self.y.unwrap_or((0.0, viewport.1));
        Rect {
            x: left,
            y: top,
            width: (right - left).max(0.0),
            height: (bottom - top).max(0.0),
        }
    }
}

/// Per-element border boxes after layout, in viewport coordinates.
pub struct DomLayout {
    pub rects: HashMap<NodeId, Rect>,
    /// Per-line border-box fragments for ordinary non-replaced inline
    /// elements. `rects` retains their union for `getBoundingClientRect()`;
    /// this list is the source for background/border painting and
    /// `getClientRects()` as wrapping support is extended.
    pub inline_fragments: HashMap<NodeId, Vec<Rect>>,
    pub styles: HashMap<NodeId, crate::LayoutStyle>,
    /// Computed custom properties in force for each element. Values are
    /// reference-counted because the overwhelmingly common case is inheritance
    /// without an override: an entire subtree can share one cascade map while
    /// CSSOM still exposes every inherited `--token` through
    /// `getComputedStyle()`.
    pub custom_properties: HashMap<NodeId, std::rc::Rc<HashMap<String, String>>>,
    /// The per-axis clip inherited from ancestor non-visible overflow, keyed
    /// per node, in SCREEN space (the clip owner's box shifted by the owner's
    /// accumulated translate; see `resolve_clip_rects`). `None` means
    /// unclipped. Does not include the node's own overflow (that only clips
    /// its children, not itself).
    pub clip_rects: HashMap<NodeId, Option<OverflowClip>>,
    /// Accumulated `transform: translate()` per node (own + ancestors), in CSS
    /// px. Only nodes with a non-zero accumulation are present; paint shifts
    /// each box by this to reach screen space.
    pub translates: HashMap<NodeId, (f32, f32)>,
    /// Accumulated authored transforms in immutable document coordinates.
    /// Entries exist only inside a non-identity transformed subtree, keeping
    /// ordinary documents on the allocation-free identity path.
    pub transforms: HashMap<NodeId, crate::Affine2>,
    /// Per-word geometry for text nodes: a text DOM node lays out as one
    /// taffy leaf per word (see `build_text_words`), each wrapping
    /// independently within its container, so its rendered content is a list
    /// of (box, word text) pairs rather than a single box, unlike every other
    /// node kind. Keyed by the text node's `NodeId`, in layout order.
    pub text_runs: HashMap<NodeId, Vec<(Rect, String)>>,
    /// Inline formatting contexts that were shaped by cosmic-text (see
    /// `inline`): a single leaf per container, its glyphs held in
    /// `text_engine`. `ifc_items` maps the container's `NodeId` to its item
    /// index. Paint rasterizes these instead of the per-word `text_runs`.
    #[cfg(feature = "paint")]
    pub text_engine: crate::inline::TextEngine,
    #[cfg(feature = "paint")]
    pub ifc_items: HashMap<NodeId, usize>,
    /// Anonymous inline-run contexts: a mixed block's consecutive inline
    /// children folded to one shaped leaf each (see `build_mixed_block`).
    /// Keyed by the parent block's `NodeId`, in document order; the leaves
    /// have no DOM node of their own.
    #[cfg(feature = "paint")]
    pub run_ifc_items: HashMap<NodeId, Vec<usize>>,
    /// Webfont-shaped items backing the general per-word fallback. Unlike
    /// `run_ifc_items`, these remain keyed by their real DOM text node so they
    /// can paint in exact tree order at each independently wrapped Taffy box.
    #[cfg(feature = "paint")]
    pub word_ifc_items: HashMap<NodeId, Vec<usize>>,
    /// Anonymous in-flow boxes generated by `::before`/`::after`. These have
    /// no DOM node of their own, but unlike the legacy text-only fast path
    /// they participate in layout with their pseudo style's real box model.
    pub(crate) generated_boxes: Vec<GeneratedBox>,
}

/// One connected element attribute mutation eligible for conservative
/// retained-style invalidation. The planner distinguishes ordinary selector
/// attributes from mapped presentation hints and attributes which can change
/// global stylesheets or loaded resources.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeStyleMutation {
    pub node: NodeId,
    pub name: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

/// How an attribute mutation participates in retained style.
///
/// This classification is public so DOM bindings can decide whether to keep a
/// prepared render without maintaining a second, inevitably divergent HTML
/// attribute allowlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetainedAttributeMutationKind {
    /// Only selector matching or declaration `attr()` can observe the value.
    Selector,
    /// The renderer maps the attribute into computed style or a native box.
    Subtree,
    /// The attribute can replace global CSS/resources or has document-wide
    /// semantics which cannot be represented by a bounded style dirty set.
    Full,
}

/// Classify an element attribute for conservative retained-style planning.
///
/// Unknown attributes are deliberately selector-only. Custom elements and
/// frameworks routinely toggle attributes such as `autocomplete`, `part`,
/// and `itemprop`; forcing a document cascade for an unreferenced attribute is
/// both unnecessary and particularly costly during hydration. Known HTML
/// resource/global attributes remain an explicit full fallback.
pub fn retained_attribute_mutation_kind(
    tree: &DomTree,
    node: NodeId,
    name: &str,
) -> RetainedAttributeMutationKind {
    use RetainedAttributeMutationKind::{Full, Selector, Subtree};

    let name = name.to_ascii_lowercase();
    if name == "style" {
        return Subtree;
    }
    let Some(local) = tree.get_node(node).and_then(|node| {
        node.as_element()
            .map(|element| element.local.as_ref().to_ascii_lowercase())
    }) else {
        return Full;
    };

    // These nodes feed document-global stylesheet, URL, metadata, or script
    // state. A selector dirty set cannot describe the external side effect.
    if (local == "base" && matches!(name.as_str(), "href" | "target"))
        || (local == "meta"
            && matches!(name.as_str(), "charset" | "content" | "http-equiv" | "name"))
        || (local == "style" && matches!(name.as_str(), "media" | "type" | "title"))
        || (local == "link"
            && matches!(
                name.as_str(),
                "as" | "crossorigin"
                    | "disabled"
                    | "fetchpriority"
                    | "href"
                    | "hreflang"
                    | "imagesizes"
                    | "imagesrcset"
                    | "integrity"
                    | "media"
                    | "referrerpolicy"
                    | "rel"
                    | "sizes"
                    | "type"
            ))
        || (local == "script"
            && matches!(
                name.as_str(),
                "async"
                    | "crossorigin"
                    | "defer"
                    | "fetchpriority"
                    | "integrity"
                    | "nomodule"
                    | "referrerpolicy"
                    | "src"
                    | "type"
            ))
    {
        return Full;
    }

    // Resource selection changes require the embedding runtime to rebuild its
    // decoded-resource/intrinsic-size inputs, not merely recompute CSS.
    if (local == "img"
        && matches!(
            name.as_str(),
            "crossorigin"
                | "decoding"
                | "fetchpriority"
                | "referrerpolicy"
                | "sizes"
                | "src"
                | "srcset"
        ))
        || (local == "source"
            && matches!(
                name.as_str(),
                "height" | "media" | "sizes" | "src" | "srcset" | "type" | "width"
            ))
        || (matches!(local.as_str(), "audio" | "video" | "track")
            && matches!(
                name.as_str(),
                "crossorigin"
                    | "kind"
                    | "label"
                    | "media"
                    | "poster"
                    | "preload"
                    | "src"
                    | "srclang"
                    | "type"
            ))
        || (matches!(local.as_str(), "embed" | "iframe")
            && matches!(name.as_str(), "src" | "srcdoc" | "type"))
        || (local == "object" && matches!(name.as_str(), "data" | "type"))
        || (matches!(local.as_str(), "image" | "use")
            && matches!(name.as_str(), "href" | "xlink:href"))
    {
        return Full;
    }

    // These values enter the UA/presentational cascade or are folded into a
    // native control's final used style. Refreshing the complete subtree also
    // covers inheritance and generated content while retaining clean peers.
    if matches!(
        name.as_str(),
        "align"
            | "bgcolor"
            | "cellpadding"
            | "cellspacing"
            | "color"
            | "height"
            | "hidden"
            | "valign"
            | "viewbox"
            | "width"
    ) || (local == "input" && matches!(name.as_str(), "size" | "type" | "value"))
        || (local == "select" && name == "size")
        || (local == "textarea" && matches!(name.as_str(), "cols" | "rows" | "wrap"))
        || matches!(name.as_str(), "dir" | "lang" | "xml:lang")
    {
        return Subtree;
    }

    Selector
}

/// A connected tree change which can reuse computed styles from the previous
/// render.  Node ids are stable across detach/reparent operations; recording
/// both parents lets the post-mutation traversal invalidate the old and new
/// sibling scopes without retaining a second DOM snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeStyleMutation {
    Insert {
        node: NodeId,
        old_parent: Option<NodeId>,
        new_parent: NodeId,
    },
    Remove {
        node: NodeId,
        old_parent: NodeId,
    },
    Text {
        node: NodeId,
        parent: Option<NodeId>,
    },
}

/// Damage input for a retained-style rebuild. Attribute invalidation uses
/// selector-key dependencies; tree invalidation uses parent/sibling scopes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetainedStyleMutation {
    Attribute(AttributeStyleMutation),
    Tree(TreeStyleMutation),
    /// A document-timeline sample changed while the DOM and stylesheet stayed
    /// stable. Re-cascade this animation owner and its inheritance-dependent
    /// subtree while retaining unrelated branches.
    Animation { node: NodeId },
    /// A Web Animation sample changed. Obscura's WAAPI surface currently
    /// animates only transform and opacity; neither property inherits, so the
    /// target alone needs a fresh animation cascade. Transform descendants
    /// are moved later by visual-geometry propagation, not by recascading
    /// their declarations.
    WaapiAnimation { node: NodeId },
    /// Cached image or font bytes became available. Resource selection,
    /// intrinsic sizes, shaping, layout, and paint must be rebuilt, but the
    /// DOM, stylesheet, and computed declarations are unchanged, so no style
    /// node is dirty.
    Resource,
}

impl From<AttributeStyleMutation> for RetainedStyleMutation {
    fn from(mutation: AttributeStyleMutation) -> Self {
        Self::Attribute(mutation)
    }
}

impl From<TreeStyleMutation> for RetainedStyleMutation {
    fn from(mutation: TreeStyleMutation) -> Self {
        Self::Tree(mutation)
    }
}

/// Style maps moved out of a previous layout. Moving, rather than cloning,
/// keeps incremental restyle from retaining a second heap-heavy LayoutStyle
/// graph beside the final used styles.
pub(crate) struct RetainedStyleMaps {
    pub styles: HashMap<NodeId, crate::LayoutStyle>,
    pub custom_properties: HashMap<NodeId, std::rc::Rc<HashMap<String, String>>>,
}

/// Dense identifier for one scrolling area in a prepared render. Index zero
/// is always the root viewport; element scroll containers follow in DOM order.
/// The ids are rebuild-local and must never be persisted by an embedding
/// runtime (persist element offsets by `NodeId` instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrollId(pub(crate) u32);

impl ScrollId {
    pub(crate) const ROOT: Self = Self(0);

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ScrollContainer {
    pub node: Option<NodeId>,
    pub parent: Option<ScrollId>,
    pub client_size: (f32, f32),
    pub content_size: (f32, f32),
    pub max_offset: (f32, f32),
}

/// Immutable scrolling topology derived from one final layout. Hot paint and
/// geometry paths index the two node vectors directly by `NodeId`; they never
/// walk ancestors or hash a node to discover which scrolling area moves it.
#[derive(Debug, Clone)]
pub(crate) struct ScrollTree {
    pub containers: Vec<ScrollContainer>,
    /// Scrolling area established by this node, if any.
    pub node_container: Vec<Option<ScrollId>>,
    /// CSSOM scrolling-overflow size for every node with a layout box. This is
    /// populated even for `overflow:visible` and `overflow:clip`: those values
    /// expose real `scrollWidth`/`scrollHeight`, although only an actual scroll
    /// container receives a `ScrollId` and can change its offset.
    pub node_content_size: Vec<Option<(f32, f32)>>,
    /// Area whose content movement applies to this node's own box. A scroll
    /// container's border/background therefore retain the parent's owner,
    /// while its descendants use the container itself.
    pub movement_owner: Vec<Option<ScrollId>>,
}

/// Prepared geometry shared by CSSOM, scrolling, sticky positioning, and
/// paint. Building these values together avoids independently rediscovering
/// viewport-fixed ownership and root overflow several times per frame.
pub(crate) struct DerivedLayoutState {
    pub content_size: (f32, f32),
    pub viewport_fixed: HashSet<NodeId>,
    pub sticky: StickyLayout,
    pub scroll_tree: ScrollTree,
}

pub(crate) struct DerivedGeometryState {
    pub content_size: (f32, f32),
    pub sticky: StickyLayout,
    pub scroll_tree: ScrollTree,
}

/// Root-scroll sticky-position constraints captured from normal-flow layout.
///
/// The normal boxes stay immutable in the layout cache. A scroll offset is
/// resolved into one accumulated translation per affected node, which keeps
/// JS geometry and screenshot paint on the same path.
#[derive(Debug, Clone, Default)]
pub struct StickyLayout {
    frames: Vec<StickyFrame>,
    owners: HashMap<NodeId, NodeId>,
    clip_owners: HashMap<NodeId, NodeId>,
    // Only populated by the public compatibility constructor. Production
    // prepared renders own the topology separately and avoid duplicating its
    // dense node vectors.
    compatibility_scroll_tree: Option<ScrollTree>,
}

#[derive(Debug, Clone)]
struct StickyFrame {
    id: NodeId,
    parent_sticky: Option<NodeId>,
    scroll_owner: ScrollId,
    scrollport_node: Option<NodeId>,
    scrollport: Rect,
    normal: Rect,
    containing: Rect,
    containing_is_scrollport: bool,
    margin: crate::Edges,
    inset: [Option<crate::Dimension>; 4],
    inset_expressions: [Option<String>; 4],
    font_size: f32,
    root_font_size: f32,
    rtl_inline: bool,
}

impl StickyLayout {
    fn frame_offsets(
        &self,
        viewport: (f32, f32),
        scroll: (f32, f32),
    ) -> HashMap<NodeId, (f32, f32)> {
        if let Some(scroll_tree) = &self.compatibility_scroll_tree {
            let cumulative = root_only_cumulative_scroll(scroll_tree, scroll);
            return self.resolved_frame_offsets(viewport, scroll_tree, &cumulative);
        }
        let mut frame_offsets = HashMap::with_capacity(self.frames.len());
        for frame in &self.frames {
            let inherited = frame
                .parent_sticky
                .and_then(|id| frame_offsets.get(&id).copied())
                .unwrap_or((0.0, 0.0));
            if frame.scroll_owner != ScrollId::ROOT {
                // Tuple-only callers carry no element scroll state. A nested
                // sticky frame therefore has no local movement, but it still
                // inherits an outer root-sticky frame when one exists.
                frame_offsets.insert(frame.id, inherited);
                continue;
            }
            let normal = Rect {
                x: frame.normal.x + inherited.0,
                y: frame.normal.y + inherited.1,
                ..frame.normal
            };
            let containing = Rect {
                x: frame.containing.x + inherited.0,
                y: frame.containing.y + inherited.1,
                ..frame.containing
            };
            let x = sticky_axis_position(
                normal.x,
                normal.width,
                containing.x + frame.margin.left,
                containing.x + containing.width - frame.margin.right - normal.width,
                scroll.0,
                viewport.0,
                resolve_frame_sticky_inset(frame, 3, viewport.0, viewport),
                resolve_frame_sticky_inset(frame, 1, viewport.0, viewport),
                frame.rtl_inline,
            );
            let y = sticky_axis_position(
                normal.y,
                normal.height,
                containing.y + frame.margin.top,
                containing.y + containing.height - frame.margin.bottom - normal.height,
                scroll.1,
                viewport.1,
                resolve_frame_sticky_inset(frame, 0, viewport.1, viewport),
                resolve_frame_sticky_inset(frame, 2, viewport.1, viewport),
                false,
            );
            frame_offsets.insert(
                frame.id,
                (inherited.0 + x - normal.x, inherited.1 + y - normal.y),
            );
        }
        frame_offsets
    }

    pub(crate) fn resolved_translations(
        &self,
        viewport: (f32, f32),
        scroll_tree: &ScrollTree,
        cumulative_scroll: &[(f32, f32)],
    ) -> HashMap<NodeId, (f32, f32)> {
        let frame_offsets = self.resolved_frame_offsets(viewport, scroll_tree, cumulative_scroll);
        self.owners
            .iter()
            .filter_map(|(&id, owner)| frame_offsets.get(owner).copied().map(|offset| (id, offset)))
            .collect()
    }

    pub(crate) fn resolved_root_translations(
        &self,
        viewport: (f32, f32),
        scroll_tree: &ScrollTree,
        scroll: (f32, f32),
    ) -> HashMap<NodeId, (f32, f32)> {
        let cumulative = root_only_cumulative_scroll(scroll_tree, scroll);
        self.resolved_translations(viewport, scroll_tree, &cumulative)
    }

    fn resolved_frame_offsets(
        &self,
        viewport: (f32, f32),
        scroll_tree: &ScrollTree,
        cumulative_scroll: &[(f32, f32)],
    ) -> HashMap<NodeId, (f32, f32)> {
        let mut frame_offsets = HashMap::with_capacity(self.frames.len());
        for frame in &self.frames {
            let inherited = frame
                .parent_sticky
                .and_then(|id| frame_offsets.get(&id).copied())
                .unwrap_or((0.0, 0.0));
            let container = scroll_tree.containers[frame.scroll_owner.index()];
            let content_move = cumulative_scroll
                .get(frame.scroll_owner.index())
                .copied()
                .unwrap_or((0.0, 0.0));
            let port_parent_move = container
                .parent
                .and_then(|parent| cumulative_scroll.get(parent.index()).copied())
                .unwrap_or((0.0, 0.0));
            let port_sticky = frame
                .scrollport_node
                .and_then(|node| self.owners.get(&node))
                .and_then(|owner| frame_offsets.get(owner).copied())
                .unwrap_or((0.0, 0.0));
            let normal = Rect {
                x: frame.normal.x + content_move.0 + inherited.0,
                y: frame.normal.y + content_move.1 + inherited.1,
                ..frame.normal
            };
            let scrollport = if frame.scroll_owner == ScrollId::ROOT {
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: viewport.0,
                    height: viewport.1,
                }
            } else {
                Rect {
                    x: frame.scrollport.x + port_parent_move.0 + port_sticky.0,
                    y: frame.scrollport.y + port_parent_move.1 + port_sticky.1,
                    ..frame.scrollport
                }
            };
            let containing_move = if frame.containing_is_scrollport {
                (
                    port_parent_move.0 + port_sticky.0,
                    port_parent_move.1 + port_sticky.1,
                )
            } else {
                (
                    content_move.0 + inherited.0,
                    content_move.1 + inherited.1,
                )
            };
            let containing = Rect {
                x: frame.containing.x + containing_move.0,
                y: frame.containing.y + containing_move.1,
                ..frame.containing
            };
            let x = sticky_axis_position(
                normal.x,
                normal.width,
                containing.x + frame.margin.left,
                containing.x + containing.width - frame.margin.right - normal.width,
                scrollport.x,
                scrollport.width,
                resolve_frame_sticky_inset(frame, 3, scrollport.width, viewport),
                resolve_frame_sticky_inset(frame, 1, scrollport.width, viewport),
                frame.rtl_inline,
            );
            let y = sticky_axis_position(
                normal.y,
                normal.height,
                containing.y + frame.margin.top,
                containing.y + containing.height - frame.margin.bottom - normal.height,
                scrollport.y,
                scrollport.height,
                resolve_frame_sticky_inset(frame, 0, scrollport.height, viewport),
                resolve_frame_sticky_inset(frame, 2, scrollport.height, viewport),
                false,
            );
            frame_offsets.insert(
                frame.id,
                (
                    inherited.0 + x - normal.x,
                    inherited.1 + y - normal.y,
                ),
            );
        }
        frame_offsets
    }

    pub(crate) fn resolved_translation_for(
        &self,
        id: NodeId,
        viewport: (f32, f32),
        scroll_tree: &ScrollTree,
        cumulative_scroll: &[(f32, f32)],
    ) -> (f32, f32) {
        let Some(owner) = self.owners.get(&id) else {
            return (0.0, 0.0);
        };
        self.resolved_frame_offsets(viewport, scroll_tree, cumulative_scroll)
            .get(owner)
            .copied()
            .unwrap_or((0.0, 0.0))
    }

    /// Resolve a single geometry query without materializing an entry for
    /// every descendant in every sticky subtree. This is O(sticky frames), not
    /// O(DOM), and is the hot path for repeated getBoundingClientRect reads.
    pub fn translation_for(
        &self,
        id: NodeId,
        viewport: (f32, f32),
        scroll: (f32, f32),
    ) -> (f32, f32) {
        let Some(owner) = self.owners.get(&id) else {
            return (0.0, 0.0);
        };
        self.frame_offsets(viewport, scroll)
            .get(owner)
            .copied()
            .unwrap_or((0.0, 0.0))
    }

    /// Accumulated sticky translation for every node in a sticky subtree.
    /// Frames are stored in DOM preorder, so an outer sticky frame's resolved
    /// movement is available before a nested sticky frame is constrained.
    /// Paint calls this once for the whole document; geometry uses
    /// [`StickyLayout::translation_for`] to avoid an O(DOM) map per query.
    pub fn translations(
        &self,
        viewport: (f32, f32),
        scroll: (f32, f32),
    ) -> HashMap<NodeId, (f32, f32)> {
        let frame_offsets = self.frame_offsets(viewport, scroll);
        self.owners
            .iter()
            .filter_map(|(&id, owner)| frame_offsets.get(owner).copied().map(|offset| (id, offset)))
            .collect()
    }

    /// Sticky-space movement of the ancestor that owns a node's inherited
    /// overflow clip. A sticky descendant must move inside an outer clip,
    /// while a clip established inside the sticky subtree moves with it.
    pub fn clip_translations_from(
        &self,
        translations: &HashMap<NodeId, (f32, f32)>,
    ) -> HashMap<NodeId, (f32, f32)> {
        self.clip_owners
            .iter()
            .filter_map(|(&id, owner)| translations.get(owner).copied().map(|offset| (id, offset)))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

fn root_only_cumulative_scroll(scroll_tree: &ScrollTree, scroll: (f32, f32)) -> Vec<(f32, f32)> {
    let mut cumulative = vec![(0.0, 0.0); scroll_tree.containers.len()];
    if let Some(root) = cumulative.first_mut() {
        *root = (-scroll.0, -scroll.1);
    }
    for index in 1..cumulative.len() {
        cumulative[index] = scroll_tree.containers[index]
            .parent
            .map(|parent| cumulative[parent.index()])
            .unwrap_or((0.0, 0.0));
    }
    cumulative
}

fn resolve_sticky_inset(value: Option<crate::Dimension>, basis: f32) -> Option<f32> {
    match value {
        Some(crate::Dimension::Px(px)) => Some(px),
        Some(crate::Dimension::Percent(percent)) => Some(percent * basis),
        _ => None,
    }
}

fn resolve_frame_sticky_inset(
    frame: &StickyFrame,
    index: usize,
    percent_basis: f32,
    viewport: (f32, f32),
) -> Option<f32> {
    if let Some(expression) = frame.inset_expressions[index].as_deref() {
        return crate::style::resolve_contextual_length(
            expression,
            frame.font_size,
            frame.root_font_size,
            viewport.0 / 100.0,
            viewport.1 / 100.0,
            percent_basis,
        );
    }
    resolve_sticky_inset(frame.inset[index], percent_basis)
}

fn sticky_axis_position(
    normal: f32,
    size: f32,
    contain_min: f32,
    contain_max: f32,
    scroll: f32,
    viewport: f32,
    start: Option<f32>,
    end: Option<f32>,
    end_is_inline_start: bool,
) -> f32 {
    let mut stick_start = start.map(|inset| scroll + inset);
    let mut stick_end = end.map(|inset| scroll + viewport - inset - size);

    // When both insets leave a sticky view rectangle smaller than the box,
    // the physical end inset is reduced in LTR, while the physical start
    // inset is reduced in RTL so the logical inline-start edge wins.
    if let (Some(start), Some(end)) = (stick_start, stick_end) {
        if end < start {
            if end_is_inline_start {
                stick_start = Some(end);
            } else {
                stick_end = Some(start);
            }
        }
    }

    let mut position = normal;
    if let Some(start) = stick_start.take() {
        position = position.max(start.min(contain_max));
    }
    if let Some(end) = stick_end {
        position = position.min(end.max(contain_min));
    }
    position
}

impl DomLayout {
    pub(crate) fn derived_layout_state(
        &self,
        tree: &DomTree,
        viewport: (f32, f32),
    ) -> DerivedLayoutState {
        let viewport_fixed = self.viewport_fixed_nodes(tree);
        let geometry = self.derived_geometry_with_fixed(tree, viewport, &viewport_fixed);
        DerivedLayoutState {
            content_size: geometry.content_size,
            viewport_fixed,
            sticky: geometry.sticky,
            scroll_tree: geometry.scroll_tree,
        }
    }

    pub(crate) fn derived_geometry_with_fixed(
        &self,
        tree: &DomTree,
        viewport: (f32, f32),
        viewport_fixed: &HashSet<NodeId>,
    ) -> DerivedGeometryState {
        let content_size = self.scrolling_content_size_with_fixed(tree, viewport, viewport_fixed);
        let scroll_tree = self.scroll_tree(tree, viewport, content_size, viewport_fixed);
        let sticky =
            self.sticky_layout_with_geometry(tree, viewport, content_size, &scroll_tree);
        DerivedGeometryState {
            content_size,
            sticky,
            scroll_tree,
        }
    }

    /// Refresh the derived paint-culling bit after an opacity-only sample.
    /// Keep opacity as a binary zero ancestor flag instead of multiplying the
    /// chain, which avoids underflow making a deep fractional-opacity subtree
    /// incorrectly disappear.
    pub(crate) fn refresh_effective_visibility(&mut self, tree: &DomTree) {
        let mut inherited: HashMap<NodeId, (bool, bool)> = HashMap::new();
        for id in rendered_descendants(tree, tree.document()) {
            let parent_state = rendered_parent(tree, id)
                .and_then(|parent| inherited.get(&parent).copied())
                .unwrap_or((false, false));
            let Some(style) = self.styles.get_mut(&id) else {
                inherited.insert(id, parent_state);
                continue;
            };
            let visibility_hidden = style.visibility_hidden.unwrap_or(parent_state.0);
            let opacity_zero = parent_state.1 || style.opacity.is_some_and(|value| value <= 0.0);
            style.effectively_invisible = visibility_hidden || opacity_zero;
            if let Some(pseudo) = style.before_pseudo.as_deref_mut() {
                pseudo.effectively_invisible = style.effectively_invisible;
            }
            if let Some(pseudo) = style.after_pseudo.as_deref_mut() {
                pseudo.effectively_invisible = style.effectively_invisible;
            }
            inherited.insert(id, (visibility_hidden, opacity_zero));
        }
    }

    /// Rebuild visual geometry after a transform-only animation sample while
    /// retaining normal-flow boxes and shaped text. This is deliberately a
    /// complete top-down visual walk: ancestor transforms affect descendant
    /// matrices, overflow clips, scrolling overflow, and text raster clips.
    pub(crate) fn refresh_visual_geometry(&mut self, tree: &DomTree, viewport: (f32, f32)) {
        self.clip_rects.clear();
        self.translates.clear();
        self.transforms.clear();

        let root = rendered_descendants(tree, tree.document())
            .into_iter()
            .find(|id| tree.get_node(*id).is_some_and(|node| node.is_element()));
        if let Some(root_id) = root {
            let root_font_size = self
                .styles
                .get(&root_id)
                .and_then(|style| style.font_size)
                .unwrap_or(16.0);
            resolve_clip_rects(
                tree,
                root_id,
                None,
                0.0,
                0.0,
                &self.rects,
                &self.styles,
                &mut self.clip_rects,
                &mut self.translates,
                crate::Affine2::IDENTITY,
                &mut self.transforms,
                root_font_size,
                viewport,
            );
        }

        #[cfg(feature = "paint")]
        {
            for (&node, &item) in &self.ifc_items {
                self.text_engine.set_clip(
                    item,
                    shaped_item_clip(
                        node,
                        &self.rects,
                        &self.styles,
                        &self.clip_rects,
                        &self.translates,
                        viewport,
                    ),
                );
            }
            for (&parent, items) in &self.run_ifc_items {
                let clip = shaped_item_clip(
                    parent,
                    &self.rects,
                    &self.styles,
                    &self.clip_rects,
                    &self.translates,
                    viewport,
                );
                for &item in items {
                    self.text_engine.set_clip(item, clip);
                }
            }
            for (&text_node, items) in &self.word_ifc_items {
                let clip = shaped_item_clip(
                    text_node,
                    &self.rects,
                    &self.styles,
                    &self.clip_rects,
                    &self.translates,
                    viewport,
                );
                for &item in items {
                    self.text_engine.set_clip(item, clip);
                }
            }
        }
    }

    /// Nodes whose painted coordinate space is anchored to the initial
    /// containing block. A viewport-fixed element and its whole subtree stay
    /// stationary when the document scrolls. A fixed element captured by a
    /// transformed/filter/contain ancestor remains in the document scrolling
    /// coordinate space instead.
    pub fn viewport_fixed_nodes(&self, tree: &DomTree) -> HashSet<NodeId> {
        let mut fixed = HashSet::new();
        let mut has_fixed_cb: HashMap<NodeId, bool> = HashMap::new();

        for id in rendered_descendants(tree, tree.document()) {
            let parent = rendered_parent(tree, id);
            let parent_is_fixed = parent.is_some_and(|parent| fixed.contains(&parent));
            let ancestor_has_fixed_cb = parent
                .and_then(|parent| has_fixed_cb.get(&parent).copied())
                .unwrap_or(false);
            let style = self.styles.get(&id);
            let starts_viewport_fixed =
                style.is_some_and(|style| style.position_fixed && !ancestor_has_fixed_cb);

            if parent_is_fixed || starts_viewport_fixed {
                fixed.insert(id);
            }

            let establishes_fixed_cb =
                style.is_some_and(|style| style.containing_block_triggers != 0);
            has_fixed_cb.insert(id, ancestor_has_fixed_cb || establishes_fixed_cb);
        }

        fixed
    }

    /// Build the dense element-scroll topology and local scrolling overflow.
    ///
    /// This is deliberately one pair of DOM-order passes. The first assigns
    /// each box to its nearest scrolling-content owner; the second unions each
    /// box into exactly that owner. A nested scroller's own border box thus
    /// contributes to the outer area, while its hidden inner overflow only
    /// contributes to the nested area.
    pub(crate) fn scroll_tree(
        &self,
        tree: &DomTree,
        viewport: (f32, f32),
        root_content_size: (f32, f32),
        viewport_fixed: &HashSet<NodeId>,
    ) -> ScrollTree {
        let nodes = rendered_descendants(tree, tree.document());
        let node_capacity = nodes
            .iter()
            .map(|id| id.index())
            .max()
            .map_or(0, |max| max + 1);
        let mut node_container = vec![None; node_capacity];
        let mut movement_owner = vec![None; node_capacity];
        let mut node_content_size = vec![None; node_capacity];
        let mut containers = vec![ScrollContainer {
            node: None,
            parent: None,
            client_size: viewport,
            content_size: root_content_size,
            max_offset: (
                crate::quantized_scroll_range(root_content_size.0, viewport.0, 1.0),
                crate::quantized_scroll_range(root_content_size.1, viewport.1, 1.0),
            ),
        }];

        // `transforms` is accumulated top-down, so a non-translation matrix on
        // the candidate identifies either its own transform or an affine
        // ancestor. Stage 1 cannot insert scroll movement in that coordinate
        // space without making geometry and pixels disagree. A transformed
        // *descendant* is safe: its visual overflow is measured below and the
        // scrolling translation applies to the already transformed subtree.
        let mut affine_coordinate_space = vec![false; node_capacity];
        for &id in &nodes {
            if let Some(slot) = affine_coordinate_space.get_mut(id.index()) {
                *slot = self
                    .transforms
                    .get(&id)
                    .is_some_and(|transform| !transform.is_translation());
            }
        }

        let root = nodes
            .iter()
            .copied()
            .find(|id| tree.get_node(*id).is_some_and(|node| node.is_element()));
        fn assign(
            laid: &DomLayout,
            tree: &DomTree,
            id: NodeId,
            inherited_owner: Option<ScrollId>,
            viewport_fixed: &HashSet<NodeId>,
            affine_coordinate_space: &[bool],
            containers: &mut Vec<ScrollContainer>,
            node_container: &mut [Option<ScrollId>],
            movement_owner: &mut [Option<ScrollId>],
        ) {
            let fixed = viewport_fixed.contains(&id);
            let parent_fixed = rendered_parent(tree, id)
                .is_some_and(|parent| viewport_fixed.contains(&parent));
            // Only the boundary that enters the viewport-fixed coordinate
            // space drops root movement. Descendants inherit that zero-based
            // space and may establish ordinary nested scrolling areas.
            let starts_viewport_fixed = fixed && !parent_fixed;
            let owner = if starts_viewport_fixed {
                None
            } else {
                inherited_owner
            };
            if let Some(slot) = movement_owner.get_mut(id.index()) {
                *slot = owner;
            }

            let style = laid.styles.get(&id);
            let rect = laid.rects.get(&id).copied();
            let establishes = style.is_some_and(|style| {
                style.overflow_scroll_container
                    && !style.overflow_propagated_to_viewport
                    && !style.display_contents
            }) && rect.is_some()
                && !affine_coordinate_space
                    .get(id.index())
                    .copied()
                    .unwrap_or(false);
            let child_owner = if establishes {
                let style = style.expect("scroll container style");
                let rect = rect.expect("scroll container rect");
                let client = (
                    (rect.width - style.border.left - style.border.right).max(0.0),
                    (rect.height - style.border.top - style.border.bottom).max(0.0),
                );
                let sid = ScrollId(containers.len() as u32);
                containers.push(ScrollContainer {
                    node: Some(id),
                    parent: owner,
                    client_size: client,
                    content_size: client,
                    max_offset: (0.0, 0.0),
                });
                if let Some(slot) = node_container.get_mut(id.index()) {
                    *slot = Some(sid);
                }
                Some(sid)
            } else {
                owner
            };

            for child in rendered_children(tree, id) {
                assign(
                    laid,
                    tree,
                    child,
                    child_owner,
                    viewport_fixed,
                    affine_coordinate_space,
                    containers,
                    node_container,
                    movement_owner,
                );
            }
        }
        if let Some(root) = root {
            assign(
                self,
                tree,
                root,
                Some(ScrollId::ROOT),
                viewport_fixed,
                &affine_coordinate_space,
                &mut containers,
                &mut node_container,
                &mut movement_owner,
            );
        }

        // Compute scrolling overflow for every boxed node in one reverse pass.
        // A clipping/scrolling child contributes only its own border box to an
        // ancestor, while still exposing its complete local overflow through
        // CSSOM. Visible descendants propagate their union upward.
        let mut overflow_bounds = vec![None; node_capacity];
        for &id in nodes.iter().rev() {
            let Some(rect) = self.rects.get(&id).copied() else {
                continue;
            };
            let style = self.styles.get(&id);
            let visual = |rect: Rect| {
                self.transforms
                    .get(&id)
                    .copied()
                    .map(|transform| transform.map_rect(rect))
                    .unwrap_or(rect)
            };
            let padding_rect = style.map_or(rect, |style| Rect {
                x: rect.x + style.border.left,
                y: rect.y + style.border.top,
                width: (rect.width - style.border.left - style.border.right).max(0.0),
                height: (rect.height - style.border.top - style.border.bottom).max(0.0),
            });
            let visual_padding = visual(padding_rect);
            let mut local = visual_padding;
            for child in rendered_children(tree, id) {
                let Some(child_overflow) = overflow_bounds.get(child.index()).copied().flatten()
                else {
                    continue;
                };
                let contribution =
                    if let Some(style) = self.styles.get(&child).filter(|style| {
                        style.overflow_hidden && !style.overflow_propagated_to_viewport
                    }) {
                        let border_box = self
                            .rects
                            .get(&child)
                            .copied()
                            .map(|rect| {
                                self.transforms
                                    .get(&child)
                                    .copied()
                                    .map(|transform| transform.map_rect(rect))
                                    .unwrap_or(rect)
                            })
                            .unwrap_or(child_overflow);
                        let x0 = if style.clips_overflow_x() {
                            border_box.x
                        } else {
                            child_overflow.x
                        };
                        let x1 = if style.clips_overflow_x() {
                            border_box.x + border_box.width
                        } else {
                            child_overflow.x + child_overflow.width
                        };
                        let y0 = if style.clips_overflow_y() {
                            border_box.y
                        } else {
                            child_overflow.y
                        };
                        let y1 = if style.clips_overflow_y() {
                            border_box.y + border_box.height
                        } else {
                            child_overflow.y + child_overflow.height
                        };
                        Rect {
                            x: x0,
                            y: y0,
                            width: (x1 - x0).max(0.0),
                            height: (y1 - y0).max(0.0),
                        }
                    } else {
                        child_overflow
                    };
                local = local.union(&contribution);
            }
            let mut content = (
                (local.x + local.width - visual_padding.x).max(visual_padding.width),
                (local.y + local.height - visual_padding.y).max(visual_padding.height),
            );
            // Scroll containers include trailing end padding in their
            // scrolling area. Visible/clip boxes expose descendant overflow
            // but, matching Chromium, do not append that trailing padding.
            if let Some(style) = style.filter(|style| style.overflow_scroll_container) {
                if content.0 > visual_padding.width {
                    content.0 += style.padding.right;
                }
                if content.1 > visual_padding.height {
                    content.1 += style.padding.bottom;
                }
            }
            if let Some(slot) = node_content_size.get_mut(id.index()) {
                *slot = Some(
                    if style.is_some_and(|style| style.ignores_used_box_sizes()) {
                        (0.0, 0.0)
                    } else {
                        content
                    },
                );
            }
            if let Some(slot) = overflow_bounds.get_mut(id.index()) {
                *slot = Some(local);
            }
        }
        for container in containers.iter_mut().skip(1) {
            container.content_size = container
                .node
                .and_then(|node| node_content_size.get(node.index()).copied().flatten())
                .unwrap_or(container.client_size);
            container.max_offset = (
                crate::quantized_scroll_range(
                    container.content_size.0,
                    container.client_size.0,
                    1.0,
                ),
                crate::quantized_scroll_range(
                    container.content_size.1,
                    container.client_size.1,
                    1.0,
                ),
            );
        }

        ScrollTree {
            containers,
            node_container,
            node_content_size,
            movement_owner,
        }
    }

    /// Root scrolling overflow in CSS pixels. This is the union of laid-out
    /// document boxes after translate transforms, bounded by inherited
    /// overflow clips. Viewport-fixed subtrees do not enlarge the document.
    pub fn scrolling_content_size(&self, tree: &DomTree, viewport: (f32, f32)) -> (f32, f32) {
        let fixed = self.viewport_fixed_nodes(tree);
        self.scrolling_content_size_with_fixed(tree, viewport, &fixed)
    }

    fn scrolling_content_size_with_fixed(
        &self,
        tree: &DomTree,
        viewport: (f32, f32),
        fixed: &HashSet<NodeId>,
    ) -> (f32, f32) {
        let mut right = viewport.0.max(0.0);
        let mut bottom = viewport.1.max(0.0);

        if let Some(root) = tree
            .descendants(tree.document())
            .into_iter()
            .find(|id| tree.get_node(*id).is_some_and(|node| node.is_element()))
        {
            accumulate_scrolling_overflow(
                tree,
                root,
                None,
                fixed,
                &self.rects,
                &self.styles,
                &self.translates,
                &self.transforms,
                &mut right,
                &mut bottom,
            );
        }

        (right.ceil(), bottom.ceil())
    }

    /// Capture sticky constraints from the normal-flow result. The historical
    /// method name remains for API compatibility; returned frames include both
    /// the root viewport and element scrolling containers.
    pub fn root_sticky_layout(&self, tree: &DomTree, viewport: (f32, f32)) -> StickyLayout {
        let viewport_fixed = self.viewport_fixed_nodes(tree);
        let content = self.scrolling_content_size_with_fixed(tree, viewport, &viewport_fixed);
        let scroll_tree = self.scroll_tree(tree, viewport, content, &viewport_fixed);
        let mut sticky = self.sticky_layout_with_geometry(tree, viewport, content, &scroll_tree);
        sticky.compatibility_scroll_tree = Some(scroll_tree);
        sticky
    }

    fn sticky_layout_with_geometry(
        &self,
        tree: &DomTree,
        viewport: (f32, f32),
        content: (f32, f32),
        scroll_tree: &ScrollTree,
    ) -> StickyLayout {
        let root_containing = Rect {
            x: 0.0,
            y: 0.0,
            width: content.0,
            height: content.1,
        };
        let mut layout = StickyLayout::default();
        let mut nearest_sticky: HashMap<NodeId, Option<NodeId>> = HashMap::new();
        let mut inherited_clip_sticky: HashMap<NodeId, Option<NodeId>> = HashMap::new();
        // Propagate the nearest authored scroll container once in rendered
        // preorder. The boolean distinguishes a supported ScrollTree owner
        // from a real but affine/otherwise unsupported scrolling coordinate
        // space, which must not accidentally promote sticky to the viewport.
        let mut nearest_scrollport = vec![None; scroll_tree.movement_owner.len()];
        let root_font_size = tree
            .query_selector("html")
            .ok()
            .flatten()
            .and_then(|root| self.styles.get(&root).and_then(|style| style.font_size))
            .unwrap_or(16.0);

        for id in rendered_descendants(tree, tree.document()) {
            let parent = rendered_parent(tree, id);
            let inherited_scrollport = parent
                .and_then(|parent| nearest_scrollport.get(parent.index()).copied())
                .flatten();
            let style = self.styles.get(&id);
            let establishes_authored_scrollport = style.is_some_and(|style| {
                style.overflow_scroll_container
                    && !is_viewport_overflow_source(id, &self.styles)
                    && !style.display_contents
            }) && self.rects.contains_key(&id);
            let descendants_scrollport = if establishes_authored_scrollport {
                Some((
                    id,
                    scroll_tree
                        .node_container
                        .get(id.index())
                        .copied()
                        .flatten()
                        .is_some(),
                ))
            } else {
                inherited_scrollport
            };
            if let Some(slot) = nearest_scrollport.get_mut(id.index()) {
                *slot = descendants_scrollport;
            }
            let parent_sticky = parent
                .and_then(|parent| nearest_sticky.get(&parent).copied())
                .flatten();
            let inherited_clip = parent.and_then(|parent| {
                let parent_style = self.styles.get(&parent);
                if parent_style.is_some_and(|style| style.overflow_hidden) {
                    nearest_sticky.get(&parent).copied().flatten()
                } else {
                    inherited_clip_sticky.get(&parent).copied().flatten()
                }
            });
            if let Some(owner) = inherited_clip {
                layout.clip_owners.insert(id, owner);
            }
            inherited_clip_sticky.insert(id, inherited_clip);

            let scroll_owner = scroll_tree
                .movement_owner
                .get(id.index())
                .copied()
                .flatten();
            let Some(scroll_owner) = scroll_owner else {
                nearest_sticky.insert(id, None);
                continue;
            };
            let container = scroll_tree.containers[scroll_owner.index()];
            let scrollport_node = container.node;
            // ScrollTree conservatively declines affine scrolling coordinate
            // spaces. Preserve that fallback for sticky too instead of
            // accidentally attaching such a frame to the root viewport.
            let supported_scroll_owner = match inherited_scrollport {
                Some((authored, true)) => scrollport_node == Some(authored),
                Some((_authored, false)) => false,
                None => scrollport_node.is_none(),
            };
            // General affine sticky constraints require solving in the
            // transformed scrollport coordinate space. Until that solver is
            // present, decline sticky below an affine ancestor instead of
            // returning geometry that disagrees with paint.
            let supported_coordinate_space = parent
                .and_then(|parent| self.transforms.get(&parent))
                .is_none_or(|transform| transform.is_translation());

            let is_sticky = style.is_some_and(|style| {
                style.position_sticky
                    && style.inset.iter().any(Option::is_some)
                    && supported_scroll_owner
                    && supported_coordinate_space
            });
            if !is_sticky {
                nearest_sticky.insert(id, parent_sticky);
                if let Some(owner) = parent_sticky {
                    layout.owners.insert(id, owner);
                }
                continue;
            }
            let (Some(style), Some(rect)) = (style, self.rects.get(&id).copied()) else {
                nearest_sticky.insert(id, parent_sticky);
                continue;
            };
            let (tx, ty) = self.translates.get(&id).copied().unwrap_or((0.0, 0.0));
            // Sticky constraints are solved from the box's normal-flow
            // position, before its own transform is painted. `translates`
            // carries the accumulated transform chain, so remove exactly this
            // frame's resolved translate while retaining every ancestor's.
            let (own_tx, own_ty) = resolved_own_translate(style, &rect, 16.0, viewport);
            let normal = Rect {
                x: rect.x + tx - own_tx,
                y: rect.y + ty - own_ty,
                ..rect
            };

            let mut containing = None;
            let mut containing_node = None;
            let mut ancestor = parent;
            while let Some(candidate) = ancestor {
                if let (Some(candidate_style), Some(candidate_rect)) = (
                    self.styles.get(&candidate),
                    self.rects.get(&candidate).copied(),
                ) {
                    if candidate_style.display != crate::Display::Inline
                        && !candidate_style.display_contents
                    {
                        let (ctx, cty) = self
                            .translates
                            .get(&candidate)
                            .copied()
                            .unwrap_or((0.0, 0.0));
                        containing = Some(Rect {
                            x: candidate_rect.x
                                + ctx
                                + candidate_style.border.left
                                + candidate_style.padding.left,
                            y: candidate_rect.y
                                + cty
                                + candidate_style.border.top
                                + candidate_style.padding.top,
                            width: (candidate_rect.width
                                - candidate_style.border.left
                                - candidate_style.border.right
                                - candidate_style.padding.left
                                - candidate_style.padding.right)
                                .max(0.0),
                            height: (candidate_rect.height
                                - candidate_style.border.top
                                - candidate_style.border.bottom
                                - candidate_style.padding.top
                                - candidate_style.padding.bottom)
                                .max(0.0),
                        });
                        containing_node = Some(candidate);
                        break;
                    }
                }
                ancestor = rendered_parent(tree, candidate);
            }

            let scrollport = scrollport_node
                .and_then(|node| {
                    let style = self.styles.get(&node)?;
                    let rect = self.rects.get(&node).copied()?;
                    let (tx, ty) = self.translates.get(&node).copied().unwrap_or((0.0, 0.0));
                    Some(Rect {
                        x: rect.x + tx + style.border.left + style.padding.left,
                        y: rect.y + ty + style.border.top + style.padding.top,
                        width: (rect.width
                            - style.border.left
                            - style.border.right
                            - style.padding.left
                            - style.padding.right)
                            .max(0.0),
                        height: (rect.height
                            - style.border.top
                            - style.border.bottom
                            - style.padding.top
                            - style.padding.bottom)
                            .max(0.0),
                    })
                })
                .unwrap_or(Rect {
                    x: 0.0,
                    y: 0.0,
                    width: viewport.0,
                    height: viewport.1,
                });
            layout.frames.push(StickyFrame {
                id,
                parent_sticky,
                scroll_owner,
                scrollport_node,
                scrollport,
                normal,
                containing: containing.unwrap_or(root_containing),
                containing_is_scrollport: containing_node == scrollport_node,
                margin: style.margin,
                inset: style.inset,
                inset_expressions: style.inset_expressions.clone(),
                font_size: style.font_size.unwrap_or(16.0),
                root_font_size,
                rtl_inline: style.direction == Some(taffy::Direction::Rtl),
            });
            layout.owners.insert(id, id);
            nearest_sticky.insert(id, Some(id));
        }

        layout
    }
}

#[allow(clippy::too_many_arguments)]
fn accumulate_scrolling_overflow(
    tree: &DomTree,
    id: NodeId,
    inherited_clip: Option<OverflowClip>,
    fixed: &HashSet<NodeId>,
    rects: &HashMap<NodeId, Rect>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    translates: &HashMap<NodeId, (f32, f32)>,
    transforms: &HashMap<NodeId, crate::Affine2>,
    right: &mut f32,
    bottom: &mut f32,
) {
    if fixed.contains(&id) {
        return;
    }

    let translated = rects.get(&id).map(|rect| {
        transforms
            .get(&id)
            .copied()
            .map(|transform| transform.map_rect(*rect))
            .unwrap_or(*rect)
    });
    if let Some(overflow) = translated {
        let visible = if let Some(ref clip) = inherited_clip {
            clip.intersect_rect(&overflow)
        } else {
            Some(overflow)
        };
        if let Some(overflow) = visible {
            *right = right.max(overflow.x + overflow.width);
            *bottom = bottom.max(overflow.y + overflow.height);
        }
    }

    // Overflow propagated from html/body establishes the viewport clip for
    // painting, but it does not truncate the root scrolling area's CSSOM
    // overflow dimensions. Ordinary descendant overflow clips still bound
    // their subtree's contribution.
    let child_clip = match (styles.get(&id), rects.get(&id)) {
        (Some(style), Some(rect))
            if style.overflow_hidden && !is_viewport_overflow_source(id, styles) =>
        {
            let (tx, ty) = translates.get(&id).copied().unwrap_or((0.0, 0.0));
            let own = OverflowClip::for_box(rect, style, tx, ty);
            Some(match inherited_clip {
                Some(clip) => clip.intersect(own),
                None => own,
            })
        }
        _ => inherited_clip,
    };
    for child in rendered_children(tree, id) {
        accumulate_scrolling_overflow(
            tree,
            child,
            child_clip.clone(),
            fixed,
            rects,
            styles,
            translates,
            transforms,
            right,
            bottom,
        );
    }
}

fn is_viewport_overflow_source(id: NodeId, styles: &HashMap<NodeId, crate::LayoutStyle>) -> bool {
    styles
        .get(&id)
        .is_some_and(|style| style.overflow_propagated_to_viewport)
}

fn mark_viewport_overflow_source(
    tree: &DomTree,
    root: NodeId,
    styles: &mut HashMap<NodeId, crate::LayoutStyle>,
) {
    let root_is_html = tree.get_node(root).is_some_and(|node| {
        node.as_element()
            .is_some_and(|element| element.local.as_ref() == "html")
    });
    if !root_is_html {
        return;
    }
    let root_owns_overflow = styles.get(&root).is_some_and(|style| style.overflow_hidden);
    if let Some(style) = styles.get_mut(&root) {
        style.overflow_propagated_to_viewport = true;
    }
    if root_owns_overflow {
        return;
    }
    if let Some(body) = tree.children(root).into_iter().find(|child| {
        tree.get_node(*child).is_some_and(|node| {
            node.as_element()
                .is_some_and(|element| element.local.as_ref() == "body")
        })
    }) {
        if let Some(style) = styles.get_mut(&body) {
            style.overflow_propagated_to_viewport = true;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GeneratedBoxKind {
    Before,
    After,
}

#[derive(Clone, Copy)]
pub(crate) struct GeneratedBox {
    pub host: NodeId,
    pub kind: GeneratedBoxKind,
    pub rect: Rect,
}

#[derive(Clone, Copy)]
struct GeneratedBoxBuild {
    host: NodeId,
    kind: GeneratedBoxKind,
    node: taffy::NodeId,
}

fn sync_resolved_percentage_padding(
    taffy_tree: &TaffyTree<usize>,
    taffy_root: taffy::NodeId,
    initial_containing_block_width: f32,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    generated: &[GeneratedBoxBuild],
    styles: &mut HashMap<NodeId, crate::LayoutStyle>,
) {
    let has_percentage_padding = styles.values().any(|style| {
        style.padding_percent.iter().any(Option::is_some)
            || style
                .before_pseudo
                .as_deref()
                .is_some_and(|pseudo| pseudo.padding_percent.iter().any(Option::is_some))
            || style
                .after_pseudo
                .as_deref()
                .is_some_and(|pseudo| pseudo.padding_percent.iter().any(Option::is_some))
    });
    if !has_percentage_padding {
        return;
    }

    fn sync(style: &mut crate::LayoutStyle, containing_block_width: f32) {
        if let Some(percent) = style.padding_percent[0] {
            style.padding.top = percent * containing_block_width;
        }
        if let Some(percent) = style.padding_percent[1] {
            style.padding.right = percent * containing_block_width;
        }
        if let Some(percent) = style.padding_percent[2] {
            style.padding.bottom = percent * containing_block_width;
        }
        if let Some(percent) = style.padding_percent[3] {
            style.padding.left = percent * containing_block_width;
        }
    }

    fn visit(
        taffy_tree: &TaffyTree<usize>,
        node: taffy::NodeId,
        containing_block_width: f32,
        id_map: &HashMap<taffy::NodeId, NodeId>,
        generated: &HashMap<taffy::NodeId, (NodeId, GeneratedBoxKind)>,
        styles: &mut HashMap<NodeId, crate::LayoutStyle>,
    ) {
        let Ok(layout) = taffy_tree.layout(node) else {
            return;
        };
        if let Some(dom_id) = id_map.get(&node) {
            if let Some(style) = styles.get_mut(dom_id) {
                sync(style, containing_block_width);
            }
        } else if let Some((host, kind)) = generated.get(&node) {
            if let Some(host_style) = styles.get_mut(host) {
                let pseudo = match kind {
                    GeneratedBoxKind::Before => host_style.before_pseudo.as_deref_mut(),
                    GeneratedBoxKind::After => host_style.after_pseudo.as_deref_mut(),
                };
                if let Some(pseudo) = pseudo {
                    sync(pseudo, containing_block_width);
                }
            }
        }

        let child_containing_block_width = layout.content_box_width().max(0.0);
        if let Ok(children) = taffy_tree.children(node) {
            for child in children {
                visit(
                    taffy_tree,
                    child,
                    child_containing_block_width,
                    id_map,
                    generated,
                    styles,
                );
            }
        }
    }

    let generated = generated
        .iter()
        .map(|generated| (generated.node, (generated.host, generated.kind)))
        .collect();
    visit(
        taffy_tree,
        taffy_root,
        initial_containing_block_width,
        id_map,
        &generated,
        styles,
    );
}

/// Taffy's flex stand-in for an inline formatting context can lose the
/// block-axis percentage basis of an atomic containing block. This is most
/// visible when the atomic box is floated: the float's synthetic placement
/// wrapper has an indefinite block size, so a `height:100%` descendant
/// collapses even though the float itself has a definite used height.
///
/// Blink's `CalculateChildAvailableSize` and Gecko's `ReflowInput` both pass
/// the containing block's used content-box block size to such descendants.
/// Reify that basis after the preliminary layout, when min/max constraints and
/// border-box edges are known exactly. Keep the repair to ordinary direct
/// descendants of definite floated/inline-block containing blocks; grid-area,
/// flex-item, positioned, and anonymous containing-block rules remain native.
fn resolve_atomic_percentage_heights(
    tree: &DomTree,
    taffy_tree: &mut TaffyTree<usize>,
    taffy_root: taffy::NodeId,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    definite_height_nodes: &HashSet<NodeId>,
) -> bool {
    fn visit(
        tree: &DomTree,
        taffy_tree: &mut TaffyTree<usize>,
        node: taffy::NodeId,
        nearest_dom_parent: Option<NodeId>,
        containing_block_height: f32,
        id_map: &HashMap<taffy::NodeId, NodeId>,
        styles: &HashMap<NodeId, crate::LayoutStyle>,
        definite_height_nodes: &HashSet<NodeId>,
        changed: &mut bool,
    ) {
        let Ok(layout) = taffy_tree.layout(node) else {
            return;
        };
        let own_content_height = layout.content_box_height().max(0.0);
        let dom_id = id_map.get(&node).copied();

        if let (Some(dom_id), Some(parent_id)) = (dom_id, nearest_dom_parent) {
            let is_direct_dom_child = rendered_parent(tree, dom_id) == Some(parent_id);
            let child_percent = styles.get(&dom_id).and_then(|style| {
                (style.size_expressions[1].is_none()
                    && !style.ignores_used_box_sizes()
                    && !matches!(style.position, Some(taffy::Position::Absolute)))
                .then_some(style.height)
                .and_then(|height| match height {
                    crate::Dimension::Percent(percent) => Some(percent),
                    _ => None,
                })
            });
            let parent_is_definite_atomic = styles.get(&parent_id).is_some_and(|style| {
                (style.float.is_some() || style.is_inline_block)
                    && definite_height_nodes.contains(&parent_id)
                    && style.size_expressions[1].is_none()
            });
            if is_direct_dom_child
                && parent_is_definite_atomic
                && containing_block_height.is_finite()
            {
                if let Some(percent) = child_percent {
                    if let Ok(current) = taffy_tree.style(node) {
                        let mut resolved = current.clone();
                        resolved.size.height =
                            taffy::Dimension::length((percent * containing_block_height).max(0.0));
                        if taffy_tree.set_style(node, resolved).is_ok() {
                            *changed = true;
                        }
                    }
                }
            }
        }

        let next_dom_parent = dom_id.or(nearest_dom_parent);
        let next_containing_block_height = if dom_id.is_some() {
            own_content_height
        } else {
            containing_block_height
        };
        let children = taffy_tree.children(node).unwrap_or_default();
        for child in children {
            visit(
                tree,
                taffy_tree,
                child,
                next_dom_parent,
                next_containing_block_height,
                id_map,
                styles,
                definite_height_nodes,
                changed,
            );
        }
    }

    let mut changed = false;
    visit(
        tree,
        taffy_tree,
        taffy_root,
        None,
        0.0,
        id_map,
        styles,
        definite_height_nodes,
        &mut changed,
    );
    changed
}

fn sync_positioned_pseudo_percentage_padding(
    rects: &HashMap<NodeId, Rect>,
    styles: &mut HashMap<NodeId, crate::LayoutStyle>,
) {
    let has_positioned_percentage_padding = styles.values().any(|style| {
        [
            style.before_pseudo.as_deref(),
            style.after_pseudo.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|pseudo| {
            pseudo.position == Some(taffy::Position::Absolute)
                && pseudo.padding_percent.iter().any(Option::is_some)
        })
    });
    if !has_positioned_percentage_padding {
        return;
    }

    fn sync(pseudo: &mut crate::LayoutStyle, containing_block_width: f32) {
        if pseudo.position != Some(taffy::Position::Absolute) {
            return;
        }
        for (index, percent) in pseudo.padding_percent.into_iter().enumerate() {
            let Some(percent) = percent else { continue };
            let value = percent * containing_block_width;
            match index {
                0 => pseudo.padding.top = value,
                1 => pseudo.padding.right = value,
                2 => pseudo.padding.bottom = value,
                _ => pseudo.padding.left = value,
            }
        }
    }

    for (host, style) in styles {
        let Some(rect) = rects.get(host) else {
            continue;
        };
        let containing_block_width = (rect.width - style.border.left - style.border.right).max(0.0);
        if let Some(pseudo) = style.before_pseudo.as_deref_mut() {
            sync(pseudo, containing_block_width);
        }
        if let Some(pseudo) = style.after_pseudo.as_deref_mut() {
            sync(pseudo, containing_block_width);
        }
    }
}

/// Registry of cosmic-text inline formatting contexts created during the
/// taffy-tree build: `whole` holds single-leaf containers keyed by the
/// container's own DOM id, `runs` holds anonymous inline-run leaves keyed by
/// the parent block's DOM id (those leaves have no DOM node, so their final
/// rects are captured separately; see `compute_absolute_rects`).
#[derive(Default)]
struct IfcRegistry {
    whole: HashMap<NodeId, usize>,
    runs: HashMap<NodeId, Vec<usize>>,
    word_items: HashMap<NodeId, Vec<usize>>,
    generated: Vec<GeneratedBoxBuild>,
    /// Specified column widths per table grid node, from `<col>` elements and
    /// colspan-1 cells: `(px, percent)` per column index. Consumed by the
    /// table column-balancing pass in `layout_dom_with_images`, which pins
    /// specified columns instead of sizing them purely from content.
    table_cols: HashMap<taffy::NodeId, (Vec<Option<f32>>, Vec<Option<f32>>)>,
    /// Column constraints for the fixed table-layout algorithm. Unlike
    /// `table_cols`, these are sourced only from columns and the first row and
    /// deliberately exclude later-row intrinsic content.
    fixed_table_cols: HashMap<taffy::NodeId, Vec<FixedTableColumn>>,
    /// Minimum row heights per table grid node, one entry per source row.
    /// The post-width row-sizing pass combines these with each cell's final
    /// content height and pins the tracks before vertical alignment.
    table_rows: HashMap<taffy::NodeId, Vec<Option<f32>>>,
    /// Floats whose direct block parent does not establish a block formatting
    /// context and whose exclusion can therefore continue through later
    /// descendant blocks of the same BFC. The first layout pass gives these
    /// nodes real geometry; `apply_float_continuations` uses it to narrow the
    /// later intersecting bands before the final layout.
    float_continuations: Vec<FloatContinuation>,
    /// Ordinary blocks participating in a native float band must keep a
    /// distinct inner line box. Collapsing the block and all of its text into
    /// one measured leaf gives Taffy no descendant at which the shared BFC
    /// float context can shorten the line while retaining the block's full
    /// border-box width.
    float_aware_blocks: HashSet<NodeId>,
    /// CSS multi-column containers built as a row of anonymous block
    /// fragmentainers. All in-flow boxes start in the first fragmentainer so
    /// the preliminary layout measures them at the final column width; the
    /// post-pass then balances those already-built boxes without rebuilding
    /// or reshaping their subtrees.
    multicol: Vec<MulticolBuild>,
}

#[derive(Clone, Copy, Debug, Default)]
struct FixedTableColumn {
    length: f32,
    percentage: f32,
    specified: bool,
}

struct MulticolBuild {
    columns: Vec<taffy::NodeId>,
    children: Vec<taffy::NodeId>,
}

#[derive(Clone, Copy)]
struct FloatContinuation {
    owner: NodeId,
    float: taffy::NodeId,
    flow: taffy::NodeId,
    side: crate::Float,
}

/// Walk the tree top-down accumulating the clip rect imposed by ancestor
/// `overflow: hidden` boxes. Must run after layout, since it needs border
/// boxes. This is what makes `overflow: hidden` (used pervasively for the
/// "visually hidden but accessible" pattern: a 1x1 absolutely-positioned box
/// with clipped overflow) actually hide its contents instead of letting text
/// paint wherever the box's static position happens to land.
/// Clips are stored in SCREEN space: the clip owner's border box offset by the
/// owner's own accumulated `transform: translate()`. A clip belongs to its
/// owner's coordinate space, not the painted descendant's; shifting an
/// inherited clip by the descendant's translate (the old behavior) let a
/// carousel track's `translateX` drag the viewport's clip along with every
/// slide, so all slides stayed visible inside their own shifted clip.
///
/// The same walk records each node's accumulated translate in `translates`
/// (one pass here instead of an ancestor walk per painted node).
fn resolve_clip_rects(
    tree: &DomTree,
    id: NodeId,
    inherited: Option<OverflowClip>,
    tx: f32,
    ty: f32,
    rects: &HashMap<NodeId, Rect>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    clip_rects: &mut HashMap<NodeId, Option<OverflowClip>>,
    translates: &mut HashMap<NodeId, (f32, f32)>,
    parent_transform: crate::Affine2,
    transforms: &mut HashMap<NodeId, crate::Affine2>,
    root_font_size: f32,
    viewport: (f32, f32),
) {
    clip_rects.insert(id, inherited.clone());
    // This node's own translate joins the accumulation for its box and its
    // whole subtree (percentages resolve against its own border box).
    let (own_tx, own_ty) = styles.get(&id).map_or((0.0, 0.0), |style| {
        let rect = rects.get(&id).copied().unwrap_or_default();
        resolved_own_translate(style, &rect, root_font_size, viewport)
    });
    let (tx, ty) = (tx + own_tx, ty + own_ty);
    if tx != 0.0 || ty != 0.0 {
        translates.insert(id, (tx, ty));
    }
    let own_transform = styles.get(&id).map_or(crate::Affine2::IDENTITY, |style| {
        let rect = rects.get(&id).copied().unwrap_or_default();
        resolved_transform_matrix(style, &rect, root_font_size, viewport)
    });
    let transform = parent_transform.then(own_transform);
    if !transform.is_identity() {
        transforms.insert(id, transform);
    }
    // Overflow propagated from html/body establishes the root scrolling
    // viewport. It is anchored to the capture surface, not to document
    // coordinates, and the pixmap already supplies that viewport clip.
    // Materializing it here as an ordinary descendant clip would make root
    // scrolling translate the viewport itself offscreen.
    let next = match (styles.get(&id), rects.get(&id)) {
        (Some(style), Some(rect))
            if style.overflow_hidden && !is_viewport_overflow_source(id, styles) =>
        {
            let own = OverflowClip::for_box(rect, style, tx, ty);
            Some(match inherited {
                Some(clip) => clip.intersect(own),
                None => own,
            })
        }
        _ => inherited,
    };
    for cid in rendered_children(tree, id) {
        resolve_clip_rects(
            tree,
            cid,
            next.clone(),
            tx,
            ty,
            rects,
            styles,
            clip_rects,
            translates,
            transform,
            transforms,
            root_font_size,
            viewport,
        );
    }
}

fn resolved_own_translate(
    style: &crate::LayoutStyle,
    rect: &Rect,
    root_font_size: f32,
    viewport: (f32, f32),
) -> (f32, f32) {
    let transform = resolved_transform_matrix(style, rect, root_font_size, viewport);
    if transform.is_translation() {
        (transform.e, transform.f)
    } else {
        (0.0, 0.0)
    }
}

fn resolve_transform_length(
    length: &crate::TransformLength,
    axis: usize,
    style: &crate::LayoutStyle,
    rect: &Rect,
    root_font_size: f32,
    viewport: (f32, f32),
) -> f32 {
    let basis = if axis == 0 { rect.width } else { rect.height };
    let em = style.font_size.unwrap_or(16.0);
    length
        .expression
        .as_deref()
        .and_then(|expression| {
            crate::style::resolve_contextual_length(
                expression,
                em,
                root_font_size,
                viewport.0 / 100.0,
                viewport.1 / 100.0,
                basis,
            )
        })
        .unwrap_or_else(|| resolve_translate(length.value, basis))
}

pub(crate) fn resolved_transform_matrix(
    style: &crate::LayoutStyle,
    rect: &Rect,
    root_font_size: f32,
    viewport: (f32, f32),
) -> crate::Affine2 {
    let mut matrix = crate::Affine2::IDENTITY;
    if let Some((x, y)) = style.individual_translate {
        let lengths = [
            crate::TransformLength {
                value: x,
                expression: style.individual_translate_expressions[0].clone(),
            },
            crate::TransformLength {
                value: y,
                expression: style.individual_translate_expressions[1].clone(),
            },
        ];
        matrix = matrix.then(crate::Affine2::translate(
            resolve_transform_length(&lengths[0], 0, style, rect, root_font_size, viewport),
            resolve_transform_length(&lengths[1], 1, style, rect, root_font_size, viewport),
        ));
    }
    if let Some(angle) = style.individual_rotate {
        matrix = matrix.then(crate::Affine2::rotate(angle));
    }
    if let Some((x, y)) = style.individual_scale {
        matrix = matrix.then(crate::Affine2::scale(x, y));
    }
    matrix = matrix.then(resolved_transform_property_matrix(
        style,
        rect,
        root_font_size,
        viewport,
    ));
    if matrix.is_identity() {
        return matrix;
    }
    let (origin_x, origin_y) = style.transform_origin.unwrap_or((
        crate::Dimension::Percent(0.5),
        crate::Dimension::Percent(0.5),
    ));
    matrix.around((
        rect.x + resolve_translate(origin_x, rect.width),
        rect.y + resolve_translate(origin_y, rect.height),
    ))
}

/// Resolved value of the `transform` property alone. Individual transform
/// properties and `transform-origin` are intentionally excluded: CSSOM
/// serializes them as separate computed properties.
pub(crate) fn resolved_transform_property_matrix(
    style: &crate::LayoutStyle,
    rect: &Rect,
    root_font_size: f32,
    viewport: (f32, f32),
) -> crate::Affine2 {
    let mut matrix = crate::Affine2::IDENTITY;
    for operation in &style.transform_ops {
        let operation = match operation {
            crate::TransformOp::Translate(x, y) => crate::Affine2::translate(
                resolve_transform_length(x, 0, style, rect, root_font_size, viewport),
                resolve_transform_length(y, 1, style, rect, root_font_size, viewport),
            ),
            crate::TransformOp::Scale(x, y) => crate::Affine2::scale(*x, *y),
            crate::TransformOp::Rotate(angle) => crate::Affine2::rotate(*angle),
            crate::TransformOp::Skew(x, y) => crate::Affine2::skew(*x, *y),
            crate::TransformOp::Matrix(matrix) => *matrix,
        };
        matrix = matrix.then(operation);
    }
    matrix
}

/// The CSS overflow clip is the padding box, not the outer border box. This is
/// also the coordinate-space boundary Gecko captures for descendant display
/// items: transformed content may cover the padding area but must not repaint
/// its clip owner's border.
/// Resolve one `transform: translate()` component to px: a length passes
/// through, a percentage is taken against `basis` (the element's own
/// border-box extent on that axis). Font/viewport-relative leftovers fall
/// back to a coarse px value (translate rarely uses them).
pub(crate) fn resolve_translate(d: crate::Dimension, basis: f32) -> f32 {
    match d {
        crate::Dimension::Px(px) => px,
        crate::Dimension::Percent(p) => p * basis,
        crate::Dimension::Em(v) | crate::Dimension::Rem(v) => v * 16.0,
        crate::Dimension::Ex(v) => v * 16.0 * 0.528_320_3,
        crate::Dimension::Vw(v)
        | crate::Dimension::Vh(v)
        | crate::Dimension::Vmin(v)
        | crate::Dimension::Vmax(v) => v,
        crate::Dimension::Auto => 0.0,
    }
}

/// Apply HTML presentational attributes at their cascade origin: above the UA
/// defaults, but below every author stylesheet and style attribute.
fn apply_presentational_hints(node: &obscura_dom::tree::Node, style: &mut crate::LayoutStyle) {
    if let Some(direction) = node.get_attribute("dir") {
        style.direction = match direction.trim().to_ascii_lowercase().as_str() {
            "ltr" => Some(taffy::Direction::Ltr),
            "rtl" => Some(taffy::Direction::Rtl),
            _ => style.direction,
        };
    }
    if let Some(color) = node.get_attribute("color") {
        crate::style::apply_inline(style, &format!("color: {}", color));
    }
    if let Some(bgcolor) = node.get_attribute("bgcolor") {
        crate::style::apply_inline(style, &format!("background-color: {}", bgcolor));
    }
    if let Some(width) = node.get_attribute("width") {
        if width.chars().all(|c| c.is_ascii_digit()) {
            crate::style::apply_inline(style, &format!("width: {}px", width));
        } else {
            crate::style::apply_inline(style, &format!("width: {}", width));
        }
    }
    if let Some(align) = node.get_attribute("align") {
        crate::style::apply_inline(style, &format!("text-align: {}", align));
    }
    if let Some(valign) = node.get_attribute("valign") {
        crate::style::apply_inline(style, &format!("vertical-align: {}", valign));
    }
    if let Some(cellspacing) = node.get_attribute("cellspacing") {
        if cellspacing.chars().all(|c| c.is_ascii_digit()) {
            crate::style::apply_inline(style, &format!("border-spacing: {}px", cellspacing));
        }
    }
    if let Some(height) = node.get_attribute("height") {
        if height.chars().all(|c| c.is_ascii_digit()) {
            crate::style::apply_inline(style, &format!("height: {}px", height));
        } else {
            crate::style::apply_inline(style, &format!("height: {}", height));
        }
    }
    if style.aspect_ratio.is_none() {
        let aw = node
            .get_attribute("width")
            .and_then(|w| w.parse::<f32>().ok());
        let ah = node
            .get_attribute("height")
            .and_then(|h| h.parse::<f32>().ok());
        if let (Some(w), Some(h)) = (aw, ah) {
            if w > 0.0 && h > 0.0 {
                style.aspect_ratio = Some(w / h);
                style.aspect_ratio_is_mapped = true;
                style.aspect_ratio_is_intrinsic = true;
            }
        }
    }
    // An inline SVG with one CSS axis and a viewBox derives the other axis
    // from the viewBox's intrinsic aspect ratio. Framework logos commonly set
    // only a responsive utility width (`w-24 lg:w-28`) and rely on this rule;
    // without it the SVG border box had zero height and paint skipped it.
    if style.aspect_ratio.is_none()
        && node
            .as_element()
            .map_or(false, |element| element.local.as_ref() == "svg")
    {
        if let Some(view_box) = node
            .get_attribute("viewBox")
            .or_else(|| node.get_attribute("viewbox"))
        {
            let values: Vec<f32> = view_box
                .split(|c: char| c.is_ascii_whitespace() || c == ',')
                .filter(|part| !part.is_empty())
                .filter_map(|part| part.parse::<f32>().ok())
                .collect();
            if values.len() == 4 && values[2] > 0.0 && values[3] > 0.0 {
                style.aspect_ratio = Some(values[2] / values[3]);
                style.aspect_ratio_is_intrinsic = true;
            }
        }
    }
}

/// The selected `<picture><source>` contributes presentation hints to its
/// associated `<img>`. In particular, source width/height replace the fallback
/// image attributes and provide the pre-load aspect ratio. Gecko exposes this
/// as an extra mapped declaration block on HTMLImageElement; doing the same
/// before the author cascade preserves the correct origin and precedence.
fn apply_picture_source_hints(
    tree: &DomTree,
    img_id: NodeId,
    viewport: (f32, f32),
    style: &mut crate::LayoutStyle,
) {
    let Some(img) = tree.get_node(img_id) else {
        return;
    };
    if img
        .as_element()
        .map_or(true, |element| element.local.as_ref() != "img")
    {
        return;
    }
    let Some(parent_id) = img.parent else { return };
    let is_picture = tree.get_node(parent_id).is_some_and(|parent| {
        parent
            .as_element()
            .is_some_and(|element| element.local.as_ref() == "picture")
    });
    if !is_picture {
        return;
    }

    let mut selected = None;
    for child_id in tree.children(parent_id) {
        if child_id == img_id {
            break;
        }
        let Some(source) = tree.get_node(child_id) else {
            continue;
        };
        if source
            .as_element()
            .map_or(true, |element| element.local.as_ref() != "source")
        {
            continue;
        }
        if source
            .get_attribute("srcset")
            .is_none_or(|srcset| srcset.trim().is_empty())
        {
            continue;
        }
        if let Some(media) = source.get_attribute("media") {
            if !media.trim().is_empty()
                && !crate::css::media_query_applies_for_viewport(media, viewport)
            {
                continue;
            }
        }
        if let Some(kind) = source.get_attribute("type") {
            if !crate::source_type_supported(kind) {
                continue;
            }
        }
        selected = Some(source);
        break;
    }
    let Some(source) = selected else { return };
    let parse_dimension = |name| {
        source
            .get_attribute(name)
            .and_then(|value| value.trim().parse::<f32>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
    };
    let width = parse_dimension("width");
    let height = parse_dimension("height");
    if width.is_none() && height.is_none() {
        return;
    }

    // A missing source dimension explicitly maps to auto so it replaces the
    // corresponding fallback <img> presentation hint.
    style.width = width.map_or(crate::Dimension::Auto, crate::Dimension::Px);
    style.height = height.map_or(crate::Dimension::Auto, crate::Dimension::Px);
    style.width_set = true;
    style.height_set = true;
    style.aspect_ratio = match (width, height) {
        (Some(width), Some(height)) => Some(width / height),
        _ => None,
    };
    style.aspect_ratio_is_mapped = style.aspect_ratio.is_some();
    style.aspect_ratio_is_intrinsic = style.aspect_ratio.is_some();
}

/// Compute the UA + author style for every element in preorder, maintaining
/// `matcher`'s ancestor filter as we descend so descendant-combinator rules
/// fast-reject correctly. Non-element nodes (text, comments) are skipped but
/// still walked through, since an element may be their descendant.
fn cascade_walk(
    tree: &DomTree,
    id: NodeId,
    sheet: &crate::css::Stylesheet,
    document_sheet: &crate::css::Stylesheet,
    shadow_sheets: &HashMap<NodeId, std::sync::Arc<crate::css::Stylesheet>>,
    matcher: &mut obscura_dom::selector::Matcher,
    styles: &mut HashMap<NodeId, crate::LayoutStyle>,
    custom_properties: &mut HashMap<NodeId, std::rc::Rc<HashMap<String, String>>>,
    parent_props: &std::rc::Rc<HashMap<String, String>>,
    container_evaluator: &mut Option<&mut crate::css::ContainerQueryEvaluator<'_>>,
    quirks_mode: bool,
    viewport: (f32, f32),
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
    inherited_cell_padding: Option<f32>,
    inherited_color_scheme_dark: bool,
    fresh_styles: Option<&HashSet<NodeId>>,
) {
    let Some(node) = tree.get_node(id) else {
        return;
    };
    let is_element = node.is_element();
    // The custom-property map in force for this node's subtree: the parent's,
    // unless this element declares its own `--x` (then a richer map).
    let mut this_props = parent_props.clone();
    let mut descendant_cell_padding = inherited_cell_padding;
    let mut descendant_color_scheme_dark = inherited_color_scheme_dark;
    let reuse_style = node.is_element()
        && fresh_styles.is_some_and(|fresh| !fresh.contains(&id))
        && styles.contains_key(&id)
        && custom_properties.contains_key(&id);
    if reuse_style {
        this_props = custom_properties
            .get(&id)
            .cloned()
            .unwrap_or_else(|| parent_props.clone());
        descendant_color_scheme_dark = styles
            .get(&id)
            .is_some_and(|style| style.color_scheme_dark);
        if node.as_element().is_some_and(|elem| elem.local.as_ref() == "table") {
            descendant_cell_padding = node
                .get_attribute("cellpadding")
                .and_then(|value| value.trim().parse::<f32>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0);
        }
    } else if let Some(elem) = node.as_element() {
        if elem.local.as_ref() == "table" {
            // `cellpadding` is a table-scoped presentational hint applied to
            // its cells, below author CSS. Entering any nested table resets
            // the outer table's value before reading the nested attribute.
            descendant_cell_padding = node
                .get_attribute("cellpadding")
                .and_then(|value| value.trim().parse::<f32>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0);
        }
        let mut style = crate::style::ua_style(elem.local.as_ref());
        style.is_replaced_box = crate::inline::is_replaced(elem.local.as_ref());
        style.has_replaced_sizing = crate::inline::has_replaced_sizing(elem.local.as_ref())
            || (elem.local.as_ref() == "input"
                && node
                    .get_attribute("type")
                    .is_some_and(|value| value.eq_ignore_ascii_case("image")));
        style.color_scheme_dark = inherited_color_scheme_dark;
        if matches!(elem.local.as_ref(), "dir" | "dl" | "menu" | "ol" | "ul") {
            let mut list_ancestor_count = 0usize;
            let mut has_list_or_definition_ancestor = false;
            let mut ancestor = node.parent;
            while let Some(ancestor_id) = ancestor {
                let Some(ancestor_node) = tree.get_node(ancestor_id) else {
                    break;
                };
                if let Some(ancestor_element) = ancestor_node.as_element() {
                    let local = ancestor_element.local.as_ref();
                    if matches!(local, "dir" | "dl" | "menu" | "ol" | "ul") {
                        has_list_or_definition_ancestor = true;
                    }
                    if matches!(local, "dir" | "menu" | "ol" | "ul") {
                        list_ancestor_count += 1;
                    }
                }
                ancestor = ancestor_node.parent;
            }
            if has_list_or_definition_ancestor {
                style.margin.top = 0.0;
                style.margin.bottom = 0.0;
                style.margin_relative[0] = None;
                style.margin_relative[2] = None;
            }
            if matches!(elem.local.as_ref(), "dir" | "menu" | "ul") {
                style.list_style = match list_ancestor_count {
                    0 => Some(crate::ListStyle::Disc),
                    1 => Some(crate::ListStyle::Circle),
                    _ => Some(crate::ListStyle::Square),
                };
            }
        }
        if matches!(elem.local.as_ref(), "td" | "th") {
            if let Some(padding) = inherited_cell_padding {
                style.padding = crate::Edges {
                    top: padding,
                    right: padding,
                    bottom: padding,
                    left: padding,
                };
            }
        }
        if quirks_mode && elem.local.as_ref() == "form" {
            // Legacy HTML/quirks rendering keeps one em after forms. Standards
            // mode does not; Hacker News and many old document templates omit
            // a doctype and rely on this spacing.
            style.margin_relative[2] = Some(crate::Dimension::Em(1.0));
        }
        if elem.local.as_ref() == "input" {
            let input_type = node
                .get_attribute("type")
                .unwrap_or("text")
                .trim()
                .to_ascii_lowercase();
            if quirks_mode {
                style.box_sizing = crate::BoxSizing::BorderBox;
            }
            match input_type.as_str() {
                "checkbox" | "radio" => {
                    style.margin = crate::Edges {
                        top: 3.0,
                        right: 3.0,
                        bottom: 3.0,
                        left: 4.0,
                    };
                    style.padding = crate::Edges::default();
                    style.border = crate::Edges::default();
                }
                "range" | "color" => {
                    style.margin = crate::Edges {
                        top: 2.0,
                        right: 2.0,
                        bottom: 2.0,
                        left: 2.0,
                    };
                    style.padding = crate::Edges::default();
                    style.border = crate::Edges::default();
                }
                _ => {}
            }
        }
        // UA rule `[hidden]:not([hidden=until-found]) { display: none }`.
        // Applied before the author cascade so a matching author `display`
        // still wins (UA origin, per the HTML rendering spec). Sites ship
        // dialogs, tab panels, and menus with a plain `hidden` attribute and
        // reveal them by removing it (eBay's homepage interstitial uses
        // `.lightbox-dialog[role=dialog]:not([hidden]){display:flex}`), so
        // without this the hidden overlay paints its full-page dark backdrop.
        if let Some(h) = node.get_attribute("hidden") {
            if !h.eq_ignore_ascii_case("until-found") {
                style.display = crate::Display::None;
            }
        }
        apply_presentational_hints(&node, &mut style);
        apply_picture_source_hints(tree, id, viewport, &mut style);
        let node_id = node.get_attribute("id");
        let classes: Vec<String> = node
            .get_attribute("class")
            .map(|c| c.split_whitespace().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        let shadow_host_sheet = tree
            .shadow_root(id)
            .and_then(|root| shadow_sheets.get(&root));
        let mut slotted_scopes = Vec::new();
        let mut assigned_slot = tree.assigned_slot(id);
        for _ in 0..tree.len() {
            let Some(slot) = assigned_slot else { break };
            let Some(root) = tree.containing_shadow_root(slot) else {
                break;
            };
            let Some(host) = tree.shadow_root_info(root).map(|root| root.host) else {
                break;
            };
            if let Some(shadow_sheet) = shadow_sheets.get(&root) {
                slotted_scopes.push(crate::css::ShadowSlottedScope {
                    sheet: shadow_sheet,
                    host,
                });
            }
            assigned_slot = tree.assigned_slot(slot);
        }
        let effective_props = if shadow_host_sheet.is_some() || !slotted_scopes.is_empty() {
            sheet.apply_with_shadow_scopes_at_animation_time(
                shadow_host_sheet.map(|sheet| &**sheet),
                &slotted_scopes,
                tree,
                matcher,
                id,
                node_id,
                &classes,
                elem.local.as_ref(),
                &mut style,
                parent_props,
                node.get_attribute("style"),
                container_evaluator.as_deref_mut(),
                animation_sample,
                animation_timeline,
            )
        } else if let Some(evaluator) = container_evaluator.as_deref_mut() {
            sheet.apply_with_container_queries_at_animation_time(
                tree,
                matcher,
                id,
                node_id,
                &classes,
                elem.local.as_ref(),
                &mut style,
                parent_props,
                node.get_attribute("style"),
                evaluator,
                animation_sample,
                animation_timeline,
            )
        } else {
            sheet.apply_at_animation_time(
                tree,
                matcher,
                id,
                node_id,
                &classes,
                elem.local.as_ref(),
                &mut style,
                parent_props,
                node.get_attribute("style"),
                animation_sample,
                animation_timeline,
            )
        };
        if let Some(m) = effective_props {
            this_props = std::rc::Rc::new(m);
        }
        custom_properties.insert(id, this_props.clone());
        style.is_replaced_box |= style.content_image.is_some();
        style.has_replaced_sizing |= style.content_image.is_some();
        let (mut before_pseudo, mut after_pseudo, placeholder_pseudo) = sheet.all_pseudo_styles(
            tree,
            matcher,
            id,
            &this_props,
            &style,
            container_evaluator.as_deref_mut(),
        );
        for pseudo in [&mut before_pseudo, &mut after_pseudo]
            .into_iter()
            .flatten()
        {
            pseudo.is_replaced_box = pseudo.content_image.is_some();
            pseudo.has_replaced_sizing = pseudo.content_image.is_some();
        }
        style.before_content = before_pseudo
            .as_ref()
            .filter(|pseudo| pseudo.position != Some(taffy::Position::Absolute))
            .and_then(|pseudo| pseudo.before_content.clone());
        style.after_content = after_pseudo
            .as_ref()
            .filter(|pseudo| pseudo.position != Some(taffy::Position::Absolute))
            .and_then(|pseudo| pseudo.before_content.clone());
        style.before_pseudo = before_pseudo.map(Box::new);
        style.after_pseudo = after_pseudo.map(Box::new);
        style.placeholder_pseudo = placeholder_pseudo.map(Box::new);
        descendant_color_scheme_dark = style.color_scheme_dark;
        styles.insert(id, style);
    }

    if is_element {
        matcher.push_ancestor(tree, id);
    }
    let is_shadow_host = tree.shadow_root(id).is_some();
    for cid in tree.children(id) {
        // Assigned light children are cascaded from their flattened slot
        // below. Unslotted children still need CSSOM-computed styles and stay
        // on the ordinary host path.
        if is_shadow_host && tree.assigned_slot(cid).is_some() {
            continue;
        }
        cascade_walk(
            tree,
            cid,
            sheet,
            document_sheet,
            shadow_sheets,
            matcher,
            styles,
            custom_properties,
            &this_props,
            container_evaluator,
            quirks_mode,
            viewport,
            animation_sample,
            animation_timeline,
            descendant_cell_padding,
            descendant_color_scheme_dark,
            fresh_styles,
        );
    }
    if let Some(assigned_nodes) = tree.assigned_nodes(id) {
        for cid in assigned_nodes {
            let assigned_sheet = tree
                .containing_shadow_root(cid)
                .and_then(|root| shadow_sheets.get(&root).map(|sheet| &**sheet))
                .unwrap_or(document_sheet);
            let mut assigned_matcher = tree.matcher();
            for ancestor in tree.ancestors(cid).into_iter().rev() {
                if tree
                    .get_node(ancestor)
                    .is_some_and(|node| node.is_element())
                {
                    assigned_matcher.push_ancestor(tree, ancestor);
                }
            }
            cascade_walk(
                tree,
                cid,
                assigned_sheet,
                document_sheet,
                shadow_sheets,
                &mut assigned_matcher,
                styles,
                custom_properties,
                &this_props,
                container_evaluator,
                quirks_mode,
                viewport,
                animation_sample,
                animation_timeline,
                descendant_cell_padding,
                descendant_color_scheme_dark,
                fresh_styles,
            );
        }
    }
    if let Some(root) = tree.shadow_root(id) {
        // Start matching with an empty ancestor filter at each tree-scope
        // boundary. That keeps document rules out of the shadow tree and
        // shadow rules out of the document/other roots by construction.
        if let Some(shadow_sheet) = shadow_sheets.get(&root) {
            let mut shadow_matcher = tree.matcher();
            let mut no_container_evaluator = None;
            for child in tree.children(root) {
                cascade_walk(
                    tree,
                    child,
                    shadow_sheet,
                    document_sheet,
                    shadow_sheets,
                    &mut shadow_matcher,
                    styles,
                    custom_properties,
                    &this_props,
                    &mut no_container_evaluator,
                    quirks_mode,
                    viewport,
                    animation_sample,
                    animation_timeline,
                    None,
                    descendant_color_scheme_dark,
                    fresh_styles,
                );
            }
        }
    }
    if is_element {
        matcher.pop_ancestor();
    }
}

#[derive(Default)]
struct CssCounterState {
    values: HashMap<String, Vec<i32>>,
}

impl CssCounterState {
    fn apply(
        &mut self,
        reset: &[crate::CounterDirective],
        increment: &[crate::CounterDirective],
        set: &[crate::CounterDirective],
    ) -> Vec<String> {
        let mut created = Vec::new();
        for directive in reset {
            self.values
                .entry(directive.name.clone())
                .or_default()
                .push(directive.value);
            created.push(directive.name.clone());
        }
        for directive in increment {
            let stack = self.values.entry(directive.name.clone()).or_default();
            if stack.is_empty() {
                stack.push(0);
                created.push(directive.name.clone());
            }
            if let Some(value) = stack.last_mut() {
                *value = value.saturating_add(directive.value);
            }
        }
        for directive in set {
            let stack = self.values.entry(directive.name.clone()).or_default();
            if stack.is_empty() {
                stack.push(0);
                created.push(directive.name.clone());
            }
            if let Some(value) = stack.last_mut() {
                *value = directive.value;
            }
        }
        created
    }

    fn pop_created(&mut self, created: &[String]) {
        for name in created.iter().rev() {
            if let Some(stack) = self.values.get_mut(name) {
                stack.pop();
                if stack.is_empty() {
                    self.values.remove(name);
                }
            }
        }
    }

    fn render(&self, items: &[crate::GeneratedContentItem]) -> String {
        let mut result = String::new();
        for item in items {
            match item {
                crate::GeneratedContentItem::Text(text) => result.push_str(text),
                crate::GeneratedContentItem::Counter { name, style } => {
                    let value = self
                        .values
                        .get(name)
                        .and_then(|stack| stack.last())
                        .copied()
                        .unwrap_or(0);
                    result.push_str(&crate::css::format_counter_value(value, *style));
                }
                crate::GeneratedContentItem::Counters {
                    name,
                    separator,
                    style,
                } => {
                    if let Some(stack) = self.values.get(name).filter(|stack| !stack.is_empty()) {
                        for (index, value) in stack.iter().enumerate() {
                            if index != 0 {
                                result.push_str(separator);
                            }
                            result.push_str(&crate::css::format_counter_value(*value, *style));
                        }
                    } else {
                        result.push_str(&crate::css::format_counter_value(0, *style));
                    }
                }
            }
        }
        result
    }
}

/// Resolve generated CSS counter text in tree order after the complete author
/// cascade is known. Counter scopes created by an element remain visible to
/// its descendants and following siblings, and expire with their shared
/// parent. That is the scope shape used by browser counter managers and covers
/// nested chapter numbering as well as line counters reset on a `<code>`.
fn resolve_css_counters(tree: &DomTree, styles: &mut HashMap<NodeId, crate::LayoutStyle>) {
    fn walk(
        tree: &DomTree,
        id: NodeId,
        styles: &mut HashMap<NodeId, crate::LayoutStyle>,
        counters: &mut CssCounterState,
    ) -> Vec<String> {
        let Some(node) = tree.get_node(id) else {
            return Vec::new();
        };
        if styles
            .get(&id)
            .is_some_and(|style| style.display == crate::Display::None)
        {
            return Vec::new();
        }

        let created = styles.get(&id).map_or_else(Vec::new, |style| {
            counters.apply(
                &style.counter_reset,
                &style.counter_increment,
                &style.counter_set,
            )
        });

        if let Some(style) = styles.get_mut(&id) {
            if let Some(pseudo) = style.before_pseudo.as_mut() {
                if let Some(items) = pseudo.generated_content.as_deref() {
                    pseudo.before_content = Some(counters.render(items));
                }
            }
            style.before_content = style
                .before_pseudo
                .as_ref()
                .filter(|pseudo| pseudo.position != Some(taffy::Position::Absolute))
                .and_then(|pseudo| pseudo.before_content.clone());
        }

        let mut child_scopes = Vec::new();
        for child in rendered_children(tree, id) {
            child_scopes.extend(walk(tree, child, styles, counters));
        }
        counters.pop_created(&child_scopes);

        if let Some(style) = styles.get_mut(&id) {
            if let Some(pseudo) = style.after_pseudo.as_mut() {
                if let Some(items) = pseudo.generated_content.as_deref() {
                    pseudo.before_content = Some(counters.render(items));
                }
            }
            style.after_content = style
                .after_pseudo
                .as_ref()
                .filter(|pseudo| pseudo.position != Some(taffy::Position::Absolute))
                .and_then(|pseudo| pseudo.before_content.clone());
        }

        // Non-element nodes cannot create counter scopes, but walking through
        // them keeps this robust to document fragments and template wrappers.
        let _ = node;
        created
    }

    let mut counters = CssCounterState::default();
    let root_scopes = walk(tree, tree.document(), styles, &mut counters);
    counters.pop_created(&root_scopes);
}

/// Lay out a DOM tree within `viewport` (width, height) in CSS pixels.
pub fn layout_dom(tree: &DomTree, viewport: (f32, f32)) -> DomLayout {
    layout_dom_with_images(tree, viewport, &HashMap::new())
}

/// Like [`layout_dom`], but `intrinsic` supplies fetched intrinsic pixel sizes
/// (width, height) for replaced elements keyed by `NodeId`. Paint collects
/// these before layout (it has the base URL and the image cache) so a
/// CSS-sized `<img>` with no width/height attribute gets a real box from its
/// intrinsic ratio instead of collapsing to zero area.
pub fn layout_dom_with_images(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &HashMap<NodeId, (f32, f32)>,
) -> DomLayout {
    layout_dom_with_resources(tree, viewport, intrinsic, &[])
}

/// Like [`layout_dom_with_images`], with decoded OpenType web-font data loaded
/// into the shaping database for this render pass.
pub fn layout_dom_with_resources(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &HashMap<NodeId, (f32, f32)>,
    fonts: &[Vec<u8>],
) -> DomLayout {
    let fonts: Vec<_> = fonts
        .iter()
        .map(|data| crate::inline::WebFont {
            data: data.clone(),
            family: None,
            weight: None,
            italic: None,
        })
        .collect();
    let intrinsic = intrinsic
        .iter()
        .filter_map(|(&nid, &(width, height))| {
            (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
                .then(|| (nid, crate::ReplacedIntrinsic::from_dimensions(width, height)))
        })
        .collect();
    layout_dom_with_web_fonts(tree, viewport, &intrinsic, &fonts)
}

type ReplacedIntrinsicMap = HashMap<NodeId, crate::ReplacedIntrinsic>;

const CONTAINER_LAYOUT_SAFETY_LIMIT: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContainerLayoutTermination {
    NoQueries,
    NoContainers,
    GeometryStable,
    SignatureStable,
    OscillationFallback,
    PassCapFallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ContainerLayoutTelemetry {
    passes: usize,
    termination: ContainerLayoutTermination,
    query: crate::css::ContainerQueryStats,
    retained_reused: usize,
    retained_fresh: usize,
    retained_fallback: usize,
}

fn container_iteration_termination<T: PartialEq>(
    geometry_stable: bool,
    signature: &T,
    previous_signature: Option<&T>,
) -> Option<ContainerLayoutTermination> {
    if geometry_stable {
        Some(ContainerLayoutTermination::GeometryStable)
    } else if previous_signature == Some(signature) {
        Some(ContainerLayoutTermination::SignatureStable)
    } else {
        None
    }
}

enum RetainedStylePlan {
    Reuse {
        dirty: HashSet<NodeId>,
        has_animation_damage: bool,
    },
    Full,
}

fn add_style_subtree(tree: &DomTree, root: NodeId, dirty: &mut HashSet<NodeId>) {
    dirty.insert(root);
    dirty.extend(tree.descendants(root));
    // Rebuild the context chain too. A retained ancestor may carry a final
    // post-layout Px repair (native controls, blockification, table/grid
    // fixups) where a full pass would feed its specified Auto/inherit form to
    // the fresh descendant's normalization. Ancestors are fresh individually;
    // their unrelated sibling subtrees remain reusable.
    dirty.extend(tree.ancestors(root));
}

fn add_container_query_reset_scopes(
    tree: &DomTree,
    id: NodeId,
    sheet: &crate::css::Stylesheet,
    matcher: &mut obscura_dom::selector::Matcher,
    active_containers: &HashSet<NodeId>,
    inside_active_container: bool,
    selected_ancestor: bool,
    dirty: &mut HashSet<NodeId>,
) {
    let is_element = tree.get_node(id).is_some_and(|node| node.is_element());
    let can_query_here = inside_active_container || active_containers.contains(&id);
    let selected_here = !selected_ancestor
        && can_query_here
        && is_element
        && sheet.node_matches_container_query_rule(tree, matcher, id);
    let selected = selected_ancestor || selected_here;
    if selected {
        // The retained style is the previous converged, query-enabled value.
        // The convergence seed pass deliberately disables query rules, so
        // reset every possible subject and inherited descendant to its base
        // cascade before geometry is sampled again.
        dirty.insert(id);
    }
    if selected_here {
        // Stop as soon as an already-recorded context node is reached. An
        // earlier selected sibling necessarily inserted the rest of this same
        // ancestor chain, so each ancestor is visited at most once.
        let mut ancestor = tree.get_node(id).and_then(|node| node.parent);
        while let Some(parent) = ancestor {
            if !dirty.insert(parent) {
                break;
            }
            ancestor = tree.get_node(parent).and_then(|node| node.parent);
        }
    }
    if is_element {
        matcher.push_ancestor(tree, id);
    }
    let children_inside_container = inside_active_container || active_containers.contains(&id);
    for child in tree.children(id) {
        add_container_query_reset_scopes(
            tree,
            child,
            sheet,
            matcher,
            active_containers,
            children_inside_container,
            selected,
            dirty,
        );
    }
    if is_element {
        matcher.pop_ancestor();
    }
}

fn add_following_sibling_subtrees(
    tree: &DomTree,
    node: NodeId,
    dirty: &mut HashSet<NodeId>,
) {
    let mut sibling = tree.get_node(node).and_then(|node| node.next_sibling);
    while let Some(id) = sibling {
        add_style_subtree(tree, id, dirty);
        sibling = tree.get_node(id).and_then(|node| node.next_sibling);
    }
}

fn subtree_contains_style_element(tree: &DomTree, root: NodeId) -> bool {
    std::iter::once(root)
        .chain(tree.descendants(root))
        .any(|id| {
            tree.get_node(id).is_some_and(|node| {
                node.as_element()
                    .is_some_and(|element| element.local.as_ref() == "style")
            })
        })
}

fn node_is_style_text(tree: &DomTree, node: NodeId, parent: Option<NodeId>) -> bool {
    parent.is_some_and(|parent| {
        tree.get_node(parent).is_some_and(|node| {
            node.as_element()
                .is_some_and(|element| element.local.as_ref() == "style")
        })
    }) || tree.get_node(node).is_some_and(|node| {
        node.as_element()
            .is_some_and(|element| element.local.as_ref() == "style")
    })
}

fn subtree_may_match_relational_path(
    tree: &DomTree,
    invalidation: &crate::css::RelationalInvalidation,
    root: NodeId,
) -> bool {
    std::iter::once(root)
        .chain(tree.descendants(root))
        .any(|id| invalidation.relative_path_may_match(tree, id))
}

/// Candidate anchors after an insertion. Relative selectors search from the
/// changed subtree toward ancestors and earlier siblings; only the boundary
/// can connect the already-fresh inserted subtree to retained elements.
fn add_inserted_relational_anchor_candidates(
    tree: &DomTree,
    node: NodeId,
    candidates: &mut HashSet<NodeId>,
) {
    let mut current = Some(node);
    while let Some(id) = current {
        let mut sibling = tree.get_node(id).and_then(|node| node.prev_sibling);
        while let Some(previous) = sibling {
            candidates.insert(previous);
            sibling = tree.get_node(previous).and_then(|node| node.prev_sibling);
        }
        current = tree.get_node(id).and_then(|node| node.parent);
        if let Some(parent) = current {
            candidates.insert(parent);
        }
    }
}

/// Removal has already destroyed the old sibling links. As Gecko does for a
/// removal side effect, inspect every sibling at each old ancestor boundary;
/// this includes the old previous/next neighbors without retaining a DOM
/// snapshot in every mutation record.
fn add_removed_relational_anchor_candidates(
    tree: &DomTree,
    old_parent: NodeId,
    candidates: &mut HashSet<NodeId>,
) {
    let mut current = Some(old_parent);
    while let Some(id) = current {
        candidates.insert(id);
        candidates.extend(tree.children(id));
        let parent = tree.get_node(id).and_then(|node| node.parent);
        if let Some(parent) = parent {
            candidates.extend(tree.children(parent));
        }
        current = parent;
    }
}

fn add_relational_anchor_scope(
    tree: &DomTree,
    anchor: NodeId,
    reaches: crate::css::InvalidationReaches,
    dirty: &mut HashSet<NodeId>,
) {
    // Re-cascading the anchor subtree covers anchor-self changes, inheritance,
    // and every descendant subject. It is intentionally broader than the
    // dependency's exact reach but keeps the retained-style implementation
    // independent of selector matching internals.
    add_style_subtree(tree, anchor, dirty);
    if reaches.contains(crate::css::InvalidationReaches::SIBLINGS) {
        add_following_sibling_subtrees(tree, anchor, dirty);
    }
}

/// Apply Gecko-style upward `:has()` invalidation for one child-list or text
/// mutation. Returns false only when the path outside the anchor combines
/// traversals which the renderer's flat reach bits cannot represent soundly.
fn add_relational_tree_invalidation(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    mutation: &TreeStyleMutation,
    dirty: &mut HashSet<NodeId>,
) -> bool {
    if map.relational_invalidations().is_empty() {
        return true;
    }
    let mut candidates = HashSet::new();
    match *mutation {
        TreeStyleMutation::Insert {
            node,
            old_parent,
            ..
        } => {
            add_inserted_relational_anchor_candidates(tree, node, &mut candidates);
            if let Some(old_parent) = old_parent {
                add_removed_relational_anchor_candidates(tree, old_parent, &mut candidates);
            }
        }
        TreeStyleMutation::Remove { old_parent, .. } => {
            add_removed_relational_anchor_candidates(tree, old_parent, &mut candidates);
        }
        TreeStyleMutation::Text { node, parent } => {
            add_inserted_relational_anchor_candidates(tree, node, &mut candidates);
            if let Some(parent) = parent {
                candidates.insert(parent);
            }
        }
    }

    for invalidation in map.relational_invalidations() {
        let triggered = match mutation {
                TreeStyleMutation::Remove { .. } => true,
                TreeStyleMutation::Insert { node, .. } => {
                    subtree_may_match_relational_path(tree, invalidation, *node)
                        || invalidation.unkeyed_subject
                        || invalidation.sibling_side_effect
                        || invalidation.structural_side_effect
                }
                TreeStyleMutation::Text { .. } => invalidation.text_side_effect,
            };
        if !triggered {
            continue;
        }
        for anchor in candidates.iter().copied() {
            if !invalidation.anchor_may_match(tree, anchor) {
                continue;
            }
            if invalidation.unrepresentable_outer_path {
                if std::env::var_os("OBSCURA_RENDER_TIMING").is_some() {
                    eprintln!(
                        "[timing] retained-style fallback reason=relational-unrepresentable rule_order={} anchor={} mutation={mutation:?}",
                        invalidation.rule_order,
                        anchor.index(),
                    );
                }
                return false;
            }
            add_relational_anchor_scope(
                tree,
                anchor,
                invalidation.anchor_reaches,
                dirty,
            );
        }
    }
    true
}

fn add_style_context_chain(tree: &DomTree, node: NodeId, dirty: &mut HashSet<NodeId>) {
    dirty.insert(node);
    dirty.extend(tree.ancestors(node));
}

fn add_table_row_child_scope(tree: &DomTree, parent: NodeId, dirty: &mut HashSet<NodeId>) {
    let is_table_row = tree.get_node(parent).is_some_and(|node| {
        node.as_element()
            .is_some_and(|element| element.local.as_ref() == "tr")
    });
    if !is_table_row {
        return;
    }
    // The table fallback assigns surplus growth to the trailing auto cell
    // after cascade. A child-list change can move that role even without an
    // authored structural selector, so reset every surviving cell from its
    // specified style before running the fixup again.
    for child in element_children(tree, parent) {
        add_style_subtree(tree, child, dirty);
    }
}

fn element_children(tree: &DomTree, parent: NodeId) -> Vec<NodeId> {
    tree.children(parent)
        .into_iter()
        .filter(|child| tree.get_node(*child).is_some_and(|node| node.is_element()))
        .collect()
}

fn element_local_name(tree: &DomTree, node: NodeId) -> Option<String> {
    tree.get_node(node)
        .and_then(|node| node.as_element().map(|element| element.local.to_string()))
}

/// Re-cascade a node whose structural pseudo state may have changed. Even a
/// self-only selector can change an inherited property, so the node's complete
/// subtree is the smallest renderer-independent safe unit. A structural state
/// used left of a sibling combinator additionally reaches following siblings.
fn add_structural_candidate_scope(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    state: &str,
    candidate: NodeId,
    parent: NodeId,
    dirty: &mut HashSet<NodeId>,
) {
    let invalidations = map
        .structural_invalidations(state)
        .into_iter()
        .filter(|invalidation| {
            !invalidation.inside_relational
                && invalidation.subject_may_match(tree, candidate)
        })
        .collect::<Vec<_>>();
    if invalidations.is_empty() {
        return;
    }
    if invalidations.iter().any(|invalidation| {
        invalidation
            .reaches
            .contains(crate::css::InvalidationReaches::CONSERVATIVE)
    }) {
        add_style_subtree(tree, parent, dirty);
        return;
    }
    add_style_subtree(tree, candidate, dirty);
    if invalidations.iter().any(|invalidation| {
        invalidation
            .reaches
            .contains(crate::css::InvalidationReaches::SIBLINGS)
    }) {
        add_following_sibling_subtrees(tree, candidate, dirty);
    }
}

fn add_inserted_structural_scopes(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    node: NodeId,
    parent: NodeId,
    mutations: &[RetainedStyleMutation],
    dirty: &mut HashSet<NodeId>,
) {
    let siblings = element_children(tree, parent);
    let Some(position) = siblings.iter().position(|candidate| *candidate == node) else {
        return;
    };
    let mut add = |state: &str, candidates: &[NodeId]| {
        for candidate in candidates.iter().copied() {
            add_structural_candidate_scope(tree, map, state, candidate, parent, dirty);
        }
    };

    let insertion_count = mutations
        .iter()
        .filter(|mutation| {
            matches!(
                mutation,
                RetainedStyleMutation::Tree(TreeStyleMutation::Insert {
                    node,
                    new_parent,
                    ..
                }) if *new_parent == parent
                    && tree.get_node(*node).is_some_and(|node| node.is_element())
            )
        })
        .count();
    if insertion_count > 1 {
        // Mutation records do not retain the old sibling boundaries. With two
        // fresh insertions, the old first/last/only child can sit beyond the
        // final two boundary nodes and would otherwise keep a stale match.
        // Direct children are still a bounded scope, and keyed structural
        // metadata avoids cascading unrelated candidates.
        for state in [
            "first-child",
            "last-child",
            "only-child",
            "first-of-type",
            "last-of-type",
            "only-of-type",
        ] {
            add(state, &siblings);
        }
    }

    if position == 0 {
        add("first-child", &siblings[..siblings.len().min(2)]);
    }
    if position + 1 == siblings.len() {
        add(
            "last-child",
            &siblings[position.saturating_sub(1)..],
        );
    }
    if siblings.len() <= 2 {
        add("only-child", &siblings);
    }
    add("nth-child", &siblings[position..]);
    add("nth-last-child", &siblings[..=position]);

    let Some(local) = element_local_name(tree, node) else {
        return;
    };
    let same_type = siblings
        .iter()
        .copied()
        .filter(|candidate| element_local_name(tree, *candidate).as_deref() == Some(&local))
        .collect::<Vec<_>>();
    let Some(type_position) = same_type.iter().position(|candidate| *candidate == node) else {
        return;
    };
    if type_position == 0 {
        add("first-of-type", &same_type[..same_type.len().min(2)]);
    }
    if type_position + 1 == same_type.len() {
        add(
            "last-of-type",
            &same_type[type_position.saturating_sub(1)..],
        );
    }
    if same_type.len() <= 2 {
        add("only-of-type", &same_type);
    }
    add("nth-of-type", &same_type[type_position..]);
    add("nth-last-of-type", &same_type[..=type_position]);
}

fn add_removed_structural_scopes(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    removed: NodeId,
    parent: NodeId,
    dirty: &mut HashSet<NodeId>,
) {
    let siblings = element_children(tree, parent);
    if let Some(first) = siblings.first().copied() {
        add_structural_candidate_scope(tree, map, "first-child", first, parent, dirty);
    }
    if let Some(last) = siblings.last().copied() {
        add_structural_candidate_scope(tree, map, "last-child", last, parent, dirty);
    }
    if siblings.len() <= 1 {
        for candidate in siblings.iter().copied() {
            add_structural_candidate_scope(tree, map, "only-child", candidate, parent, dirty);
        }
    }
    for state in ["nth-child", "nth-last-child"] {
        for candidate in siblings.iter().copied() {
            add_structural_candidate_scope(tree, map, state, candidate, parent, dirty);
        }
    }

    let Some(local) = element_local_name(tree, removed) else {
        return;
    };
    let same_type = siblings
        .iter()
        .copied()
        .filter(|candidate| element_local_name(tree, *candidate).as_deref() == Some(&local))
        .collect::<Vec<_>>();
    if let Some(first) = same_type.first().copied() {
        add_structural_candidate_scope(tree, map, "first-of-type", first, parent, dirty);
    }
    if let Some(last) = same_type.last().copied() {
        add_structural_candidate_scope(tree, map, "last-of-type", last, parent, dirty);
    }
    if same_type.len() <= 1 {
        for candidate in same_type.iter().copied() {
            add_structural_candidate_scope(tree, map, "only-of-type", candidate, parent, dirty);
        }
    }
    for state in ["nth-of-type", "nth-last-of-type"] {
        for candidate in same_type.iter().copied() {
            add_structural_candidate_scope(tree, map, state, candidate, parent, dirty);
        }
    }
}

fn add_inserted_sibling_scopes(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    node: NodeId,
    parent: NodeId,
    dirty: &mut HashSet<NodeId>,
) {
    let siblings = element_children(tree, parent);
    let Some(position) = siblings.iter().position(|candidate| *candidate == node) else {
        return;
    };
    let may_start_sibling_selector = map.node_may_start_sibling_selector(tree, node);
    if map.has_adjacent_sibling_selectors() {
        if position != 0 || may_start_sibling_selector {
            if let Some(next) = siblings.get(position + 1).copied() {
                add_style_subtree(tree, next, dirty);
            }
        }
    }
    if map.has_general_sibling_selectors() && may_start_sibling_selector {
        for following in siblings.iter().skip(position + 1).copied() {
            add_style_subtree(tree, following, dirty);
        }
    }
}

fn add_removed_sibling_scopes(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    parent: NodeId,
    dirty: &mut HashSet<NodeId>,
) {
    if !map.has_adjacent_sibling_selectors() && !map.has_general_sibling_selectors() {
        return;
    }
    // Removal records intentionally do not retain old sibling pointers. All
    // direct element siblings are the bounded sound recovery set; their clean
    // descendant subtrees remain reusable when no sibling selector reaches
    // through them.
    for sibling in element_children(tree, parent) {
        add_style_subtree(tree, sibling, dirty);
    }
}

fn empty_state_may_have_changed(
    tree: &DomTree,
    parent: NodeId,
    mutations: &[RetainedStyleMutation],
) -> bool {
    let relevant_children = tree
        .children(parent)
        .into_iter()
        .filter(|child| {
            tree.get_node(*child).is_some_and(|node| match &node.data {
                obscura_dom::tree::NodeData::Element { .. } => true,
                obscura_dom::tree::NodeData::Text { contents } => !contents.is_empty(),
                _ => false,
            })
        })
        .count();
    let boundary_mutations = mutations
        .iter()
        .filter(|mutation| match mutation {
            RetainedStyleMutation::Attribute(_) => false,
            RetainedStyleMutation::Animation { .. } => false,
            RetainedStyleMutation::WaapiAnimation { .. } => false,
            RetainedStyleMutation::Resource => false,
            RetainedStyleMutation::Tree(TreeStyleMutation::Insert {
                old_parent,
                new_parent,
                ..
            }) => *new_parent == parent || *old_parent == Some(parent),
            RetainedStyleMutation::Tree(TreeStyleMutation::Remove { old_parent, .. }) => {
                *old_parent == parent
            }
            RetainedStyleMutation::Tree(TreeStyleMutation::Text {
                parent: text_parent,
                ..
            }) => *text_parent == Some(parent),
        })
        .count();
    // At least one relevant child untouched by this mutation batch proves the
    // parent was non-empty before and after every queued boundary operation.
    relevant_children <= boundary_mutations
}

fn add_empty_parent_scope(
    tree: &DomTree,
    map: &crate::css::InvalidationMap,
    parent: NodeId,
    mutations: &[RetainedStyleMutation],
    dirty: &mut HashSet<NodeId>,
) {
    if !empty_state_may_have_changed(tree, parent, mutations) {
        return;
    }
    let invalidations = map
        .structural_invalidations("empty")
        .into_iter()
        .filter(|invalidation| {
            !invalidation.inside_relational
                && invalidation.subject_may_match(tree, parent)
        })
        .collect::<Vec<_>>();
    if invalidations.is_empty() {
        return;
    }
    add_style_subtree(tree, parent, dirty);
    if invalidations.iter().any(|invalidation| {
        invalidation
            .reaches
            .contains(crate::css::InvalidationReaches::SIBLINGS)
            || invalidation
                .reaches
                .contains(crate::css::InvalidationReaches::CONSERVATIVE)
    }) {
        add_following_sibling_subtrees(tree, parent, dirty);
    }
}

/// Convert old/new selector keys into a conservative set of fresh cascade
/// roots. Every selected root is expanded to its complete subtree so ordinary
/// CSS inheritance, custom properties, generated content, and later selector
/// compounds remain sound without retaining Gecko's full dependency chains.
fn retained_style_plan(
    tree: &DomTree,
    sheet: &crate::css::Stylesheet,
    mutations: &[RetainedStyleMutation],
) -> RetainedStylePlan {
    let mut dirty = HashSet::new();
    let mut has_animation_damage = false;
    for mutation in mutations {
        if matches!(mutation, RetainedStyleMutation::Resource) {
            continue;
        }
        if let RetainedStyleMutation::Animation { node } = mutation {
            // Animated color and visibility inherit, keyframe endpoints can
            // consume inherited custom properties, and generated pseudos are
            // rebuilt with their originating element. The existing subtree
            // expansion covers all three while retaining sibling branches.
            add_style_subtree(tree, *node, &mut dirty);
            has_animation_damage = true;
            continue;
        }
        if let RetainedStyleMutation::WaapiAnimation { node } = mutation {
            dirty.insert(*node);
            has_animation_damage = true;
            continue;
        }
        let RetainedStyleMutation::Attribute(mutation) = mutation else {
            let RetainedStyleMutation::Tree(mutation) = mutation else {
                unreachable!()
            };
            let map = sheet.invalidation_map();
            match *mutation {
                TreeStyleMutation::Insert {
                    node,
                    old_parent,
                    new_parent,
                } => {
                    // Inserting or moving a style subtree changes the ordered
                    // author stylesheet, so parsing/index reuse is forbidden.
                    if subtree_contains_style_element(tree, node)
                        || node_is_style_text(tree, node, old_parent)
                        || node_is_style_text(tree, node, Some(new_parent))
                    {
                        if std::env::var_os("OBSCURA_RENDER_TIMING").is_some() {
                            eprintln!(
                                "[timing] retained-style fallback reason=style-subtree-insert node={} old_parent={old_parent:?} new_parent={}",
                                node.index(),
                                new_parent.index(),
                            );
                        }
                        return RetainedStylePlan::Full;
                    }
                    if !add_relational_tree_invalidation(tree, map, mutation, &mut dirty) {
                        return RetainedStylePlan::Full;
                    }
                    add_style_subtree(tree, node, &mut dirty);
                    if let Some(parent) = old_parent {
                        add_style_context_chain(tree, parent, &mut dirty);
                        add_table_row_child_scope(tree, parent, &mut dirty);
                        add_removed_structural_scopes(tree, map, node, parent, &mut dirty);
                        add_removed_sibling_scopes(tree, map, parent, &mut dirty);
                        add_empty_parent_scope(tree, map, parent, mutations, &mut dirty);
                    }
                    add_style_context_chain(tree, new_parent, &mut dirty);
                    add_table_row_child_scope(tree, new_parent, &mut dirty);
                    add_inserted_structural_scopes(
                        tree,
                        map,
                        node,
                        new_parent,
                        mutations,
                        &mut dirty,
                    );
                    add_inserted_sibling_scopes(tree, map, node, new_parent, &mut dirty);
                    add_empty_parent_scope(tree, map, new_parent, mutations, &mut dirty);
                }
                TreeStyleMutation::Remove { node, old_parent } => {
                    if subtree_contains_style_element(tree, node)
                        || node_is_style_text(tree, node, Some(old_parent))
                    {
                        if std::env::var_os("OBSCURA_RENDER_TIMING").is_some() {
                            eprintln!(
                                "[timing] retained-style fallback reason=style-subtree-remove node={} old_parent={}",
                                node.index(),
                                old_parent.index(),
                            );
                        }
                        return RetainedStylePlan::Full;
                    }
                    if !add_relational_tree_invalidation(tree, map, mutation, &mut dirty) {
                        return RetainedStylePlan::Full;
                    }
                    add_style_context_chain(tree, old_parent, &mut dirty);
                    add_table_row_child_scope(tree, old_parent, &mut dirty);
                    add_removed_structural_scopes(tree, map, node, old_parent, &mut dirty);
                    add_removed_sibling_scopes(tree, map, old_parent, &mut dirty);
                    add_empty_parent_scope(tree, map, old_parent, mutations, &mut dirty);
                }
                TreeStyleMutation::Text { node, parent } => {
                    if node_is_style_text(tree, node, parent) {
                        if std::env::var_os("OBSCURA_RENDER_TIMING").is_some() {
                            eprintln!(
                                "[timing] retained-style fallback reason=style-text node={} parent={parent:?}",
                                node.index(),
                            );
                        }
                        return RetainedStylePlan::Full;
                    }
                    if !add_relational_tree_invalidation(tree, map, mutation, &mut dirty) {
                        return RetainedStylePlan::Full;
                    }
                    if let Some(parent) = parent {
                        add_style_context_chain(tree, parent, &mut dirty);
                        add_empty_parent_scope(tree, map, parent, mutations, &mut dirty);
                    } else {
                        dirty.insert(node);
                    }
                }
            }
            continue;
        };
        let name = mutation.name.to_ascii_lowercase();
        match retained_attribute_mutation_kind(tree, mutation.node, &name) {
            RetainedAttributeMutationKind::Full => return RetainedStylePlan::Full,
            RetainedAttributeMutationKind::Subtree => {
                // Mapped hints and native-control sizing enter the mutable
                // LayoutStyle graph. Re-cascade from specified values before
                // post-cascade used-value repair runs again.
                add_style_subtree(tree, mutation.node, &mut dirty);
            }
            RetainedAttributeMutationKind::Selector => {}
        }

        let map = sheet.invalidation_map();
        let mut dependencies = Vec::new();
        dependencies.extend_from_slice(map.attribute_dependencies(&name));
        if name == "id" {
            if let Some(old) = mutation.old_value.as_deref() {
                dependencies.extend_from_slice(map.id_dependencies(old));
            }
            if let Some(new) = mutation.new_value.as_deref() {
                dependencies.extend_from_slice(map.id_dependencies(new));
            }
        } else if name == "class" {
            for class in mutation
                .old_value
                .iter()
                .chain(&mutation.new_value)
                .flat_map(|value| value.split_whitespace())
            {
                dependencies.extend_from_slice(map.class_dependencies(class));
            }
        }

        // Several HTML boolean/value attributes also back selector pseudo
        // states. Include both sides of paired states so removing an attribute
        // invalidates rules such as `:enabled` and `:optional`, not only the
        // positive state.
        let states: &[&str] = match name.as_str() {
            "checked" | "selected" => &["checked"],
            "disabled" => &["disabled", "enabled"],
            "dir" => &["dir"],
            "href" => &["any-link", "link", "visited"],
            "lang" | "xml:lang" => &["lang"],
            "open" => &["open"],
            "placeholder" | "value" => &["placeholder-shown"],
            "readonly" => &["read-only", "read-write"],
            "required" => &["required", "optional"],
            _ => &[],
        };
        for state in states {
            dependencies.extend_from_slice(map.state_dependencies(state));
        }

        for dependency in dependencies {
            let reaches = dependency.reaches;
            if reaches.contains(crate::css::InvalidationReaches::CONSERVATIVE) {
                return RetainedStylePlan::Full;
            }
            if reaches.contains(crate::css::InvalidationReaches::SELF)
                || reaches.contains(crate::css::InvalidationReaches::DESCENDANTS)
            {
                add_style_subtree(tree, mutation.node, &mut dirty);
            }
            if reaches.contains(crate::css::InvalidationReaches::SIBLINGS) {
                add_following_sibling_subtrees(tree, mutation.node, &mut dirty);
            }
        }
    }
    RetainedStylePlan::Reuse {
        dirty,
        has_animation_damage,
    }
}

pub(crate) fn layout_dom_with_web_fonts(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
) -> DomLayout {
    layout_dom_with_web_fonts_measured(tree, viewport, intrinsic, fonts).0
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn layout_dom_with_web_fonts_and_stylesheet_cache(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
) -> DomLayout {
    layout_dom_with_web_fonts_and_stylesheet_cache_at_animation_time(
        tree,
        viewport,
        intrinsic,
        fonts,
        stylesheet_cache,
        crate::AnimationSampleTime::default(),
    )
}

pub(crate) fn layout_dom_with_web_fonts_and_stylesheet_cache_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    animation_sample_time: crate::AnimationSampleTime,
) -> DomLayout {
    let mut animation_timeline = crate::AnimationTimelineState::default();
    layout_dom_with_web_fonts_and_stylesheet_cache_with_animation_state(
        tree,
        viewport,
        intrinsic,
        fonts,
        stylesheet_cache,
        crate::AnimationSample {
            time: animation_sample_time,
            mode: crate::AnimationSampleMode::DocumentTime,
        },
        &mut animation_timeline,
    )
}

pub(crate) fn layout_dom_with_web_fonts_and_stylesheet_cache_with_animation_state(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> DomLayout {
    layout_dom_with_web_fonts_and_stylesheet_cache_for_media_with_animation_state(
        tree,
        viewport,
        intrinsic,
        fonts,
        stylesheet_cache,
        crate::CssMediaType::Screen,
        animation_sample,
        animation_timeline,
    )
}

pub(crate) fn layout_dom_with_web_fonts_and_stylesheet_cache_for_media_with_animation_state(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    media_type: crate::CssMediaType,
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> DomLayout {
    layout_dom_with_web_fonts_pass_limit_at_animation_time(
        tree,
        viewport,
        intrinsic,
        fonts,
        None,
        Some(stylesheet_cache),
        None,
        &[],
        media_type,
        animation_sample,
        animation_timeline,
    )
    .0
}

#[allow(dead_code)]
pub(crate) fn layout_dom_with_web_fonts_and_retained_styles(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    retained: RetainedStyleMaps,
    mutations: &[RetainedStyleMutation],
) -> DomLayout {
    layout_dom_with_web_fonts_and_retained_styles_at_animation_time(
        tree,
        viewport,
        intrinsic,
        fonts,
        stylesheet_cache,
        retained,
        mutations,
        crate::AnimationSampleTime::default(),
    )
}

pub(crate) fn layout_dom_with_web_fonts_and_retained_styles_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    retained: RetainedStyleMaps,
    mutations: &[RetainedStyleMutation],
    animation_sample_time: crate::AnimationSampleTime,
) -> DomLayout {
    let mut animation_timeline = crate::AnimationTimelineState::default();
    layout_dom_with_web_fonts_and_retained_styles_with_animation_state(
        tree,
        viewport,
        intrinsic,
        fonts,
        stylesheet_cache,
        retained,
        mutations,
        crate::AnimationSample {
            time: animation_sample_time,
            mode: crate::AnimationSampleMode::DocumentTime,
        },
        &mut animation_timeline,
    )
}

pub(crate) fn layout_dom_with_web_fonts_and_retained_styles_with_animation_state(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    stylesheet_cache: &mut crate::css::StylesheetCache,
    retained: RetainedStyleMaps,
    mutations: &[RetainedStyleMutation],
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> DomLayout {
    layout_dom_with_web_fonts_pass_limit_at_animation_time(
        tree,
        viewport,
        intrinsic,
        fonts,
        None,
        Some(stylesheet_cache),
        Some(retained),
        mutations,
        crate::CssMediaType::Screen,
        animation_sample,
        animation_timeline,
    )
    .0
}

fn layout_dom_with_web_fonts_measured(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
) -> (DomLayout, ContainerLayoutTelemetry) {
    layout_dom_with_web_fonts_pass_limit(tree, viewport, intrinsic, fonts, None, None, None, &[])
}

fn layout_dom_with_web_fonts_pass_limit(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    pass_limit: Option<usize>,
    stylesheet_cache: Option<&mut crate::css::StylesheetCache>,
    retained: Option<RetainedStyleMaps>,
    mutations: &[RetainedStyleMutation],
) -> (DomLayout, ContainerLayoutTelemetry) {
    let mut animation_timeline = crate::AnimationTimelineState::default();
    layout_dom_with_web_fonts_pass_limit_at_animation_time(
        tree,
        viewport,
        intrinsic,
        fonts,
        pass_limit,
        stylesheet_cache,
        retained,
        mutations,
        crate::CssMediaType::Screen,
        crate::AnimationSample::default(),
        &mut animation_timeline,
    )
}

/// Compile one author stylesheet per native ShadowRoot.
///
/// `DomTree::descendants` deliberately stays inside one tree scope, so the
/// document collector cannot accidentally absorb a component's styles and a
/// shadow collector cannot absorb a nested component's styles. Walking host
/// edges explicitly here also discovers roots nested inside other roots.
fn collect_shadow_stylesheets(
    tree: &DomTree,
    viewport: (f32, f32),
    media_type: crate::CssMediaType,
) -> HashMap<NodeId, std::sync::Arc<crate::css::Stylesheet>> {
    let mut roots = Vec::new();
    let mut stack = vec![tree.document()];
    let mut visited = HashSet::new();
    while let Some(node) = stack.pop() {
        if !visited.insert(node) {
            continue;
        }
        if let Some(root) = tree.shadow_root(node) {
            roots.push(root);
            stack.extend(tree.children(root).into_iter().rev());
        }
        stack.extend(tree.children(node).into_iter().rev());
    }

    roots
        .into_iter()
        .map(|root| {
            let sources = tree
                .descendants(root)
                .into_iter()
                .filter_map(|node_id| {
                    let node = tree.get_node(node_id)?;
                    let element = node.as_element()?;
                    (element.local.as_ref() == "style"
                        && node.get_attribute("media").is_none_or(|media| {
                            media.trim().is_empty()
                                || crate::css::media_query_applies_for_viewport_and_type(
                                    media,
                                    viewport,
                                    media_type,
                                )
                        }))
                    .then(|| tree.text_content(node_id))
                })
                .collect::<Vec<_>>();
            let sheet = crate::css::Stylesheet::parse_for_viewport_and_media(
                tree,
                &sources,
                viewport,
                media_type,
            );
            (root, std::sync::Arc::new(sheet))
        })
        .collect()
}

fn layout_dom_with_web_fonts_pass_limit_at_animation_time(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    pass_limit: Option<usize>,
    stylesheet_cache: Option<&mut crate::css::StylesheetCache>,
    retained: Option<RetainedStyleMaps>,
    mutations: &[RetainedStyleMutation],
    media_type: crate::CssMediaType,
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> (DomLayout, ContainerLayoutTelemetry) {
    let timing = std::env::var("OBSCURA_RENDER_TIMING").is_ok();

    // Collect the text of every <style> block in document order.
    let mut css_sources = Vec::new();
    for nid in tree.descendants(tree.document()) {
        if let Some(node) = tree.get_node(nid) {
            if let Some(elem) = node.as_element() {
                if elem.local.as_ref() == "style"
                    && node.get_attribute("media").is_none_or(|media| {
                        media.trim().is_empty()
                            || crate::css::media_query_applies_for_viewport_and_type(
                                media,
                                viewport,
                                media_type,
                            )
                    })
                {
                    css_sources.push(tree.text_content(nid));
                }
            }
        }
    }

    let t0 = std::time::Instant::now();
    let (sheet, stylesheet_cache_hit) = match stylesheet_cache {
        Some(cache) => cache.get_or_parse(tree, &css_sources, viewport, media_type),
        None => (
            std::sync::Arc::new(crate::css::Stylesheet::parse_for_viewport_and_media(
                tree,
                &css_sources,
                viewport,
                media_type,
            )),
            false,
        ),
    };
    let shadow_sheets = collect_shadow_stylesheets(tree, viewport, media_type);
    let t_parse = t0.elapsed();

    let retained_requested = retained.as_ref().map_or(0, |retained| retained.styles.len());
    let retained = retained.and_then(|mut retained| {
        // The document cache key intentionally contains only document-scope
        // sources. Until shadow sheets have their own retained cache keys and
        // invalidation maps, reusing computed styles after DOM/style damage in
        // a document with a native root could preserve stale shadow rules or
        // inherited host custom properties. Resource-only damage cannot
        // change either cascade, so its zero-dirty reuse remains sound.
        let resource_only = !mutations.is_empty()
            && mutations
                .iter()
                .all(|mutation| matches!(mutation, RetainedStyleMutation::Resource));
        if !shadow_sheets.is_empty() && !resource_only {
            return None;
        }
        if !stylesheet_cache_hit {
            return None;
        }
        let active_containers = retained
            .styles
            .iter()
            .filter_map(|(node, style)| {
                (style.container_type != crate::ContainerType::Normal).then_some(*node)
            })
            .collect::<HashSet<_>>();
        let connected = std::iter::once(tree.document())
            .chain(rendered_descendants(tree, tree.document()))
            .collect::<HashSet<_>>();
        retained.styles.retain(|node, _| connected.contains(node));
        retained
            .custom_properties
            .retain(|node, _| connected.contains(node));
        match retained_style_plan(tree, &sheet, mutations) {
            RetainedStylePlan::Reuse {
                mut dirty,
                has_animation_damage,
            } => {
                if !active_containers.is_empty() && sheet.has_container_queries() {
                    let mut matcher = tree.matcher();
                    add_container_query_reset_scopes(
                        tree,
                        tree.document(),
                        &sheet,
                        &mut matcher,
                        &active_containers,
                        false,
                        false,
                        &mut dirty,
                    );
                }
                // Once animation damage reaches at least half of the retained
                // style graph, sparse HashMap reuse no longer offsets dirty-set
                // bookkeeping and branch checks. This is a document-relative
                // coverage threshold, not a site- or node-count heuristic.
                if has_animation_damage
                    && dirty.len().saturating_mul(2) >= retained.styles.len()
                {
                    if std::env::var_os("OBSCURA_RENDER_TIMING").is_some() {
                        eprintln!(
                            "[timing] retained-style fallback reason=animation-dirty-coverage dirty={} retained={}",
                            dirty.len(),
                            retained.styles.len(),
                        );
                    }
                    return None;
                }
                Some((retained, dirty))
            }
            RetainedStylePlan::Full => None,
        }
    });
    let retained_fresh = retained.as_ref().map_or(0, |(retained, fresh)| {
        retained
            .styles
            .keys()
            .filter(|node| fresh.contains(node))
            .count()
    });
    let retained_reused = retained
        .as_ref()
        .map_or(0, |(retained, _)| retained.styles.len() - retained_fresh);
    let retained_fallback = usize::from(retained_requested != 0 && retained.is_none());
    let (mut laid, _, mut query, mut cascade_time) =
        layout_dom_once(
            tree,
            viewport,
            intrinsic,
            fonts,
            &sheet,
            &shadow_sheets,
            None,
            retained,
            animation_sample,
            animation_timeline,
        );
    if !sheet.has_container_queries() {
        if timing {
            let (r, i, c, a, l, u) = sheet.debug_stats();
            eprintln!("[timing] parse+index={:?} stylesheet_cache_hit={} cascade={:?} rules={} id_keys={} class_keys={} attr_keys={} local_keys={} universal={} cq_passes=1 cq_termination=no-queries retained_reused={} retained_fresh={} retained_fallback={}", t_parse, stylesheet_cache_hit, cascade_time, r, i, c, a, l, u, retained_reused, retained_fresh, retained_fallback);
        }
        return (
            laid,
            ContainerLayoutTelemetry {
                passes: 1,
                termination: ContainerLayoutTermination::NoQueries,
                query,
                retained_reused,
                retained_fresh,
                retained_fallback,
            },
        );
    }

    let mut snapshot = container_snapshot(tree, &laid);
    // A container condition has no matching query container when the initial
    // cascade produced neither container-type nor container-name. Re-running
    // the entire cascade cannot create the first container because conditional
    // rules are inactive until a container already exists. Large framework
    // stylesheets commonly ship dormant @container blocks; keeping them on the
    // one-pass path avoids a redundant whole-document layout.
    if snapshot.boxes.is_empty() {
        if timing {
            let (r, i, c, a, l, u) = sheet.debug_stats();
            eprintln!("[timing] parse+index={:?} stylesheet_cache_hit={} cascade={:?} rules={} id_keys={} class_keys={} attr_keys={} local_keys={} universal={} cq_passes=1 cq_termination=no-containers retained_reused={} retained_fresh={} retained_fallback={}", t_parse, stylesheet_cache_hit, cascade_time, r, i, c, a, l, u, retained_reused, retained_fresh, retained_fallback);
        }
        return (
            laid,
            ContainerLayoutTelemetry {
                passes: 1,
                termination: ContainerLayoutTermination::NoContainers,
                query,
                retained_reused,
                retained_fresh,
                retained_fallback,
            },
        );
    }
    let mut previous_candidate: Option<(DomLayout, crate::css::ContainerDecisionSignature)> = None;
    let mut seen_signatures = Vec::new();
    let mut passes = 1;
    let mut termination = ContainerLayoutTermination::PassCapFallback;
    // Gecko permits at most one CQ-triggered update per container element in
    // one flush and processes ancestors before descendants. Our whole-tree
    // passes need the same order of growth: a chain can legitimately reveal
    // one deeper query container per pass. Scale the useful bound with DOM
    // ancestry and nested conditional depth, retaining a high safety limit
    // against adversarial non-convergence. Hitting it is visible telemetry and
    // uses the conservative fallback below, never a silently stale layout.
    // `descendants` is preorder, so every parent depth is available before
    // its children. Keep this O(nodes): walking every ancestor separately
    // makes a deeply nested document quadratic before layout even starts.
    let mut element_depths = HashMap::new();
    element_depths.insert(tree.document(), 0usize);
    let mut max_dom_depth = 1usize;
    for id in rendered_descendants(tree, tree.document()) {
        let Some(node) = tree.get_node(id) else {
            continue;
        };
        let parent_depth = rendered_parent(tree, id)
            .and_then(|parent| element_depths.get(&parent).copied())
            .unwrap_or(0);
        let depth = parent_depth + usize::from(node.is_element());
        element_depths.insert(id, depth);
        max_dom_depth = max_dom_depth.max(depth);
    }
    let max_passes = pass_limit.unwrap_or_else(|| {
        (max_dom_depth + sheet.container_condition_depth() + 2)
            .clamp(4, CONTAINER_LAYOUT_SAFETY_LIMIT)
    });
    let mut needs_fallback = false;
    for pass in 2..=max_passes {
        let (next, signature, pass_query, pass_cascade) =
            layout_dom_once(
                tree,
                viewport,
                intrinsic,
                fonts,
                &sheet,
                &shadow_sheets,
                Some(&snapshot),
                None,
                animation_sample,
                animation_timeline,
            );
        passes = pass;
        query.evaluations += pass_query.evaluations;
        query.cache_hits += pass_query.cache_hits;
        query.ancestor_steps += pass_query.ancestor_steps;
        cascade_time += pass_cascade;
        let next_snapshot = container_snapshot(tree, &next);
        let signature = signature.expect("container pass must produce a signature");
        if let Some(reason) = container_iteration_termination(
            next_snapshot == snapshot,
            &signature,
            previous_candidate.as_ref().map(|(_, signature)| signature),
        ) {
            termination = reason;
            // Equal adjacent signatures prove that the *previous* candidate's
            // applied decisions match an evaluation of its own final
            // snapshot. Returning `next` here would be off by one and could
            // expose styles evaluated against geometry it no longer has.
            laid = if reason == ContainerLayoutTermination::SignatureStable {
                previous_candidate
                    .take()
                    .expect("stable signature requires a previous candidate")
                    .0
            } else {
                next
            };
            break;
        }
        if seen_signatures.contains(&signature) {
            termination = ContainerLayoutTermination::OscillationFallback;
            needs_fallback = true;
            break;
        }
        seen_signatures.push(signature.clone());
        previous_candidate = Some((next, signature));
        snapshot = next_snapshot;
        if pass == max_passes {
            needs_fallback = true;
        }
    }

    if needs_fallback {
        // Author-controlled CSS must never crash rendering, but neither may
        // we return a layout whose conditional declarations contradict the
        // geometry used to choose them. Disable the unstable conditional
        // rules for this render and expose the downgrade in telemetry.
        let (fallback, _, fallback_query, fallback_cascade) =
            layout_dom_once(
                tree,
                viewport,
                intrinsic,
                fonts,
                &sheet,
                &shadow_sheets,
                None,
                None,
                animation_sample,
                animation_timeline,
            );
        laid = fallback;
        passes += 1;
        query.evaluations += fallback_query.evaluations;
        query.cache_hits += fallback_query.cache_hits;
        query.ancestor_steps += fallback_query.ancestor_steps;
        cascade_time += fallback_cascade;
    }

    if timing {
        let (r, i, c, a, l, u) = sheet.debug_stats();
        eprintln!("[timing] parse+index={:?} stylesheet_cache_hit={} cascade_total={:?} rules={} id_keys={} class_keys={} attr_keys={} local_keys={} universal={} cq_passes={} cq_termination={:?} cq_evaluations={} cq_cache_hits={} cq_ancestor_steps={} retained_reused={} retained_fresh={} retained_fallback={}", t_parse, stylesheet_cache_hit, cascade_time, r, i, c, a, l, u, passes, termination, query.evaluations, query.cache_hits, query.ancestor_steps, retained_reused, retained_fresh, retained_fallback);
    }
    (
        laid,
        ContainerLayoutTelemetry {
            passes,
            termination,
            query,
            retained_reused,
            retained_fresh,
            retained_fallback,
        },
    )
}

fn layout_dom_once(
    tree: &DomTree,
    viewport: (f32, f32),
    intrinsic: &ReplacedIntrinsicMap,
    fonts: &[crate::inline::WebFont],
    sheet: &crate::css::Stylesheet,
    shadow_sheets: &HashMap<NodeId, std::sync::Arc<crate::css::Stylesheet>>,
    snapshot: Option<&crate::css::ContainerSnapshot>,
    retained: Option<(RetainedStyleMaps, HashSet<NodeId>)>,
    animation_sample: crate::AnimationSample,
    animation_timeline: &mut crate::AnimationTimelineState,
) -> (
    DomLayout,
    Option<crate::css::ContainerDecisionSignature>,
    crate::css::ContainerQueryStats,
    std::time::Duration,
) {
    let t1 = std::time::Instant::now();
    let mut matcher = tree.matcher();
    let (mut styles, mut custom_properties, fresh_styles) = match retained {
        Some((retained, fresh)) => (retained.styles, retained.custom_properties, Some(fresh)),
        None => (HashMap::new(), HashMap::new(), None),
    };
    let root_props = std::rc::Rc::new(HashMap::new());
    let mut evaluator =
        snapshot.map(|snapshot| crate::css::ContainerQueryEvaluator::new(tree, snapshot));
    let mut evaluator_ref = evaluator.as_mut();
    // A real preorder walk (not a flat descendants() scan) so the matcher's
    // ancestor bloom filter tracks the current path: push before recursing
    // into children, pop on the way back out. This is what lets descendant
    // combinators (".mw-body .firstHeading") fast-reject via the filter
    // instead of falling back to the always-true "can't reject" case.
    let quirks_mode = !tree.descendants(tree.document()).into_iter().any(|id| {
        tree.get_node(id).map_or(false, |node| {
            matches!(node.data, obscura_dom::tree::NodeData::Doctype { .. })
        })
    });
    cascade_walk(
        tree,
        tree.document(),
        &sheet,
        &sheet,
        shadow_sheets,
        &mut matcher,
        &mut styles,
        &mut custom_properties,
        &root_props,
        &mut evaluator_ref,
        quirks_mode,
        viewport,
        animation_sample,
        animation_timeline,
        None,
        false,
        fresh_styles.as_ref(),
    );
    resolve_css_counters(tree, &mut styles);
    let cascade_time = t1.elapsed();
    let (signature, query_stats) = evaluator.map_or_else(
        || (None, crate::css::ContainerQueryStats::default()),
        |evaluator| {
            let (signature, stats) = evaluator.finish();
            (Some(signature), stats)
        },
    );
    grow_trailing_auto_cells(tree, &mut styles);

    let descendants = tree.descendants(tree.document());
    let needs_emoji_font = descendants.iter().any(|id| {
        tree.get_node(*id).is_some_and(|node| match &node.data {
            obscura_dom::tree::NodeData::Text { contents } => {
                crate::inline::text_may_need_emoji_font(contents)
            }
            _ => false,
        })
    }) || styles.values().any(|style| {
        style
            .before_content
            .as_deref()
            .is_some_and(crate::inline::text_may_need_emoji_font)
            || style
                .after_content
                .as_deref()
                .is_some_and(crate::inline::text_may_need_emoji_font)
    });

    // The leaf context is the index of a cosmic-text inline formatting
    // context in `engine`; leaves without text carry no context.
    let mut taffy_tree: TaffyTree<usize> = crate::new_taffy_tree();
    let mut id_map: HashMap<taffy::NodeId, NodeId> = HashMap::new();
    let mut words: HashMap<taffy::NodeId, (NodeId, String)> = HashMap::new();
    let mut engine =
        crate::inline::TextEngine::new_with_web_fonts_and_emoji(fonts, needs_emoji_font);
    let mut ifc_items = IfcRegistry::default();

    // The document node itself is not an element; lay out from the first
    // element descendant (the <html> root).
    let root = descendants
        .into_iter()
        .find(|id| tree.get_node(*id).map(|n| n.is_element()).unwrap_or(false));

    let mut rects = HashMap::new();
    let mut inline_fragments = HashMap::new();
    let mut text_runs = HashMap::new();
    // Final absolute rects of anonymous inline-run leaves, keyed by the
    // engine item index (they have no DOM id to key `rects` by).
    let mut anon_rects: HashMap<usize, Rect> = HashMap::new();
    let mut generated_rects: Vec<Option<Rect>> = Vec::new();
    if let Some(root_id) = root {
        // Headless Chromium's classic scrollbar gutter is 15 CSS pixels.
        // `scrollbar-gutter:stable` reserves it even when the scrollbar track
        // is hidden, and Gecko subtracts the platform gutter from the viewport
        // containing block. Media queries and vw still see the full visual
        // viewport; percentages and auto widths use this reduced ICB.
        const CLASSIC_SCROLLBAR_GUTTER: f32 = 15.0;
        let root_gutters = styles
            .get(&root_id)
            .map_or(0, |style| style.scrollbar_gutters.min(2));
        let initial_cb_width =
            (viewport.0 - CLASSIC_SCROLLBAR_GUTTER * f32::from(root_gutters)).max(0.0);
        let initial_cb_x = if root_gutters == 2 {
            CLASSIC_SCROLLBAR_GUTTER
        } else {
            0.0
        };
        // Top-down inheritance of the properties CSS inherits by default.
        #[derive(Clone)]
        struct Inherited {
            display: crate::Display,
            direction: taffy::Direction,
            display_contents: bool,
            is_inline_block: bool,
            flow_root: bool,
            is_table_box: bool,
            is_table_cell_box: bool,
            color: Option<[u8; 4]>,
            font_size: Option<f32>,
            font_weight: u16,
            font_family: Option<String>,
            font_optical_sizing: crate::FontOpticalSizing,
            font_variation_settings: Vec<crate::FontVariationSetting>,
            letter_spacing: f32,
            letter_spacing_non_normal: bool,
            container_type: crate::ContainerType,
            container_names: Vec<String>,
            text_align: Option<taffy::AlignItems>,
            text_indent: crate::Dimension,
            legacy_center: bool,
            visibility_hidden: bool,
            has_zero_opacity: bool,
            list_style: crate::ListStyle,
            line_height: crate::LineHeight,
            white_space: crate::WhiteSpace,
            overflow_wrap: crate::OverflowWrap,
            word_break: crate::WordBreak,
            text_wrap_style: crate::TextWrapStyle,
            text_transform: crate::TextTransform,
            italic: bool,
            box_sizing: crate::BoxSizing,
            border_collapse: bool,
            table_vertical_align: Option<crate::VerticalAlign>,
            overflow_x: u8,
            overflow_y: u8,
            /// Containing-block width in px for the current element, carried
            /// down so percentage padding/margin (which resolve against the
            /// containing block WIDTH, all sides) can be turned into px before
            /// taffy layout. Not a CSS-inherited property; it is recomputed to
            /// the element's own content width for its children.
            cb_width: f32,
            /// Whether the containing block has a definite height. Percentage
            /// heights in ordinary flow compute to auto when this is false;
            /// resolving them against a synthetic zero height collapses
            /// content-heavy modern UI wrappers such as code editors.
            cb_height_definite: bool,
        }
        impl Default for Inherited {
            fn default() -> Self {
                Inherited {
                    // CSS Display's initial outer/inner value is inline flow.
                    // The root is blockified after computed values settle.
                    display: crate::Display::Inline,
                    direction: taffy::Direction::Ltr,
                    display_contents: false,
                    is_inline_block: false,
                    flow_root: false,
                    is_table_box: false,
                    is_table_cell_box: false,
                    color: None,
                    font_size: None,
                    font_weight: 400,
                    font_family: None,
                    font_optical_sizing: crate::FontOpticalSizing::Auto,
                    font_variation_settings: Vec::new(),
                    letter_spacing: 0.0,
                    letter_spacing_non_normal: false,
                    container_type: crate::ContainerType::Normal,
                    container_names: Vec::new(),
                    text_align: None,
                    text_indent: crate::Dimension::Px(0.0),
                    legacy_center: false,
                    visibility_hidden: false,
                    has_zero_opacity: false,
                    // CSS initial value of list-style-type.
                    list_style: crate::ListStyle::Disc,
                    line_height: crate::LineHeight::Normal,
                    white_space: crate::WhiteSpace::Normal,
                    overflow_wrap: crate::OverflowWrap::Normal,
                    word_break: crate::WordBreak::Normal,
                    text_wrap_style: crate::TextWrapStyle::Auto,
                    text_transform: crate::TextTransform::None,
                    italic: false,
                    box_sizing: crate::BoxSizing::ContentBox,
                    border_collapse: false,
                    table_vertical_align: None,
                    overflow_x: 0,
                    overflow_y: 0,
                    cb_width: 0.0,
                    cb_height_definite: false,
                }
            }
        }
        // Viewport-per-unit for vw/vh, and the root font-size for rem. em/rem/
        // vw/vh lengths were parsed unresolved (the element font-size and
        // viewport are not known at parse time); resolve them here, top-down,
        // where the computed font-size is available.
        let vw = viewport.0 / 100.0;
        let vh = viewport.1 / 100.0;
        let root_fs = {
            let s = styles.get(&root_id);
            match (
                s.and_then(|s| s.font_size),
                s.and_then(|s| s.font_size_raw),
                s.and_then(|s| s.font_size_expression.as_deref()),
            ) {
                (Some(px), _, _) => px,
                (None, _, Some(expression)) => {
                    crate::style::resolve_contextual_length(expression, 16.0, 16.0, vw, vh, 16.0)
                        .unwrap_or(16.0)
                }
                (None, Some(d), _) => match d.resolve(16.0, 16.0, vw, vh) {
                    crate::Dimension::Px(p) => p,
                    crate::Dimension::Percent(p) => 16.0 * p,
                    _ => 16.0,
                },
                _ => 16.0,
            }
        };

        // The root element's containing block is the initial containing block,
        // i.e. the viewport width.
        let mut root_inh = Inherited::default();
        root_inh.cb_width = initial_cb_width;
        root_inh.cb_height_definite = true;
        // This set records computed definiteness after walking the real
        // containing-block chain. Merely retaining `height:Percent` is not
        // sufficient: under an auto-height containing block it computes to
        // auto and must never become a post-layout percentage basis.
        let mut definite_height_nodes = HashSet::new();
        let mut queue = vec![(root_id, root_inh)];
        while let Some((id, mut inh)) = queue.pop() {
            // Default the child containing-block width to this element's own
            // (updated to its content width inside the block below).
            let mut child_cb_width = inh.cb_width;
            let mut child_cb_height_definite = false;
            let reused_computed_style = fresh_styles
                .as_ref()
                .is_some_and(|fresh| !fresh.contains(&id));
            if reused_computed_style {
                // Retained styles have already passed through this destructive
                // normalization once. Do not resolve their em/rem/percent or
                // CSS-wide inheritance markers a second time. Seed the
                // inherited context for a fresh descendant directly from the
                // prior computed values, then retain the style byte-for-byte.
                if let Some(style) = styles.get(&id) {
                    crate::style::set_grid_calc_context(
                        style,
                        style.font_size.or(inh.font_size).unwrap_or(16.0),
                        root_fs,
                        vw,
                        vh,
                    );
                    inh.display = style.display;
                    inh.direction = style.direction.unwrap_or(inh.direction);
                    inh.display_contents = style.display_contents;
                    inh.is_inline_block = style.is_inline_block;
                    inh.flow_root = style.flow_root;
                    inh.is_table_box = style.is_table_box;
                    inh.is_table_cell_box = style.is_table_cell_box;
                    inh.color = style.color.or(inh.color);
                    inh.font_size = style.font_size.or(inh.font_size);
                    inh.font_weight = crate::style::used_font_weight(style);
                    if let Some(family) = style.font_family.as_ref() {
                        inh.font_family = Some(family.clone());
                    }
                    if let Some(value) = style.font_optical_sizing {
                        inh.font_optical_sizing = value;
                    }
                    if let Some(settings) = style.font_variation_settings.as_ref() {
                        inh.font_variation_settings.clone_from(settings);
                    }
                    if let Some(spacing) = style.letter_spacing {
                        inh.letter_spacing = spacing;
                    }
                    if let Some(non_normal) = style.letter_spacing_non_normal {
                        inh.letter_spacing_non_normal = non_normal;
                    }
                    inh.container_type = style.container_type;
                    inh.container_names.clone_from(&style.container_names);
                    if let Some(align) = style.text_align {
                        inh.text_align = Some(align);
                        inh.legacy_center = style.legacy_center;
                    }
                    if let Some(indent) = style.text_indent {
                        inh.text_indent = indent;
                    }
                    inh.visibility_hidden = style
                        .visibility_hidden
                        .unwrap_or(inh.visibility_hidden);
                    inh.has_zero_opacity |= style.opacity.is_some_and(|value| value <= 0.0);
                    if let Some(value) = style.list_style {
                        inh.list_style = value;
                    }
                    if let Some(value) = style.line_height {
                        inh.line_height = value;
                    }
                    if let Some(value) = style.white_space {
                        inh.white_space = value;
                    }
                    if let Some(value) = style.overflow_wrap {
                        inh.overflow_wrap = value;
                    }
                    if let Some(value) = style.word_break {
                        inh.word_break = value;
                    }
                    if let Some(value) = style.text_wrap_style {
                        inh.text_wrap_style = value;
                    }
                    if let Some(value) = style.text_transform {
                        inh.text_transform = value;
                    }
                    if let Some(value) = style.font_style_italic {
                        inh.italic = value;
                    }
                    inh.box_sizing = style.box_sizing;
                    if let Some(value) = style.border_collapse {
                        inh.border_collapse = value;
                    }
                    let is_table_part = tree.get_node(id).is_some_and(|node| {
                        node.as_element().is_some_and(|element| {
                            matches!(
                                element.local.as_ref(),
                                "tbody" | "thead" | "tfoot" | "tr" | "td" | "th"
                            )
                        })
                    });
                    if is_table_part {
                        if let Some(value) = style.vertical_align {
                            inh.table_vertical_align = Some(value);
                        }
                    }
                    inh.overflow_x = if style.overflow_scroll_x {
                        2
                    } else if style.overflow_clip_x {
                        1
                    } else {
                        0
                    };
                    inh.overflow_y = if style.overflow_scroll_y {
                        2
                    } else if style.overflow_clip_y {
                        1
                    } else {
                        0
                    };
                    child_cb_height_definite = matches!(
                        style.height,
                        crate::Dimension::Px(_) | crate::Dimension::Percent(_)
                    );
                    if child_cb_height_definite {
                        definite_height_nodes.insert(id);
                    }
                    let cb_w = inh.cb_width;
                    let used_w = match style.width {
                        crate::Dimension::Px(width) => width,
                        crate::Dimension::Percent(percent) => percent * cb_w,
                        _ => (cb_w - style.margin.left - style.margin.right).max(0.0),
                    };
                    let definite_content_box = matches!(
                        style.width,
                        crate::Dimension::Px(_) | crate::Dimension::Percent(_)
                    ) && style.box_sizing == crate::BoxSizing::ContentBox;
                    child_cb_width = if definite_content_box {
                        used_w.max(0.0)
                    } else {
                        (used_w
                            - style.padding.left
                            - style.padding.right
                            - style.border.left
                            - style.border.right)
                            .max(0.0)
                    };
                }
                inh.cb_width = child_cb_width;
                inh.cb_height_definite = child_cb_height_definite;
                for child in style_children(tree, id).into_iter().rev() {
                    queue.push((child, inh.clone()));
                }
                continue;
            }
            let inherited_grid_auto_tracks = styles.get(&id).and_then(|style| {
                (style.grid_auto_columns_inherit || style.grid_auto_rows_inherit).then(|| {
                    rendered_parent(tree, id)
                        .and_then(|parent| styles.get(&parent))
                        .map(|parent| {
                            (
                                parent.grid_auto_columns.clone(),
                                parent.grid_auto_rows.clone(),
                                parent.grid_calc_expressions[2].clone(),
                                parent.grid_calc_expressions[3].clone(),
                            )
                        })
                        .unwrap_or_default()
                })
            });
            if let Some(style) = styles.get_mut(&id) {
                if style.grid_auto_columns_inherit {
                    style.grid_auto_columns = inherited_grid_auto_tracks
                        .as_ref()
                        .map(|tracks| tracks.0.clone())
                        .unwrap_or_default();
                    style.grid_calc_expressions[2] = inherited_grid_auto_tracks
                        .as_ref()
                        .map(|tracks| tracks.2.clone())
                        .unwrap_or_default();
                    style.grid_auto_columns_inherit = false;
                }
                if style.grid_auto_rows_inherit {
                    style.grid_auto_rows = inherited_grid_auto_tracks
                        .as_ref()
                        .map(|tracks| tracks.1.clone())
                        .unwrap_or_default();
                    style.grid_calc_expressions[3] = inherited_grid_auto_tracks
                        .as_ref()
                        .map(|tracks| tracks.3.clone())
                        .unwrap_or_default();
                    style.grid_auto_rows_inherit = false;
                }
                if style.display_inherit {
                    style.display = inh.display;
                    style.display_contents = inh.display_contents;
                    style.is_inline_block = inh.is_inline_block;
                    style.flow_root = inh.flow_root;
                    style.is_table_box = inh.is_table_box;
                    style.is_table_cell_box = inh.is_table_cell_box;
                    // Reconstruct the internal cell-content wrapper only when
                    // the inherited computed display is table-cell.
                    style.internal_flex_container = style.is_table_cell_box;
                    if style.is_table_cell_box {
                        style.flex_direction = Some(taffy::FlexDirection::Column);
                        style.align_items = Some(taffy::AlignItems::FLEX_START);
                    }
                    style.display_inherit = false;
                }
                inh.display = style.display;
                match style.direction {
                    Some(direction) => inh.direction = direction,
                    None => style.direction = Some(inh.direction),
                }
                crate::style::resolve_logical_borders(style);
                inh.display_contents = style.display_contents;
                inh.is_inline_block = style.is_inline_block;
                inh.flow_root = style.flow_root;
                inh.is_table_box = style.is_table_box;
                inh.is_table_cell_box = style.is_table_cell_box;
                match style.color {
                    Some(c) => inh.color = Some(c),
                    None => style.color = inh.color,
                }
                // Resolve a relative font-size against the PARENT (em/%) or
                // ROOT (rem) font-size before inheriting it downward.
                let parent_fs = inh.font_size.unwrap_or(16.0);
                if let Some(expression) = style.font_size_expression.as_deref() {
                    style.font_size = crate::style::resolve_contextual_length(
                        expression, parent_fs, root_fs, vw, vh, parent_fs,
                    );
                } else if let Some(raw) = style.font_size_raw {
                    let resolved = match raw {
                        crate::Dimension::Percent(p) => parent_fs * p,
                        crate::Dimension::Em(v) => parent_fs * v,
                        d => match d.resolve(parent_fs, root_fs, vw, vh) {
                            crate::Dimension::Px(p) => p,
                            _ => parent_fs,
                        },
                    };
                    style.font_size = Some(resolved);
                }
                match style.font_size {
                    Some(s) => inh.font_size = Some(s),
                    None => style.font_size = inh.font_size,
                }
                // em in non-font-size properties is relative to this element's
                // OWN computed font-size; resolve every relative length now.
                let em_px = style.font_size.unwrap_or(parent_fs);
                crate::style::set_grid_calc_context(style, em_px, root_fs, vw, vh);
                if let Some(expression) = style.letter_spacing_expression.as_deref() {
                    style.letter_spacing = crate::style::resolve_contextual_length(
                        expression, em_px, root_fs, vw, vh, em_px,
                    );
                } else if let Some(raw) = style.letter_spacing_raw {
                    style.letter_spacing = match raw.resolve(em_px, root_fs, vw, vh) {
                        crate::Dimension::Px(pixels) if pixels.is_finite() => Some(pixels),
                        _ => None,
                    };
                }
                match style.letter_spacing {
                    Some(spacing) if spacing.is_finite() => inh.letter_spacing = spacing,
                    _ => style.letter_spacing = Some(inh.letter_spacing),
                }
                match style.letter_spacing_non_normal {
                    Some(non_normal) => inh.letter_spacing_non_normal = non_normal,
                    None => style.letter_spacing_non_normal = Some(inh.letter_spacing_non_normal),
                }
                if style.container_type_inherit {
                    style.container_type = inh.container_type;
                }
                if style.container_names_inherit {
                    style.container_names.clone_from(&inh.container_names);
                }
                inh.container_type = style.container_type;
                inh.container_names.clone_from(&style.container_names);
                if style.overflow_inherit_x {
                    style.overflow_specified_x = inh.overflow_x;
                    style.overflow_inherit_x = false;
                }
                if style.overflow_inherit_y {
                    style.overflow_specified_y = inh.overflow_y;
                    style.overflow_inherit_y = false;
                }
                crate::style::recompute_overflow(style);
                inh.overflow_x = if style.overflow_scroll_x {
                    2
                } else if style.overflow_clip_x {
                    1
                } else {
                    0
                };
                inh.overflow_y = if style.overflow_scroll_y {
                    2
                } else if style.overflow_clip_y {
                    1
                } else {
                    0
                };
                if let Some(expression) = style.row_gap_expression.as_deref() {
                    style.row_gap = crate::style::resolve_contextual_length(
                        expression,
                        em_px,
                        root_fs,
                        vw,
                        vh,
                        inh.cb_width,
                    );
                }
                if let Some(expression) = style.column_gap_expression.as_deref() {
                    style.column_gap = crate::style::resolve_contextual_length(
                        expression,
                        em_px,
                        root_fs,
                        vw,
                        vh,
                        inh.cb_width,
                    );
                }
                if let Some(expression) = style.line_height_expression.as_deref() {
                    if let Some(resolved) = crate::style::resolve_contextual_length(
                        expression, em_px, root_fs, vw, vh, em_px,
                    ) {
                        style.line_height = Some(
                            if crate::style::line_height_expression_is_length(expression) {
                                crate::LineHeight::Px(resolved)
                            } else {
                                crate::LineHeight::Ratio(resolved)
                            },
                        );
                    }
                } else if let Some(crate::LineHeight::Relative(relative)) = style.line_height {
                    let pixels = match relative {
                        crate::Dimension::Percent(percent) => em_px * percent,
                        dimension => match dimension.resolve(em_px, root_fs, vw, vh) {
                            crate::Dimension::Px(pixels) => pixels,
                            _ => em_px,
                        },
                    };
                    style.line_height = Some(crate::LineHeight::Px(pixels));
                }
                let cb_w = inh.cb_width;
                for index in 0..6 {
                    let Some(expression) = style.size_expressions[index].as_deref() else {
                        continue;
                    };
                    // A proper replaced box's cyclic percentage maximum has a
                    // distinct intrinsic-sizing rule. Preserve functional
                    // spellings such as calc(100%) as the same typed percent
                    // used by the bare-token path instead of prematurely
                    // collapsing them against an ancestor width.
                    if index == 4 && style.has_replaced_sizing {
                        if let Some(percent) =
                            crate::style::functional_percentage_factor(expression)
                        {
                            style.max_width = crate::Dimension::Percent(percent);
                            continue;
                        }
                    }
                    let percent_base = if matches!(index, 1 | 3 | 5) {
                        viewport.1
                    } else {
                        cb_w
                    };
                    let Some(px) = crate::style::resolve_contextual_length(
                        expression,
                        em_px,
                        root_fs,
                        vw,
                        vh,
                        percent_base,
                    ) else {
                        continue;
                    };
                    let resolved = crate::Dimension::Px(px);
                    match index {
                        0 => style.width = resolved,
                        1 => style.height = resolved,
                        2 => style.min_width = resolved,
                        3 => style.min_height = resolved,
                        4 => style.max_width = resolved,
                        _ => style.max_height = resolved,
                    }
                }
                style.width = style.width.resolve(em_px, root_fs, vw, vh);
                style.height = style.height.resolve(em_px, root_fs, vw, vh);
                style.min_width = style.min_width.resolve(em_px, root_fs, vw, vh);
                style.min_height = style.min_height.resolve(em_px, root_fs, vw, vh);
                style.max_width = style.max_width.resolve(em_px, root_fs, vw, vh);
                style.max_height = style.max_height.resolve(em_px, root_fs, vw, vh);
                style.flex_basis = style.flex_basis.resolve(em_px, root_fs, vw, vh);
                if matches!(style.height, crate::Dimension::Percent(_))
                    && !inh.cb_height_definite
                    && !matches!(style.position, Some(taffy::Position::Absolute))
                {
                    style.height = crate::Dimension::Auto;
                }
                child_cb_height_definite = matches!(
                    style.height,
                    crate::Dimension::Px(_) | crate::Dimension::Percent(_)
                );
                if child_cb_height_definite {
                    definite_height_nodes.insert(id);
                }
                for index in 0..4 {
                    let Some(expression) = style.inset_expressions[index].as_deref() else {
                        continue;
                    };
                    let percent_base = if matches!(index, 1 | 3) {
                        cb_w
                    } else {
                        viewport.1
                    };
                    style.inset[index] = crate::style::resolve_contextual_length(
                        expression,
                        em_px,
                        root_fs,
                        vw,
                        vh,
                        percent_base,
                    )
                    .map(crate::Dimension::Px);
                }
                for i in style.inset.iter_mut() {
                    if let Some(d) = i {
                        *i = Some(d.resolve(em_px, root_fs, vw, vh));
                    }
                }
                // A fixed box is positioned against the initial containing
                // block, whose dimensions are the viewport, not the full
                // scrollable root element. Taffy attaches out-of-flow boxes to
                // a layout node, and the root node can grow to the document's
                // full content height. Make the CSS 2.1 stretch equation
                // explicit for the ubiquitous `position:fixed; inset:0` case
                // so overlays/canvases remain exactly viewport-sized.
                if style.position_fixed {
                    if style.width == crate::Dimension::Auto {
                        if let (
                            Some(crate::Dimension::Px(right)),
                            Some(crate::Dimension::Px(left)),
                        ) = (style.inset[1], style.inset[3])
                        {
                            style.width =
                                crate::Dimension::Px((initial_cb_width - left - right).max(0.0));
                        }
                    }
                    if style.height == crate::Dimension::Auto {
                        if let (
                            Some(crate::Dimension::Px(top)),
                            Some(crate::Dimension::Px(bottom)),
                        ) = (style.inset[0], style.inset[2])
                        {
                            style.height =
                                crate::Dimension::Px((viewport.1 - top - bottom).max(0.0));
                        }
                    }
                    child_cb_height_definite = matches!(
                        style.height,
                        crate::Dimension::Px(_) | crate::Dimension::Percent(_)
                    );
                }
                let computed_weight = crate::style::computed_font_weight(
                    style.font_weight.as_deref(),
                    inh.font_weight,
                );
                style.font_weight = Some(computed_weight.to_string());
                inh.font_weight = computed_weight;
                match &style.font_family {
                    Some(f) => inh.font_family = Some(f.clone()),
                    None => style.font_family = inh.font_family.clone(),
                }
                match style.font_optical_sizing {
                    Some(value) => inh.font_optical_sizing = value,
                    None => style.font_optical_sizing = Some(inh.font_optical_sizing),
                }
                match &style.font_variation_settings {
                    Some(settings) => inh.font_variation_settings.clone_from(settings),
                    None => {
                        style.font_variation_settings = Some(inh.font_variation_settings.clone())
                    }
                }
                let is_table = tree.get_node(id).map_or(false, |node| {
                    node.as_element()
                        .map_or(false, |name| name.local.as_ref() == "table")
                });
                if is_table && inh.legacy_center && style.text_align.is_none() {
                    // The vendor alignment used by <center> centers the table
                    // outer box but does not leak into its internal formatting
                    // context. Browser UA table layout resets it to start.
                    style.text_align = Some(taffy::AlignItems::FLEX_START);
                    style.legacy_center = false;
                    inh.text_align = style.text_align;
                    inh.legacy_center = false;
                } else {
                    match style.text_align {
                        Some(a) => {
                            inh.text_align = Some(a);
                            inh.legacy_center = style.legacy_center;
                        }
                        None => {
                            style.text_align = inh.text_align;
                            style.legacy_center = inh.legacy_center;
                        }
                    }
                }
                match style.text_indent {
                    Some(indent) => {
                        let indent = indent.resolve(em_px, root_fs, vw, vh);
                        style.text_indent = Some(indent);
                        inh.text_indent = indent;
                    }
                    None => style.text_indent = Some(inh.text_indent),
                }
                inh.visibility_hidden = style.visibility_hidden.unwrap_or(inh.visibility_hidden);
                inh.has_zero_opacity |= style.opacity.is_some_and(|value| value <= 0.0);
                style.effectively_invisible = inh.visibility_hidden || inh.has_zero_opacity;
                match style.list_style {
                    Some(v) => inh.list_style = v,
                    None => style.list_style = Some(inh.list_style),
                }
                match style.line_height {
                    Some(v) => inh.line_height = v,
                    None => style.line_height = Some(inh.line_height),
                }
                match style.white_space {
                    Some(v) => inh.white_space = v,
                    None => style.white_space = Some(inh.white_space),
                }
                match style.overflow_wrap {
                    Some(v) => inh.overflow_wrap = v,
                    None => style.overflow_wrap = Some(inh.overflow_wrap),
                }
                match style.word_break {
                    Some(v) => inh.word_break = v,
                    None => style.word_break = Some(inh.word_break),
                }
                match style.text_wrap_style {
                    Some(v) => inh.text_wrap_style = v,
                    None => style.text_wrap_style = Some(inh.text_wrap_style),
                }
                match style.text_transform {
                    Some(v) => inh.text_transform = v,
                    None => style.text_transform = Some(inh.text_transform),
                }
                match style.font_style_italic {
                    Some(v) => inh.italic = v,
                    None => style.font_style_italic = Some(inh.italic),
                }
                if style.box_sizing == crate::BoxSizing::Inherit {
                    style.box_sizing = inh.box_sizing;
                }
                inh.box_sizing = style.box_sizing;
                match style.border_collapse {
                    Some(value) => inh.border_collapse = value,
                    None => style.border_collapse = Some(inh.border_collapse),
                }
                let is_table_part = tree.get_node(id).map_or(false, |node| {
                    node.as_element().map_or(false, |name| {
                        matches!(
                            name.local.as_ref(),
                            "tbody" | "thead" | "tfoot" | "tr" | "td" | "th"
                        )
                    })
                });
                if is_table_part {
                    match style.vertical_align {
                        Some(value) => inh.table_vertical_align = Some(value),
                        None => style.vertical_align = inh.table_vertical_align,
                    }
                }

                // Resolve font/viewport-relative box edges now that their
                // reference sizes are known. Percentage margins still use the
                // estimated containing-block width here. Pure percentage
                // padding stays typed until Taffy knows the final flex/grid
                // containing-block inline size; its used pixels are synced
                // back into `LayoutStyle` after the final layout.
                for i in 0..4 {
                    if let Some(expression) = style.padding_expressions[i].as_deref() {
                        if let Some(px) = crate::style::resolve_contextual_length(
                            expression, em_px, root_fs, vw, vh, cb_w,
                        ) {
                            match i {
                                0 => style.padding.top = px.max(0.0),
                                1 => style.padding.right = px.max(0.0),
                                2 => style.padding.bottom = px.max(0.0),
                                _ => style.padding.left = px.max(0.0),
                            }
                        }
                    }
                    if let Some(expression) = style.margin_expressions[i].as_deref() {
                        if let Some(px) = crate::style::resolve_contextual_length(
                            expression, em_px, root_fs, vw, vh, cb_w,
                        ) {
                            match i {
                                0 => style.margin.top = px,
                                1 => style.margin.right = px,
                                2 => style.margin.bottom = px,
                                _ => style.margin.left = px,
                            }
                        }
                    }
                    if let Some(relative) = style.padding_relative[i] {
                        if let crate::Dimension::Px(px) = relative.resolve(em_px, root_fs, vw, vh) {
                            match i {
                                0 => style.padding.top = px.max(0.0),
                                1 => style.padding.right = px.max(0.0),
                                2 => style.padding.bottom = px.max(0.0),
                                _ => style.padding.left = px.max(0.0),
                            }
                        }
                    }
                    if let Some(relative) = style.margin_relative[i] {
                        if let crate::Dimension::Px(px) = relative.resolve(em_px, root_fs, vw, vh) {
                            match i {
                                0 => style.margin.top = px,
                                1 => style.margin.right = px,
                                2 => style.margin.bottom = px,
                                _ => style.margin.left = px,
                            }
                        }
                    }
                    if let Some(frac) = style.margin_percent[i] {
                        let px = frac * cb_w;
                        match i {
                            0 => style.margin.top = px,
                            1 => style.margin.right = px,
                            2 => style.margin.bottom = px,
                            _ => style.margin.left = px,
                        }
                    }
                }

                // Pseudo-elements inherit the originating element's COMPUTED
                // values. They were cascaded beside the host before this
                // top-down pass, so inheriting its still-unresolved specified
                // values there made `font-size:.875rem` disappear and left a
                // positioned generated label at the 16px paint fallback.
                // Resolve pseudo-authored relative values against the host,
                // then fill every omitted inherited property from the host's
                // now-final computed style.
                let host_color = style.color;
                let host_font_size = style.font_size.unwrap_or(parent_fs);
                let host_letter_spacing = style.letter_spacing.unwrap_or(0.0);
                let host_letter_spacing_non_normal =
                    style.letter_spacing_non_normal.unwrap_or(false);
                let host_weight = crate::style::used_font_weight(style);
                let host_family = style.font_family.clone();
                let host_optical_sizing = style.font_optical_sizing;
                let host_variation_settings = style.font_variation_settings.clone();
                let host_line_height = style.line_height;
                let host_white_space = style.white_space;
                let host_overflow_wrap = style.overflow_wrap;
                let host_word_break = style.word_break;
                let host_text_wrap_style = style.text_wrap_style;
                let host_transform = style.text_transform;
                let host_italic = style.font_style_italic;
                let host_text_align = style.text_align;
                let host_text_indent = style.text_indent;
                let host_invisible = style.effectively_invisible;
                let host_display = style.display;
                let host_direction = style.direction.unwrap_or(taffy::Direction::Ltr);
                let host_display_contents = style.display_contents;
                let host_is_inline_block = style.is_inline_block;
                let host_flow_root = style.flow_root;
                let host_is_table_box = style.is_table_box;
                let host_is_table_cell_box = style.is_table_cell_box;
                let host_grid_auto_columns = style.grid_auto_columns.clone();
                let host_grid_auto_rows = style.grid_auto_rows.clone();
                let host_grid_auto_column_calcs = style.grid_calc_expressions[2].clone();
                let host_grid_auto_row_calcs = style.grid_calc_expressions[3].clone();
                let host_overflow_x = if style.overflow_scroll_x {
                    2
                } else if style.overflow_clip_x {
                    1
                } else {
                    0
                };
                let host_overflow_y = if style.overflow_scroll_y {
                    2
                } else if style.overflow_clip_y {
                    1
                } else {
                    0
                };
                let settle_pseudo = |pseudo: &mut crate::LayoutStyle| {
                    if pseudo.direction.is_none() {
                        pseudo.direction = Some(host_direction);
                    }
                    crate::style::resolve_logical_borders(pseudo);
                    if pseudo.grid_auto_columns_inherit {
                        pseudo.grid_auto_columns = host_grid_auto_columns.clone();
                        pseudo.grid_calc_expressions[2] = host_grid_auto_column_calcs.clone();
                        pseudo.grid_auto_columns_inherit = false;
                    }
                    if pseudo.grid_auto_rows_inherit {
                        pseudo.grid_auto_rows = host_grid_auto_rows.clone();
                        pseudo.grid_calc_expressions[3] = host_grid_auto_row_calcs.clone();
                        pseudo.grid_auto_rows_inherit = false;
                    }
                    if pseudo.display_inherit {
                        pseudo.display = host_display;
                        pseudo.display_contents = host_display_contents;
                        pseudo.is_inline_block = host_is_inline_block;
                        pseudo.flow_root = host_flow_root;
                        pseudo.is_table_box = host_is_table_box;
                        pseudo.is_table_cell_box = host_is_table_cell_box;
                        pseudo.internal_flex_container = pseudo.is_table_cell_box;
                        if pseudo.is_table_cell_box {
                            pseudo.flex_direction = Some(taffy::FlexDirection::Column);
                            pseudo.align_items = Some(taffy::AlignItems::FLEX_START);
                        }
                        pseudo.display_inherit = false;
                    }
                    if pseudo.overflow_inherit_x {
                        pseudo.overflow_specified_x = host_overflow_x;
                        pseudo.overflow_inherit_x = false;
                    }
                    if pseudo.overflow_inherit_y {
                        pseudo.overflow_specified_y = host_overflow_y;
                        pseudo.overflow_inherit_y = false;
                    }
                    crate::style::recompute_overflow(pseudo);
                    if let Some(expression) = pseudo.font_size_expression.as_deref() {
                        pseudo.font_size = crate::style::resolve_contextual_length(
                            expression,
                            host_font_size,
                            root_fs,
                            vw,
                            vh,
                            host_font_size,
                        );
                    } else if let Some(raw) = pseudo.font_size_raw {
                        pseudo.font_size = Some(match raw {
                            crate::Dimension::Percent(percent) => host_font_size * percent,
                            crate::Dimension::Em(value) => host_font_size * value,
                            dimension => match dimension.resolve(host_font_size, root_fs, vw, vh) {
                                crate::Dimension::Px(pixels) => pixels,
                                _ => host_font_size,
                            },
                        });
                    } else if pseudo.font_size.is_none() {
                        pseudo.font_size = Some(host_font_size);
                    }
                    let pseudo_em = pseudo.font_size.unwrap_or(host_font_size);
                    crate::style::set_grid_calc_context(pseudo, pseudo_em, root_fs, vw, vh);
                    if let Some(expression) = pseudo.letter_spacing_expression.as_deref() {
                        pseudo.letter_spacing = crate::style::resolve_contextual_length(
                            expression, pseudo_em, root_fs, vw, vh, pseudo_em,
                        );
                    } else if let Some(raw) = pseudo.letter_spacing_raw {
                        pseudo.letter_spacing = match raw.resolve(pseudo_em, root_fs, vw, vh) {
                            crate::Dimension::Px(pixels) if pixels.is_finite() => Some(pixels),
                            _ => None,
                        };
                    } else if pseudo.letter_spacing.is_none() {
                        pseudo.letter_spacing = Some(host_letter_spacing);
                    }
                    if pseudo.letter_spacing_non_normal.is_none() {
                        pseudo.letter_spacing_non_normal = Some(host_letter_spacing_non_normal);
                    }
                    for index in 0..6 {
                        if let Some(expression) = pseudo.size_expressions[index].as_deref() {
                            let percent_base = if matches!(index, 1 | 3 | 5) {
                                viewport.1
                            } else {
                                cb_w
                            };
                            if let Some(px) = crate::style::resolve_contextual_length(
                                expression,
                                pseudo_em,
                                root_fs,
                                vw,
                                vh,
                                percent_base,
                            ) {
                                let resolved = crate::Dimension::Px(px);
                                match index {
                                    0 => pseudo.width = resolved,
                                    1 => pseudo.height = resolved,
                                    2 => pseudo.min_width = resolved,
                                    3 => pseudo.min_height = resolved,
                                    4 => pseudo.max_width = resolved,
                                    _ => pseudo.max_height = resolved,
                                }
                            }
                        }
                    }
                    pseudo.width = pseudo.width.resolve(pseudo_em, root_fs, vw, vh);
                    pseudo.height = pseudo.height.resolve(pseudo_em, root_fs, vw, vh);
                    pseudo.min_width = pseudo.min_width.resolve(pseudo_em, root_fs, vw, vh);
                    pseudo.min_height = pseudo.min_height.resolve(pseudo_em, root_fs, vw, vh);
                    pseudo.max_width = pseudo.max_width.resolve(pseudo_em, root_fs, vw, vh);
                    pseudo.max_height = pseudo.max_height.resolve(pseudo_em, root_fs, vw, vh);
                    for index in 0..4 {
                        if let Some(relative) = pseudo.padding_relative[index] {
                            if let crate::Dimension::Px(px) =
                                relative.resolve(pseudo_em, root_fs, vw, vh)
                            {
                                match index {
                                    0 => pseudo.padding.top = px.max(0.0),
                                    1 => pseudo.padding.right = px.max(0.0),
                                    2 => pseudo.padding.bottom = px.max(0.0),
                                    _ => pseudo.padding.left = px.max(0.0),
                                }
                            }
                        }
                        if let Some(relative) = pseudo.margin_relative[index] {
                            if let crate::Dimension::Px(px) =
                                relative.resolve(pseudo_em, root_fs, vw, vh)
                            {
                                match index {
                                    0 => pseudo.margin.top = px,
                                    1 => pseudo.margin.right = px,
                                    2 => pseudo.margin.bottom = px,
                                    _ => pseudo.margin.left = px,
                                }
                            }
                        }
                        if let Some(percent) = pseudo.margin_percent[index] {
                            let px = percent * cb_w;
                            match index {
                                0 => pseudo.margin.top = px,
                                1 => pseudo.margin.right = px,
                                2 => pseudo.margin.bottom = px,
                                _ => pseudo.margin.left = px,
                            }
                        }
                        if let Some(inset) = pseudo.inset[index] {
                            pseudo.inset[index] = Some(inset.resolve(pseudo_em, root_fs, vw, vh));
                        }
                    }
                    let weight = crate::style::computed_font_weight(
                        pseudo.font_weight.as_deref(),
                        host_weight,
                    );
                    pseudo.font_weight = Some(weight.to_string());
                    if pseudo.font_family.is_none() {
                        pseudo.font_family = host_family.clone();
                    }
                    if pseudo.font_optical_sizing.is_none() {
                        pseudo.font_optical_sizing = host_optical_sizing;
                    }
                    if pseudo.font_variation_settings.is_none() {
                        pseudo.font_variation_settings = host_variation_settings.clone();
                    }
                    if let Some(expression) = pseudo.line_height_expression.as_deref() {
                        if let Some(resolved) = crate::style::resolve_contextual_length(
                            expression, pseudo_em, root_fs, vw, vh, pseudo_em,
                        ) {
                            pseudo.line_height = Some(
                                if crate::style::line_height_expression_is_length(expression) {
                                    crate::LineHeight::Px(resolved)
                                } else {
                                    crate::LineHeight::Ratio(resolved)
                                },
                            );
                        }
                    } else if let Some(crate::LineHeight::Relative(relative)) = pseudo.line_height {
                        let pixels = match relative {
                            crate::Dimension::Percent(percent) => pseudo_em * percent,
                            dimension => match dimension.resolve(pseudo_em, root_fs, vw, vh) {
                                crate::Dimension::Px(pixels) => pixels,
                                _ => pseudo_em,
                            },
                        };
                        pseudo.line_height = Some(crate::LineHeight::Px(pixels));
                    } else if pseudo.line_height.is_none() {
                        pseudo.line_height = host_line_height;
                    }
                    if pseudo.white_space.is_none() {
                        pseudo.white_space = host_white_space;
                    }
                    if pseudo.overflow_wrap.is_none() {
                        pseudo.overflow_wrap = host_overflow_wrap;
                    }
                    if pseudo.word_break.is_none() {
                        pseudo.word_break = host_word_break;
                    }
                    if pseudo.text_wrap_style.is_none() {
                        pseudo.text_wrap_style = host_text_wrap_style;
                    }
                    if pseudo.color.is_none() {
                        pseudo.color = host_color;
                    }
                    if pseudo.text_transform.is_none() {
                        pseudo.text_transform = host_transform;
                    }
                    if pseudo.font_style_italic.is_none() {
                        pseudo.font_style_italic = host_italic;
                    }
                    if pseudo.text_align.is_none() {
                        pseudo.text_align = host_text_align;
                    }
                    if pseudo.text_indent.is_none() {
                        pseudo.text_indent = host_text_indent;
                    } else if let Some(indent) = pseudo.text_indent {
                        pseudo.text_indent = Some(indent.resolve(pseudo_em, root_fs, vw, vh));
                    }
                    pseudo.effectively_invisible = host_invisible;
                };
                if let Some(pseudo) = style.before_pseudo.as_deref_mut() {
                    settle_pseudo(pseudo);
                }
                if let Some(pseudo) = style.after_pseudo.as_deref_mut() {
                    settle_pseudo(pseudo);
                }

                // Containing-block width handed to this element's children is
                // its own content-box width. A definite content-box width is
                // already that value; a border-box or auto width includes
                // padding and border, which must be removed.
                let used_w = match style.width {
                    crate::Dimension::Px(w) => w,
                    crate::Dimension::Percent(p) => p * cb_w,
                    _ => (cb_w - style.margin.left - style.margin.right).max(0.0),
                };
                let definite_content_box = matches!(
                    style.width,
                    crate::Dimension::Px(_) | crate::Dimension::Percent(_)
                ) && style.box_sizing == crate::BoxSizing::ContentBox;
                child_cb_width = if definite_content_box {
                    used_w.max(0.0)
                } else {
                    (used_w
                        - style.padding.left
                        - style.padding.right
                        - style.border.left
                        - style.border.right)
                        .max(0.0)
                };
            }
            inh.cb_width = child_cb_width;
            inh.cb_height_definite = child_cb_height_definite;
            for cid in style_children(tree, id).into_iter().rev() {
                queue.push((cid, inh.clone()));
            }
        }

        // Root/body overflow propagated to the viewport leaves the source
        // element itself overflow-visible for Taffy and BFC decisions.
        mark_viewport_overflow_source(tree, root_id, &mut styles);

        // Border-collapse is inherited, so only distribute a table's
        // effective spacing to the legacy flex fallback after the computed
        // top-down values are known.
        propagate_border_spacing(tree, &mut styles);

        // Resolve native form-control intrinsic border-box geometry after
        // inheritance and author cascading. Text-like inputs use the HTML
        // `size` attribute (20 by default) and the control's own computed
        // font; author CSS widths/heights remain authoritative. Without this
        // replaced-control sizing, inputs/selects are empty auto-sized leaves
        // (0px tall and often stretched to their container).
        let native_button_contents: HashMap<NodeId, NativeButtonIntrinsicContent> = styles
            .iter()
            .filter_map(|(&id, style)| {
                let node = tree.get_node(id)?;
                let element = node.as_element()?;
                (element.local.as_ref() == "button"
                    && style.width == crate::Dimension::Auto
                    && container_auto_inline_size(tree, id, style, &styles)
                        != ContainerAutoInlineSize::StretchedGridItem)
                    .then(|| {
                        let font_size = style.font_size.unwrap_or(13.333_333).max(1.0);
                        (
                            id,
                            native_button_intrinsic_content(tree, id, &styles, font_size),
                        )
                    })
            })
            .collect();
        let native_control_grid_stretch: HashMap<NodeId, (bool, bool)> = styles
            .iter()
            .filter_map(|(&id, style)| {
                let node = tree.get_node(id)?;
                let element = node.as_element()?;
                matches!(element.local.as_ref(), "input" | "select" | "textarea").then(|| {
                    (
                        id,
                        (
                            container_auto_inline_size(tree, id, style, &styles)
                                == ContainerAutoInlineSize::StretchedGridItem,
                            container_auto_block_size(tree, id, style, &styles)
                                == ContainerAutoBlockSize::StretchedGridItem,
                        ),
                    )
                })
            })
            .collect();
        for (&id, style) in styles.iter_mut() {
            let Some(node) = tree.get_node(id) else {
                continue;
            };
            let Some(element) = node.as_element() else {
                continue;
            };
            if element.local.as_ref() == "button"
                && style.width == crate::Dimension::Auto
                && native_button_contents.contains_key(&id)
            {
                // Buttons remain intrinsically sized form controls even when
                // author CSS changes their inner display to flex/grid. Treating
                // `display:flex` as an ordinary block makes an auto-width
                // button stretch to its containing block (MDN's compact search
                // pill became 637px wide). Approximate the native max-content
                // border box from its label, generated icon, gap, and edges.
                let font_size = style.font_size.unwrap_or(13.333_333).max(1.0);
                let bold = crate::style::used_font_weight(style) >= 600;
                let intrinsic_content = native_button_contents.get(&id);
                let label = intrinsic_content
                    .map(|content| content.text.as_str())
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut content_width = text_width(
                    &label,
                    font_size,
                    bold,
                    style.font_family.as_deref(),
                    style.letter_spacing.unwrap_or(0.0),
                );
                content_width += intrinsic_content
                    .map(|content| content.atomic_width)
                    .unwrap_or(0.0);
                let pseudo_width = |pseudo: Option<&crate::LayoutStyle>| {
                    let Some(pseudo) = pseudo else { return 0.0 };
                    let content = match pseudo.width {
                        crate::Dimension::Px(px) => px.max(0.0),
                        crate::Dimension::Em(value) | crate::Dimension::Rem(value) => {
                            (value * font_size).max(0.0)
                        }
                        crate::Dimension::Percent(percent) => (percent * font_size).max(0.0),
                        _ if pseudo.mask_image.is_some() || pseudo.background_image.is_some() => {
                            font_size
                        }
                        _ => 0.0,
                    };
                    let horizontal_edges = pseudo.padding.left
                        + pseudo.padding.right
                        + pseudo.border.left
                        + pseudo.border.right;
                    let border_box = if pseudo.box_sizing == crate::BoxSizing::ContentBox {
                        content + horizontal_edges
                    } else {
                        content.max(horizontal_edges)
                    };
                    border_box + pseudo.margin.left.max(0.0) + pseudo.margin.right.max(0.0)
                };
                let before = pseudo_width(style.before_pseudo.as_deref());
                let after = pseudo_width(style.after_pseudo.as_deref());
                let extra_parts = usize::from(before > 0.0) + usize::from(after > 0.0);
                content_width += before + after;
                if extra_parts > 0 && !label.is_empty() {
                    content_width += style.column_gap.unwrap_or(0.0) * extra_parts as f32;
                }
                let horizontal_edges = style.padding.left
                    + style.padding.right
                    + style.border.left
                    + style.border.right;
                style.width =
                    crate::Dimension::Px(if style.box_sizing == crate::BoxSizing::ContentBox {
                        content_width
                    } else {
                        content_width + horizontal_edges
                    });
            }
            if element.local.as_ref() == "select" {
                let option_labels: Vec<String> = tree
                    .descendants(id)
                    .into_iter()
                    .filter(|option_id| {
                        tree.get_node(*option_id).map_or(false, |option| {
                            option
                                .as_element()
                                .map_or(false, |name| name.local.as_ref() == "option")
                        })
                    })
                    .map(|option_id| tree.text_content(option_id).trim().to_string())
                    .collect();
                let font_size = style.font_size.unwrap_or(13.333_333).max(1.0);
                let bold = crate::style::used_font_weight(style) >= 600;
                let label_width = option_labels
                    .iter()
                    .map(|label| {
                        text_width(
                            label,
                            font_size,
                            bold,
                            style.font_family.as_deref(),
                            style.letter_spacing.unwrap_or(0.0),
                        )
                    })
                    .fold(0.0f32, f32::max);
                let horizontal_edges = style.padding.left
                    + style.padding.right
                    + style.border.left
                    + style.border.right;
                let vertical_edges = style.padding.top
                    + style.padding.bottom
                    + style.border.top
                    + style.border.bottom;
                let rows = node
                    .get_attribute("size")
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|&value| value > 1)
                    .unwrap_or(1) as f32;
                let intrinsic_width = label_width + horizontal_edges;
                let intrinsic_height =
                    crate::inline::used_line_height(style).max(1.0) * rows + vertical_edges;
                assign_native_control_size(
                    style,
                    native_control_grid_stretch.get(&id).copied().unwrap_or_default(),
                    intrinsic_width,
                    intrinsic_height,
                    horizontal_edges,
                    vertical_edges,
                );
                continue;
            }
            if element.local.as_ref() == "textarea" {
                // A native textarea's intrinsic border box comes from the
                // rows/cols content attributes (HTML defaults 2 and 20), not
                // from its text content: an empty textarea is still a
                // visible, clickable control (Chromium cols=20 rows=2 ->
                // 168x36, rows=8 -> 126 tall). Per-column width is the
                // fixed-pitch average advance calibrated to Chromium's
                // control metrics; per-row height is the normal line height.
                let rows = node
                    .get_attribute("rows")
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .filter(|&value| value > 0)
                    .unwrap_or(2) as f32;
                let cols = node
                    .get_attribute("cols")
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .filter(|&value| value > 0)
                    .unwrap_or(20) as f32;
                let font_size = style.font_size.unwrap_or(13.333_333).max(1.0);
                let horizontal_edges = style.padding.left
                    + style.padding.right
                    + style.border.left
                    + style.border.right;
                let vertical_edges = style.padding.top
                    + style.padding.bottom
                    + style.border.top
                    + style.border.bottom;
                let intrinsic_width = cols * font_size * 0.6075 + horizontal_edges;
                let intrinsic_height =
                    crate::inline::used_line_height(style).max(1.0) * rows + vertical_edges;
                assign_native_control_size(
                    style,
                    native_control_grid_stretch.get(&id).copied().unwrap_or_default(),
                    intrinsic_width,
                    intrinsic_height,
                    horizontal_edges,
                    vertical_edges,
                );
                continue;
            }
            if element.local.as_ref() != "input" {
                continue;
            }
            let input_type = node
                .get_attribute("type")
                .unwrap_or("text")
                .trim()
                .to_ascii_lowercase();
            if input_type == "hidden" {
                style.display = crate::Display::None;
                continue;
            }

            let font_size = style.font_size.unwrap_or(13.333_333).max(1.0);
            let horizontal_edges =
                style.padding.left + style.padding.right + style.border.left + style.border.right;
            let vertical_edges =
                style.padding.top + style.padding.bottom + style.border.top + style.border.bottom;
            let default_height = crate::inline::used_line_height(style).max(1.0) + vertical_edges;

            let (intrinsic_width, intrinsic_height) = match input_type.as_str() {
                "checkbox" | "radio" => (13.0, 13.0),
                "range" => (129.0, 16.0),
                "color" => (50.0, 27.0),
                "file" => (253.0, default_height.max(22.0)),
                "submit" | "reset" | "button" => {
                    let fallback = match input_type.as_str() {
                        "reset" => "Reset",
                        "button" => "",
                        _ => "Submit Query",
                    };
                    let label = node.get_attribute("value").unwrap_or(fallback);
                    (
                        (label.chars().count() as f32 * font_size * 0.55
                            + font_size * 1.5
                            + horizontal_edges)
                            .max(20.0),
                        default_height,
                    )
                }
                "image" => (horizontal_edges, vertical_edges),
                _ => {
                    let size = node
                        .get_attribute("size")
                        .and_then(|value| value.parse::<u32>().ok())
                        .filter(|&value| value > 0)
                        .unwrap_or(20) as f32;
                    (
                        size * font_size * 0.6 + font_size * 0.675 + horizontal_edges,
                        default_height,
                    )
                }
            };
            assign_native_control_size(
                style,
                native_control_grid_stretch.get(&id).copied().unwrap_or_default(),
                intrinsic_width,
                intrinsic_height,
                horizontal_edges,
                vertical_edges,
            );
        }

        resolve_grid_areas(tree, root_id, &mut styles);

        // The root's outer display is always blockified. Keep a declared
        // inline-flex/grid root's inner mode, but remove its inline
        // participation marker before box construction.
        if let Some(root_style) = styles.get_mut(&root_id) {
            crate::blockify_outer_display(root_style);
        }

        // CSS Display blockification changes only the outer display. Preserve
        // the inner Flex/Grid mode of inline-flex/grid items.
        let item_parents: Vec<NodeId> = styles
            .iter()
            .filter(|(_, s)| {
                s.display == crate::Display::Grid
                    || (s.display == crate::Display::Flex && !s.internal_flex_container)
            })
            .map(|(&id, _)| id)
            .collect();
        for pid in item_parents {
            blockify_layout_children(tree, pid, &mut styles);
        }

        // In an auto-height column flex container, a zero flex basis still
        // participates in intrinsic main-size calculation through the item's
        // automatic minimum size. Taffy otherwise treats `flex: 1 1` plus
        // overflow:auto as an unconditional zero contribution, collapsing the
        // entire ancestor chain even when the item contains many lines. Use
        // an auto basis for this indefinite-size phase; definite-height flex
        // containers keep the authored zero basis and normal free-space math.
        let intrinsic_column_flex_parents: Vec<NodeId> = styles
            .iter()
            .filter(|(_, style)| {
                style.display == crate::Display::Flex
                    && style.flex_direction == Some(taffy::FlexDirection::Column)
                    && style.height == crate::Dimension::Auto
            })
            .map(|(&id, _)| id)
            .collect();
        for parent in intrinsic_column_flex_parents {
            for child in tree.children(parent) {
                let Some(style) = styles.get_mut(&child) else {
                    continue;
                };
                let zero_basis = matches!(
                    style.flex_basis,
                    crate::Dimension::Px(value) | crate::Dimension::Percent(value)
                        if value == 0.0
                );
                if zero_basis
                    && style.height == crate::Dimension::Auto
                    && style.min_height == crate::Dimension::Auto
                {
                    style.flex_basis = crate::Dimension::Auto;
                }
            }
        }

        // Absolute/fixed boxes and floats are blockified externally but retain
        // their inner formatting mode and their context-specific shrink-fit.
        for style in styles.values_mut() {
            if matches!(style.position, Some(taffy::Position::Absolute)) || style.float.is_some() {
                crate::blockify_outer_display(style);
            }
        }
        blockify_generated_pseudos(&mut styles);

        // The initial containing block is modelled by the root taffy node's
        // definite viewport height (set at build), which is what `html,body{
        // height:100%}` chains and sticky-footer app shells resolve against.
        // taffy 0.7 needed an extra `min-height: viewport` push on the root
        // style to stop those chains collapsing to content height; taffy 0.12
        // resolves them from the node's definite height directly, and that push
        // instead over-propagated the viewport height down every `height:100%`
        // subtree (nextjs.org's nav stretched to a full 688px, burying the
        // hero), so it is gone.

        // Capture the definite containing width for ratio-only auto/auto
        // replaced boxes before installing their metadata. The inline layout
        // stand-in asks atomic items for max-content before its own percentage
        // width becomes definite; Gecko instead resolves this case directly
        // from the containing block's content width.
        let ratio_only_available_widths: HashMap<NodeId, f32> = intrinsic
            .iter()
            .filter_map(|(&nid, metadata)| {
                let style = styles.get(&nid)?;
                (metadata.width.is_none()
                    && metadata.height.is_none()
                    && metadata.ratio.is_some()
                    && style.width == crate::Dimension::Auto
                    && style.height == crate::Dimension::Auto)
                    .then(|| {
                        reliable_ratio_only_available_width(
                            tree,
                            nid,
                            &styles,
                            initial_cb_width,
                        )
                        .map(|width| (nid, width))
                    })
                    .flatten()
            })
            .collect();

        // Apply fetched intrinsic image sizes. A replaced element with no
        // explicit dimensions must size from its intrinsic pixels (else it is
        // 0x0 and never paints); with one dimension given, the aspect ratio
        // fills the other. An authored `max-width:100%` still caps it to the
        // container, and the aspect ratio keeps it proportional.
        for (&nid, &metadata) in intrinsic {
            if metadata.natural_size().is_none() {
                continue;
            }
            if let Some(s) = styles.get_mut(&nid) {
                s.intrinsic_size = metadata.natural_size();
                s.replaced_intrinsic = Some(metadata);
                s.ratio_only_available_width = ratio_only_available_widths.get(&nid).copied();
                if (s.aspect_ratio.is_none() || s.aspect_ratio_is_mapped)
                    && metadata.ratio.is_some()
                {
                    s.aspect_ratio = metadata.ratio;
                    s.aspect_ratio_is_mapped = false;
                    s.aspect_ratio_is_intrinsic = true;
                }
            }
        }

        let deferred_cyclic_inline_sizes =
            defer_cyclic_flex_inline_sizes(tree, &mut styles, root_fs, vw, vh);

        if let Some(taffy_root) = build(
            tree,
            root_id,
            &mut taffy_tree,
            &mut id_map,
            &mut words,
            &mut engine,
            &mut ifc_items,
            &styles,
        ) {
            // Taffy has no outer display type and only gives an auto-width
            // Block root the initial-containing-block width. CSS blockifies
            // Flex/Grid roots too, so supply the equivalent used width while
            // leaving an authored root width untouched.
            if let Some(root_style) = styles.get(&root_id) {
                if root_style.width == crate::Dimension::Auto
                    && matches!(
                        root_style.display,
                        crate::Display::Flex | crate::Display::Grid
                    )
                {
                    if let Ok(current) = taffy_tree.style(taffy_root) {
                        let mut adjusted = current.clone();
                        let outer =
                            (initial_cb_width - root_style.margin.left - root_style.margin.right)
                                .max(0.0);
                        let declared = if root_style.box_sizing == crate::BoxSizing::ContentBox {
                            (outer
                                - root_style.padding.left
                                - root_style.padding.right
                                - root_style.border.left
                                - root_style.border.right)
                                .max(0.0)
                        } else {
                            outer
                        };
                        adjusted.size.width = taffy::Dimension::length(declared);
                        let _ = taffy_tree.set_style(taffy_root, adjusted);
                    }
                }
            }
            let static_position_candidates = reparent_inset_positioned_nodes(
                tree,
                &mut taffy_tree,
                taffy_root,
                &id_map,
                &styles,
            );
            let available = taffy::Size {
                width: taffy::AvailableSpace::Definite(initial_cb_width),
                height: taffy::AvailableSpace::Definite(viewport.1),
            };
            #[cfg(feature = "paint")]
            {
                let engine = &mut engine;
                let mut measure = |known: taffy::Size<Option<f32>>,
                                   avail: taffy::Size<taffy::AvailableSpace>,
                                   _node,
                                   ctx: Option<&mut usize>,
                                   _style: &taffy::Style| {
                    match ctx {
                        Some(&mut idx) => engine.measure_taffy(idx, known, avail),
                        None => taffy::Size::ZERO,
                    }
                };

                // Table used-width pass. A grid table is built at width:auto, so
                // its final width has to be chosen the way CSS chooses a table's
                // used width: at least the content min-content (so an unbreakable
                // wide cell, e.g. a fixed-width image row in an infobox, makes the
                // table grow rather than overflow), at most the available width,
                // preferring the author's specified width when there is one.
                let mut tables: Vec<(taffy::NodeId, NodeId, usize)> = id_map
                    .iter()
                    .filter(|(taffy, _)| ifc_items.table_rows.contains_key(taffy))
                    .map(|(t, d)| (*t, *d, table_ancestor_depth(tree, *d, &styles)))
                    .collect();
                // Outer tables consume their nested tables' intrinsic sizes.
                // Only after an outer depth is fixed can a root layout expose
                // the real cell width available to the next nested depth.
                tables.sort_by(|a, b| a.2.cmp(&b.2));
                let mut table_index = 0usize;
                while table_index < tables.len() {
                    let depth = tables[table_index].2;
                    let mut group_end = table_index + 1;
                    while group_end < tables.len() && tables[group_end].2 == depth {
                        group_end += 1;
                    }
                    let group = &tables[table_index..group_end];
                    let mut available_widths: HashMap<taffy::NodeId, f32> = HashMap::new();
                    let needs_layout_snapshot = group.iter().any(|(_, dom, _)| {
                        styles.get(dom).is_some_and(|style| {
                            (style.width == crate::Dimension::Auto
                                && (depth > 0
                                    || reliable_table_available_width(
                                        tree,
                                        *dom,
                                        &styles,
                                        initial_cb_width,
                                    )
                                    .is_none()))
                                || (style.table_layout_fixed
                                    && matches!(style.width, crate::Dimension::Percent(_)))
                        })
                    });
                    if needs_layout_snapshot {
                        // This pass is gated to layout-dependent containing
                        // blocks and nested auto tables. Flat tables in normal
                        // block flow use the reliable cascade-time width below
                        // and pay no additional full-tree layout.
                        let _ = taffy_tree.compute_layout_with_measure(
                            taffy_root,
                            available,
                            &mut measure,
                        );
                        for &(tnode, dom, _) in group {
                            if styles
                                .get(&dom)
                                .is_some_and(|style| {
                                    style.width == crate::Dimension::Auto
                                        || (style.table_layout_fixed
                                            && matches!(
                                                style.width,
                                                crate::Dimension::Percent(_)
                                            ))
                                })
                            {
                                if let Ok(layout) = taffy_tree.layout(tnode) {
                                    available_widths.insert(tnode, layout.size.width.max(0.0));
                                }
                            }
                        }
                    }
                    for &(tnode, dom, _) in group {
                        let Some(table_style) = styles.get(&dom) else {
                            continue;
                        };
                        if let Some(columns) = ifc_items.fixed_table_cols.get(&tnode) {
                            let ncols = columns.len();
                            if ncols == 0 {
                                continue;
                            }
                            let inline_outer_edges = table_inline_outer_edges(table_style);
                            let (horizontal_spacing, _) = table_spacing(table_style);
                            let mut used_outer = match table_style.width {
                                crate::Dimension::Px(width)
                                    if table_style.box_sizing
                                        == crate::BoxSizing::ContentBox =>
                                {
                                    // Border spacing lives inside a CSS table's
                                    // content box. It is artificial Taffy
                                    // padding in our grid model, but must not
                                    // enlarge an authored content-box width.
                                    width.max(0.0)
                                        + (inline_outer_edges - horizontal_spacing * 2.0)
                                            .max(0.0)
                                }
                                crate::Dimension::Px(width) => width.max(0.0),
                                crate::Dimension::Percent(_) => available_widths
                                    .get(&tnode)
                                    .copied()
                                    .unwrap_or_else(|| {
                                        reliable_table_available_width(
                                            tree,
                                            dom,
                                            &styles,
                                            initial_cb_width,
                                        )
                                        .unwrap_or(initial_cb_width)
                                    }),
                                _ => continue,
                            };
                            let interior_spacing =
                                horizontal_spacing * ncols.saturating_sub(1) as f32;
                            let target =
                                (used_outer - inline_outer_edges - interior_spacing).max(0.0);
                            let widths = distribute_fixed_table_columns(target, columns);
                            let required_outer = widths.iter().sum::<f32>()
                                + inline_outer_edges
                                + interior_spacing;
                            used_outer = used_outer.max(required_outer);
                            let used_declaration =
                                if table_style.box_sizing == crate::BoxSizing::ContentBox {
                                    (used_outer - inline_outer_edges).max(0.0)
                                } else {
                                    used_outer
                                };
                            if let Ok(cur) = taffy_tree.style(tnode) {
                                let mut fixed_style = cur.clone();
                                fixed_style.size.width = length(used_declaration);
                                fixed_style.grid_template_columns = widths
                                    .iter()
                                    .map(|width| {
                                        taffy::GridTemplateComponent::Single(taffy::MinMax {
                                            min: taffy::MinTrackSizingFunction::length(*width),
                                            max: taffy::MaxTrackSizingFunction::length(*width),
                                        })
                                    })
                                    .collect();
                                let _ = taffy_tree.set_style(tnode, fixed_style);
                            }
                            continue;
                        }
                        // A percentage-width table resolves against its container, so
                        // leave taffy's percentage handling in place.
                        let width_style = table_style.width;
                        if matches!(width_style, crate::Dimension::Percent(_)) {
                            continue;
                        }
                        let min_c = {
                            let _ = taffy_tree.compute_layout_with_measure(
                                tnode,
                                taffy::Size {
                                    width: taffy::AvailableSpace::MinContent,
                                    height: taffy::AvailableSpace::MaxContent,
                                },
                                &mut measure,
                            );
                            taffy_tree
                                .layout(tnode)
                                .map(|l| l.size.width)
                                .unwrap_or(0.0)
                        };
                        // A table can never be narrower than an unshrinkable
                        // fixed-width descendant. A Wikipedia infobox holds a
                        // ~267px image montage and a ~250px geologic-timeline
                        // widget, each with an explicit px width; taffy's grid
                        // min-content does not surface such a deep descendant's
                        // definite width up through the intervening flex/absolute
                        // boxes, leaving the table too narrow so those widgets
                        // overflow it. Floor min-content by the widest fixed child,
                        // matching CSS (a table's min-content is at least any
                        // fixed-width content it contains).
                        let min_c = min_c.max(
                            max_definite_table_content_width(tree, dom, &styles).unwrap_or(0.0),
                        );
                        let max_c = {
                            let _ = taffy_tree.compute_layout_with_measure(
                                tnode,
                                taffy::Size {
                                    width: taffy::AvailableSpace::MaxContent,
                                    height: taffy::AvailableSpace::MaxContent,
                                },
                                &mut measure,
                            );
                            taffy_tree
                                .layout(tnode)
                                .map(|l| l.size.width)
                                .unwrap_or(0.0)
                        };
                        let inline_outer_edges = table_inline_outer_edges(table_style);
                        let preferred_outer = match width_style {
                            crate::Dimension::Px(w)
                                if table_style.box_sizing == crate::BoxSizing::ContentBox =>
                            {
                                w + inline_outer_edges
                            }
                            crate::Dimension::Px(w) => w,
                            _ => max_c,
                        };
                        let available_outer = available_widths
                            .get(&tnode)
                            .copied()
                            .or_else(|| {
                                reliable_table_available_width(
                                    tree,
                                    dom,
                                    &styles,
                                    initial_cb_width,
                                )
                            })
                            .unwrap_or(initial_cb_width);
                        let mut used_outer = match width_style {
                            // A definite table width is not clamped to its
                            // containing block. Like other fixed boxes it may
                            // overflow, while min-content can still make it wider.
                            crate::Dimension::Px(_) => preferred_outer.max(min_c),
                            _ => preferred_outer.max(min_c).min(available_outer.max(min_c)),
                        };
                        let mut used_declaration =
                            if table_style.box_sizing == crate::BoxSizing::ContentBox {
                                (used_outer - inline_outer_edges).max(0.0)
                            } else {
                                used_outer
                            };
                        // Distribute the used track space proportionally between
                        // each column's own min-content and max-content width, the
                        // way CSS tables do, instead of letting the grid hand every
                        // auto track an equal share of the surplus (which over-widens
                        // narrow label columns and starves wide prose columns).
                        let cells: Vec<(taffy::NodeId, usize, usize)> = taffy_tree
                            .children(tnode)
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|cell| {
                                let gc = taffy_tree.style(cell).ok()?.grid_column.clone();
                                let col = match gc.start {
                                    GridPlacement::Line(l) => (l.as_i16().max(1) as usize) - 1,
                                    _ => return None,
                                };
                                let span = match gc.end {
                                    GridPlacement::Span(s) => (s as usize).max(1),
                                    _ => 1,
                                };
                                Some((cell, col, span))
                            })
                            .collect();
                        let ncols = taffy_tree
                            .style(tnode)
                            .map(|s| s.grid_template_columns.len())
                            .unwrap_or(0);
                        // Bound the extra per-cell measurement work; a pathologically
                        // large table keeps the grid's equal-share sizing (still renders).
                        if ncols > 0 && cells.len() <= 4096 {
                            // Measure every cell's own min-content and max-content width
                            // in isolation (the cell node is a valid subtree root; its
                            // grid placement only matters to its parent, so it lays out
                            // as a plain box here).
                            let mut measured: Vec<(usize, usize, f32, f32)> =
                                Vec::with_capacity(cells.len());
                            for (cell, col, span) in &cells {
                                let _ = taffy_tree.compute_layout_with_measure(
                                    *cell,
                                    taffy::Size {
                                        width: taffy::AvailableSpace::MinContent,
                                        height: taffy::AvailableSpace::MaxContent,
                                    },
                                    &mut measure,
                                );
                                let cmin = taffy_tree
                                    .layout(*cell)
                                    .map(|l| l.size.width)
                                    .unwrap_or(0.0);
                                let _ = taffy_tree.compute_layout_with_measure(
                                    *cell,
                                    taffy::Size {
                                        width: taffy::AvailableSpace::MaxContent,
                                        height: taffy::AvailableSpace::MaxContent,
                                    },
                                    &mut measure,
                                );
                                let cmax = taffy_tree
                                    .layout(*cell)
                                    .map(|l| l.size.width)
                                    .unwrap_or(0.0);
                                measured.push((*col, *span, cmin, cmax.max(cmin)));
                            }
                            let mut col_min = vec![0.0f32; ncols];
                            let mut col_max = vec![0.0f32; ncols];
                            // Pass 1: single-column cells set each column's floor.
                            for &(col, span, cmin, cmax) in &measured {
                                if span == 1 && col < ncols {
                                    col_min[col] = col_min[col].max(cmin);
                                    col_max[col] = col_max[col].max(cmax);
                                }
                            }
                            // Pass 2: a spanning cell that needs more than its columns
                            // currently give grows them, splitting the shortfall evenly
                            // across the spanned columns (keeps colspan lining up).
                            for &(col, span, cmin, cmax) in &measured {
                                if span <= 1 || col >= ncols {
                                    continue;
                                }
                                let end = (col + span).min(ncols);
                                let n = (end - col) as f32;
                                let cur_min: f32 = col_min[col..end].iter().sum();
                                if cmin > cur_min {
                                    let add = (cmin - cur_min) / n;
                                    for w in &mut col_min[col..end] {
                                        *w += add;
                                    }
                                }
                                let cur_max: f32 = col_max[col..end].iter().sum();
                                if cmax > cur_max {
                                    let add = (cmax - cur_max) / n;
                                    for w in &mut col_max[col..end] {
                                        *w += add;
                                    }
                                }
                            }
                            for j in 0..ncols {
                                if col_max[j] < col_min[j] {
                                    col_max[j] = col_min[j];
                                }
                            }
                            // Track space is the final table border box minus its
                            // actual border/padding, the two outer spacing bands,
                            // and the spacing between columns. Inferring this from
                            // min-content is wrong when a specified cell/column
                            // width already inflated that measurement: the fixed
                            // width gets counted as "overhead", starving later
                            // auto columns and forcing avoidable text wrapping.
                            let (horizontal_spacing, _) = table_spacing(table_style);
                            let interior_spacing =
                                horizontal_spacing * ncols.saturating_sub(1) as f32;
                            // A specified length on a cell/column is a preferred
                            // contribution, not a min-content floor. Preserve the
                            // content minimum and raise only the preferred width so
                            // a constrained table can interpolate below the hint.
                            let specified_columns = ifc_items.table_cols.get(&tnode);
                            let fixed: Vec<Option<f32>> = (0..ncols)
                                .map(|index| {
                                    specified_columns
                                        .and_then(|(px, _)| px.get(index))
                                        .copied()
                                        .flatten()
                                })
                                .collect();
                            let percentages: Vec<Option<f32>> = (0..ncols)
                                .map(|index| {
                                    specified_columns
                                        .and_then(|(_, pct)| pct.get(index))
                                        .copied()
                                        .flatten()
                                })
                                .collect();
                            for j in 0..ncols {
                                if let Some(width) = fixed[j] {
                                    col_max[j] = width.max(col_min[j]);
                                }
                            }
                            if !matches!(width_style, crate::Dimension::Px(_)) {
                                let percentage_floor =
                                    auto_table_percentage_intrinsic_floor(&col_min, &percentages);
                                let percentage_outer =
                                    percentage_floor + inline_outer_edges + interior_spacing;
                                used_outer = used_outer
                                    .max(percentage_outer)
                                    .min(available_outer.max(min_c));
                                used_declaration =
                                    if table_style.box_sizing == crate::BoxSizing::ContentBox {
                                        (used_outer - inline_outer_edges).max(0.0)
                                    } else {
                                        used_outer
                                    };
                            }
                            let target =
                                (used_outer - inline_outer_edges - interior_spacing).max(0.0);
                            let widths = distribute_auto_table_columns(
                                target,
                                &col_min,
                                &col_max,
                                &fixed,
                                &percentages,
                            );
                            if let Ok(cur) = taffy_tree.style(tnode) {
                                let mut s = cur.clone();
                                s.size.width = length(used_declaration);
                                s.grid_template_columns = widths
                                    .iter()
                                    .map(|w| {
                                        taffy::GridTemplateComponent::Single(taffy::MinMax {
                                            min: taffy::MinTrackSizingFunction::length(*w),
                                            max: taffy::MaxTrackSizingFunction::length(*w),
                                        })
                                    })
                                    .collect();
                                let _ = taffy_tree.set_style(tnode, s);
                            }
                        } else if let Ok(cur) = taffy_tree.style(tnode) {
                            let mut s = cur.clone();
                            s.size.width = length(used_declaration);
                            let _ = taffy_tree.set_style(tnode, s);
                        }
                    }
                    table_index = group_end;
                }

                let _ = taffy_tree.compute_layout_with_measure(taffy_root, available, &mut measure);
                if deferred_cyclic_inline_sizes.is_empty()
                    && apply_fit_content_widths(
                        &mut taffy_tree,
                        &id_map,
                        &styles,
                        initial_cb_width,
                        |tree, node, width| {
                            tree.compute_layout_with_measure(
                                node,
                                taffy::Size {
                                    width,
                                    height: taffy::AvailableSpace::MaxContent,
                                },
                                &mut measure,
                            )
                            .ok()?;
                            tree.layout(node).ok().map(|layout| layout.size.width)
                        },
                    )
                {
                    let _ =
                        taffy_tree.compute_layout_with_measure(taffy_root, available, &mut measure);
                }
                if resolve_atomic_percentage_heights(
                    tree,
                    &mut taffy_tree,
                    taffy_root,
                    &id_map,
                    &styles,
                    &definite_height_nodes,
                ) {
                    let _ =
                        taffy_tree.compute_layout_with_measure(taffy_root, available, &mut measure);
                }
                if repair_intrinsic_column_flex_negative_margins(&mut taffy_tree, &id_map, &styles)
                {
                    let _ =
                        taffy_tree.compute_layout_with_measure(taffy_root, available, &mut measure);
                }
                let _ = resolve_deferred_flex_inline_sizes(
                    tree,
                    &mut taffy_tree,
                    &id_map,
                    &mut styles,
                    &deferred_cyclic_inline_sizes,
                    root_fs,
                    vw,
                    vh,
                    |tree, resolved_styles, phase| match phase {
                        DeferredFlexReflowPhase::Layout => {
                            let _ = tree.compute_layout_with_measure(
                                taffy_root,
                                available,
                                &mut measure,
                            );
                        }
                        DeferredFlexReflowPhase::FitContent => {
                            if apply_fit_content_widths(
                                tree,
                                &id_map,
                                resolved_styles,
                                initial_cb_width,
                                |tree, node, width| {
                                    tree.compute_layout_with_measure(
                                        node,
                                        taffy::Size {
                                            width,
                                            height: taffy::AvailableSpace::MaxContent,
                                        },
                                        &mut measure,
                                    )
                                    .ok()?;
                                    tree.layout(node).ok().map(|layout| layout.size.width)
                                },
                            ) {
                                let _ = tree.compute_layout_with_measure(
                                    taffy_root,
                                    available,
                                    &mut measure,
                                );
                            }
                        }
                    }
                );
                if apply_multicol_balance(&mut taffy_tree, &ifc_items.multicol) {
                    let _ =
                        taffy_tree.compute_layout_with_measure(taffy_root, available, &mut measure);
                }
                if apply_float_continuations(tree, &mut taffy_tree, &id_map, &styles, &ifc_items) {
                    let _ =
                        taffy_tree.compute_layout_with_measure(taffy_root, available, &mut measure);
                }
                if apply_table_row_geometry(&mut taffy_tree, &id_map, &styles, &ifc_items) {
                    let _ =
                        taffy_tree.compute_layout_with_measure(taffy_root, available, &mut measure);
                }
                if apply_full_span_column_subgrids(
                    tree,
                    &mut taffy_tree,
                    &id_map,
                    &styles,
                    |tree, node| {
                        tree.compute_layout_with_measure(
                            node,
                            taffy::Size {
                                width: taffy::AvailableSpace::MaxContent,
                                height: taffy::AvailableSpace::MaxContent,
                            },
                            &mut measure,
                        )
                        .ok()?;
                        tree.layout(node).ok().map(|layout| layout.size.width)
                    },
                ) {
                    let _ =
                        taffy_tree.compute_layout_with_measure(taffy_root, available, &mut measure);
                }
                if apply_table_cell_block_alignment(tree, &mut taffy_tree, &id_map, &styles) {
                    let _ =
                        taffy_tree.compute_layout_with_measure(taffy_root, available, &mut measure);
                }
                // A fully-auto positioned axis uses the box's static position
                // in its original formatting context. Harvest that coordinate
                // only after every intrinsic/table/fragmentation repair has
                // produced final in-flow geometry; otherwise reparenting pins
                // the box to a stale preliminary document offset and can
                // manufacture scrolling overflow. The resolver snapshots all
                // candidates before changing any parent, preserving nested
                // static-position candidates.
                if !static_position_candidates.is_empty() {
                    resolve_static_positions_and_reparent(
                        &mut taffy_tree,
                        &static_position_candidates,
                    );
                    let _ =
                        taffy_tree.compute_layout_with_measure(taffy_root, available, &mut measure);
                }
            }
            #[cfg(not(feature = "paint"))]
            {
                let _ = taffy_tree.compute_layout(taffy_root, available);
                if deferred_cyclic_inline_sizes.is_empty()
                    && apply_fit_content_widths(
                        &mut taffy_tree,
                        &id_map,
                        &styles,
                        initial_cb_width,
                        |tree, node, width| {
                            tree.compute_layout(
                                node,
                                taffy::Size {
                                    width,
                                    height: taffy::AvailableSpace::MaxContent,
                                },
                            )
                            .ok()?;
                            tree.layout(node).ok().map(|layout| layout.size.width)
                        },
                    )
                {
                    let _ = taffy_tree.compute_layout(taffy_root, available);
                }
                if resolve_atomic_percentage_heights(
                    tree,
                    &mut taffy_tree,
                    taffy_root,
                    &id_map,
                    &styles,
                    &definite_height_nodes,
                ) {
                    let _ = taffy_tree.compute_layout(taffy_root, available);
                }
                if repair_intrinsic_column_flex_negative_margins(&mut taffy_tree, &id_map, &styles)
                {
                    let _ = taffy_tree.compute_layout(taffy_root, available);
                }
                let _ = resolve_deferred_flex_inline_sizes(
                    tree,
                    &mut taffy_tree,
                    &id_map,
                    &mut styles,
                    &deferred_cyclic_inline_sizes,
                    root_fs,
                    vw,
                    vh,
                    |tree, resolved_styles, phase| match phase {
                        DeferredFlexReflowPhase::Layout => {
                            let _ = tree.compute_layout(taffy_root, available);
                        }
                        DeferredFlexReflowPhase::FitContent => {
                            if apply_fit_content_widths(
                                tree,
                                &id_map,
                                resolved_styles,
                                initial_cb_width,
                                |tree, node, width| {
                                    tree.compute_layout(
                                        node,
                                        taffy::Size {
                                            width,
                                            height: taffy::AvailableSpace::MaxContent,
                                        },
                                    )
                                    .ok()?;
                                    tree.layout(node).ok().map(|layout| layout.size.width)
                                },
                            ) {
                                let _ = tree.compute_layout(taffy_root, available);
                            }
                        }
                    }
                );
                if apply_multicol_balance(&mut taffy_tree, &ifc_items.multicol) {
                    let _ = taffy_tree.compute_layout(taffy_root, available);
                }
                if apply_float_continuations(tree, &mut taffy_tree, &id_map, &styles, &ifc_items) {
                    let _ = taffy_tree.compute_layout(taffy_root, available);
                }
                if apply_table_row_geometry(&mut taffy_tree, &id_map, &styles, &ifc_items) {
                    let _ = taffy_tree.compute_layout(taffy_root, available);
                }
                if apply_full_span_column_subgrids(
                    tree,
                    &mut taffy_tree,
                    &id_map,
                    &styles,
                    |tree, node| {
                        tree.compute_layout(
                            node,
                            taffy::Size {
                                width: taffy::AvailableSpace::MaxContent,
                                height: taffy::AvailableSpace::MaxContent,
                            },
                        )
                        .ok()?;
                        tree.layout(node).ok().map(|layout| layout.size.width)
                    },
                ) {
                    let _ = taffy_tree.compute_layout(taffy_root, available);
                }
                if apply_table_cell_block_alignment(tree, &mut taffy_tree, &id_map, &styles) {
                    let _ = taffy_tree.compute_layout(taffy_root, available);
                }
                // Keep the no-paint geometry path in the same final-position
                // contract as screenshots: static coordinates are resolved
                // after every pass that can move in-flow placeholders.
                if !static_position_candidates.is_empty() {
                    resolve_static_positions_and_reparent(
                        &mut taffy_tree,
                        &static_position_candidates,
                    );
                    let _ = taffy_tree.compute_layout(taffy_root, available);
                }
            }
            sync_resolved_percentage_padding(
                &taffy_tree,
                taffy_root,
                initial_cb_width,
                &id_map,
                &ifc_items.generated,
                &mut styles,
            );
            let generated_nodes: HashMap<taffy::NodeId, usize> = ifc_items
                .generated
                .iter()
                .enumerate()
                .map(|(index, generated)| (generated.node, index))
                .collect();
            generated_rects.resize(ifc_items.generated.len(), None);
            compute_absolute_rects(
                &taffy_tree,
                taffy_root,
                initial_cb_x,
                0.0,
                &id_map,
                &words,
                &mut rects,
                &mut text_runs,
                &mut anon_rects,
                &generated_nodes,
                &mut generated_rects,
            );
            inline_fragments = synthesize_ordinary_inline_fragments(&mut rects, &styles, &engine);
            synthesize_row_rects(tree, &mut rects);
        }
    }
    sync_positioned_pseudo_percentage_padding(&rects, &mut styles);

    let mut clip_rects = HashMap::new();
    let mut translates = HashMap::new();
    let mut transforms = HashMap::new();
    if let Some(root_id) = root {
        let root_font_size = styles
            .get(&root_id)
            .and_then(|style| style.font_size)
            .unwrap_or(16.0);
        resolve_clip_rects(
            tree,
            root_id,
            None,
            0.0,
            0.0,
            &rects,
            &styles,
            &mut clip_rects,
            &mut translates,
            crate::Affine2::IDENTITY,
            &mut transforms,
            root_font_size,
            viewport,
        );
    }

    // Pin each shaped inline context to its final content-box origin/width now
    // that layout is done, so paint draws the same line breaks it was sized for.
    #[cfg(feature = "paint")]
    for (nid, &idx) in &ifc_items.whole {
        if let (Some(rect), Some(style)) = (rects.get(nid), styles.get(nid)) {
            let mut origin = crate::inline::content_origin(rect, style);
            let cw = crate::inline::content_width(rect, style);
            // A table cell stretched taller than its text (row height from a
            // neighbor) aligns its content per vertical-align; the pure-text
            // leaf path has no inner box to align, so shift the pinned origin
            // by the leftover space instead.
            if let Some(va @ (crate::VerticalAlign::Middle | crate::VerticalAlign::Bottom)) =
                style.vertical_align
            {
                let (_, th) = engine.measure(idx, Some(cw));
                let content_h = rect.height
                    - style.padding.top
                    - style.padding.bottom
                    - style.border.top
                    - style.border.bottom;
                let free = (content_h - th).max(0.0);
                origin.1 += if va == crate::VerticalAlign::Middle {
                    free / 2.0
                } else {
                    free
                };
            }
            // The shaped text is this container's own content, so an
            // `overflow: hidden` on the container clips it (this is what keeps
            // the 1x1 "visually hidden" skip-link box from painting its text,
            // now that the text is one leaf rather than clipped word boxes).
            // Clips live in screen space; the container's own box joins the
            // chain shifted by its accumulated translate.
            let inherited = clip_rects.get(nid).cloned().flatten();
            let clip = if style.overflow_hidden {
                let (tx, ty) = translates.get(nid).copied().unwrap_or((0.0, 0.0));
                let own = OverflowClip::for_box(rect, style, tx, ty);
                Some(match inherited {
                    Some(c) => c.intersect(own),
                    None => own,
                })
            } else {
                inherited
            }
            .map(|clip| clip.viewport_rect(viewport));
            engine.finalize(idx, origin, cw, clip);
        }
    }
    // Anonymous run leaves have no DOM node and no border/padding of their
    // own: the leaf rect IS the content box. Clipping comes from the parent
    // block (its inherited chain, plus its own overflow like any child).
    #[cfg(feature = "paint")]
    for (parent, items) in &ifc_items.runs {
        let inherited = clip_rects.get(parent).cloned().flatten();
        let clip = match (styles.get(parent), rects.get(parent)) {
            (Some(style), Some(prect)) if style.overflow_hidden => {
                let (tx, ty) = translates.get(parent).copied().unwrap_or((0.0, 0.0));
                let own = OverflowClip::for_box(prect, style, tx, ty);
                Some(match inherited {
                    Some(c) => c.intersect(own),
                    None => own,
                })
            }
            _ => inherited,
        }
        .map(|clip| clip.viewport_rect(viewport));
        for &idx in items {
            if let Some(rect) = anon_rects.get(&idx) {
                engine.finalize(idx, (rect.x, rect.y), rect.width, clip);
            }
        }
    }
    // General word-fallback items are shaped with the same loaded font system
    // as full IFCs but retain one independently wrapped Taffy rect per token.
    // Their source is a real text node, so clipping/translation follows that
    // node while each item is pinned to its anonymous leaf.
    #[cfg(feature = "paint")]
    for (text_node, items) in &ifc_items.word_items {
        let clip = clip_rects
            .get(text_node)
            .cloned()
            .flatten()
            .map(|clip| clip.viewport_rect(viewport));
        for &idx in items {
            if let Some(rect) = anon_rects.get(&idx) {
                engine.finalize(idx, (rect.x, rect.y), rect.width, clip);
            }
        }
    }

    // Pure-text IFC descendants do not own Taffy nodes. Once their shared
    // buffer has its final line breaks, derive their real continuations from
    // shaping provenance and install those fragments as canonical DOM geometry.
    // The common paragraph with no nested inline owner skips both this work and
    // the second clip-tree walk.
    #[cfg(feature = "paint")]
    if engine.has_inline_owners() {
        let canonical = synthesize_shaped_inline_fragments(tree, &mut rects, &styles, &engine);
        if !canonical.is_empty() {
            let relative_offsets = canonical
                .keys()
                .filter_map(|&owner| {
                    folded_inline_relative_offset(tree, owner, &rects, &styles)
                        .filter(|offset| *offset != (0.0, 0.0))
                        .map(|offset| (owner, offset))
                })
                .collect();
            engine.set_inline_owner_offsets(&relative_offsets);
            inline_fragments.extend(canonical);
            clip_rects.clear();
            translates.clear();
            transforms.clear();
            if let Some(root_id) = root {
                let root_font_size = styles
                    .get(&root_id)
                    .and_then(|style| style.font_size)
                    .unwrap_or(16.0);
                resolve_clip_rects(
                    tree,
                    root_id,
                    None,
                    0.0,
                    0.0,
                    &rects,
                    &styles,
                    &mut clip_rects,
                    &mut translates,
                    crate::Affine2::IDENTITY,
                    &mut transforms,
                    root_font_size,
                    viewport,
                );
            }
            for (nid, &idx) in &ifc_items.whole {
                engine.set_clip(
                    idx,
                    shaped_item_clip(*nid, &rects, &styles, &clip_rects, &translates, viewport),
                );
            }
            for (parent, items) in &ifc_items.runs {
                let clip =
                    shaped_item_clip(*parent, &rects, &styles, &clip_rects, &translates, viewport);
                for &idx in items {
                    engine.set_clip(idx, clip);
                }
            }
        }
    }

    let generated_boxes = ifc_items
        .generated
        .iter()
        .zip(generated_rects)
        .filter_map(|(generated, rect)| {
            rect.map(|rect| GeneratedBox {
                host: generated.host,
                kind: generated.kind,
                rect,
            })
        })
        .collect();

    (
        DomLayout {
            rects,
            inline_fragments,
            styles,
            custom_properties,
            clip_rects,
            translates,
            transforms,
            text_runs,
            #[cfg(feature = "paint")]
            text_engine: engine,
            #[cfg(feature = "paint")]
            ifc_items: ifc_items.whole,
            #[cfg(feature = "paint")]
            run_ifc_items: ifc_items.runs,
            #[cfg(feature = "paint")]
            word_ifc_items: ifc_items.word_items,
            generated_boxes,
        },
        signature,
        query_stats,
        cascade_time,
    )
}

/// Convert Taffy's ordinary-inline line surrogate into the element's actual
/// visual fragment box.
///
/// The surrogate remains at CSS line-height so it contributes the correct
/// line advance. The fragment uses the selected face's grid-fitted ascent plus
/// descent, expanded by block-axis padding and border. Those decorations
/// protrude into the leading and never feed back into the line height.
fn synthesize_ordinary_inline_fragments(
    rects: &mut HashMap<NodeId, Rect>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    engine: &crate::inline::TextEngine,
) -> HashMap<NodeId, Vec<Rect>> {
    let mut fragments = HashMap::new();
    let mut unions = Vec::new();
    for (&id, style) in styles {
        if !style.ignores_used_box_sizes() {
            continue;
        }
        let Some(flow_rect) = rects.get(&id).copied() else {
            continue;
        };
        let font_height = engine.inline_font_box_height(style).max(0.0);
        let line_height = engine.selected_line_height(style).max(0.0);
        // Taffy's surrogate may be taller than this element's own strut when
        // a nested inline enlarges the line. Center the element's logical
        // line-height inside that allocation before removing its leading.
        let logical_top = flow_rect.y + (flow_rect.height - line_height) / 2.0;
        let fragment = Rect {
            x: flow_rect.x,
            y: logical_top + (line_height - font_height) / 2.0
                - style.padding.top
                - style.border.top,
            width: flow_rect.width,
            height: font_height
                + style.padding.top
                + style.padding.bottom
                + style.border.top
                + style.border.bottom,
        };
        fragments.insert(id, vec![fragment]);
        unions.push((id, fragment));
    }
    for (id, union) in unions {
        rects.insert(id, union);
    }
    fragments
}

#[cfg(feature = "paint")]
fn shaped_item_clip(
    owner: NodeId,
    rects: &HashMap<NodeId, Rect>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    clip_rects: &HashMap<NodeId, Option<OverflowClip>>,
    translates: &HashMap<NodeId, (f32, f32)>,
    viewport: (f32, f32),
) -> Option<Rect> {
    let inherited = clip_rects.get(&owner).cloned().flatten();
    let clip = match (styles.get(&owner), rects.get(&owner)) {
        (Some(style), Some(rect)) if style.overflow_hidden => {
            let (tx, ty) = translates.get(&owner).copied().unwrap_or((0.0, 0.0));
            let own = OverflowClip::for_box(rect, style, tx, ty);
            Some(match inherited {
                Some(clip) => clip.intersect(own),
                None => own,
            })
        }
        _ => inherited,
    };
    clip.map(|clip| clip.viewport_rect(viewport))
}

#[cfg(feature = "paint")]
fn synthesize_shaped_inline_fragments(
    tree: &DomTree,
    rects: &mut HashMap<NodeId, Rect>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    engine: &crate::inline::TextEngine,
) -> HashMap<NodeId, Vec<Rect>> {
    let mut fragments: HashMap<NodeId, Vec<((usize, usize), Rect)>> = HashMap::new();
    for shaped in engine.inline_owner_line_fragments() {
        let Some(style) = styles.get(&shaped.owner) else {
            continue;
        };
        let (ascent, descent) = engine.inline_font_box_metrics(style);
        let Some((relative_x, relative_y)) =
            folded_inline_relative_offset(tree, shaped.owner, rects, styles)
        else {
            // Keep the pre-existing Taffy surrogate geometry when an inset
            // cannot be resolved faithfully. Installing a knowingly wrong
            // shaped fragment would make the DOM geometry canonical.
            continue;
        };
        let rect = Rect {
            x: shaped.x + relative_x,
            y: shaped.baseline_y - ascent - style.padding.top - style.border.top + relative_y,
            width: shaped.width,
            height: ascent
                + descent
                + style.padding.top
                + style.padding.bottom
                + style.border.top
                + style.border.bottom,
        };
        fragments
            .entry(shaped.owner)
            .or_default()
            .push(((shaped.item_index, shaped.line_index), rect));
    }

    let mut canonical = HashMap::with_capacity(fragments.len());
    for (owner, mut pieces) in fragments {
        pieces.sort_by(|(order_a, rect_a), (order_b, rect_b)| {
            order_a
                .cmp(order_b)
                .then_with(|| rect_a.x.total_cmp(&rect_b.x))
        });
        let fragments = pieces.into_iter().map(|(_, rect)| rect).collect::<Vec<_>>();
        let mut union = fragments[0];
        for fragment in &fragments[1..] {
            let left = union.x.min(fragment.x);
            let top = union.y.min(fragment.y);
            let right = (union.x + union.width).max(fragment.x + fragment.width);
            let bottom = (union.y + union.height).max(fragment.y + fragment.height);
            union = Rect {
                x: left,
                y: top,
                width: (right - left).max(0.0),
                height: (bottom - top).max(0.0),
            };
        }
        rects.insert(owner, union);
        canonical.insert(owner, fragments);
    }
    canonical
}

#[cfg(feature = "paint")]
fn folded_inline_relative_offset(
    tree: &DomTree,
    owner: NodeId,
    rects: &HashMap<NodeId, Rect>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Option<(f32, f32)> {
    fn containing_block_size(
        tree: &DomTree,
        owner: NodeId,
        rects: &HashMap<NodeId, Rect>,
        styles: &HashMap<NodeId, crate::LayoutStyle>,
    ) -> Option<(f32, f32)> {
        let mut ancestor = rendered_parent(tree, owner);
        while let Some(id) = ancestor {
            let style = styles.get(&id)?;
            if !style.ignores_used_box_sizes() && !style.display_contents {
                let rect = rects.get(&id)?;
                return Some((
                    (rect.width
                        - style.border.left
                        - style.border.right
                        - style.padding.left
                        - style.padding.right)
                        .max(0.0),
                    (rect.height
                        - style.border.top
                        - style.border.bottom
                        - style.padding.top
                        - style.padding.bottom)
                        .max(0.0),
                ));
            }
            ancestor = rendered_parent(tree, id);
        }
        None
    }

    let mut offset = (0.0, 0.0);
    let mut current = Some(owner);
    while let Some(id) = current {
        let Some(style) = styles.get(&id) else {
            return None;
        };
        if !style.ignores_used_box_sizes() {
            break;
        }
        if style.position == Some(taffy::Position::Relative) && !style.position_sticky {
            let basis = containing_block_size(tree, id, rects, styles);
            let resolve = |value: Option<crate::Dimension>, horizontal: bool| match value {
                None | Some(crate::Dimension::Auto) => Ok(None),
                Some(crate::Dimension::Px(value)) if value.is_finite() => Ok(Some(value)),
                Some(crate::Dimension::Percent(value)) if value.is_finite() => basis
                    .map(|size| Some(value * if horizontal { size.0 } else { size.1 }))
                    .ok_or(()),
                _ => Err(()),
            };
            // Failed calc()/var() resolution is represented by an expression
            // with no corresponding used Dimension. Do not reinterpret it as
            // `auto` merely because this sidecar runs after style resolution.
            for index in 0..4 {
                if style.inset_expressions[index].is_some() && style.inset[index].is_none() {
                    return None;
                }
            }
            let left = resolve(style.inset[3], true).ok()?;
            let right = resolve(style.inset[1], true).ok()?;
            let top = resolve(style.inset[0], false).ok()?;
            let bottom = resolve(style.inset[2], false).ok()?;
            if let Some(left) = left {
                offset.0 += left;
            } else if let Some(right) = right {
                offset.0 -= right;
            }
            if let Some(top) = top {
                offset.1 += top;
            } else if let Some(bottom) = bottom {
                offset.1 -= bottom;
            }
        }
        current = rendered_parent(tree, id);
    }
    Some(offset)
}

fn container_snapshot(tree: &DomTree, layout: &DomLayout) -> crate::css::ContainerSnapshot {
    let root_font_size = tree
        .descendants(tree.document())
        .into_iter()
        .find(|id| tree.get_node(*id).is_some_and(|node| node.is_element()))
        .and_then(|id| layout.styles.get(&id))
        .and_then(|style| style.font_size)
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or(16.0);
    let mut snapshot = crate::css::ContainerSnapshot {
        root_font_size,
        ..Default::default()
    };
    for (&id, style) in &layout.styles {
        let available_type = if style.display_contents {
            crate::ContainerType::Normal
        } else {
            effective_container_type(style)
        };
        if (style.container_type == crate::ContainerType::Normal
            && style.container_names.is_empty())
            || style.display == crate::Display::None
        {
            continue;
        }
        let rect = layout.rects.get(&id);
        // An inline box that computes to `container-type` is still the
        // nearest matching query container even when size containment cannot
        // apply to it. Inline layout may flatten that box and omit a DOM rect,
        // but dropping it from the snapshot would incorrectly let the query
        // fall through to a farther ancestor. Its queried axes are
        // unavailable, so the placeholder geometry is intentionally unused.
        if rect.is_none() && available_type != crate::ContainerType::Normal {
            continue;
        }
        let horizontal_edges =
            style.border.left + style.border.right + style.padding.left + style.padding.right;
        let vertical_edges =
            style.border.top + style.border.bottom + style.padding.top + style.padding.bottom;
        snapshot.boxes.insert(
            id,
            crate::css::ContainerBox {
                container_type: style.container_type,
                available_type,
                names: style.container_names.clone(),
                content_width: rect
                    .map(|rect| (rect.width - horizontal_edges).max(0.0))
                    .unwrap_or(0.0),
                content_height: rect
                    .map(|rect| (rect.height - vertical_edges).max(0.0))
                    .unwrap_or(0.0),
                font_size: style
                    .font_size
                    .filter(|size| size.is_finite() && *size > 0.0)
                    .unwrap_or(16.0),
            },
        );
    }
    snapshot
}

/// HTML table auto-layout approximation: for each `<tr>`, the last `<td>`/
/// `<th>` child with no explicit width absorbs the row's leftover space. Real
/// browsers run a genuine column-width-negotiation algorithm across every row
/// in a table; giving `flex_grow` to *every* auto-width cell approximates that
/// badly; multiple growing siblings compete and the split becomes sensitive to
/// each cell's own content size, which looks like near-random drift row to
/// row. Growing only the trailing auto cell matches the extremely common
/// layout intent (fixed-size leading cells, one expanding trailing cell) and
/// leaves the others shrink-to-fit, matching a shrink-to-fit table exactly
/// when there is no surplus width to distribute in the first place.
fn grow_trailing_auto_cells(tree: &DomTree, styles: &mut HashMap<NodeId, crate::LayoutStyle>) {
    let is_tag = |id: NodeId, tags: &[&str]| -> bool {
        match tree
            .get_node(id)
            .and_then(|n| n.as_element().map(|e| e.local.to_string()))
        {
            Some(local) => tags.contains(&local.as_str()),
            None => false,
        }
    };
    for tr in rendered_descendants(tree, tree.document()) {
        if !is_tag(tr, &["tr"]) {
            continue;
        }
        let last_auto_cell = tree.children(tr).into_iter().rev().find(|&cid| {
            is_tag(cid, &["td", "th"])
                && styles
                    .get(&cid)
                    .map(|s| s.width == crate::Dimension::Auto && s.flex_grow.is_none())
                    .unwrap_or(false)
        });
        if let Some(cid) = last_auto_cell {
            if let Some(style) = styles.get_mut(&cid) {
                style.flex_grow = Some(1.0);
            }
        }
    }
}

/// `border-spacing` (from CSS or the `cellspacing` attribute) is set on a
/// `<table>`, but taffy has no native table display mode: our table is a
/// column-flex stack of row-flex `<tr>`s, so the gap that separates cells has
/// to live on each row individually. Distribute the table's border-spacing
/// down as the table's own row gap (space between stacked `<tr>`s) and every
/// descendant `<tr>`'s column gap (space between cells within a row), without
/// crossing into a nested `<table>`'s own scope.
fn propagate_border_spacing(tree: &DomTree, styles: &mut HashMap<NodeId, crate::LayoutStyle>) {
    fn local_name(tree: &DomTree, id: NodeId) -> Option<String> {
        tree.get_node(id)
            .and_then(|n| n.as_element().map(|e| e.local.to_string()))
    }

    fn apply_to_rows(
        tree: &DomTree,
        id: NodeId,
        h: f32,
        v: f32,
        styles: &mut HashMap<NodeId, crate::LayoutStyle>,
    ) {
        for cid in tree.children(id) {
            if local_name(tree, cid).as_deref() == Some("table") {
                continue;
            }
            if local_name(tree, cid).as_deref() == Some("tr") {
                if let Some(s) = styles.get_mut(&cid) {
                    s.column_gap = Some(h);
                    s.row_gap = Some(v);
                    s.column_gap_expression = None;
                    s.row_gap_expression = None;
                }
            }
            apply_to_rows(tree, cid, h, v, styles);
        }
    }

    for id in rendered_descendants(tree, tree.document()) {
        if local_name(tree, id).as_deref() != Some("table") {
            continue;
        }
        let Some(table_style) = styles.get(&id) else {
            continue;
        };
        let (h, v) = table_spacing(table_style);
        if let Some(s) = styles.get_mut(&id) {
            s.row_gap = Some(v);
            s.row_gap_expression = None;
        }
        apply_to_rows(tree, id, h, v, styles);
    }
}

#[derive(Clone, Copy)]
enum EffectiveGridChild {
    Dom(NodeId),
    Generated {
        host: NodeId,
        kind: GeneratedBoxKind,
    },
}

fn collect_effective_grid_children(
    tree: &DomTree,
    children: &[NodeId],
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    out: &mut Vec<EffectiveGridChild>,
) {
    for &child in children {
        let transparent = styles
            .get(&child)
            .is_some_and(|style| style.display_contents && style.display != crate::Display::None);
        if !transparent {
            out.push(EffectiveGridChild::Dom(child));
            continue;
        }
        if styles
            .get(&child)
            .and_then(|style| style.before_pseudo.as_ref())
            .is_some()
        {
            out.push(EffectiveGridChild::Generated {
                host: child,
                kind: GeneratedBoxKind::Before,
            });
        }
        collect_effective_grid_children(tree, &rendered_children(tree, child), styles, out);
        if styles
            .get(&child)
            .and_then(|style| style.after_pseudo.as_ref())
            .is_some()
        {
            out.push(EffectiveGridChild::Generated {
                host: child,
                kind: GeneratedBoxKind::After,
            });
        }
    }
}

fn effective_grid_child_style<'a>(
    child: EffectiveGridChild,
    styles: &'a HashMap<NodeId, crate::LayoutStyle>,
) -> Option<&'a crate::LayoutStyle> {
    match child {
        EffectiveGridChild::Dom(node) => styles.get(&node),
        EffectiveGridChild::Generated { host, kind } => {
            let host = styles.get(&host)?;
            match kind {
                GeneratedBoxKind::Before => host.before_pseudo.as_deref(),
                GeneratedBoxKind::After => host.after_pseudo.as_deref(),
            }
        }
    }
}

fn effective_grid_child_style_mut<'a>(
    child: EffectiveGridChild,
    styles: &'a mut HashMap<NodeId, crate::LayoutStyle>,
) -> Option<&'a mut crate::LayoutStyle> {
    match child {
        EffectiveGridChild::Dom(node) => styles.get_mut(&node),
        EffectiveGridChild::Generated { host, kind } => {
            let host = styles.get_mut(&host)?;
            match kind {
                GeneratedBoxKind::Before => host.before_pseudo.as_deref_mut(),
                GeneratedBoxKind::After => host.after_pseudo.as_deref_mut(),
            }
        }
    }
}

/// Walk the tree; for each `display: grid` element that declares
/// `grid-template-areas`, resolve each box child's `grid-area` name to a taffy
/// line placement. `display:contents` wrappers are transparent here for the
/// same reason they are transparent when the Taffy child list is built.
fn resolve_grid_areas(
    tree: &DomTree,
    root: NodeId,
    styles: &mut HashMap<NodeId, crate::LayoutStyle>,
) {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        for cid in tree.children(id) {
            stack.push(cid);
        }
        let (areas, col_lines, row_lines) = match styles.get(&id) {
            Some(s) if s.display == crate::Display::Grid => (
                s.grid_areas.clone().filter(|a| !a.is_empty()),
                s.grid_col_line_names.clone(),
                s.grid_row_line_names.clone(),
            ),
            _ => continue,
        };
        if areas.is_none() && col_lines.is_none() && row_lines.is_none() {
            continue;
        }
        let mut grid_children = Vec::new();
        collect_effective_grid_children(
            tree,
            &rendered_children(tree, id),
            styles,
            &mut grid_children,
        );

        if let Some(areas) = &areas {
            // name -> (row_start, row_end, col_start, col_end) in 0-based track indices.
            let mut spans: HashMap<String, (usize, usize, usize, usize)> = HashMap::new();
            for (r, row) in areas.iter().enumerate() {
                for (c, name) in row.iter().enumerate() {
                    if name == "." {
                        continue;
                    }
                    spans
                        .entry(name.clone())
                        .and_modify(|s| {
                            s.0 = s.0.min(r);
                            s.1 = s.1.max(r);
                            s.2 = s.2.min(c);
                            s.3 = s.3.max(c);
                        })
                        .or_insert((r, r, c, c));
                }
            }

            for child in grid_children.iter().copied() {
                let Some(cstyle) = effective_grid_child_style_mut(child, styles) else {
                    continue;
                };
                let Some(name) = cstyle.grid_area_name.clone() else {
                    continue;
                };
                if let Some(&(r0, r1, c0, c1)) = spans.get(&name) {
                    use taffy::style_helpers::line;
                    cstyle.grid_row = Some(taffy::Line {
                        start: line((r0 + 1) as i16),
                        end: line((r1 + 2) as i16),
                    });
                    cstyle.grid_column = Some(taffy::Line {
                        start: line((c0 + 1) as i16),
                        end: line((c1 + 2) as i16),
                    });
                }
            }
        }

        // Named grid lines: resolve children placed with `grid-column`/`grid-row`
        // values that reference a line name against this container's maps.
        if col_lines.is_some() || row_lines.is_some() {
            for child in grid_children.iter().copied() {
                let Some(cstyle) = effective_grid_child_style_mut(child, styles) else {
                    continue;
                };
                if let (Some(raw), Some(map)) = (cstyle.grid_column_raw.clone(), &col_lines) {
                    if let Some(l) = resolve_named_placement(&raw, map) {
                        cstyle.grid_column = Some(l);
                    }
                }
                if let (Some(raw), Some(map)) = (cstyle.grid_row_raw.clone(), &row_lines) {
                    if let Some(l) = resolve_named_placement(&raw, map) {
                        cstyle.grid_row = Some(l);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ColumnSubgridWrapper {
    node: taffy::NodeId,
    gap: f32,
    start_mbp: f32,
    end_mbp: f32,
}

#[derive(Clone, Copy)]
struct ColumnSubgridLeaf {
    node: taffy::NodeId,
    dom: NodeId,
    column: usize,
    gap: f32,
    start_mbp: f32,
    end_mbp: f32,
}

struct ColumnSubgridPlan {
    parent: taffy::NodeId,
    track_count: usize,
    parent_gap: f32,
    wrappers: Vec<ColumnSubgridWrapper>,
    leaves: Vec<ColumnSubgridLeaf>,
}

fn is_full_span_column_subgrid(style: &crate::LayoutStyle) -> bool {
    if style.display != crate::Display::Grid
        || !style.grid_template_columns_subgrid
        || style.overflow_hidden
        || style.position == Some(taffy::Position::Absolute)
        || style.width != crate::Dimension::Auto
        || style.justify_self.is_some_and(|value| {
            value.resolve_normal(taffy::AlignSelf::STRETCH) != taffy::AlignSelf::STRETCH
        })
        || style.margin_auto[1]
        || style.margin_auto[3]
    {
        return false;
    }
    let Some(line) = &style.grid_column else {
        return false;
    };
    matches!(
        (&line.start, &line.end),
        (taffy::GridPlacement::Line(start), taffy::GridPlacement::Line(end))
            if start.as_i16() == 1 && end.as_i16() == -1
    )
}

/// Whether an item is wholly eligible for ordinary grid auto-placement.
///
/// The style model retains an explicit `grid-area:auto` as `Some(Line {
/// Auto, Auto })`, while an omitted placement stays `None`. Both represent
/// the same indefinite row and column spans to the grid placement algorithm.
/// Raw named placements are never auto: an unresolved name must not enter the
/// bounded subgrid reduction merely because it has no numeric `Line` yet.
fn has_only_auto_grid_placement(style: &crate::LayoutStyle) -> bool {
    if style.grid_column_raw.is_some() || style.grid_row_raw.is_some() {
        return false;
    }
    let axis_is_auto = |line: Option<&taffy::Line<taffy::GridPlacement>>| {
        line.is_none_or(|line| {
            matches!(line.start, taffy::GridPlacement::Auto)
                && matches!(line.end, taffy::GridPlacement::Auto)
        })
    };
    axis_is_auto(style.grid_column.as_ref()) && axis_is_auto(style.grid_row.as_ref())
}

/// Collect the deliberately bounded Grid Level 2 subset used by broad
/// "aligned rows" components: a full-span column subgrid, optionally nested
/// through more full-span column subgrids, whose final children auto-place one
/// per inherited column. Partial spans, explicit leaf placement, independent
/// formatting contexts, and authored inline sizing are left on the existing
/// fallback rather than being represented inaccurately.
fn collect_column_subgrid_descendants(
    tree: &DomTree,
    dom: NodeId,
    track_count: usize,
    taffy_by_dom: &HashMap<NodeId, taffy::NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    start_mbp: f32,
    end_mbp: f32,
    depth: usize,
    wrappers: &mut Vec<ColumnSubgridWrapper>,
    leaves: &mut Vec<ColumnSubgridLeaf>,
) -> bool {
    if depth > 8 {
        return false;
    }
    let Some(style) = styles.get(&dom) else {
        return false;
    };
    let Some(&node) = taffy_by_dom.get(&dom) else {
        return false;
    };
    let start_mbp = start_mbp + style.margin.left + style.border.left + style.padding.left;
    let end_mbp = end_mbp + style.margin.right + style.border.right + style.padding.right;
    let gap = style.column_gap.unwrap_or(0.0);
    wrappers.push(ColumnSubgridWrapper {
        node,
        gap,
        start_mbp,
        end_mbp,
    });

    let children: Vec<NodeId> = tree
        .children(dom)
        .into_iter()
        .filter(|child| {
            styles
                .get(child)
                .map(|style| style.display != crate::Display::None)
                .unwrap_or(false)
        })
        .collect();
    if children.is_empty() {
        return false;
    }
    let nested = children
        .iter()
        .filter(|child| styles.get(child).is_some_and(is_full_span_column_subgrid))
        .count();
    if nested > 0 {
        if nested != children.len() {
            return false;
        }
        return children.into_iter().all(|child| {
            collect_column_subgrid_descendants(
                tree,
                child,
                track_count,
                taffy_by_dom,
                styles,
                start_mbp,
                end_mbp,
                depth + 1,
                wrappers,
                leaves,
            )
        });
    }

    let flow = style.grid_auto_flow.unwrap_or(taffy::GridAutoFlow::Row);
    if !matches!(
        flow,
        taffy::GridAutoFlow::Row | taffy::GridAutoFlow::RowDense
    ) {
        return false;
    }
    for (index, child) in children.into_iter().enumerate() {
        let Some(child_style) = styles.get(&child) else {
            return false;
        };
        // This first subset intentionally excludes spanning and explicitly
        // placed items. It is the common data-row/card-row pattern and keeps
        // each descendant contribution attributable to one ancestor track.
        // Explicit `grid-area:auto` remains ordinary auto-placement in both
        // axes and is therefore equivalent to omitting all placement values.
        if !has_only_auto_grid_placement(child_style)
            || child_style.position == Some(taffy::Position::Absolute)
            || child_style.margin_auto[1]
            || child_style.margin_auto[3]
        {
            return false;
        }
        let Some(&node) = taffy_by_dom.get(&child) else {
            return false;
        };
        leaves.push(ColumnSubgridLeaf {
            node,
            dom: child,
            column: index % track_count,
            gap,
            start_mbp,
            end_mbp,
        });
    }
    true
}

fn fixed_grid_tracks(widths: &[f32]) -> Vec<taffy::GridTemplateComponent<String>> {
    widths
        .iter()
        .map(|width| {
            taffy::GridTemplateComponent::Single(taffy::MinMax {
                min: taffy::MinTrackSizingFunction::length((*width).max(0.0)),
                max: taffy::MaxTrackSizingFunction::length((*width).max(0.0)),
            })
        })
        .collect()
}

/// Resolve a safe full-span column-subgrid subset in two passes.
///
/// Gecko first collects subgrid descendants into the nearest non-subgridded
/// ancestor's track sizing, then copies that ancestor's *used* track sizes
/// down the chain. Its descendant contributions add accumulated edge
/// margin/border/padding and center a custom subgrid gap over the ancestor
/// gap. Taffy has no subgrid primitive, so we reproduce those same operations
/// only for definite, all-auto, single-span rows. Once every max-content
/// growth limit fits, `justify-content:normal` stretches all auto tracks by an
/// equal share; narrower/cyclic cases decline this fast path.
///
/// The current style model does not expose orthogonal writing modes. RTL grids
/// are deliberately excluded because this physical-column reduction has not
/// yet been mirrored into logical track coordinates. Percentage-width parents are
/// accepted only after the preliminary layout produced finite, explicit used
/// track sizes; that resolved track sum is the definite basis for pass two.
fn apply_full_span_column_subgrids<F>(
    tree: &DomTree,
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    mut measure_max_content: F,
) -> bool
where
    F: FnMut(&mut TaffyTree<usize>, taffy::NodeId) -> Option<f32>,
{
    let taffy_by_dom: HashMap<NodeId, taffy::NodeId> =
        id_map.iter().map(|(taffy, dom)| (*dom, *taffy)).collect();
    let mut plans = Vec::new();

    for (&dom, style) in styles {
        if style.display != crate::Display::Grid
            || style.direction == Some(taffy::Direction::Rtl)
            || style.grid_template_columns_subgrid
            || !matches!(
                style.width,
                crate::Dimension::Px(_) | crate::Dimension::Percent(_)
            )
            || style
                .justify_content
                .is_some_and(|value| value != taffy::JustifyContent::STRETCH)
        {
            continue;
        }
        let all_auto = !style.grid_template_columns.is_empty()
            && style.grid_template_columns.iter().all(|track| {
                matches!(
                    track,
                    taffy::GridTemplateComponent::Single(size)
                        if size.min.is_auto() && size.max.is_auto()
                )
            });
        if !all_auto || style.grid_template_columns.len() > 32 {
            continue;
        }
        let track_count = style.grid_template_columns.len();
        let direct: Vec<NodeId> = tree
            .children(dom)
            .into_iter()
            .filter(|child| {
                styles
                    .get(child)
                    .is_some_and(|style| style.display != crate::Display::None)
            })
            .collect();
        if direct.is_empty()
            || !direct
                .iter()
                .all(|child| styles.get(child).is_some_and(is_full_span_column_subgrid))
        {
            continue;
        }
        let Some(&parent) = taffy_by_dom.get(&dom) else {
            continue;
        };
        let mut wrappers = Vec::new();
        let mut leaves = Vec::new();
        if !direct.into_iter().all(|child| {
            collect_column_subgrid_descendants(
                tree,
                child,
                track_count,
                &taffy_by_dom,
                styles,
                0.0,
                0.0,
                0,
                &mut wrappers,
                &mut leaves,
            )
        }) || leaves.is_empty()
        {
            continue;
        }
        plans.push(ColumnSubgridPlan {
            parent,
            track_count,
            parent_gap: style.column_gap.unwrap_or(0.0),
            wrappers,
            leaves,
        });
    }

    let mut changed = false;
    for plan in plans {
        let target_track_sum = match taffy_tree.detailed_layout_info(plan.parent) {
            taffy::tree::DetailedLayoutInfo::Grid(info)
                if info.columns.negative_implicit_tracks == 0
                    && info.columns.positive_implicit_tracks == 0
                    && info.columns.explicit_tracks as usize == plan.track_count =>
            {
                info.columns.sizes.iter().sum::<f32>()
            }
            _ => continue,
        };
        if !target_track_sum.is_finite() || target_track_sum <= 0.0 {
            continue;
        }
        let mut max_content = vec![0.0f32; plan.track_count];
        let mut valid = true;
        for leaf in &plan.leaves {
            let Some(mut contribution) = measure_max_content(taffy_tree, leaf.node) else {
                valid = false;
                break;
            };
            let Some(leaf_style) = styles.get(&leaf.dom) else {
                valid = false;
                break;
            };
            contribution += leaf_style.margin.left + leaf_style.margin.right;
            if plan.track_count > 1 {
                let gap_delta = leaf.gap - plan.parent_gap;
                contribution += if leaf.column == 0 || leaf.column + 1 == plan.track_count {
                    gap_delta / 2.0
                } else {
                    gap_delta
                };
            }
            if leaf.column == 0 {
                contribution += leaf.start_mbp;
            }
            if leaf.column + 1 == plan.track_count {
                contribution += leaf.end_mbp;
            }
            max_content[leaf.column] = max_content[leaf.column].max(contribution.max(0.0));
        }
        let max_sum: f32 = max_content.iter().sum();
        if !valid || max_sum > target_track_sum + 0.01 {
            continue;
        }
        let stretch = (target_track_sum - max_sum) / plan.track_count as f32;
        let used: Vec<f32> = max_content.iter().map(|width| width + stretch).collect();
        if used.iter().any(|width| !width.is_finite() || *width < 0.0) {
            continue;
        }

        // Validate the entire copied chain before mutating anything. A large
        // edge MBP or gap delta can exhaust an outer track; declining the plan
        // atomically is safer than leaving only the ancestor frozen.
        let mut copied_wrappers = Vec::with_capacity(plan.wrappers.len());
        for wrapper in &plan.wrappers {
            let mut copied = used.clone();
            if plan.track_count > 1 {
                let root_half = plan.parent_gap / 2.0;
                let child_half = wrapper.gap / 2.0;
                for (index, width) in copied.iter_mut().enumerate() {
                    *width += if index == 0 || index + 1 == plan.track_count {
                        root_half - child_half
                    } else {
                        plan.parent_gap - wrapper.gap
                    };
                }
            }
            copied[0] -= wrapper.start_mbp;
            copied[plan.track_count - 1] -= wrapper.end_mbp;
            if copied
                .iter()
                .any(|width| !width.is_finite() || *width < 0.0)
                || taffy_tree.style(wrapper.node).is_err()
            {
                valid = false;
                break;
            }
            copied_wrappers.push((wrapper.node, copied));
        }
        if !valid || taffy_tree.style(plan.parent).is_err() {
            continue;
        }

        let mut parent_style = taffy_tree.style(plan.parent).unwrap().clone();
        parent_style.grid_template_columns = fixed_grid_tracks(&used);
        let _ = taffy_tree.set_style(plan.parent, parent_style);
        for (node, copied) in copied_wrappers {
            let mut style = taffy_tree.style(node).unwrap().clone();
            style.grid_template_columns = fixed_grid_tracks(&copied);
            let _ = taffy_tree.set_style(node, style);
        }
        changed = true;
    }
    changed
}

/// Resolve a raw `grid-column`/`grid-row` value that names grid lines into a
/// numeric `taffy::Line`, using `map` (line-name -> 1-based line number). Handles
/// `a / b` (each side a name, integer, or `span n`) and the single-ident
/// `grid-column: foo` area shorthand (`foo-start / foo-end`). Returns `None` when
/// a referenced name is absent, leaving the item to auto-place.
fn resolve_named_placement(
    raw: &str,
    map: &HashMap<String, i16>,
) -> Option<taffy::Line<taffy::GridPlacement>> {
    use taffy::style_helpers::{line, span};
    let side = |tok: &str, is_start: bool| -> Option<taffy::GridPlacement> {
        let t = tok.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("auto") {
            return Some(taffy::GridPlacement::Auto);
        }
        if let Some(n) = t.strip_prefix("span") {
            if let Ok(s) = n.trim().parse::<u16>() {
                return Some(span(s));
            }
        }
        if let Ok(i) = t.parse::<i16>() {
            return Some(line(i));
        }
        map.get(t)
            .or_else(|| map.get(&format!("{t}-{}", if is_start { "start" } else { "end" })))
            .map(|&l| line(l))
    };
    if let Some((a, b)) = raw.split_once('/') {
        Some(taffy::Line {
            start: side(a, true)?,
            end: side(b, false)?,
        })
    } else {
        let name = raw.trim();
        if let Some(&s) = map.get(&format!("{name}-start")) {
            let end = map
                .get(&format!("{name}-end"))
                .map(|&e| line(e))
                .unwrap_or(taffy::GridPlacement::Auto);
            return Some(taffy::Line {
                start: line(s),
                end,
            });
        }
        map.get(name).map(|&s| taffy::Line {
            start: line(s),
            end: taffy::GridPlacement::Auto,
        })
    }
}

fn compute_absolute_rects(
    taffy_tree: &TaffyTree<usize>,
    taffy_id: taffy::NodeId,
    abs_x: f32,
    abs_y: f32,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    words: &HashMap<taffy::NodeId, (NodeId, String)>,
    rects: &mut HashMap<NodeId, Rect>,
    text_runs: &mut HashMap<NodeId, Vec<(Rect, String)>>,
    anon_rects: &mut HashMap<usize, Rect>,
    generated_nodes: &HashMap<taffy::NodeId, usize>,
    generated_rects: &mut [Option<Rect>],
) {
    if let Ok(layout) = taffy_tree.layout(taffy_id) {
        let x = abs_x + layout.location.x;
        let y = abs_y + layout.location.y;
        let rect = Rect {
            x,
            y,
            width: layout.size.width,
            height: layout.size.height,
        };

        if let Some(dom_id) = id_map.get(&taffy_id) {
            rects.insert(*dom_id, rect);
        } else if let Some(&item) = taffy_tree.get_node_context(taffy_id) {
            // A taffy leaf with an engine-item context but no DOM id is an
            // anonymous inline-run leaf (see `build_mixed_block`); record its
            // final rect by item index so the finalize pass can pin it.
            anon_rects.insert(item, rect);
        }
        if let Some(index) = generated_nodes.get(&taffy_id) {
            generated_rects[*index] = Some(rect);
        }
        // A word leaf's dom_id is its owning text node, shared by every other
        // word from the same node, so this appends rather than overwrites.
        if let Some((text_dom_id, word)) = words.get(&taffy_id) {
            text_runs
                .entry(*text_dom_id)
                .or_default()
                .push((rect, word.clone()));
        }

        if let Ok(children) = taffy_tree.children(taffy_id) {
            for child_id in children {
                compute_absolute_rects(
                    taffy_tree,
                    child_id,
                    x,
                    y,
                    id_map,
                    words,
                    rects,
                    text_runs,
                    anon_rects,
                    generated_nodes,
                    generated_rects,
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
struct StaticPositionCandidate {
    child: taffy::NodeId,
    target: taffy::NodeId,
    inline_axis: bool,
    block_axis: bool,
}

/// Attach positioned boxes to their CSS containing block rather than their
/// immediate DOM parent.
///
/// Taffy resolves an absolute child's insets against its direct layout-tree
/// parent. CSS instead uses the nearest positioned or transformed ancestor;
/// fixed boxes use the nearest transformed ancestor or the initial containing
/// block. A box with a fully-auto axis first remains in its original formatting
/// context so taffy can produce the placeholder-like static coordinate. The
/// caller harvests that coordinate and reparents it in a bounded second pass.
fn reparent_inset_positioned_nodes(
    tree: &DomTree,
    taffy_tree: &mut TaffyTree<usize>,
    taffy_root: taffy::NodeId,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Vec<StaticPositionCandidate> {
    let reverse: HashMap<NodeId, taffy::NodeId> = id_map
        .iter()
        .map(|(&taffy_id, &dom_id)| (dom_id, taffy_id))
        .collect();
    let mut nearest_abs_cb_for_children: HashMap<NodeId, taffy::NodeId> = HashMap::new();
    let mut nearest_fixed_cb_for_children: HashMap<NodeId, taffy::NodeId> = HashMap::new();
    let mut static_candidates = Vec::new();

    for dom_id in rendered_descendants(tree, tree.document()) {
        let Some(style) = styles.get(&dom_id) else {
            continue;
        };
        let parent = rendered_parent(tree, dom_id);
        let inherited_abs_cb = parent
            .and_then(|id| nearest_abs_cb_for_children.get(&id).copied())
            .unwrap_or(taffy_root);
        let inherited_fixed_cb = parent
            .and_then(|id| nearest_fixed_cb_for_children.get(&id).copied())
            .unwrap_or(taffy_root);

        // Record this before any candidate early-exit so all descendants get
        // O(1) nearest-containing-block lookups. Positioned and transformed
        // boxes capture absolute descendants; only transformed boxes capture
        // fixed descendants. The full walk stays O(n).
        let own_box = reverse.get(&dom_id).copied();
        let establishes_cb = style.establishes_positioning_containing_block();
        let abs_child_cb = if style.position.is_some() || establishes_cb {
            own_box.unwrap_or(inherited_abs_cb)
        } else {
            inherited_abs_cb
        };
        let fixed_child_cb = if establishes_cb {
            own_box.unwrap_or(inherited_fixed_cb)
        } else {
            inherited_fixed_cb
        };
        nearest_abs_cb_for_children.insert(dom_id, abs_child_cb);
        nearest_fixed_cb_for_children.insert(dom_id, fixed_child_cb);

        if !matches!(style.position, Some(taffy::Position::Absolute)) {
            continue;
        }
        let has_block_inset = style.inset[0].is_some() || style.inset[2].is_some();
        let has_inline_inset = style.inset[1].is_some() || style.inset[3].is_some();
        let Some(&child) = reverse.get(&dom_id) else {
            continue;
        };
        let target = if style.position_fixed {
            inherited_fixed_cb
        } else {
            inherited_abs_cb
        };
        let Some(current) = taffy_tree.parent(child) else {
            continue;
        };
        if current == target {
            continue;
        }
        if !has_block_inset || !has_inline_inset {
            static_candidates.push(StaticPositionCandidate {
                child,
                target,
                inline_axis: !has_inline_inset,
                block_axis: !has_block_inset,
            });
            continue;
        }
        if taffy_tree.remove_child(current, child).is_ok() {
            let _ = taffy_tree.add_child(target, child);
        }
    }
    static_candidates
}

fn taffy_global_origin(taffy_tree: &TaffyTree<usize>, node: taffy::NodeId) -> Option<(f32, f32)> {
    let mut current = Some(node);
    let mut x = 0.0;
    let mut y = 0.0;
    while let Some(id) = current {
        let layout = taffy_tree.layout(id).ok()?;
        x += layout.location.x;
        y += layout.location.y;
        current = taffy_tree.parent(id);
    }
    Some((x, y))
}

fn collect_taffy_global_rects(
    taffy_tree: &TaffyTree<usize>,
    node: taffy::NodeId,
    parent_x: f32,
    parent_y: f32,
    rects: &mut HashMap<taffy::NodeId, Rect>,
) {
    let Ok(layout) = taffy_tree.layout(node) else {
        return;
    };
    let x = parent_x + layout.location.x;
    let y = parent_y + layout.location.y;
    rects.insert(
        node,
        Rect {
            x,
            y,
            width: layout.size.width,
            height: layout.size.height,
        },
    );
    for child in taffy_tree.children(node).unwrap_or_default() {
        collect_taffy_global_rects(taffy_tree, child, x, y, rects);
    }
}

#[derive(Clone, Copy)]
struct FloatBand {
    top: f32,
    bottom: f32,
    left: f32,
    right: f32,
    side: crate::Float,
}

fn narrow_node_to_float_band(
    taffy_tree: &mut TaffyTree<usize>,
    node: taffy::NodeId,
    preliminary_rects: &HashMap<taffy::NodeId, Rect>,
    band: FloatBand,
) -> bool {
    let Some(&rect) = preliminary_rects.get(&node) else {
        return false;
    };
    if rect.y >= band.bottom || rect.y + rect.height <= band.top || rect.width <= 0.0 {
        return false;
    }
    let Ok(current) = taffy_tree.style(node) else {
        return false;
    };
    let Ok(layout) = taffy_tree.layout(node) else {
        return false;
    };
    let mut narrowed = current.clone();
    let (available, left_shift) = match band.side {
        crate::Float::Right => {
            if band.left >= rect.x + rect.width {
                return false;
            }
            ((band.left - rect.x).max(0.0), 0.0)
        }
        crate::Float::Left => {
            if band.right <= rect.x {
                return false;
            }
            let shift = (band.right - rect.x).max(0.0);
            ((rect.width - shift).max(0.0), shift)
        }
    };
    if available >= rect.width - 0.01 {
        return false;
    }
    let specified = if current.box_sizing == taffy::BoxSizing::ContentBox {
        (available
            - layout.padding.left
            - layout.padding.right
            - layout.border.left
            - layout.border.right)
            .max(0.0)
    } else {
        available
    };
    narrowed.size.width = taffy::Dimension::length(specified);
    narrowed.max_size.width = taffy::Dimension::length(specified);
    if left_shift > 0.0 {
        narrowed.margin.left = taffy::LengthPercentageAuto::length(layout.margin.left + left_shift);
    }
    taffy_tree.set_style(node, narrowed).is_ok()
}

fn grow_bfc_to_float_bottom(
    taffy_tree: &mut TaffyTree<usize>,
    node: taffy::NodeId,
    preliminary_rects: &HashMap<taffy::NodeId, Rect>,
    float_bottom: f32,
) -> bool {
    let Some(&rect) = preliminary_rects.get(&node) else {
        return false;
    };
    let desired_border_height = (float_bottom - rect.y).max(0.0);
    if desired_border_height <= rect.height + 0.01 {
        return false;
    }
    let Ok(current) = taffy_tree.style(node) else {
        return false;
    };
    let Ok(layout) = taffy_tree.layout(node) else {
        return false;
    };
    let specified = if current.box_sizing == taffy::BoxSizing::ContentBox {
        (desired_border_height
            - layout.padding.top
            - layout.padding.bottom
            - layout.border.top
            - layout.border.bottom)
            .max(0.0)
    } else {
        desired_border_height
    };
    let mut grown = current.clone();
    grown.min_size.height = taffy::Dimension::length(specified);
    taffy_tree.set_style(node, grown).is_ok()
}

fn narrow_intersecting_descendants(
    tree: &DomTree,
    id: NodeId,
    taffy_tree: &mut TaffyTree<usize>,
    reverse: &HashMap<NodeId, taffy::NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    ifc_items: &IfcRegistry,
    preliminary_rects: &HashMap<taffy::NodeId, Rect>,
    band: FloatBand,
) -> bool {
    let Some(&node) = reverse.get(&id) else {
        return false;
    };
    let Some(&rect) = preliminary_rects.get(&node) else {
        return false;
    };
    if rect.y >= band.bottom || rect.y + rect.height <= band.top {
        return false;
    }
    let Some(style) = styles.get(&id) else {
        return false;
    };
    if style.display == crate::Display::None
        || style.float.is_some()
        || matches!(style.position, Some(taffy::Position::Absolute))
    {
        return false;
    }

    // Float-avoiding formatting contexts move as one box. A shaped IFC or a
    // leaf has no deeper block boundary at which its width can change, so it
    // also consumes the current band as a unit.
    let in_flow_element_children: Vec<NodeId> = tree
        .children(id)
        .into_iter()
        .filter(|child| {
            styles.get(child).map_or(false, |child_style| {
                child_style.display != crate::Display::None
                    && child_style.float.is_none()
                    && !matches!(child_style.position, Some(taffy::Position::Absolute))
            })
        })
        .collect();
    if establishes_block_formatting_context(style)
        || ifc_items.whole.contains_key(&id)
        || in_flow_element_children.is_empty()
    {
        return narrow_node_to_float_band(taffy_tree, node, preliminary_rects, band);
    }

    let mut changed = false;
    for child in in_flow_element_children {
        changed |= narrow_intersecting_descendants(
            tree,
            child,
            taffy_tree,
            reverse,
            styles,
            ifc_items,
            preliminary_rects,
            band,
        );
    }
    changed
}

fn apply_float_continuations(
    tree: &DomTree,
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    ifc_items: &IfcRegistry,
) -> bool {
    let reverse: HashMap<NodeId, taffy::NodeId> =
        id_map.iter().map(|(&taffy, &dom)| (dom, taffy)).collect();
    let Some(root) = id_map
        .keys()
        .copied()
        .find(|node| taffy_tree.parent(*node).is_none())
    else {
        return false;
    };
    let mut preliminary_rects = HashMap::with_capacity(id_map.len());
    collect_taffy_global_rects(taffy_tree, root, 0.0, 0.0, &mut preliminary_rects);
    let mut changed = false;
    for continuation in &ifc_items.float_continuations {
        let Some(&float_rect) = preliminary_rects.get(&continuation.float) else {
            continue;
        };
        let Ok(float_layout) = taffy_tree.layout(continuation.float) else {
            continue;
        };
        let Some(&flow_rect) = preliminary_rects.get(&continuation.flow) else {
            continue;
        };
        let band = FloatBand {
            top: float_rect.y - float_layout.margin.top,
            bottom: float_rect.y + float_rect.height + float_layout.margin.bottom,
            left: float_rect.x - float_layout.margin.left,
            right: float_rect.x + float_rect.width + float_layout.margin.right,
            side: continuation.side,
        };
        if band.bottom <= flow_rect.y + flow_rect.height + 0.01 {
            continue;
        }

        // A non-BFC wrapper is transparent to the BFC's float manager. Visit
        // the wrapper's following siblings, then repeat at each ancestor until
        // (and including) the nearest ancestor BFC. Descend through ordinary
        // blocks so only the leaf/block bands that actually intersect the
        // float are narrowed; later siblings below the float stay full width.
        let mut current = continuation.owner;
        while let Some(parent) = rendered_parent(tree, current) {
            let siblings = rendered_children(tree, parent);
            let Some(index) = siblings.iter().position(|candidate| *candidate == current) else {
                break;
            };
            for sibling in &siblings[index + 1..] {
                changed |= narrow_intersecting_descendants(
                    tree,
                    *sibling,
                    taffy_tree,
                    &reverse,
                    styles,
                    ifc_items,
                    &preliminary_rects,
                    band,
                );
            }
            let reached_bfc = styles
                .get(&parent)
                .map(establishes_block_formatting_context)
                .unwrap_or(false);
            if reached_bfc {
                if styles
                    .get(&parent)
                    .map(|style| matches!(style.height, crate::Dimension::Auto))
                    .unwrap_or(false)
                {
                    if let Some(&bfc_node) = reverse.get(&parent) {
                        changed |= grow_bfc_to_float_bottom(
                            taffy_tree,
                            bfc_node,
                            &preliminary_rects,
                            band.bottom,
                        );
                    }
                }
                break;
            }
            current = parent;
        }
    }
    changed
}

/// Recompute table row tracks from the cells' content at their final column
/// widths. Generic grid intrinsic sizing can retain a taller contribution
/// from an earlier, narrower nested-table measurement even after that child
/// resolves to a shorter final box. CSS tables instead finish columns first,
/// measure cells at those widths, then derive rows bottom-up.
fn apply_table_row_geometry(
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    ifc_items: &IfcRegistry,
) -> bool {
    let mut changed = false;
    for (&table_node, row_minimums) in &ifc_items.table_rows {
        let nrows = row_minimums.len();
        if nrows == 0 {
            continue;
        }
        let mut row_heights: Vec<f32> = row_minimums
            .iter()
            .map(|height| height.unwrap_or(0.0).max(0.0))
            .collect();
        let mut cells = Vec::new();
        for cell in taffy_tree.children(table_node).unwrap_or_default() {
            let (Ok(cell_style), Ok(layout)) = (taffy_tree.style(cell), taffy_tree.layout(cell))
            else {
                continue;
            };
            let row = match cell_style.grid_row.start {
                GridPlacement::Line(line) => (line.as_i16().max(1) as usize) - 1,
                _ => continue,
            };
            let span = match cell_style.grid_row.end {
                GridPlacement::Span(span) => (span as usize).max(1),
                _ => 1,
            };
            if row >= nrows {
                continue;
            }
            let edges = layout.padding.top
                + layout.padding.bottom
                + layout.border.top
                + layout.border.bottom;
            // Taffy's content_size is the furthest content/child overflow in
            // border-box coordinates (an empty padded cell reports its padding
            // extent), so it is already the right natural border-box floor.
            let mut natural = layout.content_size.height.max(edges);
            if let Some(dom_id) = id_map.get(&cell) {
                if let Some(style) = styles.get(dom_id) {
                    if let crate::Dimension::Px(height) = style.height {
                        let specified = if style.box_sizing == crate::BoxSizing::ContentBox {
                            height
                                + style.padding.top
                                + style.padding.bottom
                                + style.border.top
                                + style.border.bottom
                        } else {
                            height
                        };
                        natural = natural.max(specified);
                    }
                }
            }
            cells.push((cell, row, span.min(nrows - row), natural.max(0.0)));
        }

        // Non-spanning cells define their row directly.
        for &(_, row, span, natural) in &cells {
            if span == 1 {
                row_heights[row] = row_heights[row].max(natural);
            }
        }

        let table_dom = id_map.get(&table_node).copied();
        let (row_gap, table_has_height) = table_dom
            .and_then(|dom_id| styles.get(&dom_id))
            .map(|style| {
                (
                    table_spacing(style).1,
                    matches!(style.height, crate::Dimension::Px(_)),
                )
            })
            .unwrap_or((0.0, false));

        // A rowspan contributes only the shortfall beyond the rows and gaps
        // it spans. Prefer unstyled rows; when they already have content,
        // distribute proportionally, matching the table row-group algorithm.
        for &(_, row, span, natural) in &cells {
            if span <= 1 {
                continue;
            }
            let end = row + span;
            let current: f32 =
                row_heights[row..end].iter().sum::<f32>() + row_gap * span.saturating_sub(1) as f32;
            let extra = natural - current;
            if extra <= 0.0 {
                continue;
            }
            let mut targets: Vec<usize> = (row..end)
                .filter(|&index| row_minimums[index].is_none())
                .collect();
            if targets.is_empty() {
                targets = (row..end).collect();
            }
            let weight: f32 = targets.iter().map(|&index| row_heights[index]).sum();
            for &index in &targets {
                let share = if weight > 0.0 {
                    extra * row_heights[index] / weight
                } else {
                    extra / targets.len() as f32
                };
                row_heights[index] += share;
            }
        }

        // A definite table height distributes surplus into rows; an auto
        // table must not preserve stale free space from the generic grid pass.
        if table_has_height {
            if let Ok(table_layout) = taffy_tree.layout(table_node) {
                let track_target = (table_layout.size.height
                    - table_layout.padding.top
                    - table_layout.padding.bottom
                    - table_layout.border.top
                    - table_layout.border.bottom
                    - row_gap * nrows.saturating_sub(1) as f32)
                    .max(0.0);
                let current: f32 = row_heights.iter().sum();
                let extra = track_target - current;
                if extra > 0.0 {
                    let mut targets: Vec<usize> = (0..nrows)
                        .filter(|&index| row_minimums[index].is_none())
                        .collect();
                    if targets.is_empty() {
                        targets = (0..nrows).collect();
                    }
                    let weight: f32 = targets.iter().map(|&index| row_heights[index]).sum();
                    for &index in &targets {
                        let share = if weight > 0.0 {
                            extra * row_heights[index] / weight
                        } else {
                            extra / targets.len() as f32
                        };
                        row_heights[index] += share;
                    }
                }
            }
        }

        if let Ok(current) = taffy_tree.style(table_node) {
            let mut fixed = current.clone();
            fixed.grid_template_rows = row_heights
                .iter()
                .map(|height| {
                    taffy::GridTemplateComponent::Single(taffy::MinMax {
                        min: taffy::MinTrackSizingFunction::length(*height),
                        max: taffy::MaxTrackSizingFunction::length(*height),
                    })
                })
                .collect();
            if taffy_tree.set_style(table_node, fixed).is_ok() {
                changed = true;
            }
        }
        // A CSS height on a cell is a row minimum, not a smaller final cell
        // box. Once the tracks are pinned, restore auto block-size so every
        // cell stretches to its complete (possibly spanned) grid area.
        for (cell, _, _, _) in cells {
            if let Ok(current) = taffy_tree.style(cell) {
                let mut stretched = current.clone();
                stretched.size.height = taffy::Dimension::auto();
                let _ = taffy_tree.set_style(cell, stretched);
            }
        }
    }
    changed
}

/// Balance the atomic child boxes of each CSS multi-column container.
///
/// Gecko's multicol reflow brackets a known-infeasible and known-feasible
/// column block-size, then repeatedly reflows at a tighter candidate until it
/// finds the smallest feasible height. We use the same invariant over the
/// already measured direct child boxes: a candidate is feasible when a
/// source-ordered greedy fill needs no more than the requested column count.
/// This is O(24 * children) per multicol container and does not reshape or
/// rebuild descendants during the search.
fn apply_multicol_balance(taffy_tree: &mut TaffyTree<usize>, multicol: &[MulticolBuild]) -> bool {
    let mut changed = false;
    for set in multicol {
        let column_count = set.columns.len();
        if column_count < 2 || set.children.is_empty() {
            continue;
        }
        let weights: Vec<f32> = set
            .children
            .iter()
            .map(|child| {
                taffy_tree.layout(*child).map_or(0.0, |layout| {
                    (layout.size.height + layout.margin.top + layout.margin.bottom).max(0.0)
                })
            })
            .collect();
        let groups = column_count.min(weights.len());
        let max_weight = weights.iter().copied().fold(0.0f32, f32::max);
        let total: f32 = weights.iter().sum();
        let mut low = max_weight;
        let mut high = total.max(low);
        if high > 0.0 {
            for _ in 0..24 {
                let candidate = (low + high) * 0.5;
                let mut used = 1usize;
                let mut current = 0.0f32;
                for weight in &weights {
                    if current > 0.0 && current + *weight > candidate {
                        used += 1;
                        current = *weight;
                    } else {
                        current += *weight;
                    }
                }
                if used <= groups {
                    high = candidate;
                } else {
                    low = candidate;
                }
            }
        }

        // Pack from the end at the smallest feasible height. This leaves the
        // first columns filled first (column-major source order) while still
        // guaranteeing one item for every non-empty column.
        let mut ranges = Vec::with_capacity(groups);
        let mut end = weights.len();
        for remaining_groups in (1..=groups).rev() {
            let earliest = remaining_groups - 1;
            let mut start = end;
            let mut used = 0.0f32;
            while start > earliest {
                let next = weights[start - 1];
                if start < end && used + next > high + 0.001 {
                    break;
                }
                start -= 1;
                used += next;
            }
            ranges.push(start..end);
            end = start;
        }
        ranges.reverse();

        for (index, column) in set.columns.iter().enumerate() {
            let children = ranges
                .get(index)
                .map(|range| &set.children[range.clone()])
                .unwrap_or(&[]);
            if taffy_tree.set_children(*column, children).is_ok() {
                changed = true;
            }
        }
    }
    changed
}

/// Table-cell block alignment is a post-row-sizing operation. Feeding
/// `justify-content:center/end` into a cell while grid is still computing its
/// auto row tracks lets alignment offsets contaminate the cell's intrinsic
/// block-size (a nested 24px table can incorrectly demand a 29px row). Measure
/// cells from block-start first, freeze each final cell border-box height, then
/// enable middle/bottom alignment for the final layout pass.
fn apply_table_cell_block_alignment(
    tree: &DomTree,
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> bool {
    let mut changed = false;
    for (&node, &dom_id) in id_map {
        let Some(style) = styles.get(&dom_id) else {
            continue;
        };
        if !style.internal_flex_container
            || !matches!(
                style.vertical_align,
                Some(crate::VerticalAlign::Middle | crate::VerticalAlign::Bottom)
            )
        {
            continue;
        }
        let is_cell = tree.get_node(dom_id).map_or(false, |dom_node| {
            dom_node.as_element().map_or(false, |element| {
                matches!(element.local.as_ref(), "td" | "th")
            })
        });
        if !is_cell
            || taffy_tree
                .children(node)
                .map_or(true, |children| children.is_empty())
        {
            // Pure-text cells are aligned after shaping in the text finalize
            // path, where their actual line box height is available.
            continue;
        }
        let (Ok(current), Ok(layout)) = (taffy_tree.style(node), taffy_tree.layout(node)) else {
            continue;
        };
        if current.display != taffy::style::Display::Flex {
            continue;
        }
        let mut aligned = current.clone();
        let border_padding =
            layout.border.top + layout.border.bottom + layout.padding.top + layout.padding.bottom;
        let declared_height = if aligned.box_sizing == taffy::BoxSizing::ContentBox {
            (layout.size.height - border_padding).max(0.0)
        } else {
            layout.size.height
        };
        aligned.size.height = length(declared_height);
        let is_middle = style.vertical_align == Some(crate::VerticalAlign::Middle);
        if aligned.flex_direction == taffy::FlexDirection::Column {
            aligned.justify_content = Some(if is_middle {
                taffy::JustifyContent::CENTER
            } else {
                taffy::JustifyContent::FLEX_END
            });
        } else {
            aligned.align_content = Some(if is_middle {
                taffy::AlignContent::CENTER
            } else {
                taffy::AlignContent::FLEX_END
            });
        }
        if taffy_tree.set_style(node, aligned).is_ok() {
            changed = true;
        }
    }
    changed
}

/// Repair Taffy 0.12's intrinsic main-size calculation for column flexboxes
/// containing a negative main-axis margin.
///
/// In the max-content path Taffy feeds the margin-adjusted contribution into
/// its flex-fraction calculation. A negative margin on a shrink-disabled item
/// can therefore subtract the item's entire flex basis (or many multiples of
/// it) from the container: a `margin-top:-15px` child with a 100px basis can
/// collapse an otherwise auto-height flex column to zero. Final item sizes and
/// placement are still correct; only the container's automatic block size is
/// corrupted.
///
/// CSS defines that automatic size from the flex items' outer main sizes. Use
/// the already-resolved final item margins and border-box sizes to freeze that
/// intrinsic content height, then let the caller run Taffy again so ordinary
/// min/max sizing and ancestor layout consume the corrected contribution.
fn repair_intrinsic_column_flex_negative_margins(
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> bool {
    let mut repairs = Vec::new();

    for (&node, &dom_id) in id_map {
        let Some(style) = styles.get(&dom_id) else {
            continue;
        };
        if style.display != crate::Display::Flex
            || style.internal_flex_container
            || style.height != crate::Dimension::Auto
            || effective_container_type(style) == crate::ContainerType::Size
            || !matches!(
                style.flex_direction,
                Some(taffy::FlexDirection::Column | taffy::FlexDirection::ColumnReverse)
            )
        {
            continue;
        }

        let Ok(children) = taffy_tree.children(node) else {
            continue;
        };
        let mut item_count = 0usize;
        let mut content_height = 0.0f32;
        let mut has_negative_main_margin = false;
        for child in children {
            let (Ok(child_style), Ok(child_layout)) =
                (taffy_tree.style(child), taffy_tree.layout(child))
            else {
                continue;
            };
            if child_style.display == taffy::style::Display::None
                || child_style.position == taffy::style::Position::Absolute
            {
                continue;
            }
            let margin = child_layout.margin.top + child_layout.margin.bottom;
            has_negative_main_margin |=
                child_layout.margin.top < 0.0 || child_layout.margin.bottom < 0.0;
            content_height += child_layout.size.height + margin;
            item_count += 1;
        }
        if !has_negative_main_margin || item_count == 0 {
            continue;
        }
        content_height += style.row_gap.unwrap_or(0.0) * item_count.saturating_sub(1) as f32;
        content_height = content_height.max(0.0);

        let Ok(layout) = taffy_tree.layout(node) else {
            continue;
        };
        let outer_height = content_height
            + layout.padding.top
            + layout.padding.bottom
            + layout.border.top
            + layout.border.bottom;
        if (layout.size.height - outer_height).abs() < 0.01 {
            continue;
        }
        let declared_height = if style.box_sizing == crate::BoxSizing::ContentBox {
            content_height
        } else {
            outer_height
        };
        repairs.push((node, declared_height));
    }

    let mut changed = false;
    for (node, height) in repairs {
        let Ok(current) = taffy_tree.style(node) else {
            continue;
        };
        let mut repaired = current.clone();
        repaired.size.height = taffy::Dimension::length(height);
        changed |= taffy_tree.set_style(node, repaired).is_ok();
    }
    changed
}

fn resolve_static_positions_and_reparent(
    taffy_tree: &mut TaffyTree<usize>,
    candidates: &[StaticPositionCandidate],
) {
    // Harvest every placeholder coordinate before mutating parent links.
    // Otherwise reparenting an outer candidate changes the global-origin walk
    // for a nested candidate while both still contain preliminary-pass layout
    // coordinates.
    let mut resolved = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let Some(child_origin) = taffy_global_origin(taffy_tree, candidate.child) else {
            continue;
        };
        let Some(target_origin) = taffy_global_origin(taffy_tree, candidate.target) else {
            continue;
        };
        let Ok(child_layout) = taffy_tree.layout(candidate.child) else {
            continue;
        };
        let child_margin = child_layout.margin;
        let Ok(target_layout) = taffy_tree.layout(candidate.target) else {
            continue;
        };
        let target_border = target_layout.border;
        let Ok(current_style) = taffy_tree.style(candidate.child) else {
            continue;
        };
        let mut style = current_style.clone();

        if candidate.inline_axis {
            style.inset.left =
                length(child_origin.0 - target_origin.0 - target_border.left - child_margin.left);
        }
        if candidate.block_axis {
            style.inset.top =
                length(child_origin.1 - target_origin.1 - target_border.top - child_margin.top);
        }
        resolved.push((*candidate, style));
    }

    for (candidate, style) in resolved {
        let Some(current) = taffy_tree.parent(candidate.child) else {
            continue;
        };
        if taffy_tree.remove_child(current, candidate.child).is_err() {
            continue;
        }
        if taffy_tree
            .add_child(candidate.target, candidate.child)
            .is_err()
        {
            let _ = taffy_tree.add_child(current, candidate.child);
            continue;
        }
        if taffy_tree.set_style(candidate.child, style).is_err() {
            let _ = taffy_tree.remove_child(candidate.target, candidate.child);
            let _ = taffy_tree.add_child(current, candidate.child);
        }
    }
}

/// Children for computed-value inheritance traversal.
///
/// Assigned light children inherit through their flattened parent slot, while
/// unslotted light DOM still receives computed CSSOM styles through its host.
/// Fallback children remain in the style traversal even when not rendered.
fn style_children(tree: &DomTree, id: NodeId) -> Vec<NodeId> {
    let mut children = tree.children(id);
    if let Some(shadow_children) = tree.shadow_children(id) {
        children.retain(|child| tree.assigned_slot(*child).is_none());
        children.extend(shadow_children);
    }
    if let Some(assigned) = tree.assigned_nodes(id) {
        children.extend(assigned);
    }
    children
}

/// Resolve the named-slot assignment for one `<slot>` in a native shadow tree.
///
/// Slot names are exact strings (the empty string is the default slot), and
/// only the first slot with a given name in shadow-tree order receives the
/// host's matching slottables. Text and element light children are slottable;
/// comments and other node kinds are not. When no node is assigned, the slot's
/// ordinary children remain its fallback content.
/// Return the flattened-tree children that generate boxes for `id`.
pub(crate) fn rendered_children(tree: &DomTree, id: NodeId) -> Vec<NodeId> {
    if let Some(shadow_children) = tree.shadow_children(id) {
        // A shadow host's light children stay in the DOM but its box tree is
        // generated from the shadow root. Matching light children re-enter at
        // slot insertion points below; unslotted children generate no boxes.
        return shadow_children;
    }
    if let Some(assigned_or_fallback) = tree.slot_rendered_children(id) {
        return assigned_or_fallback;
    }

    let Some(node) = tree.get_node(id) else {
        return Vec::new();
    };
    let is_closed_html_details = node.as_element().is_some_and(|name| {
        name.ns.as_ref() == "http://www.w3.org/1999/xhtml"
            && name.local.as_ref() == "details"
            && node.get_attribute("open").is_none()
    });
    if !is_closed_html_details {
        return tree.children(id);
    }

    // HTML gives a closed <details> a rendered child list containing only its
    // first direct <summary> element child. Source text before that summary,
    // non-summary elements, later summaries, and all of their descendants
    // remain in the DOM but generate no boxes until the `open` attribute is
    // present. Filtering at the box-tree boundary keeps both layout
    // classification and paint/text-run construction from observing the
    // hidden subtree.
    tree.children(id)
        .into_iter()
        .find(|child| {
            tree.get_node(*child).is_some_and(|child| {
                child.as_element().is_some_and(|name| {
                    name.ns.as_ref() == "http://www.w3.org/1999/xhtml"
                        && name.local.as_ref() == "summary"
                })
            })
        })
        .into_iter()
        .collect()
}

/// The parent of `id` in the flattened rendering tree.
///
/// Shadow-root children are parented to the host for layout/paint ancestry;
/// an assigned light child is parented to its slot; and an unslotted light
/// child has no rendered parent at all.
pub(crate) fn rendered_parent(tree: &DomTree, id: NodeId) -> Option<NodeId> {
    let parent = tree.get_node(id)?.parent?;
    if let Some(root) = tree.shadow_root_info(parent) {
        return Some(root.host);
    }
    if tree.shadow_root(parent).is_none() {
        return Some(parent);
    }
    tree.assigned_slot(id)
}

/// Flattened-tree descendants in preorder, excluding `root`.
///
/// This is the traversal paint and resource discovery must use: ordinary DOM
/// descendants omit native shadow trees, while walking both light and shadow
/// subtrees would paint slotted nodes twice and expose unslotted light DOM.
/// The live-node bound and visited set are defense in depth against a corrupt
/// assignment/tree graph; a valid flat tree visits every generated node once.
pub(crate) fn rendered_descendants(tree: &DomTree, root: NodeId) -> Vec<NodeId> {
    let limit = tree.len();
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    let mut stack = rendered_children(tree, root);
    stack.reverse();
    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        result.push(id);
        if result.len() >= limit {
            break;
        }
        let children = rendered_children(tree, id);
        stack.extend(children.into_iter().rev());
    }
    result
}

/// Does `id` have any direct rendered child that is inline-level (a
/// non-whitespace text node, or an element whose resolved display is
/// `Inline`)? Used to decide whether a block container needs the flex-row-wrap
/// approximation of an inline formatting context.
fn has_inline_content(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> bool {
    rendered_children(tree, id).into_iter().any(|cid| {
        let Some(node) = tree.get_node(cid) else {
            return false;
        };
        match &node.data {
            obscura_dom::tree::NodeData::Text { contents } => !contents.trim().is_empty(),
            _ => styles
                .get(&cid)
                .map(|s| {
                    // A display:contents wrapper is transparent: whether it
                    // reads as inline content depends on what it splices in.
                    if s.display_contents && s.display != crate::Display::None {
                        has_inline_content(tree, cid, styles)
                    } else {
                        // Out-of-flow boxes (absolutely positioned, floated)
                        // are not inline content: an inline that is the sole
                        // child of a hero wrapper must not drag the wrapper
                        // into the inline-formatting path.
                        crate::is_inline_level_box(s)
                            && !matches!(s.position, Some(taffy::Position::Absolute))
                            && s.float.is_none()
                    }
                })
                .unwrap_or(false),
        }
    })
}

/// Whether a computed box participates as an in-flow block-level child.
///
/// `LayoutStyle::display` stores the inner display mode for flex/grid boxes,
/// while `is_inline_block` preserves their authored inline outer display.
/// Consequently a non-inline flex/grid container is block-level just like a
/// `display:block` box, but inline-block/inline-flex/inline-grid are not.
fn is_in_flow_block_level(style: &crate::LayoutStyle) -> bool {
    matches!(
        style.display,
        crate::Display::Block | crate::Display::Flex | crate::Display::Grid
    ) && !style.is_inline_block
        && !style.display_contents
        && style.float.is_none()
        && !matches!(style.position, Some(taffy::Position::Absolute))
}

/// Blockify the generated layout children of a flex/grid container. A
/// display:contents element is transparent, so its first boxed descendants
/// are the items whose outer display changes.
fn blockify_layout_children(
    tree: &DomTree,
    parent: NodeId,
    styles: &mut HashMap<NodeId, crate::LayoutStyle>,
) {
    for child in rendered_children(tree, parent) {
        let transparent = styles
            .get(&child)
            .is_some_and(|style| style.display_contents && style.display != crate::Display::None);
        if transparent {
            if let Some(style) = styles.get_mut(&child) {
                for pseudo in [style.before_pseudo.as_mut(), style.after_pseudo.as_mut()]
                    .into_iter()
                    .flatten()
                {
                    crate::blockify_outer_display(pseudo);
                }
            }
            blockify_layout_children(tree, child, styles);
        } else if let Some(style) = styles.get_mut(&child) {
            crate::blockify_outer_display(style);
        }
    }
}

/// Generated `::before`/`::after` boxes participate in the same display
/// fixups as real elements. They are stored inside their host style rather
/// than in the DOM map, so the normal root/item/float/positioned loops above
/// cannot reach them.
fn blockify_generated_pseudos(styles: &mut HashMap<NodeId, crate::LayoutStyle>) {
    for host in styles.values_mut() {
        let host_is_item_container =
            matches!(host.display, crate::Display::Flex | crate::Display::Grid)
                && !host.internal_flex_container;
        for pseudo in [host.before_pseudo.as_mut(), host.after_pseudo.as_mut()]
            .into_iter()
            .flatten()
        {
            if host_is_item_container
                || matches!(pseudo.position, Some(taffy::Position::Absolute))
                || pseudo.float.is_some()
            {
                crate::blockify_outer_display(pseudo);
            }
        }
    }
}

/// Build whichever of an element or a text node `id` is, returning every
/// taffy node it produced (usually one, but a text node fans out to one leaf
/// per word; see `build_text_words`). Callers collecting a container's
/// children should use this instead of calling `build` directly, so text
/// nodes flatten into the same child list as their sibling elements rather
/// than being skipped or nested a level deeper (either of which would break
/// wrapping: the whole point is for words from different text nodes and
/// inline elements to sit as flat, interleaved siblings that a flex-wrap row
/// can break between anywhere).
fn build_any(
    tree: &DomTree,
    id: NodeId,
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &mut HashMap<taffy::NodeId, NodeId>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Vec<taffy::NodeId> {
    let is_text = tree
        .get_node(id)
        .map(|n| matches!(n.data, obscura_dom::tree::NodeData::Text { .. }))
        .unwrap_or(false);
    if is_text {
        return build_text_words(tree, id, taffy_tree, styles, words, engine, ifc_items);
    }
    // `display: contents` removes the element's own box; its children lay out as
    // if they were direct children of its parent (CSS Display 3). Splice them
    // into the caller's child list (same trick as inline wrappers below).
    // Without this the wrapper became a normal auto-sized box (news-site card
    // anchors are `<a style="display:contents">` around the image), which
    // collapsed to zero area and blanked the image inside (bbc.com/news hero,
    // theguardian.com cards). `display:none` still wins.
    let splices_children = styles
        .get(&id)
        .map(|s| s.display_contents && s.display != crate::Display::None)
        .unwrap_or(false);
    if splices_children {
        return rendered_children(tree, id)
            .into_iter()
            .flat_map(|cid| {
                build_any(
                    tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                )
            })
            .collect();
    }
    if is_flattenable_inline(tree, id, styles) {
        // A plain inline wrapper (`<span>`, `<a>`, `<b>`, ...) around text
        // with no box appearance of its own: giving it its own flex-wrap
        // container, like every other element gets, would make *it* the
        // inline-formatting context instead of the enclosing block, which is
        // backwards. Real inline boxes do not wrap independently; the words
        // they contain wrap as part of the single line-breaking process run
        // by their nearest block ancestor. Concretely, a taffy flex-wrap
        // container with an auto width sizes itself from a "measure" pass
        // that does not yet know the final available width, so it collapses
        // toward a min-content (one word wide) guess — visible on Wikipedia's
        // "53 languages" label, which wrapped to "53" / "languages" on two
        // lines purely because the wrapping `<span>` around that text was
        // itself an independent auto-width wrap container. Flattening the
        // wrapper's children into the caller's list (same trick already used
        // for text nodes in `build_text_words`) fixes this at the root: the
        // words become flat siblings that wrap only at the real block level.
        return rendered_children(tree, id)
            .into_iter()
            .flat_map(|cid| {
                build_any(
                    tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                )
            })
            .collect();
    }
    let Some(inner) = build(
        tree, id, taffy_tree, id_map, words, engine, ifc_items, styles,
    ) else {
        return Vec::new();
    };
    let inline_align = styles.get(&id).and_then(|style| {
        if !crate::is_inline_level_box(style) {
            return None;
        }
        match style.vertical_align {
            Some(crate::VerticalAlign::Middle) => Some(taffy::AlignSelf::CENTER),
            Some(crate::VerticalAlign::Bottom) => Some(taffy::AlignSelf::FLEX_END),
            Some(crate::VerticalAlign::Top) => Some(taffy::AlignSelf::FLEX_START),
            None => None,
        }
    });
    let needs_outer = styles.get(&id).is_some_and(|style| {
        style.is_inline_block
            && matches!(style.display, crate::Display::Flex | crate::Display::Grid)
            && matches!(style.width, crate::Dimension::Auto)
            && style.size_expressions[0].is_none()
    });
    if !needs_outer {
        if let Some(align_self) = inline_align {
            if let Ok(current) = taffy_tree.style(inner) {
                let mut adjusted = current.clone();
                adjusted.align_self = Some(align_self);
                let _ = taffy_tree.set_style(inner, adjusted);
            }
        }
        return vec![inner];
    }

    // Taffy stores only the inner display mode. If an inline-flex/grid node is
    // handed directly to block or flex layout, its auto width is stretched to
    // the available width. A transparent atomic outer node supplies the
    // shrink-wrapping inline participation while the authored node retains its
    // real Flex/Grid layout and remains the DOM geometry/paint owner.
    let transfers_inline_constraint = styles.get(&id).is_some_and(|style| {
        style.min_width != crate::Dimension::Auto
            || style.max_width != crate::Dimension::Auto
            || style.size_expressions[2].is_some()
            || style.size_expressions[4].is_some()
    });
    let (outer_min_width, outer_max_width) = if transfers_inline_constraint {
        taffy_tree
            .style(inner)
            .map(|inner_style| (inner_style.min_size.width, inner_style.max_size.width))
            .unwrap_or((taffy::Dimension::auto(), taffy::Dimension::auto()))
    } else {
        (taffy::Dimension::auto(), taffy::Dimension::auto())
    };
    if transfers_inline_constraint {
        if let Ok(current) = taffy_tree.style(inner) {
            let mut adjusted = current.clone();
            adjusted.size.width = taffy::Dimension::percent(1.0);
            adjusted.min_size.width = taffy::Dimension::auto();
            adjusted.max_size.width = taffy::Dimension::auto();
            let _ = taffy_tree.set_style(inner, adjusted);
        }
    }
    let outer_style = taffy::Style {
        direction: styles
            .get(&id)
            .and_then(|style| style.direction)
            .unwrap_or(taffy::Direction::Ltr),
        display: taffy::style::Display::Flex,
        flex_direction: taffy::FlexDirection::Row,
        flex_wrap: taffy::FlexWrap::Wrap,
        align_items: Some(taffy::AlignItems::FLEX_START),
        align_self: inline_align,
        min_size: taffy::Size {
            width: outer_min_width,
            height: taffy::Dimension::auto(),
        },
        max_size: taffy::Size {
            width: outer_max_width,
            height: taffy::Dimension::auto(),
        },
        ..Default::default()
    };
    match taffy_tree.new_with_children(outer_style, &[inner]) {
        Ok(outer) => vec![outer],
        Err(_) => vec![inner],
    }
}

/// Build the direct children of a genuine flex/grid container.
///
/// CSS wraps every contiguous run of in-flow text in one anonymous flex/grid
/// item whose contents form an inline formatting context. Splitting a text
/// node into one taffy item per word is only valid inside our flex-wrap IFC
/// stand-in; doing it at this outer level makes `flex-wrap:nowrap` lay an
/// entire paragraph on one line (MDN's contributor quote). Fold text runs to
/// one measured/shaped anonymous item and leave authored element children as
/// their own flex/grid items.
#[allow(clippy::too_many_arguments)]
fn build_flex_grid_children(
    tree: &DomTree,
    parent: NodeId,
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &mut HashMap<taffy::NodeId, NodeId>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Vec<taffy::NodeId> {
    let mut effective_children = Vec::new();
    collect_effective_grid_children(
        tree,
        &rendered_children(tree, parent),
        styles,
        &mut effective_children,
    );
    effective_children.retain(|child| match child {
        EffectiveGridChild::Dom(node) => {
            tree.get_node(*node).is_some_and(|node| node.is_element())
                || !tree.text_content(*node).trim().is_empty()
        }
        EffectiveGridChild::Generated { .. } => true,
    });
    effective_children.sort_by_key(|child| {
        effective_grid_child_style(*child, styles)
            .map(|style| style.order)
            .unwrap_or(0)
    });

    let mut children = Vec::new();
    let mut index = 0;
    while index < effective_children.len() {
        let is_text = match effective_children[index] {
            EffectiveGridChild::Dom(node) => tree.get_node(node).is_some_and(|node| {
                matches!(node.data, obscura_dom::tree::NodeData::Text { .. })
            }),
            EffectiveGridChild::Generated { .. } => false,
        };
        if !is_text {
            match effective_children[index] {
                EffectiveGridChild::Dom(node) => children.extend(build_any(
                    tree, node, taffy_tree, id_map, words, engine, ifc_items, styles,
                )),
                EffectiveGridChild::Generated { host, kind } => {
                    let pseudo = effective_grid_child_style(effective_children[index], styles);
                    if let Some((nodes, _)) = build_in_flow_pseudo(
                        host, kind, pseudo, taffy_tree, words, engine, ifc_items,
                    ) {
                        children.extend(nodes);
                    }
                }
            }
            index += 1;
            continue;
        }

        let mut run = Vec::new();
        while let Some(EffectiveGridChild::Dom(node)) = effective_children.get(index).copied() {
            if !tree.get_node(node).is_some_and(|node| {
                matches!(node.data, obscura_dom::tree::NodeData::Text { .. })
            }) {
                break;
            }
            run.push(node);
            index += 1;
        }
        if let Some(item) = engine.try_build_run(tree, parent, &run, styles) {
            let style = taffy::Style {
                display: taffy::style::Display::Block,
                ..Default::default()
            };
            if let Ok(leaf) = taffy_tree.new_leaf_with_context(style, item) {
                ifc_items.runs.entry(parent).or_default().push(item);
                children.push(leaf);
                continue;
            }
        }
        for text in run {
            children.extend(build_any(
                tree, text, taffy_tree, id_map, words, engine, ifc_items, styles,
            ));
        }
    }
    children
}

/// Is `id` a `display: inline` element with no box appearance or sizing of
/// its own — safe to flatten into its parent's child list instead of giving
/// it an independent (and, for wrapping auto-width containers, buggy) flex
/// context? Covers the dominant real-world case: `<a>`/`<span>`/`<b>`/etc.
/// wrapping plain text with no inline styling.
fn is_flattenable_inline(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> bool {
    let Some(node) = tree.get_node(id) else {
        return false;
    };
    let Some(element) = node.as_element() else {
        return false;
    };
    // BR is boxless-looking but semantically contributes a mandatory break.
    // Flattening it as an empty wrapper deletes the break entirely.
    if element.local.as_ref() == "br" {
        return false;
    }
    let Some(style) = styles.get(&id) else {
        return false;
    };
    style.display == crate::Display::Inline
        && !style.is_inline_block
        && !style.is_replaced_box
        && style.before_pseudo.is_none()
        && style.after_pseudo.is_none()
        && style.background_color.is_none()
        && style.background_image.is_none()
        && style.mask_image.is_none()
        && style.border == crate::Edges::default()
        && style.position.is_none()
        && !style.overflow_hidden
        && style.float.is_none()
}

/// Split a text node into one taffy leaf per word (a whitespace-delimited
/// token, each keeping a single trailing space so adjacent words stay
/// visually separated), so it can wrap across several lines within its
/// container instead of being one indivisible box. A single leaf per whole
/// text node cannot wrap internally: flex-wrap only ever breaks *between*
/// items, so a long run of plain text with no inline elements breaking it up
/// (a very common shape: prose without a link for several sentences) would
/// either fit as one giant item or overflow straight past the container's
/// edge, never wrapping onto a new line the way real text does.
fn build_text_words(
    tree: &DomTree,
    id: NodeId,
    taffy_tree: &mut TaffyTree<usize>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
) -> Vec<taffy::NodeId> {
    let Some(node) = tree.get_node(id) else {
        return Vec::new();
    };
    let obscura_dom::tree::NodeData::Text { contents } = &node.data else {
        return Vec::new();
    };

    let mut display_text = String::new();
    let mut in_space = false;
    for c in contents.chars() {
        if c.is_whitespace() {
            if !in_space {
                display_text.push(' ');
                in_space = true;
            }
        } else {
            display_text.push(c);
            in_space = false;
        }
    }

    let mut fsize = 16.0;
    let mut is_bold = false;
    let mut family = None;
    let mut line_height = fsize * 1.2;
    let mut transform = crate::TextTransform::None;
    let mut letter_spacing = 0.0;
    if let Some(parent_id) = rendered_parent(tree, id) {
        if let Some(p_style) = styles.get(&parent_id) {
            fsize = p_style.font_size.unwrap_or(16.0);
            is_bold = crate::style::used_font_weight(p_style) >= 600;
            family = p_style.font_family.as_deref();
            line_height = crate::inline::used_line_height(p_style);
            transform = p_style.text_transform.unwrap_or(crate::TextTransform::None);
            letter_spacing = p_style.letter_spacing.unwrap_or(0.0);
        }
    }
    if let Some(style) = rendered_parent(tree, id).and_then(|parent| styles.get(&parent)) {
        let shaped = build_shaped_word_leaves(
            id,
            &display_text,
            style,
            taffy_tree,
            words,
            engine,
            ifc_items,
        );
        if !shaped.is_empty() {
            return shaped;
        }
    }

    display_text = transform_word_leaf_text(&display_text, transform);
    build_word_leaves(
        id,
        &display_text,
        fsize,
        line_height,
        is_bold,
        family,
        letter_spacing,
        taffy_tree,
        words,
    )
}

/// Webfont-aware counterpart to [`build_word_leaves`].
///
/// The general flex-wrap fallback still needs one independently placeable
/// Taffy leaf per word. Shape each visible token through the render pass's
/// retained `TextEngine`, then reuse that same item at paint time. This keeps
/// loaded faces, variable axes, optical sizing, transforms, and glyph
/// rasterization identical to the full inline-formatting-context path.
fn build_shaped_word_leaves(
    source_id: NodeId,
    text: &str,
    style: &crate::LayoutStyle,
    taffy_tree: &mut TaffyTree<usize>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
) -> Vec<taffy::NodeId> {
    let mut leaves = Vec::new();
    for token in tokenize_with_spaces(text) {
        if token.trim().is_empty() {
            continue;
        }
        // A token normally retains one trailing collapsed space. Shape it as
        // preformatted content so the item keeps that advance instead of the
        // paragraph path trimming collapsible trailing whitespace.
        let mut token_style = style.clone();
        token_style.white_space = Some(crate::WhiteSpace::Pre);
        let Some(item) = engine.push_generated_text(&token, &token_style) else {
            // Returning no leaves makes the caller use the deterministic
            // layout-only/static-font fallback for the whole text node.
            return Vec::new();
        };
        let (width, height) = engine.measure_word(item);
        let taffy_style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::length(width.max(0.0)),
                height: taffy::Dimension::length(height.max(0.0)),
            },
            ..Default::default()
        };
        let Ok(leaf) = taffy_tree.new_leaf_with_context(taffy_style, item) else {
            return Vec::new();
        };
        words.insert(
            leaf,
            (
                source_id,
                transform_word_leaf_text(
                    &token,
                    style.text_transform.unwrap_or(crate::TextTransform::None),
                ),
            ),
        );
        ifc_items
            .word_items
            .entry(source_id)
            .or_default()
            .push(item);
        leaves.push(leaf);
    }
    leaves
}

fn transform_word_leaf_text(text: &str, transform: crate::TextTransform) -> String {
    match transform {
        crate::TextTransform::None => text.to_string(),
        crate::TextTransform::Uppercase => text.chars().flat_map(char::to_uppercase).collect(),
        crate::TextTransform::Lowercase => text.chars().flat_map(char::to_lowercase).collect(),
        crate::TextTransform::Capitalize => {
            let mut at_word_start = true;
            let mut out = String::with_capacity(text.len());
            for ch in text.chars() {
                if ch.is_whitespace() {
                    at_word_start = true;
                    out.push(ch);
                } else if at_word_start {
                    out.extend(ch.to_uppercase());
                    at_word_start = false;
                } else {
                    out.push(ch);
                }
            }
            out
        }
    }
}

/// Split `text` into one taffy leaf per word and register each against
/// `source_id` in `words`. Shared by `build_text_words` (a real DOM text
/// node) and `build_pseudo_content` (a `::before`/`::after` literal, which
/// has no text node of its own — `source_id` is the host element instead).
fn build_word_leaves(
    source_id: NodeId,
    text: &str,
    fsize: f32,
    line_height: f32,
    is_bold: bool,
    family: Option<&str>,
    letter_spacing: f32,
    taffy_tree: &mut TaffyTree<usize>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
) -> Vec<taffy::NodeId> {
    tokenize_with_spaces(text)
        .into_iter()
        .filter_map(|token| {
            let width = text_width(&token, fsize, is_bold, family, letter_spacing);
            // A pure-whitespace token is HTML source formatting or a bare
            // inter-element space; it keeps its (small) width so adjacent
            // inline content stays visually separated, but contributes no
            // height, so it never adds a spurious blank row when it lands
            // between block-level siblings (e.g. formatting whitespace
            // around a run of now-collapsed, display:none list items).
            let height = if token.trim().is_empty() {
                0.0
            } else {
                line_height.max(0.0)
            };
            let taffy_style = taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::length(width),
                    height: taffy::Dimension::length(height),
                },
                ..Default::default()
            };
            let taffy_id = taffy_tree.new_leaf(taffy_style).ok()?;
            words.insert(taffy_id, (source_id, token));
            Some(taffy_id)
        })
        .collect()
}

/// Build the word leaves for a `::before`/`::after` literal `content`,
/// registered against the host element's own id (there is no real DOM text
/// node backing generated content, so its geometry is reported under the
/// element itself in `DomLayout::text_runs`, and painted the same way a real
/// text node's words are).
fn build_pseudo_content(
    id: NodeId,
    content: &str,
    style: &crate::LayoutStyle,
    taffy_tree: &mut TaffyTree<usize>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
) -> Vec<taffy::NodeId> {
    let shaped = build_shaped_word_leaves(id, content, style, taffy_tree, words, engine, ifc_items);
    if !shaped.is_empty() {
        return shaped;
    }

    let fsize = style.font_size.unwrap_or(16.0);
    let is_bold = crate::style::used_font_weight(style) >= 600;
    build_word_leaves(
        id,
        content,
        fsize,
        crate::inline::used_line_height(style),
        is_bold,
        style.font_family.as_deref(),
        style.letter_spacing.unwrap_or(0.0),
        taffy_tree,
        words,
    )
}

fn pseudo_requires_generated_box(style: &crate::LayoutStyle, content: Option<&str>) -> bool {
    content == Some("")
        || style.content_image.is_some()
        || style.display != crate::Display::Inline
        || style.is_inline_block
        || style.position.is_some()
        || style.margin != crate::Edges::default()
        || style.margin_auto.iter().any(|value| *value)
        || style.padding != crate::Edges::default()
        || style.padding_percent.iter().any(|value| value.is_some())
        || style.border != crate::Edges::default()
        || style.background_color.is_some()
        || style.background_gradient.is_some()
        || style.background_radial_gradient.is_some()
        || style.background_conic_gradient.is_some()
        || style.background_image.is_some()
        || style.mask_image.is_some()
        || style.box_shadow.is_some()
        || !style.border_model.radii.is_zero()
        || style.overflow_hidden
        || !style.transform_ops.is_empty()
        || style.individual_translate.is_some()
        || style.individual_rotate.is_some()
        || style.individual_scale.is_some()
}

fn has_in_flow_generated_pseudo(pseudo: Option<&crate::LayoutStyle>) -> bool {
    pseudo.map_or(false, |pseudo| {
        pseudo.display != crate::Display::None
            && !matches!(pseudo.position, Some(taffy::Position::Absolute))
    })
}

/// Build one in-flow pseudo as either the existing zero-overhead generated
/// text leaves or a real anonymous box. The latter is used only when the
/// pseudo's own box model can affect geometry/paint; ordinary `content:"x"`
/// remains part of the surrounding inline run and does not allocate an extra
/// container.
fn build_in_flow_pseudo(
    host: NodeId,
    kind: GeneratedBoxKind,
    pseudo: Option<&crate::LayoutStyle>,
    taffy_tree: &mut TaffyTree<usize>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
) -> Option<(Vec<taffy::NodeId>, bool)> {
    let pseudo = pseudo?;
    if matches!(pseudo.position, Some(taffy::Position::Absolute))
        || pseudo.display == crate::Display::None
    {
        return None;
    }
    let content = pseudo.before_content.as_deref();
    if !pseudo_requires_generated_box(pseudo, content) {
        let leaves =
            build_pseudo_content(host, content?, pseudo, taffy_tree, words, engine, ifc_items);
        return (!leaves.is_empty()).then_some((leaves, false));
    }

    let children = content
        // CSS table fixup discards whitespace-only text between table
        // structures. Clearfixes conventionally use `content:" "`, so
        // retaining that text as a measured leaf gives the otherwise-empty
        // generated table a spurious space glyph of intrinsic width.
        .filter(|text| !text.is_empty() && !(pseudo.is_table_box && text.trim().is_empty()))
        .map(|text| build_pseudo_content(host, text, pseudo, taffy_tree, words, engine, ifc_items))
        .unwrap_or_default();
    let mut taffy_style = to_taffy_style(pseudo);
    // A block pseudo's outer participation is block-level, but its generated
    // text still forms an inline formatting context inside it. Taffy has no
    // split outer/inner display representation, so use a wrapping row for the
    // inner text while the parent block still treats this node as one child.
    if pseudo.display == crate::Display::Block && !children.is_empty() {
        taffy_style.display = taffy::style::Display::Flex;
        taffy_style.flex_direction = taffy::FlexDirection::Row;
        taffy_style.flex_wrap = taffy::FlexWrap::Wrap;
    }
    let node = if children.is_empty() {
        taffy_tree.new_leaf(taffy_style).ok()?
    } else {
        taffy_tree.new_with_children(taffy_style, &children).ok()?
    };
    ifc_items
        .generated
        .push(GeneratedBoxBuild { host, kind, node });
    let block_level = pseudo.display != crate::Display::Inline && !pseudo.is_inline_block;
    Some((vec![node], block_level))
}

/// Split already whitespace-collapsed text into tokens, each carrying at most
/// one trailing space (`"Hello World "` -> `["Hello ", "World "]`), so a
/// word's own width naturally includes the gap to the next one.
fn tokenize_with_spaces(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        cur.push(c);
        if c == ' ' {
            tokens.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Give each `<tr>`/`<tbody>`/`<thead>`/`<tfoot>` its CSS table-grid band. In
/// the grid table model these wrappers are not taffy boxes, so without this
/// their backgrounds, borders, and DOM geometry would disappear.
///
/// A row's inline extent includes all cells that start in it, but its block
/// extent must ignore the portion of a `rowspan` that continues through later
/// rows. Nested-table cells must not participate at all. Sections are then the
/// union of their already-synthesized direct rows.
fn synthesize_row_rects(tree: &DomTree, rects: &mut HashMap<NodeId, Rect>) {
    let mut rows = Vec::new();
    let mut sections = Vec::new();
    let mut table_inline: HashMap<NodeId, Rect> = HashMap::new();
    for id in rendered_descendants(tree, tree.document()) {
        let local = match tree
            .get_node(id)
            .and_then(|n| n.as_element().map(|e| e.local.to_string()))
        {
            Some(l) => l,
            None => continue,
        };
        match local.as_str() {
            "tr" => rows.push(id),
            "tbody" | "thead" | "tfoot" => sections.push(id),
            "td" | "th" => {
                let mut ancestor = rendered_parent(tree, id);
                while let Some(parent) = ancestor {
                    let is_table = tree.get_node(parent).map_or(false, |node| {
                        node.as_element()
                            .map_or(false, |element| element.local.as_ref() == "table")
                    });
                    if is_table {
                        if let Some(rect) = rects.get(&id) {
                            table_inline
                                .entry(parent)
                                .and_modify(|current| *current = current.union(rect))
                                .or_insert(*rect);
                        }
                        break;
                    }
                    ancestor = rendered_parent(tree, parent);
                }
            }
            _ => {}
        }
    }

    for id in rows {
        if rects.contains_key(&id) {
            continue;
        }
        let mut inline: Option<Rect> = None;
        let mut block: Option<Rect> = None;
        for cell in tree.children(id) {
            let is_cell = tree
                .get_node(cell)
                .and_then(|n| {
                    n.as_element()
                        .map(|e| matches!(e.local.as_ref(), "td" | "th"))
                })
                .unwrap_or(false);
            if !is_cell {
                continue;
            }
            let Some(r) = rects.get(&cell) else {
                continue;
            };
            inline = Some(match inline {
                Some(a) => a.union(r),
                None => *r,
            });
            let spans_one_row = tree
                .get_node(cell)
                .and_then(|node| {
                    node.get_attribute("rowspan")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .map_or(true, |span| span == 1);
            if spans_one_row {
                block = Some(match block {
                    Some(a) => a.union(r),
                    None => *r,
                });
            }
        }
        if let Some(mut inline) = inline {
            let mut ancestor = rendered_parent(tree, id);
            while let Some(parent) = ancestor {
                if let Some(table_band) = table_inline.get(&parent) {
                    inline.x = table_band.x;
                    inline.width = table_band.width;
                    break;
                }
                ancestor = rendered_parent(tree, parent);
            }
            let block = block.unwrap_or(inline);
            rects.insert(
                id,
                Rect {
                    x: inline.x,
                    y: block.y,
                    width: inline.width,
                    height: block.height,
                },
            );
        }
    }

    for id in sections {
        if rects.contains_key(&id) {
            continue;
        }
        let mut section: Option<Rect> = None;
        for row in tree.children(id) {
            let is_row = tree.get_node(row).map_or(false, |node| {
                node.as_element()
                    .map_or(false, |element| element.local.as_ref() == "tr")
            });
            if !is_row {
                continue;
            }
            if let Some(rect) = rects.get(&row) {
                section = Some(match section {
                    Some(current) => current.union(rect),
                    None => *rect,
                });
            }
        }
        if let Some(rect) = section {
            rects.insert(id, rect);
        }
    }
}

/// Number of ancestor tables containing `id`. Table sizing runs outer-first:
/// an outer table consumes an inner table's intrinsic contributions, then a
/// root layout establishes the inner table's real cell containing block.
fn table_ancestor_depth(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> usize {
    let mut depth = 0usize;
    let mut cur = id;
    while let Some(p) = rendered_parent(tree, cur) {
        if styles.get(&p).is_some_and(|style| style.is_table_box) {
            depth += 1;
        }
        cur = p;
        if depth > 4096 {
            break;
        }
    }
    depth
}

/// Resolve a content-box width without running layout when the complete
/// containing-block chain is ordinary block flow. Flex/grid allocation,
/// floats, positioned boxes, inline-blocks, and intrinsic sizing need a real
/// Taffy pass and deliberately return `None`.
fn reliable_normal_flow_content_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    initial_cb_width: f32,
    depth: usize,
) -> Option<f32> {
    if depth > 4096 {
        return None;
    }
    let style = styles.get(&id)?;
    if style.float.is_some()
        || matches!(style.position, Some(taffy::Position::Absolute))
        || style.is_inline_block
        || style.width_fit_content
        || style.size_expressions[0].is_some()
    {
        return None;
    }

    let mut parent = rendered_parent(tree, id);
    while parent.is_some_and(|parent_id| {
        styles
            .get(&parent_id)
            .is_some_and(|parent_style| parent_style.display_contents)
    }) {
        parent = parent.and_then(|parent_id| rendered_parent(tree, parent_id));
    }
    let containing_width = if let Some(parent_id) = parent {
        if let Some(parent_style) = styles.get(&parent_id) {
            if matches!(
                parent_style.display,
                crate::Display::Flex | crate::Display::Grid
            ) || parent_style.internal_flex_container
                || parent_style.column_count.is_some()
            {
                return None;
            }
            reliable_normal_flow_content_width(
                tree,
                parent_id,
                styles,
                initial_cb_width,
                depth + 1,
            )?
        } else {
            initial_cb_width
        }
    } else {
        initial_cb_width
    };

    let horizontal_edges =
        style.padding.left + style.padding.right + style.border.left + style.border.right;
    let declared_content = |dimension: crate::Dimension| match dimension {
        crate::Dimension::Px(value) => Some(if style.box_sizing == crate::BoxSizing::ContentBox {
            value
        } else {
            (value - horizontal_edges).max(0.0)
        }),
        crate::Dimension::Percent(percent) => {
            let value = containing_width * percent;
            Some(if style.box_sizing == crate::BoxSizing::ContentBox {
                value
            } else {
                (value - horizontal_edges).max(0.0)
            })
        }
        _ => None,
    };
    let mut content_width = declared_content(style.width).unwrap_or_else(|| {
        (containing_width - style.margin.left - style.margin.right - horizontal_edges).max(0.0)
    });
    if let Some(minimum) = declared_content(style.min_width) {
        content_width = content_width.max(minimum);
    }
    if let Some(maximum) = declared_content(style.max_width) {
        content_width = content_width.min(maximum);
    }
    Some(content_width.max(0.0))
}

/// Resolve an authored definite width even when the box's placement is owned
/// by float or positioned layout. A pixel width is independent of placement;
/// a percentage is definite only when the containing block can itself be
/// resolved without layout. Auto widths and functional size expressions stay
/// on the ordinary Taffy path.
fn reliable_declared_content_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    initial_cb_width: f32,
    depth: usize,
) -> Option<f32> {
    if depth > 4096 {
        return None;
    }
    let style = styles.get(&id)?;
    if style.width_fit_content
        || style.size_expressions[0].is_some()
        || style.size_expressions[2].is_some()
        || style.size_expressions[4].is_some()
    {
        return None;
    }

    let needs_containing_width = matches!(
        style.width,
        crate::Dimension::Percent(_)
    ) || matches!(style.min_width, crate::Dimension::Percent(_))
        || matches!(style.max_width, crate::Dimension::Percent(_));
    let containing_width = needs_containing_width.then(|| {
        let mut parent = rendered_parent(tree, id);
        while parent.is_some_and(|parent_id| {
            styles
                .get(&parent_id)
                .is_some_and(|parent_style| parent_style.display_contents)
        }) {
            parent = parent.and_then(|parent_id| rendered_parent(tree, parent_id));
        }
        if let Some(parent_id) = parent {
            reliable_normal_flow_content_width(
                tree,
                parent_id,
                styles,
                initial_cb_width,
                depth + 1,
            )
            .or_else(|| {
                reliable_declared_content_width(
                    tree,
                    parent_id,
                    styles,
                    initial_cb_width,
                    depth + 1,
                )
            })
        } else {
            Some(initial_cb_width)
        }
    });
    let containing_width = match containing_width {
        Some(Some(width)) => Some(width),
        Some(None) => return None,
        None => None,
    };

    let horizontal_edges =
        style.padding.left + style.padding.right + style.border.left + style.border.right;
    let declared_content = |dimension: crate::Dimension| match dimension {
        crate::Dimension::Px(value) => Some(if style.box_sizing == crate::BoxSizing::ContentBox {
            value
        } else {
            (value - horizontal_edges).max(0.0)
        }),
        crate::Dimension::Percent(percent) => {
            let value = containing_width? * percent;
            Some(if style.box_sizing == crate::BoxSizing::ContentBox {
                value
            } else {
                (value - horizontal_edges).max(0.0)
            })
        }
        _ => None,
    };
    let mut content_width = declared_content(style.width)?;
    if let Some(minimum) = declared_content(style.min_width) {
        content_width = content_width.max(minimum);
    }
    if let Some(maximum) = declared_content(style.max_width) {
        content_width = content_width.min(maximum);
    }
    Some(content_width.max(0.0))
}

/// Resolve the containing-block content width used by CSS's ratio-only
/// auto/auto replaced sizing branch. Decoration-free inline ancestors do not
/// establish containing blocks, so skip them (and display:contents) before
/// using the existing conservative ordinary-flow width resolver. A definite
/// authored width is also safe for floated or positioned containing boxes;
/// their auto-sized allocations still return `None` and remain layout-owned.
fn reliable_ratio_only_available_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    initial_cb_width: f32,
) -> Option<f32> {
    let image = styles.get(&id)?;
    let mut parent = rendered_parent(tree, id);
    while let Some(parent_id) = parent {
        let Some(parent_style) = styles.get(&parent_id) else {
            parent = rendered_parent(tree, parent_id);
            continue;
        };
        if parent_style.display_contents
            || (parent_style.display == crate::Display::Inline
                && !parent_style.is_inline_block)
        {
            parent = rendered_parent(tree, parent_id);
            continue;
        }
        break;
    }

    let containing_width = if let Some(parent_id) = parent {
        reliable_normal_flow_content_width(tree, parent_id, styles, initial_cb_width, 0)
            .or_else(|| {
                reliable_declared_content_width(tree, parent_id, styles, initial_cb_width, 0)
            })?
    } else {
        initial_cb_width
    };
    let horizontal_edges =
        image.padding.left + image.padding.right + image.border.left + image.border.right;
    Some(
        (containing_width
            - image.margin.left
            - image.margin.right
            - horizontal_edges)
            .max(0.0),
    )
}

fn reliable_table_available_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    initial_cb_width: f32,
) -> Option<f32> {
    let table_style = styles.get(&id)?;
    if table_style.float.is_some()
        || matches!(table_style.position, Some(taffy::Position::Absolute))
        || table_style.is_inline_block
    {
        return None;
    }
    let mut parent = rendered_parent(tree, id);
    while parent.is_some_and(|parent_id| {
        styles
            .get(&parent_id)
            .is_some_and(|parent_style| parent_style.display_contents)
    }) {
        parent = parent.and_then(|parent_id| rendered_parent(tree, parent_id));
    }
    let containing_width = match parent {
        Some(parent_id) if styles.contains_key(&parent_id) => {
            let parent_style = styles.get(&parent_id)?;
            if matches!(
                parent_style.display,
                crate::Display::Flex | crate::Display::Grid
            ) || parent_style.internal_flex_container
                || parent_style.column_count.is_some()
            {
                return None;
            }
            reliable_normal_flow_content_width(tree, parent_id, styles, initial_cb_width, 0)?
        }
        _ => initial_cb_width,
    };
    Some((containing_width - table_style.margin.left - table_style.margin.right).max(0.0))
}

/// Resolve the `width: fit-content` keyword after the containing inline space
/// is known.
///
/// Blink's shrink-to-fit helper and Gecko's `ShrinkISizeToFit` use the CSS
/// intrinsic-size formula:
///
/// `max(min-content, min(max-content, available - inline margins))`.
///
/// Taffy's box-size `Dimension` has no intrinsic keyword, so these nodes are
/// initially built as `width:auto`. That preliminary layout is useful: for a
/// stretched grid item it exposes the item's actual grid-area width (which can
/// be much narrower than the grid container), and for a normal block it
/// exposes its fill-available width. We snapshot that available space, measure
/// the subtree at min/max-content, then install the resulting definite
/// preferred width before the final root layout.
fn apply_fit_content_widths<F>(
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    initial_cb_width: f32,
    mut intrinsic_width: F,
) -> bool
where
    F: FnMut(&mut TaffyTree<usize>, taffy::NodeId, taffy::AvailableSpace) -> Option<f32>,
{
    struct Candidate {
        node: taffy::NodeId,
        available: f32,
        margin: f32,
        inline_edges: f32,
        content_box: bool,
    }

    // Snapshot every containing-space input before intrinsic subtree
    // measurements overwrite cached node layouts.
    let candidates: Vec<Candidate> = id_map
        .iter()
        .filter_map(|(&node, &dom)| {
            let style = styles.get(&dom)?;
            if !style.width_fit_content || style.ignores_used_box_sizes() {
                return None;
            }
            let layout = taffy_tree.layout(node).ok()?;
            let margin = layout.margin.left + layout.margin.right;
            let inline_edges = layout.padding.left
                + layout.padding.right
                + layout.border.left
                + layout.border.right;

            let parent = taffy_tree.parent(node);
            let parent_content = parent
                .and_then(|parent| taffy_tree.layout(parent).ok())
                .map(|layout| layout.content_box_width())
                .unwrap_or(initial_cb_width)
                .max(0.0);

            // `auto` stretches in these exact inline-axis situations, so its
            // preliminary margin-box width is the local available space. In
            // particular this preserves a grid area's track width instead of
            // incorrectly using the entire grid container.
            let uses_preliminary_stretch = parent
                .and_then(|parent| taffy_tree.style(parent).ok())
                .map(|parent_style| match parent_style.display {
                    taffy::Display::Block => {
                        style.float.is_none()
                            && !matches!(style.position, Some(taffy::Position::Absolute))
                    }
                    taffy::Display::Grid => {
                        let child_style = taffy_tree.style(node).ok();
                        let horizontal = child_style
                            .and_then(|child| child.justify_self)
                            .or(parent_style.justify_items)
                            .unwrap_or(taffy::AlignItems::NORMAL);
                        let vertical = child_style
                            .and_then(|child| child.align_self)
                            .or(parent_style.align_items)
                            .unwrap_or(taffy::AlignItems::NORMAL);
                        let normal = if child_style.is_some_and(|child| child.item_is_replaced)
                            || (child_style.is_some_and(|child| child.aspect_ratio.is_some())
                                && vertical == taffy::AlignItems::STRETCH)
                        {
                            taffy::AlignItems::START
                        } else {
                            taffy::AlignItems::STRETCH
                        };
                        let child_align = horizontal.resolve_normal(normal);
                        child_align == taffy::AlignItems::STRETCH
                    }
                    taffy::Display::Flex
                        if matches!(
                            parent_style.flex_direction,
                            taffy::FlexDirection::Column | taffy::FlexDirection::ColumnReverse
                        ) =>
                    {
                        let child_align = taffy_tree
                            .style(node)
                            .ok()
                            .and_then(|child| child.align_self)
                            .or(parent_style.align_items)
                            .unwrap_or(taffy::AlignItems::NORMAL)
                            .resolve_normal(taffy::AlignItems::STRETCH);
                        child_align == taffy::AlignItems::STRETCH
                    }
                    _ => false,
                })
                .unwrap_or(false);
            let available = if uses_preliminary_stretch {
                (layout.size.width + margin).max(0.0)
            } else {
                parent_content
            };

            Some(Candidate {
                node,
                available,
                margin,
                inline_edges,
                content_box: style.box_sizing == crate::BoxSizing::ContentBox,
            })
        })
        .collect();

    let mut changed = false;
    for candidate in candidates {
        let Some(min_content) = intrinsic_width(
            taffy_tree,
            candidate.node,
            taffy::AvailableSpace::MinContent,
        ) else {
            continue;
        };
        let Some(max_content) = intrinsic_width(
            taffy_tree,
            candidate.node,
            taffy::AvailableSpace::MaxContent,
        ) else {
            continue;
        };
        let fill = (candidate.available - candidate.margin).max(0.0);
        let used_outer = min_content.max(max_content.min(fill));
        let declaration = if candidate.content_box {
            (used_outer - candidate.inline_edges).max(0.0)
        } else {
            used_outer.max(0.0)
        };
        let Ok(current) = taffy_tree.style(candidate.node) else {
            continue;
        };
        let mut resolved = current.clone();
        resolved.size.width = taffy::Dimension::length(declaration);
        if taffy_tree.set_style(candidate.node, resolved).is_ok() {
            changed = true;
        }
    }
    changed
}

/// Collect rows together with the exclusive end of their originating row
/// group. HTML rowspans never cross a thead/tbody/tfoot boundary; `rowspan=0`
/// means the remainder of that group, not the remainder of the whole table.
fn collect_table_rows(tree: &DomTree, id: NodeId, rows: &mut Vec<(NodeId, usize)>) {
    let mut direct_start: Option<usize> = None;
    for cid in tree.children(id) {
        let local = tree
            .get_node(cid)
            .and_then(|n| n.as_element().map(|e| e.local.to_string()));
        match local.as_deref() {
            Some("tr") => {
                direct_start.get_or_insert(rows.len());
                rows.push((cid, 0));
            }
            Some("thead") | Some("tbody") | Some("tfoot") => {
                if let Some(start) = direct_start.take() {
                    let end = rows.len();
                    for entry in &mut rows[start..end] {
                        entry.1 = end;
                    }
                }
                let start = rows.len();
                for row in tree.children(cid) {
                    if tree.get_node(row).is_some_and(|node| {
                        node.as_element()
                            .is_some_and(|element| element.local.as_ref() == "tr")
                    }) {
                        rows.push((row, 0));
                    }
                }
                let end = rows.len();
                for entry in &mut rows[start..end] {
                    entry.1 = end;
                }
            }
            _ => {}
        }
    }
    if let Some(start) = direct_start {
        let end = rows.len();
        for entry in &mut rows[start..end] {
            entry.1 = end;
        }
    }
}

/// Effective separate-border spacing. Collapsed tables contribute no spacing
/// at either the outer table edges or between tracks.
fn table_spacing(style: &crate::LayoutStyle) -> (f32, f32) {
    if style.border_collapse.unwrap_or(false) {
        (0.0, 0.0)
    } else {
        style.border_spacing.unwrap_or((0.0, 0.0))
    }
}

/// Horizontal non-track area in the table's border box, excluding the gaps
/// *between* columns: authored border/padding plus one border-spacing unit at
/// each outer edge.
fn table_inline_outer_edges(style: &crate::LayoutStyle) -> f32 {
    let (spacing, _) = table_spacing(style);
    style.border.left
        + style.border.right
        + style.padding.left
        + style.padding.right
        + spacing * 2.0
}

/// Minimum track width implied by percentage columns in an auto-width table.
///
/// Percentage table-cell widths participate in the table's intrinsic width.
/// For example, if a 50% column has a 100px minimum next to 100px of auto
/// columns, the table needs 200px before both constraints can hold. When the
/// constraints cannot all fit (a 100% cell plus any non-empty neighbour), the
/// intrinsic requirement is unbounded and the used width is capped by the
/// available containing-block width. This is the behavior Bootstrap input
/// groups rely on to make an otherwise auto-width `display:table` fill their
/// form while reserving intrinsic space for narrow addon/button cells.
fn auto_table_percentage_intrinsic_floor(minimums: &[f32], percentages: &[Option<f32>]) -> f32 {
    if minimums.len() != percentages.len() || minimums.is_empty() {
        return 0.0;
    }

    let mut remaining = 1.0f32;
    let mut auto_minimum = 0.0f32;
    let mut required = 0.0f32;
    for (minimum, percentage) in minimums.iter().zip(percentages) {
        let minimum = minimum.max(0.0);
        let Some(percentage) = percentage else {
            auto_minimum += minimum;
            continue;
        };
        let effective = percentage.max(0.0).min(remaining);
        remaining = (remaining - effective).max(0.0);
        if minimum > 0.0 {
            if effective <= f32::EPSILON {
                return f32::INFINITY;
            }
            required = required.max(minimum / effective);
        }
    }
    if auto_minimum > 0.0 {
        if remaining <= f32::EPSILON {
            return f32::INFINITY;
        }
        required = required.max(auto_minimum / remaining);
    }
    required
}

/// Resolve auto-table column constraints into final track widths.
///
/// Gecko's auto-table strategy uses four monotonic guesses: all columns at
/// min-content; percentage columns raised toward their percentage; fixed
/// columns raised toward their preferred lengths; then auto columns raised to
/// max-content. The used width interpolates only the category between the two
/// surrounding guesses. This matters for Bootstrap input groups: a 100% input
/// column must shrink by the intrinsic widths of adjacent `width:1%; nowrap`
/// addon/button columns instead of making the table overflow.
fn distribute_auto_table_columns(
    target: f32,
    minimums: &[f32],
    preferreds: &[f32],
    fixed: &[Option<f32>],
    percentages: &[Option<f32>],
) -> Vec<f32> {
    let count = minimums.len();
    if count == 0
        || preferreds.len() != count
        || fixed.len() != count
        || percentages.len() != count
    {
        return minimums.to_vec();
    }
    let target = target.max(0.0);
    let mins: Vec<f32> = minimums.iter().map(|value| value.max(0.0)).collect();
    let prefs: Vec<f32> = preferreds
        .iter()
        .zip(&mins)
        .map(|(preferred, minimum)| preferred.max(*minimum))
        .collect();

    // Percentage column constraints are cumulatively clamped to 100% in
    // source order. Keep zero-valued entries as percentage columns: their
    // intrinsic minimum still participates in the percentage guess.
    let mut remaining = 1.0f32;
    let effective_percentages: Vec<Option<f32>> = percentages
        .iter()
        .map(|percentage| {
            percentage.map(|value| {
                let used = value.max(0.0).min(remaining);
                remaining = (remaining - used).max(0.0);
                used
            })
        })
        .collect();

    let guess_min = mins.clone();
    let guess_min_pct: Vec<f32> = (0..count)
        .map(|index| {
            effective_percentages[index]
                .map(|percentage| (percentage * target).max(mins[index]))
                .unwrap_or(mins[index])
        })
        .collect();
    let guess_min_spec: Vec<f32> = (0..count)
        .map(|index| {
            if effective_percentages[index].is_some() {
                guess_min_pct[index]
            } else if fixed[index].is_some() {
                prefs[index]
            } else {
                mins[index]
            }
        })
        .collect();
    let guess_pref: Vec<f32> = (0..count)
        .map(|index| {
            if effective_percentages[index].is_some() {
                guess_min_pct[index]
            } else {
                prefs[index]
            }
        })
        .collect();
    let total = |values: &[f32]| values.iter().sum::<f32>();
    let min_total = total(&guess_min);
    let min_pct_total = total(&guess_min_pct);
    let min_spec_total = total(&guess_min_spec);
    let pref_total = total(&guess_pref);

    let interpolate = |lower: &[f32], upper: &[f32], wanted: f32| {
        let lower_total = total(lower);
        let delta_total = (total(upper) - lower_total).max(0.0);
        if delta_total <= f32::EPSILON {
            return lower.to_vec();
        }
        let scale = ((wanted - lower_total) / delta_total).clamp(0.0, 1.0);
        lower
            .iter()
            .zip(upper)
            .map(|(low, high)| low + (high - low).max(0.0) * scale)
            .collect()
    };

    if target <= min_total {
        return guess_min;
    }
    if target < min_pct_total {
        return interpolate(&guess_min, &guess_min_pct, target);
    }
    if target < min_spec_total {
        return interpolate(&guess_min_pct, &guess_min_spec, target);
    }
    if target < pref_total {
        return interpolate(&guess_min_spec, &guess_pref, target);
    }

    let mut result = guess_pref;
    let extra = target - pref_total;
    if extra <= f32::EPSILON {
        return result;
    }
    let mut candidates: Vec<(usize, f32)> = (0..count)
        .filter(|index| effective_percentages[*index].is_none() && fixed[*index].is_none())
        .filter_map(|index| (prefs[index] > 0.0).then_some((index, prefs[index])))
        .collect();
    if candidates.is_empty() {
        candidates = (0..count)
            .filter(|index| {
                effective_percentages[*index].is_none() && fixed[*index].is_none()
            })
            .map(|index| (index, 1.0))
            .collect();
    }
    if candidates.is_empty() {
        candidates = (0..count)
            .filter(|index| effective_percentages[*index].is_none() && fixed[*index].is_some())
            .map(|index| (index, prefs[index].max(0.0)))
            .collect();
    }
    if candidates.is_empty() {
        candidates = effective_percentages
            .iter()
            .enumerate()
            .filter_map(|(index, percentage)| {
                (*percentage)
                    .filter(|value| *value > 0.0)
                    .map(|value| (index, value))
            })
            .collect();
    }
    if candidates.is_empty() {
        candidates = (0..count).map(|index| (index, 1.0)).collect();
    }
    let weight: f32 = candidates.iter().map(|(_, value)| *value).sum();
    let candidate_count = candidates.len() as f32;
    for (index, value) in candidates {
        result[index] += if weight > 0.0 {
            extra * value / weight
        } else {
            extra / candidate_count
        };
    }
    result
}

/// Resolve CSS 2 fixed-layout column constraints into exact track widths.
///
/// Columns and first-row cells establish the initial widths. Unspecified
/// tracks share the remaining space; when every track is specified, surplus
/// follows Gecko's fixed-length, percentage, then equal fallback order. A
/// fixed-length over-constraint grows the table, while percentage constraints
/// may shrink proportionally to keep the specified table width.
fn distribute_fixed_table_columns(target: f32, columns: &[FixedTableColumn]) -> Vec<f32> {
    if columns.is_empty() {
        return Vec::new();
    }
    let target = target.max(0.0);
    let mut widths: Vec<f32> = columns
        .iter()
        .map(|column| {
            if column.specified {
                column.length.max(0.0) + column.percentage.max(0.0) * target
            } else {
                0.0
            }
        })
        .collect();
    let mut total: f32 = widths.iter().sum();

    // Percentage columns are the flexible part of an over-constrained fixed
    // table. Never shrink the absolute component: if lengths alone do not fit,
    // the table's used width grows to contain them.
    if total > target {
        let percentage_total: f32 = columns
            .iter()
            .map(|column| column.percentage.max(0.0) * target)
            .sum();
        let shrink = (total - target).min(percentage_total);
        if shrink > 0.0 && percentage_total > 0.0 {
            for (width, column) in widths.iter_mut().zip(columns) {
                let contribution = column.percentage.max(0.0) * target;
                *width = (*width - shrink * contribution / percentage_total).max(0.0);
            }
            total -= shrink;
        }
    }

    let remaining = (target - total).max(0.0);
    if remaining <= f32::EPSILON {
        return widths;
    }
    let unresolved: Vec<usize> = columns
        .iter()
        .enumerate()
        .filter_map(|(index, column)| (!column.specified).then_some(index))
        .collect();
    if !unresolved.is_empty() {
        let share = remaining / unresolved.len() as f32;
        for index in unresolved {
            widths[index] = share;
        }
        return widths;
    }

    let mut weights: Vec<f32> = columns
        .iter()
        .map(|column| column.length.max(0.0))
        .collect();
    let mut weight: f32 = weights.iter().sum();
    if weight <= f32::EPSILON {
        weights = columns
            .iter()
            .map(|column| column.percentage.max(0.0))
            .collect();
        weight = weights.iter().sum();
    }
    if weight <= f32::EPSILON {
        weights.fill(1.0);
        weight = columns.len() as f32;
    }
    for (width, column_weight) in widths.iter_mut().zip(weights) {
        *width += remaining * column_weight / weight;
    }
    widths
}

/// Build a `<table>` as a CSS grid. Modeling the table as a grid is what makes
/// columns negotiate a shared width across every row (min-content/max-content
/// track sizing), which the old flex-row-per-`<tr>` stack could not do: each
/// row sized its cells independently, so columns drifted and never lined up.
/// Cells (`<td>`/`<th>`) become grid items placed by (row, column) with
/// colspan/rowspan mapped to grid spans; `<tr>`/`<tbody>`/`<thead>`/`<tfoot>`
/// do not get their own layout boxes (their backgrounds are not modeled yet).
/// The grid node is created at width:auto here; its final width is set later in
/// `layout_dom` by a two-pass intrinsic measurement (see the table-sizing pass)
/// so the table can grow to fit an unbreakable wide cell the way real tables do.
/// Returns `None` (falling back to the generic path) if the table has no cells.
fn build_table(
    tree: &DomTree,
    id: NodeId,
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &mut HashMap<taffy::NodeId, NodeId>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Option<taffy::NodeId> {
    let style = styles.get(&id)?;
    let native_html_table = tree.get_node(id).is_some_and(|node| {
        node.as_element()
            .is_some_and(|element| element.local.as_ref() == "table")
    });
    let authored_row_children = if native_html_table {
        None
    } else {
        let children = rendered_children(tree, id);
        let mut flattened = Vec::new();
        flatten_contents_children(tree, &children, styles, &mut flattened);
        // Full anonymous-table fixup is not represented yet. Falling back to
        // ordinary box construction preserves every child; partially taking
        // the dedicated path would silently discard direct text/non-cell
        // boxes once one real table cell made the build succeed.
        let all_rendered_children_are_cells = flattened.iter().all(|child| {
            let hidden = styles
                .get(child)
                .is_some_and(|child_style| child_style.display == crate::Display::None);
            let ignorable_whitespace = tree.get_node(*child).is_some_and(|node| {
                !node.is_element() && tree.text_content(*child).trim().is_empty()
            });
            hidden
                || ignorable_whitespace
                || styles
                    .get(child)
                    .is_some_and(|child_style| child_style.is_table_cell_box)
        });
        if !all_rendered_children_are_cells {
            return None;
        }
        Some(flattened)
    };
    let mut rows: Vec<(NodeId, usize)> = Vec::new();
    if native_html_table {
        collect_table_rows(tree, id, &mut rows);
        if rows.is_empty() {
            return None;
        }
    } else {
        // CSS table fixup inserts an anonymous row around table-cell children
        // that are direct children of a table. The grid representation does
        // not need a material row node, but retaining one logical row gives
        // those cells the same shared-track negotiation as native cells.
        rows.push((id, 1));
    }
    // Bounds so a crafted table cannot exhaust memory or time: `colspan`/
    // `rowspan` and the column count are page-controlled, and the occupancy
    // fill is O(rows x cols) native code that neither the V8 watchdog nor a
    // tokio timeout can interrupt. Real tables never approach these. Rows are
    // capped too, both to bound work and to keep grid line/span indices within
    // taffy's i16/u16 range.
    const MAX_SPAN: usize = 1000;
    const MAX_COLS: usize = 1024;
    const MAX_ROWS: usize = 10000;
    if rows.len() > MAX_ROWS {
        rows.truncate(MAX_ROWS);
    }
    let nrows = rows.len();

    // Assign every cell a (row, column) with a rowspan-occupancy grid so a cell
    // that spans down pushes later rows' cells past the columns it still covers.
    let span_attr = |cid: NodeId, name: &str| -> usize {
        tree.get_node(cid)
            .and_then(|n| {
                n.get_attribute(name)
                    .and_then(|v| v.trim().parse::<usize>().ok())
            })
            .unwrap_or(1)
    };
    let mut occupied: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut placed: Vec<(NodeId, usize, usize, usize, usize)> = Vec::new();
    let mut ncols = 0usize;
    for (r, &(tr, group_end)) in rows.iter().enumerate() {
        let mut c = 0usize;
        let row_children = if native_html_table {
            tree.children(tr)
        } else {
            authored_row_children.clone().unwrap_or_default()
        };
        for cid in row_children {
            let local = tree
                .get_node(cid)
                .and_then(|n| n.as_element().map(|e| e.local.to_string()));
            let is_cell = if native_html_table {
                matches!(local.as_deref(), Some("td") | Some("th"))
            } else {
                styles
                    .get(&cid)
                    .is_some_and(|cell| cell.is_table_cell_box)
            };
            if !is_cell {
                continue;
            }
            // A hidden cell is removed from the table model entirely (it must
            // not reserve a column slot, or the surviving cells shift right).
            if styles
                .get(&cid)
                .map(|s| s.display == crate::Display::None)
                .unwrap_or(false)
            {
                continue;
            }
            while occupied.contains(&(r, c)) {
                c += 1;
            }
            if c >= MAX_COLS {
                break;
            }
            let cs = if native_html_table {
                span_attr(cid, "colspan").clamp(1, MAX_SPAN)
            } else {
                1
            };
            // rowspan=0 means "span to the end of this row group". Explicit
            // spans are clipped at the same boundary by the effective cell map.
            let rs_raw = if native_html_table {
                span_attr(cid, "rowspan")
            } else {
                1
            };
            let rows_left_in_group = group_end.min(nrows).saturating_sub(r).max(1);
            let rs = if rs_raw == 0 {
                rows_left_in_group
            } else {
                rs_raw
            }
            .clamp(1, rows_left_in_group);
            for dr in 0..rs {
                for dc in 0..cs {
                    occupied.insert((r + dr, c + dc));
                }
            }
            placed.push((cid, r, c, rs, cs));
            c += cs;
            ncols = ncols.max(c).min(MAX_COLS);
        }
    }
    if placed.is_empty() || ncols == 0 {
        return None;
    }

    // Build each cell and pin it to its grid area.
    let mut children: Vec<taffy::NodeId> = Vec::new();
    for (cid, r, c, rs, cs) in &placed {
        let Some(cell_node) = build(
            tree, *cid, taffy_tree, id_map, words, engine, ifc_items, styles,
        ) else {
            continue;
        };
        if let Ok(cur) = taffy_tree.style(cell_node) {
            let mut cstyle = cur.clone();
            cstyle.grid_row = taffy::Line {
                start: line((*r as i16) + 1),
                end: span(*rs as u16),
            };
            cstyle.grid_column = taffy::Line {
                start: line((*c as i16) + 1),
                end: span(*cs as u16),
            };
            // Grid does the sizing; a leftover flex_grow from the flex-table
            // heuristic would be ignored anyway, but clear it to be explicit.
            cstyle.flex_grow = 0.0;
            // A cell's specified width sizes its COLUMN (fed into the track
            // by the pre-pass above); the cell box itself always fills its
            // grid area. Left in place, a `width:50%` cell would shrink to
            // half of its own already-halved track.
            cstyle.size.width = Dimension::auto();
            // Taffy's grid-item automatic minimum is an engine artifact here:
            // the table track has already collected the cell's min-content
            // contribution and owns final column sizing. Remove only the
            // initial `auto` minimum at translation time; an authored
            // min-width remains a real constraint and computed style stays
            // declaration-order independent.
            if styles
                .get(cid)
                .is_some_and(|cell_style| cell_style.min_width == crate::Dimension::Auto)
            {
                cstyle.min_size.width = taffy::Dimension::length(0.0);
            }
            let _ = taffy_tree.set_style(cell_node, cstyle);
        }
        children.push(cell_node);
    }
    if children.is_empty() {
        return None;
    }

    // Column sizing pre-pass: specified widths on `<col>` elements and on
    // colspan-1 cells feed the tracks, so author column sizing actually
    // applies (a `td{width:200px}` must size the COLUMN, across every row).
    // A percent width becomes a percent track (resolving against the table),
    // a px width caps the track at that length (min-content still protects
    // the content), and unspecified columns keep content sizing with an
    // `auto` max so they stretch to fill a definite table width instead of
    // leaving a dead strip of bare table background.
    let mut col_px: Vec<Option<f32>> = vec![None; ncols];
    let mut col_pct: Vec<Option<f32>> = vec![None; ncols];
    let fixed_layout = style.table_layout_fixed
        && matches!(style.width, crate::Dimension::Px(_) | crate::Dimension::Percent(_));
    let mut fixed_columns = vec![FixedTableColumn::default(); ncols];
    let attr_width = |cid: NodeId| -> (Option<f32>, Option<f32>) {
        let Some(v) = tree
            .get_node(cid)
            .and_then(|n| n.get_attribute("width").map(|s| s.trim().to_string()))
        else {
            return (None, None);
        };
        if let Some(p) = v
            .strip_suffix('%')
            .and_then(|s| s.trim().parse::<f32>().ok())
        {
            (None, Some(p / 100.0))
        } else {
            (v.trim_end_matches("px").trim().parse::<f32>().ok(), None)
        }
    };
    let style_width = |cid: NodeId| -> (Option<f32>, Option<f32>) {
        match styles.get(&cid).map(|s| s.width) {
            Some(crate::Dimension::Px(w)) if w > 0.0 => (Some(w), None),
            Some(crate::Dimension::Percent(p)) if p > 0.0 => (None, Some(p)),
            _ => attr_width(cid),
        }
    };
    let fixed_style_width = |cid: NodeId| -> (Option<f32>, Option<f32>) {
        match styles.get(&cid).map(|s| s.width) {
            Some(crate::Dimension::Px(w)) if w >= 0.0 => (Some(w), None),
            Some(crate::Dimension::Percent(p)) if p >= 0.0 => (None, Some(p)),
            _ => attr_width(cid),
        }
    };
    // <col> elements (direct or under <colgroup>), each spanning `span` columns.
    let mut next_col = 0usize;
    let mut col_elems: Vec<NodeId> = Vec::new();
    for cid in tree.children(id) {
        match tree
            .get_node(cid)
            .and_then(|n| n.as_element().map(|e| e.local.to_string()))
            .as_deref()
        {
            Some("col") => col_elems.push(cid),
            Some("colgroup") => {
                for gc in tree.children(cid) {
                    if tree
                        .get_node(gc)
                        .and_then(|n| n.as_element().map(|e| e.local.as_ref() == "col"))
                        .unwrap_or(false)
                    {
                        col_elems.push(gc);
                    }
                }
            }
            _ => {}
        }
    }
    for col_el in &col_elems {
        let span = tree
            .get_node(*col_el)
            .and_then(|n| {
                n.get_attribute("span")
                    .and_then(|v| v.trim().parse::<usize>().ok())
            })
            .unwrap_or(1)
            .clamp(1, MAX_SPAN);
        let (px, pct) = style_width(*col_el);
        let (fixed_px, fixed_pct) = fixed_style_width(*col_el);
        for _ in 0..span {
            if next_col >= ncols {
                break;
            }
            col_px[next_col] = px;
            col_pct[next_col] = pct;
            if fixed_layout && (fixed_px.is_some() || fixed_pct.is_some()) {
                fixed_columns[next_col] = FixedTableColumn {
                    length: fixed_px.unwrap_or(0.0),
                    percentage: fixed_pct.unwrap_or(0.0),
                    specified: true,
                };
            }
            next_col += 1;
        }
    }
    // colspan-1 cells override <col> (they are closer to the content).
    for (cid, _r, c, _rs, cs) in &placed {
        if *cs != 1 || *c >= ncols {
            continue;
        }
        let (mut px, pct) = style_width(*cid);
        // A fixed width declared on a cell describes its content box unless
        // the author opted into border-box. Grid tracks describe the cell's
        // outer border box, so carry padding and border into the pinned track.
        // `<col>` widths above already describe the column track and must not
        // receive a particular cell's box edges.
        if let (Some(w), Some(s)) = (px, styles.get(cid)) {
            if s.box_sizing == crate::BoxSizing::ContentBox {
                px = Some(w + s.padding.left + s.padding.right + s.border.left + s.border.right);
            }
        }
        if let Some(w) = px {
            col_px[*c] = Some(col_px[*c].map_or(w, |cur| cur.max(w)));
        }
        if let Some(p) = pct {
            col_pct[*c] = Some(col_pct[*c].map_or(p, |cur| cur.max(p)));
        }
    }

    // Under fixed table layout only the first row contributes cell widths,
    // and an explicit `<col>` constraint wins for every covered column. A
    // spanning cell's outer width is split with the inter-column spacing
    // removed, matching Gecko's `((width + spacing) / span) - spacing` rule.
    if fixed_layout {
        let (horizontal_spacing, _) = table_spacing(style);
        for (cid, row, start, _rowspan, colspan) in &placed {
            if *row != 0 || *start >= ncols {
                continue;
            }
            let (px, pct) = fixed_style_width(*cid);
            if px.is_none() && pct.is_none() {
                continue;
            }
            let span = (*colspan).min(ncols - *start).max(1);
            let cell_style = styles.get(cid);
            let edges = cell_style
                .filter(|cell| cell.box_sizing == crate::BoxSizing::ContentBox)
                .map(|cell| {
                    cell.padding.left
                        + cell.padding.right
                        + cell.border.left
                        + cell.border.right
                })
                .unwrap_or(0.0);
            let outer_length = px.unwrap_or(0.0) + edges;
            let per_column_length =
                ((outer_length + horizontal_spacing) / span as f32 - horizontal_spacing)
                    .max(0.0);
            let per_column_percentage = pct.unwrap_or(0.0).max(0.0) / span as f32;
            for column in &mut fixed_columns[*start..*start + span] {
                if !column.specified {
                    *column = FixedTableColumn {
                        length: per_column_length,
                        percentage: per_column_percentage,
                        specified: true,
                    };
                }
            }
        }
    }

    // Row sizing: a `height` on the row or a rowspan-1 cell is a MINIMUM
    // (content can always grow a row), matching how tables treat heights.
    let mut row_min: Vec<Option<f32>> = vec![None; nrows];
    if native_html_table {
        for (r, &(tr, _)) in rows.iter().enumerate() {
            if let Some(crate::Dimension::Px(h)) = styles.get(&tr).map(|s| s.height) {
                if h > 0.0 {
                    row_min[r] = Some(h);
                }
            }
        }
    }
    for (cid, r, _c, rs, _cs) in &placed {
        if *rs != 1 {
            continue;
        }
        if let Some(crate::Dimension::Px(h)) = styles.get(cid).map(|s| s.height) {
            if h > 0.0 {
                row_min[*r] = Some(row_min[*r].map_or(h, |cur| cur.max(h)));
            }
        }
    }

    let col = |i: usize| {
        if fixed_layout {
            return taffy::GridTemplateComponent::Single(taffy::MinMax {
                min: taffy::MinTrackSizingFunction::length(0.0),
                max: taffy::MaxTrackSizingFunction::length(0.0),
            });
        }
        let max = if let Some(p) = col_pct[i] {
            taffy::MaxTrackSizingFunction::percent(p)
        } else if let Some(px) = col_px[i] {
            taffy::MaxTrackSizingFunction::length(px)
        } else {
            taffy::MaxTrackSizingFunction::auto()
        };
        taffy::GridTemplateComponent::Single(taffy::MinMax {
            min: taffy::MinTrackSizingFunction::min_content(),
            max,
        })
    };
    let row_track = |r: usize| {
        let min = match row_min[r] {
            Some(h) => taffy::MinTrackSizingFunction::length(h),
            None => taffy::MinTrackSizingFunction::auto(),
        };
        taffy::GridTemplateComponent::Single(taffy::MinMax {
            min,
            max: taffy::MaxTrackSizingFunction::auto(),
        })
    };
    // In the separate-border model, border-spacing also exists between the
    // table edge and the first/last row and column. Grid `gap` only covers
    // interior tracks, so model the two outer spacing bands as internal
    // layout padding while leaving the computed CSS padding unchanged.
    let (horizontal_spacing, vertical_spacing) = table_spacing(style);
    let mut grid_style = style.clone();
    grid_style.padding.left += horizontal_spacing;
    grid_style.padding.right += horizontal_spacing;
    grid_style.padding.top += vertical_spacing;
    grid_style.padding.bottom += vertical_spacing;
    let mut tstyle = to_taffy_style(&grid_style);
    tstyle.display = Display::Grid;
    // A percentage width resolves against the container, so keep it and let the
    // used-width pass leave it to taffy. Any other width (px or auto) is forced
    // to auto here so that pass can measure content before choosing the width.
    if !matches!(style.width, crate::Dimension::Percent(_)) {
        tstyle.size.width = Dimension::auto();
    }
    tstyle.grid_template_columns = (0..ncols).map(col).collect();
    tstyle.grid_template_rows = (0..nrows).map(row_track).collect();
    tstyle.gap = taffy::Size {
        width: length(horizontal_spacing),
        height: length(vertical_spacing),
    };
    let table_node = taffy_tree.new_with_children(tstyle, &children).ok()?;
    id_map.insert(table_node, id);
    ifc_items.table_rows.insert(table_node, row_min);
    if fixed_layout {
        ifc_items.fixed_table_cols.insert(table_node, fixed_columns);
    }
    if col_px.iter().any(Option::is_some) || col_pct.iter().any(Option::is_some) {
        ifc_items.table_cols.insert(table_node, (col_px, col_pct));
    }
    Some(table_node)
}

/// Effective size-query containment for this generated box.
///
/// CSS size containment has no effect on a non-atomic inline box or an
/// internal table box. Keeping the check centralized also prevents those
/// boxes from exposing queryable size axes through `container_snapshot`.
fn effective_container_type(style: &crate::LayoutStyle) -> crate::ContainerType {
    if style.internal_flex_container
        || style.is_table_box
        || (style.display == crate::Display::Inline && !style.is_inline_block)
    {
        crate::ContainerType::Normal
    } else {
        style.container_type
    }
}

#[inline]
fn used_flex_alignment(
    value: Option<taffy::AlignSelf>,
    parent: Option<taffy::AlignItems>,
) -> taffy::AlignSelf {
    value
        .or(parent)
        .unwrap_or(taffy::AlignSelf::NORMAL)
        .resolve_normal(taffy::AlignSelf::STRETCH)
}

#[inline]
fn used_grid_alignments(
    style: &crate::LayoutStyle,
    parent: &crate::LayoutStyle,
) -> (taffy::AlignSelf, taffy::AlignSelf) {
    let horizontal = style
        .justify_self
        .or(parent.justify_items)
        .unwrap_or(taffy::AlignSelf::NORMAL);
    let vertical = style
        .align_self
        .or(parent.align_items)
        .unwrap_or(taffy::AlignSelf::NORMAL);
    if style.has_replaced_sizing {
        return (
            horizontal.resolve_normal(taffy::AlignSelf::START),
            vertical.resolve_normal(taffy::AlignSelf::START),
        );
    }
    if style.aspect_ratio.is_none() {
        return (
            horizontal.resolve_normal(taffy::AlignSelf::STRETCH),
            vertical.resolve_normal(taffy::AlignSelf::STRETCH),
        );
    }

    let horizontal_is_normal = horizontal == taffy::AlignSelf::NORMAL;
    let vertical_is_normal = vertical == taffy::AlignSelf::NORMAL;
    let horizontal = if horizontal_is_normal {
        if !vertical_is_normal && vertical == taffy::AlignSelf::STRETCH {
            taffy::AlignSelf::START
        } else {
            taffy::AlignSelf::STRETCH
        }
    } else {
        horizontal
    };
    let vertical = if vertical_is_normal {
        if !horizontal_is_normal && horizontal != taffy::AlignSelf::STRETCH {
            taffy::AlignSelf::STRETCH
        } else {
            taffy::AlignSelf::START
        }
    } else {
        vertical
    };
    (horizontal, vertical)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContainerAutoInlineSize {
    FillAvailable,
    Intrinsic,
    StretchedGridItem,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContainerAutoBlockSize {
    Intrinsic,
    StretchedGridItem,
}

/// Assign a native control's intrinsic border-box size to its auto axes.
/// The intrinsic figures are border boxes (Chromium's control metrics), so
/// a content-box control receives the content part. An authored width or
/// height keeps winning, as does a grid axis that stretches the item:
/// only an untouched auto axis adopts the intrinsic value.
fn assign_native_control_size(
    style: &mut crate::LayoutStyle,
    stretched_grid_item: (bool, bool),
    intrinsic_width: f32,
    intrinsic_height: f32,
    horizontal_edges: f32,
    vertical_edges: f32,
) {
    let (stretch_inline, stretch_block) = stretched_grid_item;
    let content_box = style.box_sizing == crate::BoxSizing::ContentBox;
    if style.width == crate::Dimension::Auto && !stretch_inline {
        let declared = if content_box {
            (intrinsic_width - horizontal_edges).max(0.0)
        } else {
            intrinsic_width
        };
        style.width = crate::Dimension::Px(declared);
    }
    if style.height == crate::Dimension::Auto && !stretch_block {
        let declared = if content_box {
            (intrinsic_height - vertical_edges).max(0.0)
        } else {
            intrinsic_height
        };
        style.height = crate::Dimension::Px(declared);
    }
}

/// Classify how an auto inline-size is resolved. Stretched grid items need
/// their intrinsic contribution contained without replacing the final auto
/// size that stretch alignment consumes.
fn container_auto_inline_size(
    tree: &DomTree,
    id: NodeId,
    style: &crate::LayoutStyle,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> ContainerAutoInlineSize {
    // Grid items are blockified and their float is ignored. Their original
    // inline-block/float provenance must not suppress grid self-alignment.
    if (style.is_inline_block || style.float.is_some())
        && !is_in_flow_grid_item(tree, id, style, styles)
    {
        return ContainerAutoInlineSize::Intrinsic;
    }
    if matches!(style.position, Some(taffy::Position::Absolute)) {
        // An absolutely positioned auto width is fill-available only when
        // both inline insets are definite.
        return if style.inset[1].is_none() || style.inset[3].is_none() {
            ContainerAutoInlineSize::Intrinsic
        } else {
            ContainerAutoInlineSize::FillAvailable
        };
    }

    let mut parent = rendered_parent(tree, id);
    let parent_style = loop {
        let Some(parent_id) = parent else {
            return ContainerAutoInlineSize::FillAvailable;
        };
        let Some(parent_style) = styles.get(&parent_id) else {
            return ContainerAutoInlineSize::FillAvailable;
        };
        if parent_style.display_contents {
            parent = rendered_parent(tree, parent_id);
            continue;
        }
        break parent_style;
    };
    match parent_style.display {
        crate::Display::Flex => {
            let row = !matches!(
                parent_style.flex_direction,
                Some(taffy::FlexDirection::Column | taffy::FlexDirection::ColumnReverse)
            );
            if row {
                ContainerAutoInlineSize::Intrinsic
            } else if used_flex_alignment(style.align_self, parent_style.align_items)
                == taffy::AlignSelf::STRETCH
            {
                ContainerAutoInlineSize::FillAvailable
            } else {
                ContainerAutoInlineSize::Intrinsic
            }
        }
        crate::Display::Grid => {
            if used_grid_alignments(style, parent_style).0 == taffy::AlignSelf::STRETCH
                && !style.margin_auto[1]
                && !style.margin_auto[3]
            {
                ContainerAutoInlineSize::StretchedGridItem
            } else {
                ContainerAutoInlineSize::Intrinsic
            }
        }
        crate::Display::Inline => ContainerAutoInlineSize::Intrinsic,
        _ => ContainerAutoInlineSize::FillAvailable,
    }
}

/// Classify an auto block-size for size containment. A stretched grid item
/// must keep its auto size so final alignment can fill a definite grid area;
/// only its intrinsic track contribution is contained.
fn container_auto_block_size(
    tree: &DomTree,
    id: NodeId,
    style: &crate::LayoutStyle,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> ContainerAutoBlockSize {
    // Absolutely positioned descendants are not grid items. Preserve the
    // existing contained intrinsic-height behavior until their independent
    // inset-based fill sizing is modeled explicitly.
    if matches!(style.position, Some(taffy::Position::Absolute)) {
        return ContainerAutoBlockSize::Intrinsic;
    }

    let mut parent = rendered_parent(tree, id);
    let parent_style = loop {
        let Some(parent_id) = parent else {
            return ContainerAutoBlockSize::Intrinsic;
        };
        let Some(parent_style) = styles.get(&parent_id) else {
            return ContainerAutoBlockSize::Intrinsic;
        };
        if parent_style.display_contents {
            parent = rendered_parent(tree, parent_id);
            continue;
        }
        break parent_style;
    };

    if parent_style.display == crate::Display::Grid
        && used_grid_alignments(style, parent_style).1 == taffy::AlignSelf::STRETCH
        && !style.margin_auto[0]
        && !style.margin_auto[2]
    {
        ContainerAutoBlockSize::StretchedGridItem
    } else {
        ContainerAutoBlockSize::Intrinsic
    }
}

/// Apply the used-size part of `container-type`'s implicit size containment.
///
/// The default contain-intrinsic-size is zero. Fill-available block boxes keep
/// their normal auto inline size, which is already descendant-independent.
/// Shrink-to-fit boxes instead receive a zero intrinsic inline size, and a
/// `size` container with auto block-size receives a zero intrinsic block size.
/// Descendants remain in the layout tree and may visibly overflow; size
/// containment is not paint containment and must not clip them.
fn apply_container_size_containment(
    tree: &DomTree,
    id: NodeId,
    style: &crate::LayoutStyle,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    taffy_style: &mut taffy::Style,
) {
    let kind = effective_container_type(style);
    if kind == crate::ContainerType::Normal {
        return;
    }

    if matches!(style.min_width, crate::Dimension::Auto) {
        taffy_style.min_size.width = taffy::Dimension::length(0.0);
    }
    let ratio_transfers_inline_size =
        style.aspect_ratio.is_some() && !matches!(style.height, crate::Dimension::Auto);
    if matches!(style.width, crate::Dimension::Auto) && !ratio_transfers_inline_size {
        match container_auto_inline_size(tree, id, style, styles) {
            ContainerAutoInlineSize::Intrinsic => {
                taffy_style.size.width = taffy::Dimension::length(0.0);
            }
            ContainerAutoInlineSize::StretchedGridItem => {
                taffy_style.intrinsic_size_containment.width = true;
            }
            ContainerAutoInlineSize::FillAvailable => {}
        }
    }

    if kind == crate::ContainerType::Size {
        if matches!(style.min_height, crate::Dimension::Auto) {
            taffy_style.min_size.height = taffy::Dimension::length(0.0);
        }
        let ratio_transfers_block_size =
            style.aspect_ratio.is_some() && !matches!(style.width, crate::Dimension::Auto);
        if matches!(style.height, crate::Dimension::Auto) && !ratio_transfers_block_size {
            match container_auto_block_size(tree, id, style, styles) {
                ContainerAutoBlockSize::Intrinsic => {
                    taffy_style.size.height = taffy::Dimension::length(0.0);
                }
                ContainerAutoBlockSize::StretchedGridItem => {
                    taffy_style.intrinsic_size_containment.height = true;
                }
            }
        }
    }
}

#[derive(Clone)]
struct DeferredCyclicInlineSize {
    node: NodeId,
    flex_item: NodeId,
    slot: usize,
    source: DeferredCyclicInlineSource,
}

#[derive(Clone)]
enum DeferredCyclicInlineSource {
    Expression(String),
    Percent(f32),
}

#[derive(Clone, Copy)]
enum DeferredFlexReflowPhase {
    Layout,
    FitContent,
}

/// During a flex item's intrinsic-size calculation, percentages in descendant
/// inline sizes are cyclic: their containing block does not have its used
/// width yet. Resolving those percentages against the width inherited from a
/// more distant ancestor turns the temporary value into a permanent
/// min-content floor. Gecko instead measures the flex item first, then reflows
/// its descendants under the item's final main size.
///
/// Keep the non-percentage part for the intrinsic pass (percentage basis zero),
/// and remember enough information to resolve the complete expression after
/// flex sizing has selected the item's used width.
fn defer_cyclic_flex_inline_sizes(
    tree: &DomTree,
    styles: &mut HashMap<NodeId, crate::LayoutStyle>,
    root_fs: f32,
    vw: f32,
    vh: f32,
) -> Vec<DeferredCyclicInlineSize> {
    let candidates: Vec<(NodeId, usize, DeferredCyclicInlineSource)> = styles
        .iter()
        .filter(|(_, style)| !style.ignores_used_box_sizes())
        .flat_map(|(&id, style)| {
            [0usize, 2, 4].into_iter().filter_map(move |slot| {
                let functional = style.size_expressions[slot]
                    .as_ref()
                    .filter(|expression| expression.contains('%'))
                    .filter(|_| slot != 4 || !style.has_replaced_sizing)
                    .cloned()
                    .map(DeferredCyclicInlineSource::Expression);
                let dimension = match slot {
                    0 => style.width,
                    2 => style.min_width,
                    4 => style.max_width,
                    _ => unreachable!(),
                };
                functional
                    .or_else(|| match dimension {
                        crate::Dimension::Percent(percent)
                            if slot != 4 || !style.has_replaced_sizing =>
                        {
                            Some(DeferredCyclicInlineSource::Percent(percent))
                        }
                        _ => None,
                    })
                    .map(|source| (id, slot, source))
            })
        })
        .collect();
    let mut deferred = Vec::new();

    for (id, slot, source) in candidates {
        // Start at the expression's containing-box chain, not the sized
        // node itself. A percentage-sized direct child of a nested flex
        // container makes that container's contribution cyclic when the
        // container is itself an item in an outer flex row.
        let mut candidate = rendered_parent(tree, id);
        let mut flex_item = None;
        while let Some(item) = candidate {
            let mut parent = rendered_parent(tree, item);
            while let Some(parent_id) = parent {
                let Some(parent_style) = styles.get(&parent_id) else {
                    break;
                };
                if parent_style.display_contents {
                    parent = rendered_parent(tree, parent_id);
                    continue;
                }
                let row_flex = parent_style.display == crate::Display::Flex
                    && !parent_style.internal_flex_container
                    && !matches!(
                        parent_style.flex_direction,
                        Some(taffy::FlexDirection::Column | taffy::FlexDirection::ColumnReverse)
                    );
                let item_is_indefinite = styles.get(&item).map_or(false, |style| {
                    (!matches!(style.width, crate::Dimension::Px(_))
                        || style.size_expressions[0]
                            .as_deref()
                            .is_some_and(|expression| expression.contains('%')))
                        && style.float.is_none()
                        && !matches!(style.position, Some(taffy::Position::Absolute))
                });
                if row_flex && item_is_indefinite {
                    flex_item = Some(item);
                }
                break;
            }
            if flex_item.is_some() {
                break;
            }
            candidate = rendered_parent(tree, item);
        }
        let Some(flex_item) = flex_item else {
            continue;
        };

        let em = styles
            .get(&id)
            .and_then(|style| style.font_size)
            .unwrap_or(root_fs);
        let intrinsic = match &source {
            DeferredCyclicInlineSource::Expression(expression) => {
                crate::style::resolve_contextual_length(expression, em, root_fs, vw, vh, 0.0)
            }
            DeferredCyclicInlineSource::Percent(_) => Some(0.0),
        };
        let Some(intrinsic) = intrinsic
        else {
            continue;
        };
        if let Some(style) = styles.get_mut(&id) {
            let intrinsic = crate::Dimension::Px(intrinsic.max(0.0));
            match slot {
                0 => style.width = intrinsic,
                2 => style.min_width = intrinsic,
                // A cyclic percentage max-size behaves as its initial value
                // during intrinsic contribution sizing; treating 50% as a
                // zero maximum would erase real text/content minimums.
                4 => style.max_width = crate::Dimension::Auto,
                _ => unreachable!(),
            }
        }
        deferred.push(DeferredCyclicInlineSize {
            node: id,
            flex_item,
            slot,
            source,
        });
    }

    deferred
}

fn resolve_deferred_flex_inline_sizes<F>(
    tree: &DomTree,
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &HashMap<taffy::NodeId, NodeId>,
    styles: &mut HashMap<NodeId, crate::LayoutStyle>,
    deferred: &[DeferredCyclicInlineSize],
    root_fs: f32,
    vw: f32,
    vh: f32,
    mut relayout: F,
) -> bool
where
    F: FnMut(
        &mut TaffyTree<usize>,
        &HashMap<NodeId, crate::LayoutStyle>,
        DeferredFlexReflowPhase,
    ),
{
    if deferred.is_empty() {
        return false;
    }
    let taffy_by_dom: HashMap<NodeId, taffy::NodeId> =
        id_map.iter().map(|(&taffy, &dom)| (dom, taffy)).collect();

    // This is the final-reflow boundary: preserve the main size selected by
    // the outer flex algorithm while descendants are resolved against it.
    //
    // Do not read descendant containing blocks from this preliminary layout.
    // Their percentage sizes were deliberately neutralized for intrinsic flex
    // measurement, so an otherwise ordinary auto-width block can still have a
    // cached zero inline size here. Gecko's final flex-item reflow first
    // imposes the item's used inline size and only then constructs descendant
    // ReflowInputs. Mirror that ordering by fixing every affected flex item,
    // restoring percentages that Taffy can resolve natively, and laying out
    // once before resolving functional sizes.
    let flex_items: HashSet<NodeId> = deferred.iter().map(|entry| entry.flex_item).collect();
    for flex_item in flex_items {
        let Some(&taffy_id) = taffy_by_dom.get(&flex_item) else {
            continue;
        };
        let Ok(layout) = taffy_tree.layout(taffy_id) else {
            continue;
        };
        let Some(style) = styles.get(&flex_item) else {
            continue;
        };
        let horizontal_edges =
            style.padding.left + style.padding.right + style.border.left + style.border.right;
        let used_declaration = if style.box_sizing == crate::BoxSizing::ContentBox {
            (layout.size.width - horizontal_edges).max(0.0)
        } else {
            layout.size.width
        };
        if let Ok(current) = taffy_tree.style(taffy_id) {
            let mut fixed = current.clone();
            fixed.size.width = taffy::Dimension::length(used_declaration);
            let _ = taffy_tree.set_style(taffy_id, fixed);
        }
    }

    // Once the flex item's used inline size is definite, plain percentages are
    // no longer cyclic. Keep them typed in Taffy instead of flattening them to
    // pixels so later ancestor changes (including fit-content) automatically
    // propagate through nested 100%/400% descendants.
    for entry in deferred {
        let DeferredCyclicInlineSource::Percent(percent) = &entry.source else {
            continue;
        };
        if let Some(style) = styles.get_mut(&entry.node) {
            let value = crate::Dimension::Percent(*percent);
            match entry.slot {
                0 => style.width = value,
                2 => style.min_width = value,
                4 => style.max_width = value,
                _ => unreachable!(),
            }
        }
        let Some(&taffy_id) = taffy_by_dom.get(&entry.node) else {
            continue;
        };
        if let Ok(current) = taffy_tree.style(taffy_id) {
            let mut restored = current.clone();
            let value = taffy::Dimension::percent(*percent);
            match entry.slot {
                0 => restored.size.width = value,
                2 => restored.min_size.width = value,
                4 => restored.max_size.width = value,
                _ => unreachable!(),
            }
            let _ = taffy_tree.set_style(taffy_id, restored);
        }
    }
    relayout(taffy_tree, styles, DeferredFlexReflowPhase::Layout);

    // Shrink-to-fit ancestors must be finalized while functional descendant
    // widths still have their intrinsic-neutral declarations. Flattening a
    // calc(100% - x) first would let that provisional width inflate the
    // ancestor's max-content contribution, then permanently sample the wrong
    // containing block.
    relayout(taffy_tree, styles, DeferredFlexReflowPhase::FitContent);

    // Taffy cannot retain an arbitrary calc()/min()/max()/clamp() expression
    // in a box-size field. Resolve those expressions only after their parent
    // has final geometry. The ordering dependency is not general DOM depth:
    // only an ancestor whose own functional inline size is deferred can
    // change the basis sampled by a descendant entry. Memoize ranks across
    // rendered-parent chains so unrelated deep trees do not turn this pass
    // into O(entries * DOM depth), and reflow between dependency ranks.
    let functional_nodes: HashSet<NodeId> = deferred
        .iter()
        .filter(|entry| matches!(entry.source, DeferredCyclicInlineSource::Expression(_)))
        .map(|entry| entry.node)
        .collect();
    let mut rank_cache = HashMap::<NodeId, usize>::new();
    let mut functional: Vec<(usize, &DeferredCyclicInlineSize)> = deferred
        .iter()
        .filter(|entry| matches!(entry.source, DeferredCyclicInlineSource::Expression(_)))
        .map(|entry| {
            let mut chain = Vec::new();
            let mut current = entry.node;
            while !rank_cache.contains_key(&current) {
                chain.push(current);
                let Some(parent) = rendered_parent(tree, current) else {
                    break;
                };
                current = parent;
            }
            while let Some(node) = chain.pop() {
                let rank = rendered_parent(tree, node)
                    .map(|parent| {
                        rank_cache.get(&parent).copied().unwrap_or(0)
                            + usize::from(functional_nodes.contains(&parent))
                    })
                    .unwrap_or(0);
                rank_cache.insert(node, rank);
            }
            (rank_cache.get(&entry.node).copied().unwrap_or(0), entry)
        })
        .collect();
    functional.sort_by_key(|(rank, _)| *rank);

    let mut start = 0usize;
    while start < functional.len() {
        let current_rank = functional[start].0;
        let mut end = start + 1;
        while end < functional.len() && functional[end].0 == current_rank {
            end += 1;
        }

        for (_, entry) in &functional[start..end] {
            let mut containing = rendered_parent(tree, entry.node);
            let basis = loop {
                let Some(parent) = containing else {
                    break None;
                };
                let parent_style = styles.get(&parent);
                if parent_style.is_some_and(|style| style.display_contents) {
                    containing = rendered_parent(tree, parent);
                    continue;
                }
                let Some(&taffy_id) = taffy_by_dom.get(&parent) else {
                    containing = rendered_parent(tree, parent);
                    continue;
                };
                let Ok(layout) = taffy_tree.layout(taffy_id) else {
                    break None;
                };
                break Some(layout.content_box_width().max(0.0));
            };
            let Some(basis) = basis else {
                continue;
            };
            let em = styles
                .get(&entry.node)
                .and_then(|style| style.font_size)
                .unwrap_or(root_fs);
            let value = match &entry.source {
                DeferredCyclicInlineSource::Expression(expression) => {
                    crate::style::resolve_contextual_length(
                        expression, em, root_fs, vw, vh, basis,
                    )
                }
                DeferredCyclicInlineSource::Percent(_) => unreachable!(),
            };
            let Some(value) = value else {
                continue;
            };
            let value = value.max(0.0);
            if let Some(style) = styles.get_mut(&entry.node) {
                let value = crate::Dimension::Px(value);
                match entry.slot {
                    0 => style.width = value,
                    2 => style.min_width = value,
                    4 => style.max_width = value,
                    _ => unreachable!(),
                }
            }
            let Some(&taffy_id) = taffy_by_dom.get(&entry.node) else {
                continue;
            };
            if let Ok(current) = taffy_tree.style(taffy_id) {
                let mut resolved = current.clone();
                match entry.slot {
                    0 => resolved.size.width = taffy::Dimension::length(value),
                    2 => resolved.min_size.width = taffy::Dimension::length(value),
                    4 => resolved.max_size.width = taffy::Dimension::length(value),
                    _ => unreachable!(),
                }
                let _ = taffy_tree.set_style(taffy_id, resolved);
            }
        }
        relayout(taffy_tree, styles, DeferredFlexReflowPhase::Layout);
        start = end;
    }
    true
}

fn is_in_flow_grid_item(
    tree: &DomTree,
    id: NodeId,
    style: &crate::LayoutStyle,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> bool {
    if matches!(style.position, Some(taffy::Position::Absolute)) {
        return false;
    }
    let mut parent = rendered_parent(tree, id);
    loop {
        let Some(parent_id) = parent else {
            return false;
        };
        let Some(parent_style) = styles.get(&parent_id) else {
            return false;
        };
        if parent_style.display_contents {
            parent = rendered_parent(tree, parent_id);
            continue;
        }
        return parent_style.display == crate::Display::Grid;
    }
}

fn build(
    tree: &DomTree,
    id: NodeId,
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &mut HashMap<taffy::NodeId, NodeId>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Option<taffy::NodeId> {
    let node = tree.get_node(id)?;
    let _name = node.as_element()?;
    let style = styles.get(&id)?;
    if style.display == crate::Display::None {
        return None;
    }

    // Real table layout: every computed table box becomes a CSS grid so columns negotiate
    // across rows (the flex approximation could not) and colspan/rowspan map to
    // grid spans. Falls through to the generic path only if the table has no
    // usable rows/cells.
    if style.is_table_box {
        if let Some(node) = build_table(
            tree, id, taffy_tree, id_map, words, engine, ifc_items, styles,
        ) {
            return Some(node);
        }
    }

    let mut taffy_style = to_taffy_style(style);
    apply_container_size_containment(tree, id, style, styles, &mut taffy_style);

    // A non-stretched flex item in a column flex container uses fit-content
    // for its auto inline size. In a nested flex layout taffy can retain the
    // item's earlier max-content measurement after the outer row shrinks its
    // column, so direct prose stays one enormous line instead of being
    // remeasured at the final column width (MDN's contributor quote).
    //
    // Taffy has no fit-content box-size value. For the narrow text-container
    // case, a synthetic percentage max has the same final effect: it is
    // indefinite during intrinsic measurement, then caps the item to its
    // containing block once that width is known. The automatic min-content
    // size still wins for an unbreakable word, preserving intentional
    // overflow. Keep the workaround away from replaced/atomic boxes and
    // authored sizing constraints.
    if needs_column_flex_text_fit_content_cap(tree, id, style, styles) {
        taffy_style.max_size.width = taffy::Dimension::percent(1.0);
    }

    // A grid item's automatic minimum size is clamped by a definite
    // max-width. Taffy's intrinsic track pass cannot resolve a percentage max
    // until the track exists, so `max-width:100%` otherwise participates with
    // its full max-content minimum and circularly expands an implicit auto
    // track (a row of fixed gallery columns widened Tailwind's 1200px content
    // track to 1528px). Let the authored percentage remain relative to the
    // final grid area, but remove that circular automatic minimum. Do not
    // eagerly turn the percentage into viewport pixels: a grid area may be
    // narrower than its container because of gutters.
    if matches!(style.max_width, crate::Dimension::Percent(_))
        && matches!(style.min_width, crate::Dimension::Auto)
    {
        if is_in_flow_grid_item(tree, id, style, styles) {
            taffy_style.min_size.width = taffy::Dimension::length(0.0);
        }
    }

    // Outside a foldable inline run, BR is a zero-inline-size line
    // participant. The mixed-inline fallback adds a separate anonymous
    // full-width breaker after this mapped marker; keeping that control box
    // anonymous prevents its layout surrogate from leaking into CSSOM/paint.
    if _name.local.as_ref() == "br" {
        let height = crate::inline::used_line_height(style).max(0.0);
        taffy_style.size.width = taffy::style::Dimension::length(0.0);
        taffy_style.size.height = taffy::style::Dimension::length(height);
        taffy_style.flex_grow = 0.0;
        taffy_style.flex_shrink = 0.0;
        let leaf = taffy_tree.new_leaf(taffy_style).ok()?;
        id_map.insert(leaf, id);
        return Some(leaf);
    }

    // CSS has separate outer and inner display types, while taffy exposes one
    // display value. An inline-block normally needs our wrapping inline
    // approximation so it remains atomic in an inline formatting context.
    // As a direct flex/grid item, however, its outer display is blockified and
    // its flow-root inner display must stack block children. Apply the block
    // inner mode only in that formatting context; doing it globally turns
    // inline-block lists into one full-width item per line.
    if style.is_inline_block {
        let mut parent = rendered_parent(tree, id);
        let flex_or_grid_item = loop {
            let Some(parent_id) = parent else { break false };
            let Some(parent_style) = styles.get(&parent_id) else {
                break false;
            };
            if parent_style.display_contents {
                parent = rendered_parent(tree, parent_id);
                continue;
            }
            break matches!(
                parent_style.display,
                crate::Display::Flex | crate::Display::Grid
            ) && !parent_style.internal_flex_container;
        };
        if flex_or_grid_item {
            taffy_style.display = taffy::style::Display::Block;
            taffy_style.flex_wrap = taffy::FlexWrap::NoWrap;
        } else if style.width == crate::Dimension::Auto {
            // An auto-width inline-block shrink-fits to its max-content width
            // when that fits the available line. Taffy's wrapping flex
            // approximation otherwise chooses a min-content width first,
            // turning ordinary two-word buttons into two-line controls. Keep
            // wrapping for explicitly constrained inline-blocks and for ones
            // with real in-flow block children; those need their inner block
            // formatting rather than a single max-content line.
            let has_in_flow_block_child = rendered_children(tree, id)
                .iter()
                .any(|child| styles.get(child).map_or(false, is_in_flow_block_level));
            if !has_in_flow_block_child {
                taffy_style.flex_wrap = taffy::FlexWrap::NoWrap;
            }
        }
    }

    // An inline SVG is an atomic replaced box in the surrounding formatting
    // context. Its descendants paint inside its SVG viewport; they must not
    // become CSS layout children. In particular, a viewBox supplies an
    // intrinsic ratio whose auto axis is transferred from a percentage-sized
    // definite axis during the grid's final pass. Treating the SVG as a normal
    // empty container makes its intrinsic contribution zero during grid track
    // sizing, collapsing responsive logo walls to zero-height rows.
    if _name.local.as_ref() == "svg" {
        if let Some(ratio) = style
            .aspect_ratio
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
        {
            // A 100% inline size inside an auto grid track is cyclic
            // during intrinsic sizing: it behaves as auto for the contribution
            // pass, then fills the final track. Taffy 0.12 keeps the intrinsic
            // fallback width instead of revisiting that percentage. Express
            // the final fill as grid self-stretch so the known content width
            // reaches the replaced-item measure callback. This substitution is
            // only equivalent for an in-flow grid item at exactly 100%; an
            // abs-pos SVG or a smaller percentage must keep its containing-
            // block-relative authored size.
            if matches!(style.width, crate::Dimension::Percent(percent) if percent == 1.0)
                && matches!(style.height, crate::Dimension::Auto)
                && is_in_flow_grid_item(tree, id, style, styles)
            {
                taffy_style.size.width = taffy::Dimension::auto();
                taffy_style.min_size.width = taffy::Dimension::length(0.0);
                taffy_style.justify_self = Some(taffy::AlignSelf::STRETCH);
            }
            // A viewBox supplies a ratio but no intrinsic dimensions. The
            // 300px default inline size is CSS Images' default object size;
            // the ratio supplies the corresponding block size. Definite CSS
            // axes still arrive through `known` and override this fallback.
            let width = 300.0;
            let height = width / ratio;
            let context = engine.register_replaced(width, height, style);
            let leaf = taffy_tree
                .new_leaf_with_context(taffy_style, context)
                .ok()?;
            id_map.insert(leaf, id);
            return Some(leaf);
        }
    }

    // A loaded poster supplies `<video>`'s replaced-content dimensions before
    // decoded video metadata exists. Only use HTML's 300x150 fallback when no
    // poster intrinsic is available.
    if !(_name.local.as_ref() == "video" && style.replaced_intrinsic.is_some()) {
        if let Some((width, height)) = crate::inline::default_replaced_intrinsic_size(
            _name.local.as_ref(),
            style.font_size.unwrap_or(16.0),
            node.get_attribute("controls").is_some(),
            _name.local.as_ref() != "embed"
                || node
                    .get_attribute("src")
                    .is_some_and(|value| !value.trim().is_empty()),
        ) {
            let context = engine.register_replaced(width, height, style);
            let leaf = taffy_tree
                .new_leaf_with_context(taffy_style, context)
                .ok()?;
            id_map.insert(leaf, id);
            return Some(leaf);
        }
    }

    // A replaced image is a measured leaf, even when CSS gives it a percentage
    // width. Its intrinsic dimensions participate in an auto-sized ancestor's
    // max-content measurement; once the percentage axis becomes definite, the
    // measure callback derives the other axis through the intrinsic ratio.
    if matches!(_name.local.as_ref(), "img" | "video") {
        if let Some(intrinsic) = style.replaced_intrinsic.or_else(|| {
            style
                .intrinsic_size
                .map(|(width, height)| crate::ReplacedIntrinsic::from_dimensions(width, height))
        }) {
            let (width, height) = intrinsic.natural_size().unwrap_or((300.0, 150.0));
            let intrinsic_ratio = intrinsic.ratio.unwrap_or(2.0);
            let preferred_ratio = style
                .aspect_ratio
                .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
                .unwrap_or(intrinsic_ratio);

            // Taffy has one preferred-size field but CSS intrinsic sizing has
            // separate preferred and min-content contributions. For a proper
            // replaced element with a percentage maximum, encode the
            // equivalent min(preferred-width, percentage-max) function by
            // swapping the two operands: the percentage is indefinite during
            // intrinsic contribution sizing and resolves in final layout,
            // while the fixed maximum retains the replaced box's preferred
            // width. This applies to authored sizes and intrinsic auto sizes
            // alike; it is not tied to HTML width attributes.
            if let crate::Dimension::Percent(maximum) = style.max_width {
                let preferred_width = match style.width {
                    crate::Dimension::Px(width) => Some(width),
                    crate::Dimension::Auto => match style.height {
                        crate::Dimension::Px(height) => Some(height * preferred_ratio),
                        _ => Some(
                            crate::inline::constrained_auto_replaced_size(width, height, style)
                                .width,
                        ),
                    },
                    _ => None,
                };
                if let Some(preferred_width) = preferred_width {
                    taffy_style.size.width = taffy::Dimension::percent(maximum);
                    taffy_style.max_size.width = taffy::Dimension::length(preferred_width.max(0.0));
                }
            }

            // An auto/auto replaced box starts from its intrinsic width rather
            // than the fill-available width of an ordinary auto-width block.
            // Seed that preferred width before handing the leaf to taffy, or
            // block layout passes the containing-block width as a known
            // dimension and stretches a source-less/content:url image to the
            // viewport. Definite constraints additionally resolve CSS2's
            // ratio-preserving constraint table up front; without constraints,
            // leave height auto so flex shrink can still transfer through the
            // preferred aspect ratio.
            let has_definite_constraint = [
                style.min_width,
                style.min_height,
                style.max_width,
                style.max_height,
            ]
            .into_iter()
            .any(|dimension| matches!(dimension, crate::Dimension::Px(_)));
            let has_percentage_constraint = [
                style.min_width,
                style.min_height,
                style.max_width,
                style.max_height,
            ]
            .into_iter()
            .any(|dimension| matches!(dimension, crate::Dimension::Percent(_)))
                || style.size_expressions[2..=5]
                    .iter()
                    .flatten()
                    .any(|expression| expression.contains('%'));
            let ratio_only = intrinsic.width.is_none()
                && intrinsic.height.is_none()
                && intrinsic.ratio.is_some();
            if matches!(style.width, crate::Dimension::Auto)
                && matches!(style.height, crate::Dimension::Auto)
                && !has_percentage_constraint
                && !ratio_only
                && !is_in_flow_grid_item(tree, id, style, styles)
            {
                let constrained =
                    crate::inline::constrained_auto_replaced_size(width, height, style);
                taffy_style.size.width = taffy::Dimension::length(constrained.width);
                if has_definite_constraint {
                    taffy_style.size.height = taffy::Dimension::length(constrained.height);
                }
            }

            // When an auto axis has a definite min/max constraint, the
            // measured replaced leaf must own intrinsic-ratio transfer so it
            // can clamp the derived size. Leaving the same ratio on taffy's
            // style lets taffy synthesize that axis before measurement and
            // bypass the constraint (width:100%; height:auto;
            // max-height:200px became the uncapped intrinsic height).
            //
            // Keep taffy's ratio in the unconstrained case: percentage-sized
            // images in auto wrappers can be measured at intrinsic size before
            // their percentage width becomes definite, and taffy then needs
            // the ratio to transfer that final width to height.
            let measured_axis_constraint = (!matches!(style.height, crate::Dimension::Auto)
                && matches!(style.width, crate::Dimension::Auto)
                && matches!(
                    (style.min_width, style.max_width),
                    (crate::Dimension::Px(_), _) | (_, crate::Dimension::Px(_))
                ))
                || (!matches!(style.width, crate::Dimension::Auto)
                    && matches!(style.height, crate::Dimension::Auto)
                    && matches!(
                        (style.min_height, style.max_height),
                        (crate::Dimension::Px(_), _) | (_, crate::Dimension::Px(_))
                    ));
            if measured_axis_constraint {
                taffy_style.aspect_ratio = None;
            }
            let context = engine.register_replaced_intrinsic(intrinsic, style);
            let leaf = taffy_tree
                .new_leaf_with_context(taffy_style, context)
                .ok()?;
            id_map.insert(leaf, id);
            return Some(leaf);
        }
    }

    // If this container is a pure-text inline formatting context, collapse its
    // whole subtree to one leaf shaped and line-broken by cosmic-text (real
    // text layout), sized on demand by the measure function. This is the fast,
    // correct path for paragraphs/headings/labels/cells of text; the flex-wrap
    // approximation below only handles the leftovers (mixed inline + atomic
    // boxes, and layout-only builds where `try_build` always declines).
    let is_closed_html_details = _name.ns.as_ref() == "http://www.w3.org/1999/xhtml"
        && _name.local.as_ref() == "details"
        && node.get_attribute("open").is_none();
    if !is_closed_html_details && !ifc_items.float_aware_blocks.contains(&id) {
        if let Some(item) = engine.try_build(tree, id, styles) {
            if style.display == crate::Display::Block && style.width == crate::Dimension::Auto {
                // A pure-text block is still a fill-available block. Its shaped
                // inline context performs text alignment internally; retaining
                // the flex alignment stand-in here shrink-wraps the leaf to its
                // text, leaving no free width in which center/end can move it.
                taffy_style.display = taffy::style::Display::Block;
            }
            let leaf = taffy_tree.new_leaf_with_context(taffy_style, item).ok()?;
            id_map.insert(leaf, id);
            ifc_items.whole.insert(id, item);
            return Some(leaf);
        }
    }

    // Taffy has no real inline formatting context: its native Block layout
    // treats every direct child as its own full-width block box, stacked
    // vertically. That is correct when a block's children are other block
    // elements (a div full of divs), but wrong the moment a block holds
    // inline-level content (running text with <a>/<span>/<b> mixed in, i.e.
    // any ordinary paragraph or list item), where real CSS instead lays
    // consecutive inline boxes out left-to-right, wrapping at the container's
    // width. Approximate that inline formatting context by promoting such
    // blocks to a wrapping flex row, so text and inline elements actually
    // flow together on shared lines instead of each getting its own row.
    //
    // The same fix applies to `<td>`/`<th>` (and any other UA default that
    // stacks its children in a flex column rather than taffy's Block mode):
    // without it, a text node's per-word leaves (see `build_text_words`)
    // become direct children of a column-direction flex container and each
    // word lands on its own line, one word per row, instead of wrapping
    // several words per line the way inline text actually flows.
    let stacks_children_vertically = style.display == crate::Display::Block
        || (style.display == crate::Display::Flex
            && style.flex_direction == Some(taffy::FlexDirection::Column));
    let has_inline_ish_content = has_inline_content(tree, id, styles)
        || has_in_flow_generated_pseudo(style.before_pseudo.as_deref())
        || has_in_flow_generated_pseudo(style.after_pseudo.as_deref());

    let mut dom_children = rendered_children(tree, id);
    // A boxless inline wrapper is transparent to our approximated box tree.
    // Flatten it before deciding whether this block has mixed inline/block
    // content, not only later while building children. Otherwise
    // `<pre><code><span style="display:block">…` looks like one inline child
    // during classification, is promoted to a flex row, and only then exposes
    // the block spans—too late, so every line lands side by side. Real engines
    // split the inline around those blocks and make the blocks siblings in the
    // containing block formatting context.
    if style.display == crate::Display::Block || style.internal_flex_container {
        let mut flattened = Vec::new();
        flatten_boxless_inline_children(tree, &dom_children, styles, &mut flattened);
        dom_children = flattened;
    }
    let internal_mixed_block_flow = style.internal_flex_container
        && dom_children
            .iter()
            .any(|cid| styles.get(cid).map_or(false, is_in_flow_block_level));
    // In flex and grid formatting contexts, collapsible whitespace-only text
    // between items does not generate an anonymous item. Taffy places each
    // stray whitespace leaf in the item sequence, shifting every real child.
    // This is especially visible in pretty-printed HTML: indentation before a
    // flex child becomes several pixels of unexplained leading space.
    //
    // Flatten display:contents first because its children participate as direct
    // flex/grid items. Otherwise formatting whitespace inside the transparent
    // wrapper would survive this filter and become an item in `build_any`.
    if matches!(style.display, crate::Display::Flex | crate::Display::Grid)
        && !internal_mixed_block_flow
    {
        let mut flat = Vec::new();
        flatten_contents_children(tree, &dom_children, styles, &mut flat);
        dom_children = flat;
        dom_children.retain(|&cid| {
            tree.get_node(cid).map_or(false, |n| n.is_element())
                || !tree.text_content(cid).trim().is_empty()
        });
        // Flex and grid placement consume the order-modified document order.
        // Rust's stable sort preserves source order for equal values, exactly
        // the CSS tie-break. Taffy has no CSS `order` style field, so feeding
        // it the correctly ordered item sequence is the missing translation.
        dom_children.sort_by_key(|cid| styles.get(cid).map(|style| style.order).unwrap_or(0));
    } else if style.display == crate::Display::Block {
        // Formatting whitespace between block lines does not generate an
        // inline line box. Keeping a zero-height taffy leaf here still breaks
        // sibling margin adjacency, turning `margin-bottom:30px` followed by
        // `margin-top:40px` into 70px instead of the collapsed 40px. Preserve
        // whitespace only when this parent actually has inline content, where
        // the inter-element space belongs to the inline formatting context.
        if !has_inline_ish_content {
            dom_children.retain(|&cid| {
                tree.get_node(cid).map_or(false, |node| node.is_element())
                    || !tree.text_content(cid).trim().is_empty()
            });
        }
        // A float nested directly in a transparent inline wrapper (the common
        // `<a><img style="float:left"></a>` logo pattern) belongs to the
        // ancestor block formatting context. Gecko reparents that out-of-flow
        // frame to the BFC and leaves only a placeholder in the inline. Expose
        // the same item to our float-band builder instead of charging the
        // wrapper a full normal-flow row.
        for child in &mut dom_children {
            if let Some(float) = inline_wrapper_float(tree, *child, styles) {
                *child = float;
            }
        }
    }
    // Main-axis auto margins absorb positive free space before
    // `justify-content` is applied. Taffy 0.12 currently applies both: it
    // end-justifies the items and then adds the auto-margin space again,
    // placing later controls far outside the container (MDN's theme/language
    // controls landed beyond x=2100). Neutralize justify-content whenever an
    // in-flow flex item owns a main-axis auto margin; taffy then performs the
    // auto-margin distribution correctly.
    if style.display == crate::Display::Flex {
        let column = matches!(
            style.flex_direction,
            Some(taffy::FlexDirection::Column | taffy::FlexDirection::ColumnReverse)
        );
        let has_main_auto_margin = dom_children.iter().any(|cid| {
            styles.get(cid).map_or(false, |child| {
                if column {
                    child.margin_auto[0] || child.margin_auto[2]
                } else {
                    child.margin_auto[1] || child.margin_auto[3]
                }
            })
        });
        if has_main_auto_margin {
            taffy_style.justify_content = Some(taffy::JustifyContent::FLEX_START);
        }
    }
    // `float` has no effect on a flex or grid item. Legacy stylesheets often
    // leave floats on children after a newer rule turns their parent into a
    // flex container; routing those children through the block float-zone
    // approximation corrupts flex sizing and percentage-margin placement.
    let has_float_child = style.display == crate::Display::Block
        && dom_children
            .iter()
            .any(|&cid| styles.get(&cid).map(|s| s.float.is_some()).unwrap_or(false));
    let native_float_band =
        has_float_child && can_use_native_float_band(tree, style, &dom_children, styles);
    let has_in_flow_block_child = dom_children
        .iter()
        .any(|cid| styles.get(cid).map_or(false, is_in_flow_block_level));
    let is_internal_table_cell = style.is_table_cell_box && style.internal_flex_container;

    // `text-align` affects inline content, never the used width or placement
    // of an in-flow block child. `to_taffy_style` promotes a centered/right
    // block to a flex column as an inline-alignment stand-in; retaining that
    // promotion when the container has real block children makes every auto
    // width child shrink to max-content. That turns full-width paragraphs,
    // responsive picture wrappers, and rows of inline-block buttons into
    // narrow columns that wrap even though their containing block has room.
    //
    // Keep the legacy `<center>` behavior: unlike CSS text-align, that element
    // historically centers block descendants as well.
    if style.display == crate::Display::Block && has_in_flow_block_child && !style.legacy_center {
        taffy_style.display = taffy::style::Display::Block;
    }

    // A block with mixed inline + block children keeps real block layout:
    // block-level children become direct block children (full available
    // width, exactly what CSS block layout gives them), while each maximal
    // run of consecutive inline-level siblings is wrapped in one anonymous
    // block-level box that carries the inline formatting context. This is
    // how real engines structure mixed content, and it replaces the old
    // whole-container flex-row-wrap promotion, which collapsed contentless
    // block children (e.g. a `position:relative` hero wrapper whose only
    // child is out-of-flow) to width 0 and let text and blocks share lines.
    // Floats still take the legacy zone path below.
    if (style.display == crate::Display::Block
        || (is_internal_table_cell && has_in_flow_block_child))
        && has_inline_ish_content
        && !has_float_child
    {
        return build_mixed_block(
            tree,
            id,
            style,
            taffy_style,
            &dom_children,
            taffy_tree,
            id_map,
            words,
            engine,
            ifc_items,
            styles,
        );
    }

    if stacks_children_vertically
        && has_inline_ish_content
        && !native_float_band
        && !(has_float_child && has_in_flow_block_child)
    {
        taffy_style.display = taffy::style::Display::Flex;
        taffy_style.flex_direction = taffy::FlexDirection::Row;
        // The row-flex stand-in is our inline formatting context. A nowrap
        // table cell (Bootstrap input groups, segmented controls, toolbars)
        // must keep adjacent atomic inline children on one line; wrapping
        // them here doubles the table row and stretches every sibling cell.
        taffy_style.flex_wrap = if style.white_space == Some(crate::WhiteSpace::NoWrap) {
            taffy::FlexWrap::NoWrap
        } else {
            taffy::FlexWrap::Wrap
        };

        // The promoted container's main axis is horizontal, so text alignment
        // belongs on `justify_content`. Real `justify-content` from actual CSS
        // wins if present.
        if style.justify_content.is_none() {
            taffy_style.justify_content = match style.text_align {
                Some(taffy::AlignItems::FLEX_END) => Some(taffy::JustifyContent::FLEX_END),
                Some(taffy::AlignItems::CENTER) => Some(taffy::JustifyContent::CENTER),
                _ => taffy_style.justify_content,
            };
        }
        taffy_style.align_items = Some(taffy::AlignItems::FLEX_START);
    }

    if native_float_band {
        // Native float placement is only selected for the guarded flat-band
        // shape. It requires a real block formatting context on the parent.
        taffy_style.display = taffy::style::Display::Block;
    }

    let mut child_ids: Vec<taffy::NodeId> = if native_float_band {
        build_children_with_native_float_band(
            tree,
            id,
            style,
            &dom_children,
            taffy_tree,
            id_map,
            words,
            engine,
            ifc_items,
            styles,
        )
    } else if has_float_child {
        build_children_with_float_zone(
            tree,
            id,
            &dom_children,
            taffy_tree,
            id_map,
            words,
            engine,
            ifc_items,
            styles,
        )
    } else if matches!(style.display, crate::Display::Flex | crate::Display::Grid)
        && !style.internal_flex_container
    {
        build_flex_grid_children(
            tree,
            id,
            taffy_tree,
            id_map,
            words,
            engine,
            ifc_items,
            styles,
        )
    } else {
        dom_children
            .into_iter()
            .flat_map(|cid| {
                build_any(
                    tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                )
            })
            .collect()
    };
    if !native_float_band {
        if let Some((mut before, _)) = build_in_flow_pseudo(
            id,
            GeneratedBoxKind::Before,
            style.before_pseudo.as_deref(),
            taffy_tree,
            words,
            engine,
            ifc_items,
        ) {
            before.append(&mut child_ids);
            child_ids = before;
        }
        if let Some((mut after, _)) = build_in_flow_pseudo(
            id,
            GeneratedBoxKind::After,
            style.after_pseudo.as_deref(),
            taffy_tree,
            words,
            engine,
            ifc_items,
        ) {
            child_ids.append(&mut after);
        }
    }

    if style.internal_flex_container
        && taffy_style.display == taffy::style::Display::Flex
        && taffy_style.flex_direction == taffy::FlexDirection::Column
    {
        // Table cells and the other native block-layout stand-ins are not
        // genuine CSS flex containers. Their block children neither flex-shrink
        // to an authored container height nor shrink-wrap along the inline
        // axis. Preserving the children's block sizes is particularly
        // important for table cells, where `height` is only a row minimum and
        // taller content must grow the row.
        for child in &child_ids {
            let fills_inline_axis = id_map
                .get(child)
                .and_then(|dom_id| styles.get(dom_id))
                .map_or(false, |child_style| {
                    child_style.display == crate::Display::Block
                        && child_style.width == crate::Dimension::Auto
                        && child_style.float.is_none()
                        && !matches!(child_style.position, Some(taffy::Position::Absolute))
                });
            if let Ok(current) = taffy_tree.style(*child) {
                let mut block_item = current.clone();
                block_item.flex_shrink = 0.0;
                if fills_inline_axis {
                    block_item.size.width = taffy::style::Dimension::percent(1.0);
                }
                let _ = taffy_tree.set_style(*child, block_item);
            }
        }
    }

    let multicol_count = style
        .column_count
        .filter(|count| *count > 1)
        .map(usize::from);
    // Box-level balancing is sound only when every generated direct box is an
    // authored atomic fragment. Ordinary prose needs line-level fragmentation;
    // pretending its paragraph boxes are columns would be worse than the
    // current single-column fallback. This gate intentionally makes
    // `break-inside:avoid` the capability boundary until inline fragment
    // continuations exist.
    let atomic_multicol_children = !child_ids.is_empty()
        && child_ids.iter().all(|child| {
            id_map
                .get(child)
                .and_then(|dom_id| styles.get(dom_id))
                .map_or(false, |child_style| {
                    child_style.break_inside_avoid
                        && child_style.float.is_none()
                        && !matches!(child_style.position, Some(taffy::Position::Absolute))
                })
        });
    let taffy_id = if let Some(column_count) = multicol_count.filter(|_| atomic_multicol_children) {
        // A multicol container keeps its ordinary outer/block box, but its
        // inner formatting context is a horizontal sequence of equal-width
        // fragmentainers. Taffy does not expose CSS fragmentation, so build
        // those fragmentainers explicitly. The first preliminary layout puts
        // every child in column one, which measures each subtree at the exact
        // final column width. `apply_multicol_balance` then performs the
        // bounded Gecko-style feasible-height search and reparents the
        // already measured boxes before the final layout.
        taffy_style.display = taffy::style::Display::Flex;
        taffy_style.flex_direction = taffy::FlexDirection::Row;
        taffy_style.flex_wrap = taffy::FlexWrap::NoWrap;
        if style.column_gap.is_none() {
            taffy_style.gap.width =
                taffy::style::LengthPercentage::length(style.font_size.unwrap_or(16.0));
        }
        let column_style = taffy::Style {
            display: taffy::style::Display::Block,
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: taffy::style::Dimension::length(0.0),
            min_size: taffy::Size {
                width: taffy::style::Dimension::length(0.0),
                height: taffy::style::Dimension::auto(),
            },
            ..Default::default()
        };
        let mut columns = Vec::with_capacity(column_count);
        columns.push(
            taffy_tree
                .new_with_children(column_style.clone(), &child_ids)
                .ok()?,
        );
        for _ in 1..column_count {
            columns.push(taffy_tree.new_leaf(column_style.clone()).ok()?);
        }
        let multicol = taffy_tree.new_with_children(taffy_style, &columns).ok()?;
        ifc_items.multicol.push(MulticolBuild {
            columns,
            children: child_ids,
        });
        multicol
    } else if child_ids.is_empty() {
        taffy_tree.new_leaf(taffy_style).ok()?
    } else {
        taffy_tree.new_with_children(taffy_style, &child_ids).ok()?
    };
    id_map.insert(taffy_id, id);
    Some(taffy_id)
}

fn inline_wrapper_float(
    tree: &DomTree,
    wrapper: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Option<NodeId> {
    let wrapper_style = styles.get(&wrapper)?;
    if wrapper_style.display != crate::Display::Inline
        || wrapper_style.margin != crate::Edges::default()
        || wrapper_style.padding != crate::Edges::default()
        || wrapper_style
            .padding_percent
            .iter()
            .any(|value| value.is_some())
        || wrapper_style.border != crate::Edges::default()
        || wrapper_style.before_pseudo.is_some()
        || wrapper_style.after_pseudo.is_some()
    {
        return None;
    }
    let visible: Vec<NodeId> = tree
        .children(wrapper)
        .into_iter()
        .filter(|&child| {
            let Some(node) = tree.get_node(child) else {
                return false;
            };
            if !node.is_element() {
                return !tree.text_content(child).trim().is_empty();
            }
            styles
                .get(&child)
                .map(|style| style.display != crate::Display::None)
                .unwrap_or(false)
        })
        .collect();
    let [child] = visible.as_slice() else {
        return None;
    };
    styles
        .get(child)
        .and_then(|style| style.float)
        .map(|_| *child)
}

/// Build a block container whose children mix inline-level and block-level
/// content, preserving real block layout for the block children.
///
/// Structure produced (mirroring how CSS block containers actually work):
/// - block-level in-flow children stay direct children of the (taffy Block)
///   parent, so an auto width fills the containing block even when the child
///   has no in-flow content of its own;
/// - each maximal run of consecutive inline-level siblings becomes ONE
///   anonymous block-level box carrying the inline formatting context. A run
///   of pure text and foldable inline wrappers collapses to a single shaped
///   cosmic-text leaf (one taffy node for the whole run instead of one per
///   word: faster and lighter than the old whole-container promotion). Runs
///   holding atomic inline boxes (img, inline-block, ...) fall back to an
///   anonymous flex-wrap wrapper around the run's boxes;
/// - out-of-flow (absolutely positioned) children neither join nor break a
///   run; they are appended after the flow children so their containing
///   block is this parent, whose used width they resolve percentages against.
#[allow(clippy::too_many_arguments)]
fn build_mixed_block(
    tree: &DomTree,
    id: NodeId,
    style: &crate::LayoutStyle,
    mut taffy_style: taffy::Style,
    dom_children: &[NodeId],
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &mut HashMap<taffy::NodeId, NodeId>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Option<taffy::NodeId> {
    // The parent is a genuine block container. `to_taffy_style` may have
    // promoted it to a flex column for `text-align` (taffy block layout has
    // no align-items); undo that: text-align moves line content, never
    // block children, so it is applied to the runs below instead.
    taffy_style.display = taffy::style::Display::Block;

    // Splice display:contents children and drop nothing else, so runs
    // partition over the child list exactly as CSS sees it.
    let mut flat: Vec<NodeId> = Vec::new();
    flatten_contents_children(tree, dom_children, styles, &mut flat);

    enum Seg {
        Run(Vec<NodeId>),
        Block(NodeId),
    }
    let mut segs: Vec<Seg> = Vec::new();
    let mut out_of_flow: Vec<NodeId> = Vec::new();
    for &cid in &flat {
        let Some(node) = tree.get_node(cid) else {
            continue;
        };
        let is_text = matches!(node.data, obscura_dom::tree::NodeData::Text { .. });
        // Comments, doctypes, and other non-rendered DOM nodes generate no CSS
        // box and cannot interrupt an inline formatting context. Hydrating
        // frameworks place marker comments between adjacent inline controls;
        // treating each marker as a block segment split one button row into
        // multiple anonymous block wrappers.
        if !is_text && !node.is_element() {
            continue;
        }
        let inline_level = if is_text {
            true
        } else if let Some(s) = styles.get(&cid) {
            if s.display == crate::Display::None {
                continue;
            }
            if matches!(s.position, Some(taffy::Position::Absolute)) {
                out_of_flow.push(cid);
                continue;
            }
            let is_forced_break = node
                .as_element()
                .map_or(false, |element| element.local.as_ref() == "br");
            is_forced_break || crate::is_inline_level_box(s)
        } else {
            false
        };
        if inline_level {
            match segs.last_mut() {
                Some(Seg::Run(run)) => run.push(cid),
                _ => segs.push(Seg::Run(vec![cid])),
            }
        } else {
            segs.push(Seg::Block(cid));
        }
    }

    // Inline pseudos join the first/last inline run. A block-level pseudo is
    // a real direct child of this block and therefore sits outside the
    // anonymous run wrappers, exactly like an authored block child.
    let before = build_in_flow_pseudo(
        id,
        GeneratedBoxKind::Before,
        style.before_pseudo.as_deref(),
        taffy_tree,
        words,
        engine,
        ifc_items,
    );
    let after = build_in_flow_pseudo(
        id,
        GeneratedBoxKind::After,
        style.after_pseudo.as_deref(),
        taffy_tree,
        words,
        engine,
        ifc_items,
    );
    let (before_leaves, before_block) = before.unwrap_or_else(|| (Vec::new(), false));
    let (after_leaves, after_block) = after.unwrap_or_else(|| (Vec::new(), false));
    let mut before_pending = !before_block && !before_leaves.is_empty();
    let mut after_pending = !after_block && !after_leaves.is_empty();

    let n_segs = segs.len();
    let mut child_ids: Vec<taffy::NodeId> = Vec::new();
    if before_block {
        child_ids.extend(before_leaves.iter().copied());
    }
    for (i, seg) in segs.into_iter().enumerate() {
        match seg {
            Seg::Block(cid) => {
                let built = build_any(
                    tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                );
                if style.legacy_center {
                    let has_default_horizontal_margins = styles.get(&cid).map_or(false, |child| {
                        child.margin.left == 0.0
                            && child.margin.right == 0.0
                            && !child.margin_auto[1]
                            && !child.margin_auto[3]
                    });
                    if has_default_horizontal_margins {
                        for child in &built {
                            if let Ok(current) = taffy_tree.style(*child) {
                                let mut centered = current.clone();
                                centered.margin.left = taffy::style::LengthPercentageAuto::auto();
                                centered.margin.right = taffy::style::LengthPercentageAuto::auto();
                                let _ = taffy_tree.set_style(*child, centered);
                            }
                        }
                    }
                }
                child_ids.extend(built);
            }
            Seg::Run(run) => {
                let has_text_strut = run.iter().any(|&cid| {
                    tree.get_node(cid).map_or(false, |node| {
                        matches!(node.data, obscura_dom::tree::NodeData::Text { .. })
                    })
                });
                // Collapsible source formatting at the start/end of an inline
                // run does not create line width. Preserve whitespace between
                // inline siblings, but trim indentation adjacent to block
                // boundaries so pretty-printed markup starts at the line edge.
                let is_whitespace_text = |cid: NodeId| {
                    tree.get_node(cid).map_or(false, |node| {
                        matches!(node.data, obscura_dom::tree::NodeData::Text { .. })
                            && tree.text_content(cid).trim().is_empty()
                    })
                };
                let start = run
                    .iter()
                    .position(|&cid| !is_whitespace_text(cid))
                    .unwrap_or(run.len());
                let end = run
                    .iter()
                    .rposition(|&cid| !is_whitespace_text(cid))
                    .map(|index| index + 1)
                    .unwrap_or(start);
                let run = &run[start..end];
                let join_before = before_pending && i == 0;
                let join_after = after_pending && i + 1 == n_segs;
                // Fast path: the whole run folds to one shaped leaf, unless
                // pseudo-content word leaves must share its lines.
                if !join_before && !join_after {
                    if let Some(item) = engine.try_build_run(tree, id, run, styles) {
                        let leaf = taffy_tree
                            .new_leaf_with_context(run_leaf_style(), item)
                            .ok()?;
                        ifc_items.runs.entry(id).or_default().push(item);
                        child_ids.push(leaf);
                        continue;
                    }
                }
                let mut atoms: Vec<taffy::NodeId> = Vec::new();
                if join_before {
                    atoms.extend(before_leaves.iter().copied());
                    before_pending = false;
                }
                for &rc in run {
                    let is_forced_break = tree.get_node(rc).is_some_and(|node| {
                        node.as_element()
                            .is_some_and(|element| element.local.as_ref() == "br")
                    });
                    if is_forced_break {
                        // A BR participates in the current line with zero
                        // inline size, then forces the following content onto
                        // a new line. A single 100%-wide, line-height-tall flex
                        // item creates a phantom third line for A<br>B. Split
                        // those responsibilities: the mapped marker supplies
                        // the current line strut, while an anonymous 100%
                        // breaker may occupy its own zero-height flex line.
                        atoms.extend(build_any(
                            tree, rc, taffy_tree, id_map, words, engine, ifc_items, styles,
                        ));
                        let breaker_style = taffy::Style {
                            flex_grow: 0.0,
                            flex_shrink: 0.0,
                            flex_basis: taffy::style::Dimension::percent(1.0),
                            size: taffy::Size {
                                width: taffy::style::Dimension::percent(1.0),
                                height: taffy::style::Dimension::length(0.0),
                            },
                            ..Default::default()
                        };
                        atoms.push(taffy_tree.new_leaf(breaker_style).ok()?);
                    } else {
                        atoms.extend(build_any(
                            tree, rc, taffy_tree, id_map, words, engine, ifc_items, styles,
                        ));
                    }
                }
                if join_after {
                    atoms.extend(after_leaves.iter().copied());
                    after_pending = false;
                }
                if atoms.is_empty() {
                    // Whitespace-only run between blocks: no anonymous box.
                    continue;
                }
                let wrapper = taffy_tree
                    .new_with_children(run_wrapper_style(style, has_text_strut), &atoms)
                    .ok()?;
                child_ids.push(wrapper);
            }
        }
    }
    // Pseudo content that found no adjacent run to join.
    if before_pending {
        let wrapper = taffy_tree
            .new_with_children(run_wrapper_style(style, true), &before_leaves)
            .ok()?;
        child_ids.insert(0, wrapper);
    }
    if after_pending {
        let wrapper = taffy_tree
            .new_with_children(run_wrapper_style(style, true), &after_leaves)
            .ok()?;
        child_ids.push(wrapper);
    }
    if after_block {
        child_ids.extend(after_leaves.iter().copied());
    }
    for cid in out_of_flow {
        child_ids.extend(build_any(
            tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
        ));
    }

    let taffy_id = if child_ids.is_empty() {
        taffy_tree.new_leaf(taffy_style).ok()?
    } else {
        taffy_tree.new_with_children(taffy_style, &child_ids).ok()?
    };
    id_map.insert(taffy_id, id);
    Some(taffy_id)
}

fn needs_column_flex_text_fit_content_cap(
    tree: &DomTree,
    id: NodeId,
    style: &crate::LayoutStyle,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> bool {
    if style.display != crate::Display::Flex
        || style.internal_flex_container
        || style.is_inline_block
        || style.float.is_some()
        || matches!(style.position, Some(taffy::Position::Absolute))
        || style.aspect_ratio.is_some()
        || !matches!(
            (style.width, style.min_width, style.max_width),
            (
                crate::Dimension::Auto,
                crate::Dimension::Auto,
                crate::Dimension::Auto
            )
        )
        || style.size_expressions[0].is_some()
        || style.size_expressions[2].is_some()
        || style.size_expressions[4].is_some()
        || style.padding.left != 0.0
        || style.padding.right != 0.0
        || style.padding_percent[1].is_some()
        || style.padding_percent[3].is_some()
        || style.border.left != 0.0
        || style.border.right != 0.0
        || style.margin.left != 0.0
        || style.margin.right != 0.0
        || style.margin_auto[1]
        || style.margin_auto[3]
        || style.margin_percent[1].is_some()
        || style.margin_percent[3].is_some()
        || style.margin_relative[1].is_some()
        || style.margin_relative[3].is_some()
        || style.margin_expressions[1].is_some()
        || style.margin_expressions[3].is_some()
    {
        return false;
    }

    let has_direct_text = rendered_children(tree, id).into_iter().any(|child| {
        tree.get_node(child).map_or(false, |node| {
            matches!(
                &node.data,
                obscura_dom::tree::NodeData::Text { contents }
                    if !contents.trim().is_empty()
            )
        })
    });
    if !has_direct_text {
        return false;
    }

    let mut parent = rendered_parent(tree, id);
    let parent_style = loop {
        let Some(parent_id) = parent else {
            return false;
        };
        let Some(parent_style) = styles.get(&parent_id) else {
            return false;
        };
        if parent_style.display_contents {
            parent = rendered_parent(tree, parent_id);
            continue;
        }
        break parent_style;
    };
    if parent_style.display != crate::Display::Flex
        || parent_style.internal_flex_container
        || !matches!(
            parent_style.flex_direction,
            Some(taffy::FlexDirection::Column | taffy::FlexDirection::ColumnReverse)
        )
    {
        return false;
    }

    used_flex_alignment(style.align_self, parent_style.align_items) != taffy::AlignSelf::STRETCH
}

/// Expand `display: contents` wrappers so their children partition into the
/// caller's segment list as if they were direct children (CSS Display 3).
fn flatten_contents_children(
    tree: &DomTree,
    children: &[NodeId],
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    out: &mut Vec<NodeId>,
) {
    for &cid in children {
        let splices = styles
            .get(&cid)
            .map(|s| s.display_contents && s.display != crate::Display::None)
            .unwrap_or(false);
        if splices {
            let kids = rendered_children(tree, cid);
            flatten_contents_children(tree, &kids, styles, out);
        } else {
            out.push(cid);
        }
    }
}

/// Does an inline contribute only in-flow block children?
///
/// Such an inline has empty before/after inline fragments once CSS performs
/// the block-in-inline split, so its block children can participate directly
/// in the containing block's segment construction. This is deliberately
/// narrower than a complete ib-split: mixed inline/block children still need
/// fragment boxes to preserve decorations. A relative inline with no offsets
/// is safe here provided it does not establish the containing block for an
/// absolutely positioned descendant.
fn inline_wraps_only_in_flow_blocks(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> bool {
    let Some(style) = styles.get(&id) else {
        return false;
    };
    if style.display != crate::Display::Inline
        || style.is_inline_block
        || style.before_pseudo.is_some()
        || style.after_pseudo.is_some()
        || style.float.is_some()
        || matches!(style.position, Some(taffy::Position::Absolute))
        || (style.position.is_some()
            && (style.inset.iter().any(Option::is_some)
                || style.inset_expressions.iter().any(Option::is_some)))
    {
        return false;
    }

    let mut saw_block = false;
    for cid in rendered_children(tree, id) {
        let Some(node) = tree.get_node(cid) else {
            continue;
        };
        if let obscura_dom::tree::NodeData::Text { contents } = &node.data {
            if !contents.trim().is_empty() {
                return false;
            }
            continue;
        }
        if !node.is_element() {
            continue;
        }
        let Some(child) = styles.get(&cid) else {
            return false;
        };
        if child.display == crate::Display::None {
            continue;
        }
        if matches!(child.position, Some(taffy::Position::Absolute)) || child.float.is_some() {
            return false;
        }
        if child.display_contents {
            if !inline_wraps_only_in_flow_blocks(tree, cid, styles) {
                return false;
            }
            saw_block = true;
        } else if is_in_flow_block_level(child) {
            saw_block = true;
        } else {
            return false;
        }
    }
    saw_block
}

/// Recursively splice `display:contents`, decoration-free inline wrappers, and
/// inline wrappers whose only generated flow content is block-level into a
/// block container's effective child list. Descendants already carry inherited
/// computed text styles, so removing these wrappers preserves shaping while
/// exposing block-in-inline descendants early enough for anonymous block
/// construction.
fn flatten_boxless_inline_children(
    tree: &DomTree,
    children: &[NodeId],
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    out: &mut Vec<NodeId>,
) {
    for &cid in children {
        let display_contents = styles
            .get(&cid)
            .map(|style| style.display_contents && style.display != crate::Display::None)
            .unwrap_or(false);
        if display_contents
            || is_flattenable_inline(tree, cid, styles)
            || inline_wraps_only_in_flow_blocks(tree, cid, styles)
        {
            let kids = rendered_children(tree, cid);
            flatten_boxless_inline_children(tree, &kids, styles, out);
        } else {
            out.push(cid);
        }
    }
}

/// Style for a single-leaf inline run: a block-level box that fills the
/// containing block's width, with height from the shaped text (measure fn).
fn run_leaf_style() -> taffy::Style {
    taffy::Style {
        size: taffy::Size {
            width: taffy::style::Dimension::percent(1.0),
            height: taffy::style::Dimension::auto(),
        },
        ..Default::default()
    }
}

/// Style for an anonymous inline-run wrapper: a full-width block-level box
/// whose interior is the flex-wrap approximation of an inline formatting
/// context. The parent's `text-align` moves the run's line content via
/// justify-content, exactly as the old whole-container
/// promotion did, but scoped to the run so sibling blocks stay full width.
fn run_wrapper_style(parent: &crate::LayoutStyle, has_text_strut: bool) -> taffy::Style {
    let justify = match parent.text_align {
        Some(taffy::AlignItems::FLEX_END) => Some(taffy::JustifyContent::FLEX_END),
        Some(taffy::AlignItems::CENTER) => Some(taffy::JustifyContent::CENTER),
        _ => None,
    };
    let line_height = if has_text_strut {
        crate::inline::used_line_height(parent).max(0.0)
    } else {
        0.0
    };
    taffy::Style {
        direction: parent.direction.unwrap_or(taffy::Direction::Ltr),
        display: taffy::style::Display::Flex,
        flex_direction: taffy::FlexDirection::Row,
        flex_wrap: taffy::FlexWrap::Wrap,
        align_items: Some(taffy::AlignItems::FLEX_START),
        justify_content: justify,
        size: taffy::Size {
            width: taffy::style::Dimension::percent(1.0),
            height: taffy::style::Dimension::auto(),
        },
        // Every CSS line box starts with the parent's font/line-height strut,
        // even when its only atomic inline is shorter (or zero-sized).
        min_size: taffy::Size {
            width: taffy::style::Dimension::auto(),
            height: taffy::style::Dimension::length(line_height),
        },
        ..Default::default()
    }
}

/// Approximate `float: left|right` without real per-line reflow (which
/// taffy's block/flex/grid modes do not provide): place the float alongside
/// the flow siblings that follow it until their estimated height reaches the
/// float's estimated bottom, a matching `clear` is encountered, or another
/// float begins, then let everything from there on revert to normal full-width
/// flow.
///
/// This is not a general CSS float implementation (a float taller than its
/// estimated flow zone won't reflow correctly), but it directly targets the overwhelmingly
/// common real-world shape: a floated image or infobox near the top of an
/// article, sitting beside the intro text, with the rest of the content
/// running full width once normal flow passes the float.
/// Rough height budget for a float with no explicit size and no images
/// (an icon-only or empty float, rare in practice): enough for a couple of
/// lines of caption-sized text without being so generous it drags in a
/// whole section the way an unbounded zone did.
const DEFAULT_FLOAT_HEIGHT_ESTIMATE: f32 = 200.0;

/// Estimate a float's rendered height in CSS px, for bounding how many flow
/// siblings should wrap alongside it (see `build_children_with_float_zone`).
/// Real layout hasn't run yet at this point (we're still building the taffy
/// tree), so this can only approximate: prefer an explicit height on the
/// float itself, else sum the explicit heights of descendant `<img>`s (the
/// common `<figure><img height=".."><figcaption>` thumbnail shape) plus a
/// text-based estimate of the float's own content (the common tall-infobox
/// shape, where the height comes from many rows of text rather than a
/// single image).
fn estimate_float_height(
    tree: &DomTree,
    float_id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> f32 {
    if let Some(crate::Dimension::Px(h)) = styles.get(&float_id).map(|s| s.height) {
        return h;
    }
    let image_height: f32 = tree
        .descendants(float_id)
        .into_iter()
        .filter(|&id| {
            tree.get_node(id)
                .and_then(|n| n.as_element().map(|e| e.local.to_string()))
                .as_deref()
                == Some("img")
        })
        .filter_map(|id| match styles.get(&id).map(|s| s.height) {
            Some(crate::Dimension::Px(h)) => Some(h),
            _ => None,
        })
        .sum();
    const ASSUMED_FLOAT_WIDTH: f32 = 280.0;
    let text_height = estimate_text_height(tree, float_id, styles, ASSUMED_FLOAT_WIDTH);
    // Flattened character count misses forced rows. A sidebar list with twenty
    // short `<li>`s or an infobox table with many `<tr>`s is much taller than
    // the same text treated as one wrapping paragraph. Add one line for each
    // structural row; the continuous-text estimate still accounts for extra
    // wrapping within those rows.
    let structural_height: f32 = tree
        .descendants(float_id)
        .into_iter()
        .filter(|&id| {
            tree.get_node(id)
                .and_then(|node| node.as_element().map(|element| element.local.to_string()))
                .map(|local| {
                    matches!(
                        local.as_str(),
                        "li" | "tr"
                            | "dt"
                            | "dd"
                            | "p"
                            | "figcaption"
                            | "h1"
                            | "h2"
                            | "h3"
                            | "h4"
                            | "h5"
                            | "h6"
                    )
                })
                .unwrap_or(false)
        })
        .map(|id| {
            styles
                .get(&id)
                .and_then(|style| style.font_size)
                .unwrap_or(16.0)
                * 1.2
        })
        .sum();
    (image_height + text_height + structural_height).max(DEFAULT_FLOAT_HEIGHT_ESTIMATE)
}

/// Estimate how tall `id`'s text content would render at `assumed_width`,
/// using the same average-character-width heuristic as the layout-only
/// (non-`paint`) text sizing fallback. Used only to bound the float-wrapping
/// zone (see `build_children_with_float_zone`), where the real available
/// width is not yet known, so this is deliberately approximate.
fn estimate_text_height(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    assumed_width: f32,
) -> f32 {
    let char_count = tree
        .text_content(id)
        .chars()
        .filter(|c| !c.is_whitespace())
        .count() as f32;
    if char_count == 0.0 {
        return 0.0;
    }
    let fsize = styles.get(&id).and_then(|s| s.font_size).unwrap_or(16.0);
    const AVG_CHAR_WIDTH_EM: f32 = 0.55;
    let chars_per_line = (assumed_width / (fsize * AVG_CHAR_WIDTH_EM)).max(1.0);
    let lines = (char_count / chars_per_line).ceil().max(1.0);
    lines * fsize * 1.2 + 16.0
}

/// Estimate the normal-flow height consumed by one sibling alongside a float.
/// Text alone is insufficient for image grids and fixed-height boxes, which
/// otherwise cost zero budget and remain squeezed beside a float long after
/// they should have passed its bottom.
fn estimate_flow_sibling_height(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
    assumed_width: f32,
) -> f32 {
    let style = styles.get(&id);
    if style
        .map(|style| style.display == crate::Display::None)
        .unwrap_or(false)
        || style
            .and_then(|style| style.position)
            .map(|position| position == taffy::Position::Absolute)
            .unwrap_or(false)
    {
        return 0.0;
    }
    let explicit_height = match style.map(|style| style.height) {
        Some(crate::Dimension::Px(height)) => height.max(0.0),
        _ => 0.0,
    };
    let descendant_image_height: f32 = tree
        .descendants(id)
        .into_iter()
        .filter(|&descendant| {
            tree.get_node(descendant)
                .and_then(|node| {
                    node.as_element()
                        .map(|element| element.local.as_ref() == "img")
                })
                .unwrap_or(false)
        })
        .filter_map(
            |descendant| match styles.get(&descendant).map(|style| style.height) {
                Some(crate::Dimension::Px(height)) => Some(height.max(0.0)),
                _ => None,
            },
        )
        .sum();
    let own_image_height = if tree
        .get_node(id)
        .and_then(|node| {
            node.as_element()
                .map(|element| element.local.as_ref() == "img")
        })
        .unwrap_or(false)
    {
        explicit_height
    } else {
        0.0
    };
    let content_height = estimate_text_height(tree, id, styles, assumed_width)
        .max(explicit_height)
        .max(descendant_image_height + own_image_height);
    let margins = style
        .map(|style| (style.margin.top + style.margin.bottom).max(0.0))
        .unwrap_or(0.0);
    content_height + margins
}

/// Largest definite (px) width among `id` and its descendants. Used to cap an
/// auto-width floated figure: a Wikipedia thumbnail is `<figure ...><img
/// width=250><figcaption>long text</figcaption></figure>`, and without a cap
/// the caption's unwrapped one-line max-content width (~700px) sizes the float
/// and starves the adjacent flow column to nothing. Real browsers size the
/// figure to the image (display:table) and wrap the caption; the image's
/// definite width is the bound that reproduces that.
fn max_definite_descendant_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Option<f32> {
    let mut best: Option<f32> = None;
    for d in tree.descendants(id) {
        if let Some(crate::Dimension::Px(w)) = styles.get(&d).map(|s| s.width.clone()) {
            if w > 0.0 {
                best = Some(best.map_or(w, |b: f32| b.max(w)));
            }
        }
    }
    best
}

/// Fixed content descendants can floor a table's intrinsic minimum when the
/// generic grid measurement fails to surface them through an intervening
/// formatting context. Width hints on cells/rows/columns are different: the
/// CSS table algorithm treats those as preferred column constraints, so they
/// must remain shrinkable down to the content minimum.
fn max_definite_table_content_width(
    tree: &DomTree,
    id: NodeId,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Option<f32> {
    let mut best: Option<f32> = None;
    for descendant in tree.descendants(id) {
        let structural = styles
            .get(&descendant)
            .is_some_and(|style| style.is_table_cell_box)
            || tree.get_node(descendant).is_some_and(|node| {
                node.as_element().is_some_and(|element| {
                    matches!(
                        element.local.as_ref(),
                        "caption"
                            | "col"
                            | "colgroup"
                            | "thead"
                            | "tbody"
                            | "tfoot"
                            | "tr"
                            | "td"
                            | "th"
                    )
                })
            });
        if structural {
            continue;
        }
        if let Some(crate::Dimension::Px(width)) = styles.get(&descendant).map(|style| style.width)
        {
            if width > 0.0 {
                best = Some(best.map_or(width, |current| current.max(width)));
            }
        }
    }
    best
}

fn establishes_block_formatting_context(style: &crate::LayoutStyle) -> bool {
    matches!(style.display, crate::Display::Flex | crate::Display::Grid)
        || effective_container_type(style) != crate::ContainerType::Normal
        || style.flow_root
        || (style.overflow_scroll_container && !style.overflow_propagated_to_viewport)
        || style.is_inline_block
        || style.float.is_some()
        || matches!(style.position, Some(taffy::Position::Absolute))
}

fn clear_matches_float_sides(clear: crate::Clear, has_left: bool, has_right: bool) -> bool {
    (!has_left || matches!(clear, crate::Clear::Left | crate::Clear::Both))
        && (!has_right || matches!(clear, crate::Clear::Right | crate::Clear::Both))
}

fn has_deferred_or_auto_margin(style: &crate::LayoutStyle) -> bool {
    style.margin_auto.iter().any(|value| *value)
        || style.margin_percent.iter().any(Option::is_some)
        || style.margin_relative.iter().any(Option::is_some)
        || style.margin_expressions.iter().any(Option::is_some)
}

fn is_structural_native_clear_box(tree: &DomTree, id: NodeId, style: &crate::LayoutStyle) -> bool {
    style.display == crate::Display::Block
        && !style.display_contents
        && !style.is_table_box
        && !style.is_inline_block
        && !style.flow_root
        && !style.overflow_hidden
        && effective_container_type(style) == crate::ContainerType::Normal
        && style.float.is_none()
        && style.clear.is_some()
        && !matches!(style.position, Some(taffy::Position::Absolute))
        && tree.text_content(id).trim().is_empty()
        && style.before_pseudo.is_none()
        && style.after_pseudo.is_none()
}

fn is_structural_native_float_pseudo(style: &crate::LayoutStyle) -> bool {
    style.display != crate::Display::None
        && style.display != crate::Display::Inline
        && !style.display_contents
        && !style.is_inline_block
        && (!style.flow_root || style.is_table_box)
        && !style.overflow_hidden
        && effective_container_type(style) == crate::ContainerType::Normal
        && style.float.is_none()
        && !matches!(style.position, Some(taffy::Position::Absolute))
        && style
            .before_content
            .as_deref()
            .map_or(true, |content| content.trim().is_empty())
}

/// Native taffy floats are currently sound for a deliberately small structural
/// subset: a flat block band made from boxed direct floats, boxed block flow,
/// and empty generated/direct clearance boxes. Text nodes directly in the
/// parent, transparent wrappers, independent formatting contexts, and tables
/// retain the synthetic float-zone path.
fn can_use_native_float_band(
    tree: &DomTree,
    parent_style: &crate::LayoutStyle,
    dom_children: &[NodeId],
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> bool {
    if parent_style.display != crate::Display::Block
        || parent_style.internal_flex_container
        || parent_style.is_inline_block
        || parent_style.is_table_box
        || matches!(parent_style.position, Some(taffy::Position::Absolute))
        || !matches!(
            parent_style.width,
            crate::Dimension::Auto | crate::Dimension::Px(_) | crate::Dimension::Percent(_)
        )
        || parent_style.size_expressions[0].is_some()
    {
        return false;
    }

    let mut has_left = false;
    let mut has_right = false;
    let mut saw_float = false;
    let mut saw_clear_after_floats = false;
    let mut saw_flow_after_floats = false;

    if let Some(before) = parent_style.before_pseudo.as_deref() {
        if !is_structural_native_float_pseudo(before) {
            return false;
        }
    }

    for &id in dom_children {
        let Some(node) = tree.get_node(id) else {
            continue;
        };
        if !node.is_element() {
            // Comments, doctypes, and processing instructions generate no
            // formatting box and cannot split a float band. In particular,
            // Bootstrap labels closing navbar wrappers with non-empty HTML
            // comments between the floated header and ordinary collapse
            // block. Only actual non-whitespace text requires the legacy
            // inline float path.
            if matches!(
                &node.data,
                obscura_dom::tree::NodeData::Text { contents }
                    if !contents.trim().is_empty()
            ) {
                return false;
            }
            continue;
        }
        let Some(style) = styles.get(&id) else {
            return false;
        };
        if style.display == crate::Display::None {
            continue;
        }

        if let Some(side) = style.float {
            if saw_clear_after_floats
                || saw_flow_after_floats
                || style.display_contents
                || style.is_table_box
                || matches!(style.position, Some(taffy::Position::Absolute))
                || !matches!(
                    style.width,
                    crate::Dimension::Auto | crate::Dimension::Px(_) | crate::Dimension::Percent(_)
                )
                || style.size_expressions[0].is_some()
                || has_deferred_or_auto_margin(style)
            {
                return false;
            }
            saw_float = true;
            has_left |= side == crate::Float::Left;
            has_right |= side == crate::Float::Right;
        } else if is_structural_native_clear_box(tree, id, style) {
            if !saw_float {
                return false;
            }
            saw_clear_after_floats |= clear_matches_float_sides(
                style.clear.expect("structural clear has a side"),
                has_left,
                has_right,
            );
        } else if saw_float
            && style.display == crate::Display::Block
            && !style.display_contents
            && !style.is_inline_block
            && !style.is_table_box
            && !style.internal_flex_container
            && !matches!(style.position, Some(taffy::Position::Absolute))
        {
            // Gecko keeps an ordinary block's border box at the BFC's full
            // inline size and narrows only its descendant line boxes. A block
            // that establishes its own BFC (overflow/flow-root) instead moves
            // as one float-avoiding box. Taffy's native block float context
            // implements both branches; the old synthetic flex row could
            // represent neither distinction because it shrank every sibling's
            // outer box beside the float.
            saw_flow_after_floats = true;
        } else {
            return false;
        }
    }

    if let Some(after) = parent_style.after_pseudo.as_deref() {
        if !is_structural_native_float_pseudo(after) {
            return false;
        }
        if let Some(clear) = after.clear {
            saw_clear_after_floats |= clear_matches_float_sides(clear, has_left, has_right);
        }
    }

    // Taffy represents scroll-container overflow as a real BFC root. Plain
    // `clip` does not establish a BFC, and viewport-propagated overflow leaves
    // its source box visible. Other Obscura BFC markers do not yet have a
    // distinct taffy-side representation, so they are not an escape signal.
    let parent_is_native_bfc =
        parent_style.overflow_scroll_container && !parent_style.overflow_propagated_to_viewport;
    saw_float && (saw_flow_after_floats || saw_clear_after_floats || parent_is_native_bfc)
}

fn set_native_float_clear(
    taffy_tree: &mut TaffyTree<usize>,
    node: taffy::NodeId,
    style: &crate::LayoutStyle,
    generated_pseudo: bool,
) {
    let Ok(current) = taffy_tree.style(node) else {
        return;
    };
    let mut native = current.clone();
    native.float = match style.float {
        Some(crate::Float::Left) => taffy::style::Float::Left,
        Some(crate::Float::Right) => taffy::style::Float::Right,
        None => taffy::style::Float::None,
    };
    native.clear = match style.clear {
        Some(crate::Clear::Left) => taffy::style::Clear::Left,
        Some(crate::Clear::Right) => taffy::style::Clear::Right,
        Some(crate::Clear::Both) => taffy::style::Clear::Both,
        None => taffy::style::Clear::None,
    };
    if generated_pseudo && style.is_table_box {
        // Bootstrap-style clearfix pseudos use display:table only to generate
        // an empty block formatting box. Taffy's independent table-item clear
        // path is incomplete, while its same-BFC block clear path is correct.
        native.display = taffy::style::Display::Block;
        native.item_is_table = false;
    }
    let _ = taffy_tree.set_style(node, native);
}

#[allow(clippy::too_many_arguments)]
fn build_children_with_native_float_band(
    tree: &DomTree,
    parent_id: NodeId,
    parent_style: &crate::LayoutStyle,
    dom_children: &[NodeId],
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &mut HashMap<taffy::NodeId, NodeId>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Vec<taffy::NodeId> {
    let mut result = Vec::new();
    for (kind, pseudo) in [
        (
            GeneratedBoxKind::Before,
            parent_style.before_pseudo.as_deref(),
        ),
        (
            GeneratedBoxKind::After,
            parent_style.after_pseudo.as_deref(),
        ),
    ] {
        if kind == GeneratedBoxKind::After {
            for &id in dom_children {
                let Some(style) = styles.get(&id) else {
                    continue;
                };
                if style.float.is_none()
                    && style.display == crate::Display::Block
                    && !establishes_block_formatting_context(style)
                {
                    ifc_items.float_aware_blocks.insert(id);
                }
                for node in build_any(
                    tree, id, taffy_tree, id_map, words, engine, ifc_items, styles,
                ) {
                    set_native_float_clear(taffy_tree, node, style, false);
                    result.push(node);
                }
            }
        }
        if let Some((nodes, _)) = build_in_flow_pseudo(
            parent_id, kind, pseudo, taffy_tree, words, engine, ifc_items,
        ) {
            if let Some(style) = pseudo {
                for node in nodes {
                    set_native_float_clear(taffy_tree, node, style, true);
                    result.push(node);
                }
            }
        }
    }
    result
}

fn build_children_with_float_zone(
    tree: &DomTree,
    parent_id: NodeId,
    dom_children: &[NodeId],
    taffy_tree: &mut TaffyTree<usize>,
    id_map: &mut HashMap<taffy::NodeId, NodeId>,
    words: &mut HashMap<taffy::NodeId, (NodeId, String)>,
    engine: &mut crate::inline::TextEngine,
    ifc_items: &mut IfcRegistry,
    styles: &HashMap<NodeId, crate::LayoutStyle>,
) -> Vec<taffy::NodeId> {
    let is_float = |cid: NodeId| {
        styles
            .get(&cid)
            .map(|style| style.display != crate::Display::None && style.float.is_some())
            .unwrap_or(false)
    };

    // A definite-height block whose only substantive contents are either one
    // full-width float or one percentage float followed by one percentage
    // inline atom has a bounded, single float band.  The general legacy path
    // below cannot represent that shape: it puts the float in an auto-width,
    // zero-height escape wrapper, so both percentage axes resolve against an
    // indefinite synthetic box.  A Bootstrap `width:100%;height:100%` column
    // then collapses or moves away from block-start, and the equally common
    // `[float:left;width:50%][inline-block;width:50%]` control row gives the
    // float zero size.
    //
    // Keep this deliberately narrower than a float manager.  In this exact
    // one-band case a full-size flex row is only a placement representation:
    // its percentage basis is the real containing block's content box, its
    // block-start is the normal flow position, and it cannot incorrectly
    // contain an escaping float because the parent already owns a definite
    // block size.  Text, multiple flow siblings, auto sizes, and floats that
    // can extend beyond an auto-height parent retain the general path.
    let parent_has_definite_height = styles.get(&parent_id).is_some_and(|parent| {
        matches!(
            parent.height,
            crate::Dimension::Px(_) | crate::Dimension::Percent(_)
        ) && parent.size_expressions[1].is_none()
    });
    let parent_has_clearfix = styles.get(&parent_id).is_some_and(|parent| {
        parent.after_pseudo.as_deref().is_some_and(|after| {
            matches!(after.clear, Some(crate::Clear::Left | crate::Clear::Both))
                && has_in_flow_generated_pseudo(Some(after))
        })
    });
    let parent_min_height = styles
        .get(&parent_id)
        .and_then(|parent| match parent.min_height {
            crate::Dimension::Px(value) => Some(value),
            _ => None,
        });
    if parent_has_definite_height || parent_has_clearfix || parent_min_height.is_some() {
        let fills_axis = |value: f32| (value - 1.0).abs() < 0.001;
        let substantive: Vec<NodeId> = dom_children
            .iter()
            .copied()
            .filter(|cid| {
                tree.get_node(*cid).is_some_and(|node| {
                    if node.is_element() {
                        styles
                            .get(cid)
                            .is_some_and(|style| style.display != crate::Display::None)
                    } else {
                        !tree.text_content(*cid).trim().is_empty()
                    }
                })
            })
            .collect();
        let floated: Vec<NodeId> = substantive
            .iter()
            .copied()
            .filter(|cid| is_float(*cid))
            .collect();
        if floated.len() == 1 {
            let float_dom = floated[0];
            let float_style = styles.get(&float_dom);
            let float_percent_width = float_style.and_then(|style| match style.width {
                crate::Dimension::Percent(value) => Some(value),
                _ => None,
            });
            let float_fills_height = float_style.is_some_and(|style| {
                matches!(style.height, crate::Dimension::Percent(value) if fills_axis(value))
                    && style.size_expressions[1].is_none()
            });
            let float_pixel_height = float_style.and_then(|style| match style.height {
                crate::Dimension::Px(value) if style.size_expressions[1].is_none() => Some(value),
                _ => None,
            });
            let float_has_definite_height = float_pixel_height.is_some() || float_fills_height;
            let height_is_already_contained = parent_has_definite_height
                || parent_has_clearfix
                || parent_min_height
                    .zip(float_pixel_height)
                    .is_some_and(|(minimum, height)| minimum + 0.001 >= height);
            let sole_full_width_float = substantive.len() == 1
                && float_percent_width.is_some_and(fills_axis)
                && float_has_definite_height
                && height_is_already_contained;
            let split_band_flow = if parent_has_definite_height
                && substantive.len() == 2
                && substantive[0] == float_dom
                && float_style.and_then(|style| style.float) == Some(crate::Float::Left)
                && float_fills_height
            {
                let flow_dom = substantive[1];
                styles.get(&flow_dom).and_then(|flow_style| {
                    let flow_percent_width = match flow_style.width {
                        crate::Dimension::Percent(value) => value,
                        _ => return None,
                    };
                    let flow_fills_height = matches!(
                        flow_style.height,
                        crate::Dimension::Percent(value) if fills_axis(value)
                    ) && flow_style.size_expressions[1].is_none();
                    let widths_fill_one_band = float_percent_width.is_some_and(|float_width| {
                        float_width > 0.0
                            && flow_percent_width > 0.0
                            && (float_width + flow_percent_width - 1.0).abs() < 0.001
                    });
                    (flow_style.display != crate::Display::None
                        && !flow_style.display_contents
                        && flow_style.is_inline_block
                        && flow_fills_height
                        && widths_fill_one_band)
                        .then_some(flow_dom)
                })
            } else {
                None
            };

            if sole_full_width_float || split_band_flow.is_some() {
                let float_node = build(
                    tree, float_dom, taffy_tree, id_map, words, engine, ifc_items, styles,
                );
                // The split-band guard admits only a boxed inline-block
                // element, so it must be built as one atomic box. Calling
                // `build_any` here would be needlessly fragile: its text and
                // transparent-inline branches may fan out into several nodes,
                // and discovering that only after construction would leave
                // detached nodes behind when falling back to the general path.
                let flow_node = split_band_flow.and_then(|flow_dom| {
                    build(
                        tree, flow_dom, taffy_tree, id_map, words, engine, ifc_items, styles,
                    )
                });
                if let Some(float_node) = float_node {
                    if sole_full_width_float || flow_node.is_some() {
                        let children: Vec<taffy::NodeId> = [Some(float_node), flow_node]
                            .into_iter()
                            .flatten()
                            .collect();
                        let band_style = taffy::Style {
                            display: taffy::style::Display::Flex,
                            flex_direction: taffy::FlexDirection::Row,
                            flex_wrap: taffy::FlexWrap::NoWrap,
                            align_items: Some(taffy::AlignItems::FLEX_START),
                            size: taffy::Size {
                                width: taffy::Dimension::percent(1.0),
                                height: if parent_has_definite_height {
                                    taffy::Dimension::percent(1.0)
                                } else {
                                    taffy::Dimension::auto()
                                },
                            },
                            ..Default::default()
                        };
                        if let Ok(band) = taffy_tree.new_with_children(band_style, &children) {
                            return vec![band];
                        }
                    }
                }
            }
        }
    }

    // A block made entirely from inline-ish flow content and two or more
    // right floats is the classic utility/navigation bar. Right floats are
    // placed from the inline end inward, so their visual order is the reverse
    // of source order, while ordinary inline content keeps filling from the
    // start of the same band. Serializing each encountered float into its own
    // synthetic row reverses those two groups and can leave the entire bar
    // shrink-wrapped at the start.
    //
    // Model this bounded one-band case as [flow | reversed right-float group].
    // A nested group preserves every float's authored margins; only the
    // anonymous group receives the auto margin used to represent the free
    // space between the two sides.
    let right_floats: Vec<NodeId> = dom_children
        .iter()
        .copied()
        .filter(|cid| {
            styles.get(cid).map_or(false, |style| {
                style.float == Some(crate::Float::Right) && style.display != crate::Display::None
            })
        })
        .collect();
    let has_left_float = dom_children.iter().any(|cid| {
        styles.get(cid).map_or(false, |style| {
            style.float == Some(crate::Float::Left) && style.display != crate::Display::None
        })
    });
    let flow_is_inline = dom_children.iter().all(|cid| {
        let Some(node) = tree.get_node(*cid) else {
            return true;
        };
        if !node.is_element() || is_float(*cid) {
            return true;
        }
        styles
            .get(cid)
            .map_or(true, |style| style.display != crate::Display::Block)
    });
    if right_floats.len() >= 2 && !has_left_float && flow_is_inline {
        // Removing out-of-flow items must not remove the one collapsible
        // space between the inline items on either side of them. Collapse
        // any run of formatting whitespace to one representative node, while
        // dropping leading/trailing whitespace at the band edges.
        let mut flow_dom = Vec::new();
        let mut pending_whitespace = None;
        let mut has_flow_content = false;
        for &cid in dom_children {
            if is_float(cid) {
                continue;
            }
            let is_whitespace = tree.get_node(cid).map_or(false, |node| {
                !node.is_element() && tree.text_content(cid).trim().is_empty()
            });
            if is_whitespace {
                if has_flow_content && pending_whitespace.is_none() {
                    pending_whitespace = Some(cid);
                }
                continue;
            }
            if has_flow_content {
                if let Some(whitespace) = pending_whitespace.take() {
                    flow_dom.push(whitespace);
                }
            }
            flow_dom.push(cid);
            has_flow_content = true;
        }
        let mut row_children: Vec<taffy::NodeId> = flow_dom
            .into_iter()
            .flat_map(|cid| {
                build_any(
                    tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                )
            })
            .collect();
        let right_children: Vec<taffy::NodeId> = right_floats
            .iter()
            .rev()
            .filter_map(|cid| {
                build(
                    tree, *cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                )
            })
            .collect();
        if !row_children.is_empty() && !right_children.is_empty() {
            let right_group_style = taffy::Style {
                display: taffy::style::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                flex_wrap: taffy::FlexWrap::Wrap,
                margin: taffy::Rect {
                    top: taffy::style::LengthPercentageAuto::length(0.0),
                    right: taffy::style::LengthPercentageAuto::length(0.0),
                    bottom: taffy::style::LengthPercentageAuto::length(0.0),
                    left: taffy::style::LengthPercentageAuto::auto(),
                },
                ..Default::default()
            };
            if let Ok(right_group) =
                taffy_tree.new_with_children(right_group_style, &right_children)
            {
                row_children.push(right_group);
                let row_style = taffy::Style {
                    display: taffy::style::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    flex_wrap: taffy::FlexWrap::Wrap,
                    align_items: Some(taffy::AlignItems::FLEX_START),
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                };
                if let Ok(row) = taffy_tree.new_with_children(row_style, &row_children) {
                    return vec![row];
                }
            }
        }
    }

    let Some(float_idx) = dom_children.iter().position(|&cid| is_float(cid)) else {
        return dom_children
            .iter()
            .flat_map(|&cid| {
                build_any(
                    tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                )
            })
            .collect();
    };

    let mut result: Vec<taffy::NodeId> = dom_children[..float_idx]
        .iter()
        .flat_map(|&cid| {
            build_any(
                tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
            )
        })
        .collect();

    let float_side = styles.get(&dom_children[float_idx]).and_then(|s| s.float);

    // Opposing header floats share one float band even when an empty legacy
    // compatibility box sits between them. This is the classic left-logo /
    // right-tagline header: serializing the two synthetic rows doubles the
    // header height and pushes every later box down. Real float placement
    // scans the same BFC band and puts the second float against the opposite
    // edge when both margin boxes fit.
    let is_empty_bridge = |cid: NodeId| {
        let Some(node) = tree.get_node(cid) else {
            return true;
        };
        if !node.is_element() {
            return tree.text_content(cid).trim().is_empty();
        }
        let style = styles.get(&cid);
        if style.is_some_and(|style| style.display == crate::Display::None) {
            return true;
        }
        let no_size = style
            .map(|style| {
                matches!(style.width, crate::Dimension::Auto)
                    && matches!(style.height, crate::Dimension::Auto)
                    && matches!(style.min_width, crate::Dimension::Auto)
                    && matches!(style.min_height, crate::Dimension::Auto)
                    && matches!(style.max_width, crate::Dimension::Auto)
                    && matches!(style.max_height, crate::Dimension::Auto)
                    && style.margin == crate::Edges::default()
                    && style.padding == crate::Edges::default()
                    && style.padding_percent.iter().all(|value| value.is_none())
                    && style.border == crate::Edges::default()
                    && style.before_pseudo.is_none()
                    && style.after_pseudo.is_none()
            })
            .unwrap_or(true);
        no_size && tree.text_content(cid).trim().is_empty()
    };
    let mut opposite_idx = float_idx + 1;
    while opposite_idx < dom_children.len() && is_empty_bridge(dom_children[opposite_idx]) {
        opposite_idx += 1;
    }
    let opposite_side = dom_children
        .get(opposite_idx)
        .and_then(|cid| styles.get(cid))
        .and_then(|style| (style.display != crate::Display::None).then_some(style.float))
        .flatten();
    if opposite_side.is_some() && opposite_side != float_side {
        let first = build(
            tree,
            dom_children[float_idx],
            taffy_tree,
            id_map,
            words,
            engine,
            ifc_items,
            styles,
        );
        let second = build(
            tree,
            dom_children[opposite_idx],
            taffy_tree,
            id_map,
            words,
            engine,
            ifc_items,
            styles,
        );
        let row_children: Vec<taffy::NodeId> = match float_side {
            Some(crate::Float::Left) => [first, second].into_iter().flatten().collect(),
            _ => [second, first].into_iter().flatten().collect(),
        };
        let row_style = taffy::Style {
            display: taffy::style::Display::Flex,
            flex_direction: taffy::FlexDirection::Row,
            justify_content: Some(taffy::JustifyContent::SPACE_BETWEEN),
            align_items: Some(taffy::AlignItems::FLEX_START),
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        if let Ok(row) = taffy_tree.new_with_children(row_style, &row_children) {
            result.push(row);
        }
        result.extend(dom_children[opposite_idx + 1..].iter().flat_map(|&cid| {
            build_any(
                tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
            )
        }));
        return result;
    }

    // A run of two or more consecutively floated siblings (the classic
    // float-grid idiom: several `float:left; width:N%` boxes forming columns,
    // e.g. craigslist's `.sites .box{float:left;width:23%}` site directory) is
    // not the single-float-beside-flow shape handled below: real float layout
    // places the run side by side, wrapping to a new line when the row fills.
    // Model the run as a wrapping flex row. Whitespace-only text between the
    // floats does not break the run.
    let mut run_end = float_idx + 1;
    let mut float_count = 1usize;
    while run_end < dom_children.len() {
        let cid = dom_children[run_end];
        if is_float(cid) && styles.get(&cid).and_then(|style| style.float) == float_side {
            float_count += 1;
            run_end += 1;
        } else if tree.get_node(cid).map_or(false, |n| !n.is_element())
            && tree.text_content(cid).trim().is_empty()
        {
            run_end += 1;
        } else {
            break;
        }
    }
    if float_count >= 2 {
        let mut run_children: Vec<taffy::NodeId> = dom_children[float_idx..run_end]
            .iter()
            // Formatting whitespace between floats does not generate an
            // in-flow flex item or consume horizontal space.
            .filter(|&&cid| styles.get(&cid).and_then(|s| s.float) == float_side)
            .flat_map(|&cid| {
                build_any(
                    tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                )
            })
            .collect();
        // A common navigation-bar shape is a run of left floats followed by
        // one right float. The right float still scans the current float band:
        // it does not start a new row merely because multiple left floats
        // precede it. Keep the run in one wrapping row and use an auto inline
        // margin to reserve all remaining space before the opposing float.
        //
        // This stays deliberately narrower than a general float manager. In
        // particular, multiple right floats have reverse source-order
        // placement semantics and need their own representation.
        let trailing_right = (float_side == Some(crate::Float::Left)
            && dom_children
                .get(run_end)
                .and_then(|cid| styles.get(cid))
                .and_then(|style| {
                    (style.display != crate::Display::None).then_some(style.float)
                })
                .flatten()
                == Some(crate::Float::Right))
        .then(|| dom_children[run_end]);
        if let Some(right_dom) = trailing_right {
            let right = build(
                tree, right_dom, taffy_tree, id_map, words, engine, ifc_items, styles,
            );
            if let Some(right) = right {
                if let Ok(current) = taffy_tree.style(right) {
                    let mut pushed_right = current.clone();
                    pushed_right.margin.left = taffy::style::LengthPercentageAuto::auto();
                    let _ = taffy_tree.set_style(right, pushed_right);
                }
                run_children.push(right);
                let row_style = taffy::Style {
                    display: taffy::style::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    flex_wrap: taffy::FlexWrap::Wrap,
                    align_items: Some(taffy::AlignItems::FLEX_START),
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                };
                if let Ok(row) = taffy_tree.new_with_children(row_style, &run_children) {
                    result.push(row);
                }
                result.extend(build_children_with_float_zone(
                    tree,
                    parent_id,
                    &dom_children[run_end + 1..],
                    taffy_tree,
                    id_map,
                    words,
                    engine,
                    ifc_items,
                    styles,
                ));
                return result;
            }
        }
        let row_style = taffy::Style {
            display: taffy::style::Display::Flex,
            flex_direction: taffy::FlexDirection::Row,
            flex_wrap: taffy::FlexWrap::Wrap,
            align_items: Some(taffy::AlignItems::FLEX_START),
            // This anonymous row represents the float band's available
            // inline size. Make that size definite before flex line
            // collection: leaving it auto lets intrinsic sizing wrap a set
            // of percentage floats before the parent later stretches the row.
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        if let Ok(row) = taffy_tree.new_with_children(row_style, &run_children) {
            result.push(row);
        }
        result.extend(build_children_with_float_zone(
            tree,
            parent_id,
            &dom_children[run_end..],
            taffy_tree,
            id_map,
            words,
            engine,
            ifc_items,
            styles,
        ));
        return result;
    }

    // Stop growing the zone once the flow siblings collected so far would
    // already fill (an estimate of) the float's own height: real float
    // reflow ends when normal-flow content passes the float's bottom edge,
    // Headings do not terminate a CSS float's influence by themselves.
    // Without this, a short floated thumbnail (a few hundred px) dragged an
    // entire multi-paragraph section into a narrow flow column alongside it
    // — visibly wrong wrapping plus a large empty gap once the (much
    // shorter) float ran out, both from treating "next heading" as the only
    // bound. The estimate is necessarily rough (actual available width is a
    // taffy layout result we don't have yet at tree-build time), but even an
    // approximate bound beats an unbounded one.
    let float_height_budget = estimate_float_height(tree, dom_children[float_idx], styles);
    const ASSUMED_FLOW_WIDTH: f32 = 500.0;
    // `clear` on a sibling ends the zone: the cleared element moves below the
    // float (the clearfix idiom), so it must not join the flow column beside it.
    let clears_this_float = |cid: NodeId| {
        let Some(c) = styles.get(&cid).and_then(|style| {
            (style.display != crate::Display::None)
                .then_some(style.clear)
                .flatten()
        }) else {
            return false;
        };
        match (float_side, c) {
            (_, crate::Clear::Both) => true,
            (Some(crate::Float::Left), crate::Clear::Left) => true,
            (Some(crate::Float::Right), crate::Clear::Right) => true,
            _ => false,
        }
    };
    let mut zone_end = float_idx + 1;
    let mut flow_height_estimate = 0.0f32;
    while zone_end < dom_children.len()
        && !is_float(dom_children[zone_end])
        && !clears_this_float(dom_children[zone_end])
    {
        flow_height_estimate +=
            estimate_flow_sibling_height(tree, dom_children[zone_end], styles, ASSUMED_FLOW_WIDTH);
        zone_end += 1;
        if flow_height_estimate >= float_height_budget {
            break;
        }
    }
    // The float itself is always an element (only elements get style
    // entries, and `is_float` above required one), so a direct `build` call
    // is correct here; only its flow siblings need the word-splitting `build_any`.
    let float_taffy = build(
        tree,
        dom_children[float_idx],
        taffy_tree,
        id_map,
        words,
        engine,
        ifc_items,
        styles,
    );
    // Cap an auto-width float at its widest definite-width descendant so a long
    // wrappable caption cannot inflate it to the caption's one-line max-content
    // width and starve the flow column beside it (the Wikipedia-thumbnail /
    // article-body-collapses-to-one-word-per-line bug). Only when the float is
    // itself auto-width and actually contains such a box; text-only floats
    // (pull quotes, sized infoboxes) are left to normal shrink-to-fit.
    if let Some(float_id) = float_taffy {
        let float_dom = dom_children[float_idx];
        let float_auto = styles
            .get(&float_dom)
            .map(|s| matches!(s.width, crate::Dimension::Auto))
            .unwrap_or(true);
        let float_is_table = styles
            .get(&float_dom)
            .is_some_and(|style| style.is_table_box);
        if float_auto && float_is_table {
            if let Some(w) = max_definite_descendant_width(tree, float_dom, styles) {
                if let Ok(cur) = taffy_tree.style(float_id) {
                    let mut st = cur.clone();
                    // A little slack for the figure's own border/padding so the
                    // image is not clipped; the caption still wraps to ~image width.
                    st.max_size.width = taffy::Dimension::length(w + 12.0);
                    let _ = taffy_tree.set_style(float_id, st);
                }
            }
        }
    }
    match float_taffy {
        Some(float_id) => {
            let flow_column_style = taffy::Style {
                display: taffy::style::Display::Block,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: taffy::Dimension::length(0.0),
                min_size: taffy::Size {
                    width: taffy::Dimension::length(0.0),
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            };
            let flow_dom = &dom_children[float_idx + 1..zone_end];
            let flow_column = if flow_dom.is_empty() {
                taffy_tree.new_leaf(flow_column_style).ok()
            } else {
                // The zone is still an ordinary block formatting context:
                // consecutive text/inline siblings must share inline runs,
                // while block siblings stack. Building each sibling directly
                // into a flex column makes every link a separate stretched
                // row. Reuse the mixed-block builder, but leave this anonymous
                // wrapper out of the DOM id map so it cannot overwrite the
                // real parent's rectangle.
                let mut flow_style = styles.get(&parent_id).cloned().unwrap_or_default();
                flow_style.before_content = None;
                flow_style.after_content = None;
                let column = build_mixed_block(
                    tree,
                    parent_id,
                    &flow_style,
                    flow_column_style,
                    flow_dom,
                    taffy_tree,
                    id_map,
                    words,
                    engine,
                    ifc_items,
                    styles,
                );
                if let Some(column_id) = column {
                    id_map.remove(&column_id);
                }
                column
            };

            let row_style = taffy::Style {
                display: taffy::style::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                align_items: Some(taffy::AlignItems::FLEX_START),
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            };
            // A float does not contribute to the height of a non-BFC block
            // that contains it. Put an escaping float inside a zero-height,
            // overflow-visible wrapper: its real width still reserves the
            // current band, but its height can protrude into later descendant
            // blocks of the ancestor BFC. A matching `clear` or any remaining
            // direct full-width sibling keeps the old containing row, since
            // that content explicitly terminates the local float zone.
            let can_escape = zone_end == dom_children.len()
                && styles
                    .get(&parent_id)
                    .map(|style| !establishes_block_formatting_context(style))
                    .unwrap_or(false);
            let row_float = if can_escape {
                let wrapper_style = taffy::Style {
                    display: taffy::style::Display::Block,
                    flex_grow: 0.0,
                    flex_shrink: 0.0,
                    size: taffy::Size {
                        width: taffy::Dimension::auto(),
                        height: taffy::Dimension::length(0.0),
                    },
                    ..Default::default()
                };
                taffy_tree
                    .new_with_children(wrapper_style, &[float_id])
                    .ok()
                    .unwrap_or(float_id)
            } else {
                float_id
            };
            let row_children: Vec<taffy::NodeId> = match float_side {
                Some(crate::Float::Left) => [Some(row_float), flow_column]
                    .into_iter()
                    .flatten()
                    .collect(),
                _ => [flow_column, Some(row_float)]
                    .into_iter()
                    .flatten()
                    .collect(),
            };
            if let Ok(row) = taffy_tree.new_with_children(row_style, &row_children) {
                result.push(row);
                if can_escape {
                    if let (Some(flow), Some(side)) = (flow_column, float_side) {
                        ifc_items.float_continuations.push(FloatContinuation {
                            owner: parent_id,
                            float: float_id,
                            flow,
                            side,
                        });
                    }
                }
            }
        }
        // The float itself failed to build (e.g. display:none resolved for
        // it specifically); still build its flow siblings so their content
        // is not silently lost.
        None => result.extend(
            dom_children[float_idx + 1..zone_end]
                .iter()
                .flat_map(|&cid| {
                    build_any(
                        tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
                    )
                }),
        ),
    }

    result.extend(dom_children[zone_end..].iter().flat_map(|&cid| {
        build_any(
            tree, cid, taffy_tree, id_map, words, engine, ifc_items, styles,
        )
    }));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use obscura_dom::tree::ShadowRootMode;
    use obscura_dom::tree_sink::parse_html;

    fn attach_programmatic_shadow(tree: &DomTree, host: NodeId, source: NodeId) -> NodeId {
        let root = tree
            .attach_shadow_root(host, ShadowRootMode::Open)
            .expect("attach native shadow root");
        for child in tree.children(source) {
            tree.append_child(root, child);
        }
        tree.remove(source);
        root
    }

    #[test]
    fn shadow_rendered_children_distribute_named_and_default_slots() {
        let tree = parse_html(
            r#"<x-card id="host"><span id="default"></span><span id="title" slot="title"></span><span id="unslotted" slot="missing"></span></x-card><div id="source"><div id="before"></div><slot id="title-slot" name="title"><b id="title-fallback"></b></slot><slot id="duplicate-title" name="title"><i id="duplicate-fallback"></i></slot><slot id="default-slot"><em id="default-fallback"></em></slot><div id="after"></div></div>"#,
        );
        let node = |id| {
            tree.get_element_by_id(id)
                .unwrap_or_else(|| panic!("fixture node {id}"))
        };
        let host = node("host");
        let source = node("source");
        let before = node("before");
        let title_slot = node("title-slot");
        let duplicate_title = node("duplicate-title");
        let duplicate_fallback = node("duplicate-fallback");
        let default_slot = node("default-slot");
        let after = node("after");
        let default_light = node("default");
        let title_light = node("title");
        let unslotted = node("unslotted");
        attach_programmatic_shadow(&tree, host, source);

        assert_eq!(
            rendered_children(&tree, host),
            vec![
                before,
                title_slot,
                duplicate_title,
                default_slot,
                after,
            ]
        );
        assert_eq!(rendered_children(&tree, title_slot), vec![title_light]);
        assert_eq!(
            rendered_children(&tree, duplicate_title),
            vec![duplicate_fallback],
            "only the first slot with a given name receives assignments"
        );
        assert_eq!(rendered_children(&tree, default_slot), vec![default_light]);
        assert_eq!(rendered_parent(&tree, before), Some(host));
        assert_eq!(rendered_parent(&tree, title_light), Some(title_slot));
        assert_eq!(rendered_parent(&tree, default_light), Some(default_slot));
        assert_eq!(rendered_parent(&tree, unslotted), None);
        assert!(
            !rendered_children(&tree, host).contains(&unslotted),
            "an unmatched light child must not enter the host's box tree"
        );
    }

    #[test]
    fn native_shadow_tree_and_slotted_light_child_generate_layout_boxes() {
        let tree = parse_html(
            r#"<html style="width:100px"><body style="margin:0"><x-card id="host" style="display:block;width:30px"><span id="light" style="display:block;height:10px"></span><span id="unslotted" slot="missing" style="display:block;height:80px"></span></x-card><div id="source"><div id="before" style="height:10px"></div><slot id="slot"></slot><div id="after" style="height:10px"></div></div></body></html>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let source = tree.get_element_by_id("source").unwrap();
        let light = tree.get_element_by_id("light").unwrap();
        let unslotted = tree.get_element_by_id("unslotted").unwrap();
        let before = tree.get_element_by_id("before").unwrap();
        let after = tree.get_element_by_id("after").unwrap();
        attach_programmatic_shadow(&tree, host, source);

        let laid = layout_dom(&tree, (100.0, 100.0));
        let host_rect = laid.rects[&host];
        let before_rect = laid.rects[&before];
        let light_rect = laid.rects[&light];
        let after_rect = laid.rects[&after];
        assert_eq!((host_rect.width, host_rect.height), (30.0, 30.0));
        assert_eq!(before_rect.y, host_rect.y);
        assert_eq!(light_rect.y, host_rect.y + 10.0);
        assert_eq!(after_rect.y, host_rect.y + 20.0);
        assert!(
            !laid.rects.contains_key(&unslotted),
            "unmatched light DOM must not generate a layout box"
        );
    }

    #[test]
    fn root_scrolling_overflow_uses_composed_shadow_tree() {
        let tree = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <x-card id="host" style="display:block;position:relative;width:100px;height:20px">
                    <div id="unslotted" slot="missing"
                         style="position:absolute;top:600px;width:10px;height:30px"></div>
                </x-card>
                <div id="source">
                    <div id="shadow-overflow"
                         style="position:absolute;top:250px;width:10px;height:30px"></div>
                </div>
            </body></html>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let source = tree.get_element_by_id("source").unwrap();
        let unslotted = tree.get_element_by_id("unslotted").unwrap();
        let shadow_overflow = tree.get_element_by_id("shadow-overflow").unwrap();
        attach_programmatic_shadow(&tree, host, source);

        let laid = layout_dom(&tree, (100.0, 100.0));
        assert!(!laid.rects.contains_key(&unslotted));
        assert_eq!(laid.rects[&shadow_overflow].y, 250.0);
        assert_eq!(
            laid.scrolling_content_size(&tree, (100.0, 100.0)),
            (100.0, 280.0),
            "shadow overflow must contribute while unslotted light DOM must not",
        );
    }

    #[test]
    fn native_shadow_author_styles_are_ordered_isolated_and_inherit_from_host() {
        let tree = parse_html(
            r#"<style>
                 .target { width:123px; height:9px; padding-left:13px; color:#ff0000 }
                 .only-first-root { margin-left:99px }
               </style>
               <x-one id="host-one" style="display:block;--host-width:41px;color:#123456">
                 <div id="light" class="target only-first-root"></div>
               </x-one>
               <x-two id="host-two" style="display:block"></x-two>
               <div id="source-one">
                 <style>.target { width:20px; height:var(--local-height) }</style>
                 <style>.target { width:var(--host-width); color:inherit }
                        .holder { --local-height:23px }
                        .only-first-root { margin-left:7px }
                 </style>
                 <section class="holder"><div id="shadow-one" class="target only-first-root"></div></section>
               </div>
               <div id="source-two">
                 <style>.target { width:67px; height:31px }</style>
                 <div id="shadow-two" class="target only-first-root"></div>
               </div>"#,
        );
        let host_one = tree.get_element_by_id("host-one").unwrap();
        let host_two = tree.get_element_by_id("host-two").unwrap();
        let light = tree.get_element_by_id("light").unwrap();
        let source_one = tree.get_element_by_id("source-one").unwrap();
        let source_two = tree.get_element_by_id("source-two").unwrap();
        let shadow_one = tree.get_element_by_id("shadow-one").unwrap();
        let shadow_two = tree.get_element_by_id("shadow-two").unwrap();
        attach_programmatic_shadow(&tree, host_one, source_one);
        attach_programmatic_shadow(&tree, host_two, source_two);

        let laid = layout_dom(&tree, (400.0, 300.0));
        let light_style = &laid.styles[&light];
        let first_style = &laid.styles[&shadow_one];
        let second_style = &laid.styles[&shadow_two];

        assert_eq!(light_style.width, crate::Dimension::Px(123.0));
        assert_eq!(light_style.height, crate::Dimension::Px(9.0));
        assert_eq!(light_style.padding.left, 13.0);
        assert_eq!(light_style.margin.left, 99.0);
        assert_eq!(first_style.width, crate::Dimension::Px(41.0));
        assert_eq!(first_style.height, crate::Dimension::Px(23.0));
        assert_eq!(first_style.padding.left, 0.0);
        assert_eq!(first_style.margin.left, 7.0);
        assert_eq!(first_style.color, Some([0x12, 0x34, 0x56, 0xff]));
        assert_eq!(second_style.width, crate::Dimension::Px(67.0));
        assert_eq!(second_style.height, crate::Dimension::Px(31.0));
        assert_eq!(second_style.margin.left, 0.0);
    }

    #[test]
    fn shadow_host_rules_match_in_scope_and_follow_encapsulation_cascade_order() {
        let tree = parse_html(
            r#"<style>
                 x-one { width:110px; --normal-size:66px; height:var(--normal-size) }
                 x-one { margin-left:8px !important; padding-left:9px !important; --critical:7px !important }
               </style>
               <x-one id="host-one" class="active" style="margin-left:11px !important"></x-one>
               <x-two id="host-two"></x-two>
               <div id="source-one">
                 <style>
                   :host { display:block; width:40px; --normal-size:22px }
                   :host(.active) { margin-left:31px !important; padding-left:var(--critical) !important; --critical:17px !important }
                   :host([hidden]) { border-left-width:19px }
                   .active { border-right-width:23px }
                 </style>
                 <div style="height:5px"></div>
               </div>
               <div id="source-two">
                 <style>:host { display:block; width:55px }</style>
                 <div style="height:7px"></div>
               </div>"#,
        );
        let host_one = tree.get_element_by_id("host-one").unwrap();
        let host_two = tree.get_element_by_id("host-two").unwrap();
        let source_one = tree.get_element_by_id("source-one").unwrap();
        let source_two = tree.get_element_by_id("source-two").unwrap();
        attach_programmatic_shadow(&tree, host_one, source_one);
        attach_programmatic_shadow(&tree, host_two, source_two);

        let laid = layout_dom(&tree, (400.0, 300.0));
        let first = &laid.styles[&host_one];
        let second = &laid.styles[&host_two];

        assert_eq!(
            first.display,
            crate::Display::Block,
            ":host supplies geometry"
        );
        assert_eq!(
            first.width,
            crate::Dimension::Px(110.0),
            "outer normal wins"
        );
        assert_eq!(
            first.height,
            crate::Dimension::Px(66.0),
            "outer custom property wins"
        );
        assert_eq!(
            first.margin.left, 31.0,
            "inner important beats inline important"
        );
        assert_eq!(
            first.padding.left, 17.0,
            "inner important custom property wins"
        );
        assert_eq!(
            first.border.left, 0.0,
            "non-matching :host argument is isolated"
        );
        assert_eq!(
            first.border.right, 0.0,
            "ordinary shadow selector cannot style host"
        );
        assert_eq!(
            laid.rects[&host_one].width, 127.0,
            "content width plus host padding"
        );

        assert_eq!(second.width, crate::Dimension::Px(55.0));
        assert_eq!(second.height, crate::Dimension::Auto);
        assert_eq!(second.margin.left, 0.0, "first shadow root cannot leak");
    }

    #[test]
    fn slotted_rules_style_only_assigned_light_children_with_shadow_cascade_order() {
        let tree = parse_html(
            r#"<style>
                 .item { width:120px; height:var(--normal-size); --normal-size:33px }
                 .item { margin-left:5px !important; padding-left:var(--critical) !important; --critical:7px !important }
               </style>
               <x-one id="host-one">
                 <span id="assigned-one" class="item" slot="title" style="margin-left:9px !important"></span>
                 <span id="unslotted" class="item" slot="missing"></span>
               </x-one>
               <x-two id="host-two"><span id="assigned-two" class="item"></span></x-two>
               <div id="source-one">
                 <style>
                   ::slotted(.item) { display:block; width:40px; --normal-size:11px; border-left:3px solid }
                   .wrapper slot.named::slotted(.item) { margin-left:21px !important; padding-left:var(--critical) !important; --critical:17px !important }
                   ::slotted([hidden]) { border-top:19px solid }
                   .item { border-right:23px solid }
                 </style>
                 <div class="wrapper"><slot class="named" name="title"></slot></div>
               </div>
               <div id="source-two">
                 <style>::slotted(.item) { display:block; border-bottom:4px solid }</style>
                 <slot></slot>
               </div>"#,
        );
        let host_one = tree.get_element_by_id("host-one").unwrap();
        let host_two = tree.get_element_by_id("host-two").unwrap();
        let source_one = tree.get_element_by_id("source-one").unwrap();
        let source_two = tree.get_element_by_id("source-two").unwrap();
        let assigned_one = tree.get_element_by_id("assigned-one").unwrap();
        let assigned_two = tree.get_element_by_id("assigned-two").unwrap();
        let unslotted = tree.get_element_by_id("unslotted").unwrap();
        attach_programmatic_shadow(&tree, host_one, source_one);
        attach_programmatic_shadow(&tree, host_two, source_two);

        let laid = layout_dom(&tree, (400.0, 300.0));
        let first = &laid.styles[&assigned_one];
        let second = &laid.styles[&assigned_two];
        let hidden = &laid.styles[&unslotted];

        assert_eq!(first.display, crate::Display::Block);
        assert_eq!(first.width, crate::Dimension::Px(120.0), "document normal wins");
        assert_eq!(first.height, crate::Dimension::Px(33.0), "document custom property wins");
        assert_eq!(first.margin.left, 21.0, "shadow important beats inline important");
        assert_eq!(first.padding.left, 17.0, "shadow important custom property wins");
        assert_eq!(first.border.left, 3.0, "slot-qualified selector matches");
        assert_eq!(first.border.right, 0.0, "ordinary shadow selector stays isolated");
        assert_eq!(first.border.top, 0.0, "slotted argument must match");
        assert_eq!(laid.rects[&assigned_one].width, 140.0);

        assert_eq!(second.border.bottom, 4.0);
        assert_eq!(second.border.left, 0.0, "first shadow root cannot leak");
        assert_eq!(hidden.border.left, 0.0, "unassigned light child is not slotted");
        assert!(!laid.rects.contains_key(&unslotted));
    }

    #[test]
    fn assigned_nodes_inherit_slot_styles_and_custom_properties() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
               <x-card id="host" style="display:block;color:#010203;font-size:13px;--host-width:61px">
                 <span id="assigned" slot="title" style="display:block;width:var(--slot-width);height:5px"></span>
                 <span id="unslotted" slot="missing" style="display:block;width:var(--host-width);height:80px"></span>
               </x-card>
               <div id="source">
                 <style>
                   slot { color:#123456; font-size:21px; font-weight:700; --slot-width:47px }
                   #empty { color:#654321; font-size:17px; --fallback-width:31px }
                 </style>
                 <slot id="title-slot" name="title">
                   <span id="suppressed-fallback" style="display:block;width:var(--slot-width);height:70px"></span>
                 </slot>
                 <slot id="empty" name="empty">
                   <span id="visible-fallback" style="display:block;width:var(--fallback-width);height:6px"></span>
                 </slot>
               </div>
               </body></html>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let source = tree.get_element_by_id("source").unwrap();
        let assigned = tree.get_element_by_id("assigned").unwrap();
        let unslotted = tree.get_element_by_id("unslotted").unwrap();
        let suppressed_fallback = tree.get_element_by_id("suppressed-fallback").unwrap();
        let visible_fallback = tree.get_element_by_id("visible-fallback").unwrap();
        attach_programmatic_shadow(&tree, host, source);

        let laid = layout_dom(&tree, (200.0, 200.0));
        let assigned_style = &laid.styles[&assigned];
        assert_eq!(assigned_style.color, Some([0x12, 0x34, 0x56, 0xff]));
        assert_eq!(assigned_style.font_size, Some(21.0));
        assert_eq!(crate::style::used_font_weight(assigned_style), 700);
        assert_eq!(
            assigned_style.width,
            crate::Dimension::Px(47.0),
            "the assigned element's own declaration must resolve a custom property inherited from the slot"
        );
        assert_eq!(
            laid.custom_properties[&assigned]
                .get("--slot-width")
                .map(String::as_str),
            Some("47px")
        );

        let fallback_style = &laid.styles[&visible_fallback];
        assert_eq!(fallback_style.color, Some([0x65, 0x43, 0x21, 0xff]));
        assert_eq!(fallback_style.font_size, Some(17.0));
        assert_eq!(fallback_style.width, crate::Dimension::Px(31.0));
        assert!(laid.rects.contains_key(&visible_fallback));

        let suppressed_style = &laid.styles[&suppressed_fallback];
        assert_eq!(suppressed_style.color, Some([0x12, 0x34, 0x56, 0xff]));
        assert_eq!(suppressed_style.width, crate::Dimension::Px(47.0));
        assert!(
            !laid.rects.contains_key(&suppressed_fallback),
            "assigned content suppresses fallback boxes without suppressing fallback computed style"
        );

        let unslotted_style = &laid.styles[&unslotted];
        assert_eq!(unslotted_style.color, Some([0x01, 0x02, 0x03, 0xff]));
        assert_eq!(unslotted_style.font_size, Some(13.0));
        assert_eq!(unslotted_style.width, crate::Dimension::Px(61.0));
        assert!(
            !laid.rects.contains_key(&unslotted),
            "unmatched light DOM keeps CSSOM style but generates no box"
        );
    }

    #[test]
    fn nested_slot_reassignment_uses_each_flattened_parent_in_order() {
        let tree = parse_html(
            r#"<html><body style="margin:0">
               <x-outer id="outer" style="display:block">
                 <span id="target" slot="forward" style="display:block;width:var(--inner-width);height:var(--forward-height)"></span>
               </x-outer>
               <div id="outer-source">
                 <style>
                   slot[name=forward] { color:#445566; --forward-height:23px }
                 </style>
                 <x-inner id="inner">
                   <slot id="forward-slot" name="forward" slot="bridge"></slot>
                 </x-inner>
               </div>
               <div id="inner-source">
                 <style>
                   slot[name=bridge] { color:#112233; font-size:19px; font-weight:700; --inner-width:47px }
                 </style>
                 <slot id="bridge-slot" name="bridge"></slot>
               </div>
               </body></html>"#,
        );
        let outer = tree.get_element_by_id("outer").unwrap();
        let inner = tree.get_element_by_id("inner").unwrap();
        let outer_source = tree.get_element_by_id("outer-source").unwrap();
        let inner_source = tree.get_element_by_id("inner-source").unwrap();
        let forward_slot = tree.get_element_by_id("forward-slot").unwrap();
        let bridge_slot = tree.get_element_by_id("bridge-slot").unwrap();
        let target = tree.get_element_by_id("target").unwrap();
        attach_programmatic_shadow(&tree, outer, outer_source);
        attach_programmatic_shadow(&tree, inner, inner_source);

        assert_eq!(tree.assigned_slot(target), Some(forward_slot));
        assert_eq!(tree.assigned_slot(forward_slot), Some(bridge_slot));

        let laid = layout_dom(&tree, (200.0, 200.0));
        let bridge_style = &laid.styles[&bridge_slot];
        let forward_style = &laid.styles[&forward_slot];
        let target_style = &laid.styles[&target];

        assert_eq!(bridge_style.color, Some([0x11, 0x22, 0x33, 0xff]));
        assert_eq!(forward_style.color, Some([0x44, 0x55, 0x66, 0xff]));
        assert_eq!(
            target_style.color,
            Some([0x44, 0x55, 0x66, 0xff]),
            "the closest flattened parent wins for inherited properties"
        );
        assert_eq!(forward_style.font_size, Some(19.0));
        assert_eq!(target_style.font_size, Some(19.0));
        assert_eq!(crate::style::used_font_weight(target_style), 700);
        assert_eq!(target_style.width, crate::Dimension::Px(47.0));
        assert_eq!(target_style.height, crate::Dimension::Px(23.0));
        assert_eq!(
            laid.custom_properties[&target]
                .get("--inner-width")
                .map(String::as_str),
            Some("47px"),
            "the inner slot's custom property must cross both flattened edges"
        );
        assert!(
            !laid.rects.contains_key(&forward_slot),
            "the reassigned slot remains box-transparent"
        );
        assert!(laid.rects.contains_key(&target));
    }

    #[test]
    fn direct_shadow_inline_block_is_blockified_as_a_flex_item() {
        let tree = parse_html(
            r#"<html><body style="margin:0"><x-row id="host" style="display:flex;width:100px"><div id="source"><span id="item" style="display:inline-block;width:20px"><span style="display:block;height:10px"></span><span style="display:block;height:10px"></span></span></div></x-row></body></html>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let source = tree.get_element_by_id("source").unwrap();
        let item = tree.get_element_by_id("item").unwrap();
        attach_programmatic_shadow(&tree, host, source);

        let laid = layout_dom(&tree, (120.0, 80.0));
        assert_eq!(
            laid.rects[&item].height, 20.0,
            "the flex formatting-context parent must be found across ShadowRoot→host"
        );
    }

    #[test]
    fn lays_out_real_dom() {
        let tree = parse_html(
            "<html><body><div style=\"width: 300px\"><div style=\"width: 100px; height: 40px\"></div><div style=\"width: 100px; height: 60px\"></div></div></body></html>",
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        // Every element gets a rect.
        assert!(
            laid.rects.len() >= 4,
            "expected >=4 element rects, got {}",
            laid.rects.len()
        );

        // The two inner divs stack vertically inside the 300px container.
        let stacks = laid
            .rects
            .values()
            .filter(|r| (r.width - 100.0).abs() < 0.1)
            .map(|r| (r.y, r.height))
            .collect::<Vec<_>>();
        assert_eq!(stacks.len(), 2, "expected two 100px-wide children");
        let mut sorted = stacks.clone();
        sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        assert!(
            (sorted[0].1 - 40.0).abs() < 0.1 && (sorted[1].1 - 60.0).abs() < 0.1,
            "children heights should be 40 and 60, got {:?}",
            sorted
        );
    }

    #[test]
    fn native_button_intrinsic_width_uses_only_rendered_in_flow_content() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                button{font-size:16px;padding:0 8px;border:0}
                .gone{display:none}
                .a11y{position:absolute;width:1px;height:1px}
              </style>
              <button id="plain">v5.3</button>
              <button id="labels"><span class="gone">Bootstrap</span><span class="a11y">Bootstrap </span>v5.3<span class="a11y">(switch to other versions)</span></button>
              <button id="icon"><svg style="width:16px;height:16px"></svg></button>
              <button id="icon-label"><svg style="width:16px;height:16px"></svg><span class="gone">Toggle theme</span></button>"#,
        );
        let laid = layout_dom(&tree, (800.0, 600.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];

        assert!(
            (rect("plain").width - rect("labels").width).abs() < 0.1,
            "hidden/out-of-flow labels enlarged the button: plain={:?}, labels={:?}",
            rect("plain"),
            rect("labels")
        );
        assert!(
            (rect("icon").width - rect("icon-label").width).abs() < 0.1,
            "display:none icon label enlarged the button: icon={:?}, labelled={:?}",
            rect("icon"),
            rect("icon-label")
        );
        assert!(
            (rect("icon").width - 32.0).abs() < 0.1,
            "the in-flow 16px SVG and horizontal padding must contribute: {:?}",
            rect("icon")
        );
    }

    #[test]
    fn text_alignment_does_not_shrink_wrap_block_grid_and_flex_children() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #host{width:900px;text-align:center}
                #grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:20px}
                .card{overflow:hidden;height:30px}
                .wide{width:1200px;height:10px}
                #flex{display:flex}
                #flex > div{flex:1;height:20px}
            </style>
            <div id="host">
              <div id="grid">
                <div id="card-a" class="card"><div class="wide"></div></div>
                <div id="card-b" class="card"><div class="wide"></div></div>
              </div>
              <div id="flex"><div></div><div></div></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (1000.0, 300.0));
        let rect = |name| laid.rects[&tree.get_element_by_id(name).unwrap()];
        let host = rect("host");
        let grid = rect("grid");
        let card_a = rect("card-a");
        let card_b = rect("card-b");
        let flex = rect("flex");

        assert!((host.width - 900.0).abs() < 0.01, "{host:?}");
        assert!((grid.width - 900.0).abs() < 0.01, "{grid:?}");
        assert!(
            (card_a.width - 440.0).abs() < 0.01
                && (card_b.width - 440.0).abs() < 0.01
                && (card_b.x - card_a.x - 460.0).abs() < 0.01,
            "grid tracks escaped their containing block: {card_a:?} {card_b:?}"
        );
        assert!((flex.width - 900.0).abs() < 0.01, "{flex:?}");
    }

    #[test]
    fn sticky_normal_flow_excludes_own_pixel_and_percentage_translates() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #flow{width:300px;height:900px}
                .spacer{height:100px}
                .sticky{position:sticky;top:0}
                #pixel{width:120px;height:40px;transform:translate(7px,-15px)}
                #percent{width:120px;height:50px;transform:translateY(-110%)}
            </style>
            <div id="flow">
              <div class="spacer"></div>
              <div id="pixel" class="sticky"></div>
              <div class="spacer"></div>
              <div id="percent" class="sticky"></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (400.0, 240.0));
        let sticky = laid.root_sticky_layout(&tree, (400.0, 240.0));

        for name in ["pixel", "percent"] {
            let id = tree.get_element_by_id(name).unwrap();
            let rect = laid.rects[&id];
            let frame = sticky.frames.iter().find(|frame| frame.id == id).unwrap();
            assert!(
                (frame.normal.x - rect.x).abs() < 0.01 && (frame.normal.y - rect.y).abs() < 0.01,
                "{name} own transform must not alter its sticky normal position: \
                 rect={rect:?} frame={frame:?}"
            );
        }
    }

    #[test]
    fn individual_translate_percentage_uses_own_border_box() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #containing-block{position:relative;width:453px;height:412px}
                #card{
                    position:absolute;
                    top:50%;
                    right:64px;
                    width:310px;
                    height:280px;
                    --tw-translate-x:0;
                    --tw-translate-y:calc(calc(1 / 2 * 100%) * -1);
                    translate:var(--tw-translate-x) var(--tw-translate-y);
                }
            </style>
            <div id="containing-block"><div id="card"></div></div>"#,
        );
        let laid = layout_dom(&tree, (800.0, 600.0));
        let card = tree.get_element_by_id("card").unwrap();
        let rect = laid.rects[&card];
        let translated = laid.translates[&card];

        assert!(
            (rect.y - 206.0).abs() < 0.01,
            "absolute top:50% must use the containing block: {rect:?}"
        );
        assert!(
            translated.0.abs() < 0.01 && (translated.1 + 140.0).abs() < 0.01,
            "translate percentage must use the card's own 280px border box: {translated:?}"
        );
        assert!(
            laid.styles[&card].establishes_positioning_containing_block(),
            "an individual translate creates a containing block like transform"
        );
    }

    #[test]
    fn sticky_normal_flow_retains_transformed_ancestor_coordinates() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #ancestor{
                    width:300px;
                    height:700px;
                    transform:translate(25px,30px)
                }
                #spacer{height:100px}
                #sticky{
                    position:sticky;
                    top:0;
                    width:120px;
                    height:40px;
                    transform:translate(-5px,-20px)
                }
            </style>
            <div id="ancestor">
              <div id="spacer"></div>
              <div id="sticky"></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (400.0, 240.0));
        let id = tree.get_element_by_id("sticky").unwrap();
        let rect = laid.rects[&id];
        let sticky = laid.root_sticky_layout(&tree, (400.0, 240.0));
        let frame = sticky.frames.iter().find(|frame| frame.id == id).unwrap();

        assert!(
            (frame.normal.x - (rect.x + 25.0)).abs() < 0.01
                && (frame.normal.y - (rect.y + 30.0)).abs() < 0.01,
            "ancestor transform must remain in sticky constraint coordinates: \
             rect={rect:?} frame={frame:?}"
        );
    }

    #[test]
    fn sticky_horizontal_overconstraint_prioritizes_logical_inline_start() {
        let ltr = sticky_axis_position(0.0, 80.0, -100.0, 100.0, 0.0, 100.0, Some(30.0), Some(30.0), false);
        let rtl = sticky_axis_position(0.0, 80.0, -100.0, 100.0, 0.0, 100.0, Some(30.0), Some(30.0), true);
        assert_eq!(ltr, 30.0, "LTR physical left is inline-start");
        assert_eq!(rtl, -10.0, "RTL physical right is inline-start");
    }

    #[test]
    fn sticky_owner_validation_skips_boxless_scrollers_and_declines_affine_spaces() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #boxless{display:contents;overflow:auto}
                #root-sticky{position:sticky;top:0;width:20px;height:20px}
                #affine{transform:rotate(2deg)}
                #affine-sticky{position:sticky;top:0;width:20px;height:20px}
            </style>
            <div style="height:50px"></div>
            <div id="boxless"><div id="root-sticky"></div></div>
            <div id="affine"><div id="affine-sticky"></div></div>
            <div style="height:400px"></div>"#,
        );
        let laid = layout_dom(&tree, (200.0, 100.0));
        let root_sticky = tree.get_element_by_id("root-sticky").unwrap();
        let affine_sticky = tree.get_element_by_id("affine-sticky").unwrap();
        let sticky = laid.root_sticky_layout(&tree, (200.0, 100.0));

        assert!(
            sticky.frames.iter().any(|frame| {
                frame.id == root_sticky && frame.scroll_owner == ScrollId::ROOT
            }),
            "display:contents cannot capture a sticky descendant",
        );
        assert!(
            sticky.frames.iter().all(|frame| frame.id != affine_sticky),
            "affine ancestor sticky must use the explicit non-sticky fallback",
        );
    }

    #[test]
    fn public_sticky_layout_resolves_nested_zero_offset_constraints() {
        let tree = parse_html(
            r#"<style>html,body{margin:0}</style>
            <div style="width:100px;height:80px;overflow:hidden">
                <div style="height:100px"></div>
                <div id="sticky" style="position:sticky;bottom:0;width:20px;height:20px"></div>
                <div style="height:20px"></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (120.0, 100.0));
        let id = tree.get_element_by_id("sticky").unwrap();
        let sticky = laid.root_sticky_layout(&tree, (120.0, 100.0));
        assert_eq!(sticky.translation_for(id, (120.0, 100.0), (0.0, 0.0)).1, -40.0);
        assert_eq!(sticky.translations((120.0, 100.0), (0.0, 0.0))[&id].1, -40.0);
    }

    #[test]
    fn container_query_uses_final_content_box_geometry() {
        let tree = parse_html(
            r#"<html><head><style>
                #container {
                    box-sizing:border-box;
                    container-type:inline-size;
                    width:500px;
                    padding:20px;
                    border:10px solid;
                }
                #target { width:1px; height:1px }
                @container (width >= 440px) { #target { width:44px } }
                @container (width > 499px) { #target { width:50px } }
            </style></head><body>
                <div id="container"><div id="target"></div></div>
            </body></html>"#,
        );
        let (laid, telemetry) =
            layout_dom_with_web_fonts_measured(&tree, (1280.0, 720.0), &HashMap::new(), &[]);
        let container = tree.get_element_by_id("container").unwrap();
        let target = tree.get_element_by_id("target").unwrap();
        assert!((laid.rects[&container].width - 500.0).abs() < 0.1);
        assert_eq!(laid.styles[&target].width, crate::Dimension::Px(44.0));
        assert_eq!(telemetry.passes, 2);
        assert_eq!(
            telemetry.termination,
            ContainerLayoutTermination::GeometryStable
        );
    }

    #[test]
    fn container_query_named_and_unnamed_lookup_choose_different_ancestors() {
        let tree = parse_html(
            r#"<html><head><style>
                #outer { container: shell / inline-size; width:600px }
                #inner { container: other / inline-size; width:300px }
                #target { width:1px; height:1px }
                @container shell (min-width:500px) { #target { width:11px } }
                @container (min-width:500px) { #target { height:22px } }
            </style></head><body>
                <div id="outer"><div id="inner"><div id="target"></div></div></div>
            </body></html>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let target = tree.get_element_by_id("target").unwrap();
        assert_eq!(laid.styles[&target].width, crate::Dimension::Px(11.0));
        assert_eq!(laid.styles[&target].height, crate::Dimension::Px(1.0));
    }

    #[test]
    fn nested_container_queries_select_independent_ancestors() {
        let tree = parse_html(
            r#"<html><head><style>
                #outer { container: outer / inline-size; width:600px }
                #inner { container: inner / inline-size; width:200px }
                #target { width:1px }
                @container outer (min-width:500px) {
                    @container inner (min-width:200px) {
                        #target { width:19px }
                    }
                }
            </style></head><body>
                <div id="outer"><div id="inner"><div id="target"></div></div></div>
            </body></html>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let target = tree.get_element_by_id("target").unwrap();
        assert_eq!(laid.styles[&target].width, crate::Dimension::Px(19.0));
    }

    #[test]
    fn container_query_pseudo_can_select_its_originating_element() {
        let tree = parse_html(
            r#"<html><head><style>
                #container { container-type:inline-size; width:300px }
                @container (min-width:300px) {
                    #container::before { content:"active"; display:block }
                }
            </style></head><body><div id="container"></div></body></html>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let container = tree.get_element_by_id("container").unwrap();
        assert_eq!(
            laid.styles[&container]
                .before_pseudo
                .as_ref()
                .and_then(|pseudo| pseudo.before_content.as_deref()),
            Some("active")
        );
    }

    #[test]
    fn container_type_zeroes_shrink_to_fit_inline_intrinsic_widths() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #host{width:600px}
                .container{container-type:inline-size}
                .wide{width:240px;height:30px}
                #inline-block{display:inline-block}
                #inline-flex{display:inline-flex}
                #inline-grid{display:inline-grid}
            </style>
            <div id="host">
              <div id="inline-block" class="container"><div class="wide"></div></div>
              <div id="inline-flex" class="container"><div class="wide"></div></div>
              <div id="inline-grid" class="container"><div class="wide"></div></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (600.0, 300.0));
        for name in ["inline-block", "inline-flex", "inline-grid"] {
            let id = tree.get_element_by_id(name).unwrap();
            let rect = laid.rects[&id];
            assert!(
                rect.width.abs() < 0.01,
                "{name} descendants leaked into contained intrinsic width: {rect:?}"
            );
            assert!(
                rect.height >= 29.9,
                "inline-size containment must retain content-driven block size: {rect:?}"
            );
        }
    }

    #[test]
    fn inline_outer_boxes_shrink_wrap_in_only_child_and_text_flows() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                .host{width:700px}
                .wide{width:400px;height:20px}
                #ib,#ibt{display:inline-block}
                #if,#ift{display:inline-flex}
                #ig,#igt{display:inline-grid}
            </style>
            <div class="host"><div id="ib"><div class="wide"></div></div></div>
            <div class="host"><div id="if"><div class="wide"></div></div></div>
            <div class="host"><div id="ig"><div class="wide"></div></div></div>
            <div class="host">A<div id="ibt"><div class="wide"></div></div>B</div>
            <div class="host">A<div id="ift"><div class="wide"></div></div>B</div>
            <div class="host">A<div id="igt"><div class="wide"></div></div>B</div>"#,
        );
        let laid = layout_dom(&tree, (700.0, 300.0));
        for name in ["ib", "if", "ig", "ibt", "ift", "igt"] {
            let rect = laid.rects[&tree.get_element_by_id(name).unwrap()];
            assert!(
                (rect.width - 400.0).abs() < 0.01,
                "{name} did not shrink-wrap its 400px child: {rect:?}"
            );
        }
    }

    #[test]
    fn inline_flex_and_grid_preserve_percentage_and_box_edge_geometry() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                .host{width:700px}
                .percent{width:50%}
                .flex{display:inline-flex}
                .grid{display:inline-grid}
                .edges{padding:10px 20px;border:5px solid;margin:7px}
                .child{width:100px;height:20px}
            </style>
            <div class="host"><div id="flex-percent" class="flex percent"><div class="child"></div></div></div>
            <div class="host"><div id="grid-percent" class="grid percent"><div class="child"></div></div></div>
            <div class="host"><div id="flex-edges" class="flex edges"><div class="child"></div></div></div>
            <div class="host"><div id="grid-edges" class="grid edges"><div class="child"></div></div></div>"#,
        );
        let laid = layout_dom(&tree, (800.0, 300.0));
        let rect = |name| laid.rects[&tree.get_element_by_id(name).unwrap()];
        for name in ["flex-percent", "grid-percent"] {
            let item = rect(name);
            assert!(
                (item.width - 350.0).abs() < 0.01 && (item.height - 20.0).abs() < 0.01,
                "{name}: {item:?}"
            );
        }
        for name in ["flex-edges", "grid-edges"] {
            let item = rect(name);
            assert!(
                (item.x - 7.0).abs() < 0.01
                    && (item.width - 150.0).abs() < 0.01
                    && (item.height - 50.0).abs() < 0.01,
                "{name}: {item:?}"
            );
        }
    }

    #[test]
    fn inline_flex_and_grid_resolve_percentage_min_max_against_containing_block() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                .host{width:700px}
                .flex{display:inline-flex}
                .grid{display:inline-grid}
                .max{max-width:50%}
                .min{min-width:50%}
                .wide{width:500px;height:20px}
                .narrow{width:100px;height:20px}
            </style>
            <div class="host"><div id="flex-max" class="flex max"><div class="wide"></div></div></div>
            <div class="host"><div id="grid-max" class="grid max"><div class="wide"></div></div></div>
            <div class="host"><div id="flex-min" class="flex min"><div class="narrow"></div></div></div>
            <div class="host"><div id="grid-min" class="grid min"><div class="narrow"></div></div></div>"#,
        );
        let laid = layout_dom(&tree, (800.0, 300.0));
        for name in ["flex-max", "grid-max", "flex-min", "grid-min"] {
            let item = laid.rects[&tree.get_element_by_id(name).unwrap()];
            assert!(
                (item.width - 350.0).abs() < 0.01,
                "{name} resolved its percentage constraint against the wrong box: {item:?}"
            );
        }
    }

    #[test]
    fn inline_atomic_vertical_align_controls_cross_axis_placement() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                .line{font:20px/40px sans-serif}
                .atom{display:inline-flex;width:30px}
                .bottom{vertical-align:bottom}
                .middle{vertical-align:middle}
                #bottom-short,#middle-short{height:10px}
                #bottom-tall,#middle-tall{height:30px}
            </style>
            <div class="line">x<span id="bottom-short" class="atom bottom"></span><span id="bottom-tall" class="atom bottom"></span></div>
            <div class="line">x<span id="middle-short" class="atom middle"></span><span id="middle-tall" class="atom middle"></span></div>"#,
        );
        let laid = layout_dom(&tree, (800.0, 200.0));
        let rect = |name| laid.rects[&tree.get_element_by_id(name).unwrap()];
        let bottom_short = rect("bottom-short");
        let bottom_tall = rect("bottom-tall");
        assert!(
            (bottom_short.y + bottom_short.height - bottom_tall.y - bottom_tall.height).abs()
                < 0.01,
            "bottom-aligned atoms did not share a line bottom: {bottom_short:?} {bottom_tall:?}"
        );
        let middle_short = rect("middle-short");
        let middle_tall = rect("middle-tall");
        assert!(
            (middle_short.y + middle_short.height / 2.0 - middle_tall.y - middle_tall.height / 2.0)
                .abs()
                < 0.01,
            "middle-aligned atoms did not share a line center: {middle_short:?} {middle_tall:?}"
        );
    }

    #[test]
    fn blockified_inline_flex_and_grid_items_retain_inner_layout() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                .host{display:flex;width:700px}
                #if{display:inline-flex}
                #ig{display:inline-grid;grid-template-columns:100px 100px}
                .item{width:100px;height:20px}
            </style>
            <div class="host"><div id="if"><div id="ifa" class="item"></div><div id="ifb" class="item"></div></div></div>
            <div class="host"><div id="ig"><div id="iga" class="item"></div><div id="igb" class="item"></div></div></div>"#,
        );
        let laid = layout_dom(&tree, (700.0, 300.0));
        let rect = |name| laid.rects[&tree.get_element_by_id(name).unwrap()];
        for (container, first, second) in [("if", "ifa", "ifb"), ("ig", "iga", "igb")] {
            let container = rect(container);
            let first = rect(first);
            let second = rect(second);
            assert!((container.width - 200.0).abs() < 0.01, "{container:?}");
            assert!(
                (first.y - second.y).abs() < 0.01 && (second.x - first.x - 100.0).abs() < 0.01,
                "inner layout was destroyed: first={first:?} second={second:?}"
            );
        }
    }

    #[test]
    fn overflowing_grid_item_auto_margins_use_safe_logical_start() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                .grid{display:grid;grid-template-columns:300px;grid-template-rows:20px;width:300px;position:relative}
                .item{height:20px}
                .wide{width:600px;height:1px}
                .left{margin-left:auto}
                .both{margin-left:auto;margin-right:auto}
                .center{justify-self:center}
                .end{justify-self:end}
                .absolute{position:absolute;grid-area:1/1;width:600px;height:20px}
            </style>
            <div id="ltr-center-grid" class="grid"><div id="ltr-center" class="item left center"><div class="wide"></div></div></div>
            <div id="ltr-end-grid" class="grid"><div id="ltr-end" class="item both end"><div class="wide"></div></div></div>
            <div id="absolute-center-grid" class="grid"><div id="absolute-center" class="absolute left center"></div></div>
            <div id="absolute-end-grid" class="grid"><div id="absolute-end" class="absolute left end"></div></div>"#,
        );
        let laid = layout_dom(&tree, (800.0, 300.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];

        for (grid_id, item_id, expected_x) in [
            ("ltr-center-grid", "ltr-center", 0.0),
            ("ltr-end-grid", "ltr-end", 0.0),
        ] {
            let grid = rect(grid_id);
            let item = rect(item_id);
            assert!(
                (item.width - 600.0).abs() < 0.01,
                "{item_id} lost its min-content floor: {item:?}"
            );
            assert!(
                (item.x - grid.x - expected_x).abs() < 0.01,
                "{item_id} did not use safe logical-start overflow: grid={grid:?} item={item:?}"
            );
        }

        for (grid_id, item_id, expected_x) in [
            ("absolute-center-grid", "absolute-center", -150.0),
            ("absolute-end-grid", "absolute-end", -300.0),
        ] {
            let grid = rect(grid_id);
            let item = rect(item_id);
            assert!(
                (item.x - grid.x - expected_x).abs() < 0.01,
                "{item_id} incorrectly used the in-flow auto-margin fallback: grid={grid:?} item={item:?}"
            );
        }
    }

    #[test]
    fn root_and_body_inline_outer_geometry_matches_blockification_rules() {
        for display in ["inline-block", "inline-flex", "inline-grid"] {
            let tree = parse_html(&format!(
                r#"<style>
                    html{{margin:0}}body{{margin:0;display:{display}}}
                    .wide{{width:400px;height:20px}}
                </style><div class="wide"></div>"#
            ));
            let laid = layout_dom(&tree, (800.0, 300.0));
            let body = tree
                .descendants(tree.document())
                .into_iter()
                .find(|id| {
                    tree.get_node(*id).is_some_and(|node| {
                        node.as_element()
                            .is_some_and(|element| element.local.as_ref() == "body")
                    })
                })
                .unwrap();
            assert!(
                (laid.rects[&body].width - 400.0).abs() < 0.01,
                "body {display}: {:?}",
                laid.rects[&body]
            );

            let tree = parse_html(&format!(
                r#"<style>
                    html{{margin:0;display:{display}}}body{{margin:0}}
                    .wide{{width:400px;height:20px}}
                </style><div class="wide"></div>"#
            ));
            let laid = layout_dom(&tree, (800.0, 300.0));
            let root = tree
                .descendants(tree.document())
                .into_iter()
                .find(|id| {
                    tree.get_node(*id).is_some_and(|node| {
                        node.as_element()
                            .is_some_and(|element| element.local.as_ref() == "html")
                    })
                })
                .unwrap();
            assert!(
                (laid.rects[&root].width - 800.0).abs() < 0.01,
                "root {display}: {:?}",
                laid.rects[&root]
            );
        }
    }

    #[test]
    fn viewport_overflow_clip_does_not_truncate_root_scrolling_overflow() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0;height:100%}
                html{overflow:hidden}
                main{padding-top:48px}
                #long{height:5000px}
            </style><main id="main"><div id="long"></div></main>"#,
        );
        let laid = layout_dom(&tree, (900.0, 1000.0));
        let main = laid.rects[&tree.get_element_by_id("main").unwrap()];
        let content = laid.scrolling_content_size(&tree, (900.0, 1000.0));
        assert!((main.height - 5048.0).abs() < 0.01, "{main:?}");
        assert_eq!(content, (900.0, 5048.0));
    }

    #[test]
    fn propagated_body_overflow_is_visible_to_layout_and_does_not_create_a_bfc() {
        let tree = parse_html(
            r#"<html style="margin:0;overflow:visible">
               <body style="margin:0;overflow:hidden"><div style="height:20px"></div></body>
               </html>"#,
        );
        let laid = layout_dom(&tree, (100.0, 60.0));
        let body = tree
            .descendants(tree.document())
            .into_iter()
            .find(|id| {
                tree.get_node(*id).is_some_and(|node| {
                    node.as_element()
                        .is_some_and(|element| element.local.as_ref() == "body")
                })
            })
            .unwrap();
        let style = &laid.styles[&body];
        assert!(style.overflow_propagated_to_viewport);
        assert!(!establishes_block_formatting_context(style));
        let taffy = crate::to_taffy_style(style);
        assert_eq!(taffy.overflow.x, taffy::style::Overflow::Visible);
        assert_eq!(taffy.overflow.y, taffy::style::Overflow::Visible);
    }

    #[test]
    fn html_owned_overflow_keeps_body_clip_in_root_scrolling_overflow() {
        let tree = parse_html(
            r#"<html style="margin:0;overflow:auto">
               <body style="margin:0;width:100px;height:50px;overflow:hidden">
                 <div style="position:absolute;left:150px;top:60px;width:20px;height:20px"></div>
               </body>
               </html>"#,
        );
        let laid = layout_dom(&tree, (80.0, 40.0));
        assert_eq!(
            laid.scrolling_content_size(&tree, (80.0, 40.0)),
            (100.0, 50.0)
        );
    }

    #[test]
    fn descendant_overflow_clip_still_bounds_root_scrolling_overflow() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #clip{height:100px;overflow:hidden}
                #long{height:5000px}
            </style><div id="clip"><div id="long"></div></div>"#,
        );
        let laid = layout_dom(&tree, (900.0, 1000.0));
        assert_eq!(
            laid.scrolling_content_size(&tree, (900.0, 1000.0)),
            (900.0, 1000.0)
        );
    }

    #[test]
    fn size_container_auto_block_size_ignores_descendants() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                .container{width:200px}
                .child{height:80px}
                #inline-only{container-type:inline-size}
                #both{container-type:size}
            </style>
            <div id="inline-only" class="container"><div class="child"></div></div>
            <div id="both" class="container"><div class="child"></div></div>"#,
        );
        let laid = layout_dom(&tree, (600.0, 300.0));
        let inline_only = laid.rects[&tree.get_element_by_id("inline-only").unwrap()];
        let both = laid.rects[&tree.get_element_by_id("both").unwrap()];
        assert!((inline_only.height - 80.0).abs() < 0.01, "{inline_only:?}");
        assert!(
            both.height.abs() < 0.01,
            "size containment must use the zero contain-intrinsic block size: {both:?}"
        );
    }

    #[test]
    fn container_type_establishes_a_float_containing_formatting_context() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #container{container-type:inline-size;width:200px}
                #float{float:left;width:50px;height:40px}
            </style>
            <div id="container"><div id="float"></div></div>"#,
        );
        let laid = layout_dom(&tree, (600.0, 200.0));
        let container = laid.rects[&tree.get_element_by_id("container").unwrap()];
        assert!(
            (container.height - 40.0).abs() < 0.01,
            "query container must contain its descendant float: {container:?}"
        );
    }

    #[test]
    fn consecutive_percentage_floats_collect_against_the_full_band_width() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #container{box-sizing:border-box;width:780px}
                .cell{float:left;box-sizing:border-box;width:25%;padding:0 15px}
                #a{height:87px} #b{height:85px} #c{height:150px} #d{height:76px}
            </style>
            <div id="container">
              <div id="a" class="cell"></div>
              <div id="b" class="cell"></div>
              <div id="c" class="cell"></div>
              <div id="d" class="cell"></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (900.0, 300.0));
        for (index, id) in ["a", "b", "c", "d"].into_iter().enumerate() {
            let rect = laid.rects[&tree.get_element_by_id(id).unwrap()];
            assert!((rect.width - 195.0).abs() < 0.01, "{id}: {rect:?}");
            assert!(
                rect.y.abs() < 0.01,
                "four 25% floats must share the 780px band; {id} wrapped: {rect:?}"
            );
            assert!(
                (rect.x - index as f32 * 195.0).abs() < 0.01,
                "{id}: {rect:?}"
            );
        }
        let container = laid.rects[&tree.get_element_by_id("container").unwrap()];
        assert!(
            (container.height - 150.0).abs() < 0.01,
            "one float band must be as tall as its tallest float: {container:?}"
        );
    }

    #[test]
    fn single_float_keeps_normal_block_outer_width_and_shortens_its_line_band() {
        // Chromium 145 geometry for a Bootstrap-style single header float,
        // an ordinary full-width collapse block, and an overflow BFC below it.
        let tree = parse_html(
            r#"<style>
                *{box-sizing:border-box} html,body{margin:0}
                #parent{width:780px}
                #parent::before,#parent::after{display:table;content:" "}
                #parent::after{clear:both}
                #float{float:left;height:100px}
                #float::after{display:table;clear:both;content:" "}
                #brand{float:left;width:250px;height:100px}
                #flow{height:50px}
                #line{display:inline-block;width:400px;height:20px}
                #bfc{overflow:hidden;height:30px}
            </style>
            <div id="parent">
              <div id="float"><div id="brand"></div></div>
              <div id="flow"><span id="line"></span></div>
              <div id="bfc"></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (900.0, 300.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        let parent = rect("parent");
        let float = rect("float");
        let flow = rect("flow");
        let line = rect("line");
        let bfc = rect("bfc");

        assert_eq!(
            (parent.x, parent.y, parent.width, parent.height),
            (0.0, 0.0, 780.0, 100.0)
        );
        assert_eq!(
            (float.x, float.y, float.width, float.height),
            (0.0, 0.0, 250.0, 100.0)
        );
        assert_eq!(
            (flow.x, flow.y, flow.width, flow.height),
            (0.0, 0.0, 780.0, 50.0),
            "a normal block border box spans beneath the float"
        );
        assert_eq!(
            (line.x, line.y, line.width, line.height),
            (250.0, 0.0, 400.0, 20.0),
            "inline content starts at the float band's available edge"
        );
        assert_eq!(
            (bfc.x, bfc.y, bfc.width, bfc.height),
            (250.0, 50.0, 530.0, 30.0),
            "a float-avoiding BFC moves and shrinks as one box"
        );
    }

    #[test]
    fn display_none_floats_do_not_hide_a_later_visible_float_from_the_band() {
        // Bootstrap keeps its responsive toggle/cart floated even when a
        // desktop media query makes them display:none. Non-generated boxes
        // with display:none must not enter float selection or split the
        // visible brand out of its float band.
        let tree = parse_html(
            r#"<style>
                *{box-sizing:border-box} html,body{margin:0}
                #container{width:780px}
                #header{float:left;width:780px;height:100px}
                #header::before,#header::after{display:table;content:" "}
                #header::after{clear:both}
                .hidden-float{display:none;float:right}
                #brand{float:left;width:208px;height:100px;margin-right:33px}
                #flow{height:50px}
                #line{display:inline-block;width:400px;height:20px}
            </style>
            <div id="container">
              <div id="header">
                <button class="hidden-float">menu</button>
                <a id="brand"></a>
                <div id="flow"><span id="line"></span></div>
                <span class="hidden-float">cart</span>
              </div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (900.0, 300.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        let header = rect("header");
        let brand = rect("brand");
        let flow = rect("flow");
        let line = rect("line");

        assert_eq!(
            (brand.x, brand.y, brand.width, brand.height),
            (0.0, 0.0, 208.0, 100.0)
        );
        assert_eq!(
            (flow.x, flow.y, flow.width, flow.height),
            (0.0, 0.0, 780.0, 50.0),
            "an ordinary flow block keeps its full border-box width beneath a float"
        );
        assert_eq!(
            (line.x, line.y, line.width, line.height),
            (241.0, 0.0, 400.0, 20.0),
            "display:none floats must not suppress the later visible float's line band"
        );
        assert_eq!((header.width, header.height), (780.0, 100.0));
    }

    #[test]
    fn html_comments_do_not_split_nested_native_float_bands() {
        // HTML comments generate no box. Bootstrap emits descriptive comments
        // between its floated navbar header, ordinary full-width collapse
        // block, and the floats inside that block. Gecko's per-BFC float
        // manager ignores those nodes completely.
        let tree = parse_html(
            r#"<style>
                *{box-sizing:border-box} html,body{margin:0}
                #parent{width:780px;height:100px}
                #parent::before,#parent::after,#collapse::before,#collapse::after{
                    display:table;content:" "
                }
                #parent::after,#collapse::after{clear:both}
                #header{float:left;width:250px;height:100px}
                #collapse{position:relative;padding:0 15px}
                #nav{float:left;width:400px;height:50px;margin-top:20px}
                #right{float:right;width:80px;height:50px;margin-top:20px}
            </style>
            <div id="parent">
              <!-- floated brand wrapper -->
              <div id="header"></div><!-- /.navbar-header -->
              <div id="collapse">
                <!-- desktop navigation -->
                <div id="nav"></div><!-- /.navbar-nav -->
                <div id="right"></div>
              </div><!-- /.navbar-collapse -->
            </div>"#,
        );
        let laid = layout_dom(&tree, (900.0, 300.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        let collapse = rect("collapse");
        let nav = rect("nav");
        let right = rect("right");

        assert_eq!(
            (collapse.x, collapse.y, collapse.width, collapse.height),
            (0.0, 0.0, 780.0, 100.0),
            "the ordinary block border box spans beneath the outer float"
        );
        assert_eq!((nav.x, nav.y, nav.width, nav.height), (250.0, 20.0, 400.0, 50.0));
        assert_eq!(
            (right.x, right.y, right.width, right.height),
            (685.0, 20.0, 80.0, 50.0)
        );
    }

    #[test]
    fn auto_width_float_uses_its_own_bfc_for_nested_float_max_content() {
        // A float establishes a BFC. Its auto inline size must therefore be
        // measured from its nested float's max-content line, not through the
        // cyclic width:100% synthetic row used for non-native float zones.
        let tree = parse_html(
            r#"<style>
                *{box-sizing:border-box} html,body{margin:0}
                #container{width:780px}
                #container::before,#container::after,
                #header::before,#header::after{display:table;content:" "}
                #container::after,#header::after{clear:both}
                #header{float:left;height:100px}
                #brand{float:left;height:100px;padding:15px;margin-right:33px}
                #logo{width:70px;height:70px}
                #label{font-size:34.5px;line-height:20px}
                #toggle{display:none;float:right;padding:14px;border:1px solid}
                .icon{display:block;width:22px;height:2px}
                #flow{height:50px}
            </style>
            <div id="container"><div id="header"><button id="toggle"><span class="icon"></span><span class="icon"></span><span class="icon"></span></button><a id="brand"><img id="logo"><span id="label">porkbun</span></a></div><div id="flow"></div></div>"#,
        );
        let laid = layout_dom(&tree, (900.0, 300.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        let header = rect("header");
        let brand = rect("brand");
        let logo = rect("logo");

        assert_eq!((logo.width, logo.height), (70.0, 70.0));
        assert!(
            brand.width > 220.0,
            "the nested float must aggregate the image and label on its max-content line: {brand:?}"
        );
        assert!(
            header.width > 255.0 && header.width < 259.0,
            "the hidden 22px toggle icon must not cap the outer shrink-to-fit width: {header:?}"
        );
        assert!(
            (header.width - brand.width - 33.0).abs() < 0.01,
            "the outer auto-width float must include the nested float's margin box: header={header:?}, brand={brand:?}"
        );
    }

    #[test]
    fn ineligible_nearest_inline_container_does_not_fall_through() {
        let tree = parse_html(
            r#"<style>
                #outer{container-type:inline-size;width:500px}
                #inner{display:inline;container-type:inline-size}
                #target{width:1px}
                @container (min-width:400px){#target{width:99px}}
            </style>
            <div id="outer"><span id="inner"><span id="target"></span></span></div>"#,
        );
        let laid = layout_dom(&tree, (600.0, 200.0));
        let target = tree.get_element_by_id("target").unwrap();
        assert_eq!(
            laid.styles[&target].width,
            crate::Dimension::Px(1.0),
            "the unavailable nearest query axis must be unknown, not select the outer container"
        );
    }

    #[test]
    fn ineligible_nearest_display_contents_container_does_not_fall_through() {
        let tree = parse_html(
            r#"<style>
                #outer{container:query/inline-size;width:500px}
                #inner{display:contents;container:query/inline-size}
                #target{width:1px}
                @container query (min-width:400px){#target{width:99px}}
            </style>
            <div id="outer"><div id="inner"><div id="target"></div></div></div>"#,
        );
        let laid = layout_dom(&tree, (600.0, 200.0));
        let target = tree.get_element_by_id("target").unwrap();
        assert_eq!(
            laid.styles[&target].width,
            crate::Dimension::Px(1.0),
            "a named display:contents container must remain the nearest match with unavailable axes"
        );
    }

    #[test]
    fn ineligible_nearest_table_container_does_not_fall_through() {
        let tree = parse_html(
            r#"<style>
                #outer{container-type:inline-size;width:500px}
                #inner{display:table;container-type:size;width:300px}
                #content{height:80px}
                #target{width:1px}
                @container (min-width:400px){#target{width:99px}}
            </style>
            <div id="outer">
              <div id="inner"><div id="content"><div id="target"></div></div></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (600.0, 200.0));
        let inner = tree.get_element_by_id("inner").unwrap();
        let target = tree.get_element_by_id("target").unwrap();
        assert!(
            laid.styles[&inner].is_table_box,
            "the internal block approximation lost authored table provenance"
        );
        assert_eq!(
            laid.styles[&target].width,
            crate::Dimension::Px(1.0),
            "an unavailable nearest table query axis must not fall through to the outer container"
        );
        assert!(
            (laid.rects[&inner].height - 80.0).abs() < 0.01,
            "container-type must not apply size containment to an authored table box: {:?}",
            laid.rects[&inner]
        );
    }

    #[test]
    fn container_type_does_not_size_contain_internal_table_cell() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                table{border-spacing:0}
                #cell{container-type:size;padding:0}
                #content{height:80px;width:120px}
            </style>
            <table><tr><td id="cell"><div id="content"></div></td></tr></table>"#,
        );
        let laid = layout_dom(&tree, (600.0, 200.0));
        let cell = tree.get_element_by_id("cell").unwrap();
        assert!(
            laid.styles[&cell].internal_flex_container,
            "the test must exercise the internal table-cell representation"
        );
        assert!(
            laid.rects[&cell].height >= 79.9,
            "container-type must not zero an internal table cell: {:?}",
            laid.rects[&cell]
        );
    }

    #[test]
    fn formerly_oscillating_inline_container_settles_consistently() {
        let tree = parse_html(
            r#"<style>
                #container{display:inline-block;container-type:inline-size}
                #child{width:200px;height:10px}
                @container (min-width:100px){#child{width:50px}}
            </style>
            <div id="container"><div id="child"></div></div>"#,
        );
        let (laid, telemetry) =
            layout_dom_with_web_fonts_measured(&tree, (600.0, 200.0), &HashMap::new(), &[]);
        let container = laid.rects[&tree.get_element_by_id("container").unwrap()];
        let child = tree.get_element_by_id("child").unwrap();
        assert!(container.width.abs() < 0.01, "{container:?}");
        assert_eq!(laid.styles[&child].width, crate::Dimension::Px(200.0));
        assert!(matches!(
            telemetry.termination,
            ContainerLayoutTermination::GeometryStable
                | ContainerLayoutTermination::SignatureStable
        ));
    }

    #[test]
    fn pass_cap_uses_visible_conservative_fallback_without_panicking() {
        let tree = parse_html(
            r#"<style>
                #c0{container:c0/inline-size;width:100px}
                #target{width:1px}
                @container c0 (min-width:100px){
                    #c1{container:c1/inline-size;width:100px}
                }
                @container c1 (min-width:100px){#target{width:99px}}
            </style>
            <div id="c0"><div id="c1"><div id="target"></div></div></div>"#,
        );
        let (laid, telemetry) = layout_dom_with_web_fonts_pass_limit(
            &tree,
            (600.0, 200.0),
            &HashMap::new(),
            &[],
            Some(2),
            None,
            None,
            &[],
        );
        let target = tree.get_element_by_id("target").unwrap();
        assert_eq!(laid.styles[&target].width, crate::Dimension::Px(1.0));
        assert_eq!(
            telemetry.termination,
            ContainerLayoutTermination::PassCapFallback
        );
        assert_eq!(telemetry.passes, 3);
    }

    #[test]
    fn ten_level_container_activation_propagates_without_fixed_pass_truncation() {
        let mut css = String::from("#c0{container:c0/inline-size;width:100px}#target{width:1px}");
        for level in 0..10 {
            css.push_str(&format!(
                "@container c{level} (min-width:100px){{#c{}{{container:c{}/inline-size;width:100px}}}}",
                level + 1,
                level + 1
            ));
        }
        css.push_str("@container c10 (min-width:100px){#target{width:77px}}");
        let mut body = String::new();
        for level in 0..=10 {
            body.push_str(&format!("<div id=\"c{level}\">"));
        }
        body.push_str("<div id=\"target\"></div>");
        for _ in 0..=10 {
            body.push_str("</div>");
        }
        let tree = parse_html(&format!("<style>{css}</style>{body}"));
        let (laid, telemetry) =
            layout_dom_with_web_fonts_measured(&tree, (800.0, 600.0), &HashMap::new(), &[]);
        let target = tree.get_element_by_id("target").unwrap();
        assert_eq!(laid.styles[&target].width, crate::Dimension::Px(77.0));
        assert!(
            telemetry.passes > 8,
            "deep propagation unexpectedly truncated: {telemetry:?}"
        );
        assert!(!matches!(
            telemetry.termination,
            ContainerLayoutTermination::PassCapFallback
                | ContainerLayoutTermination::OscillationFallback
        ));
    }

    #[test]
    fn layout_without_container_queries_keeps_one_pass_fast_path() {
        let tree = parse_html(
            r#"<html><head><style>#target{width:25px}</style></head>
               <body><div id="target"></div></body></html>"#,
        );
        let (_, telemetry) =
            layout_dom_with_web_fonts_measured(&tree, (800.0, 600.0), &HashMap::new(), &[]);
        assert_eq!(
            telemetry,
            ContainerLayoutTelemetry {
                passes: 1,
                termination: ContainerLayoutTermination::NoQueries,
                query: crate::css::ContainerQueryStats::default(),
                retained_reused: 0,
                retained_fresh: 0,
                retained_fallback: 0,
            }
        );
    }

    #[test]
    fn dormant_container_queries_without_a_container_keep_one_pass_fast_path() {
        let tree = parse_html(
            r#"<html><head><style>
                #target{width:25px}
                @container (min-width: 1px){#target{width:99px}}
            </style></head><body><div id="target"></div></body></html>"#,
        );
        let (laid, telemetry) =
            layout_dom_with_web_fonts_measured(&tree, (800.0, 600.0), &HashMap::new(), &[]);
        let target = tree.get_element_by_id("target").unwrap();
        assert_eq!(laid.styles[&target].width, crate::Dimension::Px(25.0));
        assert_eq!(
            telemetry,
            ContainerLayoutTelemetry {
                passes: 1,
                termination: ContainerLayoutTermination::NoContainers,
                query: crate::css::ContainerQueryStats::default(),
                retained_reused: 0,
                retained_fresh: 0,
                retained_fallback: 0,
            }
        );
    }

    #[test]
    fn retained_attribute_styles_match_forced_full_with_dormant_container_rules() {
        let initial_html = r#"<html><head><style>
            html,body{margin:0}
            .theme[data-theme=dark] .card{--card-width:42px;color:#123456}
            .card .child{width:var(--card-width,18px);height:2em;font-size:10px}
            .theme[data-theme=dark] .card::before{content:'dark';display:block;width:7px}
            .toggle[data-open=true] + .panel{--peer-width:31px}
            .panel .peer{width:var(--peer-width,13px);height:9px}
            .counter-scope{counter-reset:step}
            .counter-item{counter-increment:step;display:block;height:9px}
            .counter-item[data-double=true]{counter-increment:step 2}
            .counter-item::before{content:counter(step)}
            .clean{width:77px;height:11px}
            @container dormant (min-width:10px){.child{width:99px}}
        </style></head><body>
            <section id=theme class=theme data-theme=light><div id=card class=card><span id=child class=child></span></div></section>
            <div id=toggle class=toggle data-open=false></div><div class=panel><span id=peer class=peer></span></div>
            <section class=counter-scope><span id=counter-one class=counter-item></span><span id=counter-change class=counter-item data-double=false></span><span id=counter-after class=counter-item></span></section>
            <aside id=clean class=clean></aside>
            <div style="display:flex"><button id=control><span>button</span></button><div id=inherit style="display:inherit">x</div></div>
        </body></html>"#;
        let final_html = initial_html
            .replace("data-theme=light", "data-theme=dark")
            .replace("data-open=false", "data-open=true")
            .replace("data-double=false", "data-double=true");
        let tree = parse_html(initial_html);
        let mut cache = crate::css::StylesheetCache::default();
        let mut initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (300.0, 200.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        let theme = tree.get_element_by_id("theme").unwrap();
        let toggle = tree.get_element_by_id("toggle").unwrap();
        let counter_change = tree.get_element_by_id("counter-change").unwrap();
        tree.with_node_mut(theme, |node| node.set_attribute("data-theme", "dark".into()));
        tree.with_node_mut(toggle, |node| node.set_attribute("data-open", "true".into()));
        tree.with_node_mut(counter_change, |node| {
            node.set_attribute("data-double", "true".into())
        });
        let mutations = vec![
            AttributeStyleMutation {
                node: theme,
                name: "data-theme".into(),
                old_value: Some("light".into()),
                new_value: Some("dark".into()),
            }
            .into(),
            AttributeStyleMutation {
                node: toggle,
                name: "data-open".into(),
                old_value: Some("false".into()),
                new_value: Some("true".into()),
            }
            .into(),
            AttributeStyleMutation {
                node: counter_change,
                name: "data-double".into(),
                old_value: Some("false".into()),
                new_value: Some("true".into()),
            }
            .into(),
        ];
        let retained = RetainedStyleMaps {
            styles: std::mem::take(&mut initial.styles),
            custom_properties: std::mem::take(&mut initial.custom_properties),
        };
        let (incremental, telemetry) = layout_dom_with_web_fonts_pass_limit(
            &tree,
            (300.0, 200.0),
            &HashMap::new(),
            &[],
            None,
            Some(&mut cache),
            Some(retained),
            &mutations,
        );
        let final_tree = parse_html(&final_html);
        let full = layout_dom(&final_tree, (300.0, 200.0));

        assert_eq!(telemetry.termination, ContainerLayoutTermination::NoContainers);
        assert!(telemetry.retained_reused > 0, "clean branch must be reused");
        assert!(telemetry.retained_fresh > 0, "dirty subtrees must be fresh");
        assert_eq!(telemetry.retained_fallback, 0);
        for id in [
            "card",
            "child",
            "peer",
            "counter-one",
            "counter-change",
            "counter-after",
            "clean",
            "control",
            "inherit",
        ] {
            let incremental_id = tree.get_element_by_id(id).unwrap();
            let full_id = final_tree.get_element_by_id(id).unwrap();
            assert_eq!(
                incremental.rects.get(&incremental_id),
                full.rects.get(&full_id),
                "{id} rect"
            );
            assert_eq!(
                incremental.styles[&incremental_id].width,
                full.styles[&full_id].width,
                "{id} width"
            );
            assert_eq!(
                incremental.styles[&incremental_id].color,
                full.styles[&full_id].color,
                "{id} color"
            );
            assert_eq!(
                incremental.styles[&incremental_id].before_content,
                full.styles[&full_id].before_content,
                "{id} generated content"
            );
            assert_eq!(
                incremental.custom_properties[&incremental_id],
                full.custom_properties[&full_id],
                "{id} custom properties"
            );
        }
    }

    #[test]
    fn retained_waapi_restyle_dirties_only_the_non_inherited_effect_target() {
        let tree = parse_html(
            "<main><section id=target><div id=child><span id=grandchild></span></div></section><aside id=clean></aside></main>",
        );
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = crate::css::Stylesheet::parse_for_viewport(&tree, &[], (800.0, 600.0));
        let RetainedStylePlan::Reuse {
            dirty,
            has_animation_damage,
        } = retained_style_plan(
            &tree,
            &sheet,
            &[RetainedStyleMutation::WaapiAnimation { node: target }],
        ) else {
            panic!("WAAPI target damage must remain retainable")
        };
        assert_eq!(dirty, HashSet::from([target]));
        assert!(has_animation_damage);
    }

    #[test]
    fn retained_animation_restyle_reuses_clean_branches_and_matches_full() {
        let mut clean = String::new();
        for index in 0..2_000 {
            clean.push_str(&format!(
                "<span class=clean data-index={index}><i></i></span>"
            ));
        }
        let tree = parse_html(&format!(
            r#"<style>
                html,body{{margin:0}}
                @keyframes grow {{
                    from {{ width:20px; color:#ff0000 }}
                    to {{ width:120px; color:#0000ff }}
                }}
                #animated{{height:20px;animation:grow 1000ms linear both}}
                #animated > b{{color:inherit}}
                .clean{{display:block;width:3px;height:1px}}
            </style><main><section id=animated><b id=child>x</b></section>
            <aside>{clean}</aside></main>"#,
        ));
        let animated = tree.get_element_by_id("animated").unwrap();
        let mut cache = crate::css::StylesheetCache::default();
        let mut timeline = crate::AnimationTimelineState::default();
        let (mut initial, _) = layout_dom_with_web_fonts_pass_limit_at_animation_time(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            None,
            Some(&mut cache),
            None,
            &[],
            crate::CssMediaType::Screen,
            crate::AnimationSample::document(0.0),
            &mut timeline,
        );
        let retained = RetainedStyleMaps {
            styles: std::mem::take(&mut initial.styles),
            custom_properties: std::mem::take(&mut initial.custom_properties),
        };
        let (incremental, telemetry) =
            layout_dom_with_web_fonts_pass_limit_at_animation_time(
                &tree,
                (800.0, 600.0),
                &HashMap::new(),
                &[],
                None,
                Some(&mut cache),
                Some(retained),
                &[RetainedStyleMutation::Animation { node: animated }],
                crate::CssMediaType::Screen,
                crate::AnimationSample::document(500.0),
                &mut timeline,
            );
        let mut full_timeline = crate::AnimationTimelineState::default();
        let (full, _) = layout_dom_with_web_fonts_pass_limit_at_animation_time(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            None,
            Some(&mut cache),
            None,
            &[],
            crate::CssMediaType::Screen,
            crate::AnimationSample::document(500.0),
            &mut full_timeline,
        );

        assert_computed_styles_match("animation retained-vs-full", &incremental, &full);
        assert_eq!(incremental.rects, full.rects);
        assert_eq!(telemetry.retained_fallback, 0, "{telemetry:?}");
        assert!(telemetry.retained_reused >= 4_000, "{telemetry:?}");
        assert!(telemetry.retained_fresh < 16, "{telemetry:?}");
        assert!(
            (incremental.rects[&animated].width - 70.0).abs() < 0.1,
            "animated geometry did not advance: {:?}",
            incremental.rects[&animated]
        );
        let child = tree.get_element_by_id("child").unwrap();
        assert_eq!(incremental.styles[&child].color, Some([128, 0, 128, 255]));
    }

    #[test]
    fn retained_insertions_with_active_container_queries_match_forced_full() {
        let mut clean = String::new();
        for index in 0..200 {
            clean.push_str(&format!("<i class=clean data-index={index}></i>"));
        }
        let tree = parse_html(&format!(
            r#"<!doctype html><style>
                #container{{container-type:inline-size;width:200px}}
                .item{{width:11px;height:7px}}
                @container (min-width:100px){{.item{{width:71px}}}}
                .clean{{height:1px}}
            </style><main><section id=container><div id=stable class=item></div>
                <div id=popup class=item><b id=first></b><b id=second></b></div>
            </section><aside>{clean}</aside></main>"#,
        ));
        let container = tree.get_element_by_id("container").unwrap();
        let popup = tree.get_element_by_id("popup").unwrap();
        let first = tree.get_element_by_id("first").unwrap();
        let second = tree.get_element_by_id("second").unwrap();
        tree.remove_child(popup);
        tree.remove_child(first);
        tree.remove_child(second);

        let mut cache = crate::css::StylesheetCache::default();
        let mut initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (500.0, 300.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.append_child(container, popup);
        tree.append_child(popup, first);
        tree.append_child(popup, second);
        let retained = RetainedStyleMaps {
            styles: std::mem::take(&mut initial.styles),
            custom_properties: std::mem::take(&mut initial.custom_properties),
        };
        let mutations = [
            TreeStyleMutation::Insert {
                node: popup,
                old_parent: None,
                new_parent: container,
            },
            TreeStyleMutation::Insert {
                node: first,
                old_parent: None,
                new_parent: popup,
            },
            TreeStyleMutation::Insert {
                node: second,
                old_parent: None,
                new_parent: popup,
            },
        ]
        .map(RetainedStyleMutation::from);
        let (incremental, telemetry) = layout_dom_with_web_fonts_pass_limit(
            &tree,
            (500.0, 300.0),
            &HashMap::new(),
            &[],
            None,
            Some(&mut cache),
            Some(retained),
            &mutations,
        );
        let full = layout_dom(&tree, (500.0, 300.0));

        assert_computed_styles_match("active container insertion batch", &incremental, &full);
        assert_eq!(incremental.rects, full.rects);
        assert_eq!(telemetry.retained_fallback, 0, "{telemetry:?}");
        assert!(telemetry.retained_reused >= 200, "{telemetry:?}");
        assert!(telemetry.retained_fresh > 0, "{telemetry:?}");
        assert_eq!(
            incremental.styles[&tree.get_element_by_id("stable").unwrap()].width,
            crate::Dimension::Px(71.0)
        );
        assert_eq!(incremental.styles[&popup].width, crate::Dimension::Px(71.0));
    }

    #[test]
    fn retained_dormant_container_queries_do_not_dirty_large_tree() {
        let mut peers = String::new();
        for index in 0..1_000 {
            peers.push_str(&format!("<i class=peer data-index={index}></i>"));
        }
        let tree = parse_html(&format!(
            r#"<!doctype html><style>
                .peer{{height:1px}}
                @container (min-width:1px){{*{{color:#123456}}}}
            </style><main><input id=subject>{peers}</main>"#,
        ));
        let subject = tree.get_element_by_id("subject").unwrap();
        let mut cache = crate::css::StylesheetCache::default();
        let mut initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (500.0, 300.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.with_node_mut(subject, |node| {
            node.set_attribute("autocomplete", "off".into())
        });
        let retained = RetainedStyleMaps {
            styles: std::mem::take(&mut initial.styles),
            custom_properties: std::mem::take(&mut initial.custom_properties),
        };
        let (_, telemetry) = layout_dom_with_web_fonts_pass_limit(
            &tree,
            (500.0, 300.0),
            &HashMap::new(),
            &[],
            None,
            Some(&mut cache),
            Some(retained),
            &[AttributeStyleMutation {
                node: subject,
                name: "autocomplete".into(),
                old_value: None,
                new_value: Some("off".into()),
            }
            .into()],
        );
        assert_eq!(telemetry.retained_fallback, 0, "{telemetry:?}");
        assert_eq!(telemetry.retained_fresh, 0, "{telemetry:?}");
        assert!(telemetry.retained_reused >= 1_000, "{telemetry:?}");
    }

    #[test]
    fn retained_nested_universal_container_reset_is_bounded_and_matches_full() {
        let mut queried = String::new();
        let mut clean = String::new();
        for index in 0..500 {
            queried.push_str(&format!("<i class=item data-index={index}></i>"));
            clean.push_str(&format!("<i class=clean data-index={index}></i>"));
        }
        let tree = parse_html(&format!(
            r#"<!doctype html><style>
                #outer,#inner{{container-type:inline-size;width:200px}}
                .item,.clean{{height:1px}}
                @container (min-width:100px){{
                    *{{color:#123456}}
                    @container (min-width:100px){{*{{height:2px}}}}
                }}
            </style><main><section id=outer><section id=inner>{queried}</section></section>
            <aside><input id=subject>{clean}</aside></main>"#,
        ));
        let subject = tree.get_element_by_id("subject").unwrap();
        let mut cache = crate::css::StylesheetCache::default();
        let mut initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.with_node_mut(subject, |node| {
            node.set_attribute("autocomplete", "off".into())
        });
        let retained = RetainedStyleMaps {
            styles: std::mem::take(&mut initial.styles),
            custom_properties: std::mem::take(&mut initial.custom_properties),
        };
        let (incremental, telemetry) = layout_dom_with_web_fonts_pass_limit(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            None,
            Some(&mut cache),
            Some(retained),
            &[AttributeStyleMutation {
                node: subject,
                name: "autocomplete".into(),
                old_value: None,
                new_value: Some("off".into()),
            }
            .into()],
        );
        let full = layout_dom(&tree, (800.0, 600.0));
        assert_computed_styles_match("nested universal CQ reset", &incremental, &full);
        assert_eq!(incremental.rects, full.rects);
        assert_eq!(telemetry.retained_fallback, 0, "{telemetry:?}");
        assert!(telemetry.retained_fresh < 530, "{telemetry:?}");
        assert!(telemetry.retained_reused >= 500, "{telemetry:?}");
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum RetainedDifferentialExpectation {
        Incremental,
        ReuseAll,
        ConservativeFallback,
    }

    struct RetainedDifferentialCase {
        name: &'static str,
        css: &'static str,
        target: &'static str,
        attribute: &'static str,
        new_value: &'static str,
        expectation: RetainedDifferentialExpectation,
    }

    /// Compare every computed-style field through its Debug representation.
    /// `LayoutStyle` intentionally does not implement PartialEq because it is
    /// a large, evolving renderer-internal type; keeping this check centralized
    /// means new fields automatically enter the retained-vs-full oracle.
    fn assert_computed_styles_match(
        case: &str,
        incremental: &DomLayout,
        full: &DomLayout,
    ) {
        assert_eq!(
            incremental.styles.keys().collect::<HashSet<_>>(),
            full.styles.keys().collect::<HashSet<_>>(),
            "{case}: computed-style node set"
        );
        for (node, incremental_style) in &incremental.styles {
            assert_eq!(
                format!("{incremental_style:#?}"),
                format!("{:#?}", full.styles[node]),
                "{case}: computed style for {node:?}"
            );
        }
    }

    fn run_retained_differential_case(case: &RetainedDifferentialCase) {
        let html = format!(
            r#"<!doctype html><html><head><style>
                html,body{{margin:0}}
                .clean{{width:71px;height:13px}}
                {}
            </style></head><body>
                <section id="scope" class="scope theme-off" data-mode="off">
                  <div id="subject" class="subject state-off" data-state="off">
                    <span id="child" class="child"><i id="grand" class="grand leaf"></i></span>
                  </div>
                  <div id="adjacent" class="panel"><span id="adjacent-child" class="leaf"></span></div>
                  <div id="following" class="panel"><span id="following-child" class="leaf"></span></div>
                </section>
                <input id="control" type="text" size="20" value="old">
                <img id="image" alt="old">
                <aside id="clean" class="clean"><span id="clean-child"></span></aside>
            </body></html>"#,
            case.css
        );
        let tree = parse_html(&html);
        let mut cache = crate::css::StylesheetCache::default();
        let mut initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (320.0, 240.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        let target = tree.get_element_by_id(case.target).unwrap();
        let old_value = tree
            .get_node(target)
            .and_then(|node| node.get_attribute(case.attribute).map(str::to_owned));
        tree.with_node_mut(target, |node| {
            node.set_attribute(case.attribute, case.new_value.into())
        });
        let retained = RetainedStyleMaps {
            styles: std::mem::take(&mut initial.styles),
            custom_properties: std::mem::take(&mut initial.custom_properties),
        };
        let (incremental, telemetry) = layout_dom_with_web_fonts_pass_limit(
            &tree,
            (320.0, 240.0),
            &HashMap::new(),
            &[],
            None,
            Some(&mut cache),
            Some(retained),
            &[AttributeStyleMutation {
                node: target,
                name: case.attribute.into(),
                old_value,
                new_value: Some(case.new_value.into()),
            }
            .into()],
        );
        let full = layout_dom(&tree, (320.0, 240.0));

        match case.expectation {
            RetainedDifferentialExpectation::Incremental => {
                assert_eq!(telemetry.retained_fallback, 0, "{}", case.name);
                assert!(telemetry.retained_fresh > 0, "{}", case.name);
                assert!(telemetry.retained_reused > 0, "{}", case.name);
            }
            RetainedDifferentialExpectation::ReuseAll => {
                assert_eq!(telemetry.retained_fallback, 0, "{}", case.name);
                assert_eq!(telemetry.retained_fresh, 0, "{}", case.name);
                assert!(telemetry.retained_reused > 0, "{}", case.name);
            }
            RetainedDifferentialExpectation::ConservativeFallback => {
                assert_eq!(telemetry.retained_fallback, 1, "{}", case.name);
                assert_eq!(telemetry.retained_reused, 0, "{}", case.name);
            }
        }
        assert_computed_styles_match(case.name, &incremental, &full);
        assert_eq!(incremental.rects, full.rects, "{}: border boxes", case.name);
        assert_eq!(
            incremental.inline_fragments, full.inline_fragments,
            "{}: inline fragments",
            case.name
        );
        assert_eq!(incremental.text_runs, full.text_runs, "{}: text runs", case.name);
        assert_eq!(
            incremental.custom_properties, full.custom_properties,
            "{}: custom properties",
            case.name
        );
    }

    #[test]
    fn retained_attribute_styles_match_forced_full_selector_matrix() {
        use RetainedDifferentialExpectation::{
            ConservativeFallback, Incremental, ReuseAll,
        };
        let cases = [
            RetainedDifferentialCase {
                name: "self attribute selector",
                css: ".subject[data-state=on]{width:41px;height:7px;color:#123456}",
                target: "subject",
                attribute: "data-state",
                new_value: "on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "child and descendant selectors",
                css: ".subject[data-state=on]>.child .grand{width:43px;height:9px}",
                target: "subject",
                attribute: "data-state",
                new_value: "on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "ancestor selector",
                css: ".scope[data-mode=on] .grand{width:47px;height:11px}",
                target: "scope",
                attribute: "data-mode",
                new_value: "on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "adjacent sibling selector",
                css: ".subject[data-state=on]+.panel{width:53px;height:13px}",
                target: "subject",
                attribute: "data-state",
                new_value: "on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "general sibling selector",
                css: ".subject[data-state=on]~.panel{width:59px;height:15px}",
                target: "subject",
                attribute: "data-state",
                new_value: "on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "inheritance and custom property",
                css: ".scope[data-mode=on]{--leaf-width:61px;color:#234567;font-size:20px}.leaf{width:var(--leaf-width,17px);height:1em;color:inherit}",
                target: "scope",
                attribute: "data-mode",
                new_value: "on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "generated before and after content",
                css: ".subject[data-state=on]::before{content:'before';display:block;width:5px}.subject[data-state=on]::after{content:'after';display:block;width:7px}",
                target: "subject",
                attribute: "data-state",
                new_value: "on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "declaration attr dependency",
                css: ".subject::before{content:attr(data-state);display:block;width:11px}",
                target: "subject",
                attribute: "data-state",
                new_value: "on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "inline style and inherited custom property",
                css: ".subject .grand{width:var(--leaf-width,17px);color:inherit}",
                target: "subject",
                attribute: "style",
                new_value: "width:113px;height:29px;--leaf-width:37px;color:#345678",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "inline style attribute selector reaches sibling",
                css: ".subject[style]+.panel{width:127px;height:31px}",
                target: "subject",
                attribute: "style",
                new_value: "height:23px",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "counter propagation through retained siblings",
                css: ".scope{counter-reset:item}.subject,.panel{counter-increment:item}.subject[data-state=on]{counter-increment:item 3}.subject::before,.panel::before{content:counter(item)}",
                target: "subject",
                attribute: "data-state",
                new_value: "on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "class replacement",
                css: ".subject.state-on .grand{width:67px;height:17px}",
                target: "subject",
                attribute: "class",
                new_value: "subject state-on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "id replacement",
                css: "#subject-on .grand{width:71px;height:19px}",
                target: "subject",
                attribute: "id",
                new_value: "subject-on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "selector-list pseudo",
                css: ".scope:is([data-mode=on],.alternate) .grand{width:73px;height:21px}",
                target: "scope",
                attribute: "data-mode",
                new_value: "on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "negation pseudo",
                css: ".subject:not([data-state=off]){width:79px;height:23px}",
                target: "subject",
                attribute: "data-state",
                new_value: "on",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "mixed sibling descendant traversal",
                css: ".subject[data-state=on]~.panel .leaf{width:83px;height:25px}",
                target: "subject",
                attribute: "data-state",
                new_value: "on",
                expectation: ConservativeFallback,
            },
            RetainedDifferentialCase {
                name: "relative selector ancestor traversal",
                css: ".scope:has(.subject[data-state=on]){width:89px}",
                target: "subject",
                attribute: "data-state",
                new_value: "on",
                expectation: ConservativeFallback,
            },
            RetainedDifferentialCase {
                name: "nth of selector structural traversal",
                css: ".subject:nth-child(1 of [data-state=on]){width:97px}",
                target: "subject",
                attribute: "data-state",
                new_value: "on",
                expectation: ConservativeFallback,
            },
            RetainedDifferentialCase {
                name: "unreferenced aria mutation",
                css: ".subject{width:101px;height:27px}",
                target: "subject",
                attribute: "aria-expanded",
                new_value: "true",
                expectation: ReuseAll,
            },
            RetainedDifferentialCase {
                name: "unreferenced ordinary autocomplete mutation",
                css: ".subject{width:103px;height:27px}",
                target: "subject",
                attribute: "autocomplete",
                new_value: "off",
                expectation: ReuseAll,
            },
            RetainedDifferentialCase {
                name: "ordinary autocomplete attribute selector",
                css: ".subject[autocomplete=off]{width:107px;height:29px}",
                target: "subject",
                attribute: "autocomplete",
                new_value: "off",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "ordinary autocomplete declaration attr dependency",
                css: ".subject::before{content:attr(autocomplete);display:block;width:9px}",
                target: "subject",
                attribute: "autocomplete",
                new_value: "off",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "mapped width presentation hint",
                css: ".subject{height:31px}",
                target: "subject",
                attribute: "width",
                new_value: "109",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "native input size mutation",
                css: "#control{height:23px}",
                target: "control",
                attribute: "size",
                new_value: "40",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "image resource mutation",
                css: "#image{width:17px;height:19px}",
                target: "image",
                attribute: "src",
                new_value: "replacement.png",
                expectation: ConservativeFallback,
            },
            RetainedDifferentialCase {
                name: "inherited language semantic mutation",
                css: ".scope{width:113px}",
                target: "scope",
                attribute: "lang",
                new_value: "fr",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "inherited direction semantic mutation",
                css: ".scope{width:127px}",
                target: "scope",
                attribute: "dir",
                new_value: "rtl",
                expectation: Incremental,
            },
            RetainedDifferentialCase {
                name: "functional language selector",
                css: ".scope:lang(fr){width:131px}",
                target: "scope",
                attribute: "lang",
                new_value: "fr",
                expectation: ConservativeFallback,
            },
            RetainedDifferentialCase {
                name: "functional direction selector",
                css: ".scope:dir(rtl){width:137px}",
                target: "scope",
                attribute: "dir",
                new_value: "rtl",
                expectation: ConservativeFallback,
            },
        ];

        for case in &cases {
            run_retained_differential_case(case);
        }
    }

    #[test]
    fn retained_unreferenced_ordinary_attribute_keeps_large_tree_clean() {
        let mut peers = String::new();
        for index in 0..2_000 {
            peers.push_str(&format!("<span class=peer data-index={index}></span>"));
        }
        let tree = parse_html(&format!(
            r#"<!doctype html><style>.subject{{width:31px;height:7px}}.peer{{height:1px}}</style>
               <main><input id=subject class=subject>{peers}</main>"#,
        ));
        let subject = tree.get_element_by_id("subject").unwrap();
        let mut cache = crate::css::StylesheetCache::default();
        let mut initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.with_node_mut(subject, |node| {
            node.set_attribute("autocomplete", "off".into())
        });
        let retained = RetainedStyleMaps {
            styles: std::mem::take(&mut initial.styles),
            custom_properties: std::mem::take(&mut initial.custom_properties),
        };
        let (incremental, telemetry) = layout_dom_with_web_fonts_pass_limit(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            None,
            Some(&mut cache),
            Some(retained),
            &[AttributeStyleMutation {
                node: subject,
                name: "autocomplete".into(),
                old_value: None,
                new_value: Some("off".into()),
            }
            .into()],
        );
        let full = layout_dom(&tree, (800.0, 600.0));

        assert_computed_styles_match("ordinary attribute 2k tree", &incremental, &full);
        assert_eq!(incremental.rects, full.rects);
        assert_eq!(telemetry.retained_fallback, 0, "{telemetry:?}");
        assert_eq!(telemetry.retained_fresh, 0, "{telemetry:?}");
        assert!(telemetry.retained_reused >= 2_000, "{telemetry:?}");
    }

    fn finish_retained_tree_case(
        name: &str,
        tree: &DomTree,
        cache: &mut crate::css::StylesheetCache,
        initial: DomLayout,
        mutation: TreeStyleMutation,
        expectation: RetainedDifferentialExpectation,
    ) -> DomLayout {
        finish_retained_tree_batch_case(
            name,
            tree,
            cache,
            initial,
            &[mutation],
            expectation,
        )
    }

    fn finish_retained_tree_batch_case(
        name: &str,
        tree: &DomTree,
        cache: &mut crate::css::StylesheetCache,
        mut initial: DomLayout,
        mutations: &[TreeStyleMutation],
        expectation: RetainedDifferentialExpectation,
    ) -> DomLayout {
        let retained = RetainedStyleMaps {
            styles: std::mem::take(&mut initial.styles),
            custom_properties: std::mem::take(&mut initial.custom_properties),
        };
        let mutations = mutations
            .iter()
            .cloned()
            .map(RetainedStyleMutation::from)
            .collect::<Vec<_>>();
        let (incremental, telemetry) = layout_dom_with_web_fonts_pass_limit(
            tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            None,
            Some(cache),
            Some(retained),
            &mutations,
        );
        let full = layout_dom(tree, (360.0, 260.0));
        match expectation {
            RetainedDifferentialExpectation::Incremental => {
                assert_eq!(telemetry.retained_fallback, 0, "{name}");
                assert!(telemetry.retained_fresh > 0, "{name}");
                assert!(telemetry.retained_reused > 0, "{name}");
            }
            RetainedDifferentialExpectation::ReuseAll => {
                assert_eq!(telemetry.retained_fallback, 0, "{name}");
                assert_eq!(telemetry.retained_fresh, 0, "{name}");
                assert!(telemetry.retained_reused > 0, "{name}");
            }
            RetainedDifferentialExpectation::ConservativeFallback => {
                assert_eq!(telemetry.retained_fallback, 1, "{name}");
                assert_eq!(telemetry.retained_reused, 0, "{name}");
            }
        }
        assert_computed_styles_match(name, &incremental, &full);
        assert_eq!(incremental.rects, full.rects, "{name}: border boxes");
        assert_eq!(
            incremental.inline_fragments, full.inline_fragments,
            "{name}: inline fragments"
        );
        assert_eq!(incremental.text_runs, full.text_runs, "{name}: text runs");
        assert_eq!(
            incremental.custom_properties, full.custom_properties,
            "{name}: custom properties"
        );
        incremental
    }

    #[test]
    fn retained_tree_styles_match_forced_full_mutation_matrix() {
        use RetainedDifferentialExpectation::{ConservativeFallback, Incremental};

        // A fresh insertion must cascade the inserted subtree and every
        // structurally affected sibling while retaining an unrelated branch.
        let tree = parse_html(
            r#"<style>
                .list{counter-reset:item}.item{counter-increment:item;height:9px}
                .item::before{content:counter(item)}
                .item:nth-child(2){width:83px}.item:first-child{color:#123456}
                .list:empty + .after{height:77px}.clean{width:91px;height:13px}
            </style><main><section id="list" class="list">
                <i id="first" class="item"></i><i id="insert" class="item"></i><i id="last" class="item"></i>
            </section><div class="after"></div><aside class="clean"></aside></main>"#,
        );
        let list = tree.get_element_by_id("list").unwrap();
        let insert = tree.get_element_by_id("insert").unwrap();
        let last = tree.get_element_by_id("last").unwrap();
        tree.remove_child(insert);
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.insert_before(last, insert);
        finish_retained_tree_case(
            "fresh insert with nth, counters, and pseudos",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Insert {
                node: insert,
                old_parent: None,
                new_parent: list,
            },
            Incremental,
        );

        // Removing a subtree must purge its now-detached style/custom-property
        // entries instead of growing the retained maps forever.
        let tree = parse_html(
            r#"<style>.row:nth-child(even){width:72px}.row::before{content:'x'}.clean{height:15px}</style>
               <main id="rows"><div class="row"></div><div id="removed" class="row"><b id="removed-child"></b></div><div class="row"></div></main><aside class="clean"></aside>"#,
        );
        let rows = tree.get_element_by_id("rows").unwrap();
        let removed = tree.get_element_by_id("removed").unwrap();
        let removed_child = tree.get_element_by_id("removed-child").unwrap();
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.remove_child(removed);
        let incremental = finish_retained_tree_case(
            "remove and purge detached subtree",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Remove {
                node: removed,
                old_parent: rows,
            },
            Incremental,
        );
        assert!(!incremental.styles.contains_key(&removed));
        assert!(!incremental.styles.contains_key(&removed_child));
        assert!(!incremental.custom_properties.contains_key(&removed));
        assert!(!incremental.custom_properties.contains_key(&removed_child));

        // Character-data changes keep selector matching local but must rebuild
        // text shaping and parent geometry.
        let tree = parse_html(
            r#"<style>#label{display:block;width:120px}.clean{height:17px}</style><div id="label">a</div><aside class="clean"></aside>"#,
        );
        let label = tree.get_element_by_id("label").unwrap();
        let text = tree.children(label)[0];
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.with_node_mut(text, |node| {
            if let obscura_dom::tree::NodeData::Text { contents } = &mut node.data {
                *contents = "a substantially longer replacement".into();
            }
        });
        finish_retained_tree_case(
            "text replacement",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Text {
                node: text,
                parent: Some(label),
            },
            Incremental,
        );

        // Keyed :has() rules do not poison unrelated mutations. A matching
        // insertion walks upward to the keyed anchor and retains clean peers.
        for (name, inserted_class, expectation) in [
            ("unrelated keyed has insertion", "other", Incremental),
            ("matching keyed has insertion", "signal", Incremental),
        ] {
            let html = format!(
                r#"<style>.host:has(.signal) .dependent{{width:101px}}.clean{{height:19px}}</style>
                   <section id="host" class="host"><div id="target"><i id="insert" class="{inserted_class}"></i></div><span class="dependent"></span></section><aside class="clean"></aside>"#
            );
            let tree = parse_html(&html);
            let target = tree.get_element_by_id("target").unwrap();
            let insert = tree.get_element_by_id("insert").unwrap();
            tree.remove_child(insert);
            let mut cache = crate::css::StylesheetCache::default();
            let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
                &tree,
                (360.0, 260.0),
                &HashMap::new(),
                &[],
                &mut cache,
            );
            tree.append_child(target, insert);
            finish_retained_tree_case(
                name,
                &tree,
                &mut cache,
                initial,
                TreeStyleMutation::Insert {
                    node: insert,
                    old_parent: None,
                    new_parent: target,
                },
                expectation,
            );
        }

        for (name, inserted_tag, expectation) in [
            ("unrelated keyed tag has insertion", "i", Incremental),
            ("matching keyed tag has insertion", "span", Incremental),
        ] {
            let html = format!(
                r#"<style>.host:has(> span) .dependent{{width:103px}}.clean{{height:21px}}</style>
                   <section id="host" class="host"><{inserted_tag} id="insert"></{inserted_tag}><div class="dependent"></div></section><aside class="clean"></aside>"#
            );
            let tree = parse_html(&html);
            let target = tree.get_element_by_id("host").unwrap();
            let insert = tree.get_element_by_id("insert").unwrap();
            tree.remove_child(insert);
            let mut cache = crate::css::StylesheetCache::default();
            let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
                &tree,
                (360.0, 260.0),
                &HashMap::new(),
                &[],
                &mut cache,
            );
            tree.append_child(target, insert);
            finish_retained_tree_case(
                name,
                &tree,
                &mut cache,
                initial,
                TreeStyleMutation::Insert {
                    node: insert,
                    old_parent: None,
                    new_parent: target,
                },
                expectation,
            );
        }

        // Style text changes alter the cache key and must never reuse styles.
        let tree = parse_html(
            r#"<style id="sheet">.subject{width:20px}</style><div class="subject"></div>"#,
        );
        let style = tree.get_element_by_id("sheet").unwrap();
        let text = tree.children(style)[0];
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.with_node_mut(text, |node| {
            if let obscura_dom::tree::NodeData::Text { contents } = &mut node.data {
                *contents = ".subject{width:140px}".into();
            }
        });
        finish_retained_tree_case(
            "style text fallback",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Text {
                node: text,
                parent: Some(style),
            },
            ConservativeFallback,
        );
    }

    #[test]
    fn retained_relational_styles_match_forced_full_direction_matrix() {
        use RetainedDifferentialExpectation::{ConservativeFallback, Incremental};

        // Descendant/child traversal and selector-list pseudos all invalidate
        // the keyed anchor without cascading an unrelated sibling branch.
        for (name, relative) in [
            ("has descendant insertion", ".signal"),
            ("has child insertion", "> .signal"),
            ("has is insertion", ":is(.signal,.alternate)"),
            ("has where insertion", ":where(.signal,.alternate)"),
        ] {
            let tree = parse_html(&format!(
                r#"<style>.host:has({relative}) .dependent{{width:111px;height:7px}}.clean{{height:9px}}</style>
                   <main><section id=host class=host><div id=target><i id=signal class=signal></i></div><span class=dependent></span></section><aside class=clean></aside></main>"#,
            ));
            let target = tree.get_element_by_id("target").unwrap();
            let signal = tree.get_element_by_id("signal").unwrap();
            tree.remove_child(signal);
            let mut cache = crate::css::StylesheetCache::default();
            let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
                &tree,
                (360.0, 260.0),
                &HashMap::new(),
                &[],
                &mut cache,
            );
            tree.append_child(target, signal);
            finish_retained_tree_case(
                name,
                &tree,
                &mut cache,
                initial,
                TreeStyleMutation::Insert {
                    node: signal,
                    old_parent: None,
                    new_parent: target,
                },
                Incremental,
            );
        }

        // A relative sibling combinator searches toward earlier siblings of
        // the changed boundary. Both exact and general sibling paths retain a
        // clean branch.
        for (name, combinator) in [
            ("has adjacent sibling insertion", "+"),
            ("has general sibling insertion", "~"),
        ] {
            let tree = parse_html(&format!(
                r#"<style>.host:has({combinator} .signal){{width:113px;height:11px}}.clean{{height:13px}}</style>
                   <main id=parent><section id=host class=host></section><i id=signal class=signal></i><aside id=clean class=clean></aside></main>"#,
            ));
            let parent = tree.get_element_by_id("parent").unwrap();
            let signal = tree.get_element_by_id("signal").unwrap();
            let clean = tree.get_element_by_id("clean").unwrap();
            tree.remove_child(signal);
            let mut cache = crate::css::StylesheetCache::default();
            let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
                &tree,
                (360.0, 260.0),
                &HashMap::new(),
                &[],
                &mut cache,
            );
            tree.insert_before(clean, signal);
            finish_retained_tree_case(
                name,
                &tree,
                &mut cache,
                initial,
                TreeStyleMutation::Insert {
                    node: signal,
                    old_parent: None,
                    new_parent: parent,
                },
                Incremental,
            );
        }

        // Inserting an unrelated node can break `+`, even though the inserted
        // subtree has none of the selector's indexed keys.
        let tree = parse_html(
            r#"<style>.host:has(> .left + .right){width:127px}.clean{height:15px}</style>
               <main><section id=host class=host><i class=left></i><b id=blocker></b><i id=right class=right></i></section><aside class=clean></aside></main>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let blocker = tree.get_element_by_id("blocker").unwrap();
        let right = tree.get_element_by_id("right").unwrap();
        tree.remove_child(blocker);
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.insert_before(right, blocker);
        finish_retained_tree_case(
            "has adjacent insertion side effect",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Insert {
                node: blocker,
                old_parent: None,
                new_parent: host,
            },
            Incremental,
        );

        // Structural matching changes even when the inserted node does not
        // carry the keyed relative subject class.
        let tree = parse_html(
            r#"<style>.host:has(> .signal:first-child){width:131px}.clean{height:17px}</style>
               <main><section id=host class=host><b id=blocker></b><i id=signal class=signal></i></section><aside class=clean></aside></main>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let blocker = tree.get_element_by_id("blocker").unwrap();
        let signal = tree.get_element_by_id("signal").unwrap();
        tree.remove_child(blocker);
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.insert_before(signal, blocker);
        finish_retained_tree_case(
            "has structural insertion side effect",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Insert {
                node: blocker,
                old_parent: None,
                new_parent: host,
            },
            Incremental,
        );

        // Character data can toggle :empty without changing any indexed key.
        let tree = parse_html(
            r#"<style>.host:has(.label:empty){width:133px}.clean{height:18px}</style>
               <main><section id=host class=host><span id=label class=label>x</span></section><aside class=clean></aside></main>"#,
        );
        let label = tree.get_element_by_id("label").unwrap();
        let text = tree.children(label)[0];
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.with_node_mut(text, |node| {
            if let obscura_dom::tree::NodeData::Text { contents } = &mut node.data {
                contents.clear();
            }
        });
        finish_retained_tree_case(
            "has empty text side effect",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Text {
                node: text,
                parent: Some(label),
            },
            Incremental,
        );

        // The old sibling links are gone by the time removal invalidation
        // runs. Broadening each old ancestor boundary must cover both a keyed
        // removal and an adjacency match created by removing an interloper.
        for (name, css, removed_id) in [
            (
                "has descendant removal",
                ".host:has(.signal){width:137px}",
                "signal",
            ),
            (
                "has adjacent removal side effect",
                ".host:has(> .left + .right){width:139px}",
                "blocker",
            ),
        ] {
            let tree = parse_html(&format!(
                r#"<style>{css}.clean{{height:19px}}</style><main><section id=host class=host><i class=left></i><b id=blocker></b><i class=right></i><i id=signal class=signal></i></section><aside class=clean></aside></main>"#,
            ));
            let host = tree.get_element_by_id("host").unwrap();
            let removed = tree.get_element_by_id(removed_id).unwrap();
            let mut cache = crate::css::StylesheetCache::default();
            let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
                &tree,
                (360.0, 260.0),
                &HashMap::new(),
                &[],
                &mut cache,
            );
            tree.remove_child(removed);
            finish_retained_tree_case(
                name,
                &tree,
                &mut cache,
                initial,
                TreeStyleMutation::Remove {
                    node: removed,
                    old_parent: host,
                },
                Incremental,
            );
        }

        // Detached nodes are mutable before the queued render flush. Removal
        // invalidation must not depend on selector keys still being present in
        // the post-mutation subtree.
        let tree = parse_html(
            r#"<style>.host:has(.signal){width:141px}.clean{height:20px}</style><main><section id=host class=host><i id=signal class=signal></i></section><aside class=clean></aside></main>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let signal = tree.get_element_by_id("signal").unwrap();
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.remove_child(signal);
        tree.with_node_mut(signal, |node| {
            node.set_attribute("class", "detached-and-renamed".into())
        });
        finish_retained_tree_case(
            "has removal after detached key mutation",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Remove {
                node: signal,
                old_parent: host,
            },
            Incremental,
        );

        // Reparenting changes anchors in both old and new ancestor chains.
        let tree = parse_html(
            r#"<style>.host:has(> .signal){width:149px}.clean{height:21px}</style>
               <main><section id=old class=host><i id=signal class=signal></i></section><section id=new class=host></section><aside class=clean></aside></main>"#,
        );
        let old = tree.get_element_by_id("old").unwrap();
        let new = tree.get_element_by_id("new").unwrap();
        let signal = tree.get_element_by_id("signal").unwrap();
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.append_child(new, signal);
        finish_retained_tree_case(
            "has reparent old and new anchors",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Insert {
                node: signal,
                old_parent: Some(old),
                new_parent: new,
            },
            Incremental,
        );

        // A sibling-then-descendant path outside the anchor is deliberately
        // left on the correctness-first full path until dependency chains can
        // encode the continuation explicitly.
        let tree = parse_html(
            r#"<style>.host:has(.signal)~.panel .leaf{width:151px}</style><main><section id=host class=host><i id=signal class=signal></i></section><div class=panel><b class=leaf></b></div></main>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let signal = tree.get_element_by_id("signal").unwrap();
        tree.remove_child(signal);
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.append_child(host, signal);
        finish_retained_tree_case(
            "has unrepresentable outer mixed traversal",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Insert {
                node: signal,
                old_parent: None,
                new_parent: host,
            },
            ConservativeFallback,
        );
    }

    #[test]
    fn retained_mixed_outer_has_dependencies_match_forced_full() {
        use RetainedDifferentialExpectation::Incremental;

        // `:has()` owns its relative path, but structural state on the anchor
        // remains an ordinary tree dependency and must not be discarded just
        // because both occur in the same compiled rule.
        let tree = parse_html(
            r#"<style>.host:has(.signal):first-child .desc{width:173px}.clean{height:7px}</style>
               <main id=parent><b id=blocker></b><section id=host class=host><i class=signal></i><span class=desc></span></section><aside class=clean></aside></main>"#,
        );
        let parent = tree.get_element_by_id("parent").unwrap();
        let blocker = tree.get_element_by_id("blocker").unwrap();
        let host = tree.get_element_by_id("host").unwrap();
        tree.remove_child(blocker);
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.insert_before(host, blocker);
        finish_retained_tree_case(
            "outer structural state beside has",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Insert {
                node: blocker,
                old_parent: None,
                new_parent: parent,
            },
            Incremental,
        );

        // A freshly inserted anchor at position zero can create both adjacent
        // and general sibling matches outside `:has()`.
        let tree = parse_html(
            r#"<style>
                 .left:has(.signal) + .adjacent{width:179px}
                 .left:has(.signal) ~ .following{height:31px}
                 .clean{height:9px}
               </style><main id=parent><section id=left class=left><i class=signal></i></section><b id=adjacent class=adjacent></b><b class=following></b><aside class=clean></aside></main>"#,
        );
        let parent = tree.get_element_by_id("parent").unwrap();
        let left = tree.get_element_by_id("left").unwrap();
        let adjacent = tree.get_element_by_id("adjacent").unwrap();
        tree.remove_child(left);
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.insert_before(adjacent, left);
        finish_retained_tree_case(
            "outer sibling paths beside has",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Insert {
                node: left,
                old_parent: None,
                new_parent: parent,
            },
            Incremental,
        );

        // Text can toggle an outer :empty while the relative `:has()` path is
        // entirely sibling-based and therefore has no text side effect.
        let tree = parse_html(
            r#"<style>.host:has(~ .peer):empty ~ .panel{width:181px}.clean{height:11px}</style>
               <main><section id=host class=host></section><i class=peer></i><b class=panel></b><aside class=clean></aside></main>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let text = tree.new_node(obscura_dom::tree::NodeData::Text {
            contents: String::new(),
        });
        tree.append_child(host, text);
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.with_node_mut(text, |node| {
            if let obscura_dom::tree::NodeData::Text { contents } = &mut node.data {
                *contents = "now non-empty".into();
            }
        });
        finish_retained_tree_case(
            "outer empty state beside has",
            &tree,
            &mut cache,
            initial,
            TreeStyleMutation::Text {
                node: text,
                parent: Some(host),
            },
            Incremental,
        );
    }

    #[test]
    fn retained_batched_insertions_restore_old_structural_boundaries() {
        use RetainedDifferentialExpectation::Incremental;

        let tree = parse_html(
            r#"<style>
                 .item:first-child{width:191px}.item:last-child{height:37px}
                 .item:only-child{color:#123456}
                 i:first-of-type{margin-left:13px}i:last-of-type{margin-right:17px}
                 i:only-of-type{padding-top:19px}.clean{height:13px}
               </style><main><section id=list><i id=before-a class=item></i><i id=before-b class=item></i><i id=old class=item></i><i id=after-a class=item></i><i id=after-b class=item></i></section><aside class=clean></aside></main>"#,
        );
        let list = tree.get_element_by_id("list").unwrap();
        let old = tree.get_element_by_id("old").unwrap();
        let before_a = tree.get_element_by_id("before-a").unwrap();
        let before_b = tree.get_element_by_id("before-b").unwrap();
        let after_a = tree.get_element_by_id("after-a").unwrap();
        let after_b = tree.get_element_by_id("after-b").unwrap();
        for inserted in [before_a, before_b, after_a, after_b] {
            tree.remove_child(inserted);
        }
        let mut cache = crate::css::StylesheetCache::default();
        let initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (360.0, 260.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.insert_before(old, before_a);
        tree.insert_before(old, before_b);
        tree.append_child(list, after_a);
        tree.append_child(list, after_b);
        let mutations = [before_a, before_b, after_a, after_b].map(|node| {
            TreeStyleMutation::Insert {
                node,
                old_parent: None,
                new_parent: list,
            }
        });
        finish_retained_tree_batch_case(
            "batched first last only boundaries",
            &tree,
            &mut cache,
            initial,
            &mutations,
            Incremental,
        );
    }

    #[test]
    fn retained_relational_invalidation_keeps_two_thousand_node_peer_clean() {
        let mut clean = String::new();
        for index in 0..2_000 {
            clean.push_str(&format!("<span class=clean data-index={index}></span>"));
        }
        let tree = parse_html(&format!(
            r#"<style>.host:has(> .signal) .dependent{{width:157px}}.clean{{height:1px}}</style>
               <main><section id=host class=host><i id=signal class=signal></i><b class=dependent></b></section><aside>{clean}</aside></main>"#,
        ));
        let host = tree.get_element_by_id("host").unwrap();
        let signal = tree.get_element_by_id("signal").unwrap();
        tree.remove_child(signal);
        let mut cache = crate::css::StylesheetCache::default();
        let mut initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.append_child(host, signal);
        let retained = RetainedStyleMaps {
            styles: std::mem::take(&mut initial.styles),
            custom_properties: std::mem::take(&mut initial.custom_properties),
        };
        let (incremental, telemetry) = layout_dom_with_web_fonts_pass_limit(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            None,
            Some(&mut cache),
            Some(retained),
            &[TreeStyleMutation::Insert {
                node: signal,
                old_parent: None,
                new_parent: host,
            }
            .into()],
        );
        let full = layout_dom(&tree, (800.0, 600.0));
        assert_computed_styles_match("relational 2k clean peer", &incremental, &full);
        assert_eq!(incremental.rects, full.rects);
        assert_eq!(telemetry.retained_fallback, 0, "{telemetry:?}");
        assert!(telemetry.retained_reused >= 2_000, "{telemetry:?}");
        assert!(telemetry.retained_fresh < 24, "{telemetry:?}");
    }

    #[test]
    fn retained_tree_invalidation_keeps_large_unrelated_branch_clean() {
        let mut clean = String::new();
        for index in 0..2_000 {
            clean.push_str(&format!("<span class=clean data-index={index}></span>"));
        }
        let tree = parse_html(&format!(
            r#"<style>.item:nth-child(2){{width:88px}}.clean{{height:1px}}</style>
               <main><section id=list><i class=item></i><i id=insert class=item></i></section><aside>{clean}</aside></main>"#
        ));
        let list = tree.get_element_by_id("list").unwrap();
        let insert = tree.get_element_by_id("insert").unwrap();
        tree.remove_child(insert);
        let mut cache = crate::css::StylesheetCache::default();
        let mut initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.append_child(list, insert);
        let retained = RetainedStyleMaps {
            styles: std::mem::take(&mut initial.styles),
            custom_properties: std::mem::take(&mut initial.custom_properties),
        };
        let (incremental, telemetry) = layout_dom_with_web_fonts_pass_limit(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            None,
            Some(&mut cache),
            Some(retained),
            &[TreeStyleMutation::Insert {
                node: insert,
                old_parent: None,
                new_parent: list,
            }
            .into()],
        );
        let full = layout_dom(&tree, (800.0, 600.0));
        assert_computed_styles_match("large unrelated branch", &incremental, &full);
        assert_eq!(incremental.rects, full.rects);
        assert_eq!(telemetry.retained_fallback, 0);
        assert!(telemetry.retained_reused >= 2_000, "{telemetry:?}");
        assert!(telemetry.retained_fresh < 16, "{telemetry:?}");
    }

    #[test]
    fn retained_keyed_start_insertion_keeps_large_body_branch_clean() {
        let mut clean = String::new();
        for index in 0..2_000 {
            clean.push_str(&format!("<span class=clean data-index={index}></span>"));
        }
        let tree = parse_html(&format!(
            r#"<!doctype html><html><head><style>
                   .item:nth-child(2){{width:31px}}
                   .left + .right{{height:7px}}
                   .early ~ .late{{height:9px}}
                   .maybe-empty:empty + .after{{height:11px}}
                   .clean{{height:1px}}
               </style></head><body><main id=stable>{clean}</main><i id=watcher data-scroll-watcher></i></body></html>"#,
        ));
        let body = tree.query_selector("body").unwrap().unwrap();
        let stable = tree.get_element_by_id("stable").unwrap();
        let watcher = tree.get_element_by_id("watcher").unwrap();
        tree.remove_child(watcher);
        let mut cache = crate::css::StylesheetCache::default();
        let mut initial = layout_dom_with_web_fonts_and_stylesheet_cache(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            &mut cache,
        );
        tree.insert_before(stable, watcher);
        let retained = RetainedStyleMaps {
            styles: std::mem::take(&mut initial.styles),
            custom_properties: std::mem::take(&mut initial.custom_properties),
        };
        let (incremental, telemetry) = layout_dom_with_web_fonts_pass_limit(
            &tree,
            (800.0, 600.0),
            &HashMap::new(),
            &[],
            None,
            Some(&mut cache),
            Some(retained),
            &[TreeStyleMutation::Insert {
                node: watcher,
                old_parent: None,
                new_parent: body,
            }
            .into()],
        );
        let full = layout_dom(&tree, (800.0, 600.0));
        assert_computed_styles_match("keyed start insertion large body branch", &incremental, &full);
        assert_eq!(incremental.rects, full.rects);
        assert_eq!(telemetry.retained_fallback, 0, "{telemetry:?}");
        assert!(telemetry.retained_reused >= 2_000, "{telemetry:?}");
        assert!(telemetry.retained_fresh < 16, "{telemetry:?}");
    }

    #[test]
    fn container_convergence_classifies_self_consistent_stability() {
        assert_eq!(
            container_iteration_termination(true, &3, Some(&2)),
            Some(ContainerLayoutTermination::GeometryStable)
        );
        assert_eq!(
            container_iteration_termination(false, &3, Some(&3)),
            Some(ContainerLayoutTermination::SignatureStable)
        );
        assert_eq!(container_iteration_termination(false, &3, Some(&2)), None);
        assert_eq!(CONTAINER_LAYOUT_SAFETY_LIMIT, 512);
    }

    #[test]
    fn declarative_shadow_style_does_not_leak_into_document_cascade() {
        let tree = parse_html(
            r#"<style>
                 html, body { margin:0 }
                 .button { width:123px; height:20px }
               </style>
               <x-button>
                 <template shadowrootmode="open">
                   <style>.button { width:100% }</style>
                   <span class="button">shadow control</span>
                 </template>
               </x-button>
               <div id="light-button" class="button"></div>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let button = tree.get_element_by_id("light-button").unwrap();
        let rect = laid.rects[&button];

        assert!((rect.width - 123.0).abs() < 0.1, "{rect:?}");
        assert!((rect.height - 20.0).abs() < 0.1, "{rect:?}");
    }

    #[test]
    fn multicol_balances_atomic_blocks_like_chromium() {
        // Chromium 140 geometry for this reduction is a 630x300 container,
        // 190px columns at x=0/220/440, and a 2/3/3 source-order partition.
        // Explicit heights keep this a layout-algorithm regression rather
        // than a font/raster comparison.
        let tree = parse_html(
            "<html><head><style>#columns{columns:1}@media (width >= 700px){#columns{columns:3}}</style></head>\
             <body style=\"margin:0\"><div id=\"columns\" style=\"column-gap:30px;width:630px\">\
             <div id=\"a\" style=\"height:180px;break-inside:avoid\"></div>\
             <div id=\"b\" style=\"height:120px;break-inside:avoid\"></div>\
             <div id=\"c\" style=\"height:60px;break-inside:avoid\"></div>\
             <div id=\"d\" style=\"height:150px;break-inside:avoid\"></div>\
             <div id=\"e\" style=\"height:90px;break-inside:avoid\"></div>\
             <div id=\"f\" style=\"height:120px;break-inside:avoid\"></div>\
             <div id=\"g\" style=\"height:80px;break-inside:avoid\"></div>\
             <div id=\"h\" style=\"height:100px;break-inside:avoid\"></div>\
             </div></body></html>",
        );
        let laid = layout_dom(&tree, (800.0, 600.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        let columns = rect("columns");
        assert!((columns.width - 630.0).abs() < 0.1, "{columns:?}");
        assert!((columns.height - 300.0).abs() < 0.1, "{columns:?}");

        let expected = [
            ("a", 0.0, 0.0, 190.0, 180.0),
            ("b", 0.0, 180.0, 190.0, 120.0),
            ("c", 220.0, 0.0, 190.0, 60.0),
            ("d", 220.0, 60.0, 190.0, 150.0),
            ("e", 220.0, 210.0, 190.0, 90.0),
            ("f", 440.0, 0.0, 190.0, 120.0),
            ("g", 440.0, 120.0, 190.0, 80.0),
            ("h", 440.0, 200.0, 190.0, 100.0),
        ];
        for (id, x, y, width, height) in expected {
            let child = rect(id);
            assert!((child.x - columns.x - x).abs() < 0.1, "{id}: {child:?}");
            assert!((child.y - columns.y - y).abs() < 0.1, "{id}: {child:?}");
            assert!((child.width - width).abs() < 0.1, "{id}: {child:?}");
            assert!((child.height - height).abs() < 0.1, "{id}: {child:?}");
        }

        let narrow = layout_dom(&tree, (600.0, 1000.0));
        let columns_id = tree.get_element_by_id("columns").unwrap();
        let c_id = tree.get_element_by_id("c").unwrap();
        assert_eq!(narrow.styles[&columns_id].column_count, Some(1));
        assert!(
            (narrow.rects[&columns_id].height - 900.0).abs() < 0.1,
            "{:?}",
            narrow.rects[&columns_id]
        );
        assert!(
            (narrow.rects[&c_id].y - narrow.rects[&columns_id].y - 300.0).abs() < 0.1,
            "{:?}",
            narrow.rects[&c_id]
        );
    }

    #[test]
    fn static_positions_use_final_post_repair_geometry_without_phantom_overflow() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #columns{columns:3;width:630px;column-gap:30px}
                #columns > div{height:100px;break-inside:avoid}
                #holder{height:20px}
                #outer{position:absolute;width:40px;height:40px}
                #wrapper{margin-top:10px;height:20px}
                #nested{position:absolute;width:5px;height:5px}
            </style>
            <div id="columns">
              <div></div><div></div><div></div>
              <div></div><div></div><div></div>
            </div>
            <div id="holder">
              <div id="outer"><div id="wrapper"><div id="nested"></div></div></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (800.0, 400.0));
        let rect = |name| laid.rects[&tree.get_element_by_id(name).unwrap()];
        let columns = rect("columns");
        let holder = rect("holder");
        let outer = rect("outer");
        let wrapper = rect("wrapper");
        let nested = rect("nested");

        assert!((columns.height - 200.0).abs() < 0.01, "{columns:?}");
        assert!((holder.y - 200.0).abs() < 0.01, "{holder:?}");
        assert!(
            (outer.y - holder.y).abs() < 0.01,
            "outer static position was harvested before multicol repair: \
             holder={holder:?} outer={outer:?}"
        );
        assert!(
            (nested.y - wrapper.y).abs() < 0.01,
            "nested static-position candidate changed while reparenting its ancestor: \
             wrapper={wrapper:?} nested={nested:?}"
        );
        assert_eq!(
            laid.scrolling_content_size(&tree, (800.0, 400.0)),
            (800.0, 400.0),
            "a stale preliminary static position must not create scrolling overflow"
        );
    }

    #[test]
    fn multicol_does_not_atomize_breakable_prose_boxes() {
        let tree = parse_html(
            "<html><body style=\"margin:0\"><main id=\"columns\" style=\"columns:2;width:200px\">\
             <p id=\"first\" style=\"height:100px;margin:0\"></p>\
             <p id=\"second\" style=\"height:100px;margin:0\"></p>\
             </main></body></html>",
        );
        let laid = layout_dom(&tree, (400.0, 400.0));
        let columns = laid.rects[&tree.get_element_by_id("columns").unwrap()];
        let first = laid.rects[&tree.get_element_by_id("first").unwrap()];
        let second = laid.rects[&tree.get_element_by_id("second").unwrap()];
        assert!((columns.height - 200.0).abs() < 0.1, "{columns:?}");
        assert!((first.width - 200.0).abs() < 0.1, "{first:?}");
        assert!((second.x - first.x).abs() < 0.1, "{second:?}");
        assert!((second.y - first.y - 100.0).abs() < 0.1, "{second:?}");
    }

    #[test]
    fn direct_flex_text_is_one_wrapping_anonymous_item() {
        // Chromium 140: the pseudo is one flex item and the direct text run
        // is one anonymous flex item with a 276px inline formatting context.
        // The sentence wraps to three 20px lines; treating every word as an
        // outer flex item instead produces one overflowing 20px line.
        let tree = parse_html(
            "<style>\
             *{box-sizing:border-box}body{margin:0}\
             #quote{display:flex;gap:8px;width:300px;font:16px/20px 'Liberation Sans'}\
             #quote::before{content:'';display:block;width:16px;height:16px;flex-shrink:0}\
             </style>\
             <div id=\"quote\">This anonymous text item must wrap inside the remaining flex space instead of overflowing.</div>",
        );
        let laid = layout_dom(&tree, (500.0, 200.0));
        let quote = laid.rects[&tree.get_element_by_id("quote").unwrap()];
        assert!((quote.width - 300.0).abs() < 0.1, "{quote:?}");
        assert!((quote.height - 60.0).abs() < 0.1, "{quote:?}");
    }

    #[test]
    fn auto_width_column_flex_text_uses_fit_content_width() {
        // Chromium 145: the outer row leaves 533px beside the fixed 203px
        // sibling and 32px gap. The auto-width quote is fit-content, so its
        // direct anonymous text item reflows to five 24px lines. Measuring it
        // only at max-content instead produces a 2097px-wide, 24px-tall line.
        let tree = parse_html(
            r#"<style>
               * { box-sizing: border-box }
               body { margin: 0; font: 16px/24px "Liberation Sans" }
               #row { display: flex; gap: 32px; width: 768px }
               #column {
                 display: flex;
                 flex-direction: column;
                 align-items: flex-start;
               }
               #quote { display: flex; gap: 8px; margin: 0 }
               #quote::before {
                 content: "";
                 display: block;
                 flex-shrink: 0;
                 width: 16px;
                 height: 16px;
               }
               #logo { width: 203px; height: 128px; flex-shrink: 0 }
               </style>
               <section id="row">
                 <div id="column"><blockquote id="quote">MDN closely follows W3C standards which helps me keep up with important topics. It's a complete package as it caters to everything; complex APIs, new browser functionalities, and best practices. MDN serves as a truly valuable resource and continues to assist me in my everyday development.</blockquote></div>
                 <div id="logo"></div>
               </section>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let rect = |id: &str| -> Rect { laid.rects[&tree.get_element_by_id(id).unwrap()] };
        let row = rect("row");
        let column = rect("column");
        let quote = rect("quote");
        let logo = rect("logo");
        assert!((row.width - 768.0).abs() < 0.1, "{row:?}");
        assert!((row.height - 128.0).abs() < 0.1, "{row:?}");
        assert!((column.width - 533.0).abs() < 0.1, "{column:?}");
        assert!((quote.width - 533.0).abs() < 0.1, "{quote:?}");
        assert!((quote.height - 120.0).abs() < 0.1, "{quote:?}");
        assert!((logo.x - 565.0).abs() < 0.1, "{logo:?}");
        assert!((logo.width - 203.0).abs() < 0.1, "{logo:?}");
    }

    #[test]
    fn column_flex_fit_content_preserves_unbreakable_min_content() {
        // Chromium 145 keeps the unbreakable quote at 2561.25px and lets the
        // constrained row overflow. The synthetic fit-content cap must not
        // turn an intrinsic min-content floor into overflow-wrap:anywhere.
        let word = "X".repeat(240);
        let tree = parse_html(&format!(
            r#"<style>
               * {{ box-sizing: border-box }}
               body {{ margin: 0; font: 16px/24px "Liberation Sans" }}
               #row {{ display: flex; gap: 32px; width: 768px }}
               #column {{
                 display: flex;
                 flex-direction: column;
                 align-items: flex-start;
               }}
               #quote {{ display: flex; margin: 0 }}
               #logo {{ width: 203px; height: 128px; flex-shrink: 0 }}
               </style>
               <section id="row">
                 <div id="column"><blockquote id="quote">{word}</blockquote></div>
                 <div id="logo"></div>
               </section>"#
        ));
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let rect = |id: &str| -> Rect { laid.rects[&tree.get_element_by_id(id).unwrap()] };
        let column = rect("column");
        let quote = rect("quote");
        let logo = rect("logo");
        assert!(quote.width > 2500.0, "{quote:?}");
        assert!(
            (column.width - quote.width).abs() < 0.1,
            "{column:?} {quote:?}"
        );
        assert!((quote.height - 24.0).abs() < 0.1, "{quote:?}");
        assert!((logo.x - quote.width - 32.0).abs() < 0.1, "{logo:?}");
    }

    #[test]
    fn final_flex_reflow_resolves_nested_percentage_and_fit_content_widths() {
        // A modern application shell commonly makes its page a 100%-wide item
        // in an outer navigation row, then nests full-width sections, oversized
        // sliding strips, calc()-sized artwork, and shrink-wrapped controls.
        // Intrinsic flex measurement may treat those percentages as cyclic,
        // but the final item reflow has a definite 800px inline size.
        let tree = parse_html(
            r#"<style>
               html, body { margin:0; font-size:16px }
               * { box-sizing:border-box }
               #shell { display:flex; width:800px }
               #app { width:100% }
               #tabs { width:100%; overflow:hidden }
               #strip { width:400%; height:10px }
               #pattern { width:calc(100% - 7rem); height:10px }
               #pattern-inner { width:calc(100% - 1rem); height:10px }
               #banner-row { display:flex; width:100% }
               #banner {
                 display:flex; flex-wrap:wrap; gap:8px;
                 width:fit-content; max-width:100%;
                 padding:10px; border:1px solid
               }
               #banner-a { width:120px; height:20px; flex-shrink:0 }
               #banner-b { width:140px; height:20px; flex-shrink:0 }
               </style>
               <main id="shell"><div id="app">
                 <section id="tabs"><div id="strip"></div></section>
                 <div id="pattern"><div id="pattern-inner"></div></div>
                 <div id="banner-row"><a id="banner">
                   <span id="banner-a"></span><span id="banner-b"></span>
                 </a></div>
               </div></main>"#,
        );
        let laid = layout_dom(&tree, (1000.0, 300.0));
        let node = |id: &str| tree.get_element_by_id(id).unwrap();
        let rect = |id: &str| -> Rect { laid.rects[&node(id)] };

        assert!((rect("shell").width - 800.0).abs() < 0.01, "{:?}", rect("shell"));
        assert!((rect("app").width - 800.0).abs() < 0.01, "{:?}", rect("app"));
        assert!((rect("tabs").width - 800.0).abs() < 0.01, "{:?}", rect("tabs"));
        assert!((rect("strip").width - 3200.0).abs() < 0.01, "{:?}", rect("strip"));
        assert!((rect("pattern").width - 688.0).abs() < 0.01, "{:?}", rect("pattern"));
        assert!((rect("pattern-inner").width - 672.0).abs() < 0.01, "{:?}", rect("pattern-inner"));
        assert!((rect("banner-row").width - 800.0).abs() < 0.01, "{:?}", rect("banner-row"));
        assert!((rect("banner").width - 290.0).abs() < 0.01, "{:?}", rect("banner"));

        assert_eq!(
            laid.styles[&node("strip")].width,
            crate::Dimension::Percent(4.0),
            "plain percentages should remain typed through final layout"
        );
        assert_eq!(
            laid.styles[&node("banner")].max_width,
            crate::Dimension::Percent(1.0),
            "a fit-content maximum must not be frozen from intrinsic geometry"
        );
    }

    #[test]
    fn final_flex_reflow_finalizes_fit_content_before_descendant_calc() {
        let tree = parse_html(
            r#"<style>
               html, body { margin:0 }
               .shell { display:flex; width:800px }
               .app { width:100% }
               .wrap { width:fit-content }
               .sizer { width:300px; height:10px }
               .calc { width:calc(100% - 10px); height:10px }
               </style>
               <main id="shell" class="shell"><div id="app" class="app">
                 <div id="wrap" class="wrap">
                   <div class="sizer"></div><div id="calc" class="calc"></div>
                 </div>
               </div></main>"#,
        );
        let laid = layout_dom(&tree, (1000.0, 300.0));
        let node = |id: &str| tree.get_element_by_id(id).unwrap();
        let width = |id: &str| laid.rects[&node(id)].width;

        assert!((width("shell") - 800.0).abs() < 0.01, "{}", width("shell"));
        assert!((width("app") - 800.0).abs() < 0.01, "{}", width("app"));
        assert!((width("wrap") - 300.0).abs() < 0.01, "{}", width("wrap"));
        assert!((width("calc") - 290.0).abs() < 0.01, "{}", width("calc"));
    }

    #[test]
    fn empty_document_is_safe() {
        // html5ever always synthesizes html/head/body, so an empty document
        // still has a few element rects. The point is that it does not panic.
        let tree = parse_html("");
        let laid = layout_dom(&tree, (1280.0, 720.0));
        assert!(laid.rects.len() <= 4, "got {}", laid.rects.len());
    }

    #[test]
    fn author_cascade_honors_important_and_presentational_hint_order() {
        let tree = parse_html(
            r#"<style>
                #sheet { width: 320px !important; background: green !important }
                .case#sheet { width: 80px; background: red }
                .inline-normal { width: 320px !important; background: green !important }
                #inline-important { width: 80px !important; background: red !important }
                .custom { --w: 320px !important; --w: 80px; width: var(--w) }
                .hint { background: green }
            </style>
            <div id="sheet" class="case"></div>
            <div id="inline-normal" class="inline-normal" style="width:80px;background:red"></div>
            <div id="inline-important" style="width:320px!important;background:green!important"></div>
            <div id="inline-order" style="width:320px!important;width:80px;background:green!important;background:red"></div>
            <div id="custom" class="custom"></div>
            <div id="hint" class="hint" bgcolor="red"></div>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        for id in [
            "sheet",
            "inline-normal",
            "inline-important",
            "inline-order",
            "custom",
        ] {
            let nid = tree.query_selector(&format!("#{id}")).unwrap().unwrap();
            let style = laid.styles.get(&nid).unwrap();
            assert_eq!(
                style.width,
                crate::Dimension::Px(320.0),
                "wrong cascade width for {id}"
            );
        }
        for id in [
            "sheet",
            "inline-normal",
            "inline-important",
            "inline-order",
            "hint",
        ] {
            let nid = tree.query_selector(&format!("#{id}")).unwrap().unwrap();
            let style = laid.styles.get(&nid).unwrap();
            assert_eq!(
                style.background_color,
                Some([0, 128, 0, 255]),
                "wrong cascade color for {id}"
            );
        }
    }

    #[test]
    fn box_sizing_controls_the_declared_size_edge() {
        let tree = parse_html(
            r#"<style>
                body { margin: 0 }
                .box { width:100px; height:50px; padding:10px; border:2px solid black }
                .border { box-sizing:border-box }
                .parent { width:400px }
                .half { width:50%; padding:10px; border:2px solid black }
                .limited { width:200px; max-width:100px; padding:10px; border:2px solid black }
                .inherit-parent { box-sizing:border-box }
                .inherit-child { box-sizing:inherit; width:100px; padding:10px; border:2px solid black }
            </style>
            <div id="content" class="box"></div>
            <div id="border" class="box border"></div>
            <div class="parent"><div id="half-content" class="half"></div><div id="half-border" class="half border"></div></div>
            <div id="max-content" class="limited"></div>
            <div id="max-border" class="limited border"></div>
            <div class="inherit-parent"><div id="inherited-border" class="inherit-child"></div></div>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let size = |id: &str| {
            let nid = tree.query_selector(&format!("#{id}")).unwrap().unwrap();
            let rect = laid.rects.get(&nid).unwrap();
            (rect.width, rect.height)
        };
        assert_eq!(size("content"), (124.0, 74.0));
        assert_eq!(size("border"), (100.0, 50.0));
        assert_eq!(size("half-content").0, 224.0);
        assert_eq!(size("half-border").0, 200.0);
        assert_eq!(size("max-content").0, 124.0);
        assert_eq!(size("max-border").0, 100.0);
        assert_eq!(size("inherited-border").0, 100.0);
    }

    #[test]
    fn logical_sizes_control_final_horizontal_geometry() {
        let tree = parse_html(
            r#"<style>
                body { margin:0 }
                #logical-last {
                    width:20px; inline-size:120px;
                    height:10px; block-size:30px;
                }
                #physical-last {
                    inline-size:120px; width:40px;
                    block-size:30px; height:15px;
                }
                #bounded {
                    inline-size:calc(50vw - 10px);
                    block-size:80px;
                    min-inline-size:300px;
                    max-block-size:40px;
                }
            </style>
            <div id="logical-last"></div>
            <div id="physical-last"></div>
            <div id="bounded"></div>"#,
        );
        let laid = layout_dom(&tree, (400.0, 240.0));
        let size = |id: &str| {
            let nid = tree.query_selector(&format!("#{id}")).unwrap().unwrap();
            let rect = laid.rects.get(&nid).unwrap();
            (rect.width, rect.height)
        };
        assert_eq!(size("logical-last"), (120.0, 30.0));
        assert_eq!(size("physical-last"), (40.0, 15.0));
        assert_eq!(size("bounded"), (300.0, 40.0));
    }

    #[test]
    fn root_percentage_font_size_controls_rem_lengths() {
        let tree = parse_html(
            r#"<style>
                html { font-size: 62.5% }
                body { margin: 0 }
                #box {
                    display: grid;
                    width: 4rem;
                    margin-top: 3rem;
                    row-gap: 4rem;
                    column-gap: calc(1rem + 2vw);
                }
                #box > div { height: 2rem }
            </style>
            <div id="box"><div></div><div></div></div>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let html = tree.query_selector("html").unwrap().unwrap();
        let box_id = tree.query_selector("#box").unwrap().unwrap();
        assert_eq!(laid.styles.get(&html).unwrap().font_size, Some(10.0));
        assert_eq!(laid.styles.get(&box_id).unwrap().row_gap, Some(40.0));
        assert_eq!(laid.styles.get(&box_id).unwrap().column_gap, Some(35.6));
        assert_eq!(laid.rects.get(&box_id).unwrap().width, 40.0);
        assert_eq!(laid.rects.get(&box_id).unwrap().height, 80.0);
        assert_eq!(laid.rects.get(&box_id).unwrap().y, 30.0);
    }

    #[test]
    fn block_and_grid_sibling_margins_collapse() {
        let tree = parse_html(
            r#"<style>
                body { margin: 0 }
                .a { position: relative; height: 10px; margin-bottom: 30px }
                .b { position: relative; display: grid; height: 10px; margin-top: 40px }
            </style>
            <div class="a"></div>
            <div id="b" class="b"></div>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let b = tree.query_selector("#b").unwrap().unwrap();
        assert_eq!(laid.rects.get(&b).unwrap().y, 50.0);
    }

    #[cfg(feature = "paint")]
    #[test]
    fn text_transform_applies_to_word_leaves_in_flex_ui() {
        let tree = parse_html(
            r#"<div id="cta" style="display:flex;text-transform:uppercase">get started</div>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let cta = tree.get_element_by_id("cta").unwrap();
        let items = &laid.run_ifc_items[&cta];
        assert_eq!(items.len(), 1);
        assert_eq!(laid.text_engine.item_text(items[0]), "GET STARTED");
        assert_eq!(
            transform_word_leaf_text("hELLO wORLD", crate::TextTransform::Capitalize),
            "HELLO WORLD"
        );
    }

    #[cfg(feature = "paint")]
    #[test]
    fn word_leaves_use_the_computed_line_height() {
        let tree = parse_html(
            r#"<div id="code" style="display:flex;font:16px/32px monospace">code</div>"#,
        );
        let mut laid = layout_dom(&tree, (1280.0, 720.0));
        let code = tree.get_element_by_id("code").unwrap();
        let items = &laid.run_ifc_items[&code];
        assert_eq!(items.len(), 1);
        let (_, height) = laid.text_engine.measure(items[0], Some(1280.0));
        assert_eq!(height, 32.0);
    }

    #[test]
    fn light_root_rejects_dark_pseudo_gradient_variants() {
        let tree = parse_html(
            r#"<html class="light"><head><style>
               .hero::before {
                 content:"";
                 position:absolute;
                 background:radial-gradient(circle, #ebf3f9, #d6dee4);
               }
               @media (prefers-color-scheme:dark) {
                 :root:not(.light) .hero::before {
                   background:radial-gradient(circle, #111111, #222222);
                 }
               }
               :root.dark .hero::before {
                 background:radial-gradient(circle, #333333, #444444);
               }
               </style></head><body><div class="hero"></div></body></html>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let hero = tree.query_selector(".hero").unwrap().unwrap();
        let pseudo = laid.styles[&hero].before_pseudo.as_ref().unwrap();
        let (_, stops) = pseudo.background_radial_gradient.as_ref().unwrap();
        assert_eq!(stops[0].0, [0xeb, 0xf3, 0xf9, 255]);
        assert_eq!(stops[1].0, [0xd6, 0xde, 0xe4, 255]);
    }

    #[test]
    fn table_cell_content_box_width_includes_padding_and_border_in_track() {
        let tree = parse_html(
            r#"<style>
                table { border-spacing:0 }
                td { width:100px; padding:10px; border:2px solid black }
                .border { box-sizing:border-box }
            </style>
            <table><tr><td id="content-cell">content</td></tr></table>
            <table><tr><td id="border-cell" class="border">border</td></tr></table>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let width = |id: &str| {
            let nid = tree.query_selector(&format!("#{id}")).unwrap().unwrap();
            laid.rects.get(&nid).unwrap().width
        };
        assert_eq!(width("content-cell"), 124.0);
        assert_eq!(width("border-cell"), 100.0);
    }

    #[test]
    fn fixed_table_layout_uses_only_first_row_for_column_geometry() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                table { table-layout:fixed; width:300px; border-collapse:collapse }
                td { height:30px; padding:0; border:0 }
                #first { width:50px }
                #later { width:250px; white-space:nowrap }
            </style>
            <table><tr><td id="first"></td><td id="second"></td></tr>
            <tr><td id="later">XXXXXXXXXXXXXXXXXXXXXXXX</td><td id="last"></td></tr></table>"#,
        );
        let laid = layout_dom(&tree, (800.0, 300.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        for id in ["first", "later"] {
            assert!((rect(id).width - 50.0).abs() < 0.1, "{}: {:?}", id, rect(id));
        }
        for id in ["second", "last"] {
            assert!((rect(id).width - 250.0).abs() < 0.1, "{}: {:?}", id, rect(id));
        }
    }

    #[test]
    fn fixed_table_layout_accounts_for_separate_border_spacing() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                table { table-layout:fixed; width:300px; border:4px solid black; border-spacing:10px 0 }
                td { height:30px; padding:0; border:0 }
                #first { width:50px }
            </style>
            <table id="table"><tr><td id="first"></td><td id="second"></td></tr></table>"#,
        );
        let laid = layout_dom(&tree, (800.0, 300.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        assert!((rect("table").width - 300.0).abs() < 0.1, "{:?}", rect("table"));
        assert!((rect("first").x - 14.0).abs() < 0.1, "{:?}", rect("first"));
        assert!((rect("first").width - 50.0).abs() < 0.1, "{:?}", rect("first"));
        assert!((rect("second").x - 74.0).abs() < 0.1, "{:?}", rect("second"));
        assert!((rect("second").width - 212.0).abs() < 0.1, "{:?}", rect("second"));
    }

    #[test]
    fn fixed_content_box_table_keeps_border_spacing_inside_declared_width() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                table { box-sizing:content-box; table-layout:fixed; width:300px; border:4px solid black; border-spacing:10px 0 }
                td { height:30px; padding:0; border:0 }
                #first { width:50px }
            </style>
            <table id="table"><tr><td id="first"></td><td id="second"></td></tr></table>"#,
        );
        let laid = layout_dom(&tree, (800.0, 300.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        assert!((rect("table").width - 308.0).abs() < 0.1, "{:?}", rect("table"));
        assert!((rect("first").width - 50.0).abs() < 0.1, "{:?}", rect("first"));
        assert!((rect("second").width - 220.0).abs() < 0.1, "{:?}", rect("second"));
    }

    #[test]
    fn fixed_percentage_table_resolves_tracks_against_final_used_width() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                table { table-layout:fixed; width:50%; border-spacing:10px 0 }
                td { height:30px; padding:0; border:0 }
                #first { width:25% }
            </style>
            <table id="table"><tr><td id="first"></td><td id="second"></td></tr></table>"#,
        );
        let laid = layout_dom(&tree, (800.0, 300.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        assert!((rect("table").width - 400.0).abs() < 0.1, "{:?}", rect("table"));
        // Final paint geometry is snapped to device pixels around the 92.5 /
        // 277.5 CSS-pixel track boundary.
        assert!((rect("first").width - 92.5).abs() <= 0.5, "{:?}", rect("first"));
        assert!((rect("second").width - 277.5).abs() <= 0.5, "{:?}", rect("second"));
    }

    #[test]
    fn fixed_columns_win_over_first_row_spanning_cell_widths() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                table { table-layout:fixed; width:300px; border-spacing:10px 0 }
                td { height:30px; padding:0; border:0 }
            </style>
            <table><col style="width:40px"><col><col>
              <tr><td id="span" colspan="2" style="width:200px"></td><td id="top-last"></td></tr>
              <tr><td id="one"></td><td id="two"></td><td id="three"></td></tr>
            </table>"#,
        );
        let laid = layout_dom(&tree, (800.0, 300.0));
        let width = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()].width;
        assert!((width("span") - 145.0).abs() < 0.1, "{}", width("span"));
        assert!((width("one") - 40.0).abs() < 0.1, "{}", width("one"));
        assert!((width("two") - 95.0).abs() < 0.1, "{}", width("two"));
        assert!((width("three") - 125.0).abs() < 0.1, "{}", width("three"));
    }

    #[test]
    fn fixed_table_layout_with_auto_width_falls_back_to_auto_algorithm() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                table { table-layout:fixed; border-collapse:collapse }
                td { padding:0; border:0 }
                #first { width:50px }
                #later { width:250px }
            </style>
            <table><tr><td id="first"></td><td></td></tr>
            <tr><td id="later"></td><td></td></tr></table>"#,
        );
        let laid = layout_dom(&tree, (800.0, 300.0));
        let first = laid.rects[&tree.get_element_by_id("first").unwrap()];
        let later = laid.rects[&tree.get_element_by_id("later").unwrap()];
        assert!(first.width >= 249.9, "{first:?}");
        assert!((first.width - later.width).abs() < 0.1, "{first:?} {later:?}");
    }

    #[test]
    fn fixed_table_column_distribution_matches_css_fixed_rules() {
        let column = |length, percentage, specified| FixedTableColumn {
            length,
            percentage,
            specified,
        };
        let assert_widths = |actual: Vec<f32>, expected: &[f32]| {
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(expected) {
                assert!((actual - expected).abs() < 0.01, "{actual:?} != {expected:?}");
            }
        };
        assert_widths(
            distribute_fixed_table_columns(
                300.0,
                &[column(50.0, 0.0, true), column(0.0, 0.0, false)],
            ),
            &[50.0, 250.0],
        );
        assert_widths(
            distribute_fixed_table_columns(
                300.0,
                &[column(50.0, 0.0, true), column(100.0, 0.0, true)],
            ),
            &[100.0, 200.0],
        );
        assert_widths(
            distribute_fixed_table_columns(
                300.0,
                &[column(50.0, 0.0, true), column(0.0, 0.5, true)],
            ),
            &[150.0, 150.0],
        );
        assert_widths(
            distribute_fixed_table_columns(
                300.0,
                &[column(250.0, 0.0, true), column(100.0, 0.0, true)],
            ),
            &[250.0, 100.0],
        );
        assert_widths(
            distribute_fixed_table_columns(
                300.0,
                &[column(0.0, 0.75, true), column(0.0, 0.75, true)],
            ),
            &[150.0, 150.0],
        );
    }

    #[test]
    fn inherited_rtl_direction_controls_flex_grid_and_block_start_edges() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0; direction:rtl }
                .case { width:300px; height:40px; margin-bottom:20px }
                .flex { display:flex }
                .grid { display:grid; grid-template-columns:50px 50px }
                .item { width:50px; height:40px }
                .block > div { width:50px; height:40px }
            </style>
            <div id="flex" class="case flex"><div id="flex-one" class="item"></div><div id="flex-two" class="item"></div></div>
            <div id="grid" class="case grid"><div id="grid-one" class="item"></div><div id="grid-two" class="item"></div></div>
            <div class="case block"><div id="block"></div></div>"#,
        );
        let laid = layout_dom(&tree, (900.0, 300.0));
        let x = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()].x;
        assert!((x("flex-one") - 850.0).abs() < 0.1, "{}", x("flex-one"));
        assert!((x("flex-two") - 800.0).abs() < 0.1, "{}", x("flex-two"));
        assert!((x("grid-one") - 850.0).abs() < 0.1, "{}", x("grid-one"));
        assert!((x("grid-two") - 800.0).abs() < 0.1, "{}", x("grid-two"));
        assert!((x("block") - 850.0).abs() < 0.1, "{}", x("block"));
        for id in ["flex", "grid"] {
            let node = tree.get_element_by_id(id).unwrap();
            assert_eq!(laid.styles[&node].direction, Some(taffy::Direction::Rtl));
        }
    }

    #[test]
    fn html_dir_hint_is_overridden_by_author_css_and_inherits() {
        let tree = parse_html(
            r#"<style>#override { direction:ltr }</style>
            <div dir="rtl"><div id="inherited"></div><div id="override"></div></div>"#,
        );
        let laid = layout_dom(&tree, (900.0, 300.0));
        let style = |id: &str| &laid.styles[&tree.get_element_by_id(id).unwrap()];
        assert_eq!(style("inherited").direction, Some(taffy::Direction::Rtl));
        assert_eq!(style("override").direction, Some(taffy::Direction::Ltr));
    }

    #[test]
    fn inherited_direction_resolves_logical_border_cascade_after_stylesheets() {
        let tree = parse_html(
            r#"<style>
                body { direction:rtl; margin:0 }
                .box { width:200px; height:80px; box-sizing:border-box }
                .physical-low { border-right:4px solid green }
                #logical-last { border-inline-start:12px solid purple }
                .physical { border-inline-start:12px solid red }
                #physical-last { border-right:4px solid teal }
            </style>
            <div id="logical-last" class="box physical-low"></div>
            <div id="physical-last" class="box physical"></div>"#,
        );
        let laid = layout_dom(&tree, (900.0, 300.0));
        let style = |id: &str| &laid.styles[&tree.get_element_by_id(id).unwrap()];
        assert_eq!(style("logical-last").border.right, 12.0);
        // ID specificity wins regardless of source order: the physical alias
        // and logical property compete for the same final RTL side.
        assert_eq!(
            style("logical-last").border_model.colors.right,
            Some([128, 0, 128, 255])
        );
        assert_eq!(style("physical-last").border.right, 4.0);
        assert_eq!(style("physical-last").border_model.colors.right, Some([0, 128, 128, 255]));
    }

    #[test]
    fn authored_table_with_mixed_direct_content_preserves_non_cell_child() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                #group { display:table; width:300px }
                #cell { display:table-cell; width:100px; height:20px }
                #ordinary { display:block; width:80px; height:30px }
            </style>
            <div id="group">
                <div id="cell"></div>
                direct text
                <div id="ordinary"></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (500.0, 200.0));
        let id = |name: &str| tree.get_element_by_id(name).unwrap();

        assert!(laid.rects.contains_key(&id("group")));
        assert!(laid.rects.contains_key(&id("cell")));
        let ordinary = laid.rects[&id("ordinary")];
        assert!((ordinary.width - 80.0).abs() < 0.1, "{ordinary:?}");
        assert!((ordinary.height - 30.0).abs() < 0.1, "{ordinary:?}");
        // Before the preflight check, finding `cell` was enough to take the
        // dedicated table path, which filtered `ordinary` (and the text) out.
        // Its geometry proves the mixed subtree instead used ordinary box
        // construction and kept the non-cell sibling.
    }

    #[cfg(feature = "paint")]
    #[test]
    fn authored_table_cells_allocate_bootstrap_input_group_columns() {
        let tree = parse_html(
            r#"<style>
                * { box-sizing:border-box }
                html,body { margin:0 }
                #group { display:table; width:600px; border-collapse:separate }
                #field,#addon,#button { display:table-cell; height:34px }
                #field { width:100% }
                #addon,#button {
                    width:1%; white-space:nowrap; padding:0 12px;
                    font:16px/normal "Liberation Sans";
                }
            </style>
            <div id="group"><div id="field"></div><div id="addon">Addon label</div><div id="button">Button</div></div>"#,
        );
        let laid = layout_dom(&tree, (800.0, 200.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        let group = rect("group");
        let field = rect("field");
        let addon = rect("addon");
        let button = rect("button");

        assert!((group.width - 600.0).abs() < 0.1, "{group:?}");
        assert!((field.x - group.x).abs() < 0.1, "{field:?} {group:?}");
        assert!((addon.x - (field.x + field.width)).abs() < 0.1);
        assert!((button.x - (addon.x + addon.width)).abs() < 0.1);
        assert!(((button.x + button.width) - (group.x + group.width)).abs() < 0.1);
        assert!(addon.width > 80.0, "nowrap addon must keep its intrinsic width: {addon:?}");
        assert!(button.width > 50.0, "nowrap button must keep its intrinsic width: {button:?}");
        assert!(field.width > addon.width + button.width, "{field:?} {addon:?} {button:?}");
        for cell in [field, addon, button] {
            assert!((cell.height - 34.0).abs() < 0.1, "{cell:?}");
        }
    }

    #[cfg(feature = "paint")]
    #[test]
    fn nowrap_table_cell_keeps_bootstrap_controls_on_one_row() {
        let tree = parse_html(
            r#"<style>
                * { box-sizing:border-box }
                html,body { margin:0 }
                #group { display:table; width:750px; border-collapse:separate }
                #field,#actions { display:table-cell; height:55px }
                #field { float:left; width:100% }
                #actions { width:1%; white-space:nowrap }
                #bulk,#submit { display:inline-block; height:55px }
                #bulk { width:60px }
                #submit { width:42px }
            </style>
            <div id="group"><input id="field"><span id="actions"><a id="bulk"></a><button id="submit"></button></span></div>"#,
        );
        let laid = layout_dom(&tree, (900.0, 250.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        let group = rect("group");
        let field = rect("field");
        let actions = rect("actions");
        let bulk = rect("bulk");
        let submit = rect("submit");

        assert!((group.height - 55.0).abs() < 0.1, "{group:?}");
        assert!((field.height - 55.0).abs() < 0.1, "{field:?}");
        assert!((actions.height - 55.0).abs() < 0.1, "{actions:?}");
        assert!((submit.x - (bulk.x + bulk.width)).abs() < 0.1);
        assert!((bulk.y - submit.y).abs() < 0.1);
        assert!(((submit.x + submit.width) - (group.x + group.width)).abs() < 0.1);
    }

    #[test]
    fn auto_width_authored_table_honors_percentage_cell_intrinsic_constraints() {
        let tree = parse_html(
            r#"<style>
                * { box-sizing:border-box }
                html,body { margin:0 }
                #host { width:600px }
                #group { display:table }
                #field,#button { display:table-cell; height:34px }
                #field { width:100% }
                #button { width:1%; min-width:100px }
            </style>
            <div id="host"><div id="group"><div id="field"></div><div id="button"></div></div></div>"#,
        );
        let laid = layout_dom(&tree, (800.0, 200.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];
        let group = rect("group");
        let field = rect("field");
        let button = rect("button");

        // Chromium 145: group=600, field=500, button=100. An auto-width
        // table cannot shrink-wrap to 100px here: the 100% field and the
        // non-empty 1% neighbor make their percentage constraints fill the
        // definite available width.
        assert!((group.width - 600.0).abs() < 0.1, "{group:?}");
        assert!((field.width - 500.0).abs() < 0.1, "{field:?}");
        assert!((button.width - 100.0).abs() < 0.1, "{button:?}");
        assert!((button.x - (field.x + field.width)).abs() < 0.1);
    }

    #[test]
    fn auto_table_percent_guess_reserves_intrinsic_neighbor_columns() {
        let widths = distribute_auto_table_columns(
            600.0,
            &[0.0, 100.0, 70.0],
            &[0.0, 100.0, 70.0],
            &[None, None, None],
            &[Some(1.0), Some(0.01), Some(0.01)],
        );
        assert!((widths[0] - 430.0).abs() < 0.01, "{widths:?}");
        assert!((widths[1] - 100.0).abs() < 0.01, "{widths:?}");
        assert!((widths[2] - 70.0).abs() < 0.01, "{widths:?}");
    }

    #[test]
    fn auto_table_percentage_intrinsic_floor_matches_chromium_cases() {
        let percentages = [Some(0.1), None];
        assert!(
            (auto_table_percentage_intrinsic_floor(&[16.0, 144.0], &percentages) - 160.0).abs()
                < 0.01
        );
        let percentages = [Some(0.5), None];
        assert!(
            (auto_table_percentage_intrinsic_floor(&[16.0, 106.71875], &percentages) - 213.4375)
                .abs()
                < 0.01
        );
        let percentages = [Some(0.9), None];
        assert!(auto_table_percentage_intrinsic_floor(&[16.0, 106.71875], &percentages) > 1000.0);
    }

    #[test]
    fn white_space_inherits_into_a_block_descendant_inline_context() {
        let tree = parse_html(
            r#"<style>
                html, body, pre { margin:0 }
                pre { width:120px; white-space:pre; font-size:16px; line-height:20px }
                code { display:flex; flex-direction:column }
                .line { display:block }
                #normal { white-space:normal }
                #inherited::before { content:""; display:none }
            </style>
            <pre><code>
                <span id="inherited" class="line">alpha beta gamma delta</span>
                <span id="normal" class="line">alpha beta gamma delta</span>
            </code></pre>"#,
        );
        let laid = layout_dom(&tree, (400.0, 200.0));
        let inherited = tree.get_element_by_id("inherited").unwrap();
        let normal = tree.get_element_by_id("normal").unwrap();

        assert_eq!(
            laid.styles[&inherited].white_space,
            Some(crate::WhiteSpace::Pre),
            "a block descendant must inherit the preformatted wrapping mode"
        );
        assert_eq!(
            laid.styles[&normal].white_space,
            Some(crate::WhiteSpace::Normal),
            "an explicit descendant value must override inherited pre"
        );
        assert_eq!(
            laid.styles[&inherited]
                .before_pseudo
                .as_ref()
                .unwrap()
                .white_space,
            Some(crate::WhiteSpace::Pre),
            "a pseudo-element must inherit the originating element's computed wrapping mode"
        );
        assert_eq!(
            laid.rects[&inherited].height, 20.0,
            "the inherited pre line must overflow horizontally without wrapping"
        );
        assert!(
            laid.rects[&normal].height > 20.0,
            "the explicit normal line should still wrap at the containing width"
        );
    }

    #[cfg(feature = "paint")]
    #[test]
    fn forced_break_in_pseudo_joined_inline_run_has_no_phantom_line() {
        let tree = parse_html(
            r#"<style>
                html,body,h1,p { margin:0 }
                h1 { width:600px; font:40px/60px sans-serif }
                #heading::after, #consecutive::after {
                    content:"_"; display:inline; position:relative
                }
                p { width:300px; font:16px/20px sans-serif }
            </style>
            <h1 id="heading">first<br id="heading-break">second</h1>
            <p id="consecutive">A<br><br>B</p>"#,
        );
        let laid = layout_dom(&tree, (800.0, 400.0));
        let heading = tree.get_element_by_id("heading").unwrap();
        let heading_break = tree.get_element_by_id("heading-break").unwrap();
        let consecutive = tree.get_element_by_id("consecutive").unwrap();
        let break_style = &laid.styles[&heading_break];

        assert_eq!(break_style.display, crate::Display::Inline);
        assert_eq!(break_style.width, crate::Dimension::Auto);
        assert_eq!(break_style.height, crate::Dimension::Auto);
        assert!(
            (laid.rects[&heading].height - 120.0).abs() < 0.01,
            "A<br>B plus an inline pseudo must form two 60px lines: host={:?}, br={:?}, pseudo={}, run_ifc={}",
            laid.rects[&heading],
            laid.rects.get(&heading_break),
            laid.styles[&heading].after_pseudo.is_some(),
            laid.run_ifc_items
                .get(&heading)
                .map_or(0, |items| items.len())
        );
        assert_eq!(
            laid.rects
                .get(&heading_break)
                .map_or(0.0, |rect| rect.width),
            0.0,
            "the mapped BR marker must not expose the anonymous breaker width"
        );
        assert!(
            (laid.rects[&consecutive].height - 60.0).abs() < 0.01,
            "A<br><br>B must form three 20px lines: {:?}",
            laid.rects[&consecutive]
        );
    }

    #[test]
    fn ua_block_margins_resolve_against_the_computed_element_font() {
        let tree = parse_html(
            r#"<style>
                body { margin:0 }
                h1 { font-size:40px }
                p { font-size:24px }
            </style>
            <h1 id="heading">Heading</h1>
            <p id="paragraph">Paragraph</p>
            <section style="font-size:20px">
              <h1 id="h1">H1</h1><h2 id="h2">H2</h2><h3 id="h3">H3</h3>
              <h4 id="h4">H4</h4><h5 id="h5">H5</h5><h6 id="h6">H6</h6>
              <h1 id="inherit-size" style="font-size:inherit">Inherited</h1>
              <h1 id="initial-size" style="font-size:initial">Initial</h1>
            </section>
            <ul id="outer-list"><li>
              <ul id="second-list"><li>
                <ol><li><ul id="third-list"></ul></li></ol>
              </li></ul>
            </li></ul>
            <dl><dd><ul id="definition-list-child"></ul></dd></dl>"#,
        );
        let laid = layout_dom(&tree, (800.0, 400.0));
        let heading = tree.get_element_by_id("heading").unwrap();
        let paragraph = tree.get_element_by_id("paragraph").unwrap();

        assert!(
            (laid.styles[&heading].margin.top - 26.8).abs() < 0.01
                && (laid.styles[&heading].margin.bottom - 26.8).abs() < 0.01,
            "h1's 0.67em UA margins must follow its authored 40px font: {:?}",
            laid.styles[&heading].margin
        );
        assert!(
            (laid.styles[&paragraph].margin.top - 24.0).abs() < 0.01
                && (laid.styles[&paragraph].margin.bottom - 24.0).abs() < 0.01,
            "p's 1em UA margins must follow its authored 24px font: {:?}",
            laid.styles[&paragraph].margin
        );
        for (id, size, margin) in [
            ("h1", 40.0, 26.8),
            ("h2", 30.0, 24.9),
            ("h3", 23.4, 23.4),
            ("h4", 20.0, 26.6),
            ("h5", 16.6, 27.722),
            ("h6", 13.4, 31.222),
        ] {
            let style = &laid.styles[&tree.get_element_by_id(id).unwrap()];
            assert!(
                (style.font_size.unwrap() - size).abs() < 0.01
                    && (style.margin.top - margin).abs() < 0.01
                    && (style.margin.bottom - margin).abs() < 0.01,
                "{id} UA font and margins must resolve from the inherited 20px font: size={:?}, margin={:?}",
                style.font_size,
                style.margin
            );
        }
        for (id, size, margin) in [("inherit-size", 20.0, 13.4), ("initial-size", 16.0, 10.72)] {
            let style = &laid.styles[&tree.get_element_by_id(id).unwrap()];
            assert!(
                (style.font_size.unwrap() - size).abs() < 0.01
                    && (style.margin.top - margin).abs() < 0.01,
                "{id} CSS-wide font-size semantics must override the UA heading size: size={:?}, margin={:?}",
                style.font_size,
                style.margin
            );
        }
        for (id, marker) in [
            ("second-list", crate::ListStyle::Circle),
            ("third-list", crate::ListStyle::Square),
            ("definition-list-child", crate::ListStyle::Disc),
        ] {
            let style = &laid.styles[&tree.get_element_by_id(id).unwrap()];
            assert_eq!(style.margin.top, 0.0, "{id} must lose nested UA margins");
            assert_eq!(style.margin.bottom, 0.0, "{id} must lose nested UA margins");
            assert_eq!(style.list_style, Some(marker), "{id} marker depth");
        }
    }

    #[test]
    fn generated_inline_boxes_apply_sizes_only_after_display_blockification() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                #flex { display:flex }
                #flex::before {
                    content:""; display:inline;
                    width:100px; height:70px;
                    min-width:250px; min-height:120px;
                    max-width:50px; max-height:40px;
                    background:red
                }
                #float::before {
                    content:""; display:inline; float:left;
                    width:80px; height:40px; background:blue
                }
                #relative::before {
                    content:""; display:inline; position:relative;
                    width:80px; height:40px; background:green
                }
            </style>
            <div id="flex"></div>
            <div id="float"></div>
            <div id="relative"></div>"#,
        );
        let laid = layout_dom(&tree, (500.0, 300.0));
        let flex = tree.get_element_by_id("flex").unwrap();
        let float = tree.get_element_by_id("float").unwrap();
        let relative = tree.get_element_by_id("relative").unwrap();

        let pseudo = |host| {
            laid.generated_boxes
                .iter()
                .find(|generated| {
                    generated.host == host && generated.kind == GeneratedBoxKind::Before
                })
                .unwrap()
                .rect
        };
        assert_eq!(
            laid.styles[&flex].before_pseudo.as_ref().unwrap().display,
            crate::Display::Block,
            "a generated flex item must be blockified"
        );
        assert_eq!(
            laid.styles[&float].before_pseudo.as_ref().unwrap().display,
            crate::Display::Block,
            "a floated generated box must be blockified"
        );
        assert_eq!(
            laid.styles[&relative]
                .before_pseudo
                .as_ref()
                .unwrap()
                .display,
            crate::Display::Inline,
            "relative positioning does not make an ordinary inline atomic"
        );
        assert_eq!(pseudo(flex).width, 250.0);
        assert_eq!(pseudo(flex).height, 120.0);
        assert_eq!(pseudo(float).width, 80.0);
        assert_eq!(pseudo(float).height, 40.0);
        assert!(
            pseudo(relative).width < 80.0 && pseudo(relative).height < 40.0,
            "an ordinary inline pseudo must ignore authored box sizes"
        );
    }

    #[cfg(feature = "paint")]
    #[test]
    fn ordinary_inline_fragment_uses_font_box_while_line_keeps_line_height() {
        let tree = parse_html(
            r#"<style>
                html,body,p { margin:0 }
                p { height:auto; font:16px/40px monospace }
                code {
                    position:relative;
                    font:16px/40px monospace;
                    padding-top:5px; padding-bottom:7px;
                    border-top:2px solid; border-bottom:3px solid;
                    margin-top:19px; margin-bottom:23px;
                    background:red
                }
                #control { margin-top:0; margin-bottom:0 }
            </style>
            <p id="with-margin"><code id="token">token</code></p>
            <p id="without-margin"><code id="control">token</code></p>"#,
        );
        let laid = layout_dom(&tree, (400.0, 200.0));
        let host = tree.get_element_by_id("with-margin").unwrap();
        let control_host = tree.get_element_by_id("without-margin").unwrap();
        let token = tree.get_element_by_id("token").unwrap();
        let control = tree.get_element_by_id("control").unwrap();
        let style = &laid.styles[&token];
        let raw_font_height = laid.text_engine.inline_font_box_height(style);
        let expected_fragment_height = raw_font_height + 5.0 + 7.0 + 2.0 + 3.0;

        assert_eq!(
            laid.rects[&host].height, 40.0,
            "block-axis inline padding/border must not increase line advance"
        );
        assert_eq!(laid.rects[&control_host].height, 40.0);
        assert_eq!(laid.rects[&token].height, expected_fragment_height);
        assert_eq!(laid.inline_fragments[&token], vec![laid.rects[&token]]);
        assert!(
            ((laid.rects[&token].y - laid.rects[&host].y)
                - ((40.0 - raw_font_height) / 2.0 - 5.0 - 2.0))
                .abs()
                < 0.5,
            "final shaped baseline must retain the expected centered font box"
        );
        assert_eq!(
            laid.rects[&control].y - laid.rects[&control_host].y,
            laid.rects[&token].y - laid.rects[&host].y,
            "ordinary inline block-axis margins must not move its fragment"
        );
    }

    #[cfg(feature = "paint")]
    #[test]
    fn nested_inline_uses_final_shaping_for_its_canonical_fragment() {
        let tree = parse_html(
            r#"<style>
                html,body,p { margin:0 }
                p { width:300px; font:16px/20px monospace }
            </style>
            <p id="host">before <span id="token">plain nested text</span> after</p>"#,
        );
        let laid = layout_dom(&tree, (400.0, 100.0));
        let token = tree.get_element_by_id("token").unwrap();
        let fragments = &laid.inline_fragments[&token];

        assert_eq!(fragments.len(), 1, "{fragments:?}");
        assert_eq!(laid.rects[&token], fragments[0]);
        assert!(fragments[0].width > 100.0, "{fragments:?}");
        assert_eq!(
            laid.ifc_items.len(),
            1,
            "the span must stay in one shaped IFC"
        );
    }

    #[test]
    fn wrapped_decorated_inline_exposes_three_ordered_font_box_fragments() {
        let tree = parse_html(
            r#"<style>
                html,body,p { margin:0 }
                p { width:80px; font:16px/20px monospace }
                code {
                    padding-top:2px; padding-bottom:3px;
                    border-top:1px solid; border-bottom:2px solid;
                    background:red
                }
            </style>
            <p><code id="token">alpha beta gamma</code></p>"#,
        );
        let laid = layout_dom(&tree, (300.0, 120.0));
        let token = tree.get_element_by_id("token").unwrap();
        let fragments = &laid.inline_fragments[&token];

        assert_eq!(
            fragments.len(),
            3,
            "one canonical fragment per visual line: {fragments:?}"
        );
        assert!(fragments.windows(2).all(|pair| pair[0].y < pair[1].y));
        assert!(fragments.iter().all(|fragment| fragment.width > 0.0));
        let left = fragments
            .iter()
            .map(|rect| rect.x)
            .fold(f32::INFINITY, f32::min);
        let top = fragments
            .iter()
            .map(|rect| rect.y)
            .fold(f32::INFINITY, f32::min);
        let right = fragments
            .iter()
            .map(|rect| rect.x + rect.width)
            .fold(f32::NEG_INFINITY, f32::max);
        let bottom = fragments
            .iter()
            .map(|rect| rect.y + rect.height)
            .fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(
            laid.rects[&token],
            Rect {
                x: left,
                y: top,
                width: right - left,
                height: bottom - top
            }
        );
    }

    #[test]
    fn inline_horizontal_edges_advance_adjacent_content_but_margins_stay_outside_rect() {
        let tree = parse_html(
            r#"<style>
                html,body,p { margin:0 }
                p { font:16px/20px monospace; white-space:nowrap }
                #token {
                    margin-left:3px; padding:0 4px;
                    border-left:2px solid; border-right:2px solid;
                    margin-right:5px
                }
            </style>
            <p id="decorated"><span id="token">aa</span><span id="after">bb</span></p>
            <p id="plain-host"><span id="plain">aa</span><span id="plain-after">bb</span></p>"#,
        );
        let laid = layout_dom(&tree, (400.0, 100.0));
        let rect = |id: &str| -> Rect { laid.rects[&tree.get_element_by_id(id).unwrap()] };
        let local_x = |id: &str, host: &str| rect(id).x - rect(host).x;

        assert!((rect("token").width - rect("plain").width - 12.0).abs() < 0.01);
        assert!(
            (local_x("token", "decorated") - local_x("plain", "plain-host") - 3.0).abs() < 0.01
        );
        assert!(
            (local_x("after", "decorated") - local_x("plain-after", "plain-host") - 20.0).abs()
                < 0.01
        );
        assert!((rect("after").x - (rect("token").x + rect("token").width) - 5.0).abs() < 0.01);
    }

    #[test]
    fn inline_edges_participate_in_wrapping_and_slice_only_outer_continuation_sides() {
        let tree = parse_html(
            r#"<style>
                html,body,p { margin:0 }
                p { width:70px; font:16px/20px monospace }
                #token { padding:0 10px; border-left:2px solid; border-right:2px solid }
            </style>
            <p><span id="token">aaaa aaaa aaaa</span></p>"#,
        );
        let laid = layout_dom(&tree, (200.0, 100.0));
        let token = tree.get_element_by_id("token").unwrap();
        let fragments = &laid.inline_fragments[&token];

        assert_eq!(fragments.len(), 3, "{fragments:?}");
        assert!(
            (fragments[0].width - fragments[1].width - 12.0).abs() < 0.01,
            "{fragments:?}"
        );
        assert!(
            fragments[2].width > fragments[1].width,
            "last continuation owns its end side: {fragments:?}"
        );
        assert!(fragments.iter().all(|fragment| fragment.width <= 70.01));
    }

    #[cfg(feature = "paint")]
    #[test]
    fn empty_decorated_inline_has_a_box_and_advances_following_text() {
        let tree = parse_html(
            r#"<style>
                html,body,p { margin:0 }
                p { font:16px/20px monospace; white-space:nowrap }
                #empty {
                    margin:0 3px; padding:2px 5px; border:1px solid;
                    border-left-width:2px; border-right-width:2px
                }
            </style>
            <p id="decorated">x<span id="empty"></span><span id="after">y</span></p>
            <p id="plain-host">x<span id="plain-after">y</span></p>"#,
        );
        let laid = layout_dom(&tree, (300.0, 100.0));
        let rect = |id: &str| -> Rect { laid.rects[&tree.get_element_by_id(id).unwrap()] };

        assert!(
            (rect("empty").width - 14.0).abs() < 0.01,
            "{:?}",
            rect("empty")
        );
        assert!(
            (rect("empty").height
                - laid.text_engine.inline_font_box_height(
                    &laid.styles[&tree.get_element_by_id("empty").unwrap()]
                )
                - 6.0)
                .abs()
                < 0.01
        );
        let decorated_after = rect("after").x - rect("decorated").x;
        let plain_after = rect("plain-after").x - rect("plain-host").x;
        assert!((decorated_after - plain_after - 20.0).abs() < 0.01);
    }

    #[test]
    fn inline_provenance_maps_repeated_multibyte_text_across_hard_breaks() {
        let tree = parse_html(
            r#"<style>
                html,body,p { margin:0 }
                p { width:200px; font:18px/24px sans-serif }
            </style>
            <p><span id="token">é界<br>é界</span></p>"#,
        );
        let laid = layout_dom(&tree, (300.0, 100.0));
        let token = tree.get_element_by_id("token").unwrap();
        let fragments = &laid.inline_fragments[&token];

        assert_eq!(fragments.len(), 2, "{fragments:?}");
        assert!(
            (fragments[0].width - fragments[1].width).abs() < 0.01,
            "{fragments:?}"
        );
        assert!(
            (fragments[1].y - fragments[0].y - 24.0).abs() < 0.01,
            "{fragments:?}"
        );
    }

    #[test]
    fn relative_inline_offsets_move_canonical_fragments_in_pixels_and_percentages() {
        let tree = parse_html(
            r#"<style>
                html,body,p { margin:0 }
                p { width:200px; height:20px; font:16px/20px monospace }
                #pixels { position:relative; left:7px; top:5px }
                #percent { position:relative; left:10%; top:10% }
            </style>
            <p id="base-host"><span id="base">token</span></p>
            <p id="pixel-host"><span id="pixels">token</span></p>
            <p id="percent-host"><span id="percent">token</span></p>"#,
        );
        let laid = layout_dom(&tree, (400.0, 120.0));
        let rect = |id: &str| -> Rect { laid.rects[&tree.get_element_by_id(id).unwrap()] };
        let local = |token: &str, host: &str| {
            let token = rect(token);
            let host = rect(host);
            (token.x - host.x, token.y - host.y)
        };
        let base = local("base", "base-host");
        let pixels = local("pixels", "pixel-host");
        let percent = local("percent", "percent-host");

        assert!(
            (pixels.0 - base.0 - 7.0).abs() < 0.01,
            "{base:?} {pixels:?}"
        );
        assert!(
            (pixels.1 - base.1 - 5.0).abs() < 0.01,
            "{base:?} {pixels:?}"
        );
        assert!(
            (percent.0 - base.0 - 20.0).abs() < 0.01,
            "{base:?} {percent:?}"
        );
        assert!(
            (percent.1 - base.1 - 2.0).abs() < 0.01,
            "{base:?} {percent:?}"
        );
    }

    #[test]
    fn canonical_inline_fragments_shape_with_the_loaded_webfont() {
        let tree = parse_html(
            r#"<style>
                html,body,p { margin:0 }
                #token {
                    position:relative;
                    font-family:Fixture, sans-serif;
                    font-size:40px;
                    line-height:50px;
                    background:red
                }
            </style>
            <p><span id="token">WWWWiiii</span></p>"#,
        );
        let token = tree.get_element_by_id("token").unwrap();
        let fallback = layout_dom(&tree, (500.0, 150.0));
        let loaded = layout_dom_with_web_fonts(
            &tree,
            (500.0, 150.0),
            &HashMap::new(),
            &[crate::inline::WebFont {
                data: include_bytes!("../assets/liberation-serif.ttf").to_vec(),
                family: Some("Fixture".to_string()),
                weight: Some((400, 400)),
                italic: Some(false),
            }],
        );

        assert!(loaded.inline_fragments.contains_key(&token));
        assert_ne!(
            fallback.rects[&token].width, loaded.rects[&token].width,
            "the loaded face's advances must drive fallback inline geometry"
        );
        assert_ne!(
            fallback.rects[&token].height, loaded.rects[&token].height,
            "the loaded face's raw ascent/descent must drive the visual fragment"
        );
        assert_eq!(
            loaded.inline_fragments[&token],
            vec![loaded.rects[&token]],
            "the loaded-font fragment must be the geometry/paint source"
        );
    }

    #[test]
    fn generated_pseudo_content_shapes_with_the_loaded_webfont() {
        let tree = parse_html(
            r#"<style>
                html,body,p { margin:0 }
                #token::before {
                    content:"WWWWiiii";
                    font-family:Fixture, sans-serif;
                    font-size:40px;
                    line-height:50px
                }
            </style>
            <p id="token"></p>"#,
        );
        let token = tree.get_element_by_id("token").unwrap();
        let fallback = layout_dom(&tree, (500.0, 150.0));
        let loaded = layout_dom_with_web_fonts(
            &tree,
            (500.0, 150.0),
            &HashMap::new(),
            &[crate::inline::WebFont {
                data: include_bytes!("../assets/liberation-serif.ttf").to_vec(),
                family: Some("Fixture".to_string()),
                weight: Some((400, 400)),
                italic: Some(false),
            }],
        );

        assert!(
            loaded.word_ifc_items.contains_key(&token),
            "generated text must retain its webfont-shaped paint item"
        );
        assert_ne!(
            fallback.text_runs[&token][0].0.width, loaded.text_runs[&token][0].0.width,
            "the loaded face's advances must drive generated-content geometry"
        );
    }

    #[test]
    fn positioned_pseudo_inherits_the_hosts_computed_font_metrics() {
        let tree = parse_html(
            r#"<style>
                html { font-size:16px }
                button { font-size:.875rem; line-height:1.5 }
                button::after {
                    content:attr(text); position:absolute; inset:1px;
                }
            </style>
            <button id="cta" text="Get Started">Get Started</button>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let cta = tree.get_element_by_id("cta").unwrap();
        let host = &laid.styles[&cta];
        let pseudo = host.after_pseudo.as_ref().unwrap();

        assert_eq!(host.font_size, Some(14.0));
        assert_eq!(pseudo.font_size, host.font_size);
        assert_eq!(pseudo.font_family, host.font_family);
        assert_eq!(pseudo.font_weight, host.font_weight);
        assert_eq!(pseudo.line_height, host.line_height);
    }

    #[test]
    fn generated_pseudo_grid_calc_uses_computed_font_and_viewport_context() {
        let tree = parse_html(
            r#"<style>
                html { font-size:16px }
                #host { font-size:20px }
                #host::before {
                    content:""; display:grid;
                    grid-template-columns:calc(1em + 1rem + 10vw);
                }
            </style>
            <div id="host"></div>"#,
        );
        let laid = layout_dom(&tree, (1000.0, 400.0));
        let host = tree.get_element_by_id("host").unwrap();
        let pseudo = laid.styles[&host].before_pseudo.as_ref().unwrap();
        let calc = &pseudo.grid_calc_expressions[0][0];
        let handle = std::sync::Arc::as_ptr(calc).cast();

        assert!((crate::style::resolve_grid_calc(handle, 1000.0) - 136.0).abs() < 0.01);
    }

    #[test]
    fn positioned_pseudo_does_not_wrap_shrink_to_fit_host_text() {
        let tree = parse_html(
            r#"<style>
                html, body { margin:0 }
                #host {
                    position:absolute; width:max-content;
                    padding:12px 24px; border:0;
                    font-size:14px; font-weight:600; color:transparent;
                }
                #host::after {
                    content:attr(text); position:absolute; inset:1px;
                    display:flex; align-items:center; justify-content:center;
                    color:black; background:white;
                }
            </style>
            <button id="host" text="Learn more">Learn more</button>"#,
        );
        let laid = layout_dom(&tree, (500.0, 300.0));
        let host = tree.get_element_by_id("host").unwrap();
        let rect = laid.rects[&host];

        assert!(
            (120.0..=130.0).contains(&rect.width),
            "max-content width must include the shaped inter-word advance: {rect:?}"
        );
        assert_eq!(
            rect.height, 40.0,
            "positioned generated content must not make the host label wrap"
        );
    }

    #[test]
    fn variable_font_properties_inherit_and_reset_through_elements_and_pseudos() {
        let tree = parse_html(
            r#"<style>
                #parent {
                    font-optical-sizing:none;
                    font-variation-settings:"opsz" 20, "wght" 650;
                }
                #parent::before { content:"before" }
                #reset {
                    font-optical-sizing:initial;
                    font-variation-settings:normal;
                }
                #reset::after {
                    content:"after";
                    font-optical-sizing:inherit;
                    font-variation-settings:"wght" 300;
                }
            </style>
            <div id="parent">
                <span id="inherited"></span>
                <span id="reset"><b id="reset-child"></b></span>
            </div>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let get = |selector: &str| {
            let id = tree.query_selector(selector).unwrap().unwrap();
            &laid.styles[&id]
        };
        let parent = get("#parent");
        let inherited = get("#inherited");
        assert_eq!(
            parent.font_optical_sizing,
            Some(crate::FontOpticalSizing::None)
        );
        assert_eq!(inherited.font_optical_sizing, parent.font_optical_sizing);
        assert_eq!(
            inherited.font_variation_settings,
            parent.font_variation_settings
        );
        let before = parent.before_pseudo.as_ref().unwrap();
        assert_eq!(before.font_optical_sizing, parent.font_optical_sizing);
        assert_eq!(
            before.font_variation_settings,
            parent.font_variation_settings
        );

        let reset = get("#reset");
        let reset_child = get("#reset-child");
        assert_eq!(
            reset.font_optical_sizing,
            Some(crate::FontOpticalSizing::Auto)
        );
        assert_eq!(reset.font_variation_settings, Some(Vec::new()));
        assert_eq!(reset_child.font_optical_sizing, reset.font_optical_sizing);
        assert_eq!(
            reset_child.font_variation_settings,
            reset.font_variation_settings
        );
        let after = reset.after_pseudo.as_ref().unwrap();
        assert_eq!(after.font_optical_sizing, reset.font_optical_sizing);
        assert_eq!(
            after.font_variation_settings.as_deref(),
            Some(
                [crate::FontVariationSetting {
                    tag: *b"wght",
                    value: 300.0,
                }]
                .as_slice()
            )
        );
    }

    #[test]
    fn native_select_sizes_for_its_widest_option() {
        let tree = parse_html(
            r#"<select id="language">
                <option selected>En</option>
                <option>Brazilian Portuguese</option>
            </select>"#,
        );
        let laid = layout_dom(&tree, (500.0, 200.0));
        let select = tree.get_element_by_id("language").unwrap();
        let rect = laid.rects[&select];

        assert!(
            rect.width > 120.0,
            "widest option should set width: {rect:?}"
        );
        assert!(
            (18.0..=22.0).contains(&rect.height),
            "closed native select should have one-line control height: {rect:?}"
        );
    }

    #[test]
    fn display_contents_descendants_resolve_as_named_grid_area_items() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #grid{
                    display:grid;width:400px;height:100px;
                    grid-template-columns:100px 300px;
                    grid-template-rows:40px 60px;
                    grid-template-areas:"nav head" "nav main";
                }
                #contents{display:contents}
                #nav{grid-area:nav}#head{grid-area:head}#main{grid-area:main}
            </style>
            <div id="grid"><div id="contents">
                <nav id="nav"></nav><header id="head"></header><article id="main"></article>
            </div></div>"#,
        );
        let laid = layout_dom(&tree, (600.0, 300.0));
        let rect = |id| laid.rects[&tree.get_element_by_id(id).unwrap()];

        assert_eq!(
            rect("nav"),
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            }
        );
        assert_eq!(
            rect("head"),
            Rect {
                x: 100.0,
                y: 0.0,
                width: 300.0,
                height: 40.0,
            }
        );
        assert_eq!(
            rect("main"),
            Rect {
                x: 100.0,
                y: 40.0,
                width: 300.0,
                height: 60.0,
            }
        );
    }

    #[test]
    fn display_contents_generated_pseudo_remains_a_named_grid_item() {
        let tree = parse_html(
            r#"<style>
                html,body{margin:0}
                #grid{
                    display:grid;width:400px;height:50px;
                    grid-template-columns:100px 300px;
                    grid-template-rows:50px;
                    grid-template-areas:"marker body";
                }
                #contents{display:contents}
                #contents::before{content:"";display:block;grid-area:marker}
                #body{grid-area:body}
            </style>
            <div id="grid"><div id="contents"><article id="body"></article></div></div>"#,
        );
        let laid = layout_dom(&tree, (600.0, 300.0));
        let contents = tree.get_element_by_id("contents").unwrap();
        let body = tree.get_element_by_id("body").unwrap();
        let generated = laid
            .generated_boxes
            .iter()
            .find(|box_| {
                box_.host == contents && box_.kind == GeneratedBoxKind::Before
            })
            .expect("display:contents ::before must generate an effective grid item");

        assert_eq!(
            generated.rect,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 50.0,
            }
        );
        assert_eq!(
            laid.rects[&body],
            Rect {
                x: 100.0,
                y: 0.0,
                width: 300.0,
                height: 50.0,
            }
        );
    }

    #[test]
    fn grid_auto_columns_size_successive_implicit_column_tracks() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                #grid {
                    display:grid;
                    width:300px;
                    grid-template-columns:100px;
                    grid-auto-columns:50px;
                    grid-auto-flow:column;
                }
                .item { height:20px }
            </style>
            <div id="grid">
                <div id="first" class="item"></div>
                <div id="second" class="item"></div>
                <div id="third" class="item"></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (500.0, 200.0));
        let rect = |id| laid.rects[&tree.get_element_by_id(id).unwrap()];

        assert_eq!(
            rect("first"),
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 20.0
            }
        );
        assert_eq!(
            rect("second"),
            Rect {
                x: 100.0,
                y: 0.0,
                width: 50.0,
                height: 20.0
            }
        );
        assert_eq!(
            rect("third"),
            Rect {
                x: 150.0,
                y: 0.0,
                width: 50.0,
                height: 20.0
            }
        );
    }

    #[test]
    fn grid_auto_rows_size_successive_implicit_row_tracks() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                #grid {
                    display:grid;
                    width:100px;
                    height:300px;
                    grid-template-columns:100px;
                    grid-template-rows:100px;
                    grid-auto-rows:50px;
                    grid-auto-flow:row;
                }
            </style>
            <div id="grid">
                <div id="first"></div>
                <div id="second"></div>
                <div id="third"></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (500.0, 400.0));
        let rect = |id| laid.rects[&tree.get_element_by_id(id).unwrap()];

        assert_eq!(
            rect("first"),
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0
            }
        );
        assert_eq!(
            rect("second"),
            Rect {
                x: 0.0,
                y: 100.0,
                width: 100.0,
                height: 50.0
            }
        );
        assert_eq!(
            rect("third"),
            Rect {
                x: 0.0,
                y: 150.0,
                width: 100.0,
                height: 50.0
            }
        );
    }

    #[test]
    fn grid_auto_track_inherit_uses_the_parent_computed_lists() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                #parent { grid-auto-columns:60px;grid-auto-rows:70px }
                #grid {
                    display:grid;
                    grid-template-columns:100px;
                    grid-auto-columns:inherit;
                    grid-auto-rows:inherit;
                    grid-auto-flow:column;
                }
                #grid > div { height:20px }
            </style>
            <div id="parent">
                <div id="grid"><div></div><div id="implicit"></div></div>
            </div>"#,
        );
        let laid = layout_dom(&tree, (500.0, 300.0));
        let grid = tree.get_element_by_id("grid").unwrap();
        let implicit = tree.get_element_by_id("implicit").unwrap();

        assert_eq!(laid.rects[&implicit].x, 100.0);
        assert_eq!(laid.rects[&implicit].width, 60.0);
        assert_eq!(laid.styles[&grid].grid_auto_columns.len(), 1);
        assert_eq!(laid.styles[&grid].grid_auto_rows.len(), 1);
        assert!(!laid.styles[&grid].grid_auto_columns_inherit);
        assert!(!laid.styles[&grid].grid_auto_rows_inherit);
    }

    #[cfg(feature = "paint")]
    #[test]
    fn text_break_modes_match_chromium_fixed_width_geometry() {
        // Chromium 145 at DPR 1 with Liberation Sans produces the same
        // 20/40px line boxes. `break-word` and `anywhere` are emergency
        // wrapping modes; `break-all` adds typographic-letter opportunities.
        // keep-all must retain ordinary Latin word/space behavior.
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                .fixed { width:80px;font:16px/20px "Liberation Sans" }
                #normal { overflow-wrap:normal }
                #break-word { overflow-wrap:break-word }
                #anywhere { overflow-wrap:anywhere }
                #break-all { word-break:break-all }
                #keep-all { word-break:keep-all }
                #legacy { word-break:break-word }
            </style>
            <div id="normal" class="fixed">abcdefghijklmnop</div>
            <div id="break-word" class="fixed">abcdefghijklmnop</div>
            <div id="anywhere" class="fixed">abcdefghijklmnop</div>
            <div id="break-all" class="fixed">abcdefghijklmnop</div>
            <div id="latin-normal" class="fixed">alpha beta gamma</div>
            <div id="keep-all" class="fixed">alpha beta gamma</div>
            <div id="legacy" class="fixed">abcdefghijklmnop</div>"#,
        );
        let laid = layout_dom(&tree, (500.0, 500.0));
        let rect = |id| laid.rects[&tree.get_element_by_id(id).unwrap()];

        assert_eq!(
            rect("normal"),
            Rect {
                x: 0.0,
                y: 0.0,
                width: 80.0,
                height: 20.0
            }
        );
        assert_eq!(
            rect("break-word"),
            Rect {
                x: 0.0,
                y: 20.0,
                width: 80.0,
                height: 40.0
            }
        );
        assert_eq!(
            rect("anywhere"),
            Rect {
                x: 0.0,
                y: 60.0,
                width: 80.0,
                height: 40.0
            }
        );
        assert_eq!(
            rect("break-all"),
            Rect {
                x: 0.0,
                y: 100.0,
                width: 80.0,
                height: 40.0
            }
        );
        assert_eq!(rect("latin-normal").height, 40.0);
        assert_eq!(rect("keep-all").height, rect("latin-normal").height);
        assert_eq!(rect("legacy").height, 40.0);
    }

    #[cfg(feature = "paint")]
    #[test]
    fn text_break_inheritance_alias_and_css_wide_values_match_chromium() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                .parent { overflow-wrap:anywhere;word-break:break-all;font:16px/20px "Liberation Sans" }
                .fixed { width:80px }
                #initial { overflow-wrap:initial;word-break:initial }
                #unset { overflow-wrap:unset;word-break:unset }
                #inherit { overflow-wrap:inherit;word-break:inherit }
                #revert { overflow-wrap:revert;word-break:revert }
                #revert-layer { overflow-wrap:revert-layer;word-break:revert-layer }
                #alias { word-wrap:anywhere;word-break:normal }
                #inherited::before { content:"";display:none }
            </style>
            <section class="parent">
                <div id="inherited" class="fixed">abcdefghijklmnop</div>
                <div id="initial" class="fixed">abcdefghijklmnop</div>
                <div id="unset" class="fixed">abcdefghijklmnop</div>
                <div id="inherit" class="fixed">abcdefghijklmnop</div>
                <div id="revert" class="fixed">abcdefghijklmnop</div>
                <div id="revert-layer" class="fixed">abcdefghijklmnop</div>
                <div id="alias" class="fixed">abcdefghijklmnop</div>
            </section>"#,
        );
        let laid = layout_dom(&tree, (500.0, 500.0));
        let node = |id| tree.get_element_by_id(id).unwrap();
        let style = |id| &laid.styles[&node(id)];
        let height = |id| laid.rects[&node(id)].height;

        assert_eq!(
            style("inherited").overflow_wrap,
            Some(crate::OverflowWrap::Anywhere)
        );
        assert_eq!(
            style("inherited").word_break,
            Some(crate::WordBreak::BreakAll)
        );
        assert_eq!(height("inherited"), 40.0);
        assert_eq!(
            style("inherited")
                .before_pseudo
                .as_ref()
                .unwrap()
                .overflow_wrap,
            Some(crate::OverflowWrap::Anywhere)
        );
        assert_eq!(
            style("inherited")
                .before_pseudo
                .as_ref()
                .unwrap()
                .word_break,
            Some(crate::WordBreak::BreakAll)
        );

        assert_eq!(
            style("initial").overflow_wrap,
            Some(crate::OverflowWrap::Normal)
        );
        assert_eq!(style("initial").word_break, Some(crate::WordBreak::Normal));
        assert_eq!(height("initial"), 20.0);
        for id in ["unset", "inherit", "revert", "revert-layer"] {
            assert_eq!(
                style(id).overflow_wrap,
                Some(crate::OverflowWrap::Anywhere),
                "{id}"
            );
            assert_eq!(
                style(id).word_break,
                Some(crate::WordBreak::BreakAll),
                "{id}"
            );
            assert_eq!(height(id), 40.0, "{id}");
        }
        assert_eq!(
            style("alias").overflow_wrap,
            Some(crate::OverflowWrap::Anywhere)
        );
        assert_eq!(style("alias").word_break, Some(crate::WordBreak::Normal));
        assert_eq!(height("alias"), 40.0);
    }

    #[cfg(feature = "paint")]
    #[test]
    fn anywhere_but_not_break_word_reduces_min_content() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                .grid { display:grid;grid-template-columns:min-content;font:16px/20px "Liberation Sans" }
                .cell { display:block }
                #normal { overflow-wrap:normal }
                #break-word { overflow-wrap:break-word }
                #anywhere { overflow-wrap:anywhere }
                #break-all { word-break:break-all }
            </style>
            <div class="grid"><div id="normal" class="cell">abcdefghijklmnop</div></div>
            <div class="grid"><div id="break-word" class="cell">abcdefghijklmnop</div></div>
            <div class="grid"><div id="anywhere" class="cell">abcdefghijklmnop</div></div>
            <div class="grid"><div id="break-all" class="cell">abcdefghijklmnop</div></div>"#,
        );
        let laid = layout_dom(&tree, (500.0, 1000.0));
        let width = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()].width;

        assert_eq!(width("normal"), 125.0);
        assert_eq!(width("break-word"), 125.0);
        assert_eq!(width("anywhere"), 14.0);
        assert_eq!(width("break-all"), 14.0);
    }

    #[cfg(feature = "paint")]
    #[test]
    fn inline_descendant_break_policies_stay_at_their_style_boundaries() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                .fixed { width:80px;font:16px/20px "Liberation Sans" }
                .normal { word-break:normal;overflow-wrap:normal }
                .break-all { word-break:break-all }
                .anywhere { overflow-wrap:anywhere }
            </style>
            <div id="child-break-all" class="fixed normal"><span class="break-all">abcdefghijklmnop</span></div>
            <div id="child-normal" class="fixed break-all"><span class="normal">abcdefghijklmnop</span></div>
            <div id="child-anywhere" class="fixed normal"><span class="anywhere">abcdefghijklmnop</span></div>"#,
        );
        let laid = layout_dom(&tree, (500.0, 300.0));
        let height = |id| laid.rects[&tree.get_element_by_id(id).unwrap()].height;

        assert_eq!(height("child-break-all"), 40.0);
        assert_eq!(height("child-normal"), 20.0);
        assert_eq!(height("child-anywhere"), 40.0);
    }

    #[cfg(feature = "paint")]
    #[test]
    fn break_all_keeps_uax14_punctuation_pairs_in_min_content() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                .grid { display:grid;grid-template-columns:min-content;font:16px/20px "Liberation Sans" }
                #break-all { word-break:break-all }
                #anywhere { overflow-wrap:anywhere }
            </style>
            <div class="grid"><span id="break-all">(ab)</span></div>
            <div class="grid"><span id="anywhere">(ab)</span></div>"#,
        );
        let laid = layout_dom(&tree, (500.0, 300.0));
        let rect = |id| laid.rects[&tree.get_element_by_id(id).unwrap()];

        assert!(rect("break-all").width > rect("anywhere").width);
        assert_eq!(rect("break-all").height, 40.0);
        assert_eq!(rect("anywhere").height, 80.0);
    }

    #[cfg(feature = "paint")]
    #[test]
    fn break_all_uses_blink_punctuation_pair_semantics() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                .grid { display:grid;grid-template-columns:min-content;font:20px/24px monospace }
                .break { word-break:break-all }
            </style>
            <div class="grid"><span id="close-normal">)a</span></div>
            <div class="grid"><span id="close-break" class="break">)a</span></div>
            <div class="grid"><span id="closing-normal">a)</span></div>
            <div class="grid"><span id="closing-break" class="break">a)</span></div>
            <div class="grid"><span id="slash-normal">a/b</span></div>
            <div class="grid"><span id="slash-break" class="break">a/b</span></div>
            <div class="grid"><span id="hyphen-normal">a-b</span></div>
            <div class="grid"><span id="hyphen-break" class="break">a-b</span></div>
            <div class="grid"><span id="bar-normal">a|b</span></div>
            <div class="grid"><span id="bar-break" class="break">a|b</span></div>
            <div class="grid"><span id="plus-normal">a+b</span></div>
            <div class="grid"><span id="plus-break" class="break">a+b</span></div>
            <div class="grid"><span id="prefix-normal">$a</span></div>
            <div class="grid"><span id="prefix-break" class="break">$a</span></div>
            <div class="grid"><span id="postfix-normal">a%</span></div>
            <div class="grid"><span id="postfix-break" class="break">a%</span></div>"#,
        );
        let laid = layout_dom(&tree, (500.0, 1000.0));
        let width = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()].width;

        for stem in ["close", "slash", "hyphen", "bar", "plus"] {
            assert!(
                width(&format!("{stem}-break")) < width(&format!("{stem}-normal")),
                "break-all should expose a narrower opportunity for {stem}"
            );
        }
        for stem in ["closing", "prefix", "postfix"] {
            assert_eq!(
                width(&format!("{stem}-break")),
                width(&format!("{stem}-normal")),
                "break-all must retain the prohibited boundary for {stem}"
            );
        }
    }

    #[cfg(feature = "paint")]
    #[test]
    fn keep_all_suppresses_mixed_cjk_latin_word_boundaries() {
        let tree = parse_html(
            r#"<style>
                html,body { margin:0 }
                .grid { display:grid;grid-template-columns:min-content;font:16px/20px "Liberation Sans" }
                #keep { word-break:keep-all }
            </style>
            <div class="grid"><span id="normal">標（準）萬ab國.碼</span></div>
            <div class="grid"><span id="keep">標（準）萬ab國.碼</span></div>"#,
        );
        let laid = layout_dom(&tree, (500.0, 500.0));
        let width = |id| laid.rects[&tree.get_element_by_id(id).unwrap()].width;

        assert!(width("keep") > width("normal"));
    }

    #[test]
    fn textarea_intrinsic_box_comes_from_rows_and_cols() {
        // #685: an empty textarea must keep a real control box instead of
        // laying out as a plain block. Chromium calibrates cols=20/rows=2 to
        // a 168x36 border box with one 15px control line per row.
        let tree = parse_html(
            r#"<style>html, body { margin: 0 }</style>
            <div><textarea id="plain"></textarea></div>
            <div><textarea id="rows8" rows="8"></textarea></div>
            <div><textarea id="cssheight" style="height: 36px"></textarea></div>
            <div><textarea id="invalid-rows" rows="0"></textarea></div>"#,
        );
        let laid = layout_dom(&tree, (1280.0, 720.0));
        let rect = |id: &str| laid.rects[&tree.get_element_by_id(id).unwrap()];

        let plain = rect("plain");
        assert!((plain.width - 168.0).abs() < 0.5, "{}", plain.width);
        assert!((plain.height - 36.0).abs() < 0.5, "{}", plain.height);

        let rows8 = rect("rows8");
        assert!((rows8.width - 168.0).abs() < 0.5);
        assert!((rows8.height - 126.0).abs() < 0.5, "{}", rows8.height);

        // Author height wins over the rows-derived intrinsic height, and the
        // control is border-box, so 36px stays the border-box height.
        let cssheight = rect("cssheight");
        assert!(
            (cssheight.height - 36.0).abs() < 0.5,
            "{}",
            cssheight.height
        );

        // rows/cols are limited to positive numbers; anything else falls
        // back to the HTML defaults (rows=2).
        let invalid = rect("invalid-rows");
        assert!((invalid.height - 36.0).abs() < 0.5, "{}", invalid.height);

        // The control is an atomic inline-block, not a stretched block.
        let style = &laid.styles[&tree.get_element_by_id("plain").unwrap()];
        assert_eq!(style.display, crate::Display::Inline);
        assert!(style.is_inline_block);
    }
}
