//! Bounded raster-backed PDF export over the retained document-space painter.
//!
//! The document is laid out with print media at the paper's printable width
//! (as browsers reflow for print), rendered at the page's device scale factor,
//! and sliced into pages without cutting through lines of text. Each page carries an invisible text layer positioned over the
//! painted text, so the PDF text can be selected, searched and extracted.
//! Header/footer templates, a heading outline and `@page` sizes are supported;
//! full CSS paged media (page boxes, `break-*` fragmentation) is not.

use std::io;

use image::ImageEncoder as _;
use obscura_js::CaptureRegion;

use crate::Page;

const POINTS_PER_INCH: f32 = 72.0;
const CSS_PX_PER_INCH: f32 = 96.0;
/// Highest device-pixel ratio used for page rasters. The page's device scale
/// factor (1 unless emulated) selects the ratio, as it does for screenshots.
const MAX_PDF_RASTER_SCALE: f32 = 3.0;
const MAX_PAPER_INCHES: f32 = 200.0;
const MAX_PDF_PAGES: usize = 250;
// Page ranges may select a small bounded subset from a much longer document.
// Keep the arithmetic/index space finite without charging unselected pages
// against the output-page limit.
const MAX_PDF_DOCUMENT_PAGES: usize = 1_000_000;
const MAX_PDF_PAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_PDF_TOTAL_RASTER_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_PDF_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RasterPdfPageRange {
    /// One-based inclusive first page. `None` means the first page.
    pub start: Option<usize>,
    /// One-based inclusive last page. `None` means the final page.
    pub end: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RasterPdfOptions {
    pub landscape: bool,
    pub print_background: bool,
    pub scale: f32,
    pub page_ranges: Vec<RasterPdfPageRange>,
    pub paper_width_in: f32,
    pub paper_height_in: f32,
    pub margin_top_in: f32,
    pub margin_bottom_in: f32,
    pub margin_left_in: f32,
    pub margin_right_in: f32,
    /// Draw `header_template`/`footer_template` in the top and bottom margins.
    pub display_header_footer: bool,
    /// Chrome-style template HTML. Elements with class `date`, `title`,
    /// `url`, `pageNumber` or `totalPages` receive those values. Empty means
    /// Chrome's default (date and title; URL and page number).
    pub header_template: String,
    pub footer_template: String,
    /// Use the document's `@page { size }` over the paper size.
    pub prefer_css_page_size: bool,
    /// Add a bookmark outline built from the document's headings.
    pub generate_document_outline: bool,
}

impl Default for RasterPdfOptions {
    fn default() -> Self {
        Self {
            landscape: false,
            print_background: false,
            scale: 1.0,
            page_ranges: Vec::new(),
            paper_width_in: 8.5,
            paper_height_in: 11.0,
            // CDP's defaults are one centimetre.
            margin_top_in: 0.3937,
            margin_bottom_in: 0.3937,
            margin_left_in: 0.3937,
            margin_right_in: 0.3937,
            display_header_footer: false,
            header_template: String::new(),
            footer_template: String::new(),
            prefer_css_page_size: false,
            generate_document_outline: false,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RasterPdfError {
    #[error("PDF paper dimensions must be finite and between 0 and 200 inches")]
    InvalidPaperSize,
    #[error("PDF margins must be finite, non-negative, and leave a printable area")]
    InvalidMargins,
    #[error("PDF scale must be finite and between 0.1 and 2")]
    InvalidScale,
    #[error("PDF page ranges select no pages from this document")]
    EmptyPageRange,
    #[error("the page has no retained renderable document")]
    NoRenderableDocument,
    #[error("PDF pagination would exceed the {0}-page safety limit")]
    TooManyPages(usize),
    #[error("PDF raster work would exceed the bounded page or document pixel budget")]
    RasterWorkLimitExceeded,
    #[error("document-space PDF capture failed: {0}")]
    CaptureFailed(String),
    #[error("PDF raster image decoding failed: {0}")]
    ImageDecode(String),
    #[error("PDF JPEG encoding failed: {0}")]
    ImageEncode(String),
    #[error("encoded PDF would exceed the 64 MiB safety limit")]
    OutputLimitExceeded,
}

#[derive(Debug)]
struct RasterPage {
    rgb: image::RgbImage,
    draw_width_pt: f32,
    draw_height_pt: f32,
    /// Content-stream operators drawn after the raster: the invisible text
    /// layer and any header/footer text.
    overlay: String,
    #[cfg(test)]
    _lifetime_probe: Option<std::rc::Rc<()>>,
}

#[derive(Clone, Copy, Debug)]
struct PaginationPlan {
    points_per_css_pixel: f32,
    css_page_height: f32,
}

impl RasterPdfOptions {
    fn page_geometry(&self) -> Result<(f32, f32, f32, f32, f32, f32), RasterPdfError> {
        let values = [self.paper_width_in, self.paper_height_in];
        if values
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0 || *value > MAX_PAPER_INCHES)
        {
            return Err(RasterPdfError::InvalidPaperSize);
        }
        let margins = [
            self.margin_top_in,
            self.margin_bottom_in,
            self.margin_left_in,
            self.margin_right_in,
        ];
        if margins
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err(RasterPdfError::InvalidMargins);
        }
        let (paper_width_in, paper_height_in) = if self.landscape {
            (self.paper_height_in, self.paper_width_in)
        } else {
            (self.paper_width_in, self.paper_height_in)
        };
        let page_width = paper_width_in * POINTS_PER_INCH;
        let page_height = paper_height_in * POINTS_PER_INCH;
        let left = self.margin_left_in * POINTS_PER_INCH;
        let bottom = self.margin_bottom_in * POINTS_PER_INCH;
        let printable_width =
            page_width - (self.margin_left_in + self.margin_right_in) * POINTS_PER_INCH;
        let printable_height =
            page_height - (self.margin_top_in + self.margin_bottom_in) * POINTS_PER_INCH;
        if printable_width <= 0.0 || printable_height <= 0.0 {
            return Err(RasterPdfError::InvalidMargins);
        }
        Ok((
            page_width,
            page_height,
            printable_width,
            printable_height,
            left,
            bottom,
        ))
    }
}

fn pagination_plan(
    content_width: f32,
    content_height: f32,
    printable_width: f32,
    printable_height: f32,
    scale: f32,
) -> Result<PaginationPlan, RasterPdfError> {
    if !scale.is_finite() || !(0.1..=2.0).contains(&scale) {
        return Err(RasterPdfError::InvalidScale);
    }
    let points_per_css_pixel = printable_width / content_width * scale;
    let css_page_height = printable_height / points_per_css_pixel;
    if !points_per_css_pixel.is_finite()
        || points_per_css_pixel <= 0.0
        || !css_page_height.is_finite()
        || css_page_height <= 0.0
    {
        return Err(RasterPdfError::RasterWorkLimitExceeded);
    }

    let page_count_value = (content_height / css_page_height).ceil().max(1.0);
    if !page_count_value.is_finite() || page_count_value > MAX_PDF_DOCUMENT_PAGES as f32 {
        return Err(RasterPdfError::TooManyPages(MAX_PDF_DOCUMENT_PAGES));
    }
    Ok(PaginationPlan {
        points_per_css_pixel,
        css_page_height,
    })
}

/// Document-space (top, bottom) of each page. A break that would cut
/// through a line of text moves up to that line's top, so no line is split
/// across pages, unless that would leave the page less than half full.
fn page_slices(
    content_height: f32,
    css_page_height: f32,
    lines: &[(f32, f32)],
) -> Result<Vec<(f32, f32)>, RasterPdfError> {
    // Lines sorted by top, and the tallest line: a break at `b` can only cut
    // lines whose top lies in `(b - tallest, b)`.
    let mut lines: Vec<(f32, f32)> = lines
        .iter()
        .copied()
        .filter(|(top, bottom)| top.is_finite() && bottom.is_finite() && bottom > top)
        .collect();
    lines.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let tallest = lines.iter().map(|(top, bottom)| bottom - top).fold(0.0f32, f32::max);
    let mut slices = Vec::new();
    let mut top = 0.0f32;
    while top < content_height - 0.5 {
        if slices.len() >= MAX_PDF_DOCUMENT_PAGES {
            return Err(RasterPdfError::TooManyPages(MAX_PDF_DOCUMENT_PAGES));
        }
        let mut bottom = (top + css_page_height).min(content_height);
        if bottom < content_height {
            let first = lines.partition_point(|(line_top, _)| *line_top <= bottom - tallest);
            let last = lines.partition_point(|(line_top, _)| *line_top < bottom);
            let cut = lines[first..last]
                .iter()
                .filter(|(_, line_bottom)| *line_bottom > bottom)
                .map(|(line_top, _)| *line_top)
                .fold(bottom, f32::min);
            if cut > top + css_page_height / 2.0 {
                bottom = cut;
            }
        }
        slices.push((top, bottom));
        top = bottom;
    }
    if slices.is_empty() {
        slices.push((0.0, content_height));
    }
    Ok(slices)
}

fn validate_selected_raster_work(
    content_width: f32,
    slices: &[(f32, f32)],
    selected_pages: &[usize],
    raster_scale: f32,
) -> Result<(), RasterPdfError> {
    let pixel_width = (content_width * raster_scale).ceil();
    if !pixel_width.is_finite()
        || pixel_width <= 0.0
        || pixel_width > obscura_js::MAX_CAPTURE_DIMENSION as f32
    {
        return Err(RasterPdfError::RasterWorkLimitExceeded);
    }
    let pixel_width = pixel_width as u64;
    let mut total_pixels = 0u64;
    for &page_index in selected_pages {
        let Some(&(top, bottom)) = slices.get(page_index) else {
            return Err(RasterPdfError::EmptyPageRange);
        };
        let slice_height = ((bottom - top) * raster_scale).ceil();
        if !slice_height.is_finite()
            || slice_height <= 0.0
            || slice_height > obscura_js::MAX_CAPTURE_DIMENSION as f32
        {
            return Err(RasterPdfError::RasterWorkLimitExceeded);
        }
        let page_pixels = pixel_width
            .checked_mul(slice_height as u64)
            .ok_or(RasterPdfError::RasterWorkLimitExceeded)?;
        if page_pixels > MAX_PDF_PAGE_PIXELS {
            return Err(RasterPdfError::RasterWorkLimitExceeded);
        }
        total_pixels = total_pixels
            .checked_add(page_pixels)
            .ok_or(RasterPdfError::RasterWorkLimitExceeded)?;
        if total_pixels > MAX_PDF_TOTAL_RASTER_PIXELS {
            return Err(RasterPdfError::RasterWorkLimitExceeded);
        }
    }

    Ok(())
}

fn selected_page_indices(
    page_count: usize,
    ranges: &[RasterPdfPageRange],
) -> Result<Vec<usize>, RasterPdfError> {
    if page_count == 0 {
        return Err(RasterPdfError::EmptyPageRange);
    }
    if ranges.is_empty() {
        if page_count > MAX_PDF_PAGES {
            return Err(RasterPdfError::TooManyPages(MAX_PDF_PAGES));
        }
        return Ok((0..page_count).collect());
    }
    let mut selected = std::collections::BTreeSet::new();
    for range in ranges {
        let start = range.start.unwrap_or(1);
        let end = range.end.unwrap_or(page_count);
        if start == 0 || end == 0 || start > end {
            return Err(RasterPdfError::EmptyPageRange);
        }
        if start > page_count {
            continue;
        }
        let end = end.min(page_count);
        let span = end - start + 1;
        if span > MAX_PDF_PAGES {
            return Err(RasterPdfError::TooManyPages(MAX_PDF_PAGES));
        }
        for page in start..=end {
            selected.insert(page - 1);
            if selected.len() > MAX_PDF_PAGES {
                return Err(RasterPdfError::TooManyPages(MAX_PDF_PAGES));
            }
        }
    }
    let selected = selected.into_iter().collect::<Vec<_>>();
    if selected.is_empty() {
        return Err(RasterPdfError::EmptyPageRange);
    }
    Ok(selected)
}

impl Page {
    /// Export the current print-media layout as a paginated raster PDF.
    ///
    /// The full document width is scaled uniformly into the printable width;
    /// vertical slices become pages. Print media rules participate in normal
    /// cascade and layout, but CSS paged media, headers, and footers remain
    /// outside this raster-backed exporter.
    pub fn raster_pdf(&self, options: RasterPdfOptions) -> Result<Vec<u8>, RasterPdfError> {
        self.raster_pdf_with_animation_sample(options, self.live_animation_sample())
    }

    pub fn raster_pdf_at_animation_time(
        &self,
        options: RasterPdfOptions,
        animation_sample_time: obscura_js::AnimationSampleTime,
    ) -> Result<Vec<u8>, RasterPdfError> {
        self.raster_pdf_with_animation_sample(
            options,
            obscura_js::AnimationSample {
                time: animation_sample_time,
                mode: obscura_js::AnimationSampleMode::LocalOverride,
            },
        )
    }

    pub fn raster_pdf_with_animation_sample(
        &self,
        mut options: RasterPdfOptions,
        animation_sample: obscura_js::AnimationSample,
    ) -> Result<Vec<u8>, RasterPdfError> {
        if options.prefer_css_page_size {
            if let Some((width, height)) = self.css_page_size() {
                options.paper_width_in = width;
                options.paper_height_in = height;
                options.landscape = false;
            }
        }
        let (page_width, page_height, printable_width, printable_height, left, bottom) =
            options.page_geometry()?;
        if !options.scale.is_finite() || !(0.1..=2.0).contains(&options.scale) {
            return Err(RasterPdfError::InvalidScale);
        }
        let js = self
            .js
            .as_ref()
            .ok_or(RasterPdfError::NoRenderableDocument)?;
        if !js.set_animation_sample(animation_sample) {
            return Err(RasterPdfError::NoRenderableDocument);
        }
        // Lay out for the paper, as browsers do for print: the printable width
        // in CSS pixels at 96 per inch, divided by the scale factor.
        let css_per_point = CSS_PX_PER_INCH / POINTS_PER_INCH / options.scale;
        let print_viewport = (
            (printable_width * css_per_point).max(1.0),
            (printable_height * css_per_point).max(1.0),
        );
        let previous_viewport = js.swap_render_viewport(print_viewport);
        let previous_media = js.set_render_media(obscura_js::CssMediaType::Print);
        let header_footer = options.display_header_footer.then(|| HeaderFooter {
            header: options.header_template.clone(),
            footer: options.footer_template.clone(),
            title: self.title.clone(),
            url: self.url_string(),
            date: format_print_date(std::time::SystemTime::now()),
        });
        let result = (|| {
            let (content_width, content_height) = js
                .prepared_content_size()
                .ok_or(RasterPdfError::NoRenderableDocument)?;
            if !content_width.is_finite()
                || !content_height.is_finite()
                || content_width <= 0.0
                || content_height <= 0.0
            {
                return Err(RasterPdfError::NoRenderableDocument);
            }
            // Content wider than the paper layout (fixed-width pages) is
            // shrunk to fit, as Chrome's print does.
            let layout_width = content_width.max(print_viewport.0);

            let plan = pagination_plan(
                layout_width,
                content_height,
                printable_width,
                printable_height,
                1.0,
            )?;
            let fragments = js.prepared_text_fragments().unwrap_or_default();
            let lines: Vec<(f32, f32)> =
                fragments.iter().map(|f| (f.y, f.y + f.height)).collect();
            let slices = page_slices(content_height, plan.css_page_height, &lines)?;
            let selected_pages = selected_page_indices(slices.len(), &options.page_ranges)?;
            let requested_scale = if self.device_scale_factor.is_finite() {
                self.device_scale_factor.clamp(1.0, MAX_PDF_RASTER_SCALE)
            } else {
                1.0
            };
            let raster_scale = if validate_selected_raster_work(
                layout_width,
                &slices,
                &selected_pages,
                requested_scale,
            )
            .is_ok()
            {
                requested_scale
            } else {
                validate_selected_raster_work(layout_width, &slices, &selected_pages, 1.0)?;
                1.0
            };

            let outline = if options.generate_document_outline {
                outline_entries(
                    &js.prepared_heading_outline().unwrap_or_default(),
                    &selected_pages,
                    &slices,
                    plan.points_per_css_pixel,
                    bottom + printable_height,
                )
            } else {
                Vec::new()
            };
            let total_pages = selected_pages.len();
            let geometry = PageGeometry { page_width, page_height, left, bottom, printable_height };

            encode_pdf_pages(total_pages, geometry, &outline, |output_page_index| {
                let page_index = selected_pages[output_page_index];
                let (y, slice_bottom) = slices[page_index];
                let slice_height = slice_bottom - y;
                let png = js
                    .screenshot_prepared_region_at_scroll_with_backgrounds(
                        CaptureRegion::new(0.0, y, layout_width, slice_height, raster_scale),
                        (0.0, y),
                        options.print_background,
                    )
                    .map_err(|error| RasterPdfError::CaptureFailed(format!("{error:?}")))?;
                let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                    .map_err(|error| RasterPdfError::ImageDecode(error.to_string()))?;
                // The document capture is already a complete PNG allocation. Drop
                // it before converting the decoded pixels and, below, encoding the
                // JPEG directly into the final PDF buffer. At no point do we retain
                // PNGs or JPEGs for earlier pages.
                drop(png);
                let rgb = decoded.into_rgb8();
                let mut overlay = text_layer_ops(
                    &fragments,
                    y,
                    slice_bottom,
                    plan.points_per_css_pixel,
                    left,
                    bottom + printable_height,
                );
                if let Some(header_footer) = &header_footer {
                    overlay.push_str(&header_footer.ops(
                        output_page_index + 1,
                        total_pages,
                        geometry,
                        options.margin_top_in * POINTS_PER_INCH,
                        options.margin_bottom_in * POINTS_PER_INCH,
                    ));
                }
                Ok(RasterPage {
                    rgb,
                    draw_width_pt: layout_width * plan.points_per_css_pixel,
                    draw_height_pt: slice_height * plan.points_per_css_pixel,
                    overlay,
                    #[cfg(test)]
                    _lifetime_probe: None,
                })
            })
        })();
        js.set_render_media(previous_media);
        js.swap_render_viewport(previous_viewport);
        result
    }

    /// The first `@page { size }` in the document's author styles, in inches
    /// (width, height), for `preferCSSPageSize`.
    fn css_page_size(&self) -> Option<(f32, f32)> {
        let js = self.js.as_ref()?;
        let sheets = js.with_dom(|dom| {
            let mut sheets = Vec::new();
            let external = dom.external_stylesheets();
            for id in dom.descendants(dom.document()) {
                let local = dom.with_node(id, |node| {
                    node.as_element().map(|element| element.local.to_string())
                });
                match local.flatten().as_deref() {
                    Some("style") => sheets.push(dom.text_content(id)),
                    Some("link") => {}
                    _ => continue,
                }
                if let Some(sheet) = external.get(&id) {
                    sheets.extend(sheet.sources.iter().map(|source| source.to_string()));
                }
            }
            sheets
        })?;
        sheets.iter().find_map(|css| page_size_from_css(css))
    }
}

#[derive(Clone, Copy, Debug)]
struct PageGeometry {
    page_width: f32,
    page_height: f32,
    left: f32,
    bottom: f32,
    printable_height: f32,
}

/// A bookmark: heading level and title, the output page it lands on, and its
/// top in PDF points on that page.
#[derive(Clone, Debug, PartialEq)]
struct OutlineEntry {
    level: u8,
    title: String,
    page: usize,
    top_pt: f32,
}

// Fixed object ids. Pages follow at FIRST_PAGE_OBJECT, three objects each
// (page, content stream, image), then outline objects.
const CATALOG_OBJECT: usize = 1;
const PAGES_OBJECT: usize = 2;
const TEXT_FONT_OBJECT: usize = 3;
const TEXT_CID_FONT_OBJECT: usize = 4;
const TEXT_FONT_DESCRIPTOR_OBJECT: usize = 5;
const TEXT_TO_UNICODE_OBJECT: usize = 6;
const HELVETICA_OBJECT: usize = 7;
const FIRST_PAGE_OBJECT: usize = 8;

fn encode_pdf_pages(
    page_count: usize,
    geometry: PageGeometry,
    outline: &[OutlineEntry],
    mut page_source: impl FnMut(usize) -> Result<RasterPage, RasterPdfError>,
) -> Result<Vec<u8>, RasterPdfError> {
    page_count
        .checked_mul(3)
        .and_then(|objects| objects.checked_add(FIRST_PAGE_OBJECT + outline.len() + 1))
        .ok_or(RasterPdfError::OutputLimitExceeded)?;
    let page_object = |index: usize| FIRST_PAGE_OBJECT + index * 3;
    let mut writer = PdfWriter::new(MAX_PDF_OUTPUT_BYTES)?;

    let kids = (0..page_count)
        .map(|index| format!("{} 0 R", page_object(index)))
        .collect::<Vec<_>>()
        .join(" ");
    let pages_dictionary = format!("<< /Type /Pages /Count {page_count} /Kids [{kids}] >>");
    writer.write_object(PAGES_OBJECT, pages_dictionary.as_bytes())?;
    write_text_layer_font(&mut writer)?;
    writer.write_object(
        HELVETICA_OBJECT,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    )?;

    let PageGeometry { page_width, page_height, left, bottom, printable_height } = geometry;
    for index in 0..page_count {
        let page = page_source(index)?;
        let page_id = page_object(index);
        let content_id = page_id + 1;
        let image_id = page_id + 2;
        let page_dictionary = format!(
            "<< /Type /Page /Parent {PAGES_OBJECT} 0 R /MediaBox [0 0 {page_width:.3} {page_height:.3}] /Resources << /XObject << /Im0 {image_id} 0 R >> /Font << /F1 {TEXT_FONT_OBJECT} 0 R /F2 {HELVETICA_OBJECT} 0 R >> >> /Contents {content_id} 0 R >>"
        );
        writer.write_object(page_id, page_dictionary.as_bytes())?;

        let draw_y = bottom + printable_height - page.draw_height_pt;
        let commands = format!(
            "q\n{:.3} 0 0 {:.3} {:.3} {:.3} cm\n/Im0 Do\nQ\n{}",
            page.draw_width_pt, page.draw_height_pt, left, draw_y, page.overlay,
        );
        let content = format!(
            "<< /Length {} >>\nstream\n{}endstream",
            commands.len(),
            commands
        );
        writer.write_object(content_id, content.as_bytes())?;
        writer.write_rgb_image(image_id, &page.rgb)?;
        // `page`, including its decoded RGB raster, is dropped here before
        // the next page is captured. Only the bounded final PDF survives.
    }

    let outline_root = FIRST_PAGE_OBJECT + page_count * 3;
    let catalog = if outline.is_empty() {
        format!("<< /Type /Catalog /Pages {PAGES_OBJECT} 0 R >>")
    } else {
        write_outline(&mut writer, outline_root, outline, |page| page_object(page))?;
        format!(
            "<< /Type /Catalog /Pages {PAGES_OBJECT} 0 R /Outlines {outline_root} 0 R /PageMode /UseOutlines >>"
        )
    };
    writer.write_object(CATALOG_OBJECT, catalog.as_bytes())?;
    writer.finish()
}

/// The invisible text layer's font: a Type0 font whose character codes are
/// Unicode (UTF-16 BMP) code units, with a ToUnicode map saying so. It is
/// never painted (text render mode 3), so it needs no glyphs; viewers use it
/// only to select, search and extract text.
fn write_text_layer_font(writer: &mut PdfWriter) -> Result<(), RasterPdfError> {
    writer.write_object(
        TEXT_FONT_OBJECT,
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /GlyphLessFont /Encoding /Identity-H /DescendantFonts [{TEXT_CID_FONT_OBJECT} 0 R] /ToUnicode {TEXT_TO_UNICODE_OBJECT} 0 R >>"
        )
        .as_bytes(),
    )?;
    writer.write_object(
        TEXT_CID_FONT_OBJECT,
        format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /GlyphLessFont /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {TEXT_FONT_DESCRIPTOR_OBJECT} 0 R /DW 500 /CIDToGIDMap /Identity >>"
        )
        .as_bytes(),
    )?;
    writer.write_object(
        TEXT_FONT_DESCRIPTOR_OBJECT,
        b"<< /Type /FontDescriptor /FontName /GlyphLessFont /Flags 5 /FontBBox [0 -200 500 800] /ItalicAngle 0 /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 >>",
    )?;
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    // Each code maps to the same UTF-16 unit. A bfrange may only vary the
    // last byte, so emit one range per high byte, 100 ranges per block,
    // skipping the surrogate block.
    let highs: Vec<u32> = (0u32..=0xFF).filter(|high| !(0xD8..=0xDF).contains(high)).collect();
    for block in highs.chunks(100) {
        cmap.push_str(&format!("{} beginbfrange\n", block.len()));
        for high in block {
            cmap.push_str(&format!("<{high:02X}00> <{high:02X}FF> <{high:02X}00>\n"));
        }
        cmap.push_str("endbfrange\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    writer.write_object(
        TEXT_TO_UNICODE_OBJECT,
        format!("<< /Length {} >>\nstream\n{cmap}endstream", cmap.len()).as_bytes(),
    )
}

/// Operators placing each text fragment whose baseline falls on the page
/// slice `[slice_top, slice_bottom)` as invisible text over its painted
/// glyphs. Horizontal scaling stretches the run to the painted width, so a
/// selection highlights the same span a reader sees.
fn text_layer_ops(
    fragments: &[obscura_js::TextFragment],
    slice_top: f32,
    slice_bottom: f32,
    points_per_css_pixel: f32,
    left: f32,
    top_pt: f32,
) -> String {
    let mut ops = String::new();
    for fragment in fragments {
        if fragment.baseline < slice_top || fragment.baseline >= slice_bottom {
            continue;
        }
        let units: Vec<u16> = fragment
            .text
            .chars()
            .map(|c| if (c as u32) <= 0xFFFF { c as u32 as u16 } else { 0xFFFD })
            .filter(|unit| !(0xD800..=0xDFFF).contains(unit))
            .collect();
        if units.is_empty() || fragment.font_size <= 0.0 || !fragment.font_size.is_finite() {
            continue;
        }
        let font_size = fragment.font_size * points_per_css_pixel;
        let natural = units.len() as f32 * 0.5 * font_size;
        let target = fragment.width * points_per_css_pixel;
        let scaling = if natural > 0.0 && target > 0.0 {
            (target / natural * 100.0).clamp(1.0, 1000.0)
        } else {
            100.0
        };
        let x = left + fragment.x * points_per_css_pixel;
        let y = top_pt - (fragment.baseline - slice_top) * points_per_css_pixel;
        if ops.is_empty() {
            ops.push_str("BT\n3 Tr\n");
        }
        let hex: String = units.iter().map(|unit| format!("{unit:04X}")).collect();
        ops.push_str(&format!(
            "/F1 {font_size:.3} Tf\n{scaling:.3} Tz\n1 0 0 1 {x:.3} {y:.3} Tm\n<{hex}> Tj\n"
        ));
    }
    if !ops.is_empty() {
        ops.push_str("ET\n");
    }
    ops
}

/// Map document headings onto the selected output pages as bookmarks.
fn outline_entries(
    headings: &[(u8, String, f32)],
    selected_pages: &[usize],
    slices: &[(f32, f32)],
    points_per_css_pixel: f32,
    top_pt: f32,
) -> Vec<OutlineEntry> {
    headings
        .iter()
        .filter_map(|(level, title, y)| {
            let y = y.max(0.0);
            let document_page = slices
                .iter()
                .position(|&(top, bottom)| y >= top && y < bottom)
                .unwrap_or(slices.len().saturating_sub(1));
            let page = selected_pages.iter().position(|&selected| selected == document_page)?;
            let offset = y - slices[document_page].0;
            Some(OutlineEntry {
                level: *level,
                title: title.chars().take(256).collect(),
                page,
                top_pt: top_pt - offset.max(0.0) * points_per_css_pixel,
            })
        })
        .collect()
}

/// Write the outline dictionary at `root` and one item per entry after it,
/// nested by heading level.
fn write_outline(
    writer: &mut PdfWriter,
    root: usize,
    entries: &[OutlineEntry],
    page_object: impl Fn(usize) -> usize,
) -> Result<(), RasterPdfError> {
    let id = |index: usize| root + 1 + index;
    // Parent of each entry: the nearest earlier entry with a lower level.
    let mut parents: Vec<Option<usize>> = Vec::with_capacity(entries.len());
    let mut stack: Vec<usize> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        while stack.last().is_some_and(|&open| entries[open].level >= entry.level) {
            stack.pop();
        }
        parents.push(stack.last().copied());
        stack.push(index);
    }
    let children = |parent: Option<usize>| -> Vec<usize> {
        (0..entries.len()).filter(|&index| parents[index] == parent).collect()
    };
    let descendants = |index: usize| -> usize {
        let mut count = 0;
        let mut frontier = vec![index];
        while let Some(node) = frontier.pop() {
            for child in (0..entries.len()).filter(|&c| parents[c] == Some(node)) {
                count += 1;
                frontier.push(child);
            }
        }
        count
    };
    let top = children(None);
    writer.write_object(
        root,
        format!(
            "<< /Type /Outlines /First {} 0 R /Last {} 0 R /Count {} >>",
            id(top[0]),
            id(*top.last().unwrap()),
            entries.len()
        )
        .as_bytes(),
    )?;
    for (index, entry) in entries.iter().enumerate() {
        let siblings = children(parents[index]);
        let position = siblings.iter().position(|&sibling| sibling == index).unwrap_or(0);
        let mut dictionary = format!(
            "<< /Title {} /Parent {} 0 R /Dest [{} 0 R /XYZ null {:.3} null]",
            pdf_text_string(&entry.title),
            parents[index].map_or(root, id),
            page_object(entry.page),
            entry.top_pt,
        );
        if position > 0 {
            dictionary.push_str(&format!(" /Prev {} 0 R", id(siblings[position - 1])));
        }
        if let Some(&next) = siblings.get(position + 1) {
            dictionary.push_str(&format!(" /Next {} 0 R", id(next)));
        }
        let own = children(Some(index));
        if let (Some(first), Some(last)) = (own.first(), own.last()) {
            dictionary.push_str(&format!(
                " /First {} 0 R /Last {} 0 R /Count {}",
                id(*first),
                id(*last),
                descendants(index)
            ));
        }
        dictionary.push_str(" >>");
        writer.write_object(id(index), dictionary.as_bytes())?;
    }
    Ok(())
}

