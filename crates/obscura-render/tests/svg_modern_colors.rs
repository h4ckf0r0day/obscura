#![cfg(feature = "paint")]

use obscura_dom::parse_html;
use obscura_render::paint_dom;

fn render(markup: &str) -> tiny_skia::Pixmap {
    let tree = parse_html(&format!(
        "<body style='margin:0;background:white'>{markup}</body>"
    ));
    paint_dom(&tree, (120.0, 80.0), None).expect("SVG raster")
}

fn assert_matches_reference(template: &str, modern: &str, reference: &str) {
    let actual = render(&template.replace("PAINT", modern));
    let expected = render(&template.replace("PAINT", reference));
    assert_ne!(expected.data(), render("").data(), "reference must paint");
    assert_eq!(actual.data(), expected.data(), "paint: {modern}");
}

#[test]
fn modern_svg_fill_colors_match_srgb() {
    let template = r#"<svg width="120" height="80"><rect width="120" height="80" fill="PAINT"/></svg>"#;
    for modern in ["oklch(0.6 0 0)", "oklab(0.6 0 0)", "OKLCH(60% 0 0)"] {
        assert_matches_reference(template, modern, "#808080");
    }
}

#[test]
fn modern_svg_stroke_colors_match_srgb() {
    assert_matches_reference(
        r#"<svg width="120" height="80"><path d="M10 40H110" fill="none" stroke="PAINT" stroke-width="20"/></svg>"#,
        "oklch(0.6 0 0)",
        "#808080",
    );
}

#[test]
fn modern_svg_gradient_stops_keep_paint_server_references() {
    assert_matches_reference(
        r##"<svg width="120" height="80"><defs><linearGradient id="paint"><stop stop-color="PAINT"/><stop offset="1" stop-color="white"/></linearGradient></defs><rect width="120" height="80" fill="url(#paint)"/></svg>"##,
        "oklch(0.6 0 0)",
        "#808080",
    );
}

#[test]
fn modern_svg_color_alpha_matches_eight_digit_hex() {
    assert_matches_reference(
        r#"<svg width="120" height="80"><rect width="120" height="80" fill="PAINT"/></svg>"#,
        "oklch(0.6 0 0 / 50%)",
        "#80808080",
    );
}

#[test]
fn modern_svg_colors_resolve_custom_properties_and_author_styles() {
    for template in [
        r#"<svg width="120" height="80" style="--paint:PAINT"><rect width="120" height="80" fill="var(--paint)"/></svg>"#,
        r#"<svg width="120" height="80"><rect width="120" height="80" style="fill:PAINT"/></svg>"#,
        r#"<style>.shape{fill:PAINT}</style><svg width="120" height="80"><rect class="shape" width="120" height="80"/></svg>"#,
        r##"<svg width="120" height="80" style="--paint:PAINT"><defs><linearGradient id="paint"><stop stop-color="var(--paint)"/><stop offset="1" stop-color="white"/></linearGradient></defs><rect width="120" height="80" fill="url(#paint)"/></svg>"##,
    ] {
        assert_matches_reference(template, "oklch(0.6 0 0)", "#808080");
    }
}

#[test]
fn current_color_and_none_still_reach_svg_rasterization() {
    let actual = render(r##"<svg width="120" height="80" style="color:oklch(0.6 0 0)"><rect x="10" y="10" width="100" height="60" fill="none" stroke="currentColor" stroke-width="8"/></svg>"##);
    let expected = render(r##"<svg width="120" height="80" color="#808080"><rect x="10" y="10" width="100" height="60" fill="none" stroke="currentColor" stroke-width="8"/></svg>"##);
    assert_eq!(actual.data(), expected.data());
}
