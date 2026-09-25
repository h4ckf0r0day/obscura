// SPDX-License-Identifier: MIT OR Apache-2.0

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::hash::{Hash, Hasher};
use core::ops::Range;
use rangemap::RangeMap;
use smol_str::SmolStr;

use crate::{CacheKeyFlags, Metrics};

pub use fontdb::{Family, Stretch, Style, Weight};

/// CSS-compatible line-breaking policy attached to a rich-text span.
///
/// This is optional on [`Attrs`] so existing cosmic-text users retain the
/// library's ordinary `Wrap` behavior. Browser integrations can opt in when
/// line-breaking rules vary inside one shaped paragraph.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CssLineBreak {
    /// Whether soft wrapping is enabled for this span.
    pub wrap: bool,
    /// CSS `word-break` behavior.
    pub word_break: CssWordBreak,
    /// CSS `overflow-wrap` behavior.
    pub overflow_wrap: CssOverflowWrap,
}

impl Default for CssLineBreak {
    fn default() -> Self {
        Self {
            wrap: true,
            word_break: CssWordBreak::Normal,
            overflow_wrap: CssOverflowWrap::Normal,
        }
    }
}

/// CSS `word-break` values used by the line-breaking adapter.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum CssWordBreak {
    #[default]
    Normal,
    BreakAll,
    KeepAll,
    /// Legacy `word-break: break-word` compatibility behavior.
    BreakWord,
}

/// CSS `overflow-wrap` values used by the line-breaking adapter.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum CssOverflowWrap {
    #[default]
    Normal,
    BreakWord,
    Anywhere,
}

/// Text color
#[derive(Clone, Copy, Debug, PartialOrd, Ord, Eq, Hash, PartialEq)]
pub struct Color(pub u32);

impl Color {
    /// Create new color with red, green, and blue components
    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 0xFF)
    }

    /// Create new color with red, green, blue, and alpha components
    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
    }

    /// Get a tuple over all of the attributes, in `(r, g, b, a)` order.
    #[inline]
    pub fn as_rgba_tuple(self) -> (u8, u8, u8, u8) {
        (self.r(), self.g(), self.b(), self.a())
    }

    /// Get an array over all of the components, in `[r, g, b, a]` order.
    #[inline]
    pub fn as_rgba(self) -> [u8; 4] {
        [self.r(), self.g(), self.b(), self.a()]
    }

    /// Get the red component
    #[inline]
    pub fn r(&self) -> u8 {
        ((self.0 & 0x00_FF_00_00) >> 16) as u8
    }

    /// Get the green component
    #[inline]
    pub fn g(&self) -> u8 {
        ((self.0 & 0x00_00_FF_00) >> 8) as u8
    }

    /// Get the blue component
    #[inline]
    pub fn b(&self) -> u8 {
        (self.0 & 0x00_00_00_FF) as u8
    }

    /// Get the alpha component
    #[inline]
    pub fn a(&self) -> u8 {
        ((self.0 & 0xFF_00_00_00) >> 24) as u8
    }
}

/// An owned version of [`Family`]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum FamilyOwned {
    Name(SmolStr),
    Serif,
    SansSerif,
    Cursive,
    Fantasy,
    Monospace,
}

impl FamilyOwned {
    pub fn new(family: Family) -> Self {
        match family {
            Family::Name(name) => FamilyOwned::Name(SmolStr::from(name)),
            Family::Serif => FamilyOwned::Serif,
            Family::SansSerif => FamilyOwned::SansSerif,
            Family::Cursive => FamilyOwned::Cursive,
            Family::Fantasy => FamilyOwned::Fantasy,
            Family::Monospace => FamilyOwned::Monospace,
        }
    }

    pub fn as_family(&self) -> Family {
        match self {
            FamilyOwned::Name(name) => Family::Name(name),
            FamilyOwned::Serif => Family::Serif,
            FamilyOwned::SansSerif => Family::SansSerif,
            FamilyOwned::Cursive => Family::Cursive,
            FamilyOwned::Fantasy => Family::Fantasy,
            FamilyOwned::Monospace => Family::Monospace,
        }
    }
}

/// Metrics, but implementing Eq and Hash using u32 representation of f32
//TODO: what are the edge cases of this?
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CacheMetrics {
    font_size_bits: u32,
    line_height_bits: u32,
}

impl From<Metrics> for CacheMetrics {
    fn from(metrics: Metrics) -> Self {
        Self {
            font_size_bits: metrics.font_size.to_bits(),
            line_height_bits: metrics.line_height.to_bits(),
        }
    }
}

