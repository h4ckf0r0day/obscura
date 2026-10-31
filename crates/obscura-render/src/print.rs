//! Ordered print paint captured without reshaping text or changing screenshots.

use crate::{Pixmap, Rect};
use cosmic_text::{ttf_parser, Font};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct PrintFont(pub(crate) Arc<Font>);

impl PrintFont {
    pub fn same_face(&self, other: &Self) -> bool {
        self.0.id() == other.0.id()
    }
    pub fn data(&self) -> &[u8] {
        self.0.data()
    }
    /// Subsetting is restricted to the immutable built-in programs. Arbitrary
    /// supplied fonts stay fully embedded rather than entering a font rewriter.
    pub fn can_subset(&self) -> bool {
        self.0.rustybuzz().is_subsetting_allowed()
            && crate::inline::is_bundled_print_font(self.data())
    }
    pub fn units_per_em(&self) -> u16 {
        self.0.rustybuzz().units_per_em() as u16
    }
    pub fn bounds(&self) -> [i16; 4] {
        let b = self.0.rustybuzz().global_bounding_box();
        [b.x_min, b.y_min, b.x_max, b.y_max]
    }
    pub fn ascent(&self) -> i16 {
        self.0.rustybuzz().ascender()
    }
    pub fn descent(&self) -> i16 {
        self.0.rustybuzz().descender()
    }
    pub fn italic_angle(&self) -> f32 {
        self.0.rustybuzz().italic_angle()
    }
    pub fn descriptor_flags(&self) -> u32 {
        4 | if self.0.rustybuzz().is_italic() {
            64
        } else {
            0
        } | if self.0.rustybuzz().is_monospaced() {
            1
        } else {
            0
        }
    }
    pub fn post_script_name(&self) -> String {
        let name: String = self
            .0
            .rustybuzz()
            .names()
            .into_iter()
            .filter(|name| name.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
            .find_map(|name| name.to_string())
            .unwrap_or_else(|| "EmbeddedFont".to_owned())
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .collect();
        if name.is_empty() {
            "EmbeddedFont".to_owned()
        } else {
            name
        }
    }
    pub fn advance(&self, glyph: u16) -> u16 {
        self.0
            .rustybuzz()
            .glyph_hor_advance(ttf_parser::GlyphId(glyph))
            .unwrap_or(0)
    }

    pub(crate) fn supported(font: &Font) -> bool {
        let face = font.rustybuzz();
        let tables = face.tables();
        !font.data().starts_with(b"ttcf")
            && !face.is_variable()
            && tables.glyf.is_some()
            && tables.colr.is_none()
            && tables.sbix.is_none()
            && tables.cbdt.is_none()
            && tables.svg.is_none()
            && face.is_outline_embedding_allowed()
            && matches!(
                face.permissions(),
                Some(ttf_parser::Permissions::Installable)
                    | Some(ttf_parser::Permissions::PreviewAndPrint)
                    | Some(ttf_parser::Permissions::Editable)
            )
    }
}

#[derive(Debug)]
pub struct PrintGlyph {
    pub font: PrintFont,
    pub id: u16,
    pub size: f32,
    pub x: f32,
    pub y: f32,
    pub color: [u8; 4],
}

#[derive(Debug)]
pub struct PrintCluster {
    pub text: String,
    pub glyphs: Vec<PrintGlyph>,
}

#[derive(Debug)]
pub enum PrintOp {
    Raster {
        pixels: Pixmap,
        x: u32,
        y: u32,
    },
    Text {
        clusters: Vec<PrintCluster>,
        clip: Option<Rect>,
    },
}

#[derive(Debug)]
pub struct PrintPage {
    pub ops: Vec<PrintOp>,
}

pub(crate) struct PrintCapture {
    pub(crate) page: PrintPage,
    dirty: bool,
    initial: bool,
    scanned: u64,
    retained: u64,
    glyphs: usize,
    pub(crate) failed: bool,
}

impl PrintCapture {
    pub(crate) fn new() -> Self {
        Self {
            page: PrintPage { ops: Vec::new() },
            dirty: true,
            initial: true,
            scanned: 0,
            retained: 0,
            glyphs: 0,
            failed: false,
        }
    }

    pub(crate) fn raster_painted(&mut self) {
        self.dirty = true;
    }

