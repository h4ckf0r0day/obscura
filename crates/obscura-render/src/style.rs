//! Computed-style lite: parse inline CSS declarations plus a small UA default
//! sheet into the layout-relevant subset of [`crate::LayoutStyle`].
//!
//! This is deliberately not a full CSS cascade. It handles the properties that
//! influence phase-1 box layout (display, width/height, margin, padding, border
//! width) from inline `style="..."` attributes, layered on a tiny built-in UA
//! sheet. A real cascade with selector matching and `cssparser` can harden this
//! later; for inline-style attributes a compact tokenizer is enough and keeps
//! the crate dependency-light.

use crate::{Display, Edges, LayoutStyle};
use std::collections::BTreeMap;

/// Compute the layout-relevant style for an element: UA defaults for its tag,
/// overridden by its inline `style="..."` declarations.
pub fn compute_style(tag: &str, inline_css: Option<&str>) -> LayoutStyle {
    let mut style = ua_style(tag);
    if let Some(css) = inline_css {
        apply_inline(&mut style, css);
    }
    style
}

/// Built-in UA defaults. Inline elements currently map to block layout; real
/// inline/text layout arrives with the text/paint phase.
pub fn ua_style(tag: &str) -> LayoutStyle {
    let mut style = LayoutStyle::default();
    if tag == "b" || tag == "strong" {
        style.font_weight = Some("bold".into());
    }

    style.display = match tag {
        // Phrasing / inline-level content defaults to inline so a paragraph
        // that mixes these with text stays one inline formatting context that
        // cosmic-text can shape and wrap as a whole, instead of each element
        // becoming its own block box (which forces the flex word-promotion
        // fallback and its fragile one-word-per-line wrapping). Author CSS
        // (e.g. `code{display:block}`) still overrides this in the cascade.
        "span" | "a" | "b" | "i" | "strong" | "em" | "font" | "code" | "small" | "sub" | "sup"
        | "mark" | "abbr" | "cite" | "var" | "dfn" | "kbd" | "samp" | "q" | "time" | "s" | "u"
        | "del" | "ins" | "tt" | "big" | "bdi" | "bdo" | "br" | "wbr" | "data" | "output"
        | "label" | "ruby" | "rt" | "rp" => Display::Inline,
        "tr" => Display::Flex,
        _ => Display::Block,
    };
    if tag == "slot" {
        // HTML's UA sheet makes a slot transparent to box generation; its
        // assigned nodes or fallback children participate at the slot's
        // position in the flattened tree. Author `display` declarations may
        // still replace this default through the normal declaration parser.
        style.display = Display::Block;
        style.display_contents = true;
    } else if tag == "center" {
        // Browser UA sheets keep <center> block-level and give it a special
        // inherited text alignment which also centers fixed-width block
        // descendants. Keep that provenance separate from ordinary authored
        // text-align:center.
        style.text_align = Some(taffy::AlignItems::CENTER);
        style.legacy_center = true;
    } else if tag == "head"
        || tag == "script"
        || tag == "style"
        || tag == "title"
        || tag == "meta"
        || tag == "link"
        || tag == "noscript"
        || tag == "template"
        || tag == "desc"
        || tag == "metadata"
        || tag == "option"
        || tag == "optgroup"
        || tag == "source"
        || tag == "track"
        || tag == "param"
        || tag == "area"
    {
        // `noscript` content is only for scripting-disabled agents; with JS on
        // (as here) the parser keeps it as raw text and the browser hides it,
        // so a site's no-JS nav fallback must not paint as literal markup.
        // `template` content is inert and never rendered. svg `title`/`desc`/
        // `metadata` are AX/tooltip metadata, never rendered in flow (an inline
        // <svg> we cannot rasterize would otherwise leak its `<desc>` text).
        // `option`/`optgroup` render only inside the native select popup, so a
        // closed <select> must not paint every option label stacked.
        // `source`/`track`/`param`/`area` are metadata-only children of
        // picture/video/object/map; a `<picture><source width= height=>` must
        // not lay out as a real box (news CDNs put dimensions on `<source>`,
        // which otherwise paints an empty box the size of the image).
        style.display = crate::Display::None;
    } else if tag == "pre" {
        // HTML's UA sheet preserves source whitespace in preformatted blocks.
        style.white_space = Some(crate::WhiteSpace::Pre);
    } else if tag == "body" {
        style.margin = Edges {
            top: 8.0,
            right: 8.0,
            bottom: 8.0,
            left: 8.0,
        };
    } else if tag == "h1" {
        style.font_size = None;
        style.font_size_raw = Some(crate::Dimension::Em(2.0));
        style.font_weight = Some("bold".to_string());
        style.margin_relative[0] = Some(crate::Dimension::Em(0.67));
        style.margin_relative[2] = Some(crate::Dimension::Em(0.67));
    } else if tag == "h2" {
        style.font_size = None;
        style.font_size_raw = Some(crate::Dimension::Em(1.5));
        style.font_weight = Some("bold".to_string());
        style.margin_relative[0] = Some(crate::Dimension::Em(0.83));
        style.margin_relative[2] = Some(crate::Dimension::Em(0.83));
    } else if tag == "h3" {
        style.font_size = None;
        style.font_size_raw = Some(crate::Dimension::Em(1.17));
        style.font_weight = Some("bold".to_string());
        style.margin_relative[0] = Some(crate::Dimension::Em(1.0));
        style.margin_relative[2] = Some(crate::Dimension::Em(1.0));
    } else if tag == "h4" {
        style.font_size = None;
        style.font_size_raw = Some(crate::Dimension::Em(1.0));
        style.font_weight = Some("bold".to_string());
        style.margin_relative[0] = Some(crate::Dimension::Em(1.33));
        style.margin_relative[2] = Some(crate::Dimension::Em(1.33));
    } else if tag == "h5" {
        style.font_size = None;
        style.font_size_raw = Some(crate::Dimension::Em(0.83));
        style.font_weight = Some("bold".to_string());
        style.margin_relative[0] = Some(crate::Dimension::Em(1.67));
        style.margin_relative[2] = Some(crate::Dimension::Em(1.67));
    } else if tag == "h6" {
        style.font_size = None;
        style.font_size_raw = Some(crate::Dimension::Em(0.67));
        style.font_weight = Some("bold".to_string());
        style.margin_relative[0] = Some(crate::Dimension::Em(2.33));
        style.margin_relative[2] = Some(crate::Dimension::Em(2.33));
    } else if matches!(tag, "p" | "dl" | "ul" | "ol" | "menu" | "dir") {
        style.margin_relative[0] = Some(crate::Dimension::Em(1.0));
        style.margin_relative[2] = Some(crate::Dimension::Em(1.0));
        if matches!(tag, "ul" | "menu" | "dir") {
            style.list_style = Some(crate::ListStyle::Disc);
            style.padding.left = 40.0;
        } else if tag == "ol" {
            style.list_style = Some(crate::ListStyle::Decimal);
            style.padding.left = 40.0;
        }
    } else if tag == "b" || tag == "strong" {
        style.font_weight = Some("bold".to_string());
    } else if tag == "i"
        || tag == "em"
        || tag == "cite"
        || tag == "var"
        || tag == "dfn"
        || tag == "address"
    {
        style.font_style_italic = Some(true);
    } else if tag == "a" {
        style.color = Some([0, 0, 238, 255]); // blue
        style.underline = Some(true); // UA default: links are underlined
    } else if tag == "iframe" {
        // HTML's UA sheet gives frames a two-pixel inset border. The paint
        // and geometry models share the same four-side used state.
        style.border = Edges {
            top: 2.0,
            right: 2.0,
            bottom: 2.0,
            left: 2.0,
        };
        style.border_model.specified_widths = crate::Sides::all(2.0);
        style.border_model.styles = crate::Sides::all(crate::BorderStyle::Inset);
    } else if tag == "button" {
        // HTML buttons are inline-block controls whose anonymous inner
        // content is centered by the browser UA sheet. Keep the atomic outer
        // participation separate from the internal text alignment: authored
        // display/text-align declarations still cascade over both values.
        style.display = Display::Inline;
        style.is_inline_block = true;
        style.text_align = Some(taffy::AlignItems::CENTER);
        style.box_sizing = crate::BoxSizing::BorderBox;
        // Chromium's native button content box keeps one pixel of block-axis
        // and six pixels of inline-axis padding. Framework resets commonly
        // remove the native border/background but intentionally retain this
        // padding; percentage-height icon children therefore resolve against
        // the padded content box (44px border box -> 42px content box).
        style.padding = Edges {
            top: 1.0,
            right: 6.0,
            bottom: 1.0,
            left: 6.0,
        };
    } else if tag == "select" {
        // A closed native select is an atomic inline control. Its option
        // children belong to the popup (and remain display:none in our frame
        // tree); the select itself gets intrinsic geometry and paints the
        // selected label.
        style.display = Display::Inline;
        style.is_inline_block = true;
        style.font_size = Some(13.333_333);
        style.font_family = Some("arial".to_string());
        style.line_height = Some(crate::LineHeight::Normal);
        style.padding = Edges {
            top: 1.0,
            right: 20.0,
            bottom: 1.0,
            left: 2.0,
        };
        style.border = Edges {
            top: 1.0,
            right: 1.0,
            bottom: 1.0,
            left: 1.0,
        };
        style.border_model.specified_widths = crate::Sides::all(1.0);
        style.border_model.styles = crate::Sides::all(crate::BorderStyle::Solid);
        style.border_model.colors = crate::Sides::all(Some([118, 118, 118, 255]));
        style.border_color = Some([118, 118, 118, 255]);
        style.background_color = Some([255, 255, 255, 255]);
    } else if tag == "input" {
        // Native text controls are atomic inline-level boxes with their own
        // platform font and intrinsic border-box dimensions; they do not
        // inherit the page's font shorthand by default. The declared CSS box
        // remains content-box in standards mode and switches in quirks mode.
        // Size-dependent geometry is resolved after cascading, once the input
        // type and `size` attribute are available (dom::layout_dom).
        style.display = Display::Inline;
        style.is_inline_block = true;
        style.font_size = Some(13.333_333);
        style.font_family = Some("arial".to_string());
        style.line_height = Some(crate::LineHeight::Normal);
        style.padding = Edges {
            top: 1.0,
            right: 2.0,
            bottom: 1.0,
            left: 2.0,
        };
        style.border = Edges {
            top: 2.0,
            right: 2.0,
            bottom: 2.0,
            left: 2.0,
        };
        style.border_model.specified_widths = crate::Sides::all(2.0);
        style.border_model.styles = crate::Sides::all(crate::BorderStyle::Solid);
        style.border_model.colors = crate::Sides::all(Some([118, 118, 118, 255]));
        style.border_color = Some([118, 118, 118, 255]);
        style.background_color = Some([255, 255, 255, 255]);
    } else if tag == "textarea" {
        // A native textarea is an atomic inline-block control whose
        // intrinsic box comes from rows/cols (resolved in dom.rs once the
        // attributes are readable), not from its text content. Unlike the
        // other controls it keeps its content in the tree (the value is the
        // text child), laid out inside the sized box; line breaks in the
        // value must survive, hence pre-wrap. Chromium defaults the control
        // to the fixed-pitch face and to border-box sizing.
        style.display = Display::Inline;
        style.is_inline_block = true;
        style.font_size = Some(13.333_333);
        style.font_family = Some("monospace".to_string());
        style.line_height = Some(crate::LineHeight::Normal);
        style.white_space = Some(crate::WhiteSpace::PreWrap);
        style.box_sizing = crate::BoxSizing::BorderBox;
        style.padding = Edges {
            top: 2.0,
            right: 2.0,
            bottom: 2.0,
            left: 2.0,
        };
        style.border = Edges {
            top: 1.0,
            right: 1.0,
            bottom: 1.0,
            left: 1.0,
        };
        style.border_model.specified_widths = crate::Sides::all(1.0);
        style.border_model.styles = crate::Sides::all(crate::BorderStyle::Solid);
        style.border_model.colors = crate::Sides::all(Some([118, 118, 118, 255]));
        style.border_color = Some([118, 118, 118, 255]);
        style.background_color = Some([255, 255, 255, 255]);
    } else if matches!(tag, "table" | "tbody" | "thead" | "tfoot") {
        style.display = Display::Flex;
        style.internal_flex_container = true;
        style.flex_direction = Some(taffy::FlexDirection::Column);
        style.align_items = Some(taffy::AlignItems::STRETCH); // stretch rows to fill table width
                                                              // Rows fill the table width and may shrink below their content's
                                                              // min-content size (the flexbox automatic-minimum-size gotcha), so a
                                                              // width-constrained taxobox contains its content instead of blowing
                                                              // out sideways. (Fully matching CSS auto table layout, where a table
                                                              // grows to fit unshrinkable content, needs real table layout.)
        style.min_width = crate::Dimension::Px(0.0);
        if tag == "table" {
            style.is_table_box = true;
            // Chromium's HTML UA sheet makes the table grid border-box and
            // supplies the traditional two-pixel separate-border spacing.
            // Author declarations and the legacy `cellspacing` hint cascade
            // over these values.
            style.box_sizing = crate::BoxSizing::BorderBox;
            style.border_spacing = Some((2.0, 2.0));
            style.border_collapse = Some(false);
        } else {
            style.width = crate::Dimension::Percent(1.0);
            // The row-group UA rule is the source of the effective default
            // middle alignment; rows and cells inherit it below.
            style.vertical_align = Some(crate::VerticalAlign::Middle);
        }
    } else if tag == "tr" {
        style.internal_flex_container = true;
        // Rows fill the table width and can shrink below content min-content;
        // this is exactly why Wikipedia's own responsive CSS uses
        // `tr{min-width:100%}`. `align-items:stretch` alone did not pin them
        // once a cell's content (a 250px no-wrap widget) exceeded the box.
        style.min_width = crate::Dimension::Px(0.0);
        style.width = crate::Dimension::Percent(1.0);
    } else if tag == "td" || tag == "th" {
        style.display = Display::Flex;
        style.internal_flex_container = true;
        style.is_table_cell_box = true;
        style.flex_direction = Some(taffy::FlexDirection::Column);
        style.align_items = Some(taffy::AlignItems::FLEX_START);
        style.padding = Edges {
            top: 1.0,
            right: 1.0,
            bottom: 1.0,
            left: 1.0,
        };
        style.min_width = crate::Dimension::Px(0.0);
        if tag == "th" {
            style.font_weight = Some("bold".to_string());
        }
    } else if tag == "img" {
        // Images are inline-level replaced elements by default. Replacement
        // already makes the box atomic; `is_inline_block` is reserved for an
        // authored inline-block outer display and native controls. Chromium's
        // UA sheet does not impose a responsive max-width: author/reset CSS
        // must opt into that. A synthetic `max-width:100%` here distorts
        // deliberately oversized art whose height and intrinsic ratio
        // establish its width.
        style.display = Display::Inline;
    }
    style
}

pub fn apply_inline(style: &mut LayoutStyle, css: &str) {
    let (normal, important) = partition_declarations(css);
    let inherited_scheme = style.color_scheme_dark;
    apply_color_scheme_declarations_from(style, &normal, inherited_scheme);
    apply_color_scheme_declarations_from(style, &important, inherited_scheme);
    apply_declarations_with_locked_color_scheme(style, &normal);
    apply_declarations_with_locked_color_scheme(style, &important);
}

/// Split a declaration block into normal and `!important` declarations while
/// preserving source order inside each priority. The returned declarations
/// have the priority marker removed, ready for the computed-style pass.
pub(crate) fn partition_declarations(css: &str) -> (String, String) {
    let mut normal = String::new();
    let mut important = String::new();
    for raw in split_declarations(css) {
        let decl = raw.trim();
        if decl.is_empty() {
            continue;
        }
        let Some((name, value)) = decl.split_once(':') else {
            continue;
        };
        let mut value = value.trim();
        let mut is_important = false;
        if let Some(bang) = value.rfind('!') {
            if value[bang + 1..].trim().eq_ignore_ascii_case("important") {
                value = value[..bang].trim_end();
                is_important = true;
            }
        }
        let out = if is_important {
            &mut important
        } else {
            &mut normal
        };
        out.push_str(name.trim());
        out.push(':');
        out.push_str(value);
        out.push(';');
    }
    (normal, important)
}

/// Apply only `color-scheme` from a declaration list. The stylesheet cascade
/// runs this over every matching block before resolving color-valued
/// declarations, because `light-dark()` selects from the element's final used
/// scheme regardless of whether `color-scheme` appears before or after the
/// color in source order.
pub(crate) fn apply_color_scheme_declarations_from(
    style: &mut LayoutStyle,
    css: &str,
    inherited_scheme: bool,
) {
    for raw in split_declarations(css) {
        let Some((name, value)) = raw.trim().split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("color-scheme") {
            apply_color_scheme(style, value.trim(), inherited_scheme);
        }
    }
}

pub(crate) fn apply_declarations_with_locked_color_scheme(style: &mut LayoutStyle, css: &str) {
    for raw in split_declarations(css) {
        let decl = raw.trim();
        if decl.is_empty() {
            continue;
        }
        let Some((name, value)) = decl.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        if name != "color-scheme" {
            apply_value(style, &name, value.trim());
        }
    }
}

pub(crate) fn apply_animation_declarations(style: &mut LayoutStyle, css: &str) {
    for raw in split_declarations(css) {
        let Some((name, value)) = raw.trim().split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        if name == "animation" || name.starts_with("animation-") {
            apply_value(style, &name, value.trim());
        }
    }
}

/// Apply one already-cascaded keyframe declaration through the ordinary
/// computed-style parser. The animation sampler deliberately reuses this
/// path instead of maintaining a second, subtly different value parser.
pub(crate) fn apply_animation_property_value(style: &mut LayoutStyle, name: &str, value: &str) {
    apply_value(style, name, value);
}

fn apply_color_scheme(style: &mut LayoutStyle, value: &str, inherited_scheme: bool) {
    let tokens: Vec<String> = value
        .split_whitespace()
        .map(|token| token.to_ascii_lowercase())
        .collect();
    if tokens
        .iter()
        .any(|token| token == "inherit" || token == "unset")
    {
        style.color_scheme_dark = inherited_scheme;
        return;
    }
    if tokens
        .iter()
        .any(|token| matches!(token.as_str(), "initial" | "revert" | "revert-layer"))
    {
        style.color_scheme_dark = false;
        return;
    }
    // The current browser/user preference is light. A scheme list that admits
    // light therefore uses light; a dark-only list uses dark. `normal`,
    // initial/revert, and malformed values retain the default/inherited light
    // behavior used by this compact computed-style model.
    if tokens.iter().any(|token| token == "light") || tokens.iter().any(|token| token == "normal") {
        style.color_scheme_dark = false;
    } else if tokens.iter().any(|token| token == "dark") {
        style.color_scheme_dark = true;
    }
}

/// Split a declaration list on top-level semicolons, respecting `url(...)`
/// and quoted strings. A data: URI (`url(data:image/svg+xml;utf8,...)`, an
/// extremely common way to inline small icon SVGs) or a quoted string
/// (`content: "a; b"`) routinely contains a literal semicolon that is not a
/// declaration separator; splitting on every `;` blindly corrupts the
/// declaration into two malformed halves and silently drops it.
pub(crate) fn split_declarations(css: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut in_quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in css.char_indices() {
        if let Some(q) = in_quote {
            if c == q {
                in_quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => in_quote = Some(c),
            // Track `{...}` too: once @layer bodies are admitted, CSS-nested
            // rules (`&:hover{a:b}`, nested @media) appear inside declaration
            // lists. Keeping a nested block as one chunk makes it a single
            // unparseable declaration that is dropped, rather than leaking its
            // inner declarations into the parent rule at the first `;`.
            '(' | '{' => depth += 1,
            ')' | '}' => depth = (depth - 1).max(0),
            ';' if depth == 0 => {
                parts.push(&css[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&css[start..]);
    parts
}

fn parse_container_type(value: &str) -> Option<crate::ContainerType> {
    match value.trim().to_ascii_lowercase().as_str() {
        "normal" => Some(crate::ContainerType::Normal),
        "inline-size" => Some(crate::ContainerType::InlineSize),
        "size" => Some(crate::ContainerType::Size),
        "initial" | "unset" | "revert" | "revert-layer" => Some(crate::ContainerType::Normal),
        _ => None,
    }
}

fn parse_container_names(value: &str) -> Option<Vec<String>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "none" | "initial" | "unset" | "revert" | "revert-layer"
    ) {
        return Some(Vec::new());
    }
    let mut names = Vec::new();
    let mut input = cssparser::ParserInput::new(value);
    let mut parser = cssparser::Parser::new(&mut input);
    while !parser.is_exhausted() {
        let ident = parser.expect_ident_cloned().ok()?;
        let lower = ident.to_ascii_lowercase();
        if matches!(lower.as_str(), "none" | "not" | "and" | "or") {
            return None;
        }
        names.push(ident.to_string());
    }
    (!names.is_empty()).then_some(names)
}

fn parse_container_shorthand(value: &str) -> Option<(Vec<String>, crate::ContainerType)> {
    let value = value.trim();
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        return Some((Vec::new(), crate::ContainerType::Normal));
    }
    let parts = split_top_level(value, '/');
    match parts.as_slice() {
        [names] => Some((parse_container_names(names)?, crate::ContainerType::Normal)),
        [names, kind] => Some((parse_container_names(names)?, parse_container_type(kind)?)),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct ParsedOverflowAxis {
    specified: u8,
    inherit: bool,
}

fn parse_overflow_axis(value: &str) -> Option<ParsedOverflowAxis> {
    let lower = value.trim().to_ascii_lowercase();
    let (specified, inherit) = match lower.as_str() {
        "visible" => (0, false),
        "clip" => (1, false),
        "hidden" | "scroll" | "auto" | "overlay" => (2, false),
        "inherit" => (0, true),
        "initial" | "unset" | "revert" | "revert-layer" => (0, false),
        _ => return None,
    };
    Some(ParsedOverflowAxis { specified, inherit })
}

fn parse_overflow_declaration(
    name: &str,
    value: &str,
) -> Option<(Option<ParsedOverflowAxis>, Option<ParsedOverflowAxis>)> {
    match name {
        "overflow-x" => Some((Some(parse_overflow_axis(value)?), None)),
        "overflow-y" => Some((None, Some(parse_overflow_axis(value)?))),
        "overflow" => {
            let values = split_ws_paren(value);
            if values.is_empty() || values.len() > 2 {
                return None;
            }
            // CSS-wide keywords apply to the whole shorthand and cannot be
            // mixed with another component.
            let first = parse_overflow_axis(values[0])?;
            if first.inherit
                || matches!(
                    values[0].to_ascii_lowercase().as_str(),
                    "initial" | "unset" | "revert" | "revert-layer"
                )
            {
                return (values.len() == 1).then_some((Some(first), Some(first)));
            }
            let second = if let Some(value) = values.get(1) {
                let parsed = parse_overflow_axis(value)?;
                if parsed.inherit
                    || matches!(
                        value.to_ascii_lowercase().as_str(),
                        "initial" | "unset" | "revert" | "revert-layer"
                    )
                {
                    return None;
                }
                parsed
            } else {
                first
            };
            Some((Some(first), Some(second)))
        }
        _ => None,
    }
}

pub(crate) fn recompute_overflow(style: &mut LayoutStyle) {
    // CSS Overflow computed-value coupling: if exactly one axis is scrollable,
    // `visible` on the other computes to `auto` and `clip` computes to
    // `hidden`. A clip/visible pair remains genuinely axis-specific.
    let mut computed_x = style.overflow_specified_x;
    let mut computed_y = style.overflow_specified_y;
    if (computed_x == 2) != (computed_y == 2) {
        if computed_x == 2 {
            computed_y = 2;
        } else {
            computed_x = 2;
        }
    }
    style.overflow_clip_x = computed_x != 0;
    style.overflow_clip_y = computed_y != 0;
    style.overflow_scroll_x = computed_x == 2;
    style.overflow_scroll_y = computed_y == 2;
    style.overflow_hidden = style.overflow_clip_x || style.overflow_clip_y;
    style.overflow_scroll_container = style.overflow_scroll_x || style.overflow_scroll_y;
}

fn parse_font_variation_settings(value: &str) -> Option<Vec<crate::FontVariationSetting>> {
    let mut input = cssparser::ParserInput::new(value.trim());
    let mut parser = cssparser::Parser::new(&mut input);
    if parser.is_exhausted() {
        return None;
    }

    // A map both implements CSS's "last duplicate wins" rule and gives the
    // inline engine a stable tag order independent of author ordering.
    let mut settings = BTreeMap::new();
    loop {
        let tag = parser.expect_string_cloned().ok()?;
        let bytes = tag.as_bytes();
        if bytes.len() != 4 || !bytes.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
            return None;
        }
        let value = parse_font_variation_number(&mut parser)?;
        if !value.is_finite() {
            return None;
        }
        let tag = [bytes[0], bytes[1], bytes[2], bytes[3]];
        // Keep numerically equivalent signed zeroes canonical for stable cache
        // keys later in the shaping and rasterization pipeline.
        settings.insert(tag, if value == 0.0 { 0.0 } else { value });
        if parser.is_exhausted() {
            break;
        }
        parser.expect_comma().ok()?;
        if parser.is_exhausted() {
            return None;
        }
    }
    Some(
        settings
            .into_iter()
            .map(|(tag, value)| crate::FontVariationSetting { tag, value })
            .collect(),
    )
}

fn parse_font_variation_number(parser: &mut cssparser::Parser<'_, '_>) -> Option<f32> {
    if let Ok(value) = parser.try_parse(|input| input.expect_number()) {
        return value.is_finite().then_some(value);
    }
    let start = parser.position();
    let token = parser.next().ok()?.clone();
    let cssparser::Token::Function(name) = token else {
        return None;
    };
    if !matches!(
        name.to_ascii_lowercase().as_str(),
        "calc" | "min" | "max" | "clamp" | "round"
    ) {
        return None;
    }
    parser
        .parse_nested_block(|input| unitless_math_tokens(input, 0))
        .ok()?;
    let expression = parser.slice_from(start);
    resolve_contextual_length(expression, 0.0, 0.0, 0.0, 0.0, 0.0).filter(|value| value.is_finite())
}

fn unitless_math_tokens<'i, 't>(
    parser: &mut cssparser::Parser<'i, 't>,
    depth: u8,
) -> Result<(), cssparser::ParseError<'i, ()>> {
    if depth >= 64 {
        return Err(parser.new_custom_error(()));
    }
    while !parser.is_exhausted() {
        let token = parser.next_including_whitespace_and_comments()?.clone();
        match token {
            cssparser::Token::Number { .. }
            | cssparser::Token::WhiteSpace(_)
            | cssparser::Token::Comment(_)
            | cssparser::Token::Comma
            | cssparser::Token::Delim('+')
            | cssparser::Token::Delim('-')
            | cssparser::Token::Delim('*')
            | cssparser::Token::Delim('/') => {}
            cssparser::Token::Function(name)
                if matches!(
                    name.to_ascii_lowercase().as_str(),
                    "calc" | "min" | "max" | "clamp" | "round"
                ) =>
            {
                parser.parse_nested_block(|input| unitless_math_tokens(input, depth + 1))?;
            }
            cssparser::Token::ParenthesisBlock => {
                parser.parse_nested_block(|input| unitless_math_tokens(input, depth + 1))?;
            }
            _ => return Err(parser.new_custom_error(())),
        }
    }
    Ok(())
}

fn apply_value(style: &mut LayoutStyle, name: &str, value: &str) {
    match name {
        "direction" => {
            match value.trim().to_ascii_lowercase().as_str() {
                "ltr" | "initial" | "revert" | "revert-layer" => {
                    style.direction = Some(taffy::Direction::Ltr);
                }
                "rtl" => style.direction = Some(taffy::Direction::Rtl),
                "inherit" | "unset" => style.direction = None,
                _ => return,
            }
            resolve_logical_borders(style);
        }
        "display" => {
            let value = value.trim().to_ascii_lowercase();
            if matches!(
                value.as_str(),
                "none"
                    | "flex"
                    | "inline-flex"
                    | "inline"
                    | "inline-block"
                    | "grid"
                    | "inline-grid"
                    | "block"
                    | "flow-root"
                    | "table"
                    | "inline-table"
                    | "table-cell"
                    | "-webkit-box"
                    | "-webkit-inline-box"
                    | "contents"
                    | "inherit"
                    | "initial"
                    | "unset"
            ) {
                // Every valid authored display value replaces the complete
                // outer/inner display pair. In particular, it must not retain
                // the UA table/control approximation merely because its
                // internal enum happens to equal the newly specified value.
                style.internal_flex_container = false;
                style.is_table_box = false;
                style.is_table_cell_box = false;
                style.is_inline_block = false;
                style.flow_root = false;
                style.display_contents = false;
                style.display_inherit = false;
                style.webkit_box_display = None;
            }
            match value.as_str() {
                "none" => style.display = crate::Display::None,
                "flex" => style.display = crate::Display::Flex,
                "inline-flex" => {
                    style.display = crate::Display::Flex;
                    style.is_inline_block = true;
                }
                "inline" => style.display = crate::Display::Inline,
                "inline-block" => {
                    style.display = crate::Display::Inline;
                    style.is_inline_block = true;
                }
                "grid" => style.display = crate::Display::Grid,
                "inline-grid" => {
                    style.display = crate::Display::Grid;
                    style.is_inline_block = true;
                }
                "block" => style.display = crate::Display::Block,
                "flow-root" => {
                    style.display = crate::Display::Block;
                    style.flow_root = true;
                }
                // Taffy has no table formatting mode. Preserve the important
                // outer-display and BFC semantics instead of ignoring these
                // values and retaining a stale earlier `display` winner.
                // Native HTML tables still take the dedicated table builder.
                "table" => {
                    style.display = crate::Display::Block;
                    style.flow_root = true;
                    style.is_table_box = true;
                }
                "inline-table" => {
                    style.display = crate::Display::Inline;
                    style.is_inline_block = true;
                    style.flow_root = true;
                    style.is_table_box = true;
                }
                "table-cell" => {
                    // A table cell establishes an internal flow container. A
                    // column flexbox is our cell-content wrapper stand-in; it
                    // is marked internal so descendants do not become CSS flex
                    // items and the table grid owns the cell's inline size.
                    style.display = crate::Display::Flex;
                    style.internal_flex_container = true;
                    style.flex_direction = Some(taffy::FlexDirection::Column);
                    style.align_items = Some(taffy::AlignItems::FLEX_START);
                    style.is_table_cell_box = true;
                }
                "-webkit-box" => {
                    // The unclamped legacy display is an old flexbox. When it
                    // forms a vertical line-clamp root, the post-declaration
                    // adjustment below converts it to a flow-root just as
                    // Blink and Gecko do at computed-value time.
                    style.display = crate::Display::Flex;
                    style.webkit_box_display = Some(false);
                }
                "-webkit-inline-box" => {
                    style.display = crate::Display::Flex;
                    style.is_inline_block = true;
                    style.webkit_box_display = Some(true);
                }
                "contents" => {
                    // `display:contents` can override an earlier `display:none`
                    // in the cascade (responsive desktop/mobile wrappers do
                    // this constantly). It suppresses only this element's box;
                    // its children remain generated and are flattened into the
                    // parent formatting context.
                    style.display = crate::Display::Block;
                    style.display_contents = true;
                }
                "inherit" => {
                    // Keep a valid initial-display placeholder until the
                    // parent computed style is available. The top-down pass
                    // copies the parent's outer and inner display provenance,
                    // but never its engine-only native-layout flags.
                    style.display = crate::Display::Inline;
                    style.display_inherit = true;
                }
                "initial" | "unset" => {
                    // `display` is not inherited and its CSS initial value is
                    // `inline`, independent of the element's HTML UA display.
                    style.display = crate::Display::Inline;
                }
                _ => {}
            }
        }
        "container-type" => {
            if value.eq_ignore_ascii_case("inherit") {
                style.container_type = crate::ContainerType::Normal;
                style.container_type_inherit = true;
            } else if let Some(kind) = parse_container_type(value) {
                style.container_type = kind;
                style.container_type_inherit = false;
            }
        }
        "container-name" => {
            if value.eq_ignore_ascii_case("inherit") {
                style.container_names.clear();
                style.container_names_inherit = true;
            } else if let Some(names) = parse_container_names(value) {
                style.container_names = names;
                style.container_names_inherit = false;
            }
        }
        "container" => {
            if value.eq_ignore_ascii_case("inherit") {
                style.container_names.clear();
                style.container_type = crate::ContainerType::Normal;
                style.container_names_inherit = true;
                style.container_type_inherit = true;
            } else if let Some((names, kind)) = parse_container_shorthand(value) {
                style.container_names = names;
                style.container_type = kind;
                style.container_names_inherit = false;
                style.container_type_inherit = false;
            }
        }
        // The renderer currently supports the initial horizontal-tb writing
        // mode. In that mode logical inline/block sizing is exactly the
        // physical width/height pair, including declaration-order overrides
        // between logical and physical longhands.
        "width" | "inline-size" => {
            style.width = dimension_value(value);
            style.width_fit_content = value.trim().eq_ignore_ascii_case("fit-content");
            style.size_expressions[0] = deferred_length_expression(value);
            style.width_set = true;
        }
        "height" | "block-size" => {
            style.height = dimension_value(value);
            style.size_expressions[1] = deferred_length_expression(value);
            style.height_set = true;
        }
        "box-sizing" => {
            let value = value.trim();
            style.box_sizing = if value.eq_ignore_ascii_case("border-box") {
                crate::BoxSizing::BorderBox
            } else if value.eq_ignore_ascii_case("content-box") {
                crate::BoxSizing::ContentBox
            } else if value.eq_ignore_ascii_case("initial")
                || value.eq_ignore_ascii_case("unset")
                || value.eq_ignore_ascii_case("revert")
                || value.eq_ignore_ascii_case("revert-layer")
            {
                // `box-sizing` is not inherited, so both its initial value and
                // `unset` compute to content-box. The compact cascade cannot
                // recover a declaration from a lower origin/layer after it has
                // applied an author winner; consistently with the other
                // non-inherited properties here, approximate both revert
                // forms with the initial value rather than retaining an
                // earlier declaration from the same author origin.
                crate::BoxSizing::ContentBox
            } else if value.eq_ignore_ascii_case("inherit") {
                // Preserve the specified inherit marker until the DOM's
                // top-down computed-style pass can resolve it from the parent.
                crate::BoxSizing::Inherit
            } else {
                style.box_sizing
            };
        }
        "min-width" | "min-inline-size" => {
            style.min_width = dimension_value(value);
            style.size_expressions[2] = deferred_length_expression(value);
        }
        "min-height" | "min-block-size" => {
            style.min_height = dimension_value(value);
            style.size_expressions[3] = deferred_length_expression(value);
        }
        "max-width" | "max-inline-size" => {
            style.max_width = dimension_value(value);
            style.size_expressions[4] = deferred_length_expression(value);
        }
        "max-height" | "max-block-size" => {
            style.max_height = dimension_value(value);
            style.size_expressions[5] = deferred_length_expression(value);
        }
        "aspect-ratio" => {
            style.aspect_ratio = parse_aspect_ratio(value);
            style.aspect_ratio_is_mapped = false;
            style.aspect_ratio_is_intrinsic = false;
        }
        "margin" => apply_margin_shorthand(style, value),
        "margin-top" => set_margin_side(style, 0, value),
        "margin-right" => set_margin_side(style, 1, value),
        "margin-bottom" => set_margin_side(style, 2, value),
        "margin-left" => set_margin_side(style, 3, value),
        // Logical margins (LTR: inline = left/right, block = top/bottom).
        "margin-inline" => {
            let (s, e) = two(value);
            set_margin_side(style, 3, s);
            set_margin_side(style, 1, e);
        }
        "margin-inline-start" => set_margin_side(style, 3, value),
        "margin-inline-end" => set_margin_side(style, 1, value),
        "margin-block" => {
            let (s, e) = two(value);
            set_margin_side(style, 0, s);
            set_margin_side(style, 2, e);
        }
        "margin-block-start" => set_margin_side(style, 0, value),
        "margin-block-end" => set_margin_side(style, 2, value),
        "padding" => apply_padding_shorthand(style, value),
        "padding-top" => set_padding_side(style, 0, value),
        "padding-right" => set_padding_side(style, 1, value),
        "padding-bottom" => set_padding_side(style, 2, value),
        "padding-left" => set_padding_side(style, 3, value),
        "padding-inline" => {
            let (s, e) = two(value);
            set_padding_side(style, 3, s);
            set_padding_side(style, 1, e);
        }
        "padding-inline-start" => set_padding_side(style, 3, value),
        "padding-inline-end" => set_padding_side(style, 1, value),
        "padding-block" => {
            let (s, e) = two(value);
            set_padding_side(style, 0, s);
            set_padding_side(style, 2, e);
        }
        "padding-block-start" => set_padding_side(style, 0, value),
        "padding-block-end" => set_padding_side(style, 2, value),
        "border-radius" => apply_border_radius_shorthand(style, value),
        "border-top-left-radius" => set_corner_radius(style, 0, value),
        "border-top-right-radius" => set_corner_radius(style, 1, value),
        "border-bottom-right-radius" => set_corner_radius(style, 2, value),
        "border-bottom-left-radius" => set_corner_radius(style, 3, value),
        "clip-path" | "-webkit-clip-path" => {
            let lower = value.trim().to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "none" | "initial" | "unset" | "revert" | "revert-layer"
            ) {
                style.clip_path = None;
            } else if let Some(polygon) = parse_clip_path_polygon(value) {
                style.clip_path = Some(polygon);
            }
        }
        "border" => apply_border_shorthand(style, None, value),
        "border-top" => apply_border_shorthand(style, Some(Side::Top), value),
        "border-right" => apply_border_shorthand(style, Some(Side::Right), value),
        "border-bottom" => apply_border_shorthand(style, Some(Side::Bottom), value),
        "border-left" => apply_border_shorthand(style, Some(Side::Left), value),
        "border-width" => apply_border_widths(style, value),
        "border-top-width" => set_border_width(style, Side::Top, value),
        "border-right-width" => set_border_width(style, Side::Right, value),
        "border-bottom-width" => set_border_width(style, Side::Bottom, value),
        "border-left-width" => set_border_width(style, Side::Left, value),
        "border-style" => apply_border_styles(style, value),
        "border-top-style" => set_border_style(style, Side::Top, value),
        "border-right-style" => set_border_style(style, Side::Right, value),
        "border-bottom-style" => set_border_style(style, Side::Bottom, value),
        "border-left-style" => set_border_style(style, Side::Left, value),
        "border-color" => apply_border_colors(style, value),
        "border-top-color" => set_border_color(style, Side::Top, value),
        "border-right-color" => set_border_color(style, Side::Right, value),
        "border-bottom-color" => set_border_color(style, Side::Bottom, value),
        "border-left-color" => set_border_color(style, Side::Left, value),
        "border-inline"
        | "border-block"
        | "border-inline-start"
        | "border-inline-end"
        | "border-block-start"
        | "border-block-end"
        | "border-inline-width"
        | "border-inline-style"
        | "border-inline-color"
        | "border-block-width"
        | "border-block-style"
        | "border-block-color"
        | "border-inline-start-width"
        | "border-inline-start-style"
        | "border-inline-start-color"
        | "border-inline-end-width"
        | "border-inline-end-style"
        | "border-inline-end-color"
        | "border-block-start-width"
        | "border-block-start-style"
        | "border-block-start-color"
        | "border-block-end-width"
        | "border-block-end-style"
        | "border-block-end-color" => apply_logical_border(style, name, value),
        "outline" => apply_outline_shorthand(style, value),
        "outline-width" => set_outline_width(style, value),
        "outline-style" => set_outline_style(style, value),
        "outline-color" => set_outline_color(style, value),
        "outline-offset" => set_outline_offset(style, value),
        "background-color" => {
            style.background_color = parse_color_for_scheme(value, style.color_scheme_dark)
        }
        "background" => {
            // A shorthand resets every omitted background longhand to its
            // initial value before applying the layers it does name.
            // `background:0` is a valid position-only shorthand commonly used
            // to clear a component background; merely assigning the fields we
            // can parse leaves an earlier color/image painting underneath it.
            // An empty value is invalid (for example an unresolved `var()`),
            // so leave the prior cascade winner untouched in that case.
            if !value.trim().is_empty() {
                style.background_color = None;
                set_background_gradients(style, value);
                if style.background_gradient.is_none()
                    && style.background_radial_gradient.is_none()
                    && style.background_conic_gradient.is_none()
                {
                    style.background_color = parse_color_for_scheme(value, style.color_scheme_dark);
                }
                style.background_image = parse_url(value);
                style.background_size = None;
                style.background_size_expression = background_size_expression(value);
                style.background_size_fit = parse_background_size_fit(value);
                style.background_position = crate::BackgroundPosition::default();
                style.background_repeat = parse_image_repeat(value);
                style.background_origin = crate::BackgroundOrigin::default();
                style.background_clip = crate::BackgroundClip::default();
                if let Some((origin, clip)) = parse_background_box_shorthand(value) {
                    style.background_origin = origin;
                    style.background_clip = clip;
                }
                style.background_clip_text = style.background_clip == crate::BackgroundClip::Text;
            }
        }
        "background-image" => {
            set_background_gradients(style, value);
            style.background_image = parse_url(value);
        }
        "background-size" => {
            style.background_size = parse_background_size(value);
            style.background_size_expression =
                (!value.trim().is_empty()).then(|| value.trim().to_string());
            style.background_size_fit = parse_background_size_fit(value);
        }
        "background-position" => style.background_position = parse_background_position(value),
        "background-repeat" => {
            style.background_repeat = parse_image_repeat(value);
        }
        "background-origin" => {
            style.background_origin = parse_background_origin(value).unwrap_or_default();
        }
        // On replaced elements, an image-valued `content` replaces the
        // ordinary source and participates in intrinsic sizing. String-valued
        // generated content is handled separately for pseudo-elements.
        "content" => style.content_image = parse_url(value),
        "mask-image" | "-webkit-mask-image" => style.mask_image = parse_url(value),
        "mask-size" | "-webkit-mask-size" => style.mask_size = parse_background_size(value),
        "mask-repeat" | "-webkit-mask-repeat" => {
            style.mask_repeat = parse_image_repeat(value);
        }
        "background-clip" | "-webkit-background-clip" => {
            style.background_clip = parse_background_clip(value).unwrap_or_default();
            style.background_clip_text = style.background_clip == crate::BackgroundClip::Text;
        }
        // Blink/WebKit gradient text commonly makes the glyph fill
        // transparent through this inherited property while clipping a
        // background gradient to the text. We model one effective text color,
        // so the vendor fill color is the paint-time color winner.
        "color" | "-webkit-text-fill-color" => {
            style.color = parse_color_for_scheme(value, style.color_scheme_dark)
        }
        // Inline SVG is rasterized as a standalone SVG document. Preserve
        // author-CSS presentation values verbatim so paint can carry the
        // computed cascade into that document (including currentColor,
        // `none`, and paint-server URLs).
        "fill" => {
            style.svg_fill = Some(resolve_svg_presentation_color(
                value,
                style.color_scheme_dark,
            ))
        }
        "stroke" => {
            style.svg_stroke = Some(resolve_svg_presentation_color(
                value,
                style.color_scheme_dark,
            ))
        }
        "stroke-width" => style.svg_stroke_width = Some(value.trim().to_string()),
        "font-size" => {
            // Absolute lengths resolve now; font/viewport-relative ones defer
            // to the inheritance pass (they need parent/root font-size).
            apply_font_size(style, value);
        }
        "letter-spacing" => apply_letter_spacing(style, value),
        "font" => apply_font_shorthand(style, value),
        "font-weight" => {
            let lower = value.trim().to_ascii_lowercase();
            match lower.as_str() {
                // font-weight is inherited. Preserve the inherited marker
                // until the DOM's top-down computed-style pass can see the
                // parent's numeric weight. `unset` has the same behavior for
                // an inherited property.
                "inherit" | "unset" => {
                    style.font_weight = Some("inherit".to_string());
                }
                // The initial font weight is normal, independent of both the
                // parent and a heading's bold UA rule.
                "initial" => {
                    style.font_weight = Some("400".to_string());
                }
                // The compact cascade does not retain origin/layer history,
                // so leave its current state unchanged instead of pretending
                // the revert forms mean inherit or initial.
                "revert" | "revert-layer" => {}
                _ => {
                    if let Some(weight) = specified_font_weight(&lower) {
                        style.font_weight = Some(weight);
                    }
                }
            }
        }
        "font-family" => {
            let v = value.trim().to_ascii_lowercase();
            if !v.is_empty() && v != "inherit" {
                style.font_family = Some(v);
            }
        }
        "font-optical-sizing" => {
            let parsed = match value.trim().to_ascii_lowercase().as_str() {
                "inherit" | "unset" | "revert" | "revert-layer" => Some(None),
                "auto" | "initial" => Some(Some(crate::FontOpticalSizing::Auto)),
                "none" => Some(Some(crate::FontOpticalSizing::None)),
                _ => None,
            };
            if let Some(value) = parsed {
                style.font_optical_sizing = value;
            }
        }
        "font-variation-settings" => {
            let lower = value.trim().to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "inherit" | "unset" | "revert" | "revert-layer"
            ) {
                style.font_variation_settings = None;
            } else if matches!(lower.as_str(), "normal" | "initial") {
                style.font_variation_settings = Some(Vec::new());
            } else if let Some(settings) = parse_font_variation_settings(value) {
                style.font_variation_settings = Some(settings);
            }
        }
        // Text alignment is inherited and applies to inline line boxes. It is
        // deliberately separate from flex/grid `align-items`, which positions
        // child boxes rather than text inside them.
        "text-align" => match value {
            "right" | "end" => {
                style.text_align = Some(taffy::AlignItems::FLEX_END);
                style.legacy_center = false;
            }
            "center" => {
                style.text_align = Some(taffy::AlignItems::CENTER);
                style.legacy_center = false;
            }
            "left" | "start" | "justify" => {
                style.text_align = Some(taffy::AlignItems::FLEX_START);
                style.legacy_center = false;
            }
            _ => {}
        },
        "text-indent" => apply_text_indent(style, value),
        "align-items" => {
            if let Some(Some(value)) = self_alignment_value(value) {
                style.align_items = Some(value);
            }
        }
        "justify-items" => {
            if let Some(Some(value)) = self_alignment_value(value) {
                style.justify_items = Some(value);
            }
        }
        "place-items" => {
            if let Some((Some(align), Some(justify))) = self_alignment_pair(value) {
                style.align_items = Some(align);
                style.justify_items = Some(justify);
            }
        }
        "align-self" => {
            if let Some(value) = self_alignment_value(value) {
                style.align_self = value;
            }
        }
        "justify-self" => {
            if let Some(value) = self_alignment_value(value) {
                style.justify_self = value;
            }
        }
        "place-self" => {
            if let Some((align, justify)) = self_alignment_pair(value) {
                style.align_self = align;
                style.justify_self = justify;
            }
        }
        "align-content" => {
            if let Some(value) = content_alignment_value(value) {
                style.align_content = Some(value);
            }
        }
        "justify-content" => {
            let value = match value.trim().to_ascii_lowercase().as_str() {
                "left" => Some(taffy::JustifyContent::START),
                "right" => Some(taffy::JustifyContent::END),
                _ => content_alignment_value(value),
            };
            if let Some(value) = value {
                style.justify_content = Some(value);
            }
        }
        "place-content" => {
            if let Some((align, justify)) = content_alignment_pair(value) {
                style.align_content = Some(align);
                style.justify_content = Some(justify);
            }
        }
        "flex-flow" => {
            if let Some((direction, wrap)) = parse_flex_flow_shorthand(value) {
                // A shorthand always assigns both longhands. In particular,
                // `flex-flow: column` resets an earlier flex-wrap to nowrap,
                // while `flex-flow: wrap` resets direction to row.
                style.flex_direction = Some(direction);
                style.flex_wrap = Some(wrap);
            }
        }
        "flex-direction" => match value {
            "row" => style.flex_direction = Some(taffy::FlexDirection::Row),
            "row-reverse" => style.flex_direction = Some(taffy::FlexDirection::RowReverse),
            "column" => style.flex_direction = Some(taffy::FlexDirection::Column),
            "column-reverse" => style.flex_direction = Some(taffy::FlexDirection::ColumnReverse),
            _ => {}
        },
        "flex-wrap" => match value {
            "wrap" => style.flex_wrap = Some(taffy::FlexWrap::Wrap),
            "nowrap" => style.flex_wrap = Some(taffy::FlexWrap::NoWrap),
            "wrap-reverse" => style.flex_wrap = Some(taffy::FlexWrap::WrapReverse),
            _ => {}
        },
        "flex-grow" => {
            if let Some(v) = token(value).and_then(|t| t.parse::<f32>().ok()) {
                style.flex_grow = Some(v);
            }
        }
        "flex-shrink" => {
            if let Some(v) = token(value).and_then(|t| t.parse::<f32>().ok()) {
                style.flex_shrink = Some(v);
            }
        }
        "order" => {
            if let Ok(order) = value.trim().parse::<i32>() {
                style.order = order;
            }
        }
        "flex-basis" => {
            style.flex_basis = dimension_value(value.trim());
        }
        "flex" => parse_flex_shorthand(style, value),
        "position" => match value {
            "absolute" => {
                style.position = Some(taffy::Position::Absolute);
                style.position_fixed = false;
                style.position_sticky = false;
            }
            "fixed" => {
                style.position = Some(taffy::Position::Absolute);
                style.position_fixed = true;
                style.position_sticky = false;
            }
            "relative" => {
                style.position = Some(taffy::Position::Relative);
                style.position_fixed = false;
                style.position_sticky = false;
            }
            "sticky" => {
                style.position = Some(taffy::Position::Relative);
                style.position_fixed = false;
                style.position_sticky = true;
            }
            "static" => {
                style.position = None;
                style.position_fixed = false;
                style.position_sticky = false;
            }
            _ => {}
        },
        "float" => match value {
            "left" => style.float = Some(crate::Float::Left),
            "right" => style.float = Some(crate::Float::Right),
            "none" => style.float = None,
            _ => {}
        },
        "counter-reset" => {
            if let Some(counters) = parse_counter_directives(value, 0) {
                style.counter_reset = counters;
            }
        }
        "counter-increment" => {
            if let Some(counters) = parse_counter_directives(value, 1) {
                style.counter_increment = counters;
            }
        }
        "counter-set" => {
            if let Some(counters) = parse_counter_directives(value, 0) {
                style.counter_set = counters;
            }
        }
        "object-fit" => match value.trim().to_ascii_lowercase().as_str() {
            "fill" => style.object_fit = crate::ObjectFit::Fill,
            "contain" => style.object_fit = crate::ObjectFit::Contain,
            "cover" => style.object_fit = crate::ObjectFit::Cover,
            "scale-down" => style.object_fit = crate::ObjectFit::ScaleDown,
            "none" => style.object_fit = crate::ObjectFit::None,
            _ => {}
        },
        "object-position" => {
            let position = parse_background_position(value);
            style.object_position = crate::ObjectPosition::new(position.x, position.y);
        }
        "top" => set_inset_side(style, 0, value),
        "right" => set_inset_side(style, 1, value),
        "bottom" => set_inset_side(style, 2, value),
        "left" => set_inset_side(style, 3, value),
        // Logical insets in the engine's current horizontal LTR writing mode.
        // Utility frameworks prefer these over left/right (for example,
        // Tailwind's `inset-x-0` emits `inset-inline:0`), so dropping them
        // turns stretched fixed overlays into shrink-to-fit boxes.
        "inset-inline" => {
            let (start, end) = two(value);
            set_inset_side(style, 3, start);
            set_inset_side(style, 1, end);
        }
        "inset-inline-start" => set_inset_side(style, 3, value),
        "inset-inline-end" => set_inset_side(style, 1, value),
        "inset-block" => {
            let (start, end) = two(value);
            set_inset_side(style, 0, start);
            set_inset_side(style, 2, end);
        }
        "inset-block-start" => set_inset_side(style, 0, value),
        "inset-block-end" => set_inset_side(style, 2, value),
        "inset" => {
            // 1-4 values, CSS shorthand order: all / v h / t h b / t r b l.
            let parts = split_ws_paren(value);
            let (t, r, b, l) = match parts.as_slice() {
                [a] => (*a, *a, *a, *a),
                [v, h] => (*v, *h, *v, *h),
                [t, h, b] => (*t, *h, *b, *h),
                [t, r, b, l, ..] => (*t, *r, *b, *l),
                [] => return,
            };
            set_inset_side(style, 0, t);
            set_inset_side(style, 1, r);
            set_inset_side(style, 2, b);
            set_inset_side(style, 3, l);
        }
        "overflow" | "overflow-x" | "overflow-y" => {
            let Some((x, y)) = parse_overflow_declaration(name, value) else {
                return;
            };
            if let Some(x) = x {
                style.overflow_specified_x = x.specified;
                style.overflow_inherit_x = x.inherit;
            }
            if let Some(y) = y {
                style.overflow_specified_y = y.specified;
                style.overflow_inherit_y = y.inherit;
            }
            style.overflow_axes_set = true;
            recompute_overflow(style);
        }
        "scrollbar-gutter" => {
            let lower = value.to_ascii_lowercase();
            style.scrollbar_gutters = if lower
                .split_ascii_whitespace()
                .any(|token| token == "stable")
            {
                if lower
                    .split_ascii_whitespace()
                    .any(|token| token == "both-edges")
                {
                    2
                } else {
                    1
                }
            } else {
                0
            };
        }
        "visibility" => style.visibility_hidden = Some(value.eq_ignore_ascii_case("hidden")),
        "opacity" => style.opacity = value.trim().parse::<f32>().ok(),
        "animation" => apply_animation_shorthand(style, value),
        "animation-name" => {
            let first = split_top_level(value, ',')
                .into_iter()
                .next()
                .unwrap_or("")
                .trim();
            style.animation_name = if first.is_empty()
                || matches!(
                    first.to_ascii_lowercase().as_str(),
                    "none" | "initial" | "inherit" | "unset" | "revert" | "revert-layer"
                ) {
                None
            } else {
                Some(first.to_string())
            };
        }
        "animation-duration" => {
            let first = split_top_level(value, ',')
                .into_iter()
                .next()
                .unwrap_or("")
                .trim();
            if matches!(
                first.to_ascii_lowercase().as_str(),
                "initial" | "inherit" | "unset" | "revert" | "revert-layer"
            ) {
                style.animation_timing.duration_ms = 0.0;
            } else if let Some(milliseconds) =
                parse_animation_time_ms(first).filter(|time| *time >= 0.0)
            {
                style.animation_timing.duration_ms = milliseconds;
            }
        }
        "animation-delay" => {
            let first = split_top_level(value, ',')
                .into_iter()
                .next()
                .unwrap_or("")
                .trim();
            if matches!(
                first.to_ascii_lowercase().as_str(),
                "initial" | "inherit" | "unset" | "revert" | "revert-layer"
            ) {
                style.animation_timing.delay_ms = 0.0;
            } else if let Some(milliseconds) = parse_animation_time_ms(first) {
                style.animation_timing.delay_ms = milliseconds;
            }
        }
        "animation-fill-mode" => {
            let first = split_top_level(value, ',')
                .into_iter()
                .next()
                .unwrap_or("")
                .trim();
            if matches!(
                first.to_ascii_lowercase().as_str(),
                "initial" | "inherit" | "unset" | "revert" | "revert-layer"
            ) {
                style.animation_timing.fill_mode = crate::AnimationFillMode::None;
            } else if let Some(fill) = parse_animation_fill_mode(first) {
                style.animation_timing.fill_mode = fill;
            }
        }
        "animation-iteration-count" => {
            let first = split_top_level(value, ',')
                .into_iter()
                .next()
                .unwrap_or("")
                .trim();
            if matches!(
                first.to_ascii_lowercase().as_str(),
                "initial" | "inherit" | "unset" | "revert" | "revert-layer"
            ) {
                style.animation_timing.iteration_count = 1.0;
            } else if let Some(iterations) = parse_animation_iteration_count(first) {
                style.animation_timing.iteration_count = iterations;
            }
        }
        "animation-direction" => {
            let first = split_top_level(value, ',')
                .into_iter()
                .next()
                .unwrap_or("")
                .trim();
            if matches!(
                first.to_ascii_lowercase().as_str(),
                "initial" | "inherit" | "unset" | "revert" | "revert-layer"
            ) {
                style.animation_timing.direction = crate::AnimationDirection::Normal;
            } else if let Some(direction) = parse_animation_direction(first) {
                style.animation_timing.direction = direction;
            }
        }
        "animation-play-state" => {
            let first = split_top_level(value, ',')
                .into_iter()
                .next()
                .unwrap_or("")
                .trim();
            if matches!(
                first.to_ascii_lowercase().as_str(),
                "initial" | "inherit" | "unset" | "revert" | "revert-layer"
            ) {
                style.animation_timing.play_state = crate::AnimationPlayState::Running;
            } else if let Some(play_state) = parse_animation_play_state(first) {
                style.animation_timing.play_state = play_state;
            }
        }
        "z-index" => {
            style.z_index = match value.trim() {
                "auto" | "inherit" | "initial" => None,
                v => v.parse::<i32>().ok(),
            };
        }
        "clear" => {
            style.clear = match value.trim().to_ascii_lowercase().as_str() {
                "left" | "inline-start" => Some(crate::Clear::Left),
                "right" | "inline-end" => Some(crate::Clear::Right),
                "both" => Some(crate::Clear::Both),
                _ => None,
            };
        }
        "vertical-align" => {
            style.vertical_align = match value.trim().to_ascii_lowercase().as_str() {
                "top" | "baseline" | "text-top" => Some(crate::VerticalAlign::Top),
                "middle" => Some(crate::VerticalAlign::Middle),
                "bottom" | "text-bottom" => Some(crate::VerticalAlign::Bottom),
                // sub/super/lengths are text-level; leave the cell default.
                _ => style.vertical_align,
            };
        }
        "list-style-type" => {
            style.list_style =
                Some(list_style_keyword(value.trim()).unwrap_or(crate::ListStyle::Disc));
        }
        "list-style" => {
            // Shorthand: type | position | image in any order. We only track
            // the type keyword (and `none`, which suppresses the marker, the
            // common way nav `<ul>`s drop their bullets).
            for tok in value.split_whitespace() {
                if let Some(ls) = list_style_keyword(tok) {
                    style.list_style = Some(ls);
                }
            }
        }
        "line-height" => {
            let v = value.trim();
            if v.contains('(') {
                style.line_height = None;
                style.line_height_expression = Some(v.to_string());
                return;
            }
            style.line_height_expression = None;
            style.line_height = if v.eq_ignore_ascii_case("normal") {
                Some(crate::LineHeight::Normal)
            } else if let Some(pct) = v.strip_suffix('%') {
                pct.trim().parse::<f32>().ok().map(|number| {
                    crate::LineHeight::Relative(crate::Dimension::Percent(number / 100.0))
                })
            } else if v.ends_with("px") || v.ends_with("pt") {
                px_value(v).map(crate::LineHeight::Px)
            } else if ["rem", "em", "ex", "vw", "vh", "vmin", "vmax"]
                .iter()
                .any(|unit| v.ends_with(unit))
            {
                Some(crate::LineHeight::Relative(dimension_value(v)))
            } else {
                // Unitless number: a multiple of font-size (the common case).
                v.parse::<f32>().ok().map(crate::LineHeight::Ratio)
            };
        }
        "white-space" => {
            style.white_space = match value.trim().to_ascii_lowercase().as_str() {
                "normal" | "initial" | "revert" | "revert-layer" => Some(crate::WhiteSpace::Normal),
                "nowrap" => Some(crate::WhiteSpace::NoWrap),
                "pre" => Some(crate::WhiteSpace::Pre),
                "pre-wrap" => Some(crate::WhiteSpace::PreWrap),
                "pre-line" => Some(crate::WhiteSpace::PreLine),
                "break-spaces" => Some(crate::WhiteSpace::BreakSpaces),
                // `white-space` inherits, so unset behaves as inherit.
                "inherit" | "unset" => None,
                _ => style.white_space,
            };
        }
        "text-overflow" => {
            style.text_overflow = match value.trim().to_ascii_lowercase().as_str() {
                "ellipsis" => crate::TextOverflow::Ellipsis,
                "clip" | "initial" | "unset" | "revert" | "revert-layer" => {
                    crate::TextOverflow::Clip
                }
                _ => style.text_overflow,
            };
        }
        "-webkit-line-clamp" => {
            if let Some(lines) = webkit_line_clamp_value(value) {
                style.webkit_line_clamp = lines;
            }
        }
        "-webkit-box-orient" => {
            style.webkit_box_orient_vertical = match value.trim().to_ascii_lowercase().as_str() {
                "vertical" | "block-axis" => true,
                "horizontal" | "inline-axis" | "initial" | "unset" | "revert" | "revert-layer" => {
                    false
                }
                _ => style.webkit_box_orient_vertical,
            };
        }
        "overflow-wrap" | "word-wrap" => {
            style.overflow_wrap = match value.trim().to_ascii_lowercase().as_str() {
                "normal" | "initial" => Some(crate::OverflowWrap::Normal),
                "break-word" => Some(crate::OverflowWrap::BreakWord),
                "anywhere" => Some(crate::OverflowWrap::Anywhere),
                // Both properties inherit. With no lower-origin declaration
                // in the compact cascade, revert also exposes the inherited
                // value, as Blink and Gecko do for these inherited longhands.
                "inherit" | "unset" | "revert" | "revert-layer" => None,
                _ => style.overflow_wrap,
            };
        }
        "word-break" => {
            style.word_break = match value.trim().to_ascii_lowercase().as_str() {
                "normal" | "initial" => Some(crate::WordBreak::Normal),
                "break-all" => Some(crate::WordBreak::BreakAll),
                "keep-all" => Some(crate::WordBreak::KeepAll),
                "break-word" => Some(crate::WordBreak::BreakWord),
                "inherit" | "unset" | "revert" | "revert-layer" => None,
                _ => style.word_break,
            };
        }
        "text-wrap" => {
            style.text_wrap_style = match value.trim().to_ascii_lowercase().as_str() {
                "auto" | "wrap" | "initial" | "revert" | "revert-layer" => {
                    Some(crate::TextWrapStyle::Auto)
                }
                "balance" | "wrap balance" | "balance wrap" => Some(crate::TextWrapStyle::Balance),
                // `text-wrap` inherits, so unset behaves as inherit.
                "inherit" | "unset" => None,
                _ => style.text_wrap_style,
            };
        }
        "text-wrap-style" => {
            style.text_wrap_style = match value.trim().to_ascii_lowercase().as_str() {
                "auto" | "initial" | "revert" | "revert-layer" => Some(crate::TextWrapStyle::Auto),
                "balance" => Some(crate::TextWrapStyle::Balance),
                // `text-wrap-style` inherits, so unset behaves as inherit.
                "inherit" | "unset" => None,
                _ => style.text_wrap_style,
            };
        }
        "font-style" => {
            let v = value.trim().to_ascii_lowercase();
            style.font_style_italic = Some(v.starts_with("italic") || v.starts_with("oblique"));
        }
        "text-transform" => {
            style.text_transform = Some(match value.trim().to_ascii_lowercase().as_str() {
                "uppercase" => crate::TextTransform::Uppercase,
                "lowercase" => crate::TextTransform::Lowercase,
                "capitalize" => crate::TextTransform::Capitalize,
                _ => crate::TextTransform::None,
            });
        }
        "text-decoration" | "text-decoration-line" => {
            // Shorthand can carry color/style/thickness; we only model the
            // underline line (the dominant case, and the UA default for links).
            let toks: Vec<String> = value
                .split_whitespace()
                .map(|t| t.to_ascii_lowercase())
                .collect();
            let underline = toks.iter().any(|t| t == "underline");
            let none = toks.iter().any(|t| t == "none");
            style.underline = Some(underline && !none);
        }
        "gap" | "grid-gap" => {
            let values = split_ws_paren(value);
            if let Some(row) = values.first() {
                apply_gap_value(style, true, row);
                apply_gap_value(style, false, values.get(1).copied().unwrap_or(row));
            }
        }
        "row-gap" | "grid-row-gap" => apply_gap_value(style, true, value),
        "column-gap" | "grid-column-gap" | "-webkit-column-gap" => {
            apply_gap_value(style, false, value)
        }
        "column-count" | "-webkit-column-count" => {
            style.column_count = parse_column_count(value);
        }
        "columns" | "-webkit-columns" => {
            // `columns` is `column-width || column-count`; width remains auto
            // in our box-fragmentation subset, but the shorthand still resets
            // an earlier count when it contains no positive integer.
            style.column_count = split_ws_paren(value)
                .into_iter()
                .find_map(parse_column_count);
        }
        "break-inside" => {
            style.break_inside_avoid = matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "avoid" | "avoid-column"
            );
        }
        "-webkit-column-break-inside" => {
            style.break_inside_avoid = value.trim().eq_ignore_ascii_case("avoid");
        }
        "border-spacing" => {
            let dims: Vec<f32> = value.split_whitespace().filter_map(px_value).collect();
            if let Some(&h) = dims.first() {
                style.border_spacing = Some((h, *dims.get(1).unwrap_or(&h)));
            }
        }
        "border-collapse" => {
            style.border_collapse = match value.trim().to_ascii_lowercase().as_str() {
                "collapse" => Some(true),
                "separate" | "initial" | "revert" | "revert-layer" => Some(false),
                // This is an inherited property, so both an omitted value and
                // an explicit inherit/unset are resolved in the top-down pass.
                "inherit" | "unset" => None,
                _ => style.border_collapse,
            };
        }
        "table-layout" => match value.trim().to_ascii_lowercase().as_str() {
            "fixed" => style.table_layout_fixed = true,
            "auto" | "initial" | "unset" | "revert" | "revert-layer" => {
                style.table_layout_fixed = false;
            }
            _ => {}
        },
        "grid-template-columns" => {
            let (tracks, names, calc_expressions) = parse_track_list_named(value);
            style.grid_template_columns_subgrid = is_subgrid_track_list(value);
            style.grid_template_columns = tracks;
            style.grid_calc_expressions[0] = calc_expressions;
            style.grid_col_line_names = (!names.is_empty()).then(|| build_line_map(names));
        }
        "grid-template-rows" => {
            let (tracks, names, calc_expressions) = parse_track_list_named(value);
            style.grid_template_rows = tracks;
            style.grid_calc_expressions[1] = calc_expressions;
            style.grid_row_line_names = (!names.is_empty()).then(|| build_line_map(names));
        }
        "grid-auto-columns" => apply_grid_auto_tracks(style, value, true),
        "grid-auto-rows" => apply_grid_auto_tracks(style, value, false),
        "grid-template-areas" => style.grid_areas = Some(parse_grid_areas(value)),
        "grid-template" => parse_grid_template(style, value),
        "grid" => parse_grid_shorthand(style, value),
        "grid-auto-flow" => style.grid_auto_flow = parse_grid_auto_flow(value),
        "grid-area" => set_grid_area(style, value),
        "grid-column" => set_grid_placement(style, value, true),
        "grid-row" => set_grid_placement(style, value, false),
        "grid-column-start" => set_grid_placement_side(style, value, true, true),
        "grid-column-end" => set_grid_placement_side(style, value, true, false),
        "grid-row-start" => set_grid_placement_side(style, value, false, true),
        "grid-row-end" => set_grid_placement_side(style, value, false, false),
        "transform" => parse_transform(style, value),
        "transform-origin" => style.transform_origin = parse_transform_origin(value),
        "translate" => parse_individual_translate(style, value),
        "rotate" => parse_individual_rotate(style, value),
        "scale" => parse_individual_scale(style, value),
        "filter" => {
            set_containing_block_trigger(style, crate::CB_TRIGGER_FILTER, non_none_value(value))
        }
        "backdrop-filter" | "-webkit-backdrop-filter" => set_containing_block_trigger(
            style,
            crate::CB_TRIGGER_BACKDROP_FILTER,
            non_none_value(value),
        ),
        "perspective" => set_containing_block_trigger(
            style,
            crate::CB_TRIGGER_PERSPECTIVE,
            non_none_value(value),
        ),
        "contain" => {
            let establishes = value.split_whitespace().any(|v| {
                matches!(
                    v.to_ascii_lowercase().as_str(),
                    "layout" | "paint" | "strict" | "content"
                )
            });
            set_containing_block_trigger(style, crate::CB_TRIGGER_CONTAIN, establishes);
        }
        "will-change" => {
            let establishes = value.split([',', ' ']).map(str::trim).any(|v| {
                matches!(
                    v.to_ascii_lowercase().as_str(),
                    "transform" | "filter" | "backdrop-filter" | "perspective" | "contain"
                )
            });
            set_containing_block_trigger(style, crate::CB_TRIGGER_WILL_CHANGE, establishes);
        }
        "content-visibility" => set_containing_block_trigger(
            style,
            crate::CB_TRIGGER_CONTENT_VISIBILITY,
            value.trim().eq_ignore_ascii_case("auto"),
        ),
        "box-shadow" | "-webkit-box-shadow" => {
            style.box_shadow = parse_box_shadow(value, style.color, style.color_scheme_dark);
        }
        _ => {}
    }
    normalize_webkit_line_clamp_display(style);
}

/// Apply the legacy WebKit line-clamp display adjustment from specified
/// provenance. Calling this after every accepted declaration makes the result
/// independent of whether display, orientation, or clamp appeared first.
pub(crate) fn normalize_webkit_line_clamp_display(style: &mut LayoutStyle) {
    let Some(inline) = style.webkit_box_display else {
        return;
    };
    if style.webkit_box_orient_vertical && style.webkit_line_clamp.is_some() {
        style.display = crate::Display::Block;
        style.is_inline_block = inline;
        style.flow_root = true;
    } else {
        style.display = crate::Display::Flex;
        style.is_inline_block = inline;
        style.flow_root = false;
    }
}

/// Parse the legacy positive-integer clamp and match Chromium's saturation at
/// the signed 32-bit maximum. CSS accepts arbitrarily long integer tokens;
/// rejecting them because a Rust integer conversion overflowed would make
/// both `CSS.supports()` and the cascade diverge from the browser.
fn webkit_line_clamp_value(value: &str) -> Option<Option<u32>> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none")
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "initial" | "unset" | "revert" | "revert-layer"
        )
    {
        return Some(None);
    }
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let first_nonzero = value.bytes().find(|byte| *byte != b'0')?;
    debug_assert!(first_nonzero.is_ascii_digit());
    let lines = value
        .parse::<u64>()
        .unwrap_or(u64::MAX)
        .min(i32::MAX as u64) as u32;
    Some(Some(lines))
}

/// Whether a declaration is syntactically supported by the renderer's CSS
/// property/value implementation. `@supports` and the JavaScript-facing
/// capability model must not treat every non-empty declaration as valid:
/// modern framework sheets use negative feature probes to isolate legacy
/// browser fallbacks, and activating those fallbacks corrupts the modern
/// cascade.
/// Return whether the renderer accepts and faithfully implements a CSS
/// declaration. This is also exposed to the JavaScript runtime so
/// `CSS.supports(property, value)` and stylesheet `@supports` cannot drift.
pub fn supports_declaration(name: &str, value: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    let value = value.trim();
    if value.is_empty() || has_invalid_supports_value_syntax(value) {
        return false;
    }
    let variable_syntax = supports_variable_substitution_syntax(value);
    if variable_syntax == VariableSubstitutionSyntax::Invalid {
        return false;
    }
    if name.starts_with("--") {
        return valid_custom_property_name(&name);
    }
    let css_wide = matches!(
        value.to_ascii_lowercase().as_str(),
        "initial" | "inherit" | "unset" | "revert" | "revert-layer"
    );
    let known = matches!(
        name.as_str(),
        "display"
            | "direction"
            | "width"
            | "inline-size"
            | "height"
            | "block-size"
            | "min-width"
            | "min-inline-size"
            | "min-height"
            | "min-block-size"
            | "max-width"
            | "max-inline-size"
            | "max-height"
            | "max-block-size"
            | "box-sizing"
            | "container"
            | "container-type"
            | "container-name"
            | "aspect-ratio"
            | "margin"
            | "margin-top"
            | "margin-right"
            | "margin-bottom"
            | "margin-left"
            | "margin-inline"
            | "margin-inline-start"
            | "margin-inline-end"
            | "margin-block"
            | "margin-block-start"
            | "margin-block-end"
            | "padding"
            | "padding-top"
            | "padding-right"
            | "padding-bottom"
            | "padding-left"
            | "padding-inline"
            | "padding-inline-start"
            | "padding-inline-end"
            | "padding-block"
            | "padding-block-start"
            | "padding-block-end"
            | "border-radius"
            | "border-top-left-radius"
            | "border-top-right-radius"
            | "border-bottom-right-radius"
            | "border-bottom-left-radius"
            | "clip-path"
            | "-webkit-clip-path"
            | "border"
            | "border-width"
            | "border-top-width"
            | "border-right-width"
            | "border-bottom-width"
            | "border-left-width"
            | "border-style"
            | "border-top-style"
            | "border-right-style"
            | "border-bottom-style"
            | "border-left-style"
            | "border-top-color"
            | "border-right-color"
            | "border-bottom-color"
            | "border-left-color"
            | "border-top"
            | "border-right"
            | "border-bottom"
            | "border-left"
            | "border-inline"
            | "border-block"
            | "border-inline-start"
            | "border-inline-end"
            | "border-block-start"
            | "border-block-end"
            | "border-inline-width"
            | "border-inline-style"
            | "border-inline-color"
            | "border-block-width"
            | "border-block-style"
            | "border-block-color"
            | "border-inline-start-width"
            | "border-inline-start-style"
            | "border-inline-start-color"
            | "border-inline-end-width"
            | "border-inline-end-style"
            | "border-inline-end-color"
            | "border-block-start-width"
            | "border-block-start-style"
            | "border-block-start-color"
            | "border-block-end-width"
            | "border-block-end-style"
            | "border-block-end-color"
            | "background"
            | "background-color"
            | "background-image"
            | "background-size"
            | "background-position"
            | "background-repeat"
            | "background-origin"
            | "background-clip"
            | "-webkit-background-clip"
            | "mask-image"
            | "-webkit-mask-image"
            | "mask-size"
            | "-webkit-mask-size"
            | "mask-repeat"
            | "-webkit-mask-repeat"
            | "color"
            | "content"
            | "-webkit-text-fill-color"
            | "fill"
            | "stroke"
            | "stroke-width"
            | "border-color"
            | "outline"
            | "outline-width"
            | "outline-style"
            | "outline-color"
            | "outline-offset"
            | "color-scheme"
            | "font-size"
            | "letter-spacing"
            | "font"
            | "font-weight"
            | "font-family"
            | "font-style"
            | "font-optical-sizing"
            | "font-variation-settings"
            | "text-align"
            | "text-indent"
            | "text-transform"
            | "text-decoration"
            | "text-decoration-line"
            | "line-height"
            | "white-space"
            | "text-overflow"
            | "-webkit-line-clamp"
            | "-webkit-box-orient"
            | "overflow-wrap"
            | "word-wrap"
            | "word-break"
            | "text-wrap"
            | "text-wrap-style"
            | "align-items"
            | "justify-items"
            | "place-items"
            | "align-self"
            | "justify-self"
            | "place-self"
            | "align-content"
            | "justify-content"
            | "place-content"
            | "flex-flow"
            | "flex-direction"
            | "flex-wrap"
            | "flex-grow"
            | "flex-shrink"
            | "flex-basis"
            | "flex"
            | "order"
            | "position"
            | "float"
            | "counter-reset"
            | "counter-increment"
            | "counter-set"
            | "object-fit"
            | "object-position"
            | "top"
            | "right"
            | "bottom"
            | "left"
            | "inset"
            | "inset-inline"
            | "inset-inline-start"
            | "inset-inline-end"
            | "inset-block"
            | "inset-block-start"
            | "inset-block-end"
            | "overflow"
            | "overflow-x"
            | "overflow-y"
            | "scrollbar-gutter"
            | "visibility"
            | "opacity"
            | "animation"
            | "animation-name"
            | "animation-duration"
            | "animation-delay"
            | "animation-fill-mode"
            | "animation-iteration-count"
            | "animation-direction"
            | "animation-play-state"
            | "z-index"
            | "clear"
            | "vertical-align"
            | "list-style"
            | "list-style-type"
            | "gap"
            | "grid-gap"
            | "row-gap"
            | "grid-row-gap"
            | "column-gap"
            | "grid-column-gap"
            | "-webkit-column-gap"
            | "column-count"
            | "-webkit-column-count"
            | "columns"
            | "-webkit-columns"
            | "break-inside"
            | "-webkit-column-break-inside"
            | "border-spacing"
            | "border-collapse"
            | "table-layout"
            | "grid-template-columns"
            | "grid-template-rows"
            | "grid-auto-columns"
            | "grid-auto-rows"
            | "grid-template-areas"
            | "grid-template"
            | "grid"
            | "grid-auto-flow"
            | "grid-area"
            | "grid-column"
            | "grid-row"
            | "grid-column-start"
            | "grid-column-end"
            | "grid-row-start"
            | "grid-row-end"
            | "transform"
            | "transform-origin"
            | "translate"
            | "rotate"
            | "scale"
            | "filter"
            | "backdrop-filter"
            | "-webkit-backdrop-filter"
            | "perspective"
            | "contain"
            | "will-change"
            | "content-visibility"
            | "box-shadow"
            | "-webkit-box-shadow"
    );
    if !known {
        return false;
    }
    // A syntactically valid var() makes the declaration valid at parse time;
    // its substituted value is checked later at computed-value time. Keep the
    // deliberately unadvertised effect stubs false: accepting a variable for
    // those properties would activate framework branches that we cannot paint.
    if variable_syntax == VariableSubstitutionSyntax::Valid
        && !matches!(
            name.as_str(),
            "filter"
                | "backdrop-filter"
                | "-webkit-backdrop-filter"
                | "perspective"
                | "contain"
                | "content-visibility"
        )
    {
        return true;
    }
    if css_wide {
        return true;
    }
    match name.as_str() {
        "display" => matches!(
            value.to_ascii_lowercase().as_str(),
            "none"
                | "flex"
                | "inline-flex"
                | "inline"
                | "inline-block"
                | "grid"
                | "inline-grid"
                | "block"
                | "flow-root"
                | "table"
                | "inline-table"
                | "-webkit-box"
                | "-webkit-inline-box"
                | "contents"
        ),
        "direction" => matches!(value.to_ascii_lowercase().as_str(), "ltr" | "rtl"),
        "position" => matches!(
            value.to_ascii_lowercase().as_str(),
            "static" | "relative" | "absolute" | "fixed" | "sticky"
        ),
        "box-sizing" => matches!(
            value.to_ascii_lowercase().as_str(),
            "content-box" | "border-box"
        ),
        "table-layout" => matches!(value.to_ascii_lowercase().as_str(), "auto" | "fixed"),
        "container-type" => parse_container_type(value).is_some(),
        "container-name" => parse_container_names(value).is_some(),
        "container" => parse_container_shorthand(value).is_some(),
        "font-optical-sizing" => matches!(value.to_ascii_lowercase().as_str(), "auto" | "none"),
        "font-variation-settings" => {
            value.eq_ignore_ascii_case("normal") || parse_font_variation_settings(value).is_some()
        }
        "white-space" => matches!(
            value.to_ascii_lowercase().as_str(),
            "normal" | "nowrap" | "pre" | "pre-wrap" | "pre-line" | "break-spaces"
        ),
        "text-overflow" => matches!(value.to_ascii_lowercase().as_str(), "clip" | "ellipsis"),
        "-webkit-line-clamp" => webkit_line_clamp_value(value).is_some(),
        "-webkit-box-orient" => matches!(
            value.to_ascii_lowercase().as_str(),
            "horizontal" | "vertical" | "inline-axis" | "block-axis"
        ),
        "overflow-wrap" | "word-wrap" => matches!(
            value.to_ascii_lowercase().as_str(),
            "normal" | "break-word" | "anywhere"
        ),
        "word-break" => matches!(
            value.to_ascii_lowercase().as_str(),
            "normal" | "break-all" | "keep-all" | "break-word"
        ),
        "text-wrap" => matches!(
            value.to_ascii_lowercase().as_str(),
            "auto" | "wrap" | "balance" | "wrap balance" | "balance wrap"
        ),
        "text-wrap-style" => matches!(value.to_ascii_lowercase().as_str(), "auto" | "balance"),
        "text-indent" => parse_text_indent(value).is_some(),
        "animation-duration" => parse_animation_time_ms(value).is_some_and(|time| time >= 0.0),
        "animation-delay" => parse_animation_time_ms(value).is_some(),
        "animation-iteration-count" => parse_animation_iteration_count(value).is_some(),
        "animation-direction" => parse_animation_direction(value).is_some(),
        "clip-path" | "-webkit-clip-path" => {
            value.eq_ignore_ascii_case("none") || parse_clip_path_polygon(value).is_some()
        }
        // These properties currently participate only in containing-block
        // bookkeeping. Advertising an effect that is not painted is worse
        // than a conservative false result because feature queries commonly
        // use them to select the only visible implementation of an effect.
        "filter" | "backdrop-filter" | "-webkit-backdrop-filter" | "perspective" => {
            value.eq_ignore_ascii_case("none")
        }
        "contain" => value.eq_ignore_ascii_case("none"),
        "content-visibility" => value.eq_ignore_ascii_case("visible"),
        "content" => supports_content_value(value),
        "animation-fill-mode" => parse_animation_fill_mode(value).is_some(),
        "animation-play-state" => parse_animation_play_state(value).is_some(),
        "float" => matches!(
            value.to_ascii_lowercase().as_str(),
            "none" | "left" | "right"
        ),
        "object-fit" => matches!(
            value.to_ascii_lowercase().as_str(),
            "fill" | "contain" | "cover" | "none" | "scale-down"
        ),
        "object-position" => {
            let tokens = split_ws_paren(value);
            !tokens.is_empty()
                && tokens.len() <= 4
                && tokens.iter().all(|token| {
                    matches!(*token, "left" | "right" | "top" | "bottom" | "center")
                        || token
                            .strip_suffix('%')
                            .and_then(|number| number.parse::<f32>().ok())
                            .is_some_and(f32::is_finite)
                        || px_value(token).is_some_and(f32::is_finite)
                })
        }
        "visibility" => matches!(
            value.to_ascii_lowercase().as_str(),
            "visible" | "hidden" | "collapse"
        ),
        "scrollbar-gutter" => matches!(
            value.to_ascii_lowercase().as_str(),
            "auto" | "stable" | "stable both-edges"
        ),
        "overflow" | "overflow-x" | "overflow-y" => {
            parse_overflow_declaration(&name, value).is_some()
        }
        "grid-area" => {
            let parts = split_top_level(value, '/')
                .into_iter()
                .map(str::trim)
                .collect::<Vec<_>>();
            !parts.is_empty()
                && parts.len() <= 4
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && parse_grid_line_kind(part).is_some())
        }
        "grid-auto-columns" | "grid-auto-rows" => parse_grid_auto_track_list(value).is_some(),
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            parse_border_shorthand(value, false).is_some()
        }
        "border-inline"
        | "border-block"
        | "border-inline-start"
        | "border-inline-end"
        | "border-block-start"
        | "border-block-end"
        | "border-inline-width"
        | "border-inline-style"
        | "border-inline-color"
        | "border-block-width"
        | "border-block-style"
        | "border-block-color"
        | "border-inline-start-width"
        | "border-inline-start-style"
        | "border-inline-start-color"
        | "border-inline-end-width"
        | "border-inline-end-style"
        | "border-inline-end-color"
        | "border-block-start-width"
        | "border-block-start-style"
        | "border-block-start-color"
        | "border-block-end-width"
        | "border-block-end-style"
        | "border-block-end-color" => supports_logical_border_declaration(&name, value),
        "border-width" => {
            let values = split_ws_paren(value)
                .iter()
                .map(|token| border_width(token))
                .collect::<Option<Vec<_>>>();
            values
                .and_then(|values| crate::border::expand_sides(&values))
                .is_some()
        }
        "border-top-width"
        | "border-right-width"
        | "border-bottom-width"
        | "border-left-width"
        | "outline-width" => border_width(value).is_some(),
        "border-style" => {
            let values = split_ws_paren(value)
                .iter()
                .map(|token| border_style(token))
                .collect::<Option<Vec<_>>>();
            values
                .and_then(|values| crate::border::expand_sides(&values))
                .is_some()
        }
        "border-top-style" | "border-right-style" | "border-bottom-style" | "border-left-style" => {
            border_style(value).is_some()
        }
        "border-color" => parse_border_colors(value, false).is_some(),
        "border-top-color"
        | "border-right-color"
        | "border-bottom-color"
        | "border-left-color"
        | "outline-color" => border_color(value, false).is_some(),
        "border-radius" => parsed_border_radii(value).is_some(),
        "border-top-left-radius"
        | "border-top-right-radius"
        | "border-bottom-right-radius"
        | "border-bottom-left-radius" => {
            let tokens = split_ws_paren(value);
            matches!(tokens.as_slice(), [one] if radius_value(one).is_some())
                || matches!(tokens.as_slice(), [x, y] if radius_value(x).is_some() && radius_value(y).is_some())
        }
        "outline" => parse_outline_shorthand(value, false).is_some(),
        "outline-style" => outline_style(value).is_some(),
        "outline-offset" => strict_border_length(value).is_some(),
        "color" | "-webkit-text-fill-color" | "background-color" => parse_color(value).is_some(),
        "color-scheme" => {
            let tokens: Vec<&str> = value.split_whitespace().collect();
            !tokens.is_empty()
                && tokens.iter().all(|token| {
                    matches!(
                        token.to_ascii_lowercase().as_str(),
                        "normal" | "light" | "dark" | "only"
                    )
                })
                && tokens.iter().any(|token| {
                    matches!(
                        token.to_ascii_lowercase().as_str(),
                        "normal" | "light" | "dark"
                    )
                })
        }
        _ => supports_conservative_known_value(&name, value),
    }
}

/// CSS.supports() parses a declaration value, not an entire declaration list.
/// A top-level semicolon, brace, or `!important` therefore makes the overload
/// invalid. Delimiters inside strings or functions remain ordinary tokens.
fn has_invalid_supports_value_syntax(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut depth = 0i32;
    let mut quote = None;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == active {
                quote = None;
            }
            index += 1;
            continue;
        }
        match byte {
            b'\\' => index += 1,
            b'\'' | b'"' => quote = Some(byte),
            b'(' | b'[' => depth += 1,
            b')' | b']' => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            b';' | b'{' | b'}' if depth == 0 => return true,
            b'!' if depth == 0
                && value[index + 1..]
                    .trim_start()
                    .get(..9)
                    .is_some_and(|word| word.eq_ignore_ascii_case("important")) =>
            {
                return true;
            }
            _ => {}
        }
        index += 1;
    }
    depth != 0 || quote.is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VariableSubstitutionSyntax {
    None,
    Valid,
    Invalid,
}

/// Validate var() at CSS token-stream time, before property-specific parsing.
/// Values containing a variable are deliberately not parsed against the
/// property's grammar until substitution. This is why `grid:var(--tw)` is a
/// true feature query in browsers even though `var(--tw)` is not a grid value
/// by itself.
fn supports_variable_substitution_syntax(value: &str) -> VariableSubstitutionSyntax {
    fn scan(value: &str, found: &mut bool) -> bool {
        let bytes = value.as_bytes();
        let mut index = 0usize;
        while index < bytes.len() {
            if matches!(bytes[index], b'\'' | b'"') {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index += 2;
                    } else if bytes[index] == quote {
                        index += 1;
                        break;
                    } else {
                        index += 1;
                    }
                }
                continue;
            }
            if bytes[index] == b'\\' {
                index += 2;
                continue;
            }
            if bytes[index] != b'(' {
                index += 1;
                continue;
            }

            let open = index;
            let mut ident_start = open;
            while ident_start > 0
                && matches!(bytes[ident_start - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_')
            {
                ident_start -= 1;
            }
            let Some(close) = matching_parenthesis(value, open) else {
                return false;
            };
            let arguments = &value[open + 1..close];
            if value[ident_start..open].eq_ignore_ascii_case("var") {
                let Some((name, fallback)) = split_variable_arguments(arguments) else {
                    return false;
                };
                if !valid_custom_property_name(name.trim()) {
                    return false;
                }
                if let Some(fallback) = fallback {
                    if has_top_level_variable_forbidden_token(fallback) != Some(false)
                        || !scan(fallback, found)
                    {
                        return false;
                    }
                }
                *found = true;
            } else if !scan(arguments, found) {
                return false;
            }
            index = close + 1;
        }
        true
    }

    let mut found = false;
    if !scan(value, &mut found) {
        VariableSubstitutionSyntax::Invalid
    } else if found {
        VariableSubstitutionSyntax::Valid
    } else {
        VariableSubstitutionSyntax::None
    }
}

fn matching_parenthesis(value: &str, open: usize) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut depth = 1usize;
    let mut quote = None;
    let mut index = open + 1;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == active {
                quote = None;
            }
        } else {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'\\' => {
                    index += 2;
                    continue;
                }
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index);
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    None
}

fn split_variable_arguments(arguments: &str) -> Option<(&str, Option<&str>)> {
    let bytes = arguments.as_bytes();
    let mut stack = Vec::new();
    let mut quote = None;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == active {
                quote = None;
            }
        } else {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'\\' => {
                    index += 2;
                    continue;
                }
                b'(' | b'[' | b'{' => stack.push(byte),
                close @ (b')' | b']' | b'}') => {
                    let expected = match close {
                        b')' => b'(',
                        b']' => b'[',
                        _ => b'{',
                    };
                    if stack.pop() != Some(expected) {
                        return None;
                    }
                }
                b',' if stack.is_empty() => {
                    return Some((&arguments[..index], Some(&arguments[index + 1..])));
                }
                _ => {}
            }
        }
        index += 1;
    }
    stack
        .is_empty()
        .then_some((arguments, None))
}

fn has_top_level_variable_forbidden_token(value: &str) -> Option<bool> {
    let bytes = value.as_bytes();
    let mut stack = Vec::new();
    let mut quote = None;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == active {
                quote = None;
            }
        } else {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'\\' => {
                    index += 2;
                    continue;
                }
                b'(' | b'[' | b'{' => stack.push(byte),
                close @ (b')' | b']' | b'}') => {
                    let expected = match close {
                        b')' => b'(',
                        b']' => b'[',
                        _ => b'{',
                    };
                    if stack.pop() != Some(expected) {
                        return None;
                    }
                }
                b'!' | b';' if stack.is_empty() => return Some(true),
                _ => {}
            }
        }
        index += 1;
    }
    stack.is_empty().then_some(false)
}

fn valid_custom_property_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("--") else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    let mut escaped = false;
    for character in rest.chars() {
        if escaped {
            if matches!(character, '\n' | '\r' | '\u{c}') {
                return false;
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if !(character.is_alphanumeric() || character == '-' || character == '_' || !character.is_ascii()) {
            return false;
        }
    }
    !escaped
}

fn supports_content_value(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    if matches!(lower.as_str(), "none" | "normal") || supports_single_url(value) {
        return true;
    }
    let mut rest = value.trim();
    let mut found = false;
    while !rest.is_empty() {
        rest = rest.trim_start();
        let Some(first) = rest.chars().next() else {
            break;
        };
        if matches!(first, '\'' | '"') {
            let mut escaped = false;
            let mut end = None;
            for (offset, character) in rest[first.len_utf8()..].char_indices() {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == first {
                    end = Some(first.len_utf8() + offset + first.len_utf8());
                    break;
                }
            }
            let Some(end) = end else { return false };
            rest = &rest[end..];
            found = true;
            continue;
        }
        let name_end = rest
            .char_indices()
            .find_map(|(index, ch)| {
                (!ch.is_alphanumeric() && !matches!(ch, '-' | '_' | '\\')).then_some(index)
            })
            .unwrap_or(rest.len());
        let name = rest[..name_end].to_ascii_lowercase();
        let tail = rest[name_end..].trim_start();
        if matches!(
            name.as_str(),
            "open-quote" | "close-quote" | "no-open-quote" | "no-close-quote"
        ) {
            rest = tail;
            found = true;
            continue;
        }
        if !matches!(name.as_str(), "attr" | "counter" | "counters") || !tail.starts_with('(') {
            return false;
        }
        let mut depth = 0i32;
        let mut quote = None;
        let mut escaped = false;
        let mut end = None;
        for (offset, character) in tail.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if character == '\\' {
                escaped = true;
                continue;
            }
            if let Some(active) = quote {
                if character == active {
                    quote = None;
                }
                continue;
            }
            match character {
                '\'' | '"' => quote = Some(character),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(offset + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { return false };
        let arguments = tail[1..end - 1].trim();
        if !supports_content_function(&name, arguments) {
            return false;
        }
        rest = &tail[end..];
        found = true;
    }
    found
}

fn supports_single_url(value: &str) -> bool {
    let value = value.trim();
    if !value
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("url("))
        || !value.ends_with(')')
    {
        return false;
    }
    let mut depth = 0i32;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return index + character.len_utf8() == value.len()
                        && parse_url(value).is_some();
                }
            }
            _ => {}
        }
    }
    false
}

fn supports_content_function(name: &str, arguments: &str) -> bool {
    let arguments = split_top_level(arguments, ',');
    let ident = |value: &str| {
        let value = value.trim();
        !value.is_empty()
            && value.chars().all(|character| {
                character.is_alphanumeric() || matches!(character, '-' | '_' | '\\')
            })
    };
    let counter_style = |value: &str| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "decimal"
                | "decimal-leading-zero"
                | "lower-alpha"
                | "lower-latin"
                | "upper-alpha"
                | "upper-latin"
                | "lower-roman"
                | "upper-roman"
        )
    };
    match name {
        "attr" => arguments.len() == 1 && arguments[0].split_whitespace().next().is_some_and(ident),
        "counter" => {
            (1..=2).contains(&arguments.len())
                && ident(arguments[0])
                && arguments.get(1).is_none_or(|value| counter_style(value))
        }
        "counters" => {
            (2..=3).contains(&arguments.len())
                && ident(arguments[0])
                && {
                    let separator = arguments[1].trim();
                    separator.len() >= 2
                        && matches!(separator.as_bytes()[0], b'\'' | b'"')
                        && separator.as_bytes().last() == separator.as_bytes().first()
                }
                && arguments.get(2).is_none_or(|value| counter_style(value))
        }
        _ => false,
    }
}

fn supports_conservative_known_value(name: &str, value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    let finite_number = |input: &str| {
        input
            .trim()
            .parse::<f32>()
            .ok()
            .is_some_and(|number| number.is_finite())
    };
    let dimension = |input: &str, auto: bool| {
        let input = input.trim();
        (auto && input.eq_ignore_ascii_case("auto"))
            || (!matches!(dimension_value(input), crate::Dimension::Auto)
                && resolve_contextual_length(input, 16.0, 16.0, 1.0, 1.0, 100.0).is_some())
            || (input.contains('(')
                && resolve_contextual_length(input, 16.0, 16.0, 1.0, 1.0, 100.0).is_some())
    };
    let dimensions = |input: &str, auto: bool, max: usize| {
        let tokens = split_ws_paren(input);
        !tokens.is_empty()
            && tokens.len() <= max
            && tokens.iter().all(|token| dimension(token, auto))
    };
    match name {
        "width" | "inline-size" => lower == "fit-content" || dimension(value, true),
        "height" | "block-size" | "min-width" | "min-inline-size" | "min-height"
        | "min-block-size" | "max-width" | "max-inline-size" | "max-height" | "max-block-size"
        | "flex-basis" => dimension(value, true),
        "margin" | "margin-inline" | "margin-block" => dimensions(value, true, 4),
        "margin-top"
        | "margin-right"
        | "margin-bottom"
        | "margin-left"
        | "margin-inline-start"
        | "margin-inline-end"
        | "margin-block-start"
        | "margin-block-end" => dimension(value, true),
        "padding" | "padding-inline" | "padding-block" | "inset" => dimensions(value, false, 4),
        "padding-top"
        | "padding-right"
        | "padding-bottom"
        | "padding-left"
        | "padding-inline-start"
        | "padding-inline-end"
        | "padding-block-start"
        | "padding-block-end" => dimension(value, false),
        "top" | "right" | "bottom" | "left" | "inset-inline" | "inset-block" => {
            dimensions(value, true, 2)
        }
        "inset-inline-start" | "inset-inline-end" | "inset-block-start" | "inset-block-end" => {
            dimension(value, true)
        }
        "aspect-ratio" => lower == "auto" || parse_aspect_ratio(value).is_some(),
        "align-items" | "justify-items" | "align-self" | "justify-self" => {
            self_alignment_value(value).is_some()
        }
        "place-items" | "place-self" => self_alignment_pair(value).is_some(),
        "align-content" => content_alignment_value(value).is_some(),
        "justify-content" => {
            matches!(lower.as_str(), "left" | "right") || content_alignment_value(value).is_some()
        }
        "place-content" => content_alignment_pair(value).is_some(),
        "flex-flow" => parse_flex_flow_shorthand(value).is_some(),
        "flex-direction" => matches!(
            lower.as_str(),
            "row" | "row-reverse" | "column" | "column-reverse"
        ),
        "flex-wrap" => matches!(lower.as_str(), "nowrap" | "wrap" | "wrap-reverse"),
        "flex-grow" | "flex-shrink" => {
            finite_number(value)
                && value
                    .trim()
                    .parse::<f32>()
                    .is_ok_and(|number| number >= 0.0)
        }
        "flex" => supports_flex_value(value),
        "order" => value.trim().parse::<i32>().is_ok(),
        "opacity" => finite_number(value),
        "z-index" => lower == "auto" || value.trim().parse::<i32>().is_ok(),
        "clear" => matches!(
            lower.as_str(),
            "none" | "left" | "right" | "both" | "inline-start" | "inline-end"
        ),
        "vertical-align" => matches!(
            lower.as_str(),
            "top" | "baseline" | "text-top" | "middle" | "bottom" | "text-bottom"
        ),
        "border-collapse" => matches!(lower.as_str(), "collapse" | "separate"),
        "border-spacing" => dimensions(value, false, 2),
        "column-count" | "-webkit-column-count" => {
            lower == "auto" || parse_column_count(value).is_some()
        }
        "columns" | "-webkit-columns" => split_ws_paren(value)
            .into_iter()
            .any(|token| parse_column_count(token).is_some()),
        "break-inside" => matches!(lower.as_str(), "auto" | "avoid" | "avoid-column"),
        "-webkit-column-break-inside" => matches!(lower.as_str(), "auto" | "avoid"),
        "grid-auto-flow" => {
            let tokens = lower.split_whitespace().collect::<Vec<_>>();
            !tokens.is_empty()
                && tokens.len() <= 2
                && tokens
                    .iter()
                    .all(|token| matches!(*token, "row" | "column" | "dense"))
        }
        "grid-template-columns" | "grid-template-rows" => {
            let (tracks, _, _) = parse_track_list_named(value);
            !tracks.is_empty() || lower.starts_with("subgrid")
        }
        "grid-template-areas" => !parse_grid_areas(value).is_empty() || lower == "none",
        "grid-column" | "grid-row" => split_top_level(value, '/')
            .into_iter()
            .all(|part| parse_grid_line_kind(part.trim()).is_some()),
        "grid-column-start" | "grid-column-end" | "grid-row-start" | "grid-row-end" => {
            parse_grid_line_kind(value).is_some()
        }
        "transform-origin" => parse_transform_origin(value).is_some(),
        "translate" => lower == "none" || dimensions(value, false, 3),
        "rotate" => lower == "none" || angle_degrees(value).is_some(),
        "scale" => {
            lower == "none" || {
                let values = value.split_whitespace().collect::<Vec<_>>();
                !values.is_empty()
                    && values.len() <= 2
                    && values.iter().all(|value| scale_number(value).is_some())
            }
        }
        "transform" => supports_transform_value(value),
        "background-color" | "color" | "-webkit-text-fill-color" => parse_color(value).is_some(),
        "background-image" | "mask-image" | "-webkit-mask-image" => {
            lower == "none"
                || parse_url(value).is_some()
                || !parse_background_gradient_layers(value, false).0.is_empty()
        }
        "background-repeat" | "mask-repeat" | "-webkit-mask-repeat" => {
            parse_image_repeat(value).is_some()
        }
        "background-size" | "mask-size" | "-webkit-mask-size" => {
            matches!(lower.as_str(), "auto" | "cover" | "contain")
                || parse_background_size(value).is_some()
        }
        "background-origin" => parse_background_origin(value).is_some(),
        "background-clip" | "-webkit-background-clip" => parse_background_clip(value).is_some(),
        "font-size" => is_font_size_token(value),
        "font-weight" => specified_font_weight(value).is_some(),
        "font-family" => !value.trim().is_empty(),
        "font-style" => lower == "normal" || lower == "italic" || lower.starts_with("oblique"),
        "text-align" => matches!(
            lower.as_str(),
            "left" | "right" | "start" | "end" | "center" | "justify"
        ),
        "text-transform" => matches!(
            lower.as_str(),
            "none" | "uppercase" | "lowercase" | "capitalize"
        ),
        "text-decoration" | "text-decoration-line" => lower
            .split_whitespace()
            .all(|token| matches!(token, "none" | "underline")),
        "line-height" => lower == "normal" || finite_number(value) || dimension(value, false),
        "gap" | "grid-gap" => dimensions(value, false, 2),
        "row-gap" | "grid-row-gap" | "column-gap" | "grid-column-gap" | "-webkit-column-gap" => {
            lower == "normal" || dimension(value, false)
        }
        "counter-reset" | "counter-increment" | "counter-set" => {
            parse_counter_directives(value, 0).is_some()
        }
        "list-style-type" => list_style_keyword(value.trim()).is_some(),
        "list-style" => value
            .split_whitespace()
            .any(|token| list_style_keyword(token).is_some()),
        "animation" | "animation-name" => !value.trim().is_empty(),
        "background" | "font" | "grid-template" | "grid" | "box-shadow" | "-webkit-box-shadow"
        | "fill" | "stroke" | "stroke-width" | "letter-spacing" | "will-change" => false,
        _ => false,
    }
}

fn supports_flex_value(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    if matches!(lower.as_str(), "none" | "auto" | "initial") {
        return true;
    }
    let tokens = split_ws_paren(value);
    if tokens.is_empty() || tokens.len() > 3 {
        return false;
    }
    let mut numbers = 0usize;
    for token in tokens {
        if token
            .parse::<f32>()
            .ok()
            .is_some_and(|number| number.is_finite() && number >= 0.0)
        {
            numbers += 1;
        } else if matches!(dimension_value(token), crate::Dimension::Auto)
            && !token.eq_ignore_ascii_case("auto")
        {
            return false;
        }
    }
    numbers <= 2
}

fn supports_transform_value(value: &str) -> bool {
    if value.trim().eq_ignore_ascii_case("none") {
        return true;
    }
    parse_transform_ops(value).is_some()
}

fn apply_animation_shorthand(style: &mut LayoutStyle, value: &str) {
    let first = split_top_level(value, ',')
        .into_iter()
        .next()
        .unwrap_or("")
        .trim();
    if first.is_empty() {
        return;
    }
    if matches!(
        first.to_ascii_lowercase().as_str(),
        "initial" | "inherit" | "unset" | "revert" | "revert-layer"
    ) {
        style.animation_name = None;
        style.animation_timing = crate::AnimationTiming::default();
        return;
    }

    let mut name = None;
    let mut timing = crate::AnimationTiming::default();
    let mut time_count = 0usize;
    for token in split_ws_paren(first) {
        let lower = token.to_ascii_lowercase();
        let timing_keyword = matches!(
            lower.as_str(),
            "linear" | "ease" | "ease-in" | "ease-out" | "ease-in-out"
        ) || lower.starts_with("cubic-bezier(")
            || lower.starts_with("steps(")
            || lower.starts_with("linear(");
        if let Some(time) = parse_animation_time_ms(&lower) {
            if time_count == 0 {
                if time < 0.0 {
                    return;
                }
                timing.duration_ms = time;
            } else if time_count == 1 {
                timing.delay_ms = time;
            } else {
                return;
            }
            time_count += 1;
        } else if let Some(direction) = parse_animation_direction(&lower) {
            timing.direction = direction;
        } else if let Some(fill) = parse_animation_fill_mode(&lower) {
            timing.fill_mode = fill;
        } else if let Some(play_state) = parse_animation_play_state(&lower) {
            timing.play_state = play_state;
        } else if let Some(iterations) = parse_animation_iteration_count(&lower) {
            timing.iteration_count = iterations;
        } else if timing_keyword {
            continue;
        } else if lower == "none" {
            if name.is_some() {
                return;
            }
        } else if name.is_none() {
            name = Some(token.to_string());
        } else {
            return;
        }
    }
    style.animation_name = name;
    style.animation_timing = timing;
}

fn parse_animation_direction(value: &str) -> Option<crate::AnimationDirection> {
    match value.trim().to_ascii_lowercase().as_str() {
        "normal" => Some(crate::AnimationDirection::Normal),
        "reverse" => Some(crate::AnimationDirection::Reverse),
        "alternate" => Some(crate::AnimationDirection::Alternate),
        "alternate-reverse" => Some(crate::AnimationDirection::AlternateReverse),
        _ => None,
    }
}

fn parse_animation_fill_mode(value: &str) -> Option<crate::AnimationFillMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(crate::AnimationFillMode::None),
        "forwards" => Some(crate::AnimationFillMode::Forwards),
        "backwards" => Some(crate::AnimationFillMode::Backwards),
        "both" => Some(crate::AnimationFillMode::Both),
        _ => None,
    }
}

fn parse_animation_play_state(value: &str) -> Option<crate::AnimationPlayState> {
    match value.trim().to_ascii_lowercase().as_str() {
        "running" => Some(crate::AnimationPlayState::Running),
        "paused" => Some(crate::AnimationPlayState::Paused),
        _ => None,
    }
}

fn parse_animation_iteration_count(value: &str) -> Option<f32> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("infinite") {
        return Some(f32::INFINITY);
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|count| count.is_finite() && *count >= 0.0)
}

#[derive(Clone, Copy)]
enum AnimationCalcValue {
    Number(f32),
    Milliseconds(f32),
}

impl AnimationCalcValue {
    fn negated(self) -> Self {
        match self {
            Self::Number(value) => Self::Number(-value),
            Self::Milliseconds(value) => Self::Milliseconds(-value),
        }
    }

    fn add(self, other: Self, subtract: bool) -> Option<Self> {
        let sign = if subtract { -1.0 } else { 1.0 };
        match (self, other) {
            (Self::Number(left), Self::Number(right)) => Some(Self::Number(left + sign * right)),
            (Self::Milliseconds(left), Self::Milliseconds(right)) => {
                Some(Self::Milliseconds(left + sign * right))
            }
            _ => None,
        }
    }

    fn multiply(self, other: Self) -> Option<Self> {
        match (self, other) {
            (Self::Number(left), Self::Number(right)) => Some(Self::Number(left * right)),
            (Self::Milliseconds(time), Self::Number(number))
            | (Self::Number(number), Self::Milliseconds(time)) => {
                Some(Self::Milliseconds(time * number))
            }
            _ => None,
        }
    }

    fn divide(self, other: Self) -> Option<Self> {
        match (self, other) {
            (_, Self::Number(0.0)) => None,
            (Self::Number(left), Self::Number(right)) => Some(Self::Number(left / right)),
            (Self::Milliseconds(time), Self::Number(number)) => {
                Some(Self::Milliseconds(time / number))
            }
            (Self::Milliseconds(left), Self::Milliseconds(right)) if right != 0.0 => {
                Some(Self::Number(left / right))
            }
            _ => None,
        }
    }
}

struct AnimationCalcParser<'a> {
    input: &'a [u8],
    position: usize,
}

impl AnimationCalcParser<'_> {
    fn skip_whitespace(&mut self) {
        while self
            .input
            .get(self.position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.position += 1;
        }
    }

    fn consume(&mut self, byte: u8) -> bool {
        self.skip_whitespace();
        if self.input.get(self.position) == Some(&byte) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn sum(&mut self) -> Option<AnimationCalcValue> {
        let mut value = self.product()?;
        loop {
            if self.consume(b'+') {
                value = value.add(self.product()?, false)?;
            } else if self.consume(b'-') {
                value = value.add(self.product()?, true)?;
            } else {
                return Some(value);
            }
        }
    }

    fn product(&mut self) -> Option<AnimationCalcValue> {
        let mut value = self.unary()?;
        loop {
            if self.consume(b'*') {
                value = value.multiply(self.unary()?)?;
            } else if self.consume(b'/') {
                value = value.divide(self.unary()?)?;
            } else {
                return Some(value);
            }
        }
    }

    fn unary(&mut self) -> Option<AnimationCalcValue> {
        if self.consume(b'+') {
            self.unary()
        } else if self.consume(b'-') {
            Some(self.unary()?.negated())
        } else {
            self.primary()
        }
    }

    fn primary(&mut self) -> Option<AnimationCalcValue> {
        if self.consume(b'(') {
            let value = self.sum()?;
            self.consume(b')').then_some(value)
        } else {
            self.number()
        }
    }

    fn number(&mut self) -> Option<AnimationCalcValue> {
        self.skip_whitespace();
        let start = self.position;
        let mut saw_digit = false;
        while let Some(byte) = self.input.get(self.position) {
            if byte.is_ascii_digit() {
                saw_digit = true;
                self.position += 1;
            } else if *byte == b'.' {
                self.position += 1;
            } else {
                break;
            }
        }
        if !saw_digit {
            return None;
        }
        let number = std::str::from_utf8(&self.input[start..self.position])
            .ok()?
            .parse::<f32>()
            .ok()?;
        if self.input.get(self.position..self.position + 2) == Some(b"ms") {
            self.position += 2;
            Some(AnimationCalcValue::Milliseconds(number))
        } else if self.input.get(self.position) == Some(&b's') {
            self.position += 1;
            Some(AnimationCalcValue::Milliseconds(number * 1000.0))
        } else {
            Some(AnimationCalcValue::Number(number))
        }
    }
}

pub(crate) fn parse_animation_time_ms(value: &str) -> Option<f32> {
    let normalized = value.trim().to_ascii_lowercase();
    let is_calc = normalized.starts_with("calc(") && normalized.ends_with(')');
    if !is_calc
        && normalized
            .bytes()
            .any(|byte| matches!(byte, b'+' | b'*' | b'/' | b'(' | b')'))
    {
        return None;
    }
    let expression = if is_calc {
        &normalized[5..normalized.len() - 1]
    } else {
        normalized.as_str()
    };
    let mut parser = AnimationCalcParser {
        input: expression.as_bytes(),
        position: 0,
    };
    let result = parser.sum()?;
    parser.skip_whitespace();
    if parser.position != parser.input.len() {
        return None;
    }
    match result {
        AnimationCalcValue::Milliseconds(milliseconds) if milliseconds.is_finite() => {
            Some(milliseconds)
        }
        _ => None,
    }
}

/// Parse the common CSS Box Alignment values that taffy can represent.
///
/// The outer option distinguishes an invalid declaration from `auto`, which
/// resets a preceding declaration to the inherited item-alignment behavior.
fn self_alignment_value(value: &str) -> Option<Option<taffy::AlignSelf>> {
    let normalized = value.trim().to_ascii_lowercase();
    let alignment = match normalized.as_str() {
        "auto" => return Some(None),
        "normal" => taffy::AlignSelf::NORMAL,
        "start" | "self-start" => taffy::AlignSelf::START,
        "end" | "self-end" => taffy::AlignSelf::END,
        "flex-start" => taffy::AlignSelf::FLEX_START,
        "flex-end" => taffy::AlignSelf::FLEX_END,
        "center" => taffy::AlignSelf::CENTER,
        "baseline" | "first baseline" => taffy::AlignSelf::BASELINE,
        "stretch" => taffy::AlignSelf::STRETCH,
        "safe start" | "safe self-start" => taffy::AlignSelf::SAFE_START,
        "safe end" | "safe self-end" => taffy::AlignSelf::SAFE_END,
        "safe flex-start" => taffy::AlignSelf::SAFE_FLEX_START,
        "safe flex-end" => taffy::AlignSelf::SAFE_FLEX_END,
        "safe center" => taffy::AlignSelf::SAFE_CENTER,
        "unsafe start" | "unsafe self-start" => taffy::AlignSelf::START,
        "unsafe end" | "unsafe self-end" => taffy::AlignSelf::END,
        "unsafe flex-start" => taffy::AlignSelf::FLEX_START,
        "unsafe flex-end" => taffy::AlignSelf::FLEX_END,
        "unsafe center" => taffy::AlignSelf::CENTER,
        _ => return None,
    };
    Some(Some(alignment))
}

fn self_alignment_pair(
    value: &str,
) -> Option<(Option<taffy::AlignSelf>, Option<taffy::JustifySelf>)> {
    if let Some(alignment) = self_alignment_value(value) {
        return Some((alignment, alignment));
    }
    let tokens: Vec<&str> = value.split_whitespace().collect();
    for split in 1..tokens.len() {
        let align = tokens[..split].join(" ");
        let justify = tokens[split..].join(" ");
        if let (Some(align), Some(justify)) =
            (self_alignment_value(&align), self_alignment_value(&justify))
        {
            return Some((align, justify));
        }
    }
    None
}

fn content_alignment_value(value: &str) -> Option<taffy::AlignContent> {
    let normalized = value.trim().to_ascii_lowercase();
    Some(match normalized.as_str() {
        "normal" | "stretch" => taffy::AlignContent::STRETCH,
        "start" => taffy::AlignContent::START,
        "end" => taffy::AlignContent::END,
        "flex-start" => taffy::AlignContent::FLEX_START,
        "flex-end" => taffy::AlignContent::FLEX_END,
        "center" => taffy::AlignContent::CENTER,
        "space-between" => taffy::AlignContent::SPACE_BETWEEN,
        "space-around" => taffy::AlignContent::SPACE_AROUND,
        "space-evenly" => taffy::AlignContent::SPACE_EVENLY,
        "safe start" => taffy::AlignContent::SAFE_START,
        "safe end" => taffy::AlignContent::SAFE_END,
        "safe flex-start" => taffy::AlignContent::SAFE_FLEX_START,
        "safe flex-end" => taffy::AlignContent::SAFE_FLEX_END,
        "safe center" => taffy::AlignContent::SAFE_CENTER,
        "unsafe start" => taffy::AlignContent::START,
        "unsafe end" => taffy::AlignContent::END,
        "unsafe flex-start" => taffy::AlignContent::FLEX_START,
        "unsafe flex-end" => taffy::AlignContent::FLEX_END,
        "unsafe center" => taffy::AlignContent::CENTER,
        _ => return None,
    })
}

fn content_alignment_pair(value: &str) -> Option<(taffy::AlignContent, taffy::JustifyContent)> {
    if let Some(alignment) = content_alignment_value(value) {
        return Some((alignment, alignment));
    }
    let tokens: Vec<&str> = value.split_whitespace().collect();
    for split in 1..tokens.len() {
        let align = tokens[..split].join(" ");
        let justify = tokens[split..].join(" ");
        if let (Some(align), Some(justify)) = (
            content_alignment_value(&align),
            content_alignment_value(&justify),
        ) {
            return Some((align, justify));
        }
    }
    None
}

fn parse_transform(style: &mut LayoutStyle, value: &str) {
    let v = value.trim();
    if v.is_empty() {
        return;
    }
    if matches!(
        v.to_ascii_lowercase().as_str(),
        "none" | "initial" | "unset" | "revert" | "revert-layer"
    ) {
        style.transform_ops.clear();
        set_containing_block_trigger(style, crate::CB_TRIGGER_TRANSFORM, false);
        return;
    }
    let Some(operations) = parse_transform_ops(v) else {
        return;
    };
    style.transform_ops = operations;
    set_containing_block_trigger(style, crate::CB_TRIGGER_TRANSFORM, true);
}

fn parse_transform_length(value: &str) -> Option<crate::TransformLength> {
    let value = value.trim();
    if value.contains('(') {
        let functions = transform_functions(value);
        if functions.len() != 1 {
            return None;
        }
        let (name, arguments) = &functions[0];
        let valid = match name.to_ascii_lowercase().as_str() {
            "calc" | "min" | "max" | "clamp" => {
                resolve_contextual_length(value, 16.0, 16.0, 10.0, 10.0, 100.0)
                    .is_some_and(f32::is_finite)
            }
            "var" => {
                let parts = split_top_level(arguments, ',');
                (1..=2).contains(&parts.len())
                    && parts[0].trim().starts_with("--")
                    && parts[0].trim().len() > 2
                    && parts.get(1).is_none_or(|fallback| {
                        !fallback.trim().is_empty() && parse_transform_length(fallback).is_some()
                    })
            }
            _ => false,
        };
        if !valid {
            return None;
        }
        return Some(crate::TransformLength {
            value: crate::Dimension::Px(0.0),
            expression: Some(value.to_string()),
        });
    }
    let value = dimension_value(value);
    (!matches!(value, crate::Dimension::Auto)).then_some(crate::TransformLength {
        value,
        expression: None,
    })
}

fn parse_transform_z_length(value: &str) -> Option<crate::TransformLength> {
    // The Z coordinate is `<length>`, not `<length-percentage>`.
    if value.contains('%') {
        return None;
    }
    parse_transform_length(value)
}

fn parse_transform_ops(value: &str) -> Option<Vec<crate::TransformOp>> {
    let functions = transform_functions(value);
    if functions.is_empty() {
        return None;
    }
    let mut operations = Vec::with_capacity(functions.len());
    for (name, arguments) in functions {
        let values = split_top_level(&arguments, ',')
            .into_iter()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        let operation = match name.to_ascii_lowercase().as_str() {
            "translate" if (1..=2).contains(&values.len()) => {
                let y = if let Some(value) = values.get(1) {
                    parse_transform_length(value)?
                } else {
                    crate::TransformLength::px(0.0)
                };
                crate::TransformOp::Translate(parse_transform_length(values[0])?, y)
            }
            "translatex" if values.len() == 1 => crate::TransformOp::Translate(
                parse_transform_length(values[0])?,
                crate::TransformLength::px(0.0),
            ),
            "translatey" if values.len() == 1 => crate::TransformOp::Translate(
                crate::TransformLength::px(0.0),
                parse_transform_length(values[0])?,
            ),
            "translate3d" if values.len() == 3 => {
                // Without perspective, translation along Z does not change the
                // projection of the element's plane. Keep accepting it so the
                // useful X/Y part of common compositor transforms is retained.
                parse_transform_z_length(values[2])?;
                crate::TransformOp::Translate(
                    parse_transform_length(values[0])?,
                    parse_transform_length(values[1])?,
                )
            }
            "translatez" if values.len() == 1 => {
                parse_transform_z_length(values[0])?;
                crate::TransformOp::Matrix(crate::Affine2::IDENTITY)
            }
            "scale" if (1..=2).contains(&values.len()) => {
                let x = scale_number(values[0])?;
                crate::TransformOp::Scale(
                    x,
                    values
                        .get(1)
                        .and_then(|value| scale_number(value))
                        .unwrap_or(x),
                )
            }
            "scalex" if values.len() == 1 => {
                crate::TransformOp::Scale(scale_number(values[0])?, 1.0)
            }
            "scaley" if values.len() == 1 => {
                crate::TransformOp::Scale(1.0, scale_number(values[0])?)
            }
            "scale3d" if values.len() == 3 => {
                let x = scale_number(values[0])?;
                let y = scale_number(values[1])?;
                scale_number(values[2])?;
                crate::TransformOp::Scale(x, y)
            }
            "scalez" if values.len() == 1 && scale_number(values[0]).is_some() => {
                crate::TransformOp::Matrix(crate::Affine2::IDENTITY)
            }
            "rotate" | "rotatez" if values.len() == 1 => {
                crate::TransformOp::Rotate(angle_degrees(values[0])?)
            }
            "rotatex" if values.len() == 1 => {
                let radians = angle_degrees(values[0])?.to_radians();
                crate::TransformOp::Scale(1.0, radians.cos())
            }
            "rotatey" if values.len() == 1 => {
                let radians = angle_degrees(values[0])?.to_radians();
                crate::TransformOp::Scale(radians.cos(), 1.0)
            }
            "rotate3d" if values.len() == 4 => {
                let x = values[0].parse::<f32>().ok()?;
                let y = values[1].parse::<f32>().ok()?;
                let z = values[2].parse::<f32>().ok()?;
                if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                    return None;
                }
                let angle = angle_degrees(values[3])?;
                if x != 0.0 && y == 0.0 && z == 0.0 {
                    crate::TransformOp::Scale(1.0, (angle * x.signum()).to_radians().cos())
                } else if x == 0.0 && y != 0.0 && z == 0.0 {
                    crate::TransformOp::Scale((angle * y.signum()).to_radians().cos(), 1.0)
                } else if x == 0.0 && y == 0.0 && z != 0.0 {
                    crate::TransformOp::Rotate(angle * z.signum())
                } else {
                    // A mixed 3D rotation axis cannot be represented by the
                    // affine renderer without tracking Z between operations.
                    return None;
                }
            }
            "skew" if (1..=2).contains(&values.len()) => crate::TransformOp::Skew(
                angle_degrees(values[0])?,
                values
                    .get(1)
                    .and_then(|value| angle_degrees(value))
                    .unwrap_or(0.0),
            ),
            "skewx" if values.len() == 1 => {
                crate::TransformOp::Skew(angle_degrees(values[0])?, 0.0)
            }
            "skewy" if values.len() == 1 => {
                crate::TransformOp::Skew(0.0, angle_degrees(values[0])?)
            }
            "matrix" if values.len() == 6 => {
                let numbers = values
                    .iter()
                    .map(|value| value.parse::<f32>().ok())
                    .collect::<Option<Vec<_>>>()?;
                if numbers.iter().any(|value| !value.is_finite()) {
                    return None;
                }
                crate::TransformOp::Matrix(crate::Affine2 {
                    a: numbers[0],
                    b: numbers[1],
                    c: numbers[2],
                    d: numbers[3],
                    e: numbers[4],
                    f: numbers[5],
                })
            }
            "matrix3d" if values.len() == 16 => {
                let numbers = values
                    .iter()
                    .map(|value| value.parse::<f32>().ok())
                    .collect::<Option<Vec<_>>>()?;
                if numbers.iter().any(|value| !value.is_finite())
                    || numbers[2] != 0.0
                    || numbers[3] != 0.0
                    || numbers[6] != 0.0
                    || numbers[7] != 0.0
                    || numbers[8] != 0.0
                    || numbers[9] != 0.0
                    || numbers[10] != 1.0
                    || numbers[11] != 0.0
                    || numbers[14] != 0.0
                    || numbers[15] != 1.0
                {
                    return None;
                }
                crate::TransformOp::Matrix(crate::Affine2 {
                    a: numbers[0],
                    b: numbers[1],
                    c: numbers[4],
                    d: numbers[5],
                    e: numbers[12],
                    f: numbers[13],
                })
            }
            _ => return None,
        };
        operations.push(operation);
    }
    Some(operations)
}

fn angle_degrees(value: &str) -> Option<f32> {
    fn primitive(value: &str) -> Option<f32> {
        let value = value.trim().to_ascii_lowercase();
        if let Some(number) = value.strip_suffix("deg") {
            number.trim().parse().ok()
        } else if let Some(number) = value.strip_suffix("grad") {
            number.trim().parse::<f32>().ok().map(|value| value * 0.9)
        } else if let Some(number) = value.strip_suffix("rad") {
            number.trim().parse::<f32>().ok().map(f32::to_degrees)
        } else if let Some(number) = value.strip_suffix("turn") {
            number.trim().parse::<f32>().ok().map(|value| value * 360.0)
        } else if value == "0" {
            Some(0.0)
        } else {
            None
        }
    }

    let value = value.trim();
    let Some(inner) = value
        .strip_prefix("calc(")
        .and_then(|value| value.strip_suffix(')'))
    else {
        return primitive(value).filter(|value| value.is_finite());
    };
    let result = if let Some((angle, factor)) = inner.split_once('*') {
        Some(primitive(angle)? * factor.trim().parse::<f32>().ok()?)
    } else if let Some((angle, divisor)) = inner.split_once('/') {
        let divisor = divisor.trim().parse::<f32>().ok()?;
        (divisor != 0.0)
            .then(|| primitive(angle).map(|angle| angle / divisor))
            .flatten()
    } else {
        primitive(inner)
    };
    result.filter(|value| value.is_finite())
}

fn parse_transform_origin(value: &str) -> Option<(crate::Dimension, crate::Dimension)> {
    let mut x = None;
    let mut y = None;
    for token in value.split_whitespace().take(2) {
        match token.to_ascii_lowercase().as_str() {
            "left" => x = Some(crate::Dimension::Percent(0.0)),
            "right" => x = Some(crate::Dimension::Percent(1.0)),
            "top" => y = Some(crate::Dimension::Percent(0.0)),
            "bottom" => y = Some(crate::Dimension::Percent(1.0)),
            "center" if x.is_none() => x = Some(crate::Dimension::Percent(0.5)),
            "center" => y = Some(crate::Dimension::Percent(0.5)),
            _ if x.is_none() => x = Some(dimension_value(token)),
            _ => y = Some(dimension_value(token)),
        }
    }
    if x.is_none() && y.is_none() {
        return None;
    }
    Some((
        x.unwrap_or(crate::Dimension::Percent(0.5)),
        y.unwrap_or(crate::Dimension::Percent(0.5)),
    ))
}

fn parse_individual_scale(style: &mut LayoutStyle, value: &str) {
    if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "none" | "initial" | "unset" | "revert" | "revert-layer"
    ) {
        style.individual_scale = None;
        set_containing_block_trigger(style, crate::CB_TRIGGER_SCALE, false);
        return;
    }
    let values: Vec<f32> = value
        .split_whitespace()
        .take(2)
        .filter_map(scale_number)
        .collect();
    let Some(&x) = values.first() else { return };
    let y = values.get(1).copied().unwrap_or(x);
    style.individual_scale = Some((x, y));
    set_containing_block_trigger(style, crate::CB_TRIGGER_SCALE, true);
}

fn parse_individual_rotate(style: &mut LayoutStyle, value: &str) {
    let value = value.trim();
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "none" | "initial" | "unset" | "revert" | "revert-layer"
    ) {
        style.individual_rotate = None;
        set_containing_block_trigger(style, crate::CB_TRIGGER_ROTATE, false);
        return;
    }
    let Some(angle) = angle_degrees(value) else {
        return;
    };
    style.individual_rotate = Some(angle);
    set_containing_block_trigger(style, crate::CB_TRIGGER_ROTATE, true);
}

fn parse_individual_translate(style: &mut LayoutStyle, value: &str) {
    let value = value.trim();
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "none" | "initial" | "unset" | "revert" | "revert-layer"
    ) {
        style.individual_translate = None;
        style.individual_translate_expressions = [None, None];
        set_containing_block_trigger(style, crate::CB_TRIGGER_TRANSLATE, false);
        return;
    }

    let values = split_ws_paren(value);
    if values.is_empty() || values.len() > 3 {
        return;
    }
    let component = |token: &str| {
        if token.contains('(') {
            Some((crate::Dimension::Px(0.0), Some(token.trim().to_string())))
        } else {
            let dimension = dimension_value(token);
            (!matches!(dimension, crate::Dimension::Auto)).then_some((dimension, None))
        }
    };
    let Some((x, x_expression)) = component(values[0]) else {
        return;
    };
    let Some((y, y_expression)) = values
        .get(1)
        .map(|value| component(value))
        .unwrap_or_else(|| Some((crate::Dimension::Px(0.0), None)))
    else {
        return;
    };
    // The optional third component is a z translation. The scoped renderer is
    // two-dimensional, but accepting a valid value still preserves x/y like
    // transform:translate3d() does.
    if values
        .get(2)
        .is_some_and(|value| component(value).is_none())
    {
        return;
    }
    style.individual_translate = Some((x, y));
    style.individual_translate_expressions = [x_expression, y_expression];
    set_containing_block_trigger(style, crate::CB_TRIGGER_TRANSLATE, true);
}

fn non_none_value(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && !value.eq_ignore_ascii_case("none")
}

fn set_containing_block_trigger(style: &mut LayoutStyle, trigger: u16, enabled: bool) {
    if enabled {
        style.containing_block_triggers |= trigger;
    } else {
        style.containing_block_triggers &= !trigger;
    }
}

/// Strictly split a `transform` value into `name(args)` functions in source
/// order. Any non-whitespace between functions, missing delimiter, or
/// unbalanced parenthesis invalidates the whole declaration.
fn transform_functions(value: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = value.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'-')
        {
            cursor += 1;
        }
        if cursor == name_start || cursor >= bytes.len() || bytes[cursor] != b'(' {
            return Vec::new();
        }
        let name = value[name_start..cursor].to_string();
        cursor += 1;
        let args_start = cursor;
        let mut depth = 1i32;
        let mut end = None;
        for (offset, c) in value[cursor..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(cursor + offset);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { return Vec::new() };
        out.push((name, value[args_start..end].to_string()));
        cursor = end + 1;
    }
    out
}

/// Parse a unitless scale factor. A trailing `%` (`scale(50%)`) is accepted and
/// divided by 100, matching the individual `scale` property's percentage form.
fn scale_number(s: &str) -> Option<f32> {
    let t = s.trim();
    if let Some(p) = t.strip_suffix('%') {
        return p
            .trim()
            .parse::<f32>()
            .ok()
            .map(|value| value / 100.0)
            .filter(|value| value.is_finite());
    }
    t.parse::<f32>().ok().filter(|value| value.is_finite())
}

/// Parse a CSS grid track list (`min-content 1fr min-content`, `12.25rem
/// minmax(0,1fr)`) into taffy sizing functions. Tokenizes respecting the
/// parentheses in `minmax(...)` / `fit-content(...)`.
#[repr(align(8))]
#[derive(Debug)]
pub(crate) struct GridCalcExpression {
    expression: String,
    em_px: std::sync::atomic::AtomicU32,
    rem_px: std::sync::atomic::AtomicU32,
    vw: std::sync::atomic::AtomicU32,
    vh: std::sync::atomic::AtomicU32,
    context_initialized: std::sync::atomic::AtomicBool,
}

impl GridCalcExpression {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        if !(value.starts_with("calc(")
            || value.starts_with("min(")
            || value.starts_with("max(")
            || value.starts_with("clamp(")
            || value.starts_with("round("))
        {
            return None;
        }

        // Track math is resolved late against the actual grid-axis basis.
        // Accept only supported length units, numbers, and math function
        // names. The computed font and viewport contexts are attached by the
        // top-down style pass before Taffy resolves the percentage basis.
        for word in value
            .split(|character: char| !character.is_ascii_alphabetic() && character != '-')
            .filter(|word| !word.is_empty() && *word != "-")
        {
            if !matches!(
                word,
                "calc"
                    | "min"
                    | "max"
                    | "clamp"
                    | "round"
                    | "nearest"
                    | "up"
                    | "down"
                    | "to-zero"
                    | "px"
                    | "pt"
                    | "em"
                    | "rem"
                    | "ex"
                    | "vw"
                    | "vh"
                    | "dvw"
                    | "dvh"
                    | "svw"
                    | "svh"
                    | "lvw"
                    | "lvh"
                    | "vmin"
                    | "vmax"
            ) {
                return None;
            }
        }

        // Probe more than one basis so malformed expressions and non-finite
        // arithmetic never become an opaque Taffy handle.
        for basis in [0.0, 100.0] {
            let resolved = resolve_contextual_length(&value, 16.0, 16.0, 10.0, 10.0, basis)?;
            if !resolved.is_finite() {
                return None;
            }
        }
        Some(Self {
            expression: value,
            em_px: std::sync::atomic::AtomicU32::new(16.0f32.to_bits()),
            rem_px: std::sync::atomic::AtomicU32::new(16.0f32.to_bits()),
            vw: std::sync::atomic::AtomicU32::new(0.0f32.to_bits()),
            vh: std::sync::atomic::AtomicU32::new(0.0f32.to_bits()),
            context_initialized: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn set_context(&self, em_px: f32, rem_px: f32, vw: f32, vh: f32) {
        use std::sync::atomic::Ordering;
        // Viewport units are used values and must follow every new layout
        // viewport, including repeated calls through the public LayoutNode API.
        self.vw.store(vw.to_bits(), Ordering::Relaxed);
        self.vh.store(vh.to_bits(), Ordering::Relaxed);
        // A cloned handle represents the same computed track value. In
        // particular, explicit `inherit` must preserve the parent's em basis
        // even when the child has a different font-size.
        if self.context_initialized.load(Ordering::Acquire) {
            return;
        }
        self.em_px.store(em_px.to_bits(), Ordering::Relaxed);
        self.rem_px.store(rem_px.to_bits(), Ordering::Relaxed);
        self.context_initialized.store(true, Ordering::Release);
    }
}

pub(crate) fn set_grid_calc_context(
    style: &LayoutStyle,
    em_px: f32,
    rem_px: f32,
    vw: f32,
    vh: f32,
) {
    for expression in style.grid_calc_expressions.iter().flatten() {
        expression.set_context(em_px, rem_px, vw, vh);
    }
}

/// Resolve an Arc-backed grid calc handle installed in Taffy.
///
/// # Safety
///
/// `value` must point to a live `GridCalcExpression`. `LayoutStyle` retains
/// the Arc owners for the entire lifetime of every layout tree computation.
#[allow(unsafe_code)]
pub(crate) fn resolve_grid_calc(value: *const (), basis: f32) -> f32 {
    // SAFETY: upheld by `new_taffy_tree` and the Arc ownership documented on
    // `LayoutStyle::grid_calc_expressions`.
    let calc = unsafe { &*value.cast::<GridCalcExpression>() };
    use std::sync::atomic::Ordering;
    resolve_contextual_length(
        &calc.expression,
        f32::from_bits(calc.em_px.load(Ordering::Relaxed)),
        f32::from_bits(calc.rem_px.load(Ordering::Relaxed)),
        f32::from_bits(calc.vw.load(Ordering::Relaxed)),
        f32::from_bits(calc.vh.load(Ordering::Relaxed)),
        basis,
    )
        .filter(|value| value.is_finite())
        .unwrap_or(0.0)
        .max(0.0)
}

/// Parse a track list into taffy sizing functions plus the `[line-name]` map
/// (name -> 1-based grid line number). `repeat(n, ...)` is expanded to n copies;
/// auto-fill/auto-fit remain typed repetitions so layout can derive their count
/// from the used container width. `[name]` annotations are captured rather than
/// turned into tracks. First occurrence of a name wins.
pub(crate) fn parse_track_list_named(
    value: &str,
) -> (
    Vec<taffy::GridTemplateComponent<String>>,
    Vec<(String, i16)>,
    Vec<std::sync::Arc<GridCalcExpression>>,
) {
    let tokens = tokenize_tracks(value);
    let mut tracks = Vec::new();
    let mut names = Vec::new();
    let mut calc_expressions = Vec::new();
    let mut line: i16 = 1;
    // A subgridded axis owns line names but no sizing functions. Do not turn
    // the keyword into the generic unknown-token => auto-track fallback.
    let is_subgrid = tokens
        .first()
        .map(|token| token.eq_ignore_ascii_case("subgrid"))
        .unwrap_or(false);
    for tok in tokens.into_iter().skip(usize::from(is_subgrid)) {
        expand_track_token(
            &tok,
            &mut tracks,
            &mut names,
            &mut calc_expressions,
            &mut line,
        );
    }
    (tracks, names, calc_expressions)
}

fn expand_track_token(
    tok: &str,
    tracks: &mut Vec<taffy::GridTemplateComponent<String>>,
    names: &mut Vec<(String, i16)>,
    calc_expressions: &mut Vec<std::sync::Arc<GridCalcExpression>>,
    line: &mut i16,
) {
    let t = tok.trim();
    if t.starts_with('[') {
        let inner = t.trim_start_matches('[').trim_end_matches(']');
        for name in inner.split_whitespace() {
            names.push((name.to_string(), *line));
        }
        return;
    }
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("repeat(") && t.ends_with(')') {
        let inner = &t["repeat(".len()..t.len() - 1];
        if let Some((cnt, sub)) = inner.split_once(',') {
            let subtoks = tokenize_tracks(sub.trim());
            let repetition = match cnt.trim().to_ascii_lowercase().as_str() {
                "auto-fill" => Some(taffy::RepetitionCount::AutoFill),
                "auto-fit" => Some(taffy::RepetitionCount::AutoFit),
                _ => None,
            };
            if let Some(count) = repetition {
                let repeated_tracks = subtoks
                    .iter()
                    .filter(|st| !st.trim_start().starts_with('['))
                    .map(|st| track(st, calc_expressions))
                    .collect();
                tracks.push(taffy::GridTemplateComponent::Repeat(
                    taffy::GridTemplateRepetition {
                        count,
                        tracks: repeated_tracks,
                        line_names: Vec::new(),
                    },
                ));
                *line += 1;
                return;
            }
            let count = cnt.trim().parse::<usize>().unwrap_or(1).min(1000);
            for _ in 0..count {
                for st in &subtoks {
                    expand_track_token(st, tracks, names, calc_expressions, line);
                }
            }
        }
        return;
    }
    tracks.push(taffy::GridTemplateComponent::Single(track(
        t,
        calc_expressions,
    )));
    *line += 1;
}

pub(crate) fn build_line_map(pairs: Vec<(String, i16)>) -> std::collections::HashMap<String, i16> {
    let mut m = std::collections::HashMap::new();
    for (name, line) in pairs {
        m.entry(name).or_insert(line);
    }
    m
}

/// Split a track list on whitespace while keeping `func(a, b)` groups and
/// `[line-name lists]` intact.
fn tokenize_tracks(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut in_bracket = false;
    for c in value.chars() {
        match c {
            '[' => {
                in_bracket = true;
                cur.push(c);
            }
            ']' => {
                in_bracket = false;
                cur.push(c);
            }
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            c if c.is_whitespace() && depth == 0 && !in_bracket => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn track(
    tok: &str,
    calc_expressions: &mut Vec<std::sync::Arc<GridCalcExpression>>,
) -> taffy::TrackSizingFunction {
    use taffy::MinMax;
    let t = tok.trim();
    let lower = t.to_ascii_lowercase();
    if let Some(inner) = lower
        .strip_prefix("minmax(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let arguments = split_top_level(inner, ',');
        if let [a, b] = arguments.as_slice() {
            return MinMax {
                min: min_track(a.trim(), calc_expressions),
                max: max_track(b.trim(), calc_expressions),
            };
        }
    }
    if let Some(inner) = lower
        .strip_prefix("fit-content(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let limit = inner.trim();
        let max = if let Some(percent) = limit
            .strip_suffix('%')
            .and_then(|number| number.trim().parse::<f32>().ok())
        {
            taffy::MaxTrackSizingFunction::fit_content_percent(percent / 100.0)
        } else if let Some(px) = px_value(limit) {
            taffy::MaxTrackSizingFunction::fit_content_px(px)
        } else {
            // Taffy has no opaque-calc form for fit-content's distinct clamp
            // semantics. Preserve a non-collapsing intrinsic fallback rather
            // than turning a valid expression into fit-content(0px).
            taffy::MaxTrackSizingFunction::auto()
        };
        return MinMax {
            min: taffy::MinTrackSizingFunction::auto(),
            max,
        };
    }
    MinMax {
        min: min_track(t, calc_expressions),
        max: max_track(t, calc_expressions),
    }
}

/// Parse the `<track-size>+` grammar shared by `grid-auto-columns` and
/// `grid-auto-rows`. Unlike template track lists, implicit track lists do not
/// accept line names or `repeat()`: the authored list itself is repeated as
/// needed for successive implicit tracks.
fn parse_grid_auto_track_list(
    value: &str,
) -> Option<(
    Vec<taffy::TrackSizingFunction>,
    Vec<std::sync::Arc<GridCalcExpression>>,
)> {
    let tokens = tokenize_tracks(value);
    if tokens.is_empty()
        || tokens.iter().any(|token| {
            let lower = token.trim().to_ascii_lowercase();
            lower.starts_with('[')
                || lower.starts_with("repeat(")
                || lower == "subgrid"
                || matches!(
                    lower.as_str(),
                    "initial" | "inherit" | "unset" | "revert" | "revert-layer"
                )
                || !valid_grid_auto_track(&lower)
        })
    {
        return None;
    }
    let mut calc_expressions = Vec::new();
    let tracks = tokens
        .iter()
        .map(|token| track(token, &mut calc_expressions))
        .collect();
    Some((tracks, calc_expressions))
}

fn valid_grid_auto_track(value: &str) -> bool {
    if matches!(value, "auto" | "min-content" | "max-content") {
        return true;
    }
    if let Some(inner) = value
        .strip_prefix("minmax(")
        .and_then(|inner| inner.strip_suffix(')'))
    {
        let Some((min, max)) = inner.split_once(',') else {
            return false;
        };
        return valid_grid_track_breadth(min.trim(), false)
            && valid_grid_track_breadth(max.trim(), true);
    }
    if let Some(inner) = value
        .strip_prefix("fit-content(")
        .and_then(|inner| inner.strip_suffix(')'))
    {
        return valid_grid_track_length_percentage(inner.trim());
    }
    valid_grid_track_breadth(value, true)
}

fn valid_grid_track_breadth(value: &str, flex_allowed: bool) -> bool {
    matches!(value, "auto" | "min-content" | "max-content")
        || (flex_allowed
            && value
                .strip_suffix("fr")
                .and_then(|number| number.trim().parse::<f32>().ok())
                .is_some_and(|number| number.is_finite() && number >= 0.0))
        || valid_grid_track_length_percentage(value)
}

fn valid_grid_track_length_percentage(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if GridCalcExpression::parse(&lower).is_some() {
        return true;
    }
    if lower == "0" {
        return true;
    }
    for suffix in [
        "rem", "vmin", "vmax", "px", "pt", "em", "ex", "vw", "vh", "%",
    ] {
        if let Some(number) = lower.strip_suffix(suffix) {
            return number
                .trim()
                .parse::<f32>()
                .ok()
                .is_some_and(|number| number.is_finite() && number >= 0.0);
        }
    }
    false
}

fn apply_grid_auto_tracks(style: &mut LayoutStyle, value: &str, columns: bool) {
    let lower = value.trim().to_ascii_lowercase();
    let (tracks, inherit, calc_expressions) = match lower.as_str() {
        "inherit" => (Vec::new(), true, Vec::new()),
        // These properties are non-inherited. The compact cascade has no
        // retained lower-origin/layer declaration for revert, so it follows
        // the existing non-inherited-property policy and uses the initial
        // automatic implicit track.
        "initial" | "unset" | "revert" | "revert-layer" => {
            (Vec::new(), false, Vec::new())
        }
        _ => {
            let Some((tracks, calc_expressions)) = parse_grid_auto_track_list(value) else {
                return;
            };
            (tracks, false, calc_expressions)
        }
    };
    if columns {
        style.grid_auto_columns = tracks;
        style.grid_calc_expressions[2] = calc_expressions;
        style.grid_auto_columns_inherit = inherit;
    } else {
        style.grid_auto_rows = tracks;
        style.grid_calc_expressions[3] = calc_expressions;
        style.grid_auto_rows_inherit = inherit;
    }
}

fn min_track(
    tok: &str,
    calc_expressions: &mut Vec<std::sync::Arc<GridCalcExpression>>,
) -> taffy::MinTrackSizingFunction {
    use taffy::MinTrackSizingFunction as M;
    let lower = tok.to_ascii_lowercase();
    match lower.as_str() {
        "min-content" => M::min_content(),
        "max-content" => M::max_content(),
        "auto" => M::auto(),
        other => {
            if other.ends_with("fr") {
                // Flexible tracks have an automatic minimum.
                M::auto()
            } else if let Some(p) = other
                .strip_suffix('%')
                .and_then(|n| n.trim().parse::<f32>().ok())
            {
                M::percent(p / 100.0)
            } else if let Some(px) = px_value(other) {
                M::length(px)
            } else if let Some(calc) = GridCalcExpression::parse(other) {
                let calc = std::sync::Arc::new(calc);
                let handle = std::sync::Arc::as_ptr(&calc).cast();
                calc_expressions.push(calc);
                M::calc(handle)
            } else {
                M::auto()
            }
        }
    }
}

fn max_track(
    tok: &str,
    calc_expressions: &mut Vec<std::sync::Arc<GridCalcExpression>>,
) -> taffy::MaxTrackSizingFunction {
    use taffy::MaxTrackSizingFunction as M;
    let lower = tok.to_ascii_lowercase();
    match lower.as_str() {
        "min-content" => M::min_content(),
        "max-content" => M::max_content(),
        "auto" => M::auto(),
        other => {
            if let Some(fr) = other
                .strip_suffix("fr")
                .and_then(|n| n.trim().parse::<f32>().ok())
            {
                M::fr(fr)
            } else if let Some(p) = other
                .strip_suffix('%')
                .and_then(|n| n.trim().parse::<f32>().ok())
            {
                M::percent(p / 100.0)
            } else if let Some(px) = px_value(other) {
                M::length(px)
            } else if let Some(calc) = GridCalcExpression::parse(other) {
                let calc = std::sync::Arc::new(calc);
                let handle = std::sync::Arc::as_ptr(&calc).cast();
                calc_expressions.push(calc);
                M::calc(handle)
            } else {
                M::auto()
            }
        }
    }
}

/// Parse `grid-template-areas: 'a a' 'b c'` into a matrix of cell names.
fn parse_grid_areas(value: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut in_str = false;
    let mut cur = String::new();
    for c in value.chars() {
        match c {
            '\'' | '"' => {
                if in_str {
                    rows.push(
                        cur.split_whitespace()
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>(),
                    );
                    cur.clear();
                    in_str = false;
                } else {
                    in_str = true;
                }
            }
            _ if in_str => cur.push(c),
            _ => {}
        }
    }
    rows
}

/// Parse the `grid-template: <rows> / <cols>` shorthand. The rows side may embed
/// area strings; we support the common `tracks / tracks` form and, when area
/// strings are present, extract them too.
fn parse_grid_template(style: &mut LayoutStyle, value: &str) {
    let (rows_part, cols_part) = match value.split_once('/') {
        Some((r, c)) => (r.trim(), Some(c.trim())),
        None => (value.trim(), None),
    };
    if rows_part.contains('\'') || rows_part.contains('"') {
        // Rows side carries area strings interleaved with row track sizes.
        style.grid_areas = Some(parse_grid_areas(rows_part));
    } else if !rows_part.is_empty() {
        let (tracks, names, calc_expressions) = parse_track_list_named(rows_part);
        style.grid_template_rows = tracks;
        style.grid_calc_expressions[1] = calc_expressions;
        style.grid_row_line_names = (!names.is_empty()).then(|| build_line_map(names));
    }
    if let Some(cols) = cols_part {
        let (tracks, names, calc_expressions) = parse_track_list_named(cols);
        style.grid_template_columns_subgrid = is_subgrid_track_list(cols);
        style.grid_template_columns = tracks;
        style.grid_calc_expressions[0] = calc_expressions;
        style.grid_col_line_names = (!names.is_empty()).then(|| build_line_map(names));
    }
}

/// Parse the common `grid` shorthand forms. A side containing `auto-flow`
/// defines the implicit placement axis; the opposite side is the explicit
/// track list. Without `auto-flow`, this is the `grid-template` shorthand.
fn parse_grid_shorthand(style: &mut LayoutStyle, value: &str) {
    let Some((rows, columns)) = value.split_once('/') else {
        parse_grid_template(style, value);
        return;
    };
    let rows = rows.trim();
    let columns = columns.trim();
    if rows.to_ascii_lowercase().contains("auto-flow") {
        style.grid_template_rows.clear();
        style.grid_calc_expressions[1].clear();
        let (tracks, names, calc_expressions) = parse_track_list_named(columns);
        style.grid_template_columns_subgrid = is_subgrid_track_list(columns);
        style.grid_template_columns = tracks;
        style.grid_calc_expressions[0] = calc_expressions;
        style.grid_col_line_names = (!names.is_empty()).then(|| build_line_map(names));
        style.grid_auto_flow = Some(if rows.to_ascii_lowercase().contains("dense") {
            taffy::GridAutoFlow::RowDense
        } else {
            taffy::GridAutoFlow::Row
        });
    } else if columns.to_ascii_lowercase().contains("auto-flow") {
        style.grid_template_columns.clear();
        style.grid_calc_expressions[0].clear();
        style.grid_template_columns_subgrid = false;
        let (tracks, names, calc_expressions) = parse_track_list_named(rows);
        style.grid_template_rows = tracks;
        style.grid_calc_expressions[1] = calc_expressions;
        style.grid_row_line_names = (!names.is_empty()).then(|| build_line_map(names));
        style.grid_auto_flow = Some(if columns.to_ascii_lowercase().contains("dense") {
            taffy::GridAutoFlow::ColumnDense
        } else {
            taffy::GridAutoFlow::Column
        });
    } else {
        parse_grid_template(style, value);
    }
}

fn is_subgrid_track_list(value: &str) -> bool {
    tokenize_tracks(value)
        .first()
        .map(|token| token.eq_ignore_ascii_case("subgrid"))
        .unwrap_or(false)
}

fn parse_grid_auto_flow(value: &str) -> Option<taffy::GridAutoFlow> {
    let lower = value.trim().to_ascii_lowercase();
    let dense = lower.split_whitespace().any(|token| token == "dense");
    let column = lower.split_whitespace().any(|token| token == "column");
    Some(match (column, dense) {
        (false, false) => taffy::GridAutoFlow::Row,
        (false, true) => taffy::GridAutoFlow::RowDense,
        (true, false) => taffy::GridAutoFlow::Column,
        (true, true) => taffy::GridAutoFlow::ColumnDense,
    })
}

/// Expand the `grid-area` shorthand into its row/column placements.
///
/// The four slash-separated components are, in order, row-start,
/// column-start, row-end, and column-end. Missing end values repeat a
/// custom-ident start (the named-area form) and otherwise become `auto`, per
/// CSS Grid. Keeping the expansion in the computed style is important for the
/// common overlay idiom `grid-area: 1 / 1 / 1 / 1`; treating every slash form
/// as unsupported makes each child auto-place into a new implicit row.
fn set_grid_area(style: &mut LayoutStyle, value: &str) {
    if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "inherit" | "unset" | "revert" | "revert-layer"
    ) {
        style.grid_area_name = None;
        set_grid_placement(style, "auto / auto", true);
        set_grid_placement(style, "auto / auto", false);
        return;
    }

    let parts = split_top_level(value, '/')
        .into_iter()
        .map(str::trim)
        .collect::<Vec<_>>();
    if parts.is_empty()
        || parts.len() > 4
        || parts.iter().any(|part| part.is_empty())
        || parts
            .iter()
            .any(|part| parse_grid_line_kind(part).is_none())
    {
        return;
    }

    let row_start = parts[0];
    let column_start = parts
        .get(1)
        .copied()
        .unwrap_or_else(|| grid_area_omitted_side(row_start));
    let row_end = parts
        .get(2)
        .copied()
        .unwrap_or_else(|| grid_area_omitted_side(row_start));
    let column_end = parts
        .get(3)
        .copied()
        .unwrap_or_else(|| grid_area_omitted_side(column_start));

    // Preserve the compact named-area representation used by
    // `resolve_grid_areas`, while also retaining the generated `name-start` /
    // `name-end` line form for grids that expose those named lines directly.
    if parts.len() == 1 && is_grid_custom_ident(row_start) {
        style.grid_area_name = Some(row_start.to_string());
    } else {
        style.grid_area_name = None;
    }

    set_grid_placement(style, &format!("{column_start} / {column_end}"), true);
    set_grid_placement(style, &format!("{row_start} / {row_end}"), false);
}

fn grid_area_omitted_side(start: &str) -> &str {
    if is_grid_custom_ident(start) {
        start
    } else {
        "auto"
    }
}

fn is_grid_custom_ident(value: &str) -> bool {
    matches!(parse_grid_line_kind(value), Some(GridLineKind::IdentOnly))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GridLineKind {
    IdentOnly,
    Other,
}

/// Validate one `<grid-line>` without mutating the computed style. Shorthand
/// declarations are atomic: if any of their one-to-four components is invalid,
/// the entire declaration must be ignored rather than partially resetting
/// earlier winning longhands.
fn parse_grid_line_kind(value: &str) -> Option<GridLineKind> {
    let mut input = cssparser::ParserInput::new(value.trim());
    let mut parser = cssparser::Parser::new(&mut input);

    // auto
    {
        let state = parser.state();
        if parser
            .expect_ident_cloned()
            .is_ok_and(|ident| ident.eq_ignore_ascii_case("auto"))
            && parser.is_exhausted()
        {
            return Some(GridLineKind::Other);
        }
        parser.reset(&state);
    }

    // span && [ <positive-integer> || <custom-ident> ]
    {
        let state = parser.state();
        if parser
            .expect_ident_cloned()
            .is_ok_and(|ident| ident.eq_ignore_ascii_case("span"))
        {
            let mut saw_integer = consume_grid_integer(&mut parser, true);
            let mut saw_ident = consume_grid_custom_ident(&mut parser);
            if !saw_integer {
                saw_integer = consume_grid_integer(&mut parser, true);
            }
            if !saw_ident {
                saw_ident = consume_grid_custom_ident(&mut parser);
            }
            if (saw_integer || saw_ident) && parser.is_exhausted() {
                return Some(GridLineKind::Other);
            }
        }
        parser.reset(&state);
    }

    // [ <non-zero-integer> && <custom-ident>? ], in either order.
    {
        let state = parser.state();
        if consume_grid_integer(&mut parser, false) {
            let _ = consume_grid_custom_ident(&mut parser);
            if parser.is_exhausted() {
                return Some(GridLineKind::Other);
            }
        }
        parser.reset(&state);
    }
    {
        let state = parser.state();
        if consume_grid_custom_ident(&mut parser) {
            if parser.is_exhausted() {
                return Some(GridLineKind::IdentOnly);
            }
            if consume_grid_integer(&mut parser, false) && parser.is_exhausted() {
                return Some(GridLineKind::Other);
            }
        }
        parser.reset(&state);
    }

    // Modern engines accept an integer-valued math function as the numeric
    // half of a grid line. Keep it non-ident-only so omitted shorthand sides
    // become auto. Resolution remains deferred in the raw placement path.
    if consume_grid_integer_math(&mut parser) {
        let _ = consume_grid_custom_ident(&mut parser);
        if parser.is_exhausted() {
            return Some(GridLineKind::Other);
        }
    }
    None
}

fn consume_grid_integer(parser: &mut cssparser::Parser<'_, '_>, positive_only: bool) -> bool {
    let state = parser.state();
    let valid = parser.expect_integer().is_ok_and(|integer| {
        if positive_only {
            integer > 0
        } else {
            integer != 0
        }
    });
    if !valid {
        parser.reset(&state);
    }
    valid
}

fn consume_grid_custom_ident(parser: &mut cssparser::Parser<'_, '_>) -> bool {
    let state = parser.state();
    let valid = parser.expect_ident_cloned().is_ok_and(|ident| {
        !matches!(
            ident.to_ascii_lowercase().as_str(),
            "auto" | "span" | "initial" | "inherit" | "unset" | "revert" | "revert-layer"
        )
    });
    if !valid {
        parser.reset(&state);
    }
    valid
}

fn consume_grid_integer_math(parser: &mut cssparser::Parser<'_, '_>) -> bool {
    let state = parser.state();
    let is_math = parser.expect_function().is_ok_and(|name| {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "calc" | "min" | "max" | "clamp"
        )
    });
    if !is_math {
        parser.reset(&state);
        return false;
    }
    let valid = parser
        .parse_nested_block::<_, _, ()>(|nested| {
            if nested.is_exhausted() {
                return Err(nested.new_custom_error(()));
            }
            while nested.next_including_whitespace().is_ok() {}
            Ok(())
        })
        .is_ok();
    if !valid {
        parser.reset(&state);
    }
    valid
}

/// Store a `grid-column`/`grid-row` value. Numeric/`span` forms resolve to a
/// `taffy::Line` now; a value that names a grid line (`content-start /
/// content-end`, or the `grid-column: content` area shorthand) is kept raw and
/// resolved against the parent's line-name map in `dom::resolve_grid_areas`.
/// Whichever representation is set, the other is cleared so a later cascade
/// rule of the opposite kind fully overrides it.
fn set_grid_placement(style: &mut LayoutStyle, value: &str, is_col: bool) {
    if grid_line_has_name(value) {
        let raw = Some(value.trim().to_string());
        if is_col {
            style.grid_column_raw = raw;
            style.grid_column = None;
        } else {
            style.grid_row_raw = raw;
            style.grid_row = None;
        }
    } else {
        let line = parse_grid_line(value);
        if is_col {
            style.grid_column = line;
            style.grid_column_raw = None;
        } else {
            style.grid_row = line;
            style.grid_row_raw = None;
        }
    }
}

/// Apply one grid-placement longhand without clearing the opposite side.
/// Responsive grid systems commonly establish a default span with
/// `.layout > * { grid-column-end: span 4 }` and override only the start/end
/// on selected children; dropping these longhands traps every item in one
/// auto-placed track.
fn set_grid_placement_side(style: &mut LayoutStyle, value: &str, is_col: bool, is_start: bool) {
    if grid_line_has_name(value) {
        let raw_slot = if is_col {
            &mut style.grid_column_raw
        } else {
            &mut style.grid_row_raw
        };
        let (mut start, mut end) = match raw_slot.as_deref() {
            Some(raw) if raw.contains('/') => {
                let (start, end) = raw.split_once('/').unwrap();
                (start.trim().to_string(), end.trim().to_string())
            }
            // A single named grid-area shorthand contributes its `-start`
            // and `-end` lines. Preserve both logical sides when a later
            // longhand overrides only one of them (`grid-column:content`
            // followed by `grid-column-end:extended-full-end`, as on MDN).
            // Resolution can still fall back to an exact named line when no
            // area-generated name exists.
            Some(raw) => (raw.trim().to_string(), raw.trim().to_string()),
            None => ("auto".to_string(), "auto".to_string()),
        };
        if is_start {
            start = value.trim().to_string();
        } else {
            end = value.trim().to_string();
        }
        *raw_slot = Some(format!("{start} / {end}"));
        if is_col {
            style.grid_column = None;
        } else {
            style.grid_row = None;
        }
        return;
    }

    let placement = parse_grid_placement(value);
    let line_slot = if is_col {
        style.grid_column_raw = None;
        &mut style.grid_column
    } else {
        style.grid_row_raw = None;
        &mut style.grid_row
    };
    let mut line = line_slot.clone().unwrap_or(taffy::Line {
        start: taffy::GridPlacement::Auto,
        end: taffy::GridPlacement::Auto,
    });
    if is_start {
        line.start = placement;
    } else {
        line.end = placement;
    }
    *line_slot = Some(line);
}

/// True when a `grid-column`/`grid-row` value references a named line (any
/// alphabetic token that is not a bare `span <n>` count), so it must defer to
/// the parent's line-name map.
fn grid_line_has_name(value: &str) -> bool {
    let value = value.trim();
    // `auto` is a grid-placement keyword, not a custom-ident. Sending
    // `auto / span 4` down the named-line path loses the span whenever the
    // parent has no named-line map. CSS-wide keywords likewise cannot name a
    // line and must retain their reset-to-auto behavior in this compact
    // computed-style model.
    if value.eq_ignore_ascii_case("auto")
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "initial" | "inherit" | "unset" | "revert" | "revert-layer"
        )
    {
        return false;
    }
    value.split('/').any(|part| {
        let p = part.trim();
        if p.eq_ignore_ascii_case("auto") {
            return false;
        }
        let lower = p.to_ascii_lowercase();
        let rest = lower.strip_prefix("span").map(str::trim).unwrap_or(&lower);
        rest.chars().any(|c| c.is_ascii_alphabetic())
    })
}

/// Parse `grid-column`/`grid-row` values: `2`, `1 / 3`, `span 2`.
fn parse_grid_line(value: &str) -> Option<taffy::Line<taffy::GridPlacement>> {
    let (a, b) = match value.split_once('/') {
        Some((a, b)) => (a, Some(b)),
        None => (value, None),
    };
    let start = parse_grid_placement(a);
    let end = b
        .map(parse_grid_placement)
        .unwrap_or(taffy::GridPlacement::Auto);
    Some(taffy::Line { start, end })
}

fn parse_grid_placement(value: &str) -> taffy::GridPlacement {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if let Some(span) = lower.strip_prefix("span").map(str::trim) {
        if let Ok(span) = span.parse::<u16>() {
            return taffy::style_helpers::span(span);
        }
    }
    if let Ok(line) = value.parse::<i16>() {
        return taffy::style_helpers::line(line);
    }
    taffy::GridPlacement::Auto
}

/// Parse a CSS color to RGBA. Handles #rgb, #rgba, #rrggbb, #rrggbbaa hex,
/// rgb()/rgba(), `var(--x, fallback)` (uses the fallback), and a set of named
/// colors. Returns None for anything else (transparent).
pub(crate) fn parse_color(value: &str) -> Option<[u8; 4]> {
    parse_color_for_scheme(value, false)
}

fn parse_color_for_scheme(value: &str, dark_scheme: bool) -> Option<[u8; 4]> {
    let raw = value.trim();
    // CSS Color 5 `light-dark(light, dark)` selects by the used color scheme.
    // Obscura currently exposes the default/light scheme, so return the first
    // branch. Both branches still have to be complete valid colors: accepting
    // a valid light arm beside malformed dark syntax would keep a declaration
    // that Chromium rejects at parse time.
    let lower_full = raw.to_ascii_lowercase();
    if lower_full.starts_with("light-dark(") {
        let inner_and_close = &raw["light-dark(".len()..];
        let close = find_matching_paren(inner_and_close)?;
        if !inner_and_close[close + 1..].trim().is_empty() {
            return None;
        }
        let arguments = split_top_level(&inner_and_close[..close], ',');
        if arguments.len() != 2
            || arguments
                .iter()
                .any(|argument| argument.trim().is_empty() || !is_complete_color_token(argument))
        {
            return None;
        }
        let light = parse_color_for_scheme(arguments[0].trim(), dark_scheme)?;
        let dark = parse_color_for_scheme(arguments[1].trim(), dark_scheme)?;
        return Some(if dark_scheme { dark } else { light });
    }
    // CSS custom property with a fallback: var(--name, <fallback>). We cannot
    // resolve the variable, but the fallback after the comma is a real color.
    if let Some(rest) = raw.strip_prefix("var(") {
        let inner = rest.strip_suffix(')').unwrap_or(rest);
        if let Some((_, fallback)) = inner.split_once(',') {
            return parse_color_for_scheme(fallback.trim(), dark_scheme);
        }
        return None;
    }
    // rgb()/rgba() functional notation.
    if let Some(rest) = lower_full
        .strip_prefix("rgb(")
        .or_else(|| lower_full.strip_prefix("rgba("))
    {
        let inner = rest.strip_suffix(')').unwrap_or(rest);
        let parts: Vec<&str> = inner
            .split([',', '/', ' '])
            .filter(|p| !p.trim().is_empty())
            .collect();
        if parts.len() >= 3 {
            let c = |s: &str| -> Option<u8> {
                let s = s.trim();
                if let Some(pct) = s.strip_suffix('%') {
                    pct.parse::<f32>()
                        .ok()
                        .map(|v| (v * 2.55).round().clamp(0.0, 255.0) as u8)
                } else {
                    s.parse::<f32>()
                        .ok()
                        .map(|v| v.round().clamp(0.0, 255.0) as u8)
                }
            };
            let r = c(parts[0])?;
            let g = c(parts[1])?;
            let b = c(parts[2])?;
            let a = parts
                .get(3)
                .and_then(|s| s.trim().parse::<f32>().ok())
                .map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8)
                .unwrap_or(255);
            return Some([r, g, b, a]);
        }
        return None;
    }
    // hsl()/hsla() functional notation.
    if let Some(rest) = lower_full
        .strip_prefix("hsl(")
        .or_else(|| lower_full.strip_prefix("hsla("))
    {
        let inner = rest.strip_suffix(')').unwrap_or(rest);
        let parts: Vec<&str> = inner
            .split([',', '/', ' '])
            .filter(|p| !p.trim().is_empty())
            .collect();
        if parts.len() >= 3 {
            let h = parts[0]
                .trim()
                .trim_end_matches("deg")
                .parse::<f32>()
                .ok()?;
            let s = parts[1].trim().trim_end_matches('%').parse::<f32>().ok()? / 100.0;
            let l = parts[2].trim().trim_end_matches('%').parse::<f32>().ok()? / 100.0;
            let a = parts
                .get(3)
                .and_then(|s| s.trim().trim_end_matches('%').parse::<f32>().ok())
                .map(|v| {
                    if parts[3].contains('%') {
                        (v * 2.55).round()
                    } else {
                        (v * 255.0).round()
                    }
                    .clamp(0.0, 255.0) as u8
                })
                .unwrap_or(255);
            return Some(hsl_to_rgba(h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0), a));
        }
        return None;
    }
    // oklch()/oklab() - Tailwind v4's entire palette. Convert through OKLab to
    // sRGB; without this every modern-framework color resolved to nothing.
    if lower_full.starts_with("oklch(") || lower_full.starts_with("oklab(") {
        let is_lch = lower_full.starts_with("oklch(");
        let inner = &lower_full[if is_lch { 6 } else { 6 }..];
        let inner = inner.strip_suffix(')').unwrap_or(inner);
        let (main, alpha) = match inner.split_once('/') {
            Some((m, a)) => (m, Some(a)),
            None => (inner, None),
        };
        let comps: Vec<&str> = main
            .split([',', ' '])
            .filter(|p| !p.trim().is_empty())
            .collect();
        if comps.len() >= 3 {
            let num = |s: &str| -> Option<f32> {
                let s = s.trim();
                s.strip_suffix('%')
                    .map(|p| p.parse::<f32>().map(|v| v / 100.0))
                    .unwrap_or_else(|| s.parse::<f32>())
                    .ok()
            };
            let l = num(comps[0])?;
            let c = num(comps[1])?;
            let a = alpha
                .and_then(|s| {
                    let s = s.trim();
                    s.strip_suffix('%')
                        .map(|p| p.parse::<f32>().map(|v| v / 100.0))
                        .unwrap_or_else(|| s.parse::<f32>())
                        .ok()
                })
                .unwrap_or(1.0);
            let (oa, ob) = if is_lch {
                let h = comps[2]
                    .trim()
                    .trim_end_matches("deg")
                    .parse::<f32>()
                    .ok()?;
                let hr = h.to_radians();
                (c * hr.cos(), c * hr.sin())
            } else {
                (c, comps[2].trim().parse::<f32>().ok()?)
            };
            return Some(oklab_to_rgba(
                l,
                oa,
                ob,
                (a * 255.0).round().clamp(0.0, 255.0) as u8,
            ));
        }
        return None;
    }
    // color-mix(in <space>, c1 p1%, c2 p2%) - Tailwind v4 uses this pervasively,
    // usually `color-mix(in oklab, <color> N%, transparent)` to apply opacity.
    if lower_full.starts_with("color-mix(") {
        let inner = raw
            [raw.to_ascii_lowercase().find("color-mix(").unwrap() + "color-mix(".len()..]
            .trim_end();
        let inner = inner.strip_suffix(')').unwrap_or(inner);
        let args = split_top_commas(inner);
        if args.len() >= 3 {
            let parse_arg = |s: &str| -> Option<([u8; 4], Option<f32>)> {
                let s = s.trim();
                if let Some(idx) = s.rfind(char::is_whitespace) {
                    let tail = s[idx + 1..].trim();
                    if let Some(p) = tail.strip_suffix('%').and_then(|x| x.parse::<f32>().ok()) {
                        return parse_color_for_scheme(s[..idx].trim(), dark_scheme)
                            .map(|c| (c, Some(p / 100.0)));
                    }
                }
                parse_color_for_scheme(s, dark_scheme).map(|c| (c, None))
            };
            if let (Some((c1, p1)), Some((c2, p2))) = (parse_arg(args[1]), parse_arg(args[2])) {
                let (w1, w2) = match (p1, p2) {
                    (Some(a), Some(b)) => (a, b),
                    (Some(a), None) => (a, 1.0 - a),
                    (None, Some(b)) => (1.0 - b, b),
                    (None, None) => (0.5, 0.5),
                };
                let tot = (w1 + w2).max(1e-6);
                let (w1, w2) = (w1 / tot, w2 / tot);
                // Mixing with a fully transparent color is the opacity idiom:
                // keep the visible color, scale its alpha (not toward black).
                if c2[3] == 0 {
                    return Some([
                        c1[0],
                        c1[1],
                        c1[2],
                        (c1[3] as f32 * w1).round().clamp(0.0, 255.0) as u8,
                    ]);
                }
                if c1[3] == 0 {
                    return Some([
                        c2[0],
                        c2[1],
                        c2[2],
                        (c2[3] as f32 * w2).round().clamp(0.0, 255.0) as u8,
                    ]);
                }
                let m = |i: usize| {
                    (c1[i] as f32 * w1 + c2[i] as f32 * w2)
                        .round()
                        .clamp(0.0, 255.0) as u8
                };
                return Some([m(0), m(1), m(2), m(3)]);
            }
        }
        return None;
    }

    let v = value.split_whitespace().next()?.to_ascii_lowercase();
    if let Some(h) = v.strip_prefix('#') {
        let (r, g, b, a) = match h.len() {
            3 => (
                u8::from_str_radix(&h[0..1].repeat(2), 16).ok()?,
                u8::from_str_radix(&h[1..2].repeat(2), 16).ok()?,
                u8::from_str_radix(&h[2..3].repeat(2), 16).ok()?,
                255u8,
            ),
            4 => (
                u8::from_str_radix(&h[0..1].repeat(2), 16).ok()?,
                u8::from_str_radix(&h[1..2].repeat(2), 16).ok()?,
                u8::from_str_radix(&h[2..3].repeat(2), 16).ok()?,
                u8::from_str_radix(&h[3..4].repeat(2), 16).ok()?,
            ),
            6 => (
                u8::from_str_radix(&h[0..2], 16).ok()?,
                u8::from_str_radix(&h[2..4], 16).ok()?,
                u8::from_str_radix(&h[4..6], 16).ok()?,
                255u8,
            ),
            8 => (
                u8::from_str_radix(&h[0..2], 16).ok()?,
                u8::from_str_radix(&h[2..4], 16).ok()?,
                u8::from_str_radix(&h[4..6], 16).ok()?,
                u8::from_str_radix(&h[6..8], 16).ok()?,
            ),
            _ => return None,
        };
        return Some([r, g, b, a]);
    }
    match v.as_str() {
        "white" => Some([255, 255, 255, 255]),
        "black" => Some([0, 0, 0, 255]),
        "gray" | "grey" => Some([128, 128, 128, 255]),
        "silver" => Some([192, 192, 192, 255]),
        "lightgray" | "lightgrey" => Some([211, 211, 211, 255]),
        "darkgray" | "darkgrey" => Some([169, 169, 169, 255]),
        "whitesmoke" => Some([245, 245, 245, 255]),
        "gainsboro" => Some([220, 220, 220, 255]),
        "red" => Some([255, 0, 0, 255]),
        "green" => Some([0, 128, 0, 255]),
        "lime" => Some([0, 255, 0, 255]),
        "blue" => Some([0, 0, 255, 255]),
        "navy" => Some([0, 0, 128, 255]),
        "yellow" => Some([255, 255, 0, 255]),
        "orange" => Some([255, 165, 0, 255]),
        "purple" => Some([128, 0, 128, 255]),
        "maroon" => Some([128, 0, 0, 255]),
        "teal" => Some([0, 128, 128, 255]),
        "aqua" | "cyan" => Some([0, 255, 255, 255]),
        "fuchsia" | "magenta" => Some([255, 0, 255, 255]),
        "olive" => Some([128, 128, 0, 255]),
        "transparent" => Some([0, 0, 0, 0]),
        _ => named_color(&v),
    }
}

fn resolve_svg_presentation_color(value: &str, dark_scheme: bool) -> String {
    let raw = value.trim();
    if raw.to_ascii_lowercase().contains("light-dark(") {
        if let Some([red, green, blue, alpha]) = parse_color_for_scheme(raw, dark_scheme) {
            return format!("#{red:02x}{green:02x}{blue:02x}{alpha:02x}");
        }
    }
    raw.to_string()
}

/// The remaining common CSS named colors (the hot ones from real sites) beyond
/// the handful spelled out above.
fn named_color(v: &str) -> Option<[u8; 4]> {
    let rgb = match v {
        "darkblue" => [0, 0, 139],
        "mediumblue" => [0, 0, 205],
        "royalblue" => [65, 105, 225],
        "dodgerblue" => [30, 144, 255],
        "cornflowerblue" => [100, 149, 237],
        "steelblue" => [70, 130, 180],
        "deepskyblue" => [0, 191, 255],
        "skyblue" => [135, 206, 235],
        "lightskyblue" => [135, 206, 250],
        "lightblue" => [173, 216, 230],
        "powderblue" => [176, 224, 230],
        "cadetblue" => [95, 158, 160],
        "slateblue" => [106, 90, 205],
        "darkslateblue" => [72, 61, 139],
        "midnightblue" => [25, 25, 112],
        "indigo" => [75, 0, 130],
        "darkgreen" => [0, 100, 0],
        "forestgreen" => [34, 139, 34],
        "seagreen" => [46, 139, 87],
        "mediumseagreen" => [60, 179, 113],
        "limegreen" => [50, 205, 50],
        "yellowgreen" => [154, 205, 50],
        "olivedrab" => [107, 142, 35],
        "darkolivegreen" => [85, 107, 47],
        "greenyellow" => [173, 255, 47],
        "lightgreen" => [144, 238, 144],
        "palegreen" => [152, 251, 152],
        "springgreen" => [0, 255, 127],
        "mediumaquamarine" => [102, 205, 170],
        "aquamarine" => [127, 255, 212],
        "turquoise" => [64, 224, 208],
        "mediumturquoise" => [72, 209, 204],
        "darkcyan" => [0, 139, 139],
        "crimson" => [220, 20, 60],
        "firebrick" => [178, 34, 34],
        "darkred" => [139, 0, 0],
        "indianred" => [205, 92, 92],
        "tomato" => [255, 99, 71],
        "orangered" => [255, 69, 0],
        "coral" => [255, 127, 80],
        "salmon" => [250, 128, 114],
        "lightsalmon" => [255, 160, 122],
        "darksalmon" => [233, 150, 122],
        "hotpink" => [255, 105, 180],
        "deeppink" => [255, 20, 147],
        "pink" => [255, 192, 203],
        "lightpink" => [255, 182, 193],
        "palevioletred" => [219, 112, 147],
        "mediumvioletred" => [199, 21, 133],
        "violet" => [238, 130, 238],
        "orchid" => [218, 112, 214],
        "plum" => [221, 160, 221],
        "mediumpurple" => [147, 112, 219],
        "blueviolet" => [138, 43, 226],
        "darkviolet" => [148, 0, 211],
        "darkorchid" => [153, 50, 204],
        "darkmagenta" => [139, 0, 139],
        "lavender" => [230, 230, 250],
        "thistle" => [216, 191, 216],
        "gold" => [255, 215, 0],
        "goldenrod" => [218, 165, 32],
        "darkgoldenrod" => [184, 134, 11],
        "khaki" => [240, 230, 140],
        "darkkhaki" => [189, 183, 107],
        "peachpuff" => [255, 218, 185],
        "moccasin" => [255, 228, 181],
        "papayawhip" => [255, 239, 213],
        "wheat" => [245, 222, 179],
        "tan" => [210, 180, 140],
        "burlywood" => [222, 184, 135],
        "sandybrown" => [244, 164, 96],
        "peru" => [205, 133, 63],
        "chocolate" => [210, 105, 30],
        "sienna" => [160, 82, 45],
        "saddlebrown" => [139, 69, 19],
        "brown" => [165, 42, 42],
        "rosybrown" => [188, 143, 143],
        "darkorange" => [255, 140, 0],
        "lightyellow" => [255, 255, 224],
        "lightgoldenrodyellow" => [250, 250, 210],
        "lemonchiffon" => [255, 250, 205],
        "beige" => [245, 245, 220],
        "ivory" => [255, 255, 240],
        "azure" => [240, 255, 255],
        "mintcream" => [245, 255, 250],
        "honeydew" => [240, 255, 240],
        "snow" => [255, 250, 250],
        "seashell" => [255, 245, 238],
        "linen" => [250, 240, 230],
        "oldlace" => [253, 245, 230],
        "floralwhite" => [255, 250, 240],
        "ghostwhite" => [248, 248, 255],
        "aliceblue" => [240, 248, 255],
        "lavenderblush" => [255, 240, 245],
        "mistyrose" => [255, 228, 225],
        "cornsilk" => [255, 248, 220],
        "antiquewhite" => [250, 235, 215],
        "bisque" => [255, 228, 196],
        "blanchedalmond" => [255, 235, 205],
        "navajowhite" => [255, 222, 173],
        "dimgray" | "dimgrey" => [105, 105, 105],
        "slategray" | "slategrey" => [112, 128, 144],
        "lightslategray" | "lightslategrey" => [119, 136, 153],
        "darkslategray" | "darkslategrey" => [47, 79, 79],
        _ => return None,
    };
    Some([rgb[0], rgb[1], rgb[2], 255])
}

/// Reject trailing tokens and unbalanced functions before a `light-dark()`
/// branch reaches the intentionally permissive legacy color parser. Nested
/// color functions and their internal whitespace remain valid.
fn is_complete_color_token(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    let mut depth = 0i32;
    let mut first_open = None;
    let mut outer_close = None;
    for (index, character) in value.char_indices() {
        match character {
            '(' => {
                if depth == 0 && first_open.is_none() {
                    first_open = Some(index);
                }
                depth += 1;
            }
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
                if depth == 0 {
                    outer_close = Some(index);
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return false;
    }
    match first_open {
        Some(open) => {
            open > 0
                && value[..open]
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
                && outer_close == Some(value.len() - 1)
        }
        None => !value.chars().any(char::is_whitespace),
    }
}

/// Convert `hsl()`/`hsla()` to RGBA. `h` in degrees, `s`/`l` as 0-1.
/// Convert an OKLab color (L in 0..1, a/b unbounded) to sRGB rgba bytes.
/// (oklch is converted to oklab by the caller.) Standard Björn Ottosson matrix.
fn oklab_to_rgba(l: f32, a: f32, b: f32, alpha: u8) -> [u8; 4] {
    let l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = l - 0.0894841775 * a - 1.2914855480 * b;
    let (lc, mc, sc) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let lr = 4.0767416621 * lc - 3.3077115913 * mc + 0.2309699292 * sc;
    let lg = -1.2684380046 * lc + 2.6097574011 * mc - 0.3413193965 * sc;
    let lb = -0.0041960863 * lc - 0.7034186147 * mc + 1.7076147010 * sc;
    let enc = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        let s = if x <= 0.0031308 {
            12.92 * x
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0).round().clamp(0.0, 255.0) as u8
    };
    [enc(lr), enc(lg), enc(lb), alpha]
}

/// Split on top-level commas, respecting nested `()` (so a `color-mix` argument
/// like `oklch(0.7 0.1 20)` or `var(--x, y)` is not shattered).
fn split_top_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn hsl_to_rgba(h: f32, s: f32, l: f32, a: u8) -> [u8; 4] {
    let h = ((h % 360.0) + 360.0) % 360.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (((h / 60.0) % 2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [
        (((r + m) * 255.0).round().clamp(0.0, 255.0)) as u8,
        (((g + m) * 255.0).round().clamp(0.0, 255.0)) as u8,
        (((b + m) * 255.0).round().clamp(0.0, 255.0)) as u8,
        a,
    ]
}

#[derive(Clone, Copy)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

fn border_style(value: &str) -> Option<crate::BorderStyle> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(crate::BorderStyle::None),
        "hidden" => Some(crate::BorderStyle::Hidden),
        "dotted" => Some(crate::BorderStyle::Dotted),
        "dashed" => Some(crate::BorderStyle::Dashed),
        "solid" => Some(crate::BorderStyle::Solid),
        "double" => Some(crate::BorderStyle::Double),
        "groove" => Some(crate::BorderStyle::Groove),
        "ridge" => Some(crate::BorderStyle::Ridge),
        "inset" => Some(crate::BorderStyle::Inset),
        "outset" => Some(crate::BorderStyle::Outset),
        _ => None,
    }
}

fn outline_style(value: &str) -> Option<crate::BorderStyle> {
    if value.trim().eq_ignore_ascii_case("auto") {
        Some(crate::BorderStyle::Auto)
    } else {
        border_style(value).filter(|style| *style != crate::BorderStyle::Hidden)
    }
}

fn border_width(value: &str) -> Option<f32> {
    let value = value.trim();
    let width = match value.to_ascii_lowercase().as_str() {
        "thin" => 1.0,
        "medium" => crate::border::MEDIUM_BORDER_WIDTH,
        "thick" => 5.0,
        _ => strict_border_length(value)?,
    };
    width
        .is_finite()
        .then_some(width)
        .filter(|width| *width >= 0.0)
}

fn strict_border_length(value: &str) -> Option<f32> {
    let value = value.trim();
    if value == "0" || value == "+0" || value == "-0" {
        return Some(0.0);
    }
    let lower = value.to_ascii_lowercase();
    if lower.contains('(') {
        if lower.contains('%') {
            return None;
        }
        return px(value);
    }
    const UNITS: [&str; 5] = ["rem", "px", "pt", "em", "ex"];
    let unit = UNITS.iter().find(|unit| lower.ends_with(**unit))?;
    let number = lower[..lower.len() - unit.len()]
        .trim()
        .parse::<f32>()
        .ok()?;
    number.is_finite().then(|| px(value)).flatten()
}

fn border_color(value: &str, dark_scheme: bool) -> Option<Option<[u8; 4]>> {
    if value.trim().eq_ignore_ascii_case("currentcolor") {
        Some(None)
    } else {
        parse_color_for_scheme(value, dark_scheme).map(Some)
    }
}

fn sync_used_border(style: &mut LayoutStyle) {
    let used = style.border_model.used_widths();
    style.border = Edges {
        top: used.top,
        right: used.right,
        bottom: used.bottom,
        left: used.left,
    };
}

fn side_widths_mut(model: &mut crate::BorderModel, side: Side) -> &mut f32 {
    match side {
        Side::Top => &mut model.specified_widths.top,
        Side::Right => &mut model.specified_widths.right,
        Side::Bottom => &mut model.specified_widths.bottom,
        Side::Left => &mut model.specified_widths.left,
    }
}

fn side_styles_mut(model: &mut crate::BorderModel, side: Side) -> &mut crate::BorderStyle {
    match side {
        Side::Top => &mut model.styles.top,
        Side::Right => &mut model.styles.right,
        Side::Bottom => &mut model.styles.bottom,
        Side::Left => &mut model.styles.left,
    }
}

fn side_colors_mut(model: &mut crate::BorderModel, side: Side) -> &mut Option<[u8; 4]> {
    match side {
        Side::Top => &mut model.colors.top,
        Side::Right => &mut model.colors.right,
        Side::Bottom => &mut model.colors.bottom,
        Side::Left => &mut model.colors.left,
    }
}

fn for_each_border_cascade_side(
    side: crate::BorderCascadeSide,
    direction: taffy::Direction,
    mut apply: impl FnMut(Side),
) {
    use crate::BorderCascadeSide as Logical;
    match side {
        Logical::Top => apply(Side::Top),
        Logical::Right => apply(Side::Right),
        Logical::Bottom => apply(Side::Bottom),
        Logical::Left => apply(Side::Left),
        Logical::InlineStart => apply(if direction == taffy::Direction::Rtl {
            Side::Right
        } else {
            Side::Left
        }),
        Logical::InlineEnd => apply(if direction == taffy::Direction::Rtl {
            Side::Left
        } else {
            Side::Right
        }),
        Logical::BlockStart => apply(Side::Top),
        Logical::BlockEnd => apply(Side::Bottom),
        Logical::Inline => {
            apply(Side::Left);
            apply(Side::Right);
        }
        Logical::Block => {
            apply(Side::Top);
            apply(Side::Bottom);
        }
        Logical::All => {
            apply(Side::Top);
            apply(Side::Right);
            apply(Side::Bottom);
            apply(Side::Left);
        }
    }
}

fn record_border_cascade_op(style: &mut LayoutStyle, op: crate::BorderCascadeOp) {
    style.border_cascade_ops.push(op);
}

fn record_physical_border_component(
    style: &mut LayoutStyle,
    side: crate::BorderCascadeSide,
    width: Option<f32>,
    line_style: Option<crate::BorderStyle>,
    color: Option<Option<[u8; 4]>>,
) {
    // The existing physical model is already final until a logical property
    // appears. Avoid per-element logs on the overwhelmingly common
    // physical-only path; once logical replay is active, later physical
    // aliases must join the ordered stream.
    if style.border_cascade_base.is_none() {
        return;
    }
    record_border_cascade_op(
        style,
        crate::BorderCascadeOp {
            side,
            width,
            style: line_style,
            color,
        },
    );
}

fn record_logical_border_component(
    style: &mut LayoutStyle,
    side: crate::BorderCascadeSide,
    width: Option<f32>,
    line_style: Option<crate::BorderStyle>,
    color: Option<Option<[u8; 4]>>,
) {
    if style.border_cascade_base.is_none() {
        style.border_cascade_base = Some(style.border_model);
    }
    record_border_cascade_op(
        style,
        crate::BorderCascadeOp {
            side,
            width,
            style: line_style,
            color,
        },
    );
}

fn physical_border_cascade_side(side: Side) -> crate::BorderCascadeSide {
    match side {
        Side::Top => crate::BorderCascadeSide::Top,
        Side::Right => crate::BorderCascadeSide::Right,
        Side::Bottom => crate::BorderCascadeSide::Bottom,
        Side::Left => crate::BorderCascadeSide::Left,
    }
}

/// Replay physical and logical border declarations after inherited direction
/// is final. Keeping the original cascade sequence makes a later physical
/// declaration beat an earlier logical alias (and vice versa), independent of
/// whether `direction` appeared before or after either declaration.
pub(crate) fn resolve_logical_borders(style: &mut LayoutStyle) {
    let Some(mut model) = style.border_cascade_base else {
        return;
    };
    // Border radius is an independent family and may have cascaded after the
    // base snapshot was taken.
    model.radii = style.border_model.radii;
    let direction = style.direction.unwrap_or(taffy::Direction::Ltr);
    for op in &style.border_cascade_ops {
        for_each_border_cascade_side(op.side, direction, |side| {
            if let Some(width) = op.width {
                *side_widths_mut(&mut model, side) = width;
            }
            if let Some(line_style) = op.style {
                *side_styles_mut(&mut model, side) = line_style;
            }
            if let Some(color) = op.color {
                *side_colors_mut(&mut model, side) = color;
            }
        });
    }
    style.border_model = model;
    let colors = model.colors;
    style.border_color = (colors.top == colors.right
        && colors.right == colors.bottom
        && colors.bottom == colors.left)
        .then_some(colors.top)
        .flatten();
    sync_used_border(style);
}

fn apply_border_widths(style: &mut LayoutStyle, value: &str) {
    let values = if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        crate::Sides::all(crate::border::MEDIUM_BORDER_WIDTH)
    } else {
        let tokens = split_ws_paren(value);
        let Some(values) = tokens
            .iter()
            .map(|token| border_width(token))
            .collect::<Option<Vec<_>>>()
            .and_then(|values| crate::border::expand_sides(&values))
        else {
            return;
        };
        values
    };
    for (side, width) in [
        (crate::BorderCascadeSide::Top, values.top),
        (crate::BorderCascadeSide::Right, values.right),
        (crate::BorderCascadeSide::Bottom, values.bottom),
        (crate::BorderCascadeSide::Left, values.left),
    ] {
        record_physical_border_component(style, side, Some(width), None, None);
    }
    style.border_model.specified_widths = values;
    sync_used_border(style);
}

fn set_border_width(style: &mut LayoutStyle, side: Side, value: &str) {
    let width = if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        Some(crate::border::MEDIUM_BORDER_WIDTH)
    } else {
        border_width(value)
    };
    if let Some(width) = width {
        record_physical_border_component(
            style,
            physical_border_cascade_side(side),
            Some(width),
            None,
            None,
        );
        *side_widths_mut(&mut style.border_model, side) = width;
        sync_used_border(style);
    }
}

fn apply_border_styles(style: &mut LayoutStyle, value: &str) {
    let values = if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        crate::Sides::all(crate::BorderStyle::None)
    } else {
        let tokens = split_ws_paren(value);
        let Some(values) = tokens
            .iter()
            .map(|token| border_style(token))
            .collect::<Option<Vec<_>>>()
            .and_then(|values| crate::border::expand_sides(&values))
        else {
            return;
        };
        values
    };
    for (side, line_style) in [
        (crate::BorderCascadeSide::Top, values.top),
        (crate::BorderCascadeSide::Right, values.right),
        (crate::BorderCascadeSide::Bottom, values.bottom),
        (crate::BorderCascadeSide::Left, values.left),
    ] {
        record_physical_border_component(style, side, None, Some(line_style), None);
    }
    style.border_model.styles = values;
    sync_used_border(style);
}

fn set_border_style(style: &mut LayoutStyle, side: Side, value: &str) {
    let line_style = if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        Some(crate::BorderStyle::None)
    } else {
        border_style(value)
    };
    if let Some(line_style) = line_style {
        record_physical_border_component(
            style,
            physical_border_cascade_side(side),
            None,
            Some(line_style),
            None,
        );
        *side_styles_mut(&mut style.border_model, side) = line_style;
        sync_used_border(style);
    }
}

fn parse_border_colors(value: &str, dark_scheme: bool) -> Option<crate::Sides<Option<[u8; 4]>>> {
    let tokens = split_ws_paren(value);
    let values = tokens
        .iter()
        .map(|token| border_color(token, dark_scheme))
        .collect::<Option<Vec<_>>>()?;
    crate::border::expand_sides(&values)
}

fn apply_border_colors(style: &mut LayoutStyle, value: &str) {
    let colors = if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        crate::Sides::all(None)
    } else if let Some(colors) = parse_border_colors(value, style.color_scheme_dark) {
        colors
    } else {
        return;
    };
    for (side, color) in [
        (crate::BorderCascadeSide::Top, colors.top),
        (crate::BorderCascadeSide::Right, colors.right),
        (crate::BorderCascadeSide::Bottom, colors.bottom),
        (crate::BorderCascadeSide::Left, colors.left),
    ] {
        record_physical_border_component(style, side, None, None, Some(color));
    }
    style.border_model.colors = colors;
    style.border_color = (colors.top == colors.right
        && colors.right == colors.bottom
        && colors.bottom == colors.left)
        .then_some(colors.top)
        .flatten();
}

fn set_border_color(style: &mut LayoutStyle, side: Side, value: &str) {
    let color = if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        Some(None)
    } else {
        border_color(value, style.color_scheme_dark)
    };
    if let Some(color) = color {
        record_physical_border_component(
            style,
            physical_border_cascade_side(side),
            None,
            None,
            Some(color),
        );
        *side_colors_mut(&mut style.border_model, side) = color;
        let colors = style.border_model.colors;
        style.border_color = (colors.top == colors.right
            && colors.right == colors.bottom
            && colors.bottom == colors.left)
            .then_some(colors.top)
            .flatten();
    }
}

#[derive(Clone, Copy)]
struct BorderShorthand {
    width: f32,
    style: crate::BorderStyle,
    color: Option<[u8; 4]>,
}

fn parse_border_shorthand(value: &str, dark_scheme: bool) -> Option<BorderShorthand> {
    let lower = value.trim().to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        return Some(BorderShorthand {
            width: crate::border::MEDIUM_BORDER_WIDTH,
            style: crate::BorderStyle::None,
            color: None,
        });
    }
    if value.trim().is_empty() || lower == "inherit" {
        return None;
    }
    let mut width = None;
    let mut style = None;
    let mut color = None;
    let mut saw_color = false;
    for token in split_ws_paren(value) {
        if width.is_none() {
            if let Some(parsed) = border_width(token) {
                width = Some(parsed);
                continue;
            }
        }
        if style.is_none() {
            if let Some(parsed) = border_style(token) {
                style = Some(parsed);
                continue;
            }
        }
        if !saw_color {
            if let Some(parsed) = border_color(token, dark_scheme) {
                color = parsed;
                saw_color = true;
                continue;
            }
        }
        return None;
    }
    Some(BorderShorthand {
        width: width.unwrap_or(crate::border::MEDIUM_BORDER_WIDTH),
        style: style.unwrap_or(crate::BorderStyle::None),
        color,
    })
}

fn logical_border_pair<T: Copy>(
    value: &str,
    initial: T,
    parse: impl Fn(&str) -> Option<T>,
) -> Option<(T, T)> {
    if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        return Some((initial, initial));
    }
    let tokens = split_ws_paren(value);
    match tokens.as_slice() {
        [one] => parse(one).map(|value| (value, value)),
        [start, end] => Some((parse(start)?, parse(end)?)),
        _ => None,
    }
}

fn logical_border_single<T: Copy>(
    value: &str,
    initial: T,
    parse: impl Fn(&str) -> Option<T>,
) -> Option<T> {
    if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        return Some(initial);
    }
    let tokens = split_ws_paren(value);
    matches!(tokens.as_slice(), [one] if parse(one).is_some()).then(|| parse(tokens[0]).unwrap())
}

fn logical_border_sides(name: &str) -> Option<(crate::BorderCascadeSide, crate::BorderCascadeSide)> {
    match name {
        "border-inline-width" | "border-inline-style" | "border-inline-color" => Some((
            crate::BorderCascadeSide::InlineStart,
            crate::BorderCascadeSide::InlineEnd,
        )),
        "border-block-width" | "border-block-style" | "border-block-color" => Some((
            crate::BorderCascadeSide::BlockStart,
            crate::BorderCascadeSide::BlockEnd,
        )),
        _ => None,
    }
}

fn logical_border_single_side(name: &str) -> Option<crate::BorderCascadeSide> {
    if name.starts_with("border-inline-start") {
        Some(crate::BorderCascadeSide::InlineStart)
    } else if name.starts_with("border-inline-end") {
        Some(crate::BorderCascadeSide::InlineEnd)
    } else if name.starts_with("border-block-start") {
        Some(crate::BorderCascadeSide::BlockStart)
    } else if name.starts_with("border-block-end") {
        Some(crate::BorderCascadeSide::BlockEnd)
    } else {
        None
    }
}

fn apply_logical_border(style: &mut LayoutStyle, name: &str, value: &str) {
    let shorthand_side = match name {
        "border-inline" => Some(crate::BorderCascadeSide::Inline),
        "border-block" => Some(crate::BorderCascadeSide::Block),
        "border-inline-start" | "border-inline-end" | "border-block-start"
        | "border-block-end" => logical_border_single_side(name),
        _ => None,
    };
    if let Some(side) = shorthand_side {
        let Some(parsed) = parse_border_shorthand(value, style.color_scheme_dark) else {
            return;
        };
        record_logical_border_component(
            style,
            side,
            Some(parsed.width),
            Some(parsed.style),
            Some(parsed.color),
        );
        resolve_logical_borders(style);
        return;
    }

    let pair_sides = logical_border_sides(name);
    let single_side = logical_border_single_side(name);
    if name.ends_with("-width") {
        let (start, end) = if pair_sides.is_some() {
            let Some(values) =
                logical_border_pair(value, crate::border::MEDIUM_BORDER_WIDTH, border_width)
            else {
                return;
            };
            values
        } else {
            let Some(value) =
                logical_border_single(value, crate::border::MEDIUM_BORDER_WIDTH, border_width)
            else {
                return;
            };
            (value, value)
        };
        if let Some((start_side, end_side)) = pair_sides {
            record_logical_border_component(style, start_side, Some(start), None, None);
            record_logical_border_component(style, end_side, Some(end), None, None);
        } else if let Some(side) = single_side {
            record_logical_border_component(style, side, Some(start), None, None);
        }
    } else if name.ends_with("-style") {
        let (start, end) = if pair_sides.is_some() {
            let Some(values) = logical_border_pair(value, crate::BorderStyle::None, border_style)
            else {
                return;
            };
            values
        } else {
            let Some(value) = logical_border_single(value, crate::BorderStyle::None, border_style)
            else {
                return;
            };
            (value, value)
        };
        if let Some((start_side, end_side)) = pair_sides {
            record_logical_border_component(style, start_side, None, Some(start), None);
            record_logical_border_component(style, end_side, None, Some(end), None);
        } else if let Some(side) = single_side {
            record_logical_border_component(style, side, None, Some(start), None);
        }
    } else if name.ends_with("-color") {
        let dark_scheme = style.color_scheme_dark;
        let (start, end) = if pair_sides.is_some() {
            let Some(values) =
                logical_border_pair(value, None, |token| border_color(token, dark_scheme))
            else {
                return;
            };
            values
        } else {
            let Some(value) =
                logical_border_single(value, None, |token| border_color(token, dark_scheme))
            else {
                return;
            };
            (value, value)
        };
        if let Some((start_side, end_side)) = pair_sides {
            record_logical_border_component(style, start_side, None, None, Some(start));
            record_logical_border_component(style, end_side, None, None, Some(end));
        } else if let Some(side) = single_side {
            record_logical_border_component(style, side, None, None, Some(start));
        }
    } else {
        return;
    }
    resolve_logical_borders(style);
}

fn supports_logical_border_declaration(name: &str, value: &str) -> bool {
    if matches!(
        name,
        "border-inline"
            | "border-block"
            | "border-inline-start"
            | "border-inline-end"
            | "border-block-start"
            | "border-block-end"
    ) {
        return parse_border_shorthand(value, false).is_some();
    }
    let is_pair = matches!(
        name,
        "border-inline-width"
            | "border-inline-style"
            | "border-inline-color"
            | "border-block-width"
            | "border-block-style"
            | "border-block-color"
    );
    let tokens = split_ws_paren(value);
    if tokens.is_empty() || tokens.len() > if is_pair { 2 } else { 1 } {
        return false;
    }
    if name.ends_with("-width") {
        tokens.iter().all(|token| border_width(token).is_some())
    } else if name.ends_with("-style") {
        tokens.iter().all(|token| border_style(token).is_some())
    } else if name.ends_with("-color") {
        tokens
            .iter()
            .all(|token| border_color(token, false).is_some())
    } else {
        false
    }
}

fn apply_border_shorthand(style: &mut LayoutStyle, side: Option<Side>, value: &str) {
    let Some(parsed) = parse_border_shorthand(value, style.color_scheme_dark) else {
        return;
    };
    record_physical_border_component(
        style,
        side.map_or(crate::BorderCascadeSide::All, physical_border_cascade_side),
        Some(parsed.width),
        Some(parsed.style),
        Some(parsed.color),
    );
    if let Some(side) = side {
        *side_widths_mut(&mut style.border_model, side) = parsed.width;
        *side_styles_mut(&mut style.border_model, side) = parsed.style;
        *side_colors_mut(&mut style.border_model, side) = parsed.color;
    } else {
        style.border_model.specified_widths = crate::Sides::all(parsed.width);
        style.border_model.styles = crate::Sides::all(parsed.style);
        style.border_model.colors = crate::Sides::all(parsed.color);
        style.border_color = parsed.color;
    }
    let colors = style.border_model.colors;
    style.border_color = (colors.top == colors.right
        && colors.right == colors.bottom
        && colors.bottom == colors.left)
        .then_some(colors.top)
        .flatten();
    sync_used_border(style);
}

fn radius_value(value: &str) -> Option<crate::RadiusValue> {
    let value = value.trim();
    if let Some(number) = value.strip_suffix('%') {
        let percentage = number.trim().parse::<f32>().ok()? / 100.0;
        return (percentage.is_finite() && percentage >= 0.0)
            .then_some(crate::RadiusValue::percentage(percentage));
    }
    let length = strict_border_length(value)?;
    (length.is_finite() && length >= 0.0).then_some(crate::RadiusValue::pixels(length))
}

fn parsed_border_radii(value: &str) -> Option<crate::BorderRadii> {
    let axes = split_top_level(value, '/');
    if axes.is_empty() || axes.len() > 2 {
        return None;
    }
    let horizontal_tokens = split_ws_paren(axes[0]);
    let horizontal = horizontal_tokens
        .iter()
        .map(|token| radius_value(token))
        .collect::<Option<Vec<_>>>()
        .and_then(|values| crate::border::expand_sides(&values))?;
    let vertical = if axes.len() == 2 {
        let vertical_tokens = split_ws_paren(axes[1]);
        vertical_tokens
            .iter()
            .map(|token| radius_value(token))
            .collect::<Option<Vec<_>>>()
            .and_then(|values| crate::border::expand_sides(&values))?
    } else {
        horizontal
    };
    Some(crate::BorderRadii {
        top_left: crate::CornerRadius {
            x: horizontal.top,
            y: vertical.top,
        },
        top_right: crate::CornerRadius {
            x: horizontal.right,
            y: vertical.right,
        },
        bottom_right: crate::CornerRadius {
            x: horizontal.bottom,
            y: vertical.bottom,
        },
        bottom_left: crate::CornerRadius {
            x: horizontal.left,
            y: vertical.left,
        },
    })
}

fn apply_border_radius_shorthand(style: &mut LayoutStyle, value: &str) {
    if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        style.border_model.radii = crate::BorderRadii::default();
    } else if let Some(radii) = parsed_border_radii(value) {
        style.border_model.radii = radii;
    }
}

fn set_corner_radius(style: &mut LayoutStyle, corner: usize, value: &str) {
    let parsed = if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        Some(crate::CornerRadius::default())
    } else {
        let tokens = split_ws_paren(value);
        match tokens.as_slice() {
            [x] => radius_value(x).map(crate::CornerRadius::circular),
            [x, y] => radius_value(x)
                .zip(radius_value(y))
                .map(|(x, y)| crate::CornerRadius { x, y }),
            _ => None,
        }
    };
    let Some(parsed) = parsed else { return };
    match corner {
        0 => style.border_model.radii.top_left = parsed,
        1 => style.border_model.radii.top_right = parsed,
        2 => style.border_model.radii.bottom_right = parsed,
        3 => style.border_model.radii.bottom_left = parsed,
        _ => {}
    }
}

fn parse_outline_shorthand(value: &str, dark_scheme: bool) -> Option<crate::OutlineModel> {
    let lower = value.trim().to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        return Some(crate::OutlineModel::default());
    }
    if value.trim().is_empty() || lower == "inherit" {
        return None;
    }
    let mut outline = crate::OutlineModel::default();
    let mut saw_width = false;
    let mut saw_style = false;
    let mut saw_color = false;
    for token in split_ws_paren(value) {
        if !saw_width {
            if let Some(width) = border_width(token) {
                outline.specified_width = width;
                saw_width = true;
                continue;
            }
        }
        if !saw_style {
            if let Some(style) = outline_style(token) {
                outline.style = style;
                saw_style = true;
                continue;
            }
        }
        if !saw_color {
            if let Some(color) = border_color(token, dark_scheme) {
                outline.color = color;
                saw_color = true;
                continue;
            }
        }
        return None;
    }
    Some(outline)
}

fn apply_outline_shorthand(style: &mut LayoutStyle, value: &str) {
    if let Some(outline) = parse_outline_shorthand(value, style.color_scheme_dark) {
        style.outline = outline;
    }
}

fn set_outline_width(style: &mut LayoutStyle, value: &str) {
    let width = if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        Some(crate::border::MEDIUM_BORDER_WIDTH)
    } else {
        border_width(value)
    };
    if let Some(width) = width {
        style.outline.specified_width = width;
    }
}

fn set_outline_style(style: &mut LayoutStyle, value: &str) {
    let line_style = if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        Some(crate::BorderStyle::None)
    } else {
        outline_style(value)
    };
    if let Some(line_style) = line_style {
        style.outline.style = line_style;
    }
}

fn set_outline_color(style: &mut LayoutStyle, value: &str) {
    let color = if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        Some(None)
    } else {
        border_color(value, style.color_scheme_dark)
    };
    if let Some(color) = color {
        style.outline.color = color;
    }
}

fn set_outline_offset(style: &mut LayoutStyle, value: &str) {
    let offset = if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        Some(0.0)
    } else {
        strict_border_length(value).filter(|value| value.is_finite())
    };
    if let Some(offset) = offset {
        style.outline.offset = offset;
    }
}

/// Parse the first length in a value as CSS pixels. `auto` and non-numeric
/// values return None (treated as "no explicit size" in phase 1). Delegates
/// to `resolve_length` for anything beyond a bare token, since `calc()`,
/// `var()`, and `min()`/`max()` all contain spaces that would otherwise break
/// the single-token fast path.
fn px(value: &str) -> Option<f32> {
    let trimmed = value.trim();
    if trimmed.contains('(') {
        return resolve_length(trimmed);
    }
    token(value).and_then(px_value)
}

fn deferred_length_expression(value: &str) -> Option<String> {
    value.trim().contains('(').then(|| value.trim().to_string())
}

/// Resolve functional CSS lengths once the actual layout context is known.
/// `vw`/`vh` are one-percent viewport units and `percent_base` is the relevant
/// containing-block dimension. Custom properties have already been substituted
/// by the cascade before this function sees the expression.
pub(crate) fn resolve_contextual_length(
    value: &str,
    em_px: f32,
    rem_px: f32,
    vw: f32,
    vh: f32,
    percent_base: f32,
) -> Option<f32> {
    let context = LengthContext {
        em_px,
        rem_px,
        vw,
        vh,
        percent_base,
    };
    resolve_contextual(value, &context)
}

/// Return the percentage factor of a functional length whose value is purely
/// proportional to its percentage basis.
///
/// CSSOM preserves spellings such as `calc(100%)`, but sizing algorithms must
/// treat them like the typed `100%` value. Sampling at zero and two positive
/// bases rejects offsets and non-linear min/max/clamp expressions while still
/// accepting algebraically equivalent percentage-only forms.
pub(crate) fn functional_percentage_factor(value: &str) -> Option<f32> {
    if !value.contains('%') || !value.contains('(') {
        return None;
    }
    let at_zero = resolve_contextual_length(value, 16.0, 16.0, 1.0, 1.0, 0.0)?;
    let at_hundred = resolve_contextual_length(value, 16.0, 16.0, 1.0, 1.0, 100.0)?;
    let at_two_hundred =
        resolve_contextual_length(value, 16.0, 16.0, 1.0, 1.0, 200.0)?;
    let tolerance = 0.0001;
    if at_zero.abs() > tolerance
        || (at_two_hundred - at_hundred * 2.0).abs() > tolerance
        || !at_hundred.is_finite()
    {
        return None;
    }
    Some(at_hundred / 100.0)
}

#[derive(Clone, Copy)]
struct LengthContext {
    em_px: f32,
    rem_px: f32,
    vw: f32,
    vh: f32,
    percent_base: f32,
}

fn resolve_contextual(value: &str, context: &LengthContext) -> Option<f32> {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('(') {
        let end = find_matching_paren(rest)?;
        if end + 2 == value.len() {
            return eval_contextual_calc(&rest[..end], context);
        }
    }
    if let Some(rest) = value.strip_prefix("var(") {
        let end = find_matching_paren(rest)?;
        let inner = &rest[..end];
        let (_, fallback) = inner.split_once(',')?;
        return resolve_contextual(fallback.trim(), context);
    }
    if let Some(rest) = value.strip_prefix("calc(") {
        let end = find_matching_paren(rest)?;
        return eval_contextual_calc(&rest[..end], context);
    }
    if let Some(rest) = value
        .strip_prefix("max(")
        .or_else(|| value.strip_prefix("min("))
    {
        let is_max = value.starts_with("max(");
        let end = find_matching_paren(rest)?;
        let args = split_top_level(&rest[..end], ',');
        let mut values = args
            .iter()
            .filter_map(|arg| eval_contextual_calc(arg, context));
        let mut best = values.next()?;
        for candidate in values {
            if (is_max && candidate > best) || (!is_max && candidate < best) {
                best = candidate;
            }
        }
        return Some(best);
    }
    if let Some(rest) = value.strip_prefix("clamp(") {
        let end = find_matching_paren(rest)?;
        let args = split_top_level(&rest[..end], ',');
        if args.len() == 3 {
            let low = eval_contextual_calc(args[0], context)?;
            let preferred = eval_contextual_calc(args[1], context)?;
            let high = eval_contextual_calc(args[2], context)?;
            return Some(preferred.min(high).max(low));
        }
    }
    if let Some(rest) = value.strip_prefix("round(") {
        let end = find_matching_paren(rest)?;
        let args = split_top_level(&rest[..end], ',');
        let (strategy, value_index) = match args.first()?.trim() {
            "nearest" | "up" | "down" | "to-zero" => (args[0].trim(), 1),
            _ => ("nearest", 0),
        };
        let resolved = eval_contextual_calc(args.get(value_index)?.trim(), context)?;
        let step = match args.get(value_index + 1) {
            Some(step) => eval_contextual_calc(step.trim(), context)?,
            None => 1.0,
        };
        return round_css_value(resolved, step, strategy);
    }
    contextual_atom(value, context)
}

fn round_css_value(value: f32, step: f32, strategy: &str) -> Option<f32> {
    if !value.is_finite() || !step.is_finite() || step == 0.0 {
        return None;
    }
    let quotient = value / step.abs();
    let rounded = match strategy {
        "up" => quotient.ceil(),
        "down" => quotient.floor(),
        "to-zero" => quotient.trunc(),
        _ => quotient.round(),
    };
    Some(rounded * step.abs())
}

fn contextual_atom(value: &str, context: &LengthContext) -> Option<f32> {
    let lower = value.trim().to_ascii_lowercase();
    let parse = |number: &str| number.trim().parse::<f32>().ok();
    if let Some(value) = lower.strip_suffix("rem").and_then(parse) {
        return Some(value * context.rem_px);
    }
    if let Some(value) = lower.strip_suffix("em").and_then(parse) {
        return Some(value * context.em_px);
    }
    if let Some(value) = lower.strip_suffix("ex").and_then(parse) {
        return Some(value * context.em_px * 0.528_320_3);
    }
    if let Some(value) = lower.strip_suffix("vmin").and_then(parse) {
        return Some(value * context.vw.min(context.vh));
    }
    if let Some(value) = lower.strip_suffix("vmax").and_then(parse) {
        return Some(value * context.vw.max(context.vh));
    }
    if let Some(value) = lower
        .strip_suffix("dvw")
        .or_else(|| lower.strip_suffix("svw"))
        .or_else(|| lower.strip_suffix("lvw"))
        .and_then(parse)
    {
        return Some(value * context.vw);
    }
    if let Some(value) = lower
        .strip_suffix("dvh")
        .or_else(|| lower.strip_suffix("svh"))
        .or_else(|| lower.strip_suffix("lvh"))
        .and_then(parse)
    {
        return Some(value * context.vh);
    }
    if let Some(value) = lower.strip_suffix("vw").and_then(parse) {
        return Some(value * context.vw);
    }
    if let Some(value) = lower.strip_suffix("vh").and_then(parse) {
        return Some(value * context.vh);
    }
    if let Some(value) = lower.strip_suffix('%').and_then(parse) {
        return Some(value * context.percent_base / 100.0);
    }
    if let Some(value) = lower.strip_suffix("px").and_then(parse) {
        return Some(value);
    }
    if let Some(value) = lower.strip_suffix("pt").and_then(parse) {
        return Some(value * 1.333);
    }
    parse(&lower)
}

fn eval_contextual_calc(expr: &str, context: &LengthContext) -> Option<f32> {
    let mut terms: Vec<(f32, String)> = Vec::new();
    let mut sign = 1.0;
    let mut current = String::new();
    let mut depth = 0i32;
    for character in expr.chars() {
        match character {
            '(' => {
                depth += 1;
                current.push(character);
            }
            ')' => {
                depth -= 1;
                current.push(character);
            }
            '+' | '-' if depth == 0 => {
                let follows_product_operator = current
                    .chars()
                    .rev()
                    .find(|character| !character.is_whitespace())
                    .is_some_and(|character| matches!(character, '*' | '/'));
                if follows_product_operator {
                    current.push(character);
                } else {
                    if !current.trim().is_empty() {
                        terms.push((sign, std::mem::take(&mut current)));
                    }
                    sign = if character == '-' { -1.0 } else { 1.0 };
                }
            }
            _ => current.push(character),
        }
    }
    if !current.trim().is_empty() {
        terms.push((sign, current));
    }
    if terms.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for (term_sign, term) in terms {
        total += term_sign * eval_contextual_product(term.trim(), context)?;
    }
    Some(total)
}

fn eval_contextual_product(term: &str, context: &LengthContext) -> Option<f32> {
    let mut result: Option<f32> = None;
    let mut operator = '*';
    let mut depth = 0i32;
    let mut current = String::new();
    let mut factors: Vec<(char, String)> = Vec::new();
    for character in term.chars() {
        match character {
            '(' => {
                depth += 1;
                current.push(character);
            }
            ')' => {
                depth -= 1;
                current.push(character);
            }
            '*' | '/' if depth == 0 => {
                if current.trim().is_empty() {
                    return None;
                }
                factors.push((operator, std::mem::take(&mut current)));
                operator = character;
            }
            _ => current.push(character),
        }
    }
    if current.trim().is_empty() {
        return None;
    }
    factors.push((operator, current));
    for (operator, factor) in &factors {
        let value = resolve_contextual(factor, context)?;
        result = Some(match result {
            None => value,
            Some(previous) if *operator == '/' => previous / value,
            Some(previous) => previous * value,
        });
    }
    result
}

/// Resolve a CSS length expression to px, recursively handling the small set
/// of functional forms real stylesheets actually nest in practice:
/// `var(--x, fallback)` (substitute the fallback; we track no custom
/// property values), `calc(...)`, and `min()`/`max()`. These commonly nest
/// inside each other (`calc(max(calc(var(--x,1rem) + 4px),10px))` is a real
/// example from Wikipedia's icon sizing), so each case recurses back into
/// this function rather than assuming a flat expression.
fn resolve_length(value: &str) -> Option<f32> {
    let v = value.trim();
    if let Some(rest) = v.strip_prefix('(') {
        let end = find_matching_paren(rest)?;
        if end + 2 == v.len() {
            return eval_calc(&rest[..end]);
        }
    }
    if let Some(rest) = v.strip_prefix("var(") {
        let end = find_matching_paren(rest)?;
        let inner = &rest[..end];
        let (_, fallback) = inner.split_once(',')?;
        return resolve_length(fallback.trim());
    }
    if let Some(rest) = v.strip_prefix("calc(") {
        let end = find_matching_paren(rest)?;
        return eval_calc(&rest[..end]);
    }
    if let Some(rest) = v.strip_prefix("max(").or_else(|| v.strip_prefix("min(")) {
        let is_max = v.starts_with("max(");
        let end = find_matching_paren(rest)?;
        let args = split_top_level(&rest[..end], ',');
        let mut values = args.iter().filter_map(|a| eval_calc(a));
        let mut best = values.next()?;
        for val in values {
            if (is_max && val > best) || (!is_max && val < best) {
                best = val;
            }
        }
        return Some(best);
    }
    if let Some(rest) = v.strip_prefix("clamp(") {
        // clamp(min, preferred, max) == max(min, min(preferred, max)). Widely
        // used for responsive widths/font-sizes/gaps; returning None here made
        // any `width: clamp(...)` element collapse (svelte.dev's hero grid).
        let end = find_matching_paren(rest)?;
        let args = split_top_level(&rest[..end], ',');
        if args.len() == 3 {
            let lo = eval_calc(args[0].trim())?;
            let mid = eval_calc(args[1].trim())?;
            let hi = eval_calc(args[2].trim())?;
            return Some(mid.min(hi).max(lo));
        }
    }
    if let Some(rest) = v.strip_prefix("round(") {
        let end = find_matching_paren(rest)?;
        let args = split_top_level(&rest[..end], ',');
        let (strategy, value_index) = match args.first()?.trim() {
            "nearest" | "up" | "down" | "to-zero" => (args[0].trim(), 1),
            _ => ("nearest", 0),
        };
        let resolved = eval_calc(args.get(value_index)?.trim())?;
        let step = match args.get(value_index + 1) {
            Some(step) => eval_calc(step.trim())?,
            None => 1.0,
        };
        return round_css_value(resolved, step, strategy);
    }
    if v.contains('(') {
        return None; // an unhandled function (env(), ...): no safe fallback
    }
    px_value(v).or_else(|| v.parse::<f32>().ok())
}

/// Find the index (relative to `s`) of the `)` matching an already-consumed
/// opening `(`, accounting for nesting.
fn find_matching_paren(s: &str) -> Option<usize> {
    let mut depth = 1i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split on `sep` at paren-depth 0 only, so `max(a,b)` inside an argument
/// list is not itself split on its internal comma.
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Evaluate a `calc()` body (already stripped of `calc(` / the matching `)`):
/// a left-to-right sum of terms, each itself a left-to-right `*`/`/` chain.
/// Terms may themselves be nested `max()`/`min()`/`var()`/`calc()` calls.
fn eval_calc(expr: &str) -> Option<f32> {
    let mut terms: Vec<(f32, String)> = Vec::new();
    let mut sign = 1.0;
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in expr.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            '+' | '-' if depth == 0 => {
                let follows_product_operator = cur
                    .chars()
                    .rev()
                    .find(|character| !character.is_whitespace())
                    .is_some_and(|character| matches!(character, '*' | '/'));
                if follows_product_operator {
                    cur.push(c);
                } else {
                    if !cur.trim().is_empty() {
                        terms.push((sign, std::mem::take(&mut cur)));
                    }
                    sign = if c == '-' { -1.0 } else { 1.0 };
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        terms.push((sign, cur));
    }
    if terms.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for (term_sign, term) in terms {
        total += term_sign * eval_product(term.trim())?;
    }
    Some(total)
}

/// Evaluate a `*`/`/` chain within one additive term of a calc() expression,
/// e.g. `-1 * 22px / 2`, where a factor may itself be a nested function call.
fn eval_product(term: &str) -> Option<f32> {
    let mut result: Option<f32> = None;
    let mut op = '*';
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut factors: Vec<(char, String)> = Vec::new();
    for c in term.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            '*' | '/' if depth == 0 => {
                if cur.trim().is_empty() {
                    return None;
                }
                factors.push((op, std::mem::take(&mut cur)));
                op = c;
            }
            _ => cur.push(c),
        }
    }
    if cur.trim().is_empty() {
        return None;
    }
    factors.push((op, cur));

    for (op, tok) in &factors {
        let v = resolve_length(tok)?;
        result = Some(match result {
            None => v,
            Some(r) if *op == '/' => r / v,
            Some(r) => r * v,
        });
    }
    result
}

/// Recognize a `list-style-type` / `list-style` keyword. Returns `None` for
/// tokens that are not list-style types (positions like `inside`, `url(...)`,
/// or unknown type names), so a shorthand scan can skip them.
fn list_style_keyword(tok: &str) -> Option<crate::ListStyle> {
    match tok.trim() {
        "none" => Some(crate::ListStyle::None),
        "disc" => Some(crate::ListStyle::Disc),
        "circle" => Some(crate::ListStyle::Circle),
        "square" => Some(crate::ListStyle::Square),
        "decimal" | "decimal-leading-zero" => Some(crate::ListStyle::Decimal),
        _ => None,
    }
}

/// An inset component (top/right/bottom/left). `auto` and absent both become
/// `None`; everything else keeps its (possibly relative) dimension for the
/// resolution pass.
fn inset_dim(value: &str) -> Option<crate::Dimension> {
    match dimension_value(value) {
        crate::Dimension::Auto => None,
        d => Some(d),
    }
}

fn set_inset_side(style: &mut LayoutStyle, index: usize, value: &str) {
    let value = value.trim();
    if let Some(expression) = deferred_length_expression(value) {
        style.inset[index] = None;
        style.inset_expressions[index] = Some(expression);
    } else {
        let inset = inset_dim(value);
        style.inset[index] = inset;
        style.inset_expressions[index] = inset
            .filter(|dimension| {
                matches!(
                    dimension,
                    crate::Dimension::Vw(_)
                        | crate::Dimension::Vh(_)
                        | crate::Dimension::Vmin(_)
                        | crate::Dimension::Vmax(_)
                )
            })
            .map(|_| value.to_string());
    }
}

/// Parse `counter-reset`, `counter-increment`, and `counter-set` name/value
/// pairs. Gecko likewise assigns the property-specific default integer when a
/// name is not followed by an integer (reset/set 0, increment 1).
fn parse_counter_directives(
    value: &str,
    default_value: i32,
) -> Option<Vec<crate::CounterDirective>> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("none")
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "initial" | "inherit" | "unset" | "revert" | "revert-layer"
        )
    {
        return Some(Vec::new());
    }

    let tokens = split_ws_paren(trimmed);
    if tokens.is_empty() {
        return None;
    }
    let mut result = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let name = tokens[index].trim();
        if !valid_counter_name(name) {
            return None;
        }
        index += 1;
        let value = tokens
            .get(index)
            .and_then(|token| token.parse::<i32>().ok())
            .map(|value| {
                index += 1;
                value
            })
            .unwrap_or(default_value);
        result.push(crate::CounterDirective {
            name: name.to_string(),
            value,
        });
    }
    Some(result)
}

fn valid_counter_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !name.is_empty()
        && !matches!(
            lower.as_str(),
            "none" | "initial" | "inherit" | "unset" | "revert" | "revert-layer"
        )
        && !name
            .contains(|ch: char| ch.is_whitespace() || matches!(ch, '(' | ')' | ',' | '"' | '\''))
}

/// Absolute keyword font-sizes (the `medium`-anchored scale), for the handful
/// of pages that still use them.
fn font_size_keyword(v: &str) -> Option<f32> {
    Some(match v.to_ascii_lowercase().as_str() {
        "xx-small" => 9.6,
        "x-small" => 12.0,
        "small" => 13.3,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        _ => return None,
    })
}

fn apply_font_size(style: &mut LayoutStyle, value: &str) {
    let value = value.trim();
    match value.to_ascii_lowercase().as_str() {
        // font-size is inherited; unset therefore has inherit semantics.
        "inherit" | "unset" => {
            style.font_size = None;
            style.font_size_raw = None;
            style.font_size_expression = None;
            return;
        }
        "initial" => {
            style.font_size = Some(16.0);
            style.font_size_raw = None;
            style.font_size_expression = None;
            return;
        }
        // The compact cascade does not retain origin/layer history. Preserve
        // the already-applied lower-origin value instead of treating revert
        // as inherit or initial.
        "revert" | "revert-layer" => return,
        _ => {}
    }
    if value.contains('(') {
        style.font_size = None;
        style.font_size_raw = None;
        style.font_size_expression = Some(value.to_string());
        return;
    }
    style.font_size_expression = None;
    match dimension_value(value) {
        crate::Dimension::Px(p) => {
            style.font_size = Some(p);
            style.font_size_raw = None;
        }
        crate::Dimension::Auto => {
            // Keyword sizes (medium/small/large/...) or unknown; map the
            // common ones, else leave to inherit.
            if let Some(px) = font_size_keyword(value.trim()) {
                style.font_size = Some(px);
                style.font_size_raw = None;
            }
        }
        rel => {
            style.font_size = None;
            style.font_size_raw = Some(rel);
        }
    }
}

fn apply_letter_spacing(style: &mut LayoutStyle, value: &str) {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "normal" | "initial" | "revert" | "revert-layer"
    ) {
        style.letter_spacing = Some(0.0);
        style.letter_spacing_raw = None;
        style.letter_spacing_expression = None;
        style.letter_spacing_non_normal = Some(false);
        return;
    }
    // `letter-spacing` inherits, so `unset` has the same computed behavior as
    // `inherit`. Leave this style unspecified for the top-down pass.
    if matches!(lower.as_str(), "inherit" | "unset") || value.is_empty() {
        return;
    }
    // Percentages are invalid even inside CSS math. Do not clear an earlier
    // valid cascade winner when a later declaration is invalid.
    if value.contains('%') {
        return;
    }
    style.letter_spacing_non_normal = Some(true);
    if value.contains('(') {
        style.letter_spacing = None;
        style.letter_spacing_raw = None;
        style.letter_spacing_expression = Some(value.to_string());
        return;
    }
    style.letter_spacing_expression = None;
    match dimension_value(value) {
        crate::Dimension::Px(pixels) if pixels.is_finite() => {
            style.letter_spacing = Some(pixels);
            style.letter_spacing_raw = None;
        }
        crate::Dimension::Percent(_) | crate::Dimension::Auto => {
            // Percentages and keywords other than `normal` are invalid.
            style.letter_spacing = None;
            style.letter_spacing_raw = None;
            style.letter_spacing_non_normal = None;
        }
        relative => {
            style.letter_spacing = None;
            style.letter_spacing_raw = Some(relative);
        }
    }
}

fn parse_text_indent(value: &str) -> Option<crate::Dimension> {
    // The Level 3 `hanging` and `each-line` keywords change which lines are
    // indented. Keep them invalid until those semantics are represented.
    if split_ws_paren(value).len() != 1 {
        return None;
    }
    let dimension = dimension_value(value);
    (!matches!(dimension, crate::Dimension::Auto)).then_some(dimension)
}

fn apply_text_indent(style: &mut LayoutStyle, value: &str) {
    match value.trim().to_ascii_lowercase().as_str() {
        "initial" => style.text_indent = Some(crate::Dimension::Px(0.0)),
        // This is an inherited property, so `unset` computes like `inherit`.
        "inherit" | "unset" | "revert" | "revert-layer" => style.text_indent = None,
        _ => {
            if let Some(indent) = parse_text_indent(value) {
                style.text_indent = Some(indent);
            }
        }
    }
}

fn apply_gap_value(style: &mut LayoutStyle, row: bool, value: &str) {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let contextual = lower.contains('(')
        || lower.ends_with('%')
        || ["rem", "em", "ex", "vw", "vh", "vmin", "vmax"]
            .iter()
            .any(|unit| lower.ends_with(unit));
    let expression = if value.eq_ignore_ascii_case("normal") || value.is_empty() || !contextual {
        None
    } else {
        Some(value.to_string())
    };
    let immediate = if value.eq_ignore_ascii_case("normal") || value.is_empty() {
        None
    } else {
        px(value)
    };
    if row {
        style.row_gap = immediate;
        style.row_gap_expression = expression;
    } else {
        style.column_gap = immediate;
        style.column_gap_expression = expression;
    }
}

fn parse_column_count(value: &str) -> Option<u16> {
    let count = value.trim().parse::<u32>().ok()?;
    // Keep pathological author input from allocating thousands of anonymous
    // fragmentainers. Real page layouts use single-digit counts; 64 retains
    // ample useful range while keeping construction and balancing bounded.
    (count > 0).then(|| count.min(64) as u16)
}

pub(crate) fn line_height_expression_is_length(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains('%')
        || [
            "px", "pt", "pc", "in", "cm", "mm", "rem", "em", "ex", "vw", "vh", "vmin", "vmax",
        ]
        .iter()
        .any(|unit| lower.contains(unit))
}

/// Parse the layout-relevant portion of the CSS `font` shorthand:
/// `[style || variant || weight || stretch]? size [/ line-height]? family`.
///
/// As in Gecko's shorthand expansion, omitted longhands reset to their
/// initial values instead of inheriting a previously cascaded declaration.
/// We do not model variant/stretch, but still accept their keywords before
/// the required size so modern design-system declarations reach the size,
/// line-height, weight, style, and family fields that affect our layout.
fn apply_font_shorthand(style: &mut LayoutStyle, value: &str) {
    let tokens = split_ws_paren(value);
    let Some((size_index, size, attached_line_height)) =
        tokens.iter().enumerate().find_map(|(index, token)| {
            let (candidate, line_height) = token.split_once('/').unwrap_or((token, ""));
            is_font_size_token(candidate).then_some((index, candidate, line_height))
        })
    else {
        return;
    };

    let mut family_index = size_index + 1;
    let mut line_height = (!attached_line_height.is_empty()).then_some(attached_line_height);
    if line_height.is_none() && family_index < tokens.len() {
        if tokens[family_index] == "/" {
            family_index += 1;
            if family_index < tokens.len() {
                line_height = Some(tokens[family_index]);
                family_index += 1;
            }
        } else if let Some(after_slash) = tokens[family_index].strip_prefix('/') {
            if !after_slash.is_empty() {
                line_height = Some(after_slash);
            }
            family_index += 1;
        }
    }
    if family_index >= tokens.len() {
        return;
    }

    // The shorthand resets every constituent before applying supplied values.
    style.font_style_italic = Some(false);
    style.font_weight = Some("400".to_string());
    style.font_optical_sizing = Some(crate::FontOpticalSizing::Auto);
    style.font_variation_settings = Some(Vec::new());
    style.line_height = Some(crate::LineHeight::Normal);
    style.line_height_expression = None;
    for token in &tokens[..size_index] {
        let lower = token.to_ascii_lowercase();
        if lower == "italic" || lower.starts_with("oblique") {
            style.font_style_italic = Some(true);
        } else if let Some(weight) = specified_font_weight(&lower) {
            style.font_weight = Some(weight);
        }
    }
    apply_font_size(style, size);
    if let Some(line_height) = line_height {
        apply_value(style, "line-height", line_height);
    }
    style.font_family = Some(tokens[family_index..].join(" ").to_ascii_lowercase());
}

/// Normalize a specified CSS font weight while preserving relative keywords
/// until the top-down inheritance pass can see the parent's computed weight.
fn specified_font_weight(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    match lower.as_str() {
        "normal" => Some("400".to_string()),
        "bold" => Some("700".to_string()),
        "bolder" | "lighter" => Some(lower),
        _ => lower
            .parse::<f32>()
            .ok()
            .filter(|weight| weight.is_finite() && (1.0..=1000.0).contains(weight))
            .map(|weight| weight.round().to_string()),
    }
}

/// Resolve `font-weight` to the numeric computed value defined by CSS Fonts.
/// Relative keywords use the inherited weight table rather than a binary
/// normal/bold threshold.
pub(crate) fn computed_font_weight(specified: Option<&str>, inherited: u16) -> u16 {
    match specified {
        None => inherited,
        Some("inherit" | "unset") => inherited,
        Some("normal" | "initial") => 400,
        Some("bold") => 700,
        Some("bolder") if inherited < 100 => 400,
        Some("bolder") if inherited < 350 => 400,
        Some("bolder") if inherited < 550 => 700,
        Some("bolder") if inherited < 900 => 900,
        Some("bolder") => inherited,
        Some("lighter") if inherited < 100 => inherited,
        Some("lighter") if inherited < 350 => 100,
        Some("lighter") if inherited < 550 => 100,
        Some("lighter") if inherited < 750 => 400,
        Some("lighter") if inherited < 900 => 700,
        Some("lighter") => 700,
        Some(weight) => weight
            .parse::<f32>()
            .ok()
            .filter(|weight| weight.is_finite())
            .map(|weight| weight.round().clamp(1.0, 1000.0) as u16)
            .unwrap_or(inherited),
    }
}

pub(crate) fn used_font_weight(style: &LayoutStyle) -> u16 {
    computed_font_weight(style.font_weight.as_deref(), 400)
}

fn is_font_size_token(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    if lower == "0" || font_size_keyword(&lower).is_some() {
        return true;
    }
    if lower.starts_with("calc(")
        || lower.starts_with("min(")
        || lower.starts_with("max(")
        || lower.starts_with("clamp(")
    {
        return true;
    }
    [
        "px", "pt", "em", "ex", "rem", "vw", "vh", "dvw", "dvh", "svw", "svh", "lvw", "lvh",
        "vmin", "vmax", "%",
    ]
    .iter()
    .any(|unit| {
        lower
            .strip_suffix(unit)
            .and_then(|number| number.parse::<f32>().ok())
            .is_some()
    })
}

fn dimension_value(tok: &str) -> crate::Dimension {
    use crate::Dimension;
    let n = tok.trim();
    if n.eq_ignore_ascii_case("auto") || n.is_empty() {
        return Dimension::Auto;
    }
    // calc()/min()/max()/var(): resolve context-free to px where possible
    // (relative units inside are approximated; rare for these properties).
    if n.contains('(') {
        return px(n).map(Dimension::Px).unwrap_or(Dimension::Auto);
    }
    let lower = n.to_ascii_lowercase();
    let parse = |s: &str| s.trim().parse::<f32>().ok();
    if let Some(v) = lower.strip_suffix('%').and_then(parse) {
        return Dimension::Percent(v / 100.0);
    }
    // Order matters: check `rem` before `em`, `vmin`/`vmax` before `vw`/`vh`.
    if let Some(v) = lower.strip_suffix("rem").and_then(parse) {
        return Dimension::Rem(v);
    }
    if let Some(v) = lower.strip_suffix("em").and_then(parse) {
        return Dimension::Em(v);
    }
    if let Some(v) = lower.strip_suffix("ex").and_then(parse) {
        return Dimension::Ex(v);
    }
    if let Some(v) = lower.strip_suffix("vmin").and_then(parse) {
        return Dimension::Vmin(v);
    }
    if let Some(v) = lower.strip_suffix("vmax").and_then(parse) {
        return Dimension::Vmax(v);
    }
    if let Some(v) = lower
        .strip_suffix("dvw")
        .or_else(|| lower.strip_suffix("svw"))
        .or_else(|| lower.strip_suffix("lvw"))
        .and_then(parse)
    {
        return Dimension::Vw(v);
    }
    if let Some(v) = lower
        .strip_suffix("dvh")
        .or_else(|| lower.strip_suffix("svh"))
        .or_else(|| lower.strip_suffix("lvh"))
        .and_then(parse)
    {
        return Dimension::Vh(v);
    }
    if let Some(v) = lower.strip_suffix("vw").and_then(parse) {
        return Dimension::Vw(v);
    }
    if let Some(v) = lower.strip_suffix("vh").and_then(parse) {
        return Dimension::Vh(v);
    }
    if let Some(v) = lower.strip_suffix("px").and_then(parse) {
        return Dimension::Px(v);
    }
    if let Some(v) = lower.strip_suffix("pt").and_then(parse) {
        return Dimension::Px(v * 1.333);
    }
    // CSS lengths accept a unitless number only when it is zero. Treating
    // arbitrary numbers as pixels changes invalid declarations into tiny
    // geometry (for example `font-size:.813` must be ignored and inherited,
    // not rendered at 0.813px).
    if let Some(v) = parse(&lower).filter(|v| *v == 0.0) {
        return Dimension::Px(v);
    }
    Dimension::Auto
}

/// Parse the currently painted basic-shape subset of `clip-path`.
///
/// The default (and only supported) geometry box is `border-box`. Rejecting
/// the other reference boxes here keeps `@supports` aligned with the geometry
/// we actually paint. Functional coordinates are likewise rejected until
/// their unresolved math can be retained through computed style.
fn parse_clip_path_polygon(value: &str) -> Option<crate::ClipPathPolygon> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if !lower.starts_with("polygon(") {
        return None;
    }
    let close = value.rfind(')')?;
    let suffix = value[close + 1..].trim();
    if !suffix.is_empty() && !suffix.eq_ignore_ascii_case("border-box") {
        return None;
    }
    let inner = value["polygon(".len()..close].trim();
    let mut components = split_top_level(inner, ',');
    let mut fill_rule = crate::ClipPathFillRule::Nonzero;
    if let Some(first) = components.first().map(|component| component.trim()) {
        if first.eq_ignore_ascii_case("evenodd") {
            fill_rule = crate::ClipPathFillRule::Evenodd;
            components.remove(0);
        } else if first.eq_ignore_ascii_case("nonzero") {
            components.remove(0);
        }
    }
    if components.is_empty() {
        return None;
    }
    let mut points = Vec::with_capacity(components.len());
    for component in components {
        let coordinates = split_ws_paren(component);
        let [x, y] = coordinates.as_slice() else {
            return None;
        };
        if x.contains('(') || y.contains('(') {
            return None;
        }
        let x = dimension_value(x);
        let y = dimension_value(y);
        if matches!(x, crate::Dimension::Auto) || matches!(y, crate::Dimension::Auto) {
            return None;
        }
        let finite = |coordinate: crate::Dimension| match coordinate {
            crate::Dimension::Auto => false,
            crate::Dimension::Px(value)
            | crate::Dimension::Percent(value)
            | crate::Dimension::Em(value)
            | crate::Dimension::Ex(value)
            | crate::Dimension::Rem(value)
            | crate::Dimension::Vw(value)
            | crate::Dimension::Vh(value)
            | crate::Dimension::Vmin(value)
            | crate::Dimension::Vmax(value) => value.is_finite(),
        };
        if !finite(x) || !finite(y) {
            return None;
        }
        points.push((x, y));
    }
    Some(crate::ClipPathPolygon { fill_rule, points })
}

/// Split a 1-or-2 value shorthand into (start, end); a single value applies to
/// both. Used by the logical-property axes (`margin-inline`, `padding-block`).
fn two(value: &str) -> (&str, &str) {
    let values = split_ws_paren(value);
    let a = values.first().copied().unwrap_or("0");
    let b = values.get(1).copied().unwrap_or(a);
    (a, b)
}

/// Set one margin side (0=top,1=right,2=bottom,3=left), tracking `auto` and, for
/// a percentage, deferring resolution to the containing-block width in the
/// top-down pass (recorded in `margin_percent`).
fn set_margin_side(style: &mut LayoutStyle, idx: usize, value: &str) {
    let v = value.trim();
    let is_auto = v.eq_ignore_ascii_case("auto");
    if let Some(expression) = deferred_length_expression(v) {
        style.margin_expressions[idx] = Some(expression);
        style.margin_percent[idx] = None;
        style.margin_relative[idx] = None;
        style.margin_auto[idx] = false;
        set_margin_px(&mut style.margin, idx, 0.0);
        return;
    }
    style.margin_expressions[idx] = None;
    if let Some(frac) = percent_fraction(v) {
        style.margin_percent[idx] = Some(frac);
        style.margin_relative[idx] = None;
        set_margin_px(&mut style.margin, idx, 0.0);
        style.margin_auto[idx] = false;
        return;
    }
    let dimension = dimension_value(v);
    match dimension {
        crate::Dimension::Px(px) => {
            set_margin_px(&mut style.margin, idx, px);
            style.margin_relative[idx] = None;
        }
        crate::Dimension::Em(_)
        | crate::Dimension::Ex(_)
        | crate::Dimension::Rem(_)
        | crate::Dimension::Vw(_)
        | crate::Dimension::Vh(_)
        | crate::Dimension::Vmin(_)
        | crate::Dimension::Vmax(_) => {
            set_margin_px(&mut style.margin, idx, 0.0);
            style.margin_relative[idx] = Some(dimension);
        }
        _ => {
            set_margin_px(&mut style.margin, idx, 0.0);
            style.margin_relative[idx] = None;
        }
    }
    style.margin_auto[idx] = is_auto;
    style.margin_percent[idx] = None;
}

fn set_margin_px(margin: &mut Edges, idx: usize, px: f32) {
    match idx {
        0 => margin.top = px,
        1 => margin.right = px,
        2 => margin.bottom = px,
        3 => margin.left = px,
        _ => {}
    }
}

/// Set one padding side (0=top,1=right,2=bottom,3=left). A percentage is
/// recorded in `padding_percent` and resolved against the containing-block
/// width during the top-down pass; a length is stored directly.
fn set_padding_side(style: &mut LayoutStyle, idx: usize, value: &str) {
    let value = value.trim();
    if let Some(expression) = deferred_length_expression(value) {
        style.padding_expressions[idx] = Some(expression);
        style.padding_percent[idx] = None;
        style.padding_relative[idx] = None;
        set_padding_px(&mut style.padding, idx, 0.0);
        return;
    }
    style.padding_expressions[idx] = None;
    if let Some(frac) = percent_fraction(value) {
        style.padding_percent[idx] = Some(frac);
        style.padding_relative[idx] = None;
        set_padding_px(&mut style.padding, idx, 0.0);
        return;
    }
    let dimension = dimension_value(value);
    match dimension {
        crate::Dimension::Px(px) => {
            set_padding_px(&mut style.padding, idx, px);
            style.padding_relative[idx] = None;
            style.padding_percent[idx] = None;
        }
        crate::Dimension::Em(_)
        | crate::Dimension::Ex(_)
        | crate::Dimension::Rem(_)
        | crate::Dimension::Vw(_)
        | crate::Dimension::Vh(_)
        | crate::Dimension::Vmin(_)
        | crate::Dimension::Vmax(_) => {
            set_padding_px(&mut style.padding, idx, 0.0);
            style.padding_relative[idx] = Some(dimension);
            style.padding_percent[idx] = None;
        }
        _ => {}
    }
}

fn set_padding_px(padding: &mut Edges, idx: usize, px: f32) {
    match idx {
        0 => padding.top = px,
        1 => padding.right = px,
        2 => padding.bottom = px,
        3 => padding.left = px,
        _ => {}
    }
}

/// `padding: <t> <r>? <b>? <l>?`, percentage-aware per side.
fn apply_padding_shorthand(style: &mut LayoutStyle, value: &str) {
    let toks = split_ws_paren(value);
    let (t, r, b, l) = match toks.as_slice() {
        [a] => (*a, *a, *a, *a),
        [v, h] => (*v, *h, *v, *h),
        [t, h, b] => (*t, *h, *b, *h),
        [t, r, b, l, ..] => (*t, *r, *b, *l),
        [] => return,
    };
    set_padding_side(style, 0, t);
    set_padding_side(style, 1, r);
    set_padding_side(style, 2, b);
    set_padding_side(style, 3, l);
}

/// A bare `<number>%` token as a 0..1 fraction (`56.25%` -> `0.5625`). Returns
/// `None` for anything that is not a plain percentage (lengths, `calc(...%)`,
/// keywords), so those keep their existing length handling.
fn percent_fraction(tok: &str) -> Option<f32> {
    let num = tok.trim().strip_suffix('%')?;
    let v: f32 = num.trim().parse().ok()?;
    if v.is_finite() {
        Some(v / 100.0)
    } else {
        None
    }
}

/// `margin: <t> <r>? <b>? <l>?` with per-side `auto` (so `margin: 0 auto`
/// centers).
fn apply_margin_shorthand(style: &mut LayoutStyle, value: &str) {
    let toks = split_ws_paren(value);
    let (t, r, b, l) = match toks.as_slice() {
        [a] => (*a, *a, *a, *a),
        [v, h] => (*v, *h, *v, *h),
        [t, h, b] => (*t, *h, *b, *h),
        [t, r, b, l, ..] => (*t, *r, *b, *l),
        [] => return,
    };
    set_margin_side(style, 0, t);
    set_margin_side(style, 1, r);
    set_margin_side(style, 2, b);
    set_margin_side(style, 3, l);
}

fn px_value(tok: &str) -> Option<f32> {
    let mut n = tok;
    let mut scale = 1.0;

    if n.ends_with("px") {
        n = &n[..n.len() - 2];
    } else if n.ends_with("pt") {
        n = &n[..n.len() - 2];
        scale = 1.333; // 1pt ≈ 1.333px
    } else if n.ends_with("em") || n.ends_with("rem") {
        n = n.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        scale = 16.0; // 1em = 16px
    } else if n.ends_with("ex") {
        n = n.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        scale = 16.0 * 0.528_320_3;
    } else if n.ends_with('%') {
        n = &n[..n.len() - 1];
        scale = 16.0 / 100.0;
    } else {
        n = n.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    }

    if n.chars()
        .any(|c| !(c.is_ascii_digit() || c == '.' || c == '-'))
    {
        return None;
    }
    n.parse::<f32>().ok().map(|v| v * scale)
}

fn token(value: &str) -> Option<&str> {
    value.split_whitespace().next()
}

fn set_background_gradients(style: &mut LayoutStyle, value: &str) {
    let (layers, radial_geometries) =
        parse_background_gradient_layers(value, style.color_scheme_dark);
    style.background_gradient = layers.iter().find_map(|layer| match layer {
        crate::BackgroundGradientLayer::Linear { angle, stops, .. } => {
            Some((*angle, stops.clone()))
        }
        _ => None,
    });
    style.background_radial_gradient = layers.iter().find_map(|layer| match layer {
        crate::BackgroundGradientLayer::Radial { center, stops } => Some((*center, stops.clone())),
        _ => None,
    });
    style.background_radial_gradient_geometry =
        layers
            .iter()
            .zip(&radial_geometries)
            .find_map(|(layer, geometry)| {
                matches!(layer, crate::BackgroundGradientLayer::Radial { .. })
                    .then_some(*geometry)
                    .flatten()
            });
    style.background_conic_gradient = layers.iter().find_map(|layer| match layer {
        crate::BackgroundGradientLayer::Conic {
            angle,
            center,
            stops,
        } => Some((*angle, *center, stops.clone())),
        _ => None,
    });
    style.background_gradient_layers = layers;
    style.background_gradient_layer_radial_geometries = radial_geometries;
}

fn parse_background_gradient_layers(
    value: &str,
    dark_scheme: bool,
) -> (
    Vec<crate::BackgroundGradientLayer>,
    Vec<Option<crate::RadialGradientGeometry>>,
) {
    let mut layers = Vec::new();
    let mut radial_geometries = Vec::new();
    for authored_layer in split_top_level(value, ',') {
        if let Some(linear) = parse_linear_gradient(authored_layer, dark_scheme) {
            layers.push(crate::BackgroundGradientLayer::Linear {
                angle: linear.angle,
                stops: linear.stops,
                stop_positions: linear.stop_positions,
                repeating: linear.repeating,
            });
            radial_geometries.push(None);
        } else if let Some(radial) = parse_radial_gradient(authored_layer, dark_scheme) {
            layers.push(crate::BackgroundGradientLayer::Radial {
                center: radial.center,
                stops: radial.stops,
            });
            radial_geometries.push(Some(radial.geometry));
        } else if let Some((angle, center, stops)) =
            parse_conic_gradient(authored_layer, dark_scheme)
        {
            layers.push(crate::BackgroundGradientLayer::Conic {
                angle,
                center,
                stops,
            });
            radial_geometries.push(None);
        }
    }
    (layers, radial_geometries)
}

struct ParsedLinearGradient {
    angle: f32,
    stops: Vec<([u8; 4], Option<f32>)>,
    stop_positions: Vec<Option<String>>,
    repeating: bool,
}

/// Parse a `linear-gradient(...)` (also `repeating-`/`-webkit-`/`-moz-`) into
/// (angle-degrees, color stops). Angle is CSS convention (0deg = to top, grows
/// clockwise); `to <side>` keywords map to their angle. Color stops keep their
/// optional 0..1 position. Returns None if it is not a linear-gradient or has
/// no parseable colors. Radial/conic gradients are not handled (None).
fn parse_linear_gradient(value: &str, dark_scheme: bool) -> Option<ParsedLinearGradient> {
    let v = value.trim();
    let lower = v.to_ascii_lowercase();
    let start = lower.find("linear-gradient(")?;
    let repeating = lower[..start].ends_with("repeating-");
    // The original prefixed WebKit gradient syntax predates the standardized
    // angle system: 0deg points right and positive angles turn
    // counter-clockwise. Blink still accepts that syntax for compatibility.
    // Convert it to our standard CSS angle (0deg up, clockwise) before paint.
    let prefix = &lower[..start];
    let legacy_webkit_angle =
        prefix.ends_with("-webkit-") || prefix.ends_with("-webkit-repeating-");
    let open = start + "linear-gradient(".len();
    // Match the closing paren for this function.
    let bytes = v.as_bytes();
    let mut depth = 1;
    let mut end = open;
    while end < bytes.len() && depth > 0 {
        match bytes[end] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        end += 1;
    }
    let inner = &v[open..end.saturating_sub(1)];
    // Split on top-level commas (respect rgb()/rgba()/hsl() parens).
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut d = 0i32;
    for c in inner.chars() {
        match c {
            '(' => {
                d += 1;
                cur.push(c);
            }
            ')' => {
                d -= 1;
                cur.push(c);
            }
            ',' if d == 0 => {
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    if parts.is_empty() {
        return None;
    }
    // Leading angle / direction, if present.
    let mut angle = 180.0f32; // default: to bottom
    let first = parts[0].trim().to_ascii_lowercase();
    let mut stop_start = 0;
    if first.ends_with("deg") {
        if let Ok(a) = first.trim_end_matches("deg").trim().parse::<f32>() {
            angle = if legacy_webkit_angle {
                (90.0 - a).rem_euclid(360.0)
            } else {
                a.rem_euclid(360.0)
            };
        }
        stop_start = 1;
    } else if first.starts_with("to ") {
        angle = match first.as_str() {
            "to top" => 0.0,
            "to right" => 90.0,
            "to bottom" => 180.0,
            "to left" => 270.0,
            "to top right" | "to right top" => 45.0,
            "to bottom right" | "to right bottom" => 135.0,
            "to bottom left" | "to left bottom" => 225.0,
            "to top left" | "to left top" => 315.0,
            _ => 180.0,
        };
        stop_start = 1;
    } else if first.starts_with("turn") || first.ends_with("turn") {
        stop_start = 1;
    }
    let mut stops: Vec<([u8; 4], Option<f32>)> = Vec::new();
    let mut stop_positions = Vec::new();
    for p in &parts[stop_start..] {
        let t = p.trim();
        if t.is_empty() {
            continue;
        }
        let tokens = split_ws_paren(t);
        let parsed = (1..=tokens.len()).find_map(|color_tokens| {
            let position_tokens = &tokens[color_tokens..];
            if position_tokens.len() > 2
                || !position_tokens
                    .iter()
                    .all(|position| gradient_position_is_valid(position))
            {
                return None;
            }
            let color = tokens[..color_tokens].join(" ");
            parse_color_for_scheme(&color, dark_scheme).map(|color| (color, position_tokens))
        });
        if let Some((color, positions)) = parsed {
            if positions.is_empty() {
                stops.push((color, None));
                stop_positions.push(None);
            } else {
                for position in positions {
                    let percentage = position
                        .strip_suffix('%')
                        .and_then(|number| number.parse::<f32>().ok())
                        .map(|percentage| percentage / 100.0);
                    stops.push((color, percentage));
                    stop_positions.push(Some(position.trim().to_string()));
                }
            }
        }
    }
    if stops.len() < 2 {
        // A single-color "gradient" is just that color; let the caller fall back
        // to background_color instead (return None so parse_color runs).
        return None;
    }
    Some(ParsedLinearGradient {
        angle,
        stops,
        stop_positions,
        repeating,
    })
}

fn gradient_position_is_valid(value: &str) -> bool {
    let value = value.trim();
    if value == "0" || value == "-0" || value.contains('(') {
        return true;
    }
    [
        "%", "px", "em", "rem", "ex", "vw", "vh", "vmin", "vmax", "dvw", "dvh", "svw", "svh",
        "lvw", "lvh",
    ]
    .iter()
    .any(|suffix| {
        value
            .strip_suffix(suffix)
            .and_then(|number| number.trim().parse::<f32>().ok())
            .is_some_and(f32::is_finite)
    })
}

struct ParsedRadialGradient {
    center: (f32, f32),
    stops: Vec<([u8; 4], Option<f32>)>,
    geometry: crate::RadialGradientGeometry,
}

fn parse_radial_gradient(value: &str, dark_scheme: bool) -> Option<ParsedRadialGradient> {
    let lower = value.to_ascii_lowercase();
    let start = lower.find("radial-gradient(")?;
    let open = start + "radial-gradient(".len();
    let end = find_matching_paren(&value[open..])? + open;
    let parts = split_top_level(&value[open..end], ',');
    if parts.is_empty() {
        return None;
    }
    let mut center = (0.5, 0.5);
    let mut geometry = crate::RadialGradientGeometry::default();
    let mut stop_start = 0;
    let prelude = parts[0].trim().to_ascii_lowercase();
    if prelude.contains(" at ") || prelude.starts_with("at ") {
        let (shape, coords) = prelude
            .split_once(" at ")
            .map(|(shape, coords)| (shape, coords))
            .or_else(|| prelude.strip_prefix("at ").map(|coords| ("", coords)))?;
        geometry = parse_radial_gradient_geometry(shape)?;
        center = parse_gradient_center(coords);
        stop_start = 1;
    } else if parse_color_for_scheme(split_color_stop(parts[0].trim()).0, dark_scheme).is_none() {
        geometry = parse_radial_gradient_geometry(&prelude)?;
        stop_start = 1;
    }
    let mut stops = Vec::new();
    for part in &parts[stop_start..] {
        let (color, position) = split_color_stop(part.trim());
        if let Some(color) = parse_color_for_scheme(color, dark_scheme) {
            stops.push((color, position));
        }
    }
    (stops.len() >= 2).then_some(ParsedRadialGradient {
        center,
        stops,
        geometry,
    })
}

fn parse_radial_gradient_geometry(value: &str) -> Option<crate::RadialGradientGeometry> {
    use crate::{
        RadialGradientGeometry as Geometry, RadialGradientShape as Shape,
        RadialGradientSize as Size,
    };

    let tokens = value
        .split_whitespace()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return Some(Geometry::default());
    }

    let explicit_shape = tokens.iter().find_map(|token| match *token {
        "circle" => Some(Shape::Circle),
        "ellipse" => Some(Shape::Ellipse),
        _ => None,
    });
    if tokens
        .iter()
        .filter(|token| matches!(**token, "circle" | "ellipse"))
        .count()
        > 1
    {
        return None;
    }

    let extent = tokens.iter().find_map(|token| match *token {
        "closest-side" | "contain" => Some(Size::ClosestSide),
        "closest-corner" => Some(Size::ClosestCorner),
        "farthest-side" => Some(Size::FarthestSide),
        "farthest-corner" | "cover" => Some(Size::FarthestCorner),
        _ => None,
    });
    if tokens
        .iter()
        .filter(|token| {
            matches!(
                **token,
                "closest-side"
                    | "closest-corner"
                    | "farthest-side"
                    | "farthest-corner"
                    | "contain"
                    | "cover"
            )
        })
        .count()
        > 1
    {
        return None;
    }

    let dimensions = tokens
        .iter()
        .filter(|token| {
            !matches!(
                **token,
                "circle"
                    | "ellipse"
                    | "closest-side"
                    | "closest-corner"
                    | "farthest-side"
                    | "farthest-corner"
                    | "contain"
                    | "cover"
            )
        })
        .map(|token| dimension_value(token))
        .collect::<Vec<_>>();
    if dimensions
        .iter()
        .any(|dimension| !radial_radius_is_non_negative(*dimension))
    {
        return None;
    }
    if extent.is_some() && !dimensions.is_empty() {
        return None;
    }

    match dimensions.as_slice() {
        [] => Some(Geometry {
            shape: explicit_shape.unwrap_or(Shape::Ellipse),
            size: extent.unwrap_or(Size::FarthestCorner),
        }),
        [radius] if explicit_shape != Some(Shape::Ellipse) => {
            // Circle radii are lengths, never percentages.
            if matches!(radius, crate::Dimension::Percent(_)) {
                return None;
            }
            Some(Geometry {
                shape: Shape::Circle,
                size: Size::Explicit(*radius, *radius),
            })
        }
        [x, y] if explicit_shape != Some(Shape::Circle) => Some(Geometry {
            shape: Shape::Ellipse,
            size: Size::Explicit(*x, *y),
        }),
        _ => None,
    }
}

fn radial_radius_is_non_negative(value: crate::Dimension) -> bool {
    match value {
        crate::Dimension::Auto => false,
        crate::Dimension::Px(value)
        | crate::Dimension::Percent(value)
        | crate::Dimension::Em(value)
        | crate::Dimension::Ex(value)
        | crate::Dimension::Rem(value)
        | crate::Dimension::Vw(value)
        | crate::Dimension::Vh(value)
        | crate::Dimension::Vmin(value)
        | crate::Dimension::Vmax(value) => value.is_finite() && value >= 0.0,
    }
}

fn parse_gradient_center(value: &str) -> (f32, f32) {
    let mut center = (0.5, 0.5);
    let mut percentages = value.split_whitespace().filter_map(percent_fraction);
    if let Some(x) = percentages.next() {
        center.0 = x;
    }
    if let Some(y) = percentages.next() {
        center.1 = y;
    }
    for token in value.split_whitespace() {
        match token {
            "left" => center.0 = 0.0,
            "right" => center.0 = 1.0,
            "top" => center.1 = 0.0,
            "bottom" => center.1 = 1.0,
            // `center` leaves the corresponding unspecified axis at 50%.
            _ => {}
        }
    }
    center
}

/// Parse the common `conic-gradient(from A at X Y, color P%, ...)` form.
/// Angles follow CSS convention (0deg at 12 o'clock, clockwise); the center
/// is retained as box-relative fractions for paint-time resolution.
fn parse_conic_gradient(
    value: &str,
    dark_scheme: bool,
) -> Option<(f32, (f32, f32), Vec<([u8; 4], Option<f32>)>)> {
    let v = value.trim();
    let lower = v.to_ascii_lowercase();
    let start = lower.find("conic-gradient(")?;
    let open = start + "conic-gradient(".len();
    let end = find_matching_paren(&v[open..])? + open;
    let inner = &v[open..end];
    let parts = split_top_level(inner, ',');
    if parts.is_empty() {
        return None;
    }

    let mut angle = 0.0f32;
    let mut center = (0.5f32, 0.5f32);
    let mut stop_start = 0usize;
    let prelude = parts[0].trim().to_ascii_lowercase();
    if prelude.starts_with("from ") || prelude.starts_with("at ") {
        if let Some(from) = prelude.find("from ") {
            let token = prelude[from + 5..]
                .split_whitespace()
                .next()
                .unwrap_or_default();
            angle = parse_css_angle(token).unwrap_or(0.0).rem_euclid(360.0);
        }
        if let Some(at) = prelude.find(" at ") {
            let coords: Vec<&str> = prelude[at + 4..].split_whitespace().collect();
            if let Some(x) = coords.first().and_then(|value| percent_fraction(value)) {
                center.0 = x;
            }
            if let Some(y) = coords.get(1).and_then(|value| percent_fraction(value)) {
                center.1 = y;
            }
        } else if let Some(at) = prelude.strip_prefix("at ") {
            let coords: Vec<&str> = at.split_whitespace().collect();
            if let Some(x) = coords.first().and_then(|value| percent_fraction(value)) {
                center.0 = x;
            }
            if let Some(y) = coords.get(1).and_then(|value| percent_fraction(value)) {
                center.1 = y;
            }
        }
        stop_start = 1;
    }

    let mut stops = Vec::new();
    for part in &parts[stop_start..] {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (color, position) = split_color_stop(part);
        if let Some(color) = parse_color_for_scheme(color, dark_scheme) {
            stops.push((color, position));
        }
    }
    (stops.len() >= 2).then_some((angle, center, stops))
}

fn parse_css_angle(value: &str) -> Option<f32> {
    let value = value.trim();
    if let Some(degrees) = value.strip_suffix("deg") {
        return degrees.trim().parse::<f32>().ok();
    }
    if let Some(turns) = value.strip_suffix("turn") {
        return turns.trim().parse::<f32>().ok().map(|turns| turns * 360.0);
    }
    if let Some(gradians) = value.strip_suffix("grad") {
        return gradians.trim().parse::<f32>().ok().map(|grad| grad * 0.9);
    }
    if let Some(radians) = value.strip_suffix("rad") {
        return radians.trim().parse::<f32>().ok().map(f32::to_degrees);
    }
    None
}

fn split_color_stop(value: &str) -> (&str, Option<f32>) {
    if let Some(idx) = value.rfind(char::is_whitespace) {
        let tail = value[idx + 1..].trim();
        if let Some(percent) = tail
            .strip_suffix('%')
            .and_then(|number| number.parse::<f32>().ok())
        {
            return (value[..idx].trim(), Some((percent / 100.0).clamp(0.0, 1.0)));
        }
        if let Some(degrees) = tail
            .strip_suffix("deg")
            .and_then(|number| number.parse::<f32>().ok())
        {
            return (value[..idx].trim(), Some((degrees / 360.0).clamp(0.0, 1.0)));
        }
    }
    (value, None)
}

/// Parse `aspect-ratio` to a width/height ratio. Accepts `16 / 9`, `1.5`, and
/// the `auto <ratio>` form (the `auto` keyword alone yields `None`, meaning the
/// intrinsic ratio, which for images is filled in at layout).
fn parse_aspect_ratio(value: &str) -> Option<f32> {
    let v = value.trim();
    if v.eq_ignore_ascii_case("auto") {
        return None;
    }
    // Drop a leading/trailing `auto` keyword from the `auto <ratio>` form.
    let ratio_part: String = v
        .split_whitespace()
        .filter(|t| !t.eq_ignore_ascii_case("auto"))
        .collect::<Vec<_>>()
        .join(" ");
    if let Some((w, h)) = ratio_part.split_once('/') {
        let w: f32 = w.trim().parse().ok()?;
        let h: f32 = h.trim().parse().ok()?;
        if h > 0.0 && w > 0.0 {
            return Some(w / h);
        }
        return None;
    }
    let r: f32 = ratio_part.trim().parse().ok()?;
    (r.is_finite() && r > 0.0).then_some(r)
}

/// Extract the first `url(...)` reference from a `background`/`background-image`
/// value, unquoted. Ignores any other layers in the same shorthand (gradients,
/// `no-repeat`, etc.): we paint the referenced image, not the gradient.
fn parse_url(value: &str) -> Option<String> {
    let start = value.find("url(")? + 4;
    let mut depth = 1i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut end = None;
    for (offset, character) in value[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let inner = value[start..end?].trim();
    let unquoted = inner.trim_matches(|c| c == '"' || c == '\'');
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
}

/// `flex: none|auto|<grow>|<grow> <shrink>|<grow> <shrink> <basis>`. This is
/// the shorthand form almost all real-world flexbox CSS actually uses (far
/// more often than the flex-grow/flex-shrink longhands); leaving it
/// unhandled silently drops grow/shrink from every rule written this way.
/// flex-basis is not modeled as a distinct field from width, so a basis
/// length in the shorthand is parsed (to keep the number-only forms working)
/// but otherwise not separately applied; auto is a reasonable approximation
/// for the common case where basis is 0 or unspecified.
fn parse_flex_shorthand(style: &mut LayoutStyle, value: &str) {
    match value.trim() {
        "none" => {
            style.flex_grow = Some(0.0);
            style.flex_shrink = Some(0.0);
            style.flex_basis = crate::Dimension::Auto;
            return;
        }
        "auto" => {
            style.flex_grow = Some(1.0);
            style.flex_shrink = Some(1.0);
            style.flex_basis = crate::Dimension::Auto;
            return;
        }
        "initial" => {
            style.flex_grow = Some(0.0);
            style.flex_shrink = Some(1.0);
            style.flex_basis = crate::Dimension::Auto;
            return;
        }
        _ => {}
    }
    // Grammar: `flex: <grow> <shrink>? || <basis>`. Bare numbers are grow then
    // shrink; a token with a unit / `auto` / a third numeric is the basis
    // (e.g. `flex: 0 0 260px`, the fixed-width sidebar idiom).
    let mut numbers: Vec<f32> = Vec::new();
    let mut basis: Option<crate::Dimension> = None;
    for tok in value.split_whitespace() {
        if let Ok(n) = tok.parse::<f32>() {
            if numbers.len() < 2 {
                numbers.push(n);
            } else {
                basis = Some(dimension_value(tok));
            }
        } else {
            basis = Some(dimension_value(tok));
        }
    }
    match numbers.as_slice() {
        [grow] => {
            style.flex_grow = Some(*grow);
            style.flex_shrink = Some(1.0);
        }
        [grow, shrink, ..] => {
            style.flex_grow = Some(*grow);
            style.flex_shrink = Some(*shrink);
        }
        [] => {}
    }
    // Explicit basis wins; otherwise numbers-only shorthand implies basis 0
    // (per spec `flex: 1` == `1 1 0%`), while a bare basis keeps grow/shrink 1.
    style.flex_basis = match basis {
        Some(b) => b,
        None if !numbers.is_empty() => crate::Dimension::Px(0.0),
        None => {
            style.flex_grow = Some(1.0);
            style.flex_shrink = Some(1.0);
            crate::Dimension::Auto
        }
    };
}

/// Parse `flex-flow: <flex-direction> || <flex-wrap>`. The two constituents
/// may appear in either order and each may appear at most once. Shorthand
/// omission resets the other constituent to its initial value rather than
/// retaining an earlier longhand winner.
fn parse_flex_flow_shorthand(
    value: &str,
) -> Option<(taffy::FlexDirection, taffy::FlexWrap)> {
    let lower = value.trim().to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "initial" | "unset" | "revert" | "revert-layer"
    ) {
        return Some((taffy::FlexDirection::Row, taffy::FlexWrap::NoWrap));
    }

    let tokens = lower.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() || tokens.len() > 2 {
        return None;
    }
    let mut direction = None;
    let mut wrap = None;
    for token in tokens {
        let parsed_direction = match token {
            "row" => Some(taffy::FlexDirection::Row),
            "row-reverse" => Some(taffy::FlexDirection::RowReverse),
            "column" => Some(taffy::FlexDirection::Column),
            "column-reverse" => Some(taffy::FlexDirection::ColumnReverse),
            _ => None,
        };
        if let Some(parsed) = parsed_direction {
            if direction.replace(parsed).is_some() {
                return None;
            }
            continue;
        }
        let parsed_wrap = match token {
            "nowrap" => Some(taffy::FlexWrap::NoWrap),
            "wrap" => Some(taffy::FlexWrap::Wrap),
            "wrap-reverse" => Some(taffy::FlexWrap::WrapReverse),
            _ => None,
        };
        if let Some(parsed) = parsed_wrap {
            if wrap.replace(parsed).is_some() {
                return None;
            }
        } else {
            return None;
        }
    }
    Some((
        direction.unwrap_or(taffy::FlexDirection::Row),
        wrap.unwrap_or(taffy::FlexWrap::NoWrap),
    ))
}

/// `background-size: 10px` / `0.857em` / `10px 20px` -> explicit px pair.
/// Keyword values (`cover`, `contain`, `auto`) are left unhandled (`None`, the
/// "stretch to fill the box" fallback) since evaluating them needs the
/// image's own intrinsic aspect ratio, which is not known until it is
/// fetched, well after style resolution.
fn parse_background_size(value: &str) -> Option<(f32, f32)> {
    let tokens: Vec<&str> = value.split_whitespace().collect();
    match tokens.as_slice() {
        [one] => px_value(one).map(|v| (v, v)),
        [w, h] => Some((px_value(w)?, px_value(h)?)),
        _ => None,
    }
}

fn parse_background_size_fit(value: &str) -> Option<crate::ObjectFit> {
    let size = value.rsplit_once('/').map_or(value, |(_, size)| size);
    if size.split_whitespace().any(|token| token == "cover") {
        Some(crate::ObjectFit::Cover)
    } else if size.split_whitespace().any(|token| token == "contain") {
        Some(crate::ObjectFit::Contain)
    } else {
        None
    }
}

fn parse_image_repeat(value: &str) -> Option<(bool, bool)> {
    let tokens: Vec<&str> = split_ws_paren(value)
        .into_iter()
        .filter(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "repeat" | "no-repeat" | "repeat-x" | "repeat-y" | "space" | "round"
            )
        })
        .collect();
    match tokens.as_slice() {
        [token, ..] if token.eq_ignore_ascii_case("repeat-x") => Some((true, false)),
        [token, ..] if token.eq_ignore_ascii_case("repeat-y") => Some((false, true)),
        [x, y, ..] => Some((
            !x.eq_ignore_ascii_case("no-repeat"),
            !y.eq_ignore_ascii_case("no-repeat"),
        )),
        [both] => {
            let repeat = !both.eq_ignore_ascii_case("no-repeat");
            Some((repeat, repeat))
        }
        _ => None,
    }
}

fn parse_background_origin(value: &str) -> Option<crate::BackgroundOrigin> {
    match value.trim().to_ascii_lowercase().as_str() {
        "border-box" => Some(crate::BackgroundOrigin::BorderBox),
        "padding-box" => Some(crate::BackgroundOrigin::PaddingBox),
        "content-box" => Some(crate::BackgroundOrigin::ContentBox),
        _ => None,
    }
}

fn parse_background_clip(value: &str) -> Option<crate::BackgroundClip> {
    match value.trim().to_ascii_lowercase().as_str() {
        "border-box" => Some(crate::BackgroundClip::BorderBox),
        "padding-box" => Some(crate::BackgroundClip::PaddingBox),
        "content-box" => Some(crate::BackgroundClip::ContentBox),
        "text" => Some(crate::BackgroundClip::Text),
        _ => None,
    }
}

/// One visual-box token in the shorthand sets both origin and clip; a second
/// token overrides only clip. Function arguments are excluded by
/// `split_ws_paren`, so a URL or gradient payload cannot be mistaken for box
/// geometry.
fn parse_background_box_shorthand(
    value: &str,
) -> Option<(crate::BackgroundOrigin, crate::BackgroundClip)> {
    let first_layer = split_top_level(value, ',').into_iter().next()?;
    let boxes = split_ws_paren(first_layer)
        .into_iter()
        .filter_map(|token| parse_background_clip(token).map(|clip| (token, clip)))
        .collect::<Vec<_>>();
    match boxes.as_slice() {
        [] => None,
        [(_, crate::BackgroundClip::Text)] => Some((
            crate::BackgroundOrigin::default(),
            crate::BackgroundClip::Text,
        )),
        [(token, clip)] => Some((parse_background_origin(token)?, *clip)),
        [(origin, _), (_, clip)] => Some((parse_background_origin(origin)?, *clip)),
        _ => None,
    }
}

fn background_size_expression(value: &str) -> Option<String> {
    let (_, size) = value.rsplit_once('/')?;
    let mut depth = 0i32;
    let mut end = size.len();
    for (index, ch) in size.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            _ if depth == 0 && ch == ',' => {
                end = index;
                break;
            }
            _ => {}
        }
    }
    let size = size[..end].trim();
    (!size.is_empty()).then(|| size.to_string())
}

/// Parse the first layer of `background-position`.
///
/// Percentages are retained as fractions of the space left after sizing the
/// image, while lengths remain start-edge offsets. This distinction is
/// essential for CSS sprites: centering a 48px image in a 24px owner moves it
/// by -12px, whereas `background-position:0` must leave it at the start edge
/// and `background-position:-24px` must select its second 24px frame.
fn parse_background_position(value: &str) -> crate::BackgroundPosition {
    use crate::{BackgroundPosition, BackgroundPositionAxis};

    let first_layer = split_top_level(value, ',')
        .into_iter()
        .next()
        .unwrap_or(value)
        .trim();
    let tokens = split_ws_paren(first_layer);
    let center = BackgroundPositionAxis::percentage(0.5);
    let start = BackgroundPositionAxis::percentage(0.0);
    let end = BackgroundPositionAxis::percentage(1.0);

    let numeric = |token: &str| {
        let token = token.trim();
        if let Some(number) = token.strip_suffix('%') {
            let percentage = number.parse::<f32>().ok()? / 100.0;
            return percentage
                .is_finite()
                .then(|| BackgroundPositionAxis::percentage(percentage));
        }
        px_value(token)
            .filter(|length| length.is_finite())
            .map(BackgroundPositionAxis::pixels)
    };
    let horizontal_keyword = |token: &str| match token {
        "left" => Some(start),
        "right" => Some(end),
        _ => None,
    };
    let vertical_keyword = |token: &str| match token {
        "top" => Some(start),
        "bottom" => Some(end),
        _ => None,
    };

    match tokens.as_slice() {
        [] => BackgroundPosition::default(),
        [one] => {
            if let Some(y) = vertical_keyword(one) {
                BackgroundPosition::new(center, y)
            } else {
                let x = horizontal_keyword(one)
                    .or_else(|| (*one == "center").then_some(center))
                    .or_else(|| numeric(one))
                    .unwrap_or(center);
                BackgroundPosition::new(x, center)
            }
        }
        [first, second] => {
            if let Some(y) = vertical_keyword(first) {
                let x = horizontal_keyword(second)
                    .or_else(|| (*second == "center").then_some(center))
                    .unwrap_or(center);
                return BackgroundPosition::new(x, y);
            }
            if let Some(x) = horizontal_keyword(first) {
                let y = vertical_keyword(second)
                    .or_else(|| (*second == "center").then_some(center))
                    .or_else(|| numeric(second))
                    .unwrap_or(center);
                return BackgroundPosition::new(x, y);
            }
            if let Some(x) = horizontal_keyword(second) {
                let y = vertical_keyword(first)
                    .or_else(|| (*first == "center").then_some(center))
                    .unwrap_or(center);
                return BackgroundPosition::new(x, y);
            }
            let x = (*first == "center")
                .then_some(center)
                .or_else(|| numeric(first))
                .unwrap_or(center);
            let y = vertical_keyword(second)
                .or_else(|| (*second == "center").then_some(center))
                .or_else(|| numeric(second))
                .unwrap_or(center);
            BackgroundPosition::new(x, y)
        }
        _ => {
            // Three/four-value syntax anchors an offset to a named edge:
            // `right 10px bottom 20px` => `calc(100% - 10px)
            // calc(100% - 20px)`.
            let mut x = None;
            let mut y = None;
            let mut index = 0;
            while index < tokens.len() {
                let token = tokens[index];
                let (axis, from_end) = match token {
                    "left" => (Some(false), false),
                    "right" => (Some(false), true),
                    "top" => (Some(true), false),
                    "bottom" => (Some(true), true),
                    "center" => {
                        if x.is_none() {
                            x = Some(center);
                        } else if y.is_none() {
                            y = Some(center);
                        }
                        index += 1;
                        continue;
                    }
                    _ => (None, false),
                };
                if let Some(vertical) = axis {
                    let offset = tokens.get(index + 1).and_then(|next| numeric(next));
                    let position = match offset {
                        Some(offset) if from_end => BackgroundPositionAxis::from_end_offset(offset),
                        Some(offset) => offset,
                        None if from_end => end,
                        None => start,
                    };
                    if vertical {
                        y = Some(position);
                    } else {
                        x = Some(position);
                    }
                    index += usize::from(offset.is_some());
                } else if let Some(position) = numeric(token) {
                    if x.is_none() {
                        x = Some(position);
                    } else if y.is_none() {
                        y = Some(position);
                    }
                }
                index += 1;
            }
            BackgroundPosition::new(x.unwrap_or(center), y.unwrap_or(center))
        }
    }
}

/// Parse a `box-shadow` value into its first layer:
/// `[inset]? <offset-x> <offset-y> <blur>? <spread>? <color>?`. The `inset`
/// keyword and the color may each lead or trail the lengths; comma-separated
/// multiples are accepted but only the first layer is stored. `current_color`
/// supplies the default when the color is omitted (CSS `currentColor`).
fn parse_box_shadow(
    value: &str,
    current_color: Option<[u8; 4]>,
    dark_scheme: bool,
) -> Option<crate::BoxShadow> {
    let v = value.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("none") {
        return None;
    }
    // Only the first comma-separated layer is modeled; split at paren depth 0 so
    // the commas inside an rgba()/hsl() color are not treated as separators.
    let layer = split_top_level(v, ',').into_iter().next()?;
    let mut inset = false;
    let mut color: Option<[u8; 4]> = None;
    let mut lengths: Vec<f32> = Vec::new();
    for tok in split_ws_paren(layer.trim()) {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        if t.eq_ignore_ascii_case("inset") {
            inset = true;
            continue;
        }
        // Try a length first so a bare `0` is an offset, not a failed color;
        // px_value rejects color tokens (`#ccc`, `red`, `rgba(...)`) so they
        // fall through to parse_color.
        if lengths.len() < 4 {
            if let Some(px) = px_value(t) {
                lengths.push(px);
                continue;
            }
        }
        if let Some(c) = parse_color_for_scheme(t, dark_scheme) {
            color = Some(c);
        }
    }
    if lengths.len() < 2 {
        return None;
    }
    Some(crate::BoxShadow {
        offset_x: lengths[0],
        offset_y: lengths[1],
        blur: lengths.get(2).copied().unwrap_or(0.0),
        spread: lengths.get(3).copied().unwrap_or(0.0),
        color: color.or(current_color).unwrap_or([0, 0, 0, 255]),
        inset,
    })
}

/// Split on ASCII whitespace at paren depth 0, so a functional color like
/// `rgba(0, 0, 0, .15)` (whose internal spaces are not token separators) stays
/// one token.
fn split_ws_paren(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() && depth == 0 {
            if let Some(st) = start.take() {
                out.push(&s[st..i]);
            }
            continue;
        }
        match c {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            _ => {}
        }
        if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_parses_inherited_state_and_supports_only_real_values() {
        assert_eq!(
            compute_style("div", Some("direction:rtl")).direction,
            Some(taffy::Direction::Rtl)
        );
        assert_eq!(
            compute_style("div", Some("direction:rtl;direction:initial")).direction,
            Some(taffy::Direction::Ltr)
        );
        assert_eq!(
            compute_style("div", Some("direction:rtl;direction:inherit")).direction,
            None
        );
        assert!(supports_declaration("direction", "rtl"));
        assert!(supports_declaration("direction", "ltr"));
        assert!(!supports_declaration("direction", "auto"));
    }

    #[test]
    fn logical_borders_resolve_final_direction_and_cascade_with_physical_sides() {
        let later_logical = compute_style(
            "div",
            Some(
                "direction:rtl;border-right:4px solid #2f9e44;\
                 border-inline-start:12px solid #862e9c",
            ),
        );
        assert_eq!(later_logical.border.right, 12.0);
        assert_eq!(later_logical.border_model.colors.right, Some([0x86, 0x2e, 0x9c, 255]));

        let later_physical = compute_style(
            "div",
            Some(
                "direction:rtl;border-inline-start:12px solid #c2255c;\
                 border-right:4px solid #0b7285",
            ),
        );
        assert_eq!(later_physical.border.right, 4.0);
        assert_eq!(later_physical.border_model.colors.right, Some([0x0b, 0x72, 0x85, 255]));

        let late_direction = compute_style(
            "div",
            Some("border-inline-start:10px solid red;direction:rtl"),
        );
        assert_eq!(late_direction.border.left, 0.0);
        assert_eq!(late_direction.border.right, 10.0);
    }

    #[test]
    fn logical_border_pairs_expand_start_end_components_and_supports_grammar() {
        let style = compute_style(
            "div",
            Some(
                "border-inline:8px solid #f08c00;\
                 border-inline-width:5px 9px;\
                 border-block:3px dashed #1971c2;\
                 border-block-color:#e03131 #2f9e44",
            ),
        );
        assert_eq!((style.border.left, style.border.right), (5.0, 9.0));
        assert_eq!((style.border.top, style.border.bottom), (3.0, 3.0));
        assert_eq!(style.border_model.styles.left, crate::BorderStyle::Solid);
        assert_eq!(style.border_model.styles.top, crate::BorderStyle::Dashed);
        assert_eq!(style.border_model.colors.top, Some([0xe0, 0x31, 0x31, 255]));
        assert_eq!(style.border_model.colors.bottom, Some([0x2f, 0x9e, 0x44, 255]));

        for (property, value) in [
            ("border-inline", "1px solid red"),
            ("border-block-width", "1px 2px"),
            ("border-inline-start-style", "dashed"),
            ("border-block-end-color", "currentcolor"),
        ] {
            assert!(supports_declaration(property, value), "{property}:{value}");
        }
        assert!(!supports_declaration("border-inline-start-width", "1px 2px"));
        assert!(!supports_declaration("border-block-color", "red blue green"));
        assert!(!supports_declaration("border-inline-style", "solid banana"));
    }

    #[test]
    fn table_layout_parses_resets_and_reports_only_supported_values() {
        let fixed = compute_style("table", Some("table-layout:fixed"));
        assert!(fixed.table_layout_fixed);

        let reset = compute_style(
            "table",
            Some("table-layout:fixed;table-layout:auto;table-layout:banana"),
        );
        assert!(!reset.table_layout_fixed);
        assert!(supports_declaration("table-layout", "fixed"));
        assert!(supports_declaration("table-layout", "auto"));
        assert!(!supports_declaration("table-layout", "fixed-ish"));
    }

    #[test]
    fn supports_reports_only_implemented_value_subsets() {
        for value in ["normal", "break-all", "keep-all", "break-word"] {
            assert!(supports_declaration("word-break", value), "{value}");
        }
        for property in ["filter", "backdrop-filter", "-webkit-backdrop-filter"] {
            assert!(supports_declaration(property, "none"), "{property}");
            assert!(!supports_declaration(property, "blur(2px)"), "{property}");
        }
        assert!(!supports_declaration("perspective", "800px"));
        assert!(!supports_declaration("contain", "paint"));
        assert!(!supports_declaration("content-visibility", "auto"));
        for content in [
            "none",
            "\"new\"",
            "attr(data-label)",
            "counter(item) '. '",
            "url(icon.svg)",
        ] {
            assert!(supports_declaration("content", content), "{content}");
        }
        assert!(!supports_declaration("content", "unknown-function(x)"));
        assert!(!supports_declaration("content", "counter(item, banana)"));
        assert!(!supports_declaration("content", "url(icon.svg) garbage()"));
        assert!(!supports_declaration("display", "grid;"));
        assert!(!supports_declaration("color", "red !important"));
        assert!(!supports_declaration("display", "banana"));
    }

    #[test]
    fn supports_variable_values_at_parse_time_for_implemented_properties() {
        for (property, value) in [
            ("grid", "var(--tw)"),
            ("color", "var(--brand-color)"),
            ("width", "calc(100% - var(--gutter))"),
            ("transform", "var(--transform, garbage(1px))"),
            ("transform", "translateX(var(--offset, garbage(1px)))"),
        ] {
            assert!(supports_declaration(property, value), "{property}:{value}");
        }
        for property in [
            "filter",
            "backdrop-filter",
            "-webkit-backdrop-filter",
            "perspective",
            "contain",
            "content-visibility",
        ] {
            assert!(
                !supports_declaration(property, "var(--effect)"),
                "{property} must not advertise an unimplemented effect"
            );
        }
    }

    #[test]
    fn supports_variable_values_reject_malformed_variable_syntax() {
        for value in [
            "var(color)",
            "var(--)",
            "var(--x,!)",
            "var(--x,foo;bar)",
            "var(--x,})",
            "calc(1px + var(x))",
        ] {
            assert!(!supports_declaration("grid", value), "{value}");
        }
        assert!(supports_declaration("--theme", "var(--base, red)"));
        assert!(!supports_declaration("--theme", "var(base)"));
        assert!(!supports_declaration("--", "red"));
    }

    #[test]
    fn transform_support_rejects_invalid_z_types_nonfinite_numbers_and_fake_math() {
        for value in [
            "translateZ(10px)",
            "translate3d(1px, 2px, 3rem)",
            "scale3d(1, 2, 3)",
            "rotate3d(0, 0, 1, 45deg)",
            "translateX(calc(10px + 5%))",
            "translateX(var(--offset))",
            "translateX(var(--offset, 10px))",
        ] {
            assert!(supports_declaration("transform", value), "{value}");
        }
        for value in [
            "translateZ(10%)",
            "translate3d(1px, 2px, 10%)",
            "scale3d(1, 2, garbage)",
            "scale3d(1, 2, NaN)",
            "scale(NaN)",
            "rotate(NaNdeg)",
            "rotate(calc(1deg / 0))",
            "rotate3d(NaN, 0, 1, 45deg)",
            "translateX(garbage(10px))",
            "translateX(calc(garbage))",
            "translateX(var(x))",
        ] {
            assert!(!supports_declaration("transform", value), "{value}");
        }
    }

    #[test]
    fn clip_path_polygon_supports_only_geometry_it_can_resolve() {
        assert!(supports_declaration(
            "clip-path",
            "polygon(0 0, 100% 0, 50% 90px)"
        ));
        assert!(supports_declaration(
            "-webkit-clip-path",
            "polygon(evenodd, 0 0, 10rem 0, 10rem 10vh) border-box"
        ));
        for unsupported in [
            "polygon(0 0, 100% 0, 50% 100%) content-box",
            "content-box polygon(0 0, 100% 0, 50% 100%)",
            "polygon(0, 100% 0)",
            "polygon(evenodd 0 0, 100% 0)",
            "polygon(0 0, calc(100% - 1px) 0, 0 100%)",
            "circle(50%)",
        ] {
            assert!(
                !supports_declaration("clip-path", unsupported),
                "@supports must not advertise unpainted clip geometry: {unsupported}"
            );
        }

        let style = compute_style(
            "div",
            Some("clip-path:polygon(evenodd, -10px 0, 100% 0, 50% 2em)"),
        );
        let polygon = style.clip_path.expect("computed polygon");
        assert_eq!(polygon.fill_rule, crate::ClipPathFillRule::Evenodd);
        assert_eq!(
            polygon.points,
            vec![
                (crate::Dimension::Px(-10.0), crate::Dimension::Px(0.0)),
                (crate::Dimension::Percent(1.0), crate::Dimension::Px(0.0)),
                (crate::Dimension::Percent(0.5), crate::Dimension::Em(2.0)),
            ]
        );
    }

    #[test]
    fn parses_display_and_size() {
        let s = compute_style("div", Some("display: flex; width: 200px; height: 50px"));
        assert_eq!(s.display, Display::Flex);
        assert_eq!(s.width, crate::Dimension::Px(200.0));
        assert_eq!(s.height, crate::Dimension::Px(50.0));

        let flow_root = compute_style("div", Some("display: flow-root"));
        assert_eq!(flow_root.display, Display::Block);
        assert!(flow_root.flow_root);

        let table = compute_style("div", Some("display:none; display:table"));
        assert_eq!(table.display, Display::Block);
        assert!(table.flow_root);
        assert!(table.is_table_box);

        let inline_table = compute_style("div", Some("display:block; display:inline-table"));
        assert_eq!(inline_table.display, Display::Inline);
        assert!(inline_table.is_inline_block);
        assert!(inline_table.flow_root);
        assert!(inline_table.is_table_box);

        let table_cell = compute_style("div", Some("display:block; display:table-cell"));
        assert_eq!(table_cell.display, Display::Flex);
        assert!(table_cell.internal_flex_container);
        assert!(table_cell.is_table_cell_box);

        let min_before = compute_style("div", Some("min-width:50px;display:table-cell"));
        let min_after = compute_style("div", Some("display:table-cell;min-width:50px"));
        assert_eq!(min_before.min_width, crate::Dimension::Px(50.0));
        assert_eq!(min_after.min_width, crate::Dimension::Px(50.0));

        let reset_table = compute_style("div", Some("display:table; display:grid"));
        assert_eq!(reset_table.display, Display::Grid);
        assert!(!reset_table.is_table_box);

        let reset_cell = compute_style("div", Some("display:table-cell; display:block"));
        assert_eq!(reset_cell.display, Display::Block);
        assert!(!reset_cell.internal_flex_container);
        assert!(!reset_cell.is_table_cell_box);
    }

    #[test]
    fn parses_multicol_count_shorthand_and_break_avoidance() {
        let shorthand = compute_style("div", Some("columns: 240px 3; break-inside: avoid-column"));
        assert_eq!(shorthand.column_count, Some(3));
        assert!(shorthand.break_inside_avoid);

        let reset = compute_style("div", Some("column-count: 4; columns: auto"));
        assert_eq!(reset.column_count, None);
    }

    #[test]
    fn animation_shorthand_parses_the_first_timing_contract() {
        let finite = compute_style(
            "div",
            Some("animation: dismiss-overlay .6s ease-out forwards"),
        );
        assert_eq!(finite.animation_name.as_deref(), Some("dismiss-overlay"));
        assert_eq!(finite.animation_timing.duration_ms, 600.0);
        assert_eq!(
            finite.animation_timing.fill_mode,
            crate::AnimationFillMode::Forwards
        );
        assert_eq!(finite.animation_timing.iteration_count, 1.0);

        let infinite = compute_style("div", Some("animation: pulse 1s linear infinite"));
        assert_eq!(infinite.animation_name.as_deref(), Some("pulse"));
        assert_eq!(infinite.animation_timing.duration_ms, 1000.0);
        assert_eq!(
            infinite.animation_timing.fill_mode,
            crate::AnimationFillMode::None
        );
        assert!(infinite.animation_timing.iteration_count.is_infinite());

        let calculated = compute_style(
            "div",
            Some(
                "animation:wave 1.2s linear infinite;\
                 animation-delay:calc(.1s * -2.5);\
                 animation-direction:alternate-reverse;\
                 animation-fill-mode:both;\
                 animation-play-state:paused",
            ),
        );
        assert_eq!(calculated.animation_timing.delay_ms, -250.0);
        assert_eq!(
            calculated.animation_timing.direction,
            crate::AnimationDirection::AlternateReverse
        );
        assert_eq!(
            calculated.animation_timing.fill_mode,
            crate::AnimationFillMode::Both
        );
        assert_eq!(
            calculated.animation_timing.play_state,
            crate::AnimationPlayState::Paused
        );
        assert_eq!(parse_animation_time_ms("calc(1s / 2 + 25ms)"), Some(525.0));
        assert_eq!(parse_animation_time_ms("1s * 2"), None);

        let reset = compute_style(
            "div",
            Some("animation:fade 2s -1s 3 reverse both paused;animation:initial"),
        );
        assert_eq!(reset.animation_name, None);
        assert_eq!(reset.animation_timing, crate::AnimationTiming::default());
    }

    #[test]
    fn display_contents_overrides_an_earlier_display_none() {
        let style = compute_style("div", Some("display:none; display:contents"));
        assert_eq!(style.display, Display::Block);
        assert!(style.display_contents);
    }

    #[test]
    fn display_css_wide_values_replace_ua_and_prior_provenance() {
        let initial = compute_style("td", Some("display:initial"));
        assert_eq!(initial.display, Display::Inline);
        assert!(!initial.internal_flex_container);
        assert!(!initial.is_inline_block);
        assert!(!initial.display_contents);

        let unset = compute_style("div", Some("display:flex;display:unset"));
        assert_eq!(unset.display, Display::Inline);
        assert!(!unset.is_inline_block);

        let inherited = compute_style("td", Some("display:contents;display:inherit"));
        assert_eq!(inherited.display, Display::Inline);
        assert!(inherited.display_inherit);
        assert!(!inherited.internal_flex_container);
        assert!(!inherited.display_contents);

        let important = compute_style("div", Some("display:block!important;display:contents"));
        assert_eq!(important.display, Display::Block);
        assert!(!important.display_contents);
    }

    #[test]
    fn authored_display_replaces_internal_flex_provenance() {
        let native_cell = compute_style("td", None);
        assert_eq!(native_cell.display, Display::Flex);
        assert!(native_cell.internal_flex_container);

        let authored_cell = compute_style("td", Some("display:flex"));
        assert_eq!(authored_cell.display, Display::Flex);
        assert!(!authored_cell.internal_flex_container);

        let invalid = compute_style("td", Some("display:bogus"));
        assert!(invalid.internal_flex_container);

        let native_image = compute_style("img", None);
        assert_eq!(native_image.display, Display::Inline);
        assert!(!native_image.is_inline_block);

        let block_image = compute_style("img", Some("display:block"));
        assert_eq!(block_image.display, Display::Block);
        assert!(!block_image.is_inline_block);
    }

    #[test]
    fn table_ua_geometry_and_border_collapse_parse() {
        let table = compute_style("table", None);
        assert_eq!(table.box_sizing, crate::BoxSizing::BorderBox);
        assert_eq!(table.border_spacing, Some((2.0, 2.0)));
        assert_eq!(table.border_collapse, Some(false));

        let cell = compute_style("td", None);
        assert_eq!(
            cell.padding,
            Edges {
                top: 1.0,
                right: 1.0,
                bottom: 1.0,
                left: 1.0,
            }
        );
        assert_eq!(cell.vertical_align, None);

        let collapsed = compute_style(
            "table",
            Some("border-spacing:8px; border-collapse:collapse"),
        );
        assert_eq!(collapsed.border_spacing, Some((8.0, 8.0)));
        assert_eq!(collapsed.border_collapse, Some(true));
    }

    #[test]
    fn button_ua_style_is_a_centered_atomic_inline_box() {
        let button = ua_style("button");
        assert_eq!(button.display, crate::Display::Inline);
        assert!(button.is_inline_block);
        assert_eq!(button.text_align, Some(taffy::AlignItems::CENTER));
        assert_eq!(button.box_sizing, crate::BoxSizing::BorderBox);
        assert_eq!(
            button.padding,
            Edges {
                top: 1.0,
                right: 6.0,
                bottom: 1.0,
                left: 6.0,
            }
        );
    }

    #[test]
    fn image_ua_style_does_not_invent_a_responsive_size_cap() {
        let image = ua_style("img");
        assert_eq!(image.display, crate::Display::Inline);
        assert!(!image.is_inline_block);
        assert_eq!(image.max_width, crate::Dimension::Auto);

        let authored = compute_style("img", Some("max-width:100%"));
        assert_eq!(authored.max_width, crate::Dimension::Percent(1.0));
    }

    #[test]
    fn border_none_clears_native_and_per_side_widths() {
        let input = compute_style("input", Some("border:none"));
        assert_eq!(input.border, Edges::default());
        assert_eq!(
            input.border_model.specified_widths,
            crate::Sides::all(crate::border::MEDIUM_BORDER_WIDTH)
        );

        let side = compute_style("div", Some("border:3px solid red;border-left:none"));
        assert_eq!(side.border.top, 3.0);
        assert_eq!(side.border.right, 3.0);
        assert_eq!(side.border.bottom, 3.0);
        assert_eq!(side.border.left, 0.0);
    }

    #[test]
    fn border_radius_expands_ellipses_and_scales_overlaps() {
        let percentage = compute_style("div", Some("border-radius:50%"));
        assert_eq!(
            percentage.border_model.radii.resolve(80.0, 40.0).top_left,
            (40.0, 20.0)
        );

        let elliptical = compute_style(
            "div",
            Some("border-radius:80px 60px 40px 20px/50px 40px 30px 10px"),
        );
        let resolved = elliptical.border_model.radii.resolve(100.0, 50.0);
        assert!((resolved.top_left.0 - 80.0 * 5.0 / 7.0).abs() < 0.001);
        assert!((resolved.bottom_right.1 - 30.0 * 5.0 / 7.0).abs() < 0.001);

        let reset = compute_style("div", Some("border-radius:50%;border-radius:revert"));
        assert!(reset.border_model.radii.is_zero());
    }

    #[test]
    fn invalid_border_declarations_retain_the_previous_cascade_value() {
        let style = compute_style(
            "div",
            Some(
                "border:4px dashed red;\
                 border-width:10%;border-style:solid nonsense;\
                 border-color:red green blue purple orange;\
                 border-radius:12px;border-radius:10px/",
            ),
        );
        assert_eq!(
            style.border,
            Edges {
                top: 4.0,
                right: 4.0,
                bottom: 4.0,
                left: 4.0
            }
        );
        assert_eq!(
            style.border_model.styles,
            crate::Sides::all(crate::BorderStyle::Dashed)
        );
        assert_eq!(
            style.border_model.colors,
            crate::Sides::all(Some([255, 0, 0, 255]))
        );
        assert_eq!(
            style.border_model.radii.top_left.x,
            crate::RadiusValue::pixels(12.0)
        );
        assert!(!supports_declaration("border-width", "10%"));
        assert!(!supports_declaration("border-width", "4"));
    }

    #[test]
    fn border_and_outline_shorthands_reset_omitted_longhands() {
        let style = compute_style(
            "div",
            Some(
                "border:10px dashed red;border:solid;\
                 outline:8px dotted blue;outline:green",
            ),
        );
        assert_eq!(
            style.border,
            Edges {
                top: 3.0,
                right: 3.0,
                bottom: 3.0,
                left: 3.0
            }
        );
        assert_eq!(
            style.border_model.styles,
            crate::Sides::all(crate::BorderStyle::Solid)
        );
        assert_eq!(style.border_model.colors, crate::Sides::all(None));
        assert_eq!(style.outline.specified_width, 3.0);
        assert_eq!(style.outline.style, crate::BorderStyle::None);
        assert_eq!(style.outline.color, Some([0, 128, 0, 255]));
    }

    #[test]
    fn item_self_alignment_parses_and_resets() {
        let aligned = compute_style("div", Some("align-self:safe center;justify-self:flex-end"));
        assert_eq!(aligned.align_self, Some(taffy::AlignSelf::SAFE_CENTER));
        assert_eq!(aligned.justify_self, Some(taffy::JustifySelf::FLEX_END));

        let reset = compute_style(
            "div",
            Some("align-self:center;align-self:auto;justify-self:end;justify-self:auto"),
        );
        assert_eq!(reset.align_self, None);
        assert_eq!(reset.justify_self, None);

        let normal = compute_style("div", Some("align-self:normal;justify-self:normal"));
        assert_eq!(normal.align_self, Some(taffy::AlignSelf::NORMAL));
        assert_eq!(normal.justify_self, Some(taffy::JustifySelf::NORMAL));

        let shorthand = compute_style("div", Some("place-self:safe center flex-end"));
        assert_eq!(shorthand.align_self, Some(taffy::AlignSelf::SAFE_CENTER));
        assert_eq!(shorthand.justify_self, Some(taffy::JustifySelf::FLEX_END));

        let parent = compute_style(
            "div",
            Some("align-items:start;justify-items:safe end;place-items:end center"),
        );
        assert_eq!(parent.align_items, Some(taffy::AlignItems::END));
        assert_eq!(parent.justify_items, Some(taffy::JustifyItems::CENTER));

        let content = compute_style(
            "div",
            Some("align-content:space-between;place-content:safe center end"),
        );
        assert_eq!(
            content.align_content,
            Some(taffy::AlignContent::SAFE_CENTER)
        );
        assert_eq!(content.justify_content, Some(taffy::JustifyContent::END));
    }

    #[test]
    fn font_shorthand_expands_layout_fields_and_resets_omissions() {
        let s = compute_style(
            "div",
            Some(
                "font-style:italic;font-weight:bold;line-height:2;\
                 font:normal small-caps 500 64px/60px \"Google Sans\", sans-serif",
            ),
        );
        assert_eq!(s.font_size, Some(64.0));
        assert_eq!(s.line_height, Some(crate::LineHeight::Px(60.0)));
        assert_eq!(s.font_weight.as_deref(), Some("500"));
        assert_eq!(s.font_style_italic, Some(false));
        assert_eq!(
            s.font_family.as_deref(),
            Some("\"google sans\", sans-serif")
        );

        let reset = compute_style(
            "div",
            Some("font-style:italic;font-weight:bold;line-height:2;font:20px Arial"),
        );
        assert_eq!(reset.font_size, Some(20.0));
        assert_eq!(reset.line_height, Some(crate::LineHeight::Normal));
        assert_eq!(reset.font_weight.as_deref(), Some("400"));
        assert_eq!(reset.font_style_italic, Some(false));
    }

    #[test]
    fn font_weight_preserves_numeric_values_and_resolves_relative_keywords() {
        let medium = compute_style("div", Some("font-weight:500"));
        assert_eq!(medium.font_weight.as_deref(), Some("500"));
        let semibold = compute_style("div", Some("font-weight:600"));
        assert_eq!(semibold.font_weight.as_deref(), Some("600"));
        let normal = compute_style("strong", Some("font-weight:normal"));
        assert_eq!(normal.font_weight.as_deref(), Some("400"));

        assert_eq!(computed_font_weight(Some("bolder"), 99), 400);
        assert_eq!(computed_font_weight(Some("bolder"), 349), 400);
        assert_eq!(computed_font_weight(Some("bolder"), 350), 700);
        assert_eq!(computed_font_weight(Some("bolder"), 550), 900);
        assert_eq!(computed_font_weight(Some("bolder"), 900), 900);
        assert_eq!(computed_font_weight(Some("lighter"), 99), 99);
        assert_eq!(computed_font_weight(Some("lighter"), 100), 100);
        assert_eq!(computed_font_weight(Some("lighter"), 350), 100);
        assert_eq!(computed_font_weight(Some("lighter"), 550), 400);
        assert_eq!(computed_font_weight(Some("lighter"), 750), 700);
        assert_eq!(computed_font_weight(Some("lighter"), 900), 700);
    }

    #[test]
    fn font_weight_css_wide_keywords_override_heading_ua_weight() {
        let inherited = compute_style("h1", Some("font-weight:inherit"));
        assert_eq!(inherited.font_weight.as_deref(), Some("inherit"));
        assert_eq!(
            computed_font_weight(inherited.font_weight.as_deref(), 500),
            500
        );

        let unset = compute_style("h1", Some("font-weight:unset"));
        assert_eq!(unset.font_weight.as_deref(), Some("inherit"));
        assert_eq!(computed_font_weight(unset.font_weight.as_deref(), 500), 500);

        let initial = compute_style("h1", Some("font-weight:initial"));
        assert_eq!(initial.font_weight.as_deref(), Some("400"));
        assert_eq!(
            computed_font_weight(initial.font_weight.as_deref(), 500),
            400
        );
    }

    #[test]
    fn variable_font_properties_parse_canonically_and_atomically() {
        let style = compute_style(
            "span",
            Some(
                r#"font-optical-sizing:none;
                   font-variation-settings:"wght" 500, "\6f psz" 14, "wght" 650"#,
            ),
        );
        assert_eq!(
            style.font_optical_sizing,
            Some(crate::FontOpticalSizing::None)
        );
        assert_eq!(
            style.font_variation_settings.as_deref(),
            Some(
                [
                    crate::FontVariationSetting {
                        tag: *b"opsz",
                        value: 14.0,
                    },
                    crate::FontVariationSetting {
                        tag: *b"wght",
                        value: 650.0,
                    },
                ]
                .as_slice()
            )
        );

        let case_sensitive = compute_style(
            "span",
            Some(r#"font-variation-settings:"wght" 400, "WGHT" 700"#),
        );
        assert_eq!(
            case_sensitive.font_variation_settings.as_deref(),
            Some(
                [
                    crate::FontVariationSetting {
                        tag: *b"WGHT",
                        value: 700.0,
                    },
                    crate::FontVariationSetting {
                        tag: *b"wght",
                        value: 400.0,
                    },
                ]
                .as_slice()
            )
        );

        for malformed in [
            r#""abc" 1"#,
            r#"wght 1"#,
            r#""wght" 1,"#,
            r#""wght" calc(1px)"#,
            r#""wght" 1e999"#,
            r#""wégt" 1"#,
        ] {
            let css =
                format!(r#"font-variation-settings:"opsz" 20;font-variation-settings:{malformed}"#);
            let unchanged = compute_style("span", Some(&css));
            assert_eq!(
                unchanged.font_variation_settings.as_deref(),
                Some(
                    [crate::FontVariationSetting {
                        tag: *b"opsz",
                        value: 20.0,
                    }]
                    .as_slice()
                ),
                "invalid declaration was not atomic: {malformed}"
            );
        }
    }

    #[test]
    fn variable_font_css_wide_values_and_font_shorthand_obey_cascade() {
        let inherited = compute_style(
            "span",
            Some(
                r#"font-optical-sizing:none;font-optical-sizing:unset;
                   font-variation-settings:"wght" 700;font-variation-settings:inherit"#,
            ),
        );
        assert_eq!(inherited.font_optical_sizing, None);
        assert_eq!(inherited.font_variation_settings, None);

        let reset = compute_style(
            "span",
            Some(
                r#"font-optical-sizing:none;font-optical-sizing:initial;
                   font-variation-settings:"wght" 700;font-variation-settings:normal"#,
            ),
        );
        assert_eq!(
            reset.font_optical_sizing,
            Some(crate::FontOpticalSizing::Auto)
        );
        assert_eq!(reset.font_variation_settings, Some(Vec::new()));

        let reverted = compute_style(
            "span",
            Some(
                r#"font-optical-sizing:none;font-optical-sizing:revert;
                   font-variation-settings:"wght" 700;font-variation-settings:revert-layer"#,
            ),
        );
        assert_eq!(reverted.font_optical_sizing, None);
        assert_eq!(reverted.font_variation_settings, None);

        let shorthand = compute_style(
            "span",
            Some(
                r#"font-optical-sizing:none;font-variation-settings:"opsz" 22;
                   font:italic 500 20px/1.2 Inter"#,
            ),
        );
        assert_eq!(
            shorthand.font_optical_sizing,
            Some(crate::FontOpticalSizing::Auto)
        );
        assert_eq!(shorthand.font_variation_settings, Some(Vec::new()));

        assert!(supports_declaration("font-optical-sizing", "auto"));
        assert!(!supports_declaration("font-optical-sizing", "enabled"));
        assert!(supports_declaration(
            "font-variation-settings",
            r#""opsz" 18, "wght" 500"#
        ));
        assert!(supports_declaration(
            "font-variation-settings",
            r#""opsz" calc(18)"#
        ));

        let calculated = compute_style(
            "span",
            Some(r#"font-variation-settings:"opsz" calc(10 * 2), "wght" max(400, 500)"#),
        );
        assert_eq!(
            calculated.font_variation_settings.as_deref(),
            Some(
                [
                    crate::FontVariationSetting {
                        tag: *b"opsz",
                        value: 20.0,
                    },
                    crate::FontVariationSetting {
                        tag: *b"wght",
                        value: 500.0,
                    },
                ]
                .as_slice()
            )
        );
        assert!(!supports_declaration(
            "font-variation-settings",
            r#""opsz" calc(18px)"#
        ));
    }

    #[test]
    fn letter_spacing_preserves_units_resets_and_invalid_cascade_values() {
        let relative = compute_style("span", Some("letter-spacing:-.05em"));
        assert_eq!(
            relative.letter_spacing_raw,
            Some(crate::Dimension::Em(-0.05))
        );
        assert_eq!(relative.letter_spacing_non_normal, Some(true));

        let reset = compute_style("span", Some("letter-spacing:4px;letter-spacing:normal"));
        assert_eq!(reset.letter_spacing, Some(0.0));
        assert_eq!(reset.letter_spacing_non_normal, Some(false));

        let invalid = compute_style(
            "span",
            Some("letter-spacing:3px;letter-spacing:calc(10% + 1px)"),
        );
        assert_eq!(invalid.letter_spacing, Some(3.0));
        assert_eq!(invalid.letter_spacing_non_normal, Some(true));

        let inherited = compute_style("span", Some("letter-spacing:unset"));
        assert_eq!(inherited.letter_spacing, None);
        assert_eq!(inherited.letter_spacing_non_normal, None);
    }

    #[test]
    fn text_indent_preserves_lengths_percentages_and_inherited_resets() {
        let relative = compute_style("p", Some("text-indent:2em"));
        assert_eq!(relative.text_indent, Some(crate::Dimension::Em(2.0)));

        let percentage = compute_style("p", Some("text-indent:25%"));
        assert_eq!(
            percentage.text_indent,
            Some(crate::Dimension::Percent(0.25))
        );

        let inherited = compute_style("p", Some("text-indent:18px;text-indent:unset"));
        assert_eq!(inherited.text_indent, None);
        let initial = compute_style("p", Some("text-indent:18px;text-indent:initial"));
        assert_eq!(initial.text_indent, Some(crate::Dimension::Px(0.0)));

        let invalid = compute_style("p", Some("text-indent:18px;text-indent:10px hanging"));
        assert_eq!(invalid.text_indent, Some(crate::Dimension::Px(18.0)));
        assert!(supports_declaration("text-indent", "-9999px"));
        assert!(supports_declaration("text-indent", "25%"));
        assert!(!supports_declaration("text-indent", "10px hanging"));
    }

    #[test]
    fn truncation_properties_parse_strictly_and_share_support_truth() {
        assert!(supports_declaration("text-overflow", "clip"));
        assert!(supports_declaration("text-overflow", "ellipsis"));
        assert!(!supports_declaration("text-overflow", "clip ellipsis"));
        assert!(!supports_declaration("text-overflow", "fade"));

        for value in ["1", "2", "4294967295", "999999999999999999999", "none"] {
            assert!(supports_declaration("-webkit-line-clamp", value), "{value}");
        }
        for value in ["0", "-1", "1.5", "2px", ""] {
            assert!(
                !supports_declaration("-webkit-line-clamp", value),
                "{value}"
            );
        }
        for value in ["horizontal", "vertical", "inline-axis", "block-axis"] {
            assert!(supports_declaration("-webkit-box-orient", value), "{value}");
        }
        assert!(supports_declaration("display", "-webkit-box"));
        assert!(supports_declaration("display", "-webkit-inline-box"));

        let invalid = compute_style(
            "div",
            Some("text-overflow:ellipsis;text-overflow:fade;-webkit-line-clamp:2;-webkit-line-clamp:0"),
        );
        assert_eq!(invalid.text_overflow, crate::TextOverflow::Ellipsis);
        assert_eq!(invalid.webkit_line_clamp, Some(2));

        let saturated = compute_style("div", Some("-webkit-line-clamp:999999999999999999999"));
        assert_eq!(saturated.webkit_line_clamp, Some(i32::MAX as u32));
    }

    #[test]
    fn webkit_clamp_display_adjustment_is_declaration_order_independent() {
        for css in [
            "display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:2",
            "-webkit-line-clamp:2;display:-webkit-box;-webkit-box-orient:block-axis",
            "-webkit-box-orient:vertical;-webkit-line-clamp:2;display:-webkit-box",
        ] {
            let style = compute_style("div", Some(css));
            assert_eq!(style.display, crate::Display::Block, "{css}");
            assert!(style.flow_root, "{css}");
            assert!(!style.is_inline_block, "{css}");
        }

        let inline = compute_style(
            "span",
            Some("display:-webkit-inline-box;-webkit-box-orient:vertical;-webkit-line-clamp:2"),
        );
        assert_eq!(inline.display, crate::Display::Block);
        assert!(inline.flow_root);
        assert!(inline.is_inline_block);

        let horizontal = compute_style(
            "div",
            Some("display:-webkit-box;-webkit-box-orient:horizontal;-webkit-line-clamp:2"),
        );
        assert_eq!(horizontal.display, crate::Display::Flex);
        assert!(!horizontal.flow_root);

        let cleared = compute_style(
            "div",
            Some("display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:2;-webkit-line-clamp:none"),
        );
        assert_eq!(cleared.display, crate::Display::Flex);
        assert!(!cleared.flow_root);
    }

    #[test]
    fn text_break_longhands_parse_alias_and_css_wide_values() {
        let values = compute_style(
            "div",
            Some(
                "overflow-wrap:break-word;\
                 word-wrap:anywhere;\
                 word-break:break-all",
            ),
        );
        assert_eq!(values.overflow_wrap, Some(crate::OverflowWrap::Anywhere));
        assert_eq!(values.word_break, Some(crate::WordBreak::BreakAll));

        let legacy = compute_style("div", Some("word-break:break-word"));
        assert_eq!(legacy.word_break, Some(crate::WordBreak::BreakWord));

        for keyword in ["inherit", "unset", "revert", "revert-layer"] {
            let style = compute_style(
                "div",
                Some(&format!(
                    "overflow-wrap:anywhere;overflow-wrap:{keyword};\
                     word-break:break-all;word-break:{keyword}"
                )),
            );
            assert_eq!(style.overflow_wrap, None, "{keyword}");
            assert_eq!(style.word_break, None, "{keyword}");
        }
        let initial = compute_style(
            "div",
            Some(
                "overflow-wrap:anywhere;overflow-wrap:initial;\
                 word-break:break-all;word-break:initial",
            ),
        );
        assert_eq!(initial.overflow_wrap, Some(crate::OverflowWrap::Normal));
        assert_eq!(initial.word_break, Some(crate::WordBreak::Normal));

        for property in ["overflow-wrap", "word-wrap"] {
            assert!(supports_declaration(property, "normal"));
            assert!(supports_declaration(property, "break-word"));
            assert!(supports_declaration(property, "anywhere"));
            assert!(!supports_declaration(property, "break-all"));
        }
        for value in ["normal", "break-all", "keep-all", "break-word"] {
            assert!(supports_declaration("word-break", value), "{value}");
        }
        assert!(!supports_declaration("word-break", "anywhere"));
    }

    #[test]
    fn nonzero_unitless_font_size_is_invalid() {
        let inherited = compute_style("div", Some("font-size:14px;font-size:.813"));
        assert_eq!(inherited.font_size, Some(14.0));

        let zero = compute_style("div", Some("font-size:0"));
        assert_eq!(zero.font_size, Some(0.0));
    }

    #[test]
    fn containing_block_property_triggers_are_independent() {
        let s = compute_style("div", Some("transform:rotate(0deg);filter:none"));
        assert!(s.establishes_positioning_containing_block());

        let s = compute_style("div", Some("filter:blur(0);transform:none"));
        assert!(s.establishes_positioning_containing_block());

        let s = compute_style(
            "div",
            Some("contain:layout;content-visibility:visible;filter:none"),
        );
        assert!(s.establishes_positioning_containing_block());

        let s = compute_style(
            "div",
            Some("contain:none;content-visibility:visible;filter:none;perspective:none"),
        );
        assert!(!s.establishes_positioning_containing_block());
    }

    #[test]
    fn container_properties_and_shorthand_preserve_values() {
        let defaults = compute_style("div", None);
        assert_eq!(defaults.container_type, crate::ContainerType::Normal);
        assert!(defaults.container_names.is_empty());

        let longhands = compute_style(
            "div",
            Some("container-name:main sidebar;container-type:inline-size"),
        );
        assert_eq!(longhands.container_type, crate::ContainerType::InlineSize);
        assert_eq!(longhands.container_names, ["main", "sidebar"]);

        let shorthand = compute_style(
            "div",
            Some("container-name:old;container-type:size;container:main/inline-size"),
        );
        assert_eq!(shorthand.container_type, crate::ContainerType::InlineSize);
        assert_eq!(shorthand.container_names, ["main"]);
        let name_only = compute_style("div", Some("container-type:size;container:card"));
        assert_eq!(name_only.container_type, crate::ContainerType::Normal);

        let inherited = compute_style("div", Some("container:outer/size;container:inherit"));
        assert!(inherited.container_type_inherit);
        assert!(inherited.container_names_inherit);
        let reset = compute_style(
            "div",
            Some("container:outer/size;container:inherit;container:none"),
        );
        assert!(!reset.container_type_inherit);
        assert!(!reset.container_names_inherit);
    }

    #[test]
    fn container_supports_validation_is_typed_and_shorthand_atomic() {
        let style = compute_style(
            "div",
            Some("container:main/inline-size;container:broken/unknown;container-name:also not"),
        );
        assert_eq!(style.container_type, crate::ContainerType::InlineSize);
        assert_eq!(style.container_names, ["main"]);
        assert!(!style.establishes_positioning_containing_block());
        assert!(supports_declaration("container-type", "inline-size"));
        assert!(!supports_declaration("container-type", "inline"));
        assert!(supports_declaration("container-name", "main sidebar"));
        assert!(!supports_declaration("container-name", "main not"));
        assert!(supports_declaration("container", "main/inline-size"));
        assert!(!supports_declaration("container", "main/unknown"));
    }

    #[test]
    fn margin_shorthand_expands() {
        let s = compute_style("div", Some("margin: 10px 20px"));
        assert_eq!(
            s.margin,
            Edges {
                top: 10.0,
                right: 20.0,
                bottom: 10.0,
                left: 20.0
            }
        );
    }

    #[test]
    fn border_width_accepts_css_math_tokens() {
        let style = compute_style(
            "button",
            Some(
                "border-style:solid;\
                 border-width:calc(1 * 1px);\
                 border-color:rgba(208,217,251,.4)",
            ),
        );
        assert_eq!(
            style.border,
            Edges {
                top: 1.0,
                right: 1.0,
                bottom: 1.0,
                left: 1.0,
            }
        );
        assert_eq!(
            style.border_model.colors,
            crate::Sides::all(Some([208, 217, 251, 102]))
        );

        let asymmetric = compute_style(
            "div",
            Some("border-style:solid;border-width:calc(1px * 2) 3px calc(2px + 2px) 5px"),
        );
        assert_eq!(
            asymmetric.border,
            Edges {
                top: 2.0,
                right: 3.0,
                bottom: 4.0,
                left: 5.0,
            }
        );
    }

    #[test]
    fn longhand_overrides_shorthand() {
        let s = compute_style("div", Some("padding: 5px; padding-left: 30px"));
        assert_eq!(s.padding.top, 5.0);
        assert_eq!(s.padding.left, 30.0);
    }

    #[test]
    fn percentage_padding_recorded_not_pixelized() {
        // A percentage padding must be deferred (recorded as a 0..1 fraction),
        // not eagerly converted to a bogus px value: it resolves against the
        // containing block width later, in the top-down pass.
        let s = compute_style("div", Some("padding-top: 56.25%"));
        assert_eq!(s.padding_percent[0], Some(0.5625));
        assert_eq!(s.padding.top, 0.0);

        // The shorthand splits per side, so a mix of length and percent lands
        // in the right buckets.
        let s = compute_style("div", Some("padding: 10px 25%"));
        assert_eq!(s.padding.top, 10.0);
        assert_eq!(s.padding_percent[0], None);
        assert_eq!(s.padding_percent[1], Some(0.25));
    }

    #[test]
    fn percentage_margin_recorded() {
        let s = compute_style("div", Some("margin-left: 10%"));
        assert_eq!(s.margin_percent[3], Some(0.1));
        assert!(!s.margin_auto[3]);
    }

    #[test]
    fn relative_box_edges_remain_unresolved() {
        let s = compute_style(
            "div",
            Some("font-size:20px;margin:15vh auto 2em 10vw;padding:1rem 2vmin"),
        );
        assert_eq!(s.margin_relative[0], Some(crate::Dimension::Vh(15.0)));
        assert!(s.margin_auto[1]);
        assert_eq!(s.margin_relative[2], Some(crate::Dimension::Em(2.0)));
        assert_eq!(s.margin_relative[3], Some(crate::Dimension::Vw(10.0)));
        assert_eq!(s.padding_relative[0], Some(crate::Dimension::Rem(1.0)));
        assert_eq!(s.padding_relative[1], Some(crate::Dimension::Vmin(2.0)));
    }

    #[test]
    fn logical_insets_map_to_ltr_physical_edges() {
        let style = compute_style(
            "div",
            Some(
                "inset-inline:10px 20%;\
                 inset-block:1rem calc(100vh - 2px);\
                 inset-inline-start:3px",
            ),
        );

        assert_eq!(style.inset[3], Some(crate::Dimension::Px(3.0)));
        assert_eq!(style.inset[1], Some(crate::Dimension::Percent(0.2)));
        assert_eq!(style.inset[0], Some(crate::Dimension::Rem(1.0)));
        assert!(style.inset[2].is_none());
        assert_eq!(
            style.inset_expressions[2].as_deref(),
            Some("calc(100vh - 2px)")
        );
        assert!(supports_declaration("inset-inline", "0"));
        assert!(supports_declaration("inset-block-end", "2rem"));
    }

    #[test]
    fn logical_sizes_share_horizontal_physical_size_state() {
        let logical_last = compute_style(
            "div",
            Some(
                "width:11px;inline-size:12px;\
                 height:21px;block-size:22px;\
                 min-width:31px;min-inline-size:32px;\
                 min-height:41px;min-block-size:42px;\
                 max-width:51px;max-inline-size:52px;\
                 max-height:61px;max-block-size:62px",
            ),
        );
        assert_eq!(logical_last.width, crate::Dimension::Px(12.0));
        assert_eq!(logical_last.height, crate::Dimension::Px(22.0));
        assert_eq!(logical_last.min_width, crate::Dimension::Px(32.0));
        assert_eq!(logical_last.min_height, crate::Dimension::Px(42.0));
        assert_eq!(logical_last.max_width, crate::Dimension::Px(52.0));
        assert_eq!(logical_last.max_height, crate::Dimension::Px(62.0));

        let physical_last = compute_style(
            "div",
            Some("inline-size:70px;width:71px;block-size:80px;height:81px"),
        );
        assert_eq!(physical_last.width, crate::Dimension::Px(71.0));
        assert_eq!(physical_last.height, crate::Dimension::Px(81.0));

        let deferred = compute_style(
            "div",
            Some(
                "inline-size:calc(50vw - 10px);\
                 block-size:20vh;\
                 min-inline-size:2rem;\
                 max-block-size:calc(100vh - 5px)",
            ),
        );
        assert_eq!(
            deferred.size_expressions[0].as_deref(),
            Some("calc(50vw - 10px)")
        );
        assert_eq!(deferred.height, crate::Dimension::Vh(20.0));
        assert_eq!(deferred.min_width, crate::Dimension::Rem(2.0));
        assert_eq!(
            deferred.size_expressions[5].as_deref(),
            Some("calc(100vh - 5px)")
        );

        for name in [
            "inline-size",
            "block-size",
            "min-inline-size",
            "min-block-size",
            "max-inline-size",
            "max-block-size",
        ] {
            assert!(supports_declaration(name, "10px"), "{name}");
        }
        assert!(supports_declaration("inline-size", "fit-content"));
        assert!(!supports_declaration("inline-size", "definitely-invalid"));
    }

    #[test]
    fn counter_properties_parse_ordered_name_integer_pairs() {
        let style = compute_style(
            "div",
            Some(
                "counter-reset:chapter 2 line;\
                 counter-increment:chapter line 3;\
                 counter-set:folio 9",
            ),
        );
        let pairs = |pairs: &[crate::CounterDirective]| {
            pairs
                .iter()
                .map(|pair| (pair.name.clone(), pair.value))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            pairs(&style.counter_reset),
            [("chapter".to_string(), 2), ("line".to_string(), 0)]
        );
        assert_eq!(
            pairs(&style.counter_increment),
            [("chapter".to_string(), 1), ("line".to_string(), 3)]
        );
        assert_eq!(pairs(&style.counter_set), [("folio".to_string(), 9)]);
        assert!(supports_declaration("counter-reset", "line"));
        assert!(supports_declaration("counter-increment", "line 2"));

        let cleared = compute_style("div", Some("counter-reset:line 4;counter-reset:none"));
        assert!(cleared.counter_reset.is_empty());
    }

    #[test]
    fn responsive_grid_repetitions_and_shorthand_remain_typed() {
        let auto = compute_style(
            "div",
            Some("display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr))"),
        );
        assert!(matches!(
            auto.grid_template_columns.as_slice(),
            [taffy::GridTemplateComponent::Repeat(
                taffy::GridTemplateRepetition {
                    count: taffy::RepetitionCount::AutoFit,
                    ..
                }
            )]
        ));

        let shorthand = compute_style("div", Some("display:grid;grid:auto-flow/repeat(3,1fr)"));
        assert_eq!(shorthand.grid_auto_flow, Some(taffy::GridAutoFlow::Row));
        assert_eq!(shorthand.grid_template_columns.len(), 3);
    }

    #[test]
    fn grid_track_math_is_deferred_to_the_used_axis_basis() {
        let style = compute_style(
            "div",
            Some(
                "grid-template-columns:\
                 minmax(0,calc((100% - 84rem)/2)) 1fr \
                 minmax(0,max(10px,min(calc(25% - 5px),clamp(20px,10%,200px))))",
            ),
        );
        let owners = &style.grid_calc_expressions[0];
        assert_eq!(owners.len(), 2);

        set_grid_calc_context(&style, 16.0, 16.0, 14.4, 10.0);
        let first = std::sync::Arc::as_ptr(&owners[0]).cast();
        assert!((resolve_grid_calc(first, 1440.0) - 48.0).abs() < 0.01);
        let nested = std::sync::Arc::as_ptr(&owners[1]).cast();
        assert!((resolve_grid_calc(nested, 1000.0) - 100.0).abs() < 0.01);
    }

    #[test]
    fn winning_grid_track_declaration_releases_overridden_calc_owners() {
        let style = compute_style(
            "div",
            Some(
                "grid-template-columns:calc(50% - 10px);\
                 grid-template-rows:calc(25% - 5px);\
                 grid-template-columns:1fr",
            ),
        );
        assert!(style.grid_calc_expressions[0].is_empty());
        // A non-minmax breadth supplies both the minimum and maximum sizing
        // function, each with its own stable opaque handle.
        assert_eq!(style.grid_calc_expressions[1].len(), 2);
    }

    #[test]
    fn inherited_grid_calc_preserves_parent_em_context() {
        let parent = compute_style("div", Some("grid-auto-columns:calc(1em + 10%)"));
        set_grid_calc_context(&parent, 20.0, 16.0, 10.0, 10.0);

        let mut child = compute_style("div", Some("grid-auto-columns:inherit"));
        child.grid_auto_columns = parent.grid_auto_columns.clone();
        child.grid_calc_expressions[2] = parent.grid_calc_expressions[2].clone();
        set_grid_calc_context(&child, 40.0, 16.0, 10.0, 10.0);

        let handle = std::sync::Arc::as_ptr(&child.grid_calc_expressions[2][0]).cast();
        assert!((resolve_grid_calc(handle, 100.0) - 30.0).abs() < 0.01);
    }

    #[test]
    fn implicit_grid_track_lists_parse_and_css_wide_values_reset_them() {
        let tracks = compute_style(
            "div",
            Some(
                "grid-auto-columns:50px minmax(20px,1fr);\
                 grid-auto-rows:min-content 25%;",
            ),
        );
        assert_eq!(tracks.grid_auto_columns.len(), 2);
        assert_eq!(tracks.grid_auto_rows.len(), 2);
        assert!(supports_declaration(
            "grid-auto-columns",
            "50px minmax(20px,1fr)"
        ));
        assert!(supports_declaration("grid-auto-rows", "min-content 25%"));
        assert!(!supports_declaration("grid-auto-columns", "repeat(2,50px)"));

        for keyword in ["initial", "unset", "revert", "revert-layer"] {
            let reset = compute_style(
                "div",
                Some(&format!(
                    "grid-auto-columns:50px;grid-auto-columns:{keyword};\
                     grid-auto-rows:60px;grid-auto-rows:{keyword}"
                )),
            );
            assert!(
                reset.grid_auto_columns.is_empty() && reset.grid_auto_rows.is_empty(),
                "{keyword} must restore the initial automatic implicit track"
            );
            assert!(!reset.grid_auto_columns_inherit);
            assert!(!reset.grid_auto_rows_inherit);
        }

        let inherited = compute_style(
            "div",
            Some("grid-auto-columns:inherit;grid-auto-rows:inherit"),
        );
        assert!(inherited.grid_auto_columns_inherit);
        assert!(inherited.grid_auto_rows_inherit);
    }

    #[test]
    fn grid_placement_longhands_preserve_the_opposite_side() {
        let style = compute_style(
            "div",
            Some(
                "grid-column-start:2;grid-column-end:span 4;\
                 grid-row-start:3;grid-row-end:5",
            ),
        );
        assert_eq!(
            style.grid_column,
            Some(taffy::Line {
                start: taffy::style_helpers::line(2),
                end: taffy::style_helpers::span(4),
            })
        );
        assert_eq!(
            style.grid_row,
            Some(taffy::Line {
                start: taffy::style_helpers::line(3),
                end: taffy::style_helpers::line(5),
            })
        );
    }

    #[test]
    fn grid_placement_shorthand_keeps_auto_span_out_of_named_line_resolution() {
        let style = compute_style("div", Some("grid-column:auto / span 4"));
        assert_eq!(
            style.grid_column,
            Some(taffy::Line {
                start: taffy::GridPlacement::Auto,
                end: taffy::style_helpers::span(4),
            })
        );
        assert_eq!(style.grid_column_raw, None);

        let named = compute_style("div", Some("grid-column:content-start / content-end"));
        assert_eq!(
            named.grid_column_raw.as_deref(),
            Some("content-start / content-end")
        );
        assert_eq!(named.grid_column, None);

        let reset = compute_style("div", Some("grid-column:2 / span 4;grid-column:initial"));
        assert_eq!(
            reset.grid_column,
            Some(taffy::Line {
                start: taffy::GridPlacement::Auto,
                end: taffy::GridPlacement::Auto,
            })
        );
        assert_eq!(reset.grid_column_raw, None);
    }

    #[test]
    fn grid_area_expands_slash_placements_and_named_area_defaults() {
        let overlay = compute_style("div", Some("grid-area:1 / 2 / 3 / 4"));
        assert_eq!(
            overlay.grid_row,
            Some(taffy::Line {
                start: taffy::style_helpers::line(1),
                end: taffy::style_helpers::line(3),
            })
        );
        assert_eq!(
            overlay.grid_column,
            Some(taffy::Line {
                start: taffy::style_helpers::line(2),
                end: taffy::style_helpers::line(4),
            })
        );
        assert_eq!(overlay.grid_area_name, None);

        let named = compute_style("div", Some("grid-area:hero"));
        assert_eq!(named.grid_area_name.as_deref(), Some("hero"));
        assert_eq!(named.grid_row_raw.as_deref(), Some("hero / hero"));
        assert_eq!(named.grid_column_raw.as_deref(), Some("hero / hero"));

        let reset = compute_style("div", Some("grid-area:hero;grid-area:initial"));
        assert_eq!(reset.grid_area_name, None);
        assert_eq!(
            reset.grid_row,
            Some(taffy::Line {
                start: taffy::GridPlacement::Auto,
                end: taffy::GridPlacement::Auto,
            })
        );
        assert_eq!(
            reset.grid_column,
            Some(taffy::Line {
                start: taffy::GridPlacement::Auto,
                end: taffy::GridPlacement::Auto,
            })
        );

        for value in ["2 foo", "foo 2", "calc(1)"] {
            let line = compute_style("div", Some(&format!("grid-area:{value}")));
            let expected_row = format!("{value} / auto");
            assert_eq!(
                line.grid_area_name, None,
                "{value} is a grid line, not an ident-only named area"
            );
            assert_eq!(line.grid_row_raw.as_deref(), Some(expected_row.as_str()));
            assert_eq!(
                line.grid_column,
                Some(taffy::Line {
                    start: taffy::GridPlacement::Auto,
                    end: taffy::GridPlacement::Auto,
                })
            );
        }

        let one = compute_style("div", Some("grid-area:1"));
        assert_eq!(
            one.grid_row,
            Some(taffy::Line {
                start: taffy::style_helpers::line(1),
                end: taffy::GridPlacement::Auto,
            })
        );
        assert_eq!(
            one.grid_column,
            Some(taffy::Line {
                start: taffy::GridPlacement::Auto,
                end: taffy::GridPlacement::Auto,
            })
        );

        let mixed = compute_style("div", Some("grid-area:hero / 2"));
        assert_eq!(mixed.grid_row_raw.as_deref(), Some("hero / hero"));
        assert_eq!(
            mixed.grid_column,
            Some(taffy::Line {
                start: taffy::style_helpers::line(2),
                end: taffy::GridPlacement::Auto,
            })
        );

        let copied_column_ident = compute_style("div", Some("grid-area:1 / col / 3"));
        assert_eq!(
            copied_column_ident.grid_row,
            Some(taffy::Line {
                start: taffy::style_helpers::line(1),
                end: taffy::style_helpers::line(3),
            })
        );
        assert_eq!(
            copied_column_ident.grid_column_raw.as_deref(),
            Some("col / col")
        );

        let uppercase_span = compute_style("div", Some("grid-area:SPAN 2"));
        assert_eq!(
            uppercase_span.grid_row,
            Some(taffy::Line {
                start: taffy::style_helpers::span(2),
                end: taffy::GridPlacement::Auto,
            })
        );

        for invalid in [
            "0",
            "span",
            "span 0",
            "span -1",
            "foo bar",
            "1 / inherit",
            "1.5",
        ] {
            let style = compute_style(
                "div",
                Some(&format!("grid-area:7 / 8 / 9 / 10;grid-area:{invalid}")),
            );
            assert_eq!(
                style.grid_row,
                Some(taffy::Line {
                    start: taffy::style_helpers::line(7),
                    end: taffy::style_helpers::line(9),
                }),
                "invalid `{invalid}` must not partially mutate the earlier shorthand"
            );
            assert_eq!(
                style.grid_column,
                Some(taffy::Line {
                    start: taffy::style_helpers::line(8),
                    end: taffy::style_helpers::line(10),
                }),
                "invalid `{invalid}` must not partially mutate the earlier shorthand"
            );
            assert!(!supports_declaration("grid-area", invalid));
        }
        for valid in ["hero", "1", "hero / 2", "1 / col / 3", "SPAN 2", "span foo"] {
            assert!(
                supports_declaration("grid-area", valid),
                "valid grid-area `{valid}` should pass @supports parsing"
            );
        }
    }

    #[test]
    fn contextual_css_math_uses_runtime_geometry() {
        let context = (20.0, 16.0, 9.0, 10.0, 900.0);
        assert_eq!(
            resolve_contextual_length(
                "min(25vw,350px)",
                context.0,
                context.1,
                context.2,
                context.3,
                context.4,
            ),
            Some(225.0)
        );
        assert_eq!(
            resolve_contextual_length(
                "clamp(200px,30vw,320px)",
                context.0,
                context.1,
                context.2,
                context.3,
                context.4,
            ),
            Some(270.0)
        );
        assert_eq!(
            resolve_contextual_length(
                "calc(10vw + 2rem)",
                context.0,
                context.1,
                context.2,
                context.3,
                context.4,
            ),
            Some(122.0)
        );
        assert_eq!(
            resolve_contextual_length(
                "calc(35rem*-1/4)",
                context.0,
                context.1,
                context.2,
                context.3,
                context.4,
            ),
            Some(-140.0)
        );
        assert_eq!(
            resolve_contextual_length(
                "calc(round(247px * 1, 10px))",
                context.0,
                context.1,
                context.2,
                context.3,
                context.4,
            ),
            Some(250.0)
        );
        let grouped = resolve_contextual_length(
            "calc(clamp(128px,92px + 7vw,188px) + (100vw - 48px)*43/440)",
            context.0,
            context.1,
            context.2,
            context.3,
            context.4,
        )
        .unwrap();
        assert!(
            (grouped - 238.263_64).abs() < 0.001,
            "grouped calc should preserve the nested subtraction: {grouped}"
        );
    }

    #[test]
    fn ua_defaults_and_ignore_unknown() {
        let s = compute_style("span", Some("color: red; ; bogus: ; display: none"));
        assert_eq!(s.display, Display::None);
    }

    #[test]
    fn background_clip_text_flag() {
        let s = compute_style(
            "h1",
            Some("color: transparent; -webkit-background-clip: text"),
        );
        assert!(s.background_clip_text);
        let vendor_fill = compute_style(
            "h1",
            Some(
                "color: black; -webkit-text-fill-color: transparent;\
                 background: linear-gradient(90deg, red, blue);\
                 -webkit-background-clip: text",
            ),
        );
        assert_eq!(vendor_fill.color, Some([0, 0, 0, 0]));
        assert!(vendor_fill.background_clip_text);
        assert!(vendor_fill.background_gradient.is_some());
        let legacy_angle = compute_style(
            "span",
            Some("background:-webkit-linear-gradient(315deg,#42d392 25%,#647eff)"),
        )
        .background_gradient
        .expect("prefixed gradient")
        .0;
        assert_eq!(legacy_angle, 135.0);
        let n = compute_style("h1", Some("background-clip: border-box"));
        assert!(!n.background_clip_text);
        let l = compute_style("h1", Some("background-clip: text"));
        assert!(l.background_clip_text);
    }

    #[test]
    fn light_dark_uses_light_scheme_and_validates_both_nested_branches() {
        assert_eq!(
            parse_color("light-dark(rgb(16, 32, 48), hsl(0 100% 50%))"),
            Some([16, 32, 48, 255])
        );
        assert_eq!(
            parse_color(
                "LIGHT-DARK(color-mix(in srgb, #204060 75%, transparent), \
                 light-dark(white, black))"
            ),
            Some([32, 64, 96, 191]),
            "nested commas must stay inside their color function"
        );
        let style = compute_style(
            "div",
            Some(
                "color:light-dark(#123456,#ffffff);\
                 background-color:light-dark(rgb(1, 2, 3),rgb(4, 5, 6));\
                 border-color:light-dark(red,blue)",
            ),
        );
        assert_eq!(style.color, Some([0x12, 0x34, 0x56, 255]));
        assert_eq!(style.background_color, Some([1, 2, 3, 255]));
        assert_eq!(
            style.border_model.colors,
            crate::Sides::all(Some([255, 0, 0, 255]))
        );
        assert!(supports_declaration(
            "color",
            "light-dark(rgb(1, 2, 3), color-mix(in srgb, white 50%, black))"
        ));

        for malformed in [
            "light-dark(red)",
            "light-dark(red, blue, green)",
            "light-dark(red,)",
            "light-dark(,blue)",
            "light-dark(red, rgb(1, 2, 3)",
            "light-dark(red garbage, blue)",
            "light-dark(red, definitely-not-a-color)",
            "light-dark(red, blue) trailing",
        ] {
            assert_eq!(
                parse_color(malformed),
                None,
                "malformed light-dark() must invalidate the declaration: {malformed}"
            );
            assert!(
                !supports_declaration("color", malformed),
                "@supports must reject malformed light-dark(): {malformed}"
            );
        }
    }

    #[test]
    fn background_shorthand_resets_omitted_layers() {
        let s = compute_style(
            "div",
            Some(
                "background:#1971c2 url(icon.png);background-size:20px 20px;\
                 background-position:center;background-clip:text;background:0",
            ),
        );
        assert_eq!(s.background_color, None);
        assert_eq!(s.background_gradient, None);
        assert_eq!(s.background_conic_gradient, None);
        assert!(s.background_gradient_layers.is_empty());
        assert_eq!(s.background_image, None);
        assert_eq!(s.background_size, None);
        assert_eq!(s.background_size_expression, None);
        assert_eq!(s.background_size_fit, None);
        assert_eq!(s.background_position, crate::BackgroundPosition::default());
        assert_eq!(s.background_origin, crate::BackgroundOrigin::PaddingBox);
        assert_eq!(s.background_clip, crate::BackgroundClip::BorderBox);
        assert!(!s.background_clip_text);

        let cover = compute_style(
            "div",
            Some("background:url(hero.svg) center/cover no-repeat"),
        );
        assert_eq!(cover.background_size_fit, Some(crate::ObjectFit::Cover));
        let contain = compute_style("div", Some("background-size:contain"));
        assert_eq!(contain.background_size_fit, Some(crate::ObjectFit::Contain));
        let contextual = compute_style(
            "a",
            Some("background:url(icon.svg) no-repeat 0 50% / calc(100% - 2rem) auto"),
        );
        assert_eq!(
            contextual.background_size_expression.as_deref(),
            Some("calc(100% - 2rem) auto")
        );
    }

    #[test]
    fn background_box_longhands_and_shorthand_retain_independent_geometry() {
        let longhands = compute_style(
            "div",
            Some("background-origin:content-box;background-clip:padding-box"),
        );
        assert_eq!(
            longhands.background_origin,
            crate::BackgroundOrigin::ContentBox
        );
        assert_eq!(longhands.background_clip, crate::BackgroundClip::PaddingBox);
        assert!(!longhands.background_clip_text);

        let shorthand = compute_style(
            "div",
            Some("background:linear-gradient(90deg,red,blue) content-box padding-box no-repeat"),
        );
        assert_eq!(
            shorthand.background_origin,
            crate::BackgroundOrigin::ContentBox
        );
        assert_eq!(shorthand.background_clip, crate::BackgroundClip::PaddingBox);

        let one_box = compute_style("div", Some("background:red content-box"));
        assert_eq!(
            one_box.background_origin,
            crate::BackgroundOrigin::ContentBox
        );
        assert_eq!(one_box.background_clip, crate::BackgroundClip::ContentBox);

        let text = compute_style(
            "h1",
            Some("background-origin:border-box;background-clip:text"),
        );
        assert_eq!(text.background_origin, crate::BackgroundOrigin::BorderBox);
        assert_eq!(text.background_clip, crate::BackgroundClip::Text);
        assert!(text.background_clip_text);

        assert!(supports_declaration("background-origin", "border-box"));
        assert!(supports_declaration("background-origin", "padding-box"));
        assert!(supports_declaration("background-origin", "content-box"));
        assert!(!supports_declaration("background-origin", "text"));
    }

    #[test]
    fn background_gradients_keep_authored_layer_order_and_keyword_centers() {
        let style = compute_style(
            "div",
            Some(
                "background-image:\
                 linear-gradient(180deg,transparent,white 85%),\
                 radial-gradient(ellipse at top left,red,transparent 50%),\
                 radial-gradient(ellipse at top right,blue,transparent 50%),\
                 radial-gradient(ellipse at center right,lime,transparent 50%),\
                 radial-gradient(ellipse at center left,fuchsia,transparent 50%)",
            ),
        );
        assert_eq!(style.background_gradient_layers.len(), 5);
        assert!(matches!(
            style.background_gradient_layers[0],
            crate::BackgroundGradientLayer::Linear { angle, .. }
                if (angle - 180.0).abs() < 0.001
        ));
        let centers: Vec<(f32, f32)> = style
            .background_gradient_layers
            .iter()
            .filter_map(|layer| match layer {
                crate::BackgroundGradientLayer::Radial { center, .. } => Some(*center),
                _ => None,
            })
            .collect();
        assert_eq!(
            centers,
            vec![(0.0, 0.0), (1.0, 0.0), (1.0, 0.5), (0.0, 0.5)]
        );
    }

    #[test]
    fn radial_gradients_retain_explicit_ellipse_radii_and_extent_shape() {
        let explicit = compute_style(
            "div",
            Some(
                "background:radial-gradient(141.53% 114.68% at 87.46% 55.27%,\
                 #9a7cff 36.75%,#0e0aa200 100%)",
            ),
        );
        assert_eq!(
            explicit.background_radial_gradient_geometry,
            Some(crate::RadialGradientGeometry {
                shape: crate::RadialGradientShape::Ellipse,
                size: crate::RadialGradientSize::Explicit(
                    crate::Dimension::Percent(1.4153),
                    crate::Dimension::Percent(1.1468),
                ),
            })
        );
        assert_eq!(
            explicit
                .background_radial_gradient
                .as_ref()
                .map(|value| value.0),
            Some((0.8746, 0.5527))
        );

        let keyword = compute_style(
            "div",
            Some("background-image:radial-gradient(circle closest-side at 25% 75%,red,blue)"),
        );
        assert_eq!(
            keyword.background_radial_gradient_geometry,
            Some(crate::RadialGradientGeometry {
                shape: crate::RadialGradientShape::Circle,
                size: crate::RadialGradientSize::ClosestSide,
            })
        );

        let lengths = compute_style(
            "div",
            Some("background-image:radial-gradient(80px 2em at center,#fff 0%,#000 100%)"),
        );
        assert_eq!(
            lengths.background_radial_gradient_geometry,
            Some(crate::RadialGradientGeometry {
                shape: crate::RadialGradientShape::Ellipse,
                size: crate::RadialGradientSize::Explicit(
                    crate::Dimension::Px(80.0),
                    crate::Dimension::Em(2.0),
                ),
            })
        );
    }

    #[test]
    fn radial_gradient_rejects_invalid_shape_radius_combinations() {
        for value in [
            "radial-gradient(circle 50%,red,blue)",
            "radial-gradient(ellipse 10px,red,blue)",
            "radial-gradient(circle 10px 20px,red,blue)",
            "radial-gradient(-10px 20px,red,blue)",
        ] {
            let style = compute_style("div", Some(&format!("background-image:{value}")));
            assert!(
                style.background_gradient_layers.is_empty(),
                "invalid radial geometry must not become a painted layer: {value}"
            );
        }
    }

    #[test]
    fn repeating_linear_gradient_retains_length_stops_and_repeat_axes() {
        let style = compute_style(
            "div",
            Some(
                "background-image:repeating-linear-gradient(315deg,\
                 rgba(0,0,0,.05) 0,rgba(0,0,0,.05) 1px,\
                 transparent 0,transparent 50%);\
                 background-size:10px 10px;background-repeat:repeat-x no-repeat",
            ),
        );
        assert_eq!(style.background_repeat, Some((true, false)));
        let crate::BackgroundGradientLayer::Linear {
            stop_positions,
            repeating,
            ..
        } = &style.background_gradient_layers[0]
        else {
            panic!("expected a linear gradient layer");
        };
        assert!(*repeating);
        assert_eq!(
            stop_positions,
            &[
                Some("0".to_string()),
                Some("1px".to_string()),
                Some("0".to_string()),
                Some("50%".to_string()),
            ]
        );
    }

    #[test]
    fn background_position_preserves_sprite_offsets() {
        use crate::{BackgroundPosition, BackgroundPositionAxis};

        let first = compute_style("div", Some("background-position:0"));
        assert_eq!(
            first.background_position,
            BackgroundPosition::new(
                BackgroundPositionAxis::pixels(0.0),
                BackgroundPositionAxis::percentage(0.5),
            )
        );

        let selected = compute_style("div", Some("background-position:-24px"));
        assert_eq!(
            selected.background_position,
            BackgroundPosition::new(
                BackgroundPositionAxis::pixels(-24.0),
                BackgroundPositionAxis::percentage(0.5),
            )
        );

        let edge_offsets = compute_style("div", Some("background-position:right 10px bottom 20px"));
        assert_eq!(
            edge_offsets.background_position,
            BackgroundPosition::new(
                BackgroundPositionAxis::length_percentage(-10.0, 1.0),
                BackgroundPositionAxis::length_percentage(-20.0, 1.0),
            )
        );
    }

    #[test]
    fn conic_background_and_repeated_data_svg_mask_are_preserved() {
        let style = compute_style(
            "div",
            Some(
                "background:conic-gradient(from 122deg at 50% 50%,\
                 transparent 17%,#f627e3 25%,#6911d2 32%,transparent 91%);\
                 mask-image:url(\"data:image/svg+xml,<svg viewBox='0 0 72 72'>\
                 <g transform='translate(36 36) rotate(-60)'></g></svg>\");\
                 mask-size:22px 22px;mask-repeat:repeat",
            ),
        );
        let (angle, center, stops) = style
            .background_conic_gradient
            .expect("conic gradient should parse");
        assert_eq!(angle, 122.0);
        assert_eq!(center, (0.5, 0.5));
        assert_eq!(stops.len(), 4);
        assert_eq!(style.mask_size, Some((22.0, 22.0)));
        assert_eq!(style.mask_repeat, Some((true, true)));
        let mask = style.mask_image.expect("data SVG mask should parse");
        assert!(mask.ends_with("</svg>"));
        assert!(mask.contains("rotate(-60)"));
    }

    #[test]
    fn important_and_auto() {
        let s = compute_style("div", Some("width: 100px !important; height: auto"));
        assert_eq!(s.width, crate::Dimension::Px(100.0));
        assert_eq!(s.height, crate::Dimension::Auto);
    }

    #[test]
    fn overflow_axis_coupling_recomputes_from_winning_specified_values() {
        let style = compute_style(
            "div",
            Some(
                "overflow-x:hidden;overflow-y:auto;\
                 overflow-y:visible;overflow-x:visible",
            ),
        );
        assert!(!style.overflow_clip_x);
        assert!(!style.overflow_clip_y);
        assert!(!style.overflow_hidden);
        assert!(!style.overflow_scroll_container);

        let style = compute_style("div", Some("overflow-x:clip;overflow-y:visible"));
        assert!(style.overflow_clip_x);
        assert!(!style.overflow_clip_y);
        assert!(!style.overflow_scroll_container);
    }

    #[test]
    fn overflow_rejects_invalid_values_and_handles_css_wide_keywords_atomically() {
        let style = compute_style(
            "div",
            Some(
                "overflow-x:clip;overflow-x:nonsense;\
                 overflow:hidden;overflow:hidden visible auto",
            ),
        );
        assert!(style.overflow_scroll_x && style.overflow_scroll_y);

        let reset = compute_style("div", Some("overflow:hidden;overflow:initial"));
        assert!(!reset.overflow_hidden);
        assert!(!reset.overflow_scroll_container);

        let longhand_reset = compute_style("div", Some("overflow:hidden;overflow-x:initial"));
        assert!(
            longhand_reset.overflow_scroll_x && longhand_reset.overflow_scroll_y,
            "visible/hidden must recompute to auto/hidden"
        );

        for valid in [
            ("overflow", "hidden"),
            ("overflow", "clip visible"),
            ("overflow", "initial"),
            ("overflow-x", "inherit"),
        ] {
            assert!(supports_declaration(valid.0, valid.1), "{valid:?}");
        }
        for invalid in [
            ("overflow", ""),
            ("overflow", "nonsense"),
            ("overflow", "hidden visible auto"),
            ("overflow", "inherit visible"),
            ("overflow-y", "hidden auto"),
        ] {
            assert!(!supports_declaration(invalid.0, invalid.1), "{invalid:?}");
        }
    }

    #[test]
    fn box_sizing_parses_values_and_css_wide_keywords() {
        assert_eq!(
            compute_style("div", None).box_sizing,
            crate::BoxSizing::ContentBox
        );
        assert_eq!(
            compute_style("div", Some("box-sizing:border-box")).box_sizing,
            crate::BoxSizing::BorderBox
        );
        assert_eq!(
            compute_style("div", Some("box-sizing:border-box;box-sizing:content-box")).box_sizing,
            crate::BoxSizing::ContentBox
        );
        assert_eq!(
            compute_style("div", Some("box-sizing:inherit")).box_sizing,
            crate::BoxSizing::Inherit
        );
        for keyword in ["initial", "unset", "revert", "revert-layer"] {
            assert_eq!(
                compute_style(
                    "div",
                    Some(&format!("box-sizing:border-box;box-sizing:{keyword}"))
                )
                .box_sizing,
                crate::BoxSizing::ContentBox,
                "{keyword} must not retain the preceding border-box value"
            );
        }
    }

    #[test]
    fn box_sizing_initial_restores_content_box_geometry_after_universal_reset() {
        use obscura_dom::tree_sink::parse_html;

        let tree = parse_html(
            r#"<style>
                * { box-sizing: border-box }
                body { margin: 0 }
                .parent { width: 600px }
                .form {
                    box-sizing: initial;
                    width: 100%;
                    max-width: 435px;
                    padding: 15px;
                }
            </style>
            <div class="parent"><form id="form" class="form"></form></div>"#,
        );
        let laid = crate::dom::layout_dom(&tree, (1280.0, 720.0));
        let form = tree.query_selector("#form").unwrap().unwrap();

        assert_eq!(laid.styles[&form].box_sizing, crate::BoxSizing::ContentBox);
        assert_eq!(
            laid.rects[&form].width, 465.0,
            "435px content max-width plus 15px padding on both sides"
        );
    }

    #[test]
    fn calc_with_multiply_and_divide() {
        // The exact shape MediaWiki uses to offset a TOC toggle button into
        // the left margin: a negative product divided by a constant.
        assert_eq!(resolve_length("calc(-1 * 22px / 2)"), Some(-11.0));
        // Minifiers commonly remove every optional space. A sign immediately
        // following `*` or `/` is unary and belongs to that factor.
        assert_eq!(resolve_length("calc(35px*-1/4)"), Some(-8.75));
    }

    #[test]
    fn calc_add_and_subtract() {
        assert_eq!(resolve_length("calc(750px - 1px)"), Some(749.0));
        assert_eq!(resolve_length("calc(10px + 5px)"), Some(15.0));
    }

    #[test]
    fn var_with_fallback_resolves_to_fallback() {
        assert_eq!(resolve_length("var(--font-size-medium, 1rem)"), Some(16.0));
    }

    #[test]
    fn var_without_fallback_is_unresolvable() {
        assert_eq!(resolve_length("var(--unknown-token)"), None);
    }

    #[test]
    fn min_and_max_functions() {
        assert_eq!(resolve_length("max(5px, 10px)"), Some(10.0));
        assert_eq!(resolve_length("min(5px, 10px)"), Some(5.0));
    }

    #[test]
    fn nested_var_calc_max_like_wikipedia_icon_sizing() {
        // calc(max(calc(var(--font-size-medium,1rem) + 4px),10px))
        let expr = "calc(max(calc(var(--font-size-medium,1rem) + 4px),10px))";
        assert_eq!(resolve_length(expr), Some(20.0));
    }

    #[test]
    fn width_property_resolves_calc_with_var() {
        let s = compute_style("div", Some("width: calc(var(--x, 10px) + 5px)"));
        assert_eq!(s.width, crate::Dimension::Px(15.0));
    }

    #[test]
    fn flex_shorthand_two_numbers() {
        // The exact form Wikipedia's infobox uses for its label/value cells
        // (`.infobox tbody > tr > th/td{flex:1 0}`): without shorthand
        // support this was silently dropped, leaving both columns
        // shrink-to-fit instead of sharing the row's width.
        let s = compute_style("div", Some("flex: 1 0"));
        assert_eq!(s.flex_grow, Some(1.0));
        assert_eq!(s.flex_shrink, Some(0.0));
    }

    #[test]
    fn flex_shorthand_keywords() {
        let none = compute_style("div", Some("flex: none"));
        assert_eq!(none.flex_grow, Some(0.0));
        assert_eq!(none.flex_shrink, Some(0.0));

        let auto = compute_style("div", Some("flex: auto"));
        assert_eq!(auto.flex_grow, Some(1.0));
        assert_eq!(auto.flex_shrink, Some(1.0));
    }

    #[test]
    fn flex_shorthand_single_number_defaults_shrink_to_one() {
        let s = compute_style("div", Some("flex: 2"));
        assert_eq!(s.flex_grow, Some(2.0));
        assert_eq!(s.flex_shrink, Some(1.0));
    }

    #[test]
    fn flex_flow_is_unordered_resets_omissions_and_rejects_atomically() {
        for value in [
            "column",
            "wrap",
            "column wrap",
            "wrap column",
            "row-reverse wrap-reverse",
            "WRAP COLUMN-REVERSE",
        ] {
            assert!(supports_declaration("flex-flow", value), "{value}");
        }
        for value in [
            "",
            "none",
            "row column",
            "nowrap wrap-reverse",
            "row wrap extra",
            "10px",
        ] {
            assert!(!supports_declaration("flex-flow", value), "{value}");
        }

        let direction_only = compute_style(
            "div",
            Some("flex-wrap:wrap;flex-flow:column"),
        );
        assert_eq!(
            direction_only.flex_direction,
            Some(taffy::FlexDirection::Column)
        );
        assert_eq!(direction_only.flex_wrap, Some(taffy::FlexWrap::NoWrap));

        let wrap_only = compute_style(
            "div",
            Some("flex-direction:column;flex-flow:wrap"),
        );
        assert_eq!(wrap_only.flex_direction, Some(taffy::FlexDirection::Row));
        assert_eq!(wrap_only.flex_wrap, Some(taffy::FlexWrap::Wrap));

        let reversed = compute_style("div", Some("flex-flow:wrap column-reverse"));
        assert_eq!(
            reversed.flex_direction,
            Some(taffy::FlexDirection::ColumnReverse)
        );
        assert_eq!(reversed.flex_wrap, Some(taffy::FlexWrap::Wrap));

        let invalid_later = compute_style(
            "div",
            Some("flex-flow:column wrap;flex-flow:row column"),
        );
        assert_eq!(
            invalid_later.flex_direction,
            Some(taffy::FlexDirection::Column)
        );
        assert_eq!(invalid_later.flex_wrap, Some(taffy::FlexWrap::Wrap));
    }

    #[test]
    fn box_shadow_outset_parses() {
        let s = compute_style("div", Some("box-shadow: 0 2px 8px rgba(0,0,0,.15)"));
        let sh = s.box_shadow.expect("box-shadow parsed");
        assert!(!sh.inset);
        assert_eq!(sh.offset_x, 0.0);
        assert_eq!(sh.offset_y, 2.0);
        assert_eq!(sh.blur, 8.0);
        assert_eq!(sh.spread, 0.0);
        assert_eq!(sh.color, [0, 0, 0, 38]);
    }

    #[test]
    fn box_shadow_inset_parses() {
        let s = compute_style("div", Some("box-shadow: inset 0 0 0 1px #ccc"));
        let sh = s.box_shadow.expect("box-shadow parsed");
        assert!(sh.inset);
        assert_eq!(sh.offset_x, 0.0);
        assert_eq!(sh.offset_y, 0.0);
        assert_eq!(sh.blur, 0.0);
        assert_eq!(sh.spread, 1.0);
        assert_eq!(sh.color, [204, 204, 204, 255]);
    }

    #[test]
    fn box_shadow_color_defaults_to_current_color() {
        // No explicit color: falls back to the element's text color.
        let s = compute_style("div", Some("color: red; box-shadow: 1px 1px 2px"));
        let sh = s.box_shadow.expect("box-shadow parsed");
        assert_eq!(sh.color, [255, 0, 0, 255]);
    }

    #[test]
    fn box_shadow_none_clears() {
        let s = compute_style("div", Some("box-shadow: none"));
        assert!(s.box_shadow.is_none());
    }
}
