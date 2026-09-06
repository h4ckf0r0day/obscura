//! obscura-render: the optional scoped render layer for Obscura.
//!
//! Obscura's default build has no layout or paint engine, which is the source
//! of its speed and low memory. This crate adds a render layer behind a feature
//! flag: real CSS box geometry (so getBoundingClientRect, elementFromPoint, and
//! IntersectionObserver return true values) and, with the paint feature,
//! rasterized PNG screenshots.
//!
//! Phase 1 (this file): the layout core. A `LayoutNode` tree plus a viewport is
//! laid out with taffy and the resulting border-box geometry is returned per
//! node. Later phases build the `LayoutNode` tree from the live DOM + computed
//! styles, feed geometry back to JS, and add text + paint.

use taffy::prelude::*;

pub mod css;
pub use css::{CssMediaType, Stylesheet, StylesheetCache};

pub mod style;
pub use style::compute_style;

pub mod border;
pub use border::{
    BorderModel, BorderRadii, BorderStyle, CornerRadius, OutlineModel, RadiusValue,
    ResolvedBorderRadii, Sides,
};

pub mod dom;
pub use dom::{
    layout_dom, layout_dom_with_images, layout_dom_with_resources, AttributeStyleMutation,
    DomLayout, RetainedStyleMutation, StickyLayout, TreeStyleMutation,
};

/// Whether an image MIME type names a format supported by the renderer build.
///
/// Compare the MIME essence case-insensitively and ignore parameters. HTML
/// allows a `<source type>` value to include parameters, and HTTP MIME types
/// are ASCII case-insensitive. Keep this list aligned with the `image` crate
/// features in `Cargo.toml` plus the SVG path in `paint.rs`.
pub(crate) fn source_type_supported(value: &str) -> bool {
    let essence = value
        .split_once(';')
        .map_or(value, |(essence, _)| essence)
        .trim();
    [
        "image/apng",
        "image/bmp",
        "image/gif",
        "image/jpeg",
        "image/jpg",
        "image/png",
        "image/svg+xml",
        "image/vnd.microsoft.icon",
        "image/webp",
        "image/x-icon",
    ]
    .iter()
    .any(|supported| essence.eq_ignore_ascii_case(supported))
}

#[cfg(test)]
mod image_capability_tests {
    use super::source_type_supported;

    #[test]
    fn image_mime_filter_uses_case_insensitive_essence_and_parameters() {
        for supported in [
            "image/apng",
            "image/bmp",
            "image/gif",
            "image/jpeg",
            "image/jpg",
            "image/png",
            "image/svg+xml",
            "image/vnd.microsoft.icon",
            "image/webp",
            "image/x-icon",
            " IMAGE/WEBP ; codecs=lossless ",
            "Image/Svg+Xml;charset=utf-8",
        ] {
            assert!(source_type_supported(supported), "{supported}");
        }
        for unsupported in [
            "",
            "image/avif",
            "image/jxl",
            "image/png,image/webp",
            "text/html",
            ";image/png",
        ] {
            assert!(!source_type_supported(unsupported), "{unsupported}");
        }
    }
}

#[cfg(feature = "paint")]
mod paint;
#[cfg(feature = "paint")]
pub use paint::{
    image_intrinsic_dimensions, paint_dom, paint_dom_scrolled,
    paint_dom_scrolled_at_animation_time,
    paint_dom_scrolled_at_animation_time_with_surface_color_and_resources,
    paint_dom_scrolled_at_animation_time_with_surface_color, paint_prepared,
    paint_prepared_region_with_scroll, paint_prepared_region_with_scroll_and_surface_color,
    paint_prepared_region_with_scroll_and_surface_color_and_canvas_surfaces,
    paint_prepared_with_scroll, paint_prepared_with_scroll_and_surface_color,
    paint_prepared_with_scroll_and_surface_color_and_canvas_surfaces, prepare_dom,
    prepare_dom_at_animation_time,
    prepare_dom_with_dynamic_fonts, prepare_dom_with_dynamic_fonts_at_animation_time,
    prepare_dom_with_dynamic_fonts_and_stylesheet_cache,
    prepare_dom_with_dynamic_fonts_and_stylesheet_cache_at_animation_time,
    prepare_dom_with_dynamic_fonts_and_stylesheet_cache_for_media_with_animation_state,
    prepare_dom_with_dynamic_fonts_and_stylesheet_cache_with_animation_state,
    screenshot_png, screenshot_png_scrolled, screenshot_png_scrolled_at_animation_time,
    screenshot_png_scrolled_at_animation_time_with_surface_color_and_resources,
    screenshot_png_scrolled_at_animation_time_with_surface_color,
    prepare_dom_with_retained_attribute_styles, prepare_dom_with_retained_styles,
    prepare_dom_with_retained_styles_at_animation_time,
    prepare_dom_with_retained_styles_with_animation_state,
    screenshot_prepared,
    screenshot_prepared_region_with_scroll,
    screenshot_prepared_region_with_scroll_and_surface_color,
    screenshot_prepared_region_with_scroll_and_surface_color_and_canvas_surfaces,
    screenshot_prepared_region_with_scroll_and_backgrounds,
    screenshot_prepared_region_with_scroll_and_backgrounds_and_canvas_surfaces,
    screenshot_prepared_with_scroll,
    screenshot_prepared_with_scroll_and_surface_color,
    screenshot_prepared_with_scroll_and_surface_color_and_canvas_surfaces,
    validate_capture_region, CaptureError, CaptureRegion, DynamicFontFace, ElementScrollMetrics,
    CanvasSurface, CanvasSurfaceSource, ImageRequestProfile, PreparedRender, RenderResourceCache, RenderResourceLoader,
    ResolvedScrollState, SelectedImage,
    MAX_CAPTURE_DIMENSION, MAX_CAPTURE_PIXELS,
};

// Real inline text layout (cosmic-text) lives behind the paint feature; the
// layout-only build keeps the lighter word-split geometry. The stub lets
// `dom.rs` name `inline::TextEngine` and call `try_build` unconditionally.
#[cfg(feature = "paint")]
pub mod inline;

#[cfg(not(feature = "paint"))]
pub mod inline {
    use obscura_dom::tree::{DomTree, NodeId};
    use std::collections::HashMap;

    #[derive(Clone)]
    pub(crate) struct WebFont {
        pub data: Vec<u8>,
        pub family: Option<String>,
        pub weight: Option<(u16, u16)>,
        pub italic: Option<bool>,
    }

    #[derive(Default)]
    pub struct TextEngine;

    pub(crate) fn text_may_need_emoji_font(_text: &str) -> bool {
        false
    }

    impl TextEngine {
        pub fn new() -> Self {
            TextEngine
        }

        pub(crate) fn new_with_web_fonts(_fonts: &[WebFont]) -> Self {
            TextEngine
        }

        pub(crate) fn new_with_web_fonts_and_emoji(_fonts: &[WebFont], _load_emoji: bool) -> Self {
            TextEngine
        }

        pub fn register_replaced(
            &mut self,
            _width: f32,
            _height: f32,
            _style: &crate::LayoutStyle,
        ) -> usize {
            0
        }

        pub(crate) fn register_replaced_intrinsic(
            &mut self,
            _intrinsic: crate::ReplacedIntrinsic,
            _style: &crate::LayoutStyle,
        ) -> usize {
            0
        }
        /// Layout-only builds have no shaper, so no container is ever treated
        /// as a cosmic-text inline formatting context: the word-split path
        /// handles text geometry for `getBoundingClientRect`.
        pub fn try_build(
            &mut self,
            _tree: &DomTree,
            _id: NodeId,
            _styles: &HashMap<NodeId, crate::LayoutStyle>,
        ) -> Option<usize> {
            None
        }
        /// See `try_build`: inline runs likewise fall back to word-split
        /// geometry in layout-only builds.
        pub fn try_build_run(
            &mut self,
            _tree: &DomTree,
            _parent: NodeId,
            _run: &[NodeId],
            _styles: &HashMap<NodeId, crate::LayoutStyle>,
        ) -> Option<usize> {
            None
        }

        /// Layout-only builds do not load page fonts. Preserve the same
        /// line-vs-fragment contract with deterministic bundled metrics.
        pub(crate) fn inline_font_box_height(&self, style: &crate::LayoutStyle) -> f32 {
            style.font_size.unwrap_or(16.0)
        }

        pub(crate) fn selected_line_height(&self, style: &crate::LayoutStyle) -> f32 {
            match style.line_height.unwrap_or(crate::LineHeight::Normal) {
                crate::LineHeight::Normal => style.font_size.unwrap_or(16.0) * 1.2,
                crate::LineHeight::Ratio(value) => style.font_size.unwrap_or(16.0) * value,
                crate::LineHeight::Px(value) => value,
                crate::LineHeight::Relative(crate::Dimension::Percent(value)) => {
                    style.font_size.unwrap_or(16.0) * value
                }
                crate::LineHeight::Relative(crate::Dimension::Px(value)) => value,
                crate::LineHeight::Relative(_) => style.font_size.unwrap_or(16.0),
            }
        }

        pub(crate) fn push_generated_text(
            &mut self,
            _text: &str,
            _style: &crate::LayoutStyle,
        ) -> Option<usize> {
            None
        }

        pub(crate) fn measure_word(&mut self, _idx: usize) -> (f32, f32) {
            (0.0, 0.0)
        }
    }

    pub(crate) fn used_line_height(style: &crate::LayoutStyle) -> f32 {
        TextEngine.selected_line_height(style)
    }

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

    pub(crate) fn has_replaced_sizing(local: &str) -> bool {
        matches!(
            local,
            "img"
                | "canvas"
                | "video"
                | "audio"
                | "iframe"
                | "embed"
                | "object"
                | "progress"
                | "meter"
        )
    }

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

    pub(crate) fn constrained_auto_replaced_size(
        width: f32,
        height: f32,
        style: &crate::LayoutStyle,
    ) -> taffy::Size<f32> {
        let px = |dimension| match dimension {
            crate::Dimension::Px(value) => Some(value.max(0.0)),
            _ => None,
        };
        let ratio = style
            .aspect_ratio
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
            .unwrap_or_else(|| {
                if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
                    width / height
                } else {
                    2.0
                }
            });
        let preferred_width = px(style.width);
        let preferred_height = px(style.height);
        let min_width = px(style.min_width).unwrap_or(0.0);
        let min_height = px(style.min_height).unwrap_or(0.0);
        let max_width = px(style.max_width)
            .unwrap_or(f32::INFINITY)
            .max(min_width);
        let max_height = px(style.max_height)
            .unwrap_or(f32::INFINITY)
            .max(min_height);

        let (width, height) = match (preferred_width, preferred_height) {
            (Some(width), Some(height)) => (
                width.min(max_width).max(min_width),
                height.min(max_height).max(min_height),
            ),
            (Some(width), None) => {
                let width = width.min(max_width).max(min_width);
                (width, (width / ratio).min(max_height).max(min_height))
            }
            (None, Some(height)) => {
                let height = height.min(max_height).max(min_height);
                ((height * ratio).min(max_width).max(min_width), height)
            }
            (None, None) => {
                let height_at_max_width = (max_width / ratio).max(min_height);
                let height_at_min_width = (min_width / ratio).min(max_height);
                let width_at_max_height = (max_height * ratio).max(min_width);
                let width_at_min_height = (min_height * ratio).min(max_width);
                if width > max_width {
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
                }
            }
        };

        taffy::Size { width, height }
    }
}

