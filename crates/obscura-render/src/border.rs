//! Typed CSS border and outline state shared by style, layout, and paint.
//!
//! CSS keeps the specified border width even while `none` or `hidden` makes
//! the *used* width zero. Keeping those two values distinct is what lets a
//! later `border-style: solid` restore the previously specified width without
//! corrupting box geometry in the meantime.

/// CSS's initial `medium` border and outline width in CSS pixels.
pub const MEDIUM_BORDER_WIDTH: f32 = 3.0;

/// Four physical sides in CSS top-right-bottom-left order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sides<T> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

impl<T: Copy> Sides<T> {
    pub const fn all(value: T) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Sides<U> {
        Sides {
            top: f(self.top),
            right: f(self.right),
            bottom: f(self.bottom),
            left: f(self.left),
        }
    }

    pub const fn as_array(self) -> [T; 4] {
        [self.top, self.right, self.bottom, self.left]
    }
}

impl<T: Default> Default for Sides<T> {
    fn default() -> Self {
        Self {
            top: T::default(),
            right: T::default(),
            bottom: T::default(),
            left: T::default(),
        }
    }
}

/// Expand a CSS 1--4 value list in top-right-bottom-left order.
pub fn expand_sides<T: Copy>(values: &[T]) -> Option<Sides<T>> {
    match values {
        [all] => Some(Sides::all(*all)),
        [vertical, horizontal] => Some(Sides {
            top: *vertical,
            right: *horizontal,
            bottom: *vertical,
            left: *horizontal,
        }),
        [top, horizontal, bottom] => Some(Sides {
            top: *top,
            right: *horizontal,
            bottom: *bottom,
            left: *horizontal,
        }),
        [top, right, bottom, left] => Some(Sides {
            top: *top,
            right: *right,
            bottom: *bottom,
            left: *left,
        }),
        _ => None,
    }
}

/// The CSS border-line styles represented by the renderer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BorderStyle {
    #[default]
    None,
    Hidden,
    Dotted,
    Dashed,
    Solid,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
    /// `outline-style:auto`; border sides themselves never use this value.
    Auto,
}

impl BorderStyle {
    /// Whether this style contributes a used width and visible border paint.
    pub const fn is_visible(self) -> bool {
        !matches!(self, Self::None | Self::Hidden)
    }

    pub const fn css_name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Hidden => "hidden",
            Self::Dotted => "dotted",
            Self::Dashed => "dashed",
            Self::Solid => "solid",
            Self::Double => "double",
            Self::Groove => "groove",
            Self::Ridge => "ridge",
            Self::Inset => "inset",
            Self::Outset => "outset",
            Self::Auto => "auto",
        }
    }
}

/// Four independent specified border widths, styles, and colors.
///
/// A `None` color means CSS `currentcolor`, not transparent or absent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderModel {
    pub specified_widths: Sides<f32>,
    pub styles: Sides<BorderStyle>,
    pub colors: Sides<Option<[u8; 4]>>,
    pub radii: BorderRadii,
}

impl Default for BorderModel {
    fn default() -> Self {
        Self {
            specified_widths: Sides::all(MEDIUM_BORDER_WIDTH),
            styles: Sides::all(BorderStyle::None),
            colors: Sides::all(None),
            radii: BorderRadii::default(),
        }
    }
}

impl BorderModel {
    /// Used widths after applying CSS Backgrounds 3's `none`/`hidden` rule.
    pub fn used_widths(self) -> Sides<f32> {
        Sides {
            top: used_width(self.specified_widths.top, self.styles.top),
            right: used_width(self.specified_widths.right, self.styles.right),
            bottom: used_width(self.specified_widths.bottom, self.styles.bottom),
            left: used_width(self.specified_widths.left, self.styles.left),
        }
    }
}

fn used_width(specified: f32, style: BorderStyle) -> f32 {
    if style.is_visible() {
        specified.max(0.0)
    } else {
        0.0
    }
}

/// Uniform outline state. Outlines paint outside the border edge and never
/// participate in layout geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutlineModel {
    pub specified_width: f32,
    pub style: BorderStyle,
    /// `None` is CSS `currentcolor`.
    pub color: Option<[u8; 4]>,
    pub offset: f32,
}