/// A PDF text string: UTF-16BE with a byte-order mark, hex encoded.
fn pdf_text_string(text: &str) -> String {
    let mut hex = String::from("<FEFF");
    for unit in text.encode_utf16() {
        hex.push_str(&format!("{unit:04X}"));
    }
    hex.push('>');
    hex
}

/// Header/footer values and templates for one export.
struct HeaderFooter {
    header: String,
    footer: String,
    title: String,
    url: String,
    date: String,
}

// Chrome's defaults when displayHeaderFooter is set with empty templates.
const DEFAULT_HEADER_TEMPLATE: &str = "<div style=\"font-size:8px;display:flex\"><span class=\"date\"></span><span class=\"title\" style=\"text-align:center\"></span></div>";
const DEFAULT_FOOTER_TEMPLATE: &str = "<div style=\"font-size:8px;display:flex\"><span class=\"url\"></span><span style=\"text-align:right\"><span class=\"pageNumber\"></span>/<span class=\"totalPages\"></span></span></div>";

impl HeaderFooter {
    /// Operators drawing the header in the top margin and the footer in the
    /// bottom margin of one page, in Helvetica.
    fn ops(
        &self,
        page_number: usize,
        total_pages: usize,
        geometry: PageGeometry,
        margin_top: f32,
        margin_bottom: f32,
    ) -> String {
        let values = [
            ("date", self.date.clone()),
            ("title", self.title.clone()),
            ("url", self.url.clone()),
            ("pageNumber", page_number.to_string()),
            ("totalPages", total_pages.to_string()),
        ];
        let header = if self.header.trim().is_empty() { DEFAULT_HEADER_TEMPLATE } else { &self.header };
        let footer = if self.footer.trim().is_empty() { DEFAULT_FOOTER_TEMPLATE } else { &self.footer };
        let content_left = geometry.left;
        let content_right = geometry.page_width
            - (geometry.page_width - geometry.left
                - (geometry.page_width - 2.0 * geometry.left).max(0.0))
            .max(geometry.left);
        let mut ops = String::new();
        for (template, band_top, band_height) in [
            (header, geometry.page_height, margin_top),
            (footer, margin_bottom, margin_bottom),
        ] {
            let Some(line) = render_template(template, &values) else { continue };
            let size_pt = line.font_size_px * 0.75;
            if band_height <= size_pt {
                continue;
            }
            // Vertically centre the line in its margin band.
            let baseline = band_top - band_height / 2.0 - size_pt * 0.35;
            for (text, align) in &line.parts {
                let width = helvetica_width(text, size_pt);
                let x = match align {
                    Align::Left => content_left,
                    Align::Center => (content_left + content_right - width) / 2.0,
                    Align::Right => content_right - width,
                };
                ops.push_str(&format!(
                    "BT\n0 Tr\n/F2 {size_pt:.3} Tf\n1 0 0 1 {x:.3} {baseline:.3} Tm\n{} Tj\nET\n",
                    pdf_winansi_string(text)
                ));
            }
        }
        ops
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Align {
    Left,
    Center,
    Right,
}

struct TemplateLine {
    font_size_px: f32,
    parts: Vec<(String, Align)>,
}

/// Render a header/footer template to text parts. Elements carrying a
/// value class get that value; each top-level child of the first element (or
/// the whole template) becomes one part, aligned by its `text-align`, or by
/// position when the container is a flex row (first left, last right).
fn render_template(template: &str, values: &[(&str, String)]) -> Option<TemplateLine> {
    let dom = obscura_dom::parse_html(template);
    let body = dom.query_selector("body").ok().flatten()?;
    let style_of = |id: obscura_dom::NodeId| -> String {
        dom.with_node(id, |node| node.get_attribute("style").map(str::to_string))
            .flatten()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    let mut font_size_px = 8.0f32;
    for id in dom.descendants(body) {
        let style = style_of(id);
        if let Some(size) = css_declaration(&style, "font-size").and_then(|v| css_length_px(&v)) {
            font_size_px = size.clamp(1.0, 72.0);
            break;
        }
    }
    let text_of = |id: obscura_dom::NodeId| -> String {
        let mut out = String::new();
        let mut stack = vec![id];
        while let Some(node) = stack.pop() {
            let class = dom
                .with_node(node, |n| n.get_attribute("class").map(str::to_string))
                .flatten()
                .unwrap_or_default();
            if let Some((_, value)) = values
                .iter()
                .find(|(name, _)| class.split_whitespace().any(|c| c == *name))
            {
                out.push_str(value);
                continue;
            }
            if let Some(text) = dom.with_node(node, |n| n.text_content_of_text_node().map(str::to_string)).flatten() {
                out.push_str(&text);
                continue;
            }
            let mut children = dom.children(node);
            children.reverse();
            stack.extend(children);
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let is_element = |id: obscura_dom::NodeId| dom.with_node(id, |n| n.is_element()).unwrap_or(false);
    let top: Vec<_> = dom.children(body).into_iter().filter(|&id| is_element(id) || !text_of(id).is_empty()).collect();
    // A flex row lays its children out as separate columns; anything else is
    // one run of text.
    let (container, parts_nodes, flex_row) = match top.as_slice() {
        [single] if is_element(*single) => {
            let flex = css_declaration(&style_of(*single), "display").is_some_and(|d| d.contains("flex"));
            let kids: Vec<_> = dom.children(*single).into_iter().filter(|&id| !text_of(id).is_empty()).collect();
            (Some(*single), if flex && kids.len() > 1 { kids } else { vec![*single] }, flex)
        }
        _ => (None, vec![body], false),
    };
    let container_style = container.map(style_of).unwrap_or_default();
    let container_align = css_declaration(&container_style, "text-align");
    let count = parts_nodes.len();
    let mut parts = Vec::new();
    for (index, id) in parts_nodes.into_iter().enumerate() {
        let text = text_of(id);
        if text.is_empty() {
            continue;
        }
        let own = css_declaration(&style_of(id), "text-align").or_else(|| container_align.clone());
        let align = match own.as_deref() {
            Some("center") => Align::Center,
            Some("right") | Some("end") => Align::Right,
            Some(_) => Align::Left,
            None if flex_row && count > 1 && index == count - 1 => Align::Right,
            None if flex_row && count > 2 && index > 0 => Align::Center,
            None => Align::Left,
        };
        parts.push((text, align));
    }
    (!parts.is_empty()).then_some(TemplateLine { font_size_px, parts })
}

fn css_declaration(style: &str, name: &str) -> Option<String> {
    style.split(';').find_map(|declaration| {
        let (property, value) = declaration.split_once(':')?;
        (property.trim() == name).then(|| value.trim().to_string())
    })
}

/// A CSS length in CSS pixels (px, pt, pc, in, cm, mm, em as 16px).
fn css_length_px(value: &str) -> Option<f32> {
    let value = value.trim();
    let split = value.find(|c: char| c.is_ascii_alphabetic() || c == '%').unwrap_or(value.len());
    let number: f32 = value[..split].trim().parse().ok()?;
    let px = match value[split..].trim() {
        "" | "px" => number,
        "pt" => number * 96.0 / 72.0,
        "pc" => number * 16.0,
        "in" => number * 96.0,
        "cm" => number * 96.0 / 2.54,
        "mm" => number * 96.0 / 25.4,
        "em" | "rem" => number * 16.0,
        _ => return None,
    };
    (px.is_finite() && px > 0.0).then_some(px)
}

/// `@page { size: ... }` in inches (width, height).
fn page_size_from_css(css: &str) -> Option<(f32, f32)> {
    let lower = css.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(at) = rest.find("@page") {
        let block_start = rest[at..].find('{')? + at + 1;
        let block_end = rest[block_start..].find('}').map_or(rest.len(), |end| block_start + end);
        let block = &rest[block_start..block_end];
        if let Some(value) = css_declaration(&block.replace('\n', " "), "size") {
            if let Some(size) = parse_page_size(&value) {
                return Some(size);
            }
        }
        rest = &rest[block_end..];
    }
    None
}

fn parse_page_size(value: &str) -> Option<(f32, f32)> {
    let mut named = None;
    let mut orientation = None;
    let mut lengths = Vec::new();
    for token in value.split_whitespace() {
        let token = token.trim_end_matches("!important");
        match token {
            "a5" => named = Some((5.83, 8.27)),
            "a4" => named = Some((8.27, 11.69)),
            "a3" => named = Some((11.69, 16.54)),
            "b5" => named = Some((6.93, 9.84)),
            "b4" => named = Some((9.84, 13.9)),
            "letter" => named = Some((8.5, 11.0)),
            "legal" => named = Some((8.5, 14.0)),
            "ledger" | "tabloid" => named = Some((11.0, 17.0)),
            "landscape" => orientation = Some(true),
            "portrait" => orientation = Some(false),
            "auto" => {}
            other => lengths.push(css_length_px(other)? / 96.0),
        }
    }
    let (width, height) = match (named, lengths.as_slice()) {
        (Some(size), []) => size,
        (None, [side]) => (*side, *side),
        (None, [width, height]) => (*width, *height),
        (None, []) if orientation.is_some() => (8.5, 11.0),
        _ => return None,
    };
    Some(match orientation {
        Some(true) => (width.max(height), width.min(height)),
        Some(false) => (width.min(height), width.max(height)),
        None => (width, height),
    })
}

/// Width of `text` in Helvetica at `size` points (standard AFM widths).
fn helvetica_width(text: &str, size: f32) -> f32 {
    const ASCII: [u16; 95] = [
        278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722,
        722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556,
        556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500,
        500, 334, 260, 334, 584,
    ];
    text.chars()
        .map(|c| match c as u32 {
            code @ 32..=126 => ASCII[(code - 32) as usize] as f32,
            _ => 556.0,
        })
        .sum::<f32>()
        * size
        / 1000.0
}

/// A PDF literal string in WinAnsi (Latin-1 subset); other characters
/// become `?`.
fn pdf_winansi_string(text: &str) -> String {
    let mut out = String::from("(");
    for c in text.chars() {
        let code = c as u32;
        match c {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ if (32..=126).contains(&code) => out.push(c),
            _ if (0xA0..=0xFF).contains(&code) => out.push_str(&format!("\\{code:03o}")),
            _ => out.push('?'),
        }
    }
    out.push(')');
    out
}

/// Chrome's header date format, e.g. `9/26/26, 3:05 PM` (UTC).
fn format_print_date(now: std::time::SystemTime) -> String {
    let seconds = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    let (hour, minute) = (time / 3600, (time % 3600) / 60);
    let (hour12, meridiem) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{month}/{day}/{:02}, {hour12}:{minute:02} {meridiem}", year % 100)
}

struct PdfWriter {
    output: Vec<u8>,
    offsets: Vec<usize>,
    limit: usize,
    limit_exceeded: bool,
}

impl PdfWriter {
    fn new(limit: usize) -> Result<Self, RasterPdfError> {
        let mut writer = Self {
            output: Vec::new(),
            offsets: vec![0usize],
            limit,
            limit_exceeded: false,
        };
        writer.append(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n")?;
        Ok(writer)
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), RasterPdfError> {
        let new_len = self
            .output
            .len()
            .checked_add(bytes.len())
            .ok_or(RasterPdfError::OutputLimitExceeded)?;
        if new_len > self.limit {
            self.limit_exceeded = true;
            return Err(RasterPdfError::OutputLimitExceeded);
        }
        self.output.extend_from_slice(bytes);
        Ok(())
    }

    /// Record where object `id` starts. Ids may be written in any order but
    /// every id up to the highest must be written before `finish`.
    fn mark(&mut self, id: usize) {
        if self.offsets.len() <= id {
            self.offsets.resize(id + 1, 0);
        }
        self.offsets[id] = self.output.len();
    }

    fn write_object(&mut self, id: usize, body: &[u8]) -> Result<(), RasterPdfError> {
        self.mark(id);
        self.append(format!("{id} 0 obj\n").as_bytes())?;
        self.append(body)?;
        self.append(b"\nendobj\n")
    }

    fn write_rgb_image(&mut self, id: usize, rgb: &image::RgbImage) -> Result<(), RasterPdfError> {
        self.mark(id);
        self.append(format!(
            "{id} 0 obj\n<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length ",
            rgb.width(), rgb.height(),
        ).as_bytes())?;
        // Encode into the final PDF rather than building a second JPEG Vec.
        // A fixed-width decimal token lets us patch /Length after encoding.
        const LENGTH_DIGITS: usize = 20;
        let length_offset = self.output.len();
        self.append(b"00000000000000000000 >>\nstream\n")?;
        let stream_offset = self.output.len();
        let encode_result = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut *self, 90)
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            );
        if let Err(error) = encode_result {
            return if self.limit_exceeded {
                Err(RasterPdfError::OutputLimitExceeded)
            } else {
                Err(RasterPdfError::ImageEncode(error.to_string()))
            };
        }
        let stream_len = self.output.len() - stream_offset;
        let length = format!("{stream_len:0LENGTH_DIGITS$}");
        if length.len() != LENGTH_DIGITS {
            return Err(RasterPdfError::OutputLimitExceeded);
        }
        self.output[length_offset..length_offset + LENGTH_DIGITS]
            .copy_from_slice(length.as_bytes());
        self.append(b"\nendstream\nendobj\n")
    }

    fn finish(mut self) -> Result<Vec<u8>, RasterPdfError> {
        let object_count = self.offsets.len() - 1;
        let xref_offset = self.output.len();
        self.append(format!("xref\n0 {}\n0000000000 65535 f \n", object_count + 1).as_bytes())?;
        for index in 1..self.offsets.len() {
            let offset = self.offsets[index];
            self.append(format!("{offset:010} 00000 n \n").as_bytes())?;
        }
        self.append(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
                object_count + 1,
            )
            .as_bytes(),
        )?;
        Ok(self.output)
    }
}

impl io::Write for PdfWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.append(bytes)
            .map(|()| bytes.len())
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// Decode the actual JPEG XObjects emitted into the PDF, rather than
    /// trusting pagination options or writer-internal page counters.
    fn pdf_page_rasters(pdf: &[u8]) -> Vec<image::RgbImage> {
        let mut pages = Vec::new();
        let mut cursor = 0usize;
        while let Some(relative) = find_bytes(&pdf[cursor..], b"/Subtype /Image") {
            let image_object = cursor + relative;
            let length_key = image_object
                + find_bytes(&pdf[image_object..], b"/Length ").expect("image /Length");
            let mut digits_start = length_key + b"/Length ".len();
            while pdf[digits_start].is_ascii_whitespace() {
                digits_start += 1;
            }
            let mut digits_end = digits_start;
            while pdf[digits_end].is_ascii_digit() {
                digits_end += 1;
            }
            let length = std::str::from_utf8(&pdf[digits_start..digits_end])
                .expect("ASCII image length")
                .parse::<usize>()
                .expect("numeric image length");
            let stream_start = digits_end
                + find_bytes(&pdf[digits_end..], b"stream\n").expect("image stream")
                + b"stream\n".len();
            let stream_end = stream_start.checked_add(length).expect("bounded stream end");
            let raster = image::load_from_memory_with_format(
                &pdf[stream_start..stream_end],
                image::ImageFormat::Jpeg,
            )
            .expect("decodable page JPEG")
            .into_rgb8();
            pages.push(raster);
            cursor = stream_end;
        }
        pages
    }

    fn channel_near(actual: image::Rgb<u8>, expected: [u8; 3]) -> bool {
        actual
            .0
            .into_iter()
            .zip(expected)
            .all(|(actual, expected)| (i16::from(actual) - i16::from(expected)).abs() <= 20)
    }

    #[test]
    fn options_reject_impossible_media_boxes() {
        let mut options = RasterPdfOptions::default();
        options.paper_width_in = 0.0;
        assert_eq!(
            options.page_geometry(),
            Err(RasterPdfError::InvalidPaperSize)
        );
        let mut options = RasterPdfOptions::default();
        options.margin_left_in = 5.0;
        options.margin_right_in = 5.0;
        assert_eq!(options.page_geometry(), Err(RasterPdfError::InvalidMargins));
    }

    #[test]
    fn pagination_preflight_bounds_pages_and_raster_work() {
        let (_, _, printable_width, printable_height, _, _) =
            RasterPdfOptions::default().page_geometry().unwrap();
        let ordinary = pagination_plan(1280.0, 10_000.0, printable_width, printable_height, 1.0)
            .expect("an ordinary multi-page document stays inside the budget");
        let ordinary_slices = page_slices(10_000.0, ordinary.css_page_height, &[]).unwrap();
        assert!(ordinary_slices.len() > 1);
        let ordinary_pages = selected_page_indices(ordinary_slices.len(), &[]).unwrap();
        validate_selected_raster_work(1280.0, &ordinary_slices, &ordinary_pages, 1.0).unwrap();

        let oversized_page =
            pagination_plan(5_000.0, 5_000.0, printable_width, printable_height, 1.0).unwrap();
        let oversized_page_selection =
            selected_page_indices(page_slices(5_000.0, oversized_page.css_page_height, &[]).unwrap().len(), &[]).unwrap();
        assert_eq!(
            validate_selected_raster_work(
                5_000.0,
                &page_slices(5_000.0, oversized_page.css_page_height, &[]).unwrap(),
                &oversized_page_selection,
                1.0,
            )
            .unwrap_err(),
            RasterPdfError::RasterWorkLimitExceeded,
            "one excessively large raster page must fail before capture"
        );

        let too_much_total =
            pagination_plan(1_000.0, 70_000.0, printable_width, printable_height, 1.0).unwrap();
        let too_much_total_selection =
            selected_page_indices(page_slices(70_000.0, too_much_total.css_page_height, &[]).unwrap().len(), &[]).unwrap();
        assert_eq!(
            validate_selected_raster_work(
                1_000.0,
                &page_slices(70_000.0, too_much_total.css_page_height, &[]).unwrap(),
                &too_much_total_selection,
                1.0,
            )
            .unwrap_err(),
            RasterPdfError::RasterWorkLimitExceeded,
            "many individually valid pages must still respect a total work budget"
        );

        let too_many =
            pagination_plan(1_000.0, 400_000.0, printable_width, printable_height, 1.0).unwrap();
        assert_eq!(
            selected_page_indices(page_slices(400_000.0, too_many.css_page_height, &[]).unwrap().len(), &[]).unwrap_err(),
            RasterPdfError::TooManyPages(MAX_PDF_PAGES),
        );
    }

    #[test]
    fn selected_ranges_alone_determine_output_and_raster_budgets() {
        let (_, _, printable_width, printable_height, _, _) =
            RasterPdfOptions::default().page_geometry().unwrap();
        let long =
            pagination_plan(1_000.0, 400_000.0, printable_width, printable_height, 1.0).unwrap();
        let long_slices = page_slices(400_000.0, long.css_page_height, &[]).unwrap();
        assert!(long_slices.len() > MAX_PDF_PAGES);
        let selected = selected_page_indices(
            long_slices.len(),
            &[RasterPdfPageRange {
                start: Some(1),
                end: Some(1),
            }],
        )
        .unwrap();
        assert_eq!(selected, vec![0]);
        validate_selected_raster_work(1_000.0, &long_slices, &selected, 1.0).unwrap();

        assert_eq!(
            selected_page_indices(
                long_slices.len(),
                &[RasterPdfPageRange {
                    start: Some(1),
                    end: Some(MAX_PDF_PAGES + 1),
                }],
            ),
            Err(RasterPdfError::TooManyPages(MAX_PDF_PAGES))
        );

        let base = pagination_plan(800.0, 2_000.0, printable_width, printable_height, 1.0)
            .expect("base geometry");
        let impossible_height =
            base.css_page_height * (MAX_PDF_DOCUMENT_PAGES as f32 + 16.0);
        assert_eq!(
            pagination_plan(
                800.0,
                impossible_height,
                printable_width,
                printable_height,
                1.0,
            )
            .unwrap_err(),
            RasterPdfError::TooManyPages(MAX_PDF_DOCUMENT_PAGES)
        );
    }

    #[test]
    fn scale_changes_css_page_span_and_rejects_invalid_values() {
        let (_, _, printable_width, printable_height, _, _) =
            RasterPdfOptions::default().page_geometry().unwrap();
        let normal =
            pagination_plan(800.0, 2_000.0, printable_width, printable_height, 1.0).unwrap();
        let enlarged =
            pagination_plan(800.0, 2_000.0, printable_width, printable_height, 2.0).unwrap();
        assert_eq!(
            enlarged.points_per_css_pixel,
            normal.points_per_css_pixel * 2.0
        );
        assert_eq!(enlarged.css_page_height, normal.css_page_height / 2.0);
        assert!(
            page_slices(2_000.0, enlarged.css_page_height, &[]).unwrap().len()
                >= page_slices(2_000.0, normal.css_page_height, &[]).unwrap().len()
        );
        assert_eq!(
            pagination_plan(800.0, 2_000.0, printable_width, printable_height, 0.09,)
                .unwrap_err(),
            RasterPdfError::InvalidScale
        );
    }

    #[test]
    fn page_ranges_clip_deduplicate_and_preserve_document_order() {
        assert_eq!(selected_page_indices(4, &[]).unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(
            selected_page_indices(
                6,
                &[
                    RasterPdfPageRange {
                        start: Some(3),
                        end: Some(5),
                    },
                    RasterPdfPageRange {
                        start: Some(1),
                        end: Some(3),
                    },
                    RasterPdfPageRange {
                        start: Some(5),
                        end: None,
                    },
                ],
            )
            .unwrap(),
            vec![0, 1, 2, 3, 4, 5]
        );
        assert_eq!(
            selected_page_indices(
                6,
                &[RasterPdfPageRange {
                    start: None,
                    end: Some(2),
                }],
            )
            .unwrap(),
            vec![0, 1]
        );
        assert_eq!(
            selected_page_indices(
                3,
                &[RasterPdfPageRange {
                    start: Some(9),
                    end: Some(12),
                }],
            ),
            Err(RasterPdfError::EmptyPageRange)
        );
    }

    #[test]
    fn raster_pdf_repeats_fixed_content_and_advances_flow_on_every_selected_page() {
        let context = std::sync::Arc::new(crate::BrowserContext::new("pdf-fixed".to_string()));
        let mut page = crate::Page::new("pdf-fixed-page".to_string(), context);
        page.set_viewport((100.0, 80.0));
        let dom = obscura_dom::parse_html(
            r#"<html style="margin:0"><body style="margin:0;width:100px;height:200px">
                <div style="position:fixed;z-index:5;left:0;top:0;width:20px;height:10px;background:#111"></div>
                <div style="height:80px;background:#e02020"></div>
                <div style="height:80px;background:#20c040"></div>
                <div style="height:40px;background:#2050e0"></div>
            </body></html>"#,
        );
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(dom);
        runtime.set_url("https://example.test/pdf-fixed");
        runtime.set_viewport(100.0, 80.0);
        runtime.run_page_init();
        page.js = Some(runtime);

        let options = RasterPdfOptions {
            print_background: true,
            paper_width_in: 100.0 / CSS_PX_PER_INCH,
            paper_height_in: 80.0 / CSS_PX_PER_INCH,
            margin_top_in: 0.0,
            margin_bottom_in: 0.0,
            margin_left_in: 0.0,
            margin_right_in: 0.0,
            ..RasterPdfOptions::default()
        };
        let pdf = page.raster_pdf(options.clone()).expect("three-page PDF");
        // Paper of 100x80 CSS px (75x60pt): print layout is 100px wide, the
        // same as the body, so each page raster is 100x80.
        assert!(String::from_utf8_lossy(&pdf).contains("/MediaBox [0 0 75.000 60.000]"));
        let rasters = pdf_page_rasters(&pdf);
        assert_eq!(rasters.len(), 3);
        assert_eq!(rasters[0].dimensions(), (100, 80));
        assert_eq!(rasters[1].dimensions(), (100, 80));
        assert_eq!(
            rasters[2].dimensions(),
            (100, 40),
            "the final partial page must use its own virtual viewport height"
        );
        for (index, raster) in rasters.iter().enumerate() {
            assert!(
                channel_near(*raster.get_pixel(5, 5), [17, 17, 17]),
                "fixed header missing from decoded page {}: {:?}",
                index + 1,
                raster.get_pixel(5, 5)
            );
        }
        for (index, expected) in [[224, 32, 32], [32, 192, 64], [32, 80, 224]]
            .into_iter()
            .enumerate()
        {
            let raster = &rasters[index];
            assert!(
                channel_near(
                    *raster.get_pixel(raster.width() / 2, raster.height() / 2),
                    expected,
                ),
                "ordinary flow did not advance on page {}",
                index + 1,
            );
        }
        assert_eq!(
            page.js.as_ref().expect("runtime").scroll_offset(),
            (0.0, 0.0),
            "virtual PDF page scrolling must not mutate the live page"
        );

        let mut ranged = options;
        ranged.page_ranges = vec![RasterPdfPageRange {
            start: Some(2),
            end: Some(3),
        }];
        let selected = pdf_page_rasters(&page.raster_pdf(ranged).expect("selected pages"));
        assert_eq!(selected.len(), 2);
        assert!(channel_near(*selected[0].get_pixel(5, 5), [17, 17, 17]));
        assert!(channel_near(*selected[1].get_pixel(5, 5), [17, 17, 17]));
        assert!(channel_near(*selected[0].get_pixel(50, 40), [32, 192, 64]));
        assert!(channel_near(*selected[1].get_pixel(50, 20), [32, 80, 224]));
    }

    #[test]
    fn raster_pdf_selects_print_media_and_restores_screen_render_state() {
        let context = std::sync::Arc::new(crate::BrowserContext::new("pdf-media".to_string()));
        let mut page = crate::Page::new("pdf-media-page".to_string(), context);
        page.set_viewport((100.0, 80.0));
        let dom = obscura_dom::parse_html(
            r#"<!doctype html><html><head>
                <style>
                    html,body{margin:0;width:100px;height:80px;background:#101010}
                    #print-marker,#screen-marker{display:none}
                    @media print {
                        body{background:#2050e0}
                    }
                    @media screen {
                        body{background:#e02020}
                    }
                </style>
                <style media="print">
                    #print-marker{display:block;position:absolute;left:60px;top:10px;
                                  width:30px;height:30px;background:#f0d020}
                </style>
                <style media="screen">
                    #screen-marker{display:block;position:absolute;left:5px;top:5px;
                                   width:10px;height:10px;background:#20c040}
                </style>
            </head><body><div id="print-marker"></div><div id="screen-marker"></div></body></html>"#,
        );
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(dom);
        runtime.set_url("https://example.test/pdf-media");
        runtime.set_viewport(100.0, 80.0);
        runtime.run_page_init();
        page.js = Some(runtime);

        let screen_before = page.screenshot((100.0, 80.0)).expect("screen before PDF");
        let screen_before_pixels =
            image::load_from_memory_with_format(&screen_before, image::ImageFormat::Png)
                .expect("screen PNG")
                .into_rgb8();
        assert_eq!(screen_before_pixels.get_pixel(50, 60).0, [224, 32, 32]);
        assert_eq!(screen_before_pixels.get_pixel(8, 8).0, [32, 192, 64]);
        assert_eq!(
            screen_before_pixels.get_pixel(70, 20).0,
            [224, 32, 32],
            "media=print marker must stay out of the screen cascade"
        );

        let options = RasterPdfOptions {
            print_background: true,
            paper_width_in: 100.0 / CSS_PX_PER_INCH,
            paper_height_in: 80.0 / CSS_PX_PER_INCH,
            margin_top_in: 0.0,
            margin_bottom_in: 0.0,
            margin_left_in: 0.0,
            margin_right_in: 0.0,
            ..RasterPdfOptions::default()
        };
        let pages = pdf_page_rasters(&page.raster_pdf(options).expect("print-media PDF"));
        assert_eq!(pages.len(), 1);
        let printed = &pages[0];
        assert!(
            channel_near(*printed.get_pixel(50, 60), [32, 80, 224]),
            "@media print body color missing: {:?}",
            printed.get_pixel(50, 60)
        );
        assert!(
            channel_near(*printed.get_pixel(70, 20), [240, 208, 32]),
            "media=print stylesheet marker missing: {:?}",
            printed.get_pixel(70, 20)
        );
        assert!(
            channel_near(*printed.get_pixel(8, 8), [32, 80, 224]),
            "media=screen marker leaked into print: {:?}",
            printed.get_pixel(8, 8)
        );

        let screen_after = page.screenshot((100.0, 80.0)).expect("screen after PDF");
        assert_eq!(
            screen_after, screen_before,
            "temporary print cascade must not poison retained screen geometry or stylesheet cache"
        );
    }

    fn runtime_page(html: &str, id: &str) -> crate::Page {
        let context = std::sync::Arc::new(crate::BrowserContext::new(id.to_string()));
        let mut page = crate::Page::new(format!("{id}-page"), context);
        page.set_viewport((1280.0, 720.0));
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(obscura_dom::parse_html(html));
        runtime.set_url(&format!("https://example.test/{id}"));
        runtime.set_viewport(1280.0, 720.0);
        runtime.run_page_init();
        page.js = Some(runtime);
        page
    }

    #[test]
    fn page_breaks_move_above_lines_they_would_cut() {
        // A 100px page over lines 20px tall starting at 0, 20, ... 180 and
        // one starting at 95: the first break (100) cuts the line at 95.
        let mut lines: Vec<(f32, f32)> = (0..10).map(|i| (i as f32 * 20.0, i as f32 * 20.0 + 20.0)).collect();
        lines.push((95.0, 115.0));
        let slices = page_slices(200.0, 100.0, &lines).unwrap();
        assert_eq!(slices[0], (0.0, 95.0));
        assert_eq!(slices[1].0, 95.0);
        assert_eq!(slices.last().unwrap().1, 200.0);
        // A line taller than half a page is cut rather than leaving a page
        // mostly empty.
        let slices = page_slices(200.0, 100.0, &[(30.0, 160.0)]).unwrap();
        assert_eq!(slices[0], (0.0, 100.0));
    }

    #[test]
    fn pdf_text_is_selectable_and_layout_follows_the_paper_width() {
        let text = "Searchable caf\u{e9} text that wraps at the printable width";
        let page = runtime_page(
            &format!("<html><body style=\"margin:0;font:16px sans-serif\"><p id=\"p\">{text}</p></body></html>"),
            "pdf-text",
        );
        // 200 CSS px wide paper without margins: the paragraph must wrap.
        let options = RasterPdfOptions {
            paper_width_in: 200.0 / CSS_PX_PER_INCH,
            paper_height_in: 400.0 / CSS_PX_PER_INCH,
            margin_top_in: 0.0,
            margin_bottom_in: 0.0,
            margin_left_in: 0.0,
            margin_right_in: 0.0,
            ..RasterPdfOptions::default()
        };
        let pdf = page.raster_pdf(options).expect("PDF");
        let raw = String::from_utf8_lossy(&pdf);
        assert!(raw.contains("/ToUnicode"), "text layer font carries a ToUnicode map");
        assert!(raw.contains("3 Tr"), "text layer is invisible");
        let hex = |s: &str| s.encode_utf16().map(|u| format!("{u:04X}")).collect::<String>();
        assert!(raw.contains(&hex("Searchable")), "the words are in the text layer");
        assert!(raw.contains(&hex("caf\u{e9}")), "non-ASCII text keeps its code points");
        let lines = raw.matches(" Tj\n").count();
        // About 430px of text: one line on a 1280px screen, two on the paper.
        assert!(lines >= 2, "the paragraph wrapped at the 200px paper width into {lines} lines");
        let rasters = pdf_page_rasters(&pdf);
        assert_eq!(rasters[0].width(), 200, "the raster spans the print layout width");
    }

    #[test]
    fn header_footer_and_outline_are_written() {
        let page = runtime_page(
            "<html><head><title>Doc Title</title></head><body style=\"margin:0\">\
             <h1>Intro</h1><p>one</p><h2>Details</h2><p>two</p><h1>End</h1></body></html>",
            "pdf-extras",
        );
        let options = RasterPdfOptions {
            display_header_footer: true,
            footer_template: "<div style=\"font-size:10px;text-align:center\">Page <span class=\"pageNumber\"></span> of <span class=\"totalPages\"></span></div>".into(),
            generate_document_outline: true,
            ..RasterPdfOptions::default()
        };
        let pdf = page.raster_pdf(options).expect("PDF");
        let raw = String::from_utf8_lossy(&pdf);
        assert!(raw.contains("(Page 1 of 1) Tj"), "footer template with page values");
        assert!(raw.contains("/Outlines"));
        assert!(raw.contains(&pdf_text_string("Intro")));
        assert!(raw.contains(&pdf_text_string("Details")));
        // Details nests under Intro; End is Intro's sibling.
        assert!(raw.contains("/Type /Outlines") && raw.contains("/Count 3"));
    }

    #[test]
    fn templates_render_values_and_alignment() {
        let values = [
            ("date", "1/2/26, 3:04 PM".to_string()),
            ("title", "T".to_string()),
            ("url", "https://u.test/".to_string()),
            ("pageNumber", "2".to_string()),
            ("totalPages", "5".to_string()),
        ];
        let line = render_template(DEFAULT_FOOTER_TEMPLATE, &values).unwrap();
        assert_eq!(line.font_size_px, 8.0);
        assert_eq!(
            line.parts,
            vec![("https://u.test/".to_string(), Align::Left), ("2/5".to_string(), Align::Right)]
        );
        let line = render_template(
            "<p style='font-size:12pt;text-align:right'>Page <b class='pageNumber'></b></p>",
            &values,
        )
        .unwrap();
        assert_eq!(line.font_size_px, 16.0);
        assert_eq!(line.parts, vec![("Page 2".to_string(), Align::Right)]);
        assert!(render_template("", &values).is_none());
    }

    #[test]
    fn css_page_sizes_parse() {
        assert_eq!(page_size_from_css("@page { size: A4 }"), Some((8.27, 11.69)));
        assert_eq!(page_size_from_css("@page{margin:0;size:letter landscape}"), Some((11.0, 8.5)));
        assert_eq!(page_size_from_css("@page :first { size: 5in 7in; }"), Some((5.0, 7.0)));
        assert_eq!(page_size_from_css("@page { size: 210mm }").map(|(w, _)| (w * 100.0).round()), Some(827.0));
        assert_eq!(page_size_from_css("p { size: a4 }"), None);
        assert_eq!(page_size_from_css("@page { margin: 1in }"), None);
    }

    #[test]
    fn print_date_matches_chrome_format() {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_380_800 + 15 * 3600 + 5 * 60);
        assert_eq!(format_print_date(when), "9/26/26, 3:05 PM");
        assert_eq!(format_print_date(std::time::UNIX_EPOCH), "1/1/70, 12:00 AM");
    }

    #[test]
    fn writer_emits_xref_and_one_image_per_page() {
        let pdf = encode_pdf_pages(1, PageGeometry { page_width: 612.0, page_height: 792.0, left: 36.0, bottom: 36.0, printable_height: 720.0 }, &[], |_| {
            Ok(RasterPage {
                rgb: image::RgbImage::from_pixel(2, 3, image::Rgb([10, 20, 30])),
                draw_width_pt: 100.0,
                draw_height_pt: 150.0,
                overlay: String::new(),
                _lifetime_probe: None,
            })
        })
        .unwrap();
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Count 1"));
        assert!(text.contains("/Subtype /Image"));
        // Catalog, pages, four text-layer font objects, Helvetica, and one
        // page's page/content/image objects.
        assert!(text.contains("xref\n0 11"));
        let startxref = text
            .rsplit_once("startxref\n")
            .unwrap()
            .1
            .lines()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert!(pdf[startxref..].starts_with(b"xref\n"));
        let object_one_offset = text
            .split("xref\n0 11\n")
            .nth(1)
            .unwrap()
            .lines()
            .nth(1)
            .unwrap()[..10]
            .parse::<usize>()
            .unwrap();
        assert!(pdf[object_one_offset..].starts_with(b"1 0 obj\n"));
    }

    #[test]
    fn page_rasters_are_released_before_capturing_the_next_page() {
        let previous = std::cell::RefCell::new(None::<std::rc::Weak<()>>);
        let pdf = encode_pdf_pages(4, PageGeometry { page_width: 612.0, page_height: 792.0, left: 36.0, bottom: 36.0, printable_height: 720.0 }, &[], |index| {
            if let Some(previous) = previous.borrow().as_ref() {
                assert!(
                    previous.upgrade().is_none(),
                    "page {index} was requested while the prior raster was still retained"
                );
            }
            let probe = std::rc::Rc::new(());
            *previous.borrow_mut() = Some(std::rc::Rc::downgrade(&probe));
            Ok(RasterPage {
                rgb: image::RgbImage::from_pixel(8, 8, image::Rgb([index as u8, 0, 0])),
                draw_width_pt: 100.0,
                draw_height_pt: 100.0,
                overlay: String::new(),
                _lifetime_probe: Some(probe),
            })
        })
        .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&pdf)
                .matches("/Subtype /Image")
                .count(),
            4
        );
    }

    #[test]
    fn writer_enforces_the_output_limit_while_encoding_the_image_stream() {
        let mut writer = PdfWriter::new(600).unwrap();
        writer
            .write_object(1, b"<< /Type /Catalog /Pages 2 0 R >>")
            .unwrap();
        writer
            .write_object(2, b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>")
            .unwrap();
        let noisy = image::RgbImage::from_fn(128, 128, |x, y| {
            image::Rgb([
                x.wrapping_mul(37) as u8,
                y.wrapping_mul(53) as u8,
                x.wrapping_add(y).wrapping_mul(71) as u8,
            ])
        });
        assert_eq!(
            writer.write_rgb_image(5, &noisy),
            Err(RasterPdfError::OutputLimitExceeded)
        );
        assert!(writer.output.len() <= 600);
    }
}