/// An axis-aligned rectangle in CSS pixels, relative to the containing block.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// One two-dimensional affine transform in CSS pixel coordinates.
///
/// The six components use the CSS `matrix(a,b,c,d,e,f)` convention:
/// `x' = a*x + c*y + e`, `y' = b*x + d*y + f`. Keeping this renderer-owned
/// type independent of the raster backend lets layout geometry, scrolling
/// overflow, CSSOM, and paint consume exactly the same resolved transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2 {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Default for Affine2 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine2 {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Compose `self(other(point))`.
    pub fn then(self, other: Self) -> Self {
        Self {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    pub fn translate(x: f32, y: f32) -> Self {
        Self {
            e: x,
            f: y,
            ..Self::IDENTITY
        }
    }

    pub fn scale(x: f32, y: f32) -> Self {
        Self {
            a: x,
            d: y,
            ..Self::IDENTITY
        }
    }

    pub fn rotate(degrees: f32) -> Self {
        let (sin, cos) = degrees.to_radians().sin_cos();
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            e: 0.0,
            f: 0.0,
        }
    }

    pub fn skew(x_degrees: f32, y_degrees: f32) -> Self {
        Self {
            a: 1.0,
            b: y_degrees.to_radians().tan(),
            c: x_degrees.to_radians().tan(),
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    pub fn around(self, origin: (f32, f32)) -> Self {
        Self::translate(origin.0, origin.1)
            .then(self)
            .then(Self::translate(-origin.0, -origin.1))
    }

    pub fn map_point(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    pub fn map_rect(self, rect: Rect) -> Rect {
        let points = [
            self.map_point(rect.x, rect.y),
            self.map_point(rect.x + rect.width, rect.y),
            self.map_point(rect.x, rect.y + rect.height),
            self.map_point(rect.x + rect.width, rect.y + rect.height),
        ];
        let left = points
            .iter()
            .map(|point| point.0)
            .fold(f32::INFINITY, f32::min);
        let top = points
            .iter()
            .map(|point| point.1)
            .fold(f32::INFINITY, f32::min);
        let right = points
            .iter()
            .map(|point| point.0)
            .fold(f32::NEG_INFINITY, f32::max);
        let bottom = points
            .iter()
            .map(|point| point.1)
            .fold(f32::NEG_INFINITY, f32::max);
        Rect {
            x: left,
            y: top,
            width: (right - left).max(0.0),
            height: (bottom - top).max(0.0),
        }
    }

    pub fn inverse(self) -> Option<Self> {
        let determinant = self.a * self.d - self.b * self.c;
        if !determinant.is_finite() || determinant.abs() < 1.0e-8 {
            return None;
        }
        let inverse = 1.0 / determinant;
        Some(Self {
            a: self.d * inverse,
            b: -self.b * inverse,
            c: -self.c * inverse,
            d: self.a * inverse,
            e: (self.c * self.f - self.d * self.e) * inverse,
            f: (self.b * self.e - self.a * self.f) * inverse,
        })
    }

    pub fn is_identity(self) -> bool {
        (self.a - 1.0).abs() < f32::EPSILON
            && self.b.abs() < f32::EPSILON
            && self.c.abs() < f32::EPSILON
            && (self.d - 1.0).abs() < f32::EPSILON
            && self.e.abs() < f32::EPSILON
            && self.f.abs() < f32::EPSILON
    }

    pub fn is_translation(self) -> bool {
        (self.a - 1.0).abs() < f32::EPSILON
            && self.b.abs() < f32::EPSILON
            && self.c.abs() < f32::EPSILON
            && (self.d - 1.0).abs() < f32::EPSILON
    }
}

#[derive(Clone, Debug)]
pub struct TransformLength {
    pub value: Dimension,
    pub expression: Option<String>,
}

impl TransformLength {
    pub fn px(value: f32) -> Self {
        Self {
            value: Dimension::Px(value),
            expression: None,
        }
    }
}

/// One authored operation in a `transform` function list. Source order is
/// significant and is intentionally retained until the final reference box
/// resolves percentage translations.
#[derive(Clone, Debug)]
pub enum TransformOp {
    Translate(TransformLength, TransformLength),
    Scale(f32, f32),
    Rotate(f32),
    Skew(f32, f32),
    Matrix(Affine2),
}

impl Rect {
    /// The overlap of two rects, or `None` if they do not intersect (or the
    /// overlap is degenerate). Used to accumulate an ancestor clip chain for
    /// `overflow: hidden`.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.width).min(other.x + other.width);
        let y1 = (self.y + self.height).min(other.y + other.height);
        if x1 > x0 && y1 > y0 {
            Some(Rect {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            })
        } else {
            None
        }
    }

    /// The smallest rect covering both. Used to derive a table row/section box
    /// from its cells, since `<tr>`/`<tbody>` are not laid out as taffy boxes.
    pub fn union(&self, other: &Rect) -> Rect {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.width).max(other.x + other.width);
        let y1 = (self.y + self.height).max(other.y + other.height);
        Rect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }
}

/// Snap a CSS scrolling value to the raster device-pixel grid.
///
/// Captures currently use one device pixel per CSS pixel, but keeping the
/// scale explicit makes the same rule usable when page-level device scale is
/// introduced. CSSOM integer extents and effective offsets must share this
/// quantization or a fractional range can move geometry without moving a
/// screenshot pixel.
#[doc(hidden)]
pub fn quantize_scroll_value(value: f32, device_scale_factor: f32) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    let scale = if device_scale_factor.is_finite() && device_scale_factor > 0.0 {
        device_scale_factor
    } else {
        1.0
    };
    (value * scale).round() / scale
}

pub(crate) fn quantized_scroll_range(content: f32, client: f32, device_scale_factor: f32) -> f32 {
    (quantize_scroll_value(content, device_scale_factor)
        - quantize_scroll_value(client, device_scale_factor))
    .max(0.0)
}

/// Per-edge box values (margin / padding / border) in CSS pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

/// The display modes obscura-render cares about for phase 1. Inline text layout
/// arrives with the text/paint phase and is folded in then.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Display {
    #[default]
    Block,
    Flex,
    Grid,
    Inline,
    #[allow(dead_code)]
    None,
}

/// Computed `container-type` value. Stage A carries it through the cascade;
/// the layout convergence stage will use it for query eligibility.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ContainerType {
    #[default]
    Normal,
    InlineSize,
    Size,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Dimension {
    #[default]
    Auto,
    Px(f32),
    /// 0.0-1.0 fraction of the containing block.
    Percent(f32),
    /// Font-relative and viewport-relative units, kept unresolved at parse
    /// time (the element font-size and viewport are not known then) and
    /// resolved to `Px` during `dom::layout_dom`'s top-down pass via
    /// [`Dimension::resolve`]. Resolving font/viewport units against a
    /// hardcoded 16px at
    /// parse time (the old behavior) silently corrupted every relative length.
    Em(f32),
    Ex(f32),
    Rem(f32),
    Vw(f32),
    Vh(f32),
    Vmin(f32),
    Vmax(f32),
}

impl Dimension {
    /// Resolve font/viewport-relative units to `Px`. `em_px` is the element's
    /// own font-size, `rem_px` the root's, and `vw`/`vh` are one hundredth of
    /// the viewport width/height. `Px`, `Percent`, and `Auto` pass through
    /// (`Percent` stays for taffy to resolve against the containing block).
    pub fn resolve(self, em_px: f32, rem_px: f32, vw: f32, vh: f32) -> Dimension {
        match self {
            Dimension::Em(v) => Dimension::Px(v * em_px),
            // Liberation Sans is the deterministic generic sans face used by
            // the renderer. This is its x-height as a fraction of the em,
            // matching Chromium's generic sans face on the capture host.
            Dimension::Ex(v) => Dimension::Px(v * em_px * 0.528_320_3),
            Dimension::Rem(v) => Dimension::Px(v * rem_px),
            Dimension::Vw(v) => Dimension::Px(v * vw),
            Dimension::Vh(v) => Dimension::Px(v * vh),
            Dimension::Vmin(v) => Dimension::Px(v * vw.min(vh)),
            Dimension::Vmax(v) => Dimension::Px(v * vw.max(vh)),
            other => other,
        }
    }
}

/// Computed CSS `font-optical-sizing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontOpticalSizing {
    /// Let a variable font's `opsz` axis follow the computed font size.
    #[default]
    Auto,
    /// Leave the font's optical-size axis at its normal/default setting.
    None,
}

/// One canonical CSS `font-variation-settings` axis assignment.
///
/// The CSS parser guarantees a printable four-byte ASCII tag and a finite
/// value. Settings are stored in tag order with duplicate tags collapsed so
/// shaping and rasterization can consume one deterministic axis tuple.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontVariationSetting {
    pub tag: [u8; 4],
    pub value: f32,
}

/// One axis of a CSS `background-position`.
///
/// CSS positions combine an absolute offset with a percentage of the space
/// left after sizing the background image. Keeping both terms is necessary
/// for sprite sheets: `-24px` is an offset from the start edge, while
/// `right 10px` is equivalent to `100% - 10px`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BackgroundPositionAxis {
    length: f32,
    percentage: f32,
}

impl BackgroundPositionAxis {
    pub const fn pixels(length: f32) -> Self {
        Self {
            length,
            percentage: 0.0,
        }
    }

    pub const fn percentage(percentage: f32) -> Self {
        Self {
            length: 0.0,
            percentage,
        }
    }

    pub const fn length_percentage(length: f32, percentage: f32) -> Self {
        Self { length, percentage }
    }

    pub const fn from_end_offset(offset: Self) -> Self {
        Self {
            length: -offset.length,
            percentage: 1.0 - offset.percentage,
        }
    }

    pub fn resolve(self, leftover_space: f32) -> f32 {
        self.length + self.percentage * leftover_space
    }

    pub(crate) fn interpolate(self, other: Self, position: f32) -> Self {
        Self {
            length: self.length + (other.length - self.length) * position,
            percentage: self.percentage + (other.percentage - self.percentage) * position,
        }
    }
}

/// The two axes of CSS `background-position`.
///
/// The derived default is the CSS initial value `0% 0%`. A one-value
/// longhand such as `background-position: 0` is parsed as `0 center`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BackgroundPosition {
    pub x: BackgroundPositionAxis,
    pub y: BackgroundPositionAxis,
}

/// Box used to establish a background layer's positioning area.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BackgroundOrigin {
    BorderBox,
    #[default]
    PaddingBox,
    ContentBox,
}

/// Box (or glyph shape) that limits background painting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BackgroundClip {
    #[default]
    BorderBox,
    PaddingBox,
    ContentBox,
    Text,
}

impl BackgroundPosition {
    pub const fn new(x: BackgroundPositionAxis, y: BackgroundPositionAxis) -> Self {
        Self { x, y }
    }

    pub(crate) fn interpolate(self, other: Self, position: f32) -> Self {
        Self {
            x: self.x.interpolate(other.x, position),
            y: self.y.interpolate(other.y, position),
        }
    }
}

/// Fill rule for a CSS `clip-path: polygon(...)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ClipPathFillRule {
    #[default]
    Nonzero,
    Evenodd,
}

/// The supported CSS basic-shape clip.
///
/// Coordinates retain their CSS length/percentage unit until paint, where
/// percentages resolve independently against the border box width and height.
/// This mirrors the reference-box resolution used by browser engines and
/// avoids baking responsive polygon geometry into computed style.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipPathPolygon {
    pub fill_rule: ClipPathFillRule,
    pub points: Vec<(Dimension, Dimension)>,
}

/// One CSS gradient from the ordered `background-image` layer list.
///
/// CSS paints the first authored layer closest to the user, so paint walks
/// this vector in reverse over the background color. Keeping the authored
/// order is essential when a hero combines a linear fade with several
/// partially transparent radial highlights.
#[derive(Debug, Clone, PartialEq)]
pub enum BackgroundGradientLayer {
    Linear {
        angle: f32,
        stops: Vec<([u8; 4], Option<f32>)>,
        /// Authored stop positions, retained until paint so absolute lengths
        /// can resolve against the final gradient-line length.
        stop_positions: Vec<Option<String>>,
        /// `repeating-linear-gradient()` repeats the interval from its first
        /// resolved stop through its last, independently of background tiling.
        repeating: bool,
    },
    Radial {
        center: (f32, f32),
        stops: Vec<([u8; 4], Option<f32>)>,
    },
    Conic {
        angle: f32,
        center: (f32, f32),
        stops: Vec<([u8; 4], Option<f32>)>,
    },
}

/// Parsed ending-shape geometry for one CSS radial gradient.
///
/// This is kept alongside `BackgroundGradientLayer` rather than adding fields
/// to its public `Radial` variant, preserving the existing construction API for
/// embedders while allowing authored circle/ellipse sizing to survive to paint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RadialGradientGeometry {
    pub shape: RadialGradientShape,
    pub size: RadialGradientSize,
}

