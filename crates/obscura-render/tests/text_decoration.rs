#![cfg(feature = "paint")]

use obscura_dom::parse_html;
use obscura_render::paint_dom;

fn render(markup: &str) -> tiny_skia::Pixmap {
    let tree = parse_html(&format!(
        "<style>body{{margin:8px;font:32px/48px sans-serif;color:green}}</style>{markup}"
    ));
    paint_dom(&tree, (320.0, 160.0), None).unwrap()
}

fn decorated(lines: &str) -> tiny_skia::Pixmap {
    render(&format!(
        "<span style='text-decoration:{lines}'>MMMM</span>"
    ))
}

fn changed_rows(baseline: &tiny_skia::Pixmap, actual: &tiny_skia::Pixmap) -> Vec<usize> {
    baseline
        .pixels()
        .chunks(320)
        .zip(actual.pixels().chunks(320))
        .enumerate()
        .filter_map(|(row, (before, after))| (before != after).then_some(row))
        .collect()
}

#[test]
fn all_three_text_decoration_lines_paint_in_distinct_vertical_bands() {
    let baseline = decorated("none");
    let overline = changed_rows(&baseline, &decorated("overline"));
    let strike = changed_rows(&baseline, &decorated("line-through"));
    let underline = changed_rows(&baseline, &decorated("underline"));
    assert!(!overline.is_empty(), "overline must paint");
    assert!(!strike.is_empty(), "line-through must paint");
    assert!(!underline.is_empty(), "underline must paint");
    assert!(overline.last().unwrap() < strike.first().unwrap());
    assert!(strike.last().unwrap() < underline.first().unwrap());
    let longhand = render("<span style='text-decoration-line:overline line-through'>MMMM</span>");
    assert_eq!(longhand.data(), decorated("overline line-through").data());
}

#[test]
fn combined_text_decoration_lines_preserve_each_individual_stroke() {
    let baseline = decorated("none");
    let individual = [
        decorated("underline"),
        decorated("overline"),
        decorated("line-through"),
    ];
    let combined = decorated("underline overline line-through");
    for (index, expected) in combined.pixels().iter().enumerate() {
        let painted = individual
            .iter()
            .find_map(|image| {
                (image.pixels()[index] != baseline.pixels()[index]).then_some(image.pixels()[index])
            })
            .unwrap_or(baseline.pixels()[index]);
        assert_eq!(*expected, painted, "pixel {index}");
    }
}

#[test]
fn text_decoration_propagates_through_inline_descendants_and_none_resets_own_lines() {
    let nested = render("<span style='text-decoration:line-through'><span style='text-decoration:none'>MMMM</span></span>");
    assert_eq!(nested.data(), decorated("line-through").data());
    let combined = render("<span style='text-decoration:overline'><span style='text-decoration:line-through'>MMMM</span></span>");
    assert_eq!(combined.data(), decorated("overline line-through").data());
    let reset = render(
        "<span style='text-decoration:overline line-through;text-decoration:none'>MMMM</span>",
    );
    assert_eq!(reset.data(), decorated("none").data());
}

#[test]
fn text_decoration_respects_transparency_and_wrapped_lines() {
    let transparent = render("<span style='color:transparent;text-decoration:underline overline line-through'>MMMM</span>");
    let empty = render("");
    assert_eq!(transparent.data(), empty.data());
    let plain = render("<div style='width:90px'>MMMM MMMM</div>");
    for lines in ["overline", "line-through"] {
        let wrapped = render(&format!(
            "<div style='width:90px;text-decoration:{lines}'>MMMM MMMM</div>"
        ));
        let rows = changed_rows(&plain, &wrapped);
        assert!(rows.iter().any(|row| *row < 56), "first line {lines}");
        assert!(rows.iter().any(|row| *row >= 56), "second line {lines}");
    }
}

#[test]
fn supports_accepts_the_implemented_text_decoration_lines() {
    let result = render("<span>MMMM</span><style>@supports (text-decoration-line: overline line-through) {span{text-decoration:overline line-through}}</style>");
    assert_eq!(result.data(), decorated("overline line-through").data());
}