impl From<CacheMetrics> for Metrics {
    fn from(metrics: CacheMetrics) -> Self {
        Self {
            font_size: f32::from_bits(metrics.font_size_bits),
            line_height: f32::from_bits(metrics.line_height_bits),
        }
    }
}
/// A 4-byte `OpenType` feature tag identifier
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FeatureTag([u8; 4]);

impl FeatureTag {
    pub const fn new(tag: &[u8; 4]) -> Self {
        Self(*tag)
    }

    /// Kerning adjusts spacing between specific character pairs
    pub const KERNING: Self = Self::new(b"kern");
    /// Standard ligatures (fi, fl, etc.)
    pub const STANDARD_LIGATURES: Self = Self::new(b"liga");
    /// Contextual ligatures (context-dependent ligatures)
    pub const CONTEXTUAL_LIGATURES: Self = Self::new(b"clig");
    /// Contextual alternates (glyph substitutions based on context)
    pub const CONTEXTUAL_ALTERNATES: Self = Self::new(b"calt");
    /// Discretionary ligatures (optional stylistic ligatures)
    pub const DISCRETIONARY_LIGATURES: Self = Self::new(b"dlig");
    /// Small caps (lowercase to small capitals)
    pub const SMALL_CAPS: Self = Self::new(b"smcp");
    /// All small caps (uppercase and lowercase to small capitals)
    pub const ALL_SMALL_CAPS: Self = Self::new(b"c2sc");
    /// Stylistic Set 1 (font-specific alternate glyphs)
    pub const STYLISTIC_SET_1: Self = Self::new(b"ss01");
    /// Stylistic Set 2 (font-specific alternate glyphs)
    pub const STYLISTIC_SET_2: Self = Self::new(b"ss02");

    pub fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Feature {
    pub tag: FeatureTag,
    pub value: u32,
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct FontFeatures {
    pub features: Vec<Feature>,
}

impl FontFeatures {
    pub fn new() -> Self {
        Self {
            features: Vec::new(),
        }
    }

    pub fn set(&mut self, tag: FeatureTag, value: u32) -> &mut Self {
        self.features.push(Feature { tag, value });
        self
    }

    /// Enable a feature (set to 1)
    pub fn enable(&mut self, tag: FeatureTag) -> &mut Self {
        self.set(tag, 1)
    }

    /// Disable a feature (set to 0)
    pub fn disable(&mut self, tag: FeatureTag) -> &mut Self {
        self.set(tag, 0)
    }
}

/// A 4-byte `OpenType` variation axis tag.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VariationTag([u8; 4]);

impl VariationTag {
    /// Create a variation axis tag from its four-byte representation.
    pub const fn new(tag: &[u8; 4]) -> Self {
        Self(*tag)
    }

    /// Return the four-byte representation of this axis tag.
    pub const fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }
}

/// A variation coordinate with stable equality and hashing for shape caches.
#[derive(Clone, Copy, Debug)]
pub struct VariationValue(pub f32);

impl PartialEq for VariationValue {
    fn eq(&self, other: &Self) -> bool {
        if self.0.is_nan() {
            other.0.is_nan()
        } else {
            self.0 == other.0
        }
    }
}

impl Eq for VariationValue {}

impl Hash for VariationValue {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        const CANONICAL_NAN_BITS: u32 = 0x7fc0_0000;

        let bits = if self.0.is_nan() {
            CANONICAL_NAN_BITS
        } else {
            // Add +0.0 to canonicalize -0.0 to +0.0.
            (self.0 + 0.0).to_bits()
        };

        bits.hash(hasher);
    }
}

/// One `OpenType` variation axis coordinate.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FontVariation {
    pub tag: VariationTag,
    pub value: VariationValue,
}

/// Variation coordinates applied while shaping a text span.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct FontVariations {
    variations: Vec<FontVariation>,
}

impl FontVariations {
    pub fn new() -> Self {
        Self {
            variations: Vec::new(),
        }
    }