impl Default for RadialGradientGeometry {
    fn default() -> Self {
        Self {
            shape: RadialGradientShape::Ellipse,
            size: RadialGradientSize::FarthestCorner,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RadialGradientShape {
    Circle,
    Ellipse,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RadialGradientSize {
    ClosestSide,
    ClosestCorner,
    FarthestSide,
    FarthestCorner,
    /// Horizontal and vertical radii. Percentages resolve against the
    /// corresponding axis of the gradient box, per CSS Images.
    Explicit(Dimension, Dimension),
}

/// One computed CSS counter operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterDirective {
    pub name: String,
    pub value: i32,
}

/// Counter styles supported in generated text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GeneratedCounterStyle {
    #[default]
    Decimal,
    DecimalLeadingZero,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
}

/// One item in a computed `content` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratedContentItem {
    Text(String),
    Counter {
        name: String,
        style: GeneratedCounterStyle,
    },
    Counters {
        name: String,
        separator: String,
        style: GeneratedCounterStyle,
    },
}

/// Sparse retained provenance for direct transform-only WAAPI sampling.
/// Kept behind one pointer so ordinary, non-animated elements do not pay for
/// another `Vec` in every `LayoutStyle`.
#[derive(Debug, Clone)]
pub(crate) struct WaapiSampleState {
    pub underlying_transform_ops: Vec<TransformOp>,
    pub underlying_opacity: Option<f32>,
    pub transform_fast_path: bool,
    pub opacity_fast_path: bool,
}

/// The subset of CSS that influences box layout. Expanded in later phases.
#[derive(Debug, Clone, Default)]
pub struct LayoutStyle {
    pub display: Display,
    /// Computed inline base direction. `None` is the inherited specified
    /// state before the DOM top-down pass resolves it.
    pub direction: Option<taffy::Direction>,
    /// The specified `display` value was the CSS-wide `inherit` keyword.
    ///
    /// `display` is normally non-inherited, so the cascade cannot resolve this
    /// until the parent's computed outer/inner display is known. The DOM
    /// top-down pass copies that provenance and then clears this marker.
    pub(crate) display_inherit: bool,
    /// Original legacy flexbox display provenance. `Some(false)` is
    /// `-webkit-box`; `Some(true)` is `-webkit-inline-box`. A vertical legacy
    /// box with an active line clamp computes to flow-root/inline-block, but
    /// retaining the specified display makes that adjustment independent of
    /// declaration order and keeps unclamped CSSOM serialization honest.
    pub(crate) webkit_box_display: Option<bool>,
    /// Computed legacy box orientation. Only the vertical/block-axis form
    /// activates the legacy WebKit line-clamp contract.
    pub(crate) webkit_box_orient_vertical: bool,
    /// Computed CSS `container-type` (not inherited).
    pub container_type: ContainerType,
    /// The specified value was the CSS-wide `inherit` keyword. Resolved
    /// top-down before layout because `container-type` is otherwise
    /// non-inherited.
    pub(crate) container_type_inherit: bool,
    /// Computed CSS `container-name`; empty represents `none` (not inherited).
    pub container_names: Vec<String>,
    /// The specified `container-name` value was CSS-wide `inherit`.
    pub(crate) container_names_inherit: bool,
    /// True when `display:flex` is only an internal stand-in for native HTML
    /// layout such as table cells, rather than the computed CSS display.
    /// Descendants are not CSS flex items in these containers.
    pub internal_flex_container: bool,
    /// The computed inner display is `table`.
    ///
    /// Taffy has no table display mode, so authored table boxes are represented
    /// by block/grid-compatible layout styles. Keep this provenance for CSS
    /// features such as container-query axis availability that depend on the
    /// computed display rather than our internal layout approximation.
    pub(crate) is_table_box: bool,
    /// The computed `table-layout: fixed` value. The fixed algorithm is only
    /// activated when the table also has a definite inline size; otherwise
    /// CSS requires the automatic table layout algorithm.
    pub(crate) table_layout_fixed: bool,
    /// The computed inner display is `table-cell`.
    ///
    /// Authored internal table boxes need the same cell sizing path as native
    /// `<td>`/`<th>` elements even though Taffy represents their cell-content
    /// wrapper as an internal column flexbox.
    pub(crate) is_table_cell_box: bool,
    /// The HTML UA sheet's vendor `text-align` behavior for `<center>`.
    /// Unlike ordinary `text-align:center`, it also centers fixed-width block
    /// descendants while leaving auto-width blocks fill-available.
    pub legacy_center: bool,
    pub width: Dimension,
    /// The preferred inline size is the intrinsic `fit-content` keyword.
    ///
    /// Taffy's box-size dimension cannot represent intrinsic sizing keywords,
    /// so `width` remains `Auto` while the DOM layout convergence pass applies
    /// the CSS shrink-to-fit formula from min/max-content measurements.
    pub width_fit_content: bool,
    pub height: Dimension,
    /// Which box edge `width`/`height` and min/max sizes describe. CSS starts
    /// at `content-box`; many modern reset sheets opt into `border-box`.
    pub box_sizing: BoxSizing,
    /// Whether `width`/`height` was set by an author rule (including an explicit
    /// `auto`). Presentational `width`/`height` HTML attributes are a lower
    /// priority than author CSS, so they apply only when these are false; an
    /// explicit `width:auto` must still suppress a `width="408"` attribute so
    /// the element keeps its aspect-ratio size instead of the intrinsic one.
    pub width_set: bool,
    pub height_set: bool,
    pub min_width: Dimension,
    pub min_height: Dimension,
    pub max_width: Dimension,
    pub max_height: Dimension,
    /// Deferred CSS math expressions for width, height, min-width, min-height,
    /// max-width, and max-height. Functional lengths can depend on the actual
    /// viewport, containing block, or computed font size and cannot be safely
    /// collapsed to pixels during stylesheet parsing.
    pub size_expressions: [Option<String>; 6],
    /// `aspect-ratio` as width/height, or an image's intrinsic ratio resolved
    /// at layout. Lets a replaced element (or a padding-box card) derive the
    /// missing dimension from the given one, so a `width:100%` image gets a
    /// real height instead of collapsing to zero.
    pub aspect_ratio: Option<f32>,
    /// Whether the preferred ratio came from decoded intrinsic media and
    /// therefore applies to the content box regardless of `box-sizing`.
    pub aspect_ratio_is_intrinsic: bool,
    /// Fetched intrinsic CSS-pixel size for a replaced element. Kept alongside
    /// `aspect_ratio` so its taffy leaf can contribute a real min/max-content
    /// size when percentage dimensions are resolved through an auto-sized
    /// wrapper (`img { width:100%; height:auto }`).
    pub intrinsic_size: Option<(f32, f32)>,
    /// Per-axis decoded intrinsic metadata used by the replaced sizing path.
    /// SVG can expose only one dimension or a `viewBox` ratio, distinctions
    /// that the stable public `intrinsic_size` tuple cannot represent.
    pub(crate) replaced_intrinsic: Option<ReplacedIntrinsic>,
    /// Definite content-box width available to an auto/auto ratio-only
    /// replaced element in ordinary block flow. Taffy's flex-row stand-in for
    /// an inline formatting context asks atomic children for max-content first,
    /// so its measure callback otherwise never sees the definite line width
    /// that CSS replaced sizing uses for this special case.
    pub(crate) ratio_only_available_width: Option<f32>,
    /// The current ratio came from HTML width/height presentation hints
    /// (`<img>` or its selected `<picture><source>`), rather than authored
    /// CSS. A decoded image's natural ratio replaces this provisional ratio.
    pub aspect_ratio_is_mapped: bool,
    /// Whether this element generates a replaced box.
    ///
    /// Replaced elements with `display:inline` are atomic and therefore keep
    /// authored width/height/min/max sizing. Ordinary inline boxes compute
    /// those properties but ignore them for used layout. This provenance
    /// cannot be inferred from intrinsic dimensions because an unloaded or
    /// broken image remains replaced.
    pub(crate) is_replaced_box: bool,
    /// Whether this element uses the intrinsic replaced-element sizing
    /// algorithm. This is narrower than `is_replaced_box`: form controls such
    /// as buttons are atomic inline boxes but still use ordinary grid `normal`
    /// stretch behavior in Chromium and Gecko.
    pub(crate) has_replaced_sizing: bool,
    pub margin: Edges,
    /// Which margin sides are `auto` (top, right, bottom, left). `margin: 0
    /// auto` / `margin-inline: auto` centering needs a real Auto margin, which
    /// the f32 `margin` cannot express; this flag drives it at taffy mapping.
    pub margin_auto: [bool; 4],
    /// Percentage margin per side (top, right, bottom, left) as a 0..1 fraction,
    /// `None` when the side is a fixed length. Like padding, every side resolves
    /// against the containing block's WIDTH; the f32 `margin` cannot carry a
    /// percentage, so this is resolved to px during `dom::layout_dom`'s top-down
    /// pass once the containing-block width is known.
    pub margin_percent: [Option<f32>; 4],
    /// Font- and viewport-relative margin lengths (top, right, bottom, left).
    /// These retain their unit until the top-down pass knows the element font
    /// size, root font size, and viewport dimensions.
    pub margin_relative: [Option<Dimension>; 4],
    /// Deferred `calc()`/`min()`/`max()`/`clamp()` margin expressions.
    pub margin_expressions: [Option<String>; 4],
    pub padding: Edges,
    /// Percentage padding per side (top, right, bottom, left) as a 0..1
    /// fraction, `None` when the side is a fixed length. All four sides resolve
    /// against the containing block's WIDTH (per CSS, including top/bottom): this
    /// is the responsive aspect-ratio-box trick (`padding-top:56.25%` reserves a
    /// 16:9 area). The percentage remains typed through Taffy layout so flex/grid
    /// min/max sizing can establish the final containing-block width first.
    /// `dom::layout_dom` writes Taffy's resolved used pixels back into `padding`
    /// before paint and geometry consumers inspect the computed layout.
    pub padding_percent: [Option<f32>; 4],
    /// Font- and viewport-relative padding lengths (top, right, bottom, left),
    /// resolved alongside `margin_relative` during the top-down pass.
    pub padding_relative: [Option<Dimension>; 4],
    /// Deferred `calc()`/`min()`/`max()`/`clamp()` padding expressions.
    pub padding_expressions: [Option<String>; 4],
    pub border: Edges,
    /// Specified border state. `border` above is the derived used-width view
    /// consumed by layout; it is zero on `none`/`hidden` sides while this
    /// model retains their specified widths for later cascade declarations.
    pub border_model: BorderModel,
    /// Border state before the first cascaded physical/logical border
    /// declaration. Logical sides cannot be mapped until inherited direction
    /// is known, so the top-down pass replays `border_cascade_ops` over this
    /// snapshot in exact cascade order.
    pub(crate) border_cascade_base: Option<BorderModel>,
    pub(crate) border_cascade_ops: Vec<BorderCascadeOp>,
    /// Outline paint state. It deliberately has no counterpart in Taffy:
    /// outlines never contribute to box geometry.
    pub outline: OutlineModel,
    /// `clip-path: polygon(...)`, resolved against the final border box at
    /// paint time. `None` is the computed `none` value.
    pub clip_path: Option<ClipPathPolygon>,
    /// RGBA for the paint step. Parsed always (cheap), used only with `paint`.
    pub background_color: Option<[u8; 4]>,
    /// `linear-gradient(...)` background: (angle in degrees clockwise from 12
    /// o'clock per CSS, list of (rgba, optional 0..1 stop position)). Modern
    /// hero sections use gradients heavily; without this they paint white.
    pub background_gradient: Option<(f32, Vec<([u8; 4], Option<f32>)>)>,
    /// First `radial-gradient(...)` layer: center in box-relative fractions
    /// and color stops. It is painted below the first linear layer, matching
    /// the common `linear-gradient(...), radial-gradient(...)` hero pattern.
    pub background_radial_gradient: Option<((f32, f32), Vec<([u8; 4], Option<f32>)>)>,
    /// Geometry paired with `background_radial_gradient`. The legacy public
    /// tuple above remains unchanged for API compatibility.
    pub(crate) background_radial_gradient_geometry: Option<RadialGradientGeometry>,
    /// `conic-gradient(...)` background. The angle is the CSS `from` angle,
    /// the center is a fraction of the border box, and stops are normalized
    /// during paint. Conic gradients commonly provide the color source for a
    /// repeated SVG mask in modern hero artwork.
    pub background_conic_gradient: Option<(f32, (f32, f32), Vec<([u8; 4], Option<f32>)>)>,
    /// Every parsed gradient in authored background-layer order. The legacy
    /// single-kind fields above remain populated for mask/text fast paths.
    pub background_gradient_layers: Vec<BackgroundGradientLayer>,
    /// One entry per `background_gradient_layers` item. Radial entries carry
    /// their authored ending shape; non-radial entries are `None`.
    pub(crate) background_gradient_layer_radial_geometries: Vec<Option<RadialGradientGeometry>>,
    /// The first `url(...)` reference from `background`/`background-image`
    /// (gradients and repeat keywords in the same shorthand are ignored: we
    /// paint the referenced image, not the gradient layer).
    pub background_image: Option<String>,
    /// `background-size`, in px, when given as explicit length(s) (a bare
    /// `10px` applies to both axes, matching how small square icons are
    /// almost always sized).
    pub background_size: Option<(f32, f32)>,
    /// Raw one/two-axis `background-size` expression, retained for paint-time
    /// resolution against the final owner box (`calc(100% - 2rem) auto`).
    pub background_size_expression: Option<String>,
    /// Keyword `background-size` behavior. `None` is CSS `auto`, which uses
    /// the image's intrinsic dimensions rather than stretching it to the box.
    pub background_size_fit: Option<ObjectFit>,
    /// `background-position`, retained as a length-plus-percentage per axis.
    /// Percentages apply to the leftover space after resolving explicit,
    /// intrinsic, cover, or contain size.
    pub background_position: BackgroundPosition,
    /// Explicit `(repeat-x, repeat-y)` choice. `None` is the CSS initial
    /// `repeat` in both axes.
    pub background_repeat: Option<(bool, bool)>,
    /// Geometry longhands retained independently: gradients and images are
    /// authored in `background-origin`, while `background-clip` only limits
    /// which portion becomes visible.
    pub background_origin: BackgroundOrigin,
    pub background_clip: BackgroundClip,
    /// `background-clip: text` / `-webkit-background-clip: text`: the background
    /// paints only through the element's glyphs, not as a filled box. Combined
    /// with a transparent text color this is the common gradient-text technique
    /// (hero headings, buttons like astro.build's "Get Started"); without
    /// honoring it those labels paint invisible. Consumed in the text paint path
    /// (`inline::TextEngine` fills glyphs from the background; `paint` suppresses
    /// the box fill so the gradient does not paint as a rectangle).
    pub background_clip_text: bool,
    /// `mask-image`/`-webkit-mask-image: url(...)`: the ubiquitous "colored,
    /// scalable icon" pattern (an SVG shape used as a stencil, tinted by
    /// `background-color`/`color` instead of carrying its own colors). Without
    /// this, every such icon paints as a solid filled square.
    pub mask_image: Option<String>,
    /// Explicit `mask-size` / `-webkit-mask-size` in CSS px.
    pub mask_size: Option<(f32, f32)>,
    /// Explicit `(repeat-x, repeat-y)` choice. `None` retains the CSS default
    /// (`repeat` on both axes) when an explicit tile size exists, while
    /// preserving the legacy fill-box fallback for unsized icon masks.
    pub mask_repeat: Option<(bool, bool)>,
    /// Foreground (text) color for the paint step.
    pub color: Option<[u8; 4]>,
    /// Computed SVG presentation properties supplied by author CSS. Inline
    /// SVG is serialized into a standalone document for resvg; without
    /// carrying these values across that boundary, stylesheet-driven icons
    /// and text lose their fill/stroke and can become completely invisible.
    pub svg_fill: Option<String>,
    pub svg_stroke: Option<String>,
    pub svg_stroke_width: Option<String>,
    /// Compatibility mirror for uniform border colors. New code should use
    /// `border_model.colors`; this remains for programmatic LayoutStyle users.
    pub border_color: Option<[u8; 4]>,
    /// Used color scheme for CSS Color 5 `light-dark()`. The renderer's
    /// current user preference is light; an inherited `color-scheme: dark`
    /// subtree switches this to true, while `normal`, `light`, or a list that
    /// permits light keeps the light scheme.
    pub color_scheme_dark: bool,
    pub font_size: Option<f32>,
    /// `font-size` given in a font/viewport-relative unit, resolved to
    /// `font_size` (px) during the inheritance pass against the parent and
    /// root font-sizes. `None` when font-size was absolute or unset.
    pub font_size_raw: Option<Dimension>,
    /// Deferred functional `font-size` (`clamp()`, `min()`, `max()`, `calc()`).
    /// These expressions must see the live viewport and parent font size;
    /// eagerly treating `9vw` as the number 9 made responsive headings pin to
    /// the minimum arm of their clamp.
    pub font_size_expression: Option<String>,
    /// Computed `letter-spacing` in CSS pixels. This inherited property is
    /// resolved top-down because `em` is relative to the element's own
    /// computed font size while `rem` and viewport units need live context.
    pub letter_spacing: Option<f32>,
    /// Non-pixel `letter-spacing` retained until the inheritance pass.
    pub letter_spacing_raw: Option<Dimension>,
    /// Deferred functional `letter-spacing` (`calc()`, `min()`, `clamp()`).
    pub letter_spacing_expression: Option<String>,
    /// Whether the computed value came from a non-`normal` declaration.
    /// Keeping this provenance distinguishes an explicit zero from the
    /// `normal` initial value while resolving the inherited property.
    pub letter_spacing_non_normal: Option<bool>,
    /// Specified CSS font weight during cascade (`1..1000`, `bolder`, or
    /// `lighter`), normalized to its numeric computed value by the inheritance
    /// pass before layout and shaping.
    pub font_weight: Option<String>,
    /// The computed `font-family` list, lowercased. Inherited. The text engine
    /// resolves it to a bundled face (Liberation Sans/Serif/Mono) the way
    /// Chromium picks a generic family on this host.
    pub font_family: Option<String>,
    /// Computed inherited `font-optical-sizing`. `None` during cascade means
    /// inherit; the top-down pass resolves every element to `Some`.
    pub font_optical_sizing: Option<FontOpticalSizing>,
    /// Computed inherited `font-variation-settings`. `None` during cascade
    /// means inherit, while `Some(Vec::new())` is the `normal` initial value.
    pub font_variation_settings: Option<Vec<FontVariationSetting>>,
    /// Inherited `text-align`, represented with the matching horizontal
    /// alignment keywords. Kept separate from flex/grid `align-items`: using
    /// one field for both made `text-align:left` shrink-wrap flex children.
    pub text_align: Option<taffy::AlignItems>,
    /// Computed inherited `text-indent`. Font/viewport-relative lengths are
    /// resolved to pixels during the top-down inheritance pass; percentages
    /// remain typed until the final inline-formatting-context width is known.
    /// `None` during cascade means inherit, while the root resolves to the
    /// initial zero length.
    pub text_indent: Option<Dimension>,
    pub align_items: Option<taffy::AlignItems>,
    pub justify_items: Option<taffy::JustifyItems>,
    pub align_self: Option<taffy::AlignSelf>,
    pub justify_self: Option<taffy::JustifySelf>,
    pub align_content: Option<taffy::AlignContent>,
    pub flex_direction: Option<taffy::FlexDirection>,
    pub flex_wrap: Option<taffy::FlexWrap>,
    pub justify_content: Option<taffy::JustifyContent>,
    pub flex_grow: Option<f32>,
    pub flex_shrink: Option<f32>,
    /// Flex/grid item order. The formatting algorithm consumes children in
    /// order-modified document order, with source order breaking ties.
    pub order: i32,
    /// `flex-basis` (longhand, or the length in a `flex:` shorthand). `Auto`
    /// is the default. Fixed-basis sidebars/columns (`flex: 0 0 260px`)
    /// collapse to content width without it.
    pub flex_basis: Dimension,