    pub(crate) fn flush(&mut self, pixmap: &mut Pixmap) {
        if !self.dirty || self.failed {
            return;
        }
        let area = u64::from(pixmap.width()) * u64::from(pixmap.height());
        self.scanned = self.scanned.saturating_add(area);
        let scan_limit = area.saturating_mul(4).min(64 * 1024 * 1024);
        if self.scanned > scan_limit || self.page.ops.len() >= 4096 {
            self.failed = true;
            return;
        }
        let (w, h) = (pixmap.width(), pixmap.height());
        let (mut left, mut top, mut right, mut bottom) = (w, h, 0, 0);
        for (i, pixel) in pixmap.pixels().iter().enumerate() {
            if pixel.alpha() == 0
                || (self.initial
                    && pixel.red() == 255
                    && pixel.green() == 255
                    && pixel.blue() == 255
                    && pixel.alpha() == 255)
            {
                continue;
            }
            let (x, y) = (i as u32 % w, i as u32 / w);
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
        if right > left && bottom > top {
            let area = u64::from(right - left) * u64::from(bottom - top);
            self.retained = self.retained.saturating_add(area);
            if self.retained > 16 * 1024 * 1024 {
                self.failed = true;
                return;
            }
            let rect =
                tiny_skia::IntRect::from_xywh(left as i32, top as i32, right - left, bottom - top);
            let Some(pixels) = rect.and_then(|rect| pixmap.clone_rect(rect)) else {
                self.failed = true;
                return;
            };
            self.page.ops.push(PrintOp::Raster {
                pixels,
                x: left,
                y: top,
            });
        }
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        self.initial = false;
        self.dirty = false;
    }

    pub(crate) fn text(
        &mut self,
        pixmap: &mut Pixmap,
        clusters: Vec<PrintCluster>,
        clip: Option<Rect>,
    ) {
        if clusters.is_empty() {
            return;
        }
        let count: usize = clusters.iter().map(|c| c.glyphs.len()).sum();
        self.glyphs = self.glyphs.saturating_add(count);
        if self.glyphs > 1_000_000 || self.page.ops.len() >= 4096 {
            self.failed = true;
            return;
        }
        self.flush(pixmap);
        if !self.failed && !clusters.is_empty() {
            self.page.ops.push(PrintOp::Text { clusters, clip });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoCanvas;
    impl crate::CanvasSurfaceSource for NoCanvas {
        fn surface(&self, _: obscura_dom::tree::NodeId) -> Option<crate::CanvasSurface<'_>> {
            None
        }
    }

    fn capture(html: &str, viewport: (f32, f32)) -> PrintPage {
        let tree = obscura_dom::tree_sink::parse_html(html);
        let mut resources = crate::RenderResourceCache::with_loader(|_: &str| None);
        let mut prepared = crate::prepare_dom(&tree, viewport, None, &mut resources).unwrap();
        let scroll = prepared.resolve_scroll_state(&tree, (0.0, 0.0), &Default::default());
        crate::print_prepared_region_with_scroll(
            &tree,
            &mut prepared,
            &mut resources,
            &scroll,
            crate::CaptureRegion::new(0.0, 0.0, viewport.0, viewport.1, 1.0),
            true,
            &NoCanvas,
        )
        .unwrap()
    }

    fn text(page: &PrintPage) -> String {
        page.ops
            .iter()
            .filter_map(|op| match op {
                PrintOp::Text { clusters, .. } => {
                    Some(clusters.iter().map(|c| c.text.as_str()).collect::<String>())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn print_simple_heading_and_currency_are_shaped_text_without_raster() {
        let page = capture(
            "<body style='font-family:Arial'><h1>Hello</h1><p>RM 6,000.00</p></body>",
            (800.0, 600.0),
        );
        assert_eq!(text(&page), "HelloRM 6,000.00");
        assert!(page.ops.iter().all(|op| matches!(op, PrintOp::Text { .. })));
    }

    #[test]
    fn print_clusters_preserve_ligatures_marks_fallback_and_soft_wraps() {
        let source = "office ffi q\u{308} Ελληνικά Кириллица مرحبا";
        let page = capture(
            &format!("<p style='font:20px system-ui;width:150px'>{source}</p>"),
            (400.0, 600.0),
        );
        let compact = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        // Arabic cluster storage follows visual order. Check it separately
        // from the LTR text, whose wrapped source must never repeat a line.
        assert!(
            compact(&text(&page)).starts_with(&compact("office ffi q\u{308} Ελληνικά Кириллица"))
        );
        let clusters = page
            .ops
            .iter()
            .flat_map(|op| match op {
                PrintOp::Text { clusters, .. } => clusters.as_slice(),
                _ => &[],
            })
            .collect::<Vec<_>>();
        assert!(clusters
            .iter()
            .any(|c| c.text == "q\u{308}" && c.glyphs.len() >= 2));
        assert!(clusters
            .iter()
            .any(|c| c.text == "ffi" && c.glyphs.len() == 1));
        assert!(clusters.iter().flat_map(|c| &c.glyphs).all(|g| g.id != 0));
        assert_eq!(
            clusters
                .iter()
                .map(|c| c.text.chars().filter(|c| !c.is_whitespace()).count())
                .sum::<usize>(),
            source.chars().filter(|c| !c.is_whitespace()).count(),
            "wrapped non-whitespace clusters must occur once; layout trims break spaces"
        );
    }

    #[test]
    fn print_rectangular_clip_keeps_text_and_rounded_clip_keeps_raster() {
        let plain = capture("<p style='width:70px;height:30px;white-space:nowrap;overflow:hidden'>Visible then hidden far away</p>", (400.0, 200.0));
        assert!(!text(&plain).is_empty());
        assert!(!text(&plain).contains("away"));
        assert!(plain
            .ops
            .iter()
            .any(|op| matches!(op, PrintOp::Text { clip: Some(_), .. })));
        let rounded = capture("<p style='width:70px;height:30px;overflow:hidden;border-radius:10px'>Rounded clip text</p>", (400.0, 200.0));
        assert!(rounded
            .ops
            .iter()
            .all(|op| matches!(op, PrintOp::Raster { .. })));
    }

    #[test]
    fn print_raster_fallback_stays_between_vector_items() {
        let page = capture(
            "<p>Before</p><p style='text-decoration:underline'>Fallback</p><p>After</p>",
            (400.0, 200.0),
        );
        assert_eq!(page.ops.len(), 3);
        assert!(matches!(page.ops[0], PrintOp::Text { .. }));
        assert!(matches!(page.ops[1], PrintOp::Raster { .. }));
        assert!(matches!(page.ops[2], PrintOp::Text { .. }));
        assert_eq!(text(&page), "BeforeAfter");
    }

    #[test]
    fn print_keeps_shaped_fallback_fonts_and_rasterizes_synthetic_italic() {
        let page = capture("<p style='font:20px Arial'>Hello مرحبا</p>", (400.0, 200.0));
        let fonts = page
            .ops
            .iter()
            .flat_map(|op| match op {
                PrintOp::Text { clusters, .. } => clusters.as_slice(),
                _ => &[],
            })
            .flat_map(|c| &c.glyphs)
            .map(|g| &g.font)
            .collect::<Vec<_>>();
        assert!(!fonts.is_empty());
        assert!(fonts.iter().any(|font| !font.same_face(fonts[0])));
        assert!(fonts[0].can_subset());
        let mut db = cosmic_text::fontdb::Database::new();
        let ids = db.load_font_source(cosmic_text::fontdb::Source::Binary(Arc::new(
            fonts[0].data().to_vec(),
        )));
        let copied = PrintFont(Arc::new(Font::new(&db, ids[0]).unwrap()));
        assert!(PrintFont::supported(&copied.0));
        assert!(
            !copied.can_subset(),
            "a supplied copy must not enter the trusted subsetter path"
        );
        let italic = capture(
            "<p style='font:italic 20px system-ui'>Italic fallback</p>",
            (400.0, 200.0),
        );
        assert!(text(&italic).is_empty());
        assert!(italic
            .ops
            .iter()
            .any(|op| matches!(op, PrintOp::Raster { .. })));
    }

    #[test]
    fn print_later_white_raster_is_not_elided() {
        let mut capture = PrintCapture::new();
        let mut pixels = Pixmap::new(10, 10).unwrap();
        pixels.fill(tiny_skia::Color::WHITE);
        capture.flush(&mut pixels);
        assert!(capture.page.ops.is_empty());
        pixels.fill(tiny_skia::Color::WHITE);
        capture.raster_painted();
        capture.flush(&mut pixels);
        assert_eq!(capture.page.ops.len(), 1);
    }

    #[test]
    fn print_segment_scan_budget_preserves_output_with_whole_page_fallback() {
        let html = format!(
            "<style>p{{margin:0;font:1px Arial;height:2px}}</style>{}",
            "<p>Text</p><p style='text-decoration:underline'>Fallback</p>".repeat(80)
        );
        let page = capture(&html, (1000.0, 1000.0));
        assert_eq!(page.ops.len(), 1);
        assert!(matches!(page.ops[0], PrintOp::Raster { .. }));
    }
}