impl Default for OutlineModel {
    fn default() -> Self {
        Self {
            specified_width: MEDIUM_BORDER_WIDTH,
            style: BorderStyle::None,
            color: None,
            offset: 0.0,
        }
    }
}

impl OutlineModel {
    pub fn used_width(self) -> f32 {
        used_width(self.specified_width, self.style)
    }
}

/// One CSS `<length-percentage>` component of a corner radius.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RadiusValue {
    pub length: f32,
    /// Fraction of the corresponding border-box axis (`0.5` is `50%`).
    pub percentage: f32,
}

impl RadiusValue {
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

    pub fn resolve(self, axis: f32) -> f32 {
        (self.length + self.percentage * axis).max(0.0)
    }

    pub fn is_zero(self) -> bool {
        self.length == 0.0 && self.percentage == 0.0
    }
}

/// Horizontal and vertical radii for one corner.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CornerRadius {
    pub x: RadiusValue,
    pub y: RadiusValue,
}

impl CornerRadius {
    pub const fn circular(value: RadiusValue) -> Self {
        Self { x: value, y: value }
    }
}

/// Four elliptical radii in clockwise order from the top-left corner.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BorderRadii {
    pub top_left: CornerRadius,
    pub top_right: CornerRadius,
    pub bottom_right: CornerRadius,
    pub bottom_left: CornerRadius,
}

impl BorderRadii {
    pub const fn all(radius: CornerRadius) -> Self {
        Self {
            top_left: radius,
            top_right: radius,
            bottom_right: radius,
            bottom_left: radius,
        }
    }

    /// Resolve percentages and apply CSS Backgrounds 3's corner-overlap rule.
    /// One common scale factor preserves every ellipse's aspect ratio.
    pub fn resolve(self, width: f32, height: f32) -> ResolvedBorderRadii {
        let mut resolved = ResolvedBorderRadii {
            top_left: resolve_corner(self.top_left, width, height),
            top_right: resolve_corner(self.top_right, width, height),
            bottom_right: resolve_corner(self.bottom_right, width, height),
            bottom_left: resolve_corner(self.bottom_left, width, height),
        };
        let ratio = |available: f32, requested: f32| {
            if requested > 0.0 {
                available.max(0.0) / requested
            } else {
                1.0
            }
        };
        let scale = 1.0f32
            .min(ratio(width, resolved.top_left.0 + resolved.top_right.0))
            .min(ratio(width, resolved.bottom_left.0 + resolved.bottom_right.0))
            .min(ratio(height, resolved.top_left.1 + resolved.bottom_left.1))
            .min(ratio(height, resolved.top_right.1 + resolved.bottom_right.1));
        if scale < 1.0 {
            resolved = resolved.scaled(scale);
        }
        resolved
    }

    pub fn is_zero(self) -> bool {
        self.top_left.x.is_zero()
            && self.top_left.y.is_zero()
            && self.top_right.x.is_zero()
            && self.top_right.y.is_zero()
            && self.bottom_right.x.is_zero()
            && self.bottom_right.y.is_zero()
            && self.bottom_left.x.is_zero()
            && self.bottom_left.y.is_zero()
    }
}

fn resolve_corner(radius: CornerRadius, width: f32, height: f32) -> (f32, f32) {
    let x = radius.x.resolve(width);
    let y = radius.y.resolve(height);
    // CSS treats a corner as square when either axis has a zero radius.
    if x <= f32::EPSILON || y <= f32::EPSILON {
        (0.0, 0.0)
    } else {
        (x, y)
    }
}

/// Final used radii in CSS pixels after percentage resolution and overlap
/// scaling.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ResolvedBorderRadii {
    pub top_left: (f32, f32),
    pub top_right: (f32, f32),
    pub bottom_right: (f32, f32),
    pub bottom_left: (f32, f32),
}

impl ResolvedBorderRadii {
    pub fn scaled(self, scale: f32) -> Self {
        let scale_corner = |(x, y): (f32, f32)| (x * scale, y * scale);
        Self {
            top_left: scale_corner(self.top_left),
            top_right: scale_corner(self.top_right),
            bottom_right: scale_corner(self.bottom_right),
            bottom_left: scale_corner(self.bottom_left),
        }
    }

    pub fn is_zero(self) -> bool {
        self.top_left == (0.0, 0.0)
            && self.top_right == (0.0, 0.0)
            && self.bottom_right == (0.0, 0.0)
            && self.bottom_left == (0.0, 0.0)
    }