    // CSS Grid. Tracks are stored as taffy sizing functions; `grid_areas` is the
    // parsed `grid-template-areas` matrix (one Vec per row, `.` for a null cell),
    // resolved to line placements on children in a later pass.
    pub grid_template_columns: Vec<taffy::GridTemplateComponent<String>>,
    pub grid_template_rows: Vec<taffy::GridTemplateComponent<String>>,
    /// Keeps the opaque `calc()` handles embedded in grid track sizing
    /// functions alive until every Taffy layout pass has completed.
    pub(crate) grid_calc_expressions:
        [Vec<std::sync::Arc<crate::style::GridCalcExpression>>; 4],
    /// Track sizing functions for columns created outside the explicit grid.
    /// An empty list is the CSS initial `auto` value: taffy supplies one
    /// automatic implicit track and cycles a non-empty authored list.
    pub grid_auto_columns: Vec<taffy::TrackSizingFunction>,
    /// Track sizing functions for rows created outside the explicit grid.
    pub grid_auto_rows: Vec<taffy::TrackSizingFunction>,
    /// Explicit CSS-wide `inherit` markers. The properties are normally
    /// non-inherited, so only the keyword copies the parent's computed list.
    pub(crate) grid_auto_columns_inherit: bool,
    pub(crate) grid_auto_rows_inherit: bool,
    /// `grid-template-columns: subgrid`. Taffy has no native subgrid track
    /// component, so `dom` resolves the safe full-span column subset after
    /// measuring the non-subgridded ancestor's intrinsic track contributions.
    pub grid_template_columns_subgrid: bool,
    pub grid_auto_flow: Option<taffy::GridAutoFlow>,
    pub grid_areas: Option<Vec<Vec<String>>>,
    pub grid_area_name: Option<String>,
    pub grid_column: Option<taffy::Line<taffy::GridPlacement>>,
    pub grid_row: Option<taffy::Line<taffy::GridPlacement>>,
    /// `[line-name]` -> 1-based grid line number, parsed from
    /// `grid-template-columns`/`-rows`. taffy has no native named-line support,
    /// so children placed by name (`grid-column: content-start / content-end`,
    /// widely used by the Guardian and other editorial grids) are resolved to
    /// numeric lines against these maps in `dom::resolve_grid_areas`.
    pub grid_col_line_names: Option<std::collections::HashMap<String, i16>>,
    pub grid_row_line_names: Option<std::collections::HashMap<String, i16>>,
    /// Raw `grid-column`/`grid-row` value when it references a named line (so it
    /// cannot be resolved to a `taffy::Line` until the parent's line-name map is
    /// known). Resolved in the same later pass; numeric/`span` values still fill
    /// `grid_column`/`grid_row` directly at cascade time.
    pub grid_column_raw: Option<String>,
    pub grid_row_raw: Option<String>,
    pub column_gap: Option<f32>,
    pub row_gap: Option<f32>,
    /// Deferred gap values. Font- and viewport-relative units cannot be
    /// converted until the element's computed font-size and the live viewport
    /// are known; eagerly treating `rem` as 16px breaks pages that customize
    /// the root font-size.
    pub column_gap_expression: Option<String>,
    pub row_gap_expression: Option<String>,

    // CSS Multi-column Layout. A count greater than one creates that many
    // equal-width fragmentainer columns in `dom::build`; the first layout
    // pass measures the in-flow child boxes at their real column width and a
    // bounded balancing pass then distributes them in column-major order.
    // `None` is the initial `auto` column count.
    pub column_count: Option<u16>,
    /// `break-inside: avoid` makes this box an atomic balancing unit. The
    /// current box-level multicol implementation cannot fragment the inside
    /// of a child yet, but retaining the computed value makes that limitation
    /// explicit and lets the balancing path distinguish authored break
    /// avoidance as finer-grained fragmentation is added.
    pub break_inside_avoid: bool,

    /// `border-spacing: <horizontal> <vertical>?` (or the `cellspacing`
    /// attribute). Only meaningful on a `<table>`; taffy has no native table
    /// display mode, so `dom::propagate_border_spacing` distributes this down
    /// as the table's own row gap and each descendant `<tr>`'s column gap.
    pub border_spacing: Option<(f32, f32)>,
    /// Computed `border-collapse`. This property is inherited; `None` means
    /// no value was specified on this node yet and is resolved top-down
    /// before table construction. The collapsed-border conflict/paint model
    /// is still approximate, but collapsed tables must at minimum contribute
    /// no border-spacing to their geometry.
    pub border_collapse: Option<bool>,

    // Positioning. `position: absolute|fixed` takes the box out of normal flow.
    pub position: Option<taffy::Position>,
    /// Distinguishes `fixed` from `absolute`; both map to taffy's absolute
    /// layout mode, but fixed boxes use the initial containing block.
    pub position_fixed: bool,
    /// Distinguishes `sticky` from ordinary relative positioning. Sticky boxes
    /// remain in normal flow; their insets constrain a scroll-time translation
    /// and therefore must not be applied as taffy relative offsets.
    pub position_sticky: bool,
    pub inset: [Option<Dimension>; 4], // top, right, bottom, left
    /// Deferred functional inset expressions in top/right/bottom/left order.
    pub inset_expressions: [Option<String>; 4],

    /// `overflow`/-x/-y other than `visible`: clips this element's descendants
    /// to its border box during paint. This is what makes the ubiquitous
    /// "visually-hidden but accessible" pattern (a 1x1 absolutely-positioned,
    /// clipped box used for skip-links and screen-reader-only labels) actually
    /// invisible instead of painting its text wherever it lands.
    pub overflow_hidden: bool,
    /// Independent computed overflow clips. `overflow_hidden` remains the
    /// aggregate compatibility/BFC flag; paint and automatic minimum sizing
    /// must consult the relevant axis.
    pub overflow_clip_x: bool,
    pub overflow_clip_y: bool,
    pub(crate) overflow_axes_set: bool,
    pub(crate) overflow_specified_x: u8,
    pub(crate) overflow_specified_y: u8,
    pub(crate) overflow_inherit_x: bool,
    pub(crate) overflow_inherit_y: bool,
    pub(crate) overflow_scroll_x: bool,
    pub(crate) overflow_scroll_y: bool,
    /// This element's authored overflow is propagated to the viewport. Its
    /// own box therefore behaves as `overflow: visible` for layout/BFC
    /// purposes while the capture viewport supplies the paint clip.
    pub(crate) overflow_propagated_to_viewport: bool,
    /// Whether computed overflow establishes a scroll container. `clip`
    /// clips paint but deliberately does not establish one, which matters for
    /// selecting the scrollport that controls a sticky descendant.
    pub overflow_scroll_container: bool,
    /// Number of classic scrollbar gutters reserved by
    /// `scrollbar-gutter:stable` (one) or `stable both-edges` (two). Root
    /// gutters reduce the initial containing block even when the scrollbar
    /// itself is visually hidden in a headless screenshot.
    pub scrollbar_gutters: u8,

    /// `float: left|right`. True CSS float needs per-line reflow around the
    /// float's shape, which taffy's block/flex/grid modes do not do; see
    /// `dom::group_float_zone` for the bounded approximation this drives.
    pub float: Option<Float>,