    /// Set an axis coordinate. Setting the same tag again replaces its value,
    /// matching the last-value-wins behavior of CSS variation settings.
    pub fn set(&mut self, tag: VariationTag, value: f32) -> &mut Self {
        match self
            .variations
            .binary_search_by_key(&tag, |variation| variation.tag)
        {
            Ok(index) => self.variations[index].value = VariationValue(value),
            Err(index) => self.variations.insert(
                index,
                FontVariation {
                    tag,
                    value: VariationValue(value),
                },
            ),
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.variations.is_empty()
    }

    pub fn len(&self) -> usize {
        self.variations.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &FontVariation> {
        self.variations.iter()
    }
}

/// A wrapper for letter spacing to get around that f32 doesn't implement Eq and Hash
#[derive(Clone, Copy, Debug)]
pub struct LetterSpacing(pub f32);

impl PartialEq for LetterSpacing {
    fn eq(&self, other: &Self) -> bool {
        if self.0.is_nan() {
            other.0.is_nan()
        } else {
            self.0 == other.0
        }
    }
}

impl Eq for LetterSpacing {}

impl Hash for LetterSpacing {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        const CANONICAL_NAN_BITS: u32 = 0x7fc0_0000;

        let bits = if self.0.is_nan() {
            CANONICAL_NAN_BITS
        } else {
            // Add +0.0 to canonicalize -0.0 to +0.0
            (self.0 + 0.0).to_bits()
        };

        bits.hash(hasher);
    }
}

/// Text attributes
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Attrs<'a> {
    //TODO: should this be an option?
    pub color_opt: Option<Color>,
    pub family: Family<'a>,
    /// A specific database face selected by the caller. Font fallback remains
    /// available when that face does not contain a requested glyph.
    pub font_id_opt: Option<fontdb::ID>,
    /// High-level CSS `font-weight` coordinate, applied only when the actual
    /// shaped face provides a `wght` axis.
    pub font_weight_axis_opt: Option<VariationValue>,
    /// Automatic optical-size coordinate derived from the used font size.
    /// `None` represents `font-optical-sizing: none`.
    pub font_optical_size_opt: Option<VariationValue>,
    /// Whether high-level font style requests an italic variable instance.
    pub font_italic_axis: bool,
    pub stretch: Stretch,
    pub style: Style,
    pub weight: Weight,
    pub metadata: usize,
    pub cache_key_flags: CacheKeyFlags,
    pub metrics_opt: Option<CacheMetrics>,
    /// Letter spacing (tracking) in EM
    pub letter_spacing_opt: Option<LetterSpacing>,
    pub font_features: FontFeatures,
    pub font_variations: FontVariations,
    /// Optional browser-grade line-breaking policy for this text span.
    pub css_line_break: Option<CssLineBreak>,
}

impl<'a> Attrs<'a> {
    /// Create a new set of attributes with sane defaults
    ///
    /// This defaults to a regular Sans-Serif font.
    pub fn new() -> Self {
        Self {
            color_opt: None,
            family: Family::SansSerif,
            font_id_opt: None,
            font_weight_axis_opt: None,
            font_optical_size_opt: None,
            font_italic_axis: false,
            stretch: Stretch::Normal,
            style: Style::Normal,
            weight: Weight::NORMAL,
            metadata: 0,
            cache_key_flags: CacheKeyFlags::empty(),
            metrics_opt: None,
            letter_spacing_opt: None,
            font_features: FontFeatures::new(),
            font_variations: FontVariations::new(),
            css_line_break: None,
        }
    }

    /// Set [Color]
    pub fn color(mut self, color: Color) -> Self {
        self.color_opt = Some(color);
        self
    }

    /// Set [Family]
    pub fn family(mut self, family: Family<'a>) -> Self {
        self.family = family;
        self
    }

    /// Prefer one exact database face before ordinary family matching.
    pub fn font_id(mut self, font_id: fontdb::ID) -> Self {
        self.font_id_opt = Some(font_id);
        self
    }

    pub fn font_weight_axis(mut self, value: f32) -> Self {
        self.font_weight_axis_opt = Some(VariationValue(value));
        self
    }

    pub fn font_optical_size(mut self, value: f32) -> Self {
        self.font_optical_size_opt = Some(VariationValue(value));
        self
    }

    pub fn font_italic_axis(mut self, italic: bool) -> Self {
        self.font_italic_axis = italic;
        self
    }

    /// Set [Stretch]
    pub fn stretch(mut self, stretch: Stretch) -> Self {
        self.stretch = stretch;
        self
    }

    /// Set [Style]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set [Weight]
    pub fn weight(mut self, weight: Weight) -> Self {
        self.weight = weight;
        self
    }

    /// Set metadata
    pub fn metadata(mut self, metadata: usize) -> Self {
        self.metadata = metadata;
        self
    }

    /// Set [`CacheKeyFlags`]
    pub fn cache_key_flags(mut self, cache_key_flags: CacheKeyFlags) -> Self {
        self.cache_key_flags = cache_key_flags;
        self
    }

    /// Set [`Metrics`], overriding values in buffer
    pub fn metrics(mut self, metrics: Metrics) -> Self {
        self.metrics_opt = Some(metrics.into());
        self
    }

    /// Set letter spacing (tracking) in EM
    pub fn letter_spacing(mut self, letter_spacing: f32) -> Self {
        self.letter_spacing_opt = Some(LetterSpacing(letter_spacing));
        self
    }