    pub fn is_uniform(self) -> bool {
        self.top_left == self.top_right
            && self.top_right == self.bottom_right
            && self.bottom_right == self.bottom_left
    }

    /// Inner radii after removing the used border widths on each axis.
    pub fn inset(self, widths: Sides<f32>) -> Self {
        Self {
            top_left: shrink_corner(self.top_left, widths.left, widths.top),
            top_right: shrink_corner(self.top_right, widths.right, widths.top),
            bottom_right: shrink_corner(
                self.bottom_right,
                widths.right,
                widths.bottom,
            ),
            bottom_left: shrink_corner(self.bottom_left, widths.left, widths.bottom),
        }
    }

    /// Outer radii for an outline or shadow expanded from the border edge.
    pub fn outset(self, amounts: Sides<f32>) -> Self {
        Self {
            top_left: grow_corner(self.top_left, amounts.left, amounts.top),
            top_right: grow_corner(self.top_right, amounts.right, amounts.top),
            bottom_right: grow_corner(
                self.bottom_right,
                amounts.right,
                amounts.bottom,
            ),
            bottom_left: grow_corner(self.bottom_left, amounts.left, amounts.bottom),
        }
    }
}

fn shrink_corner((x, y): (f32, f32), horizontal: f32, vertical: f32) -> (f32, f32) {
    let x = (x - horizontal).max(0.0);
    let y = (y - vertical).max(0.0);
    if x <= f32::EPSILON || y <= f32::EPSILON {
        (0.0, 0.0)
    } else {
        (x, y)
    }
}

fn grow_corner((x, y): (f32, f32), horizontal: f32, vertical: f32) -> (f32, f32) {
    // Expanding a square corner must keep it square. Only an existing curve
    // receives the extra outline/shadow distance on each axis.
    if x <= f32::EPSILON || y <= f32::EPSILON {
        return (0.0, 0.0);
    }
    let x = (x + horizontal).max(0.0);
    let y = (y + vertical).max(0.0);
    if x <= f32::EPSILON || y <= f32::EPSILON {
        (0.0, 0.0)
    } else {
        (x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_expansion_follows_css_trbl_rules() {
        assert_eq!(expand_sides(&[1]), Some(Sides::all(1)));
        assert_eq!(
            expand_sides(&[1, 2, 3]),
            Some(Sides {
                top: 1,
                right: 2,
                bottom: 3,
                left: 2,
            })
        );
        assert_eq!(expand_sides::<i32>(&[]), None);
        assert_eq!(expand_sides(&[1, 2, 3, 4, 5]), None);
    }

    #[test]
    fn hidden_style_zeroes_only_the_used_width() {
        let mut border = BorderModel::default();
        border.specified_widths = Sides::all(10.0);
        border.styles = Sides::all(BorderStyle::None);
        assert_eq!(border.used_widths(), Sides::all(0.0));
        border.styles.left = BorderStyle::Solid;
        assert_eq!(border.used_widths().left, 10.0);
        assert_eq!(border.specified_widths.left, 10.0);
    }

    #[test]
    fn elliptical_radii_use_one_overlap_scale() {
        let radii = BorderRadii {
            top_left: CornerRadius {
                x: RadiusValue::pixels(80.0),
                y: RadiusValue::pixels(50.0),
            },
            top_right: CornerRadius {
                x: RadiusValue::pixels(60.0),
                y: RadiusValue::pixels(40.0),
            },
            bottom_right: CornerRadius {
                x: RadiusValue::pixels(40.0),
                y: RadiusValue::pixels(30.0),
            },
            bottom_left: CornerRadius {
                x: RadiusValue::pixels(20.0),
                y: RadiusValue::pixels(10.0),
            },
        };
        let resolved = radii.resolve(100.0, 50.0);
        let scale = 5.0 / 7.0;
        assert!((resolved.top_left.0 - 80.0 * scale).abs() < 0.001);
        assert!((resolved.top_left.1 - 50.0 * scale).abs() < 0.001);
        assert!((resolved.bottom_right.0 - 40.0 * scale).abs() < 0.001);
        assert!((resolved.bottom_left.1 - 10.0 * scale).abs() < 0.001);
    }
}