    /// `visibility: hidden|visible`, own value. `None` means "inherit the
    /// ancestor's computed value" (visibility, unlike most box properties, is
    /// a real inherited CSS property). Resolved into `effectively_invisible`
    /// during `dom::layout_dom`'s inheritance pass.
    pub visibility_hidden: Option<bool>,
    /// `opacity`, own (non-inherited) value in 0.0-1.0. `None` means the
    /// default of 1.0.
    pub opacity: Option<f32>,
    /// First CSS animation name and its timing contract. The stylesheet
    /// sampler contributes animated opacity after normal declarations and
    /// before author `!important`, matching the animation cascade origin.
    pub animation_name: Option<String>,
    pub animation_timing: AnimationTiming,
    /// True when the selected keyframes contain at least one property this
    /// renderer can sample. Unsupported custom-property-only animations must
    /// not keep layout or screencast damage active forever.
    pub animation_has_render_effect: bool,
    /// Strongest effect of the selected CSS keyframes. Geometry consumers use
    /// this to retain an older sampled layout only when every live effect is
    /// known to be paint-only.
    pub(crate) animation_effect_impact: AnimationEffectImpact,
    /// Local time used for this element's sampled animation instance.
    pub animation_local_time_ms: f32,
    /// `vertical-align` for a table cell's content. Cells effectively default
    /// to `middle` in browsers (the HTML UA sheet sets it on row groups and
    /// cells inherit it); obscura applies it as main-axis alignment of the
    /// cell's flex-column stand-in. `None` on non-cell elements.
    pub vertical_align: Option<VerticalAlign>,
    /// `z-index` on a positioned element. `None` is `auto` (tree order). A
    /// non-zero value lifts the element's whole subtree into a separate paint
    /// layer: negatives under the normal flow, positives above it, sorted.
    pub z_index: Option<i32>,
    /// `clear`, when set: this element moves below preceding floats on the
    /// given side(s), ending their float zone.
    pub clear: Option<Clear>,
    /// Non-inherited CSS counter operations in computed declaration order.
    /// Reset operations run before increments on the same element.
    pub counter_reset: Vec<CounterDirective>,
    pub counter_increment: Vec<CounterDirective>,
    pub counter_set: Vec<CounterDirective>,
    /// Resolved during the inheritance pass: true when this element should
    /// not be painted at all, either from its own or an inherited
    /// `visibility: hidden`, or because the product of its own and every
    /// ancestor's `opacity` is zero. Fractional values remain paintable and
    /// are isolated into composited groups by the paint pass.
    pub effectively_invisible: bool,

    /// A CSS image supplied by `content: url(...)` on a replaced element.
    /// This is distinct from generated pseudo text: on an `<img>` it becomes
    /// the element's image source and contributes intrinsic dimensions just
    /// like an HTML `src`.
    pub content_image: Option<String>,
    /// Literal text injected by a `::before`/`::after` rule with a plain
    /// string-literal `content` (see `css::Stylesheet::pseudo_content`).
    /// Rendered as an extra word-run at the start/end of this element's
    /// children, same as if it were real text content.
    pub before_content: Option<String>,
    pub after_content: Option<String>,
    /// Typed computed `content` items retained until the document-order
    /// counter pass can resolve `counter()` and `counters()`.
    pub generated_content: Option<Vec<GeneratedContentItem>>,
    /// Computed boxes generated by `::before`/`::after`. Text-only pseudos
    /// continue through `before_content`/`after_content`; these styles retain
    /// positioned decorative boxes for layout-independent painting.
    pub before_pseudo: Option<Box<LayoutStyle>>,
    pub after_pseudo: Option<Box<LayoutStyle>>,
    /// Computed author style for the native text-control `::placeholder`
    /// pseudo-element. Its anonymous glyphs are painted by the control.
    pub placeholder_pseudo: Option<Box<LayoutStyle>>,

    /// True for `inline-block`/`inline-flex`/`inline-grid`: participates in
    /// the surrounding inline flow from the outside, like plain `inline`
    /// (both currently collapse to `Display::Inline` — this engine has no
    /// separate inline-block layout mode), but unlike plain `inline` it must
    /// stay a single atomic box rather than have its own content merge into
    /// the parent's line-breaking. `dom::is_flattenable_inline` uses this to
    /// avoid flattening these away: doing so would lose the element as its
    /// own box (including any `::before`/`::after` content attached to it).
    pub is_inline_block: bool,

    /// `display: flow-root`: generates a normal block box but establishes a
    /// new block formatting context, containing descendant floats and stopping
    /// their exclusion bands from propagating into outside siblings.
    pub flow_root: bool,

    /// `display: contents`: the element generates no box of its own; its
    /// children participate in the parent's formatting context directly
    /// (`dom::build_any` splices them into the parent's child list). Kept as a
    /// flag beside `display` because the element still carries inherited styles
    /// for its subtree and `display:none` must still win.
    pub display_contents: bool,

    /// `list-style-type` (or the `list-style` shorthand). Inherited, like in
    /// real CSS; `None` means "not set on this element, inherit". Resolved to
    /// a concrete value during the inheritance pass. Only `<li>` elements draw
    /// a marker from it, but it is carried on every element because it
    /// inherits (a `list-style: none` on a `<ul>` must reach its `<li>`
    /// children, which is how nav menus suppress bullets).
    pub list_style: Option<ListStyle>,

    /// `line-height`. Inherited. `None` means "not set, inherit"; resolved to
    /// a concrete value in the inheritance pass. Drives the vertical rhythm of
    /// shaped text (a fixed ratio made real-site prose noticeably tighter than
    /// Chromium).
    pub line_height: Option<LineHeight>,
    /// `white-space`. Inherited; `None` means inherit the nearest ancestor's
    /// value (or the initial `normal`). The inline shaper uses this to retain
    /// author/source newlines in code blocks and to select wrapping behavior.
    pub white_space: Option<WhiteSpace>,
    /// Non-inherited `text-overflow`. This first implementation deliberately
    /// models Chromium's single-value `clip|ellipsis` syntax; bidi/two-sided
    /// markers remain a separate extension.
    pub text_overflow: TextOverflow,
    /// Non-inherited legacy line count. It affects direct pure-text inline
    /// formatting contexts only; nested descendant line counting needs a real
    /// block-line iterator and must not be approximated by clipping children.
    pub webkit_line_clamp: Option<u32>,
    /// `overflow-wrap` (legacy alias: `word-wrap`). Inherited; `None` means
    /// inherit the nearest ancestor's value (or the initial `normal`).
    ///
    /// `Anywhere` contributes its emergency grapheme opportunities to
    /// min-content sizing, while legacy `BreakWord` uses those opportunities
    /// only for actual line layout.
    pub overflow_wrap: Option<OverflowWrap>,
    /// `word-break`. Inherited; `None` means inherit the nearest ancestor's
    /// value (or the initial `normal`). Kept separate from `overflow-wrap`
    /// because `break-all` adds typographic-letter opportunities while
    /// retaining UAX#14 punctuation constraints, whereas `keep-all`
    /// suppresses eligible letter/number boundaries without altering text.
    pub word_break: Option<WordBreak>,
    /// `text-wrap-style`. Inherited; `None` means inherit the nearest
    /// ancestor's value (or the initial `auto`). `Balance` keeps the natural
    /// line count but tightens the effective line-breaking width during the
    /// inline formatter's final shaping pass.
    pub text_wrap_style: Option<TextWrapStyle>,
    /// Deferred functional line-height (`calc()`, `min()`, `clamp()`) resolved
    /// after the element font and live viewport are known.
    pub line_height_expression: Option<String>,

    /// `text-transform`. Inherited. Applied to span text before shaping.
    pub text_transform: Option<TextTransform>,

    /// `text-decoration-line: underline` (or the `text-decoration` shorthand).
    /// Not inherited in CSS, but a decoration visually covers descendant inline
    /// text, so it is propagated into the shaped spans of the element's subtree
    /// (this is what underlines links, which are underlined by UA default).
    pub underline: Option<bool>,

    /// `font-style: italic|oblique`. Inherited. Selects an available oblique
    /// face when shaping; the bundled Linux `system-ui` face synthesizes its
    /// slant from DejaVu Sans regular/bold to match Chromium. `None` means
    /// inherit.
    pub font_style_italic: Option<bool>,

    /// `object-fit` for a replaced element (`<img>`). Controls how the decoded
    /// image is scaled into the element's box when their aspect ratios differ;
    /// `Fill` (default) stretches to the box, the rest preserve aspect ratio.
    /// Only consulted in the image paint path.
    pub object_fit: ObjectFit,
    /// `object-position` for replaced image content. Percentages resolve
    /// against the leftover space after `object-fit`; the CSS initial value
    /// is centered on both axes.
    pub object_position: ObjectPosition,
    /// Ordered operations in the non-inherited `transform` property. Length
    /// percentages remain unresolved until the final border box is known.
    pub transform_ops: Vec<TransformOp>,
    /// Transform value immediately below the Web Animations cascade origin.
    /// A retained compositor-style sample restores this value before replaying
    /// the registered effects, so sparse keyframes never compound on the
    /// previously sampled transform.
    pub(crate) waapi_sample_state: Option<Box<WaapiSampleState>>,
    /// Individual CSS `translate` property. This composes independently with
    /// the legacy `transform` property, so `transform:none` must not clear it.
    /// Functional values are retained separately until the final border box
    /// is known because percentages resolve against that box's own axes.
    pub individual_translate: Option<(Dimension, Dimension)>,
    pub individual_translate_expressions: [Option<String>; 2],
    /// Individual CSS `rotate` property, in degrees. It composes after
    /// individual translate and before individual scale and `transform`.
    pub individual_rotate: Option<f32>,
    /// Individual CSS `scale` property. Kept separate from `transform` so
    /// declaration order cannot accidentally overwrite either property.
    pub individual_scale: Option<(f32, f32)>,
    /// Independent CSS-property triggers that establish containing blocks for
    /// absolute and fixed descendants. Kept as a bitset so `filter:none`
    /// cannot clear a transform/containment trigger from another property.
    pub containing_block_triggers: u16,
    /// Authored `transform-origin`, unresolved so percentages use the final
    /// border-box dimensions. `None` is the CSS initial value, 50% 50%.
    pub transform_origin: Option<(Dimension, Dimension)>,
    /// `box-shadow` (first layer only). Painted behind the element's own
    /// background/border box: cards, buttons, menus, and modals across the
    /// modern web rely on it for depth, and without it those elements paint
    /// flat. See [`BoxShadow`] and `paint::paint_box_shadow`.
    pub box_shadow: Option<BoxShadow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BorderCascadeSide {
    Top,
    Right,
    Bottom,
    Left,
    Inline,
    InlineStart,
    InlineEnd,
    Block,
    BlockStart,
    BlockEnd,
    All,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BorderCascadeOp {
    pub side: BorderCascadeSide,
    pub width: Option<f32>,
    pub style: Option<BorderStyle>,
    /// Outer `None` means this operation leaves color unchanged; inner `None`
    /// is the valid computed `currentcolor` value.
    pub color: Option<Option<[u8; 4]>>,
}

/// Intrinsic metadata exposed by a decoded replaced resource.
///
/// Presence of this value means metadata is available; each dimension may
/// still be absent independently. This is required for SVG, whose root can
/// declare one dimension, both dimensions, or only a `viewBox` ratio.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ReplacedIntrinsic {
    pub(crate) width: Option<f32>,
    pub(crate) height: Option<f32>,
    pub(crate) ratio: Option<f32>,
}

impl ReplacedIntrinsic {
    pub fn from_dimensions(width: f32, height: f32) -> Self {
        Self {
            width: Some(width),
            height: Some(height),
            ratio: (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
                .then_some(width / height),
        }
    }

    /// Resolve the concrete natural size from CSS Images' 300x150 default
    /// object size. A definite authored layout axis still overrides this
    /// fallback and transfers through the intrinsic ratio.
    pub fn natural_size(self) -> Option<(f32, f32)> {
        let width = self.width.filter(|value| value.is_finite() && *value > 0.0);
        let height = self
            .height
            .filter(|value| value.is_finite() && *value > 0.0);
        let ratio = self.ratio.filter(|value| value.is_finite() && *value > 0.0);
        match (width, height, ratio) {
            (Some(width), Some(height), _) => Some((width, height)),
            (Some(width), None, Some(ratio)) => Some((width, width / ratio)),
            (None, Some(height), Some(ratio)) => Some((height * ratio, height)),
            (Some(width), None, None) => Some((width, 150.0)),
            (None, Some(height), None) => Some((300.0, height)),
            (None, None, Some(ratio)) if ratio >= 2.0 => Some((300.0, 300.0 / ratio)),
            (None, None, Some(ratio)) => Some((150.0 * ratio, 150.0)),
            (None, None, None) => Some((300.0, 150.0)),
        }
    }
}

/// Whether this box has an inline outer display and participates in an inline
/// formatting context. `display` retains the inner layout mode for
/// inline-flex/grid, so this cannot be inferred from `Display::Inline` alone.
pub(crate) fn is_inline_level_box(style: &LayoutStyle) -> bool {
    style.display == Display::Inline || style.is_inline_block
}

/// Apply CSS Display blockification while preserving the inner display mode.
/// Thus inline-flex becomes flex and inline-grid becomes grid, while
/// inline-block/plain inline become block.
pub(crate) fn blockify_outer_display(style: &mut LayoutStyle) {
    if !is_inline_level_box(style) {
        return;
    }
    if style.display == Display::Inline {
        style.display = Display::Block;
    }
    style.is_inline_block = false;
}

pub(crate) const CB_TRIGGER_TRANSFORM: u16 = 1 << 0;
pub(crate) const CB_TRIGGER_FILTER: u16 = 1 << 1;
pub(crate) const CB_TRIGGER_BACKDROP_FILTER: u16 = 1 << 2;
pub(crate) const CB_TRIGGER_PERSPECTIVE: u16 = 1 << 3;
pub(crate) const CB_TRIGGER_CONTAIN: u16 = 1 << 4;
pub(crate) const CB_TRIGGER_WILL_CHANGE: u16 = 1 << 5;
pub(crate) const CB_TRIGGER_CONTENT_VISIBILITY: u16 = 1 << 6;
pub(crate) const CB_TRIGGER_TRANSLATE: u16 = 1 << 7;
pub(crate) const CB_TRIGGER_ROTATE: u16 = 1 << 8;
pub(crate) const CB_TRIGGER_SCALE: u16 = 1 << 9;

impl LayoutStyle {
    /// Whether CSS box sizes compute normally but do not apply to this box's
    /// used geometry. The display value here is post-blockification, so roots,
    /// flex/grid items, floats, and out-of-flow boxes retain applicable sizes.
    pub(crate) fn ignores_used_box_sizes(&self) -> bool {
        self.display == Display::Inline && !self.is_inline_block && !self.is_replaced_box
    }

