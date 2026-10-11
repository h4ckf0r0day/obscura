#![cfg(feature = "paint")]

use cosmic_text::{Attrs, AttrsList, AttrsOwned, Family, FontSystem, ShapeLine, Shaping};

fn font_system() -> FontSystem {
    let mut db = cosmic_text::fontdb::Database::new();
    db.load_font_data(include_bytes!("../assets/liberation-mono.ttf").to_vec());
    FontSystem::new_with_locale_and_db("en-US".into(), db)
}

fn advances(fonts: &mut FontSystem, text: &str, attrs: &AttrsList, shaping: Shaping) -> Vec<f32> {
    ShapeLine::new(fonts, text, attrs, shaping, 8).spans.iter()
        .flat_map(|span| &span.words).flat_map(|word| &word.glyphs)
        .map(|glyph| glyph.x_advance).collect()
}

#[test]
fn word_spacing_uses_separator_attributes_and_cache_identity() {
    let mut fonts = font_system();
    let base = Attrs::new().family(Family::Name("Liberation Mono"));
    for shaping in [Shaping::Basic, Shaping::Advanced] {
        let normal = advances(&mut fonts, "A B C", &AttrsList::new(&base), shaping);
        for spacing in [0.75, -0.25, -1.0, 0.0, 0.75] {
            let spaced = base.clone().word_spacing(spacing);
            let owned = AttrsOwned::new(&spaced);
            assert_eq!(owned.as_attrs(), spaced);
            let mut attrs = AttrsList::new(&base);
            attrs.add_span(1..2, &owned.as_attrs());
            attrs.add_span(3..4, &base.clone().word_spacing(spacing * 2.0));
            let actual = advances(&mut fonts, "A B C", &attrs, shaping);
            assert_eq!(actual.len(), normal.len());
            for (i, (actual, normal)) in actual.iter().zip(&normal).enumerate() {
                let extra = match i { 1 => spacing, 3 => spacing * 2.0, _ => 0.0 };
                assert!((actual - normal - extra).abs() < 0.00001, "glyph {i}: {actual} vs {normal}");
            }
        }
    }
}

#[test]
fn word_spacing_preserves_tabs_and_non_separator_glyphs() {
    let mut fonts = font_system();
    let base = Attrs::new().family(Family::Name("Liberation Mono"));
    for shaping in [Shaping::Basic, Shaping::Advanced] {
        for (text, separators) in [("A\u{a0}B", 1), ("A\tB", 0), ("A\u{2003}B", 0),
            ("A  B", 2), ("office", 0)]
        {
            let normal = advances(&mut fonts, text, &AttrsList::new(&base), shaping);
            let spaced = advances(&mut fonts, text, &AttrsList::new(&base.clone().word_spacing(0.75)), shaping);
            assert_eq!(normal.len(), spaced.len(), "{text:?}");
            let delta = spaced.iter().sum::<f32>() - normal.iter().sum::<f32>();
            assert!((delta - separators as f32 * 0.75).abs() < 0.00001, "{text:?}: {delta}");
        }
    }
}