    /// Set [`FontFeatures`]
    pub fn font_features(mut self, font_features: FontFeatures) -> Self {
        self.font_features = font_features;
        self
    }

    /// Set all variation coordinates used while shaping.
    pub fn font_variations(mut self, font_variations: FontVariations) -> Self {
        self.font_variations = font_variations;
        self
    }

    /// Set one variation coordinate used while shaping.
    pub fn font_variation(mut self, tag: VariationTag, value: f32) -> Self {
        self.font_variations.set(tag, value);
        self
    }

    /// Attach CSS line-breaking behavior to this span.
    pub fn css_line_break(mut self, policy: CssLineBreak) -> Self {
        self.css_line_break = Some(policy);
        self
    }

    /// Check if font matches
    pub fn matches(&self, face: &fontdb::FaceInfo) -> bool {
        //TODO: smarter way of including emoji
        face.post_script_name.contains("Emoji")
            || (face.style == self.style && face.stretch == self.stretch)
    }

    /// Check if this set of attributes can be shaped with another
    pub fn compatible(&self, other: &Self) -> bool {
        self.family == other.family
            && self.font_id_opt == other.font_id_opt
            && self.font_weight_axis_opt == other.font_weight_axis_opt
            && self.font_optical_size_opt == other.font_optical_size_opt
            && self.font_italic_axis == other.font_italic_axis
            && self.stretch == other.stretch
            && self.style == other.style
            && self.weight == other.weight
            && self.font_variations == other.font_variations
    }
}

/// Font-specific part of [`Attrs`] to be used for matching
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FontMatchAttrs {
    family: FamilyOwned,
    stretch: Stretch,
    style: Style,
    weight: Weight,
}

impl<'a> From<&Attrs<'a>> for FontMatchAttrs {
    fn from(attrs: &Attrs<'a>) -> Self {
        Self {
            family: FamilyOwned::new(attrs.family),
            stretch: attrs.stretch,
            style: attrs.style,
            weight: attrs.weight,
        }
    }
}

/// An owned version of [`Attrs`]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AttrsOwned {
    //TODO: should this be an option?
    pub color_opt: Option<Color>,
    pub family_owned: FamilyOwned,
    pub font_id_opt: Option<fontdb::ID>,
    pub font_weight_axis_opt: Option<VariationValue>,
    pub font_optical_size_opt: Option<VariationValue>,
    pub font_italic_axis: bool,
    pub stretch: Stretch,
    pub style: Style,
    pub weight: Weight,
    pub metadata: usize,
    pub cache_key_flags: CacheKeyFlags,
    pub metrics_opt: Option<CacheMetrics>,
    /// Letter spacing (tracking) in EM
    pub letter_spacing_opt: Option<LetterSpacing>,
    pub font_features: FontFeatures,
    pub font_variations: FontVariations,
    pub css_line_break: Option<CssLineBreak>,
}

impl AttrsOwned {
    pub fn new(attrs: &Attrs) -> Self {
        Self {
            color_opt: attrs.color_opt,
            family_owned: FamilyOwned::new(attrs.family),
            font_id_opt: attrs.font_id_opt,
            font_weight_axis_opt: attrs.font_weight_axis_opt,
            font_optical_size_opt: attrs.font_optical_size_opt,
            font_italic_axis: attrs.font_italic_axis,
            stretch: attrs.stretch,
            style: attrs.style,
            weight: attrs.weight,
            metadata: attrs.metadata,
            cache_key_flags: attrs.cache_key_flags,
            metrics_opt: attrs.metrics_opt,
            letter_spacing_opt: attrs.letter_spacing_opt,
            font_features: attrs.font_features.clone(),
            font_variations: attrs.font_variations.clone(),
            css_line_break: attrs.css_line_break,
        }
    }

    pub fn as_attrs(&self) -> Attrs {
        Attrs {
            color_opt: self.color_opt,
            family: self.family_owned.as_family(),
            font_id_opt: self.font_id_opt,
            font_weight_axis_opt: self.font_weight_axis_opt,
            font_optical_size_opt: self.font_optical_size_opt,
            font_italic_axis: self.font_italic_axis,
            stretch: self.stretch,
            style: self.style,
            weight: self.weight,
            metadata: self.metadata,
            cache_key_flags: self.cache_key_flags,
            metrics_opt: self.metrics_opt,
            letter_spacing_opt: self.letter_spacing_opt,
            font_features: self.font_features.clone(),
            font_variations: self.font_variations.clone(),
            css_line_break: self.css_line_break,
        }
    }
}