    pub(crate) fn establishes_positioning_containing_block(&self) -> bool {
        self.containing_block_triggers != 0
    }

    pub(crate) fn clips_overflow_x(&self) -> bool {
        if self.overflow_axes_set {
            self.overflow_clip_x
        } else {
            self.overflow_hidden
        }
    }

    pub(crate) fn clips_overflow_y(&self) -> bool {
        if self.overflow_axes_set {
            self.overflow_clip_y
        } else {
            self.overflow_hidden
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Float {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoxSizing {
    #[default]
    ContentBox,
    BorderBox,
    /// A specified CSS-wide `inherit` value. The DOM's top-down computed-style
    /// pass resolves this to the parent's computed value before layout.
    Inherit,
}

/// One `box-shadow` layer. Offsets, blur, and spread are in CSS px; `color` is
/// the resolved RGBA (falling back to the element's text color, per CSS
/// `currentColor`, when the value omits a color); `inset` distinguishes an
/// inner shadow from the default outer (drop) shadow. Only the first layer of a
/// comma-separated list is modeled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: [u8; 4],
    pub inset: bool,
}

/// `object-fit` for replaced elements (`<img>`): how the image's intrinsic
/// content is scaled into its box when their aspect ratios differ. `Fill` (the
/// default) stretches to the whole box; the others preserve the image's aspect
/// ratio, either letterboxing inside the box (`Contain`), cropping to cover it
/// (`Cover`), or using the intrinsic size (`None`, or `ScaleDown` which is
/// `Contain` capped at the intrinsic size so it never upscales).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectFit {
    #[default]
    Fill,
    Contain,
    Cover,
    ScaleDown,
    None,
}

/// `object-position` for replaced image content. It shares the same
/// length-percentage positioning model as backgrounds, but has a centered
/// initial value instead of `background-position`'s start-edge default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectPosition {
    pub x: BackgroundPositionAxis,
    pub y: BackgroundPositionAxis,
}

impl ObjectPosition {
    pub const fn new(x: BackgroundPositionAxis, y: BackgroundPositionAxis) -> Self {
        Self { x, y }
    }
}

impl Default for ObjectPosition {
    fn default() -> Self {
        Self {
            x: BackgroundPositionAxis::percentage(0.5),
            y: BackgroundPositionAxis::percentage(0.5),
        }
    }
}

/// `vertical-align` positions for table-cell content. `baseline` (and the
/// text-level values like sub/super, which do not apply to cells) map to
/// `Top` as an approximation: real per-row baseline alignment needs shared
/// ascent metrics across the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalAlign {
    Top,
    Middle,
    Bottom,
}

/// `clear`: which floated side(s) an element moves below. Ends the float
/// zone in `dom::build_children_with_float_zone` (the clearfix idiom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clear {
    Left,
    Right,
    Both,
}

/// `line-height`: `normal` (a font-relative default), a unitless multiple of
/// font-size, or an absolute pixel length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    Normal,
    /// Unitless number. Unlike length/percentage values, this remains a ratio
    /// when inherited and therefore scales with each descendant's font size.
    Ratio(f32),
    Px(f32),
    /// A specified length or percentage awaiting computed-value resolution.
    /// It becomes `Px` on the declaring element before inheritance.
    Relative(Dimension),
}

/// The legacy `white-space` shorthand values needed by inline collection and
/// line breaking. `BreakSpaces` currently shares pre-wrap's wrapping model;
/// its finer trailing-space opportunity rules can be added independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhiteSpace {
    #[default]
    Normal,
    NoWrap,
    Pre,
    PreWrap,
    PreLine,
    BreakSpaces,
}

/// Single-value CSS `text-overflow` behavior supported by Chromium's default
/// feature set. The marker is generated by the inline formatter and never
/// inserted into DOM text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextOverflow {
    #[default]
    Clip,
    Ellipsis,
}

/// Emergency wrapping behavior for otherwise-unbreakable text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverflowWrap {
    #[default]
    Normal,
    BreakWord,
    Anywhere,
}

/// The supported `word-break` values. `BreakWord` is the legacy compatibility
/// value whose effective behavior is `word-break: normal` plus
/// `overflow-wrap: anywhere`, matching Blink and Gecko.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WordBreak {
    #[default]
    Normal,
    BreakAll,
    KeepAll,
    BreakWord,
}

/// CSS animation sample time relative to the document timeline origin.
/// Live capture uses elapsed document time, while deterministic comparison
/// harnesses can request an exact instant such as T=0.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AnimationSampleTime {
    pub milliseconds: f32,
}

/// Which clock an animation sample represents.
///
/// Live rendering subtracts each element's animation-instance start epoch
/// from document time. Deterministic comparison instead assigns one local
/// time to every animation, matching Web Animations `currentTime` rather than
/// pretending all dynamically-created animations began at navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationSampleMode {
    #[default]
    DocumentTime,
    LocalOverride,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AnimationSample {
    pub time: AnimationSampleTime,
    pub mode: AnimationSampleMode,
}

/// Strongest renderer-visible consequence of an animation effect.
///
/// Ordering is intentional: aggregating with `max` keeps unknown or
/// geometry-affecting tracks conservative while still distinguishing
/// compositor/paint-only effects from an inactive animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum AnimationEffectImpact {
    #[default]
    None,
    Paint,
    Geometry,
}

impl AnimationSample {
    pub fn document(milliseconds: f32) -> Self {
        Self {
            time: AnimationSampleTime { milliseconds },
            mode: AnimationSampleMode::DocumentTime,
        }
    }

    pub fn local_override(milliseconds: f32) -> Self {
        Self {
            time: AnimationSampleTime { milliseconds },
            mode: AnimationSampleMode::LocalOverride,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationDirection {
    #[default]
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationFillMode {
    #[default]
    None,
    Forwards,
    Backwards,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationPlayState {
    #[default]
    Running,
    Paused,
}

/// Timing fields for the first CSS animation. Milliseconds are used
/// internally so `s`, `ms`, negative delays, and calculated stagger values
/// share one unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationTiming {
    pub duration_ms: f32,
    pub delay_ms: f32,
    /// A finite non-negative count, or positive infinity for `infinite`.
    pub iteration_count: f32,
    pub direction: AnimationDirection,
    pub fill_mode: AnimationFillMode,
    pub play_state: AnimationPlayState,
}

impl Default for AnimationTiming {
    fn default() -> Self {
        Self {
            duration_ms: 0.0,
            delay_ms: 0.0,
            iteration_count: 1.0,
            direction: AnimationDirection::Normal,
            fill_mode: AnimationFillMode::None,
            play_state: AnimationPlayState::Running,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AnimationInstanceKey {
    pub name: String,
}

#[derive(Debug, Clone)]
struct AnimationInstance {
    key: AnimationInstanceKey,
    start_ms: f32,
    hold_time_ms: Option<f32>,
    was_paused: bool,
}

/// A normalized property keyframe supplied through the Web Animations API.
/// Values stay in specified form until cascade time because transforms may
/// contain relative units whose meaning depends on the animated element.
#[derive(Debug, Clone)]
pub struct WaapiKeyframe {
    pub offset: f32,
    pub opacity: Option<f32>,
    pub transform: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaapiPlayState {
    Running,
    Paused,
    Finished,
}

/// Renderer-owned WAAPI effect. JavaScript keeps the wrapper object identity,
/// while this document-scoped record is the source of truth for cascade and
/// paint. It deliberately does not rewrite the element's inline style.
#[derive(Debug, Clone)]
pub struct WaapiAnimation {
    pub id: u64,
    pub node: obscura_dom::tree::NodeId,
    pub keyframes: Vec<WaapiKeyframe>,
    pub timing: AnimationTiming,
    /// Cubic Bezier timing function control points. `None` is linear.
    pub easing: Option<[f32; 4]>,
    /// CSS `linear()` output samples at evenly distributed input positions.
    pub linear_easing: Option<Vec<f32>>,
    pub start_time_ms: f32,
    pub hold_time_ms: Option<f32>,
    pub play_state: WaapiPlayState,
}

/// Page-owned CSS animation instance history retained across layout rebuilds.
/// Node ids are document-scoped, so navigation must replace this value.
#[derive(Debug, Default)]
pub struct AnimationTimelineState {
    instances: std::collections::HashMap<obscura_dom::tree::NodeId, AnimationInstance>,
    start_candidates: std::collections::HashMap<obscura_dom::tree::NodeId, f32>,
    subtree_start_candidates: std::collections::HashMap<obscura_dom::tree::NodeId, f32>,
    waapi: std::collections::BTreeMap<u64, WaapiAnimation>,
}

impl AnimationTimelineState {
    pub fn note_start_candidate(
        &mut self,
        node: obscura_dom::tree::NodeId,
        document_time_ms: f32,
    ) {
        if document_time_ms.is_finite() && document_time_ms >= 0.0 {
            self.start_candidates.insert(node, document_time_ms);
        }
    }

    /// Defer expansion until the next style flush. This is needed for DOM
    /// string APIs whose imported descendants do not exist until after the
    /// mutation op has completed.
    pub fn note_subtree_start_candidate(
        &mut self,
        root: obscura_dom::tree::NodeId,
        document_time_ms: f32,
    ) {
        if document_time_ms.is_finite() && document_time_ms >= 0.0 {
            self.subtree_start_candidates
                .insert(root, document_time_ms);
        }
    }

    pub fn materialize_start_candidates(&mut self, tree: &obscura_dom::DomTree) {
        for (root, start_ms) in std::mem::take(&mut self.subtree_start_candidates) {
            if tree.get_node(root).is_none() {
                continue;
            }
            self.start_candidates.insert(root, start_ms);
            for node in tree.descendants(root) {
                self.start_candidates.entry(node).or_insert(start_ms);
            }
        }
    }

    pub fn remove_subtree<'a>(
        &mut self,
        nodes: impl IntoIterator<Item = &'a obscura_dom::tree::NodeId>,
    ) {
        for node in nodes {
            self.instances.remove(node);
            self.start_candidates.remove(node);
            self.subtree_start_candidates.remove(node);
            self.waapi.retain(|_, animation| animation.node != *node);
        }
    }

    pub fn register_waapi(&mut self, animation: WaapiAnimation) {
        self.waapi.insert(animation.id, animation);
    }

    /// Target node for a registered Web Animation. Control operations use
    /// this before mutating/removing the record so retained style damage can
    /// stay scoped to the affected element.
    pub fn waapi_node(&self, id: u64) -> Option<obscura_dom::tree::NodeId> {
        self.waapi.get(&id).map(|animation| animation.node)
    }

    /// Exact set of nodes currently targeted by registered Web Animations.
    /// The render preparation boundary additionally filters disconnected
    /// targets against the current DOM before producing restyle damage.
    pub fn waapi_nodes(
        &self,
    ) -> std::collections::HashSet<obscura_dom::tree::NodeId> {
        self.waapi.values().map(|animation| animation.node).collect()
    }

    /// New animation instance epochs must reach a style flush. Retained
    /// mutation planning cascades the affected nodes while clean branches keep
    /// their existing instances.
    pub fn has_pending_start_candidates(&self) -> bool {
        !self.start_candidates.is_empty() || !self.subtree_start_candidates.is_empty()
    }

    pub fn cancel_waapi(&mut self, id: u64) -> bool {
        self.waapi.remove(&id).is_some()
    }

    pub fn set_waapi_current_time(&mut self, id: u64, document_time_ms: f32, local_time_ms: f32) -> bool {
        let Some(animation) = self.waapi.get_mut(&id) else { return false; };
        let local = local_time_ms.max(0.0);
        animation.hold_time_ms = Some(local);
        animation.start_time_ms = document_time_ms - local;
        true
    }

    pub fn set_waapi_play_state(
        &mut self,
        id: u64,
        state: WaapiPlayState,
        document_time_ms: f32,
    ) -> bool {
        let Some(animation) = self.waapi.get_mut(&id) else { return false; };
        let current = animation
            .hold_time_ms
            .unwrap_or_else(|| (document_time_ms - animation.start_time_ms).max(0.0));
        match state {
            WaapiPlayState::Running => {
                animation.start_time_ms = document_time_ms - current;
                animation.hold_time_ms = None;
            }
            WaapiPlayState::Paused | WaapiPlayState::Finished => {
                animation.hold_time_ms = Some(current);
            }
        }
        animation.play_state = state;
        true
    }

    pub fn finish_waapi(&mut self, id: u64) -> bool {
        let Some(animation) = self.waapi.get_mut(&id) else { return false; };
        let end = animation.timing.delay_ms
            + animation.timing.duration_ms * animation.timing.iteration_count.max(0.0);
        animation.hold_time_ms = Some(end.max(0.0));
        animation.play_state = WaapiPlayState::Finished;
        true
    }

    pub(crate) fn waapi_for_node(
        &self,
        node: obscura_dom::tree::NodeId,
        document_time: AnimationSampleTime,
    ) -> impl Iterator<Item = (&WaapiAnimation, AnimationSampleTime)> {
        self.waapi.values().filter_map(move |animation| {
            if animation.node != node {
                return None;
            }
            let local = animation.hold_time_ms.unwrap_or_else(|| {
                (document_time.milliseconds - animation.start_time_ms).max(0.0)
            });
            Some((animation, AnimationSampleTime { milliseconds: local }))
        })
    }

    pub(crate) fn has_active_waapi(&self, document_time: AnimationSampleTime) -> bool {
        self.waapi
            .values()
            .any(|animation| waapi_is_active(animation, document_time))
    }

    pub(crate) fn active_waapi_effect_impact(
        &self,
        document_time: AnimationSampleTime,
    ) -> AnimationEffectImpact {
        self.waapi
            .values()
            .filter(|animation| waapi_is_active(animation, document_time))
            .map(|animation| {
                if animation
                    .keyframes
                    .iter()
                    .any(|frame| frame.transform.is_some())
                {
                    // A specified WAAPI transform remains geometry-affecting
                    // even when this renderer cannot parse its value.
                    AnimationEffectImpact::Geometry
                } else if animation
                    .keyframes
                    .iter()
                    .any(|frame| frame.opacity.is_some())
                {
                    AnimationEffectImpact::Paint
                } else {
                    AnimationEffectImpact::None
                }
            })
            .max()
            .unwrap_or_default()
    }

    pub(crate) fn sample_for(
        &mut self,
        node: obscura_dom::tree::NodeId,
        key: AnimationInstanceKey,
        play_state: AnimationPlayState,
        sample: AnimationSample,
    ) -> AnimationSampleTime {
        if sample.mode == AnimationSampleMode::LocalOverride {
            return sample.time;
        }
        let document_time_ms = sample.time.milliseconds;
        let transition_time_ms = self.start_candidates.remove(&node);
        let retained = self
            .instances
            .get_mut(&node)
            .filter(|instance| instance.key == key);
        let instance = match retained {
            Some(instance) => instance,
            None => {
                let candidate = transition_time_ms.unwrap_or(0.0);
                let paused = play_state == AnimationPlayState::Paused;
                self.instances.insert(
                    node,
                    AnimationInstance {
                        key,
                        start_ms: if paused { document_time_ms } else { candidate },
                        hold_time_ms: paused.then_some(0.0),
                        was_paused: paused,
                    },
                );
                self.instances.get_mut(&node).expect("inserted animation instance")
            }
        };

        let paused = play_state == AnimationPlayState::Paused;
        if paused && !instance.was_paused {
            let paused_at = transition_time_ms.unwrap_or(document_time_ms);
            instance.hold_time_ms = Some((paused_at - instance.start_ms).max(0.0));
        } else if !paused && instance.was_paused {
            let held = instance.hold_time_ms.take().unwrap_or(0.0);
            let resumed_at = transition_time_ms.unwrap_or(document_time_ms);
            instance.start_ms = resumed_at - held;
        }
        instance.was_paused = paused;
        AnimationSampleTime {
            milliseconds: instance
                .hold_time_ms
                .unwrap_or_else(|| (document_time_ms - instance.start_ms).max(0.0)),
        }
    }

    pub(crate) fn clear_animation(&mut self, node: obscura_dom::tree::NodeId, sample: AnimationSample) {
        if sample.mode == AnimationSampleMode::DocumentTime {
            self.instances.remove(&node);
        }
    }

    pub fn retain_nodes(
        &mut self,
        mut keep: impl FnMut(obscura_dom::tree::NodeId) -> bool,
    ) {
        self.instances.retain(|node, _| keep(*node));
        self.start_candidates.retain(|node, _| keep(*node));
        self.subtree_start_candidates.retain(|node, _| keep(*node));
        self.waapi.retain(|_, animation| keep(animation.node));
    }

    pub fn clear_start_candidates(&mut self) {
        self.start_candidates.clear();
        self.subtree_start_candidates.clear();
    }
}

fn waapi_is_active(animation: &WaapiAnimation, document_time: AnimationSampleTime) -> bool {
    if animation.play_state != WaapiPlayState::Running
        || animation.timing.duration_ms <= 0.0
        || animation.timing.iteration_count <= 0.0
    {
        return false;
    }
    let local = animation.hold_time_ms.unwrap_or_else(|| {
        (document_time.milliseconds - animation.start_time_ms).max(0.0)
    });
    let end = animation.timing.delay_ms
        + animation.timing.duration_ms * animation.timing.iteration_count;
    local < end.max(0.0)
}

/// The implemented `text-wrap-style` values. Other line-breaking strategies
/// such as `pretty` remain unsupported until their distinct scoring model is
/// available; treating them as `auto` would make `@supports` lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextWrapStyle {
    #[default]
    Auto,
    Balance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

/// `list-style-type` values we render a marker for. `Decimal` numbers the
/// item by its position among sibling list items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStyle {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
}

/// A node in the input layout tree. `text` is carried for the paint phase; it
/// does not affect layout in phase 1 (inline/text layout comes later).
#[derive(Debug, Clone)]
pub struct LayoutNode {
    pub style: LayoutStyle,
    #[allow(dead_code)]
    pub text: Option<String>,
    pub children: Vec<LayoutNode>,
}

impl LayoutNode {
    pub fn leaf(style: LayoutStyle) -> Self {
        LayoutNode {
            style,
            text: None,
            children: Vec::new(),
        }
    }
}

/// Computed geometry for one node and its subtree.
#[derive(Debug, Clone, Default)]
pub struct NodeRect {
    /// Border box, in viewport coordinates.
    pub border_box: Rect,
    pub children: Vec<NodeRect>,
}

/// Lay out `root` within a `viewport` (width, height) in CSS pixels and return
/// the border-box geometry per node, mirroring the input tree.
pub fn layout(root: &LayoutNode, viewport: (f32, f32)) -> NodeRect {
    let root_font_size = root.style.font_size.unwrap_or(16.0);
    initialize_grid_calc_contexts(
        root,
        root_font_size,
        root_font_size,
        viewport.0 / 100.0,
        viewport.1 / 100.0,
    );
    let mut tree: TaffyTree = new_taffy_tree();
    let root_id = build_node(&mut tree, root);
    let _ = tree.compute_layout(
        root_id,
        taffy::Size {
            width: taffy::AvailableSpace::Definite(viewport.0),
            height: taffy::AvailableSpace::Definite(viewport.1),
        },
    );
    read_node(&tree, root_id)
}

fn initialize_grid_calc_contexts(
    node: &LayoutNode,
    inherited_font_size: f32,
    root_font_size: f32,
    vw: f32,
    vh: f32,
) {
    let font_size = node.style.font_size.unwrap_or(inherited_font_size);
    crate::style::set_grid_calc_context(&node.style, font_size, root_font_size, vw, vh);
    for child in &node.children {
        initialize_grid_calc_contexts(child, font_size, root_font_size, vw, vh);
    }
}

pub(crate) fn new_taffy_tree<NodeContext>() -> TaffyTree<NodeContext> {
    let mut tree = TaffyTree::new();
    // Every opaque handle is backed by an Arc retained in the LayoutStyle
    // map/input tree, which outlives all computations on `tree`.
    tree.set_calc_resolver(crate::style::resolve_grid_calc);
    tree
}

fn build_node(tree: &mut TaffyTree, node: &LayoutNode) -> NodeId {
    let style = to_taffy_style(&node.style);
    if node.children.is_empty() {
        tree.new_leaf(style).expect("taffy new_leaf")
    } else {
        let child_ids: Vec<NodeId> = node.children.iter().map(|c| build_node(tree, c)).collect();
        tree.new_with_children(style, &child_ids)
            .expect("taffy new_with_children")
    }
}

fn read_node(tree: &TaffyTree, id: NodeId) -> NodeRect {
    let layout = tree.layout(id).expect("taffy layout");
    NodeRect {
        border_box: Rect {
            x: layout.location.x,
            y: layout.location.y,
            width: layout.size.width,
            height: layout.size.height,
        },
        children: tree
            .children(id)
            .unwrap_or_default()
            .iter()
            .map(|&cid| read_node(tree, cid))
            .collect(),
    }
}

pub(crate) fn to_taffy_style(style: &LayoutStyle) -> Style {
    let mut s = Style::DEFAULT;
    s.direction = style.direction.unwrap_or(taffy::Direction::Ltr);
    s.item_is_replaced = style.has_replaced_sizing;
    s.item_aspect_ratio_is_intrinsic = style.aspect_ratio_is_intrinsic;
    s.box_sizing = match style.box_sizing {
        BoxSizing::ContentBox => taffy::BoxSizing::ContentBox,
        BoxSizing::BorderBox => taffy::BoxSizing::BorderBox,
        // Programmatic LayoutStyle users do not have a DOM inheritance pass;
        // fall back to the property's CSS initial value in that case.
        BoxSizing::Inherit => taffy::BoxSizing::ContentBox,
    };

    // A block box with centered/right inline content needs a flex-column
    // stand-in because taffy's native block algorithm has no line alignment.
    // `text_align` is separate from real flex/grid `align-items`, so a
    // text-align declaration never changes how flex children are sized.
    let promote_for_alignment = style.display == Display::Block
        && matches!(
            style.text_align,
            Some(taffy::AlignItems::CENTER) | Some(taffy::AlignItems::FLEX_END)
        );

    s.display = match style.display {
        Display::Block if promote_for_alignment => taffy::style::Display::Flex,
        Display::Block => taffy::style::Display::Block,
        Display::Flex => taffy::style::Display::Flex,
        Display::Grid => taffy::style::Display::Grid,
        Display::Inline => taffy::style::Display::Flex,
        Display::None => taffy::style::Display::None,
    };
    if promote_for_alignment {
        s.flex_direction = taffy::FlexDirection::Column;
        s.align_items = style.text_align;
    }
    if let Some(fd) = style.flex_direction {
        s.flex_direction = fd;
    }
    if let Some(fw) = style.flex_wrap {
        s.flex_wrap = fw;
    } else if style.display == Display::Inline {
        s.flex_direction = taffy::FlexDirection::Row;
        s.flex_wrap = taffy::FlexWrap::Wrap;
    } else {
        s.flex_wrap = taffy::FlexWrap::NoWrap;
    }
    s.size = taffy::Size {
        width: dimension(style.width),
        height: dimension(style.height),
    };
    // Tell taffy's layout algorithm about computed overflow, not just our
    // paint-time clips: a flex/grid automatic minimum depends on the relevant
    // axis. Overflow propagated from html/body belongs to the viewport and
    // must leave the source element's own layout overflow visible.
    if style.overflow_hidden && !style.overflow_propagated_to_viewport {
        s.overflow = taffy::Point {
            x: if style.overflow_scroll_x {
                taffy::style::Overflow::Hidden
            } else if style.clips_overflow_x() {
                taffy::style::Overflow::Clip
            } else {
                taffy::style::Overflow::Visible
            },
            y: if style.overflow_scroll_y {
                taffy::style::Overflow::Hidden
            } else if style.clips_overflow_y() {
                taffy::style::Overflow::Clip
            } else {
                taffy::style::Overflow::Visible
            },
        };
    }
    s.min_size = taffy::Size {
        width: dimension(style.min_width),
        height: dimension(style.min_height),
    };
    s.max_size = taffy::Size {
        width: dimension(style.max_width),
        height: dimension(style.max_height),
    };
    if let Some(ar) = style.aspect_ratio {
        if ar.is_finite() && ar > 0.0 {
            s.aspect_ratio = Some(ar);
        }
    }
    if style.ignores_used_box_sizes() {
        s.size = taffy::Size {
            width: taffy::Dimension::auto(),
            height: taffy::Dimension::auto(),
        };
        s.min_size = taffy::Size {
            width: taffy::Dimension::auto(),
            height: taffy::Dimension::auto(),
        };
        s.max_size = taffy::Size {
            width: taffy::Dimension::auto(),
            height: taffy::Dimension::auto(),
        };
        s.aspect_ratio = None;
    }
    if style.display != Display::Block {
        if let Some(ai) = style.align_items {
            s.align_items = Some(ai);
        }
    } else if !promote_for_alignment {
        // `align-items` has no effect on a block formatting context.
        s.align_items = None;
    }
    s.justify_items = style.justify_items;
    s.align_self = style.align_self;
    s.justify_self = style.justify_self;
    s.align_content = style.align_content;
    if let Some(jc) = style.justify_content {
        s.justify_content = Some(jc);
    } else if style.is_inline_block {
        // Taffy's inline-box stand-in is a wrapping flex row. `text-align`
        // aligns each line of an inline-block's internal formatting context;
        // map that inherited value onto the row axis without affecting how
        // the atomic box itself participates in its parent's inline flow.
        s.justify_content = match style.text_align {
            Some(taffy::AlignItems::CENTER) => Some(taffy::JustifyContent::CENTER),
            Some(taffy::AlignItems::FLEX_END) => Some(taffy::JustifyContent::FLEX_END),
            _ => None,
        };
    }
    if let Some(fg) = style.flex_grow {
        s.flex_grow = fg;
    }
    if let Some(fs) = style.flex_shrink {
        s.flex_shrink = fs;
    }
    if style.flex_basis != Dimension::Auto {
        s.flex_basis = dimension(style.flex_basis);
    }

    // Grid container tracks and gaps. Numeric repeat() values are expanded
    // during parsing, while auto-fill/auto-fit remain native taffy repetition
    // components so their count can use the final container size. The 0.7-era
    // fr->Auto row workaround (which stopped `minmax(0,1fr)` image rows from
    // collapsing to a sliver) is gone: taffy 0.12 treats an in-flow child's
    // vertical available space as indefinite, so fr rows of an auto-height grid
    // size to their content the way real CSS does.
    if style.display == Display::Grid {
        if !style.grid_template_columns.is_empty() {
            s.grid_template_columns = style.grid_template_columns.clone();
        }
        if !style.grid_template_rows.is_empty() {
            s.grid_template_rows = style.grid_template_rows.clone();
        }
        if !style.grid_auto_columns.is_empty() {
            s.grid_auto_columns = style.grid_auto_columns.clone();
        }
        if !style.grid_auto_rows.is_empty() {
            s.grid_auto_rows = style.grid_auto_rows.clone();
        }
        if let Some(flow) = style.grid_auto_flow {
            s.grid_auto_flow = flow;
        }
    }
    let cg = style.column_gap.unwrap_or(0.0);
    let rg = style.row_gap.unwrap_or(0.0);
    s.gap = taffy::Size {
        width: taffy::style::LengthPercentage::length(cg),
        height: taffy::style::LengthPercentage::length(rg),
    };

    // Grid item placement (resolved from grid-area names or explicit lines).
    // `GridPlacement` is no longer `Copy` in taffy 0.12 (it can carry a named
    // line), so clone out of the borrowed style.
    if let Some(gc) = &style.grid_column {
        s.grid_column = gc.clone();
    }
    if let Some(gr) = &style.grid_row {
        s.grid_row = gr.clone();
    }

    // Positioning. Absolute/fixed take the box out of flow.
    if let Some(pos) = style.position {
        s.position = pos;
        if !style.position_sticky {
            s.inset = taffy::Rect {
                top: inset_lpa(style.inset[0]),
                right: inset_lpa(style.inset[1]),
                bottom: inset_lpa(style.inset[2]),
                left: inset_lpa(style.inset[3]),
            };
        }
    }

    s.margin = rect_auto(style.margin, style.margin_auto);
    s.padding = rect_lp_percent(style.padding, style.padding_percent);
    s.border = rect_lp(style.border);
    if style.ignores_used_box_sizes() {
        // Block-axis margins on an ordinary non-replaced inline neither move
        // nor size its fragment. Padding and border do paint around the raw
        // font box, but they protrude outside the CSS line-height rather than
        // increasing line advance. Keep them in LayoutStyle for fragment
        // synthesis/paint and remove them only from this Taffy line surrogate.
        s.margin.top = taffy::LengthPercentageAuto::length(0.0);
        s.margin.bottom = taffy::LengthPercentageAuto::length(0.0);
        s.padding.top = taffy::LengthPercentage::length(0.0);
        s.padding.bottom = taffy::LengthPercentage::length(0.0);
        s.border.top = taffy::LengthPercentage::length(0.0);
        s.border.bottom = taffy::LengthPercentage::length(0.0);
    }
    s
}

fn inset_lpa(v: Option<Dimension>) -> taffy::style::LengthPercentageAuto {
    match v {
        Some(Dimension::Px(px)) => taffy::style::LengthPercentageAuto::length(px),
        Some(Dimension::Percent(p)) => taffy::style::LengthPercentageAuto::percent(p),
        // Relative units are resolved to Px before layout; unresolved leftovers
        // and `auto`/absent both map to Auto.
        _ => taffy::style::LengthPercentageAuto::auto(),
    }
}

fn dimension(v: Dimension) -> taffy::style::Dimension {
    match v {
        Dimension::Px(px) => taffy::style::Dimension::length(px),
        Dimension::Percent(p) => taffy::style::Dimension::percent(p),
        Dimension::Auto => taffy::style::Dimension::auto(),
        // Relative units are resolved to Px before layout; if one slips
        // through unresolved, fall back to its raw magnitude (em/rem ~16px)
        // rather than panicking.
        Dimension::Em(v) | Dimension::Rem(v) => taffy::style::Dimension::length(v * 16.0),
        Dimension::Ex(v) => taffy::style::Dimension::length(v * 16.0 * 0.528_320_3),
        Dimension::Vw(v) | Dimension::Vh(v) | Dimension::Vmin(v) | Dimension::Vmax(v) => {
            taffy::style::Dimension::length(v)
        }
    }
}

fn rect_lp(e: Edges) -> taffy::Rect<taffy::style::LengthPercentage> {
    taffy::Rect {
        top: taffy::style::LengthPercentage::length(e.top),
        right: taffy::style::LengthPercentage::length(e.right),
        bottom: taffy::style::LengthPercentage::length(e.bottom),
        left: taffy::style::LengthPercentage::length(e.left),
    }
}

fn rect_lp_percent(
    e: Edges,
    percent: [Option<f32>; 4],
) -> taffy::Rect<taffy::style::LengthPercentage> {
    let side = |value, percent| match percent {
        Some(percent) => taffy::style::LengthPercentage::percent(percent),
        None => taffy::style::LengthPercentage::length(value),
    };
    taffy::Rect {
        top: side(e.top, percent[0]),
        right: side(e.right, percent[1]),
        bottom: side(e.bottom, percent[2]),
        left: side(e.left, percent[3]),
    }
}

fn rect_auto(e: Edges, auto: [bool; 4]) -> taffy::Rect<taffy::style::LengthPercentageAuto> {
    let side = |value, is_auto| {
        if is_auto {
            taffy::style::LengthPercentageAuto::auto()
        } else {
            taffy::style::LengthPercentageAuto::length(value)
        }
    };
    taffy::Rect {
        top: side(e.top, auto[0]),
        right: side(e.right, auto[1]),
        bottom: side(e.bottom, auto[2]),
        left: side(e.left, auto[3]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_box(display: Display, w: f32, h: f32) -> LayoutStyle {
        LayoutStyle {
            display,
            width: Dimension::Px(w),
            height: Dimension::Px(h),
            ..Default::default()
        }
    }

    #[test]
    fn block_children_stack_vertically() {
        // A 1000px-wide viewport, two fixed-size block children: they should
        // stack top-to-bottom at the expected y offsets.
        let root = LayoutNode {
            style: make_box(Display::Block, 1000.0, 800.0),
            text: None,
            children: vec![
                LayoutNode::leaf(make_box(Display::Block, 1000.0, 50.0)),
                LayoutNode::leaf(make_box(Display::Block, 1000.0, 30.0)),
            ],
        };
        let out = layout(&root, (1000.0, 800.0));
        assert_eq!(out.border_box.width, 1000.0);
        assert_eq!(out.children.len(), 2);
        assert_eq!(out.children[0].border_box.height, 50.0);
        assert_eq!(out.children[1].border_box.height, 30.0);
        // Second block begins where the first ended.
        assert!(
            (out.children[1].border_box.y - out.children[0].border_box.y).abs()
                >= out.children[0].border_box.height - 0.01,
            "blocks should stack: c0.y={} c1.y={}",
            out.children[0].border_box.y,
            out.children[1].border_box.y
        );
    }

    #[test]
    fn grid_calc_tracks_resolve_against_container_width() {
        let grid = crate::style::compute_style(
            "div",
            Some(
                "display:grid;width:1440px;height:20px;\
                 grid-template-columns:\
                 minmax(0,calc((100% - (50rem + 20vw))/2)) 1fr \
                 minmax(0,calc((100% - (50rem + 20vw))/2))",
            ),
        );
        let child = || {
            LayoutNode::leaf(crate::style::compute_style(
                "div",
                Some("display:block;height:10px"),
            ))
        };
        let output = layout(
            &LayoutNode {
                style: grid.clone(),
                text: None,
                children: vec![child(), child(), child()],
            },
            (1440.0, 100.0),
        );

        assert!((output.children[0].border_box.width - 176.0).abs() < 0.01);
        assert!((output.children[1].border_box.x - 176.0).abs() < 0.01);
        assert!((output.children[1].border_box.width - 1088.0).abs() < 0.01);

        let resized = layout(
            &LayoutNode {
                style: grid,
                text: None,
                children: vec![child(), child(), child()],
            },
            (1000.0, 100.0),
        );
        assert!((resized.children[0].border_box.width - 220.0).abs() < 0.01);
        assert!((resized.children[1].border_box.x - 220.0).abs() < 0.01);
        assert!((resized.children[1].border_box.width - 1000.0).abs() < 0.01);
    }

    #[test]
    fn block_auto_margins_absorb_horizontal_free_space() {
        let centered = LayoutStyle {
            display: Display::Block,
            width: Dimension::Px(300.0),
            height: Dimension::Px(40.0),
            margin_auto: [false, true, false, true],
            ..Default::default()
        };
        let pushed_end = LayoutStyle {
            display: Display::Block,
            width: Dimension::Px(200.0),
            height: Dimension::Px(40.0),
            margin: Edges {
                right: 50.0,
                ..Default::default()
            },
            margin_auto: [false, false, false, true],
            ..Default::default()
        };
        let root = LayoutNode {
            style: make_box(Display::Block, 900.0, 200.0),
            text: None,
            children: vec![LayoutNode::leaf(centered), LayoutNode::leaf(pushed_end)],
        };
        let out = layout(&root, (900.0, 200.0));
        assert!((out.children[0].border_box.x - 300.0).abs() < 0.01);
        assert!((out.children[1].border_box.x - 650.0).abs() < 0.01);
    }

    #[test]
    fn negative_flex_margin_overlays_without_shifting_items() {
        let main = make_box(Display::Block, 900.0, 200.0);
        let sidebar = LayoutStyle {
            display: Display::Flex,
            width: Dimension::Px(225.0),
            height: Dimension::Px(180.0),
            margin: Edges {
                left: -900.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let root = LayoutNode {
            style: make_box(Display::Flex, 900.0, 220.0),
            text: None,
            children: vec![LayoutNode::leaf(main), LayoutNode::leaf(sidebar)],
        };
        let out = layout(&root, (900.0, 220.0));
        assert!(
            out.children[0].border_box.x.abs() < 0.01,
            "main shifted to {:?}",
            out.children[0].border_box
        );
        assert!(
            out.children[1].border_box.x.abs() < 0.01,
            "overlay shifted to {:?}",
            out.children[1].border_box
        );
    }

    #[test]
    fn flex_row_lays_out_horizontally() {
        let root = LayoutNode {
            style: LayoutStyle {
                display: Display::Flex,
                width: Dimension::Px(600.0),
                height: Dimension::Px(100.0),
                ..Default::default()
            },
            text: None,
            children: vec![
                LayoutNode::leaf(make_box(Display::Block, 200.0, 100.0)),
                LayoutNode::leaf(make_box(Display::Block, 200.0, 100.0)),
            ],
        };
        let out = layout(&root, (600.0, 400.0));
        assert_eq!(out.border_box.width, 600.0);
        assert_eq!(out.children.len(), 2);
        // In a row the second child is to the right of the first.
        assert!(
            out.children[1].border_box.x > out.children[0].border_box.x,
            "flex row should place children horizontally: c0.x={} c1.x={}",
            out.children[0].border_box.x,
            out.children[1].border_box.x
        );
    }

    #[test]
    fn padding_expands_content_box_but_not_border_box() {
        let content_box = LayoutNode {
            style: LayoutStyle {
                display: Display::Block,
                width: Dimension::Px(100.0),
                height: Dimension::Px(100.0),
                padding: Edges {
                    top: 10.0,
                    right: 10.0,
                    bottom: 10.0,
                    left: 10.0,
                },
                ..Default::default()
            },
            text: None,
            children: vec![],
        };
        let content_out = layout(&content_box, (1000.0, 800.0));
        assert_eq!(content_out.border_box.width, 120.0);

        let mut border_box = content_box;
        border_box.style.box_sizing = BoxSizing::BorderBox;
        let border_out = layout(&border_box, (1000.0, 800.0));
        assert_eq!(border_out.border_box.width, 100.0);
    }
}