/// List of text attributes to apply to a line
//TODO: have this clean up the spans when changes are made
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AttrsList {
    defaults: AttrsOwned,
    pub(crate) spans: RangeMap<usize, AttrsOwned>,
}

impl AttrsList {
    /// Create a new attributes list with a set of default [Attrs]
    pub fn new(defaults: &Attrs) -> Self {
        Self {
            defaults: AttrsOwned::new(defaults),
            spans: RangeMap::new(),
        }
    }

    /// Get the default [Attrs]
    pub fn defaults(&self) -> Attrs {
        self.defaults.as_attrs()
    }

    /// Get the current attribute spans
    pub fn spans(&self) -> Vec<(&Range<usize>, &AttrsOwned)> {
        self.spans_iter().collect()
    }

    /// Get an iterator over the current attribute spans
    pub fn spans_iter(&self) -> impl Iterator<Item = (&Range<usize>, &AttrsOwned)> + '_ {
        self.spans.iter()
    }

    /// Clear the current attribute spans
    pub fn clear_spans(&mut self) {
        self.spans.clear();
    }

    /// Add an attribute span, removes any previous matching parts of spans
    pub fn add_span(&mut self, range: Range<usize>, attrs: &Attrs) {
        //do not support 1..1 or 2..1 even if by accident.
        if range.is_empty() {
            return;
        }

        self.spans.insert(range, AttrsOwned::new(attrs));
    }

    /// Get the attribute span for an index
    ///
    /// This returns a span that contains the index
    pub fn get_span(&self, index: usize) -> Attrs {
        self.spans
            .get(&index)
            .map(|v| v.as_attrs())
            .unwrap_or(self.defaults.as_attrs())
    }

    /// Split attributes list at an offset
    #[allow(clippy::missing_panics_doc)]
    pub fn split_off(&mut self, index: usize) -> Self {
        let mut new = Self::new(&self.defaults.as_attrs());
        let mut removes = Vec::new();

        //get the keys we need to remove or fix.
        for span in self.spans.iter() {
            if span.0.end <= index {
                continue;
            } else if span.0.start >= index {
                removes.push((span.0.clone(), false));
            } else {
                removes.push((span.0.clone(), true));
            }
        }

        for (key, resize) in removes {
            let (range, attrs) = self
                .spans
                .get_key_value(&key.start)
                .map(|v| (v.0.clone(), v.1.clone()))
                .expect("attrs span not found");
            self.spans.remove(key);

            if resize {
                new.spans.insert(0..range.end - index, attrs.clone());
                self.spans.insert(range.start..index, attrs);
            } else {
                new.spans
                    .insert(range.start - index..range.end - index, attrs);
            }
        }
        new
    }

    /// Resets the attributes with new defaults.
    pub(crate) fn reset(mut self, default: &Attrs) -> Self {
        self.defaults = AttrsOwned::new(default);
        self.spans.clear();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{Attrs, AttrsOwned, FontVariations, VariationTag};
    use core::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;

    fn hash(value: impl Hash) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn variation_coordinates_round_trip_and_change_shape_identity() {
        let defaults = Attrs::new();
        let varied = Attrs::new()
            .font_variation(VariationTag::new(b"opsz"), 32.0)
            .font_variation(VariationTag::new(b"wght"), 500.0);

        assert!(!defaults.compatible(&varied));
        assert_ne!(AttrsOwned::new(&defaults), AttrsOwned::new(&varied));
        assert_ne!(
            hash(AttrsOwned::new(&defaults)),
            hash(AttrsOwned::new(&varied))
        );
        assert_eq!(AttrsOwned::new(&varied).as_attrs(), varied);
    }

    #[test]
    fn setting_an_axis_twice_replaces_its_coordinate() {
        let mut variations = FontVariations::new();
        variations
            .set(VariationTag::new(b"opsz"), 14.0)
            .set(VariationTag::new(b"opsz"), 32.0);

        assert_eq!(variations.len(), 1);
        assert_eq!(
            variations.iter().next().map(|variation| variation.value.0),
            Some(32.0)
        );
    }

    #[test]
    fn variation_order_does_not_change_shape_identity() {
        let left = Attrs::new()
            .font_variation(VariationTag::new(b"wght"), 500.0)
            .font_variation(VariationTag::new(b"opsz"), 32.0);
        let right = Attrs::new()
            .font_variation(VariationTag::new(b"opsz"), 32.0)
            .font_variation(VariationTag::new(b"wght"), 500.0);

        assert_eq!(left, right);
        assert!(left.compatible(&right));
        assert_eq!(hash(AttrsOwned::new(&left)), hash(AttrsOwned::new(&right)));
    }
}
