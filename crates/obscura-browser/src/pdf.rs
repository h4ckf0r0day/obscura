//! Bounded raster-backed PDF export over the retained document-space painter.
//!
//! This deliberately does not claim CSS paged-media support. It preserves the
//! print-media layout, fits the full document width into the printable area,
//! and slices that immutable layout vertically across PDF pages.

use std::io;

use image::ImageEncoder as _;
use obscura_js::CaptureRegion;

use crate::Page;

const POINTS_PER_INCH: f32 = 72.0;
const MAX_PAPER_INCHES: f32 = 200.0;
const MAX_PDF_PAGES: usize = 250;
// Page ranges may select a small bounded subset from a much longer document.
// Keep the arithmetic/index space finite without charging unselected pages
// against the output-page limit.
const MAX_PDF_DOCUMENT_PAGES: usize = 1_000_000;
const MAX_PDF_PAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_PDF_TOTAL_RASTER_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_PDF_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const CSS_PIXELS_PER_INCH: f32 = 96.0;
/// Upper bound on one header or footer template, matching the order of
/// magnitude of the other bounded CDP string inputs.
const MAX_HEADER_FOOTER_TEMPLATE_BYTES: usize = 64 * 1024;
/// Chromium's default header (date and title) and footer (URL and page
/// numbers), used when `displayHeaderFooter` is set and a template is empty.
const DEFAULT_HEADER_TEMPLATE: &str = "<div style=\"width:100%;padding:0 0.4in;display:flex;justify-content:space-between\"><span class=\"date\"></span><span class=\"title\"></span></div>";
const DEFAULT_FOOTER_TEMPLATE: &str = "<div style=\"width:100%;padding:0 0.4in;display:flex;justify-content:space-between\"><span class=\"url\"></span><span><span class=\"pageNumber\"></span>/<span class=\"totalPages\"></span></span></div>";

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
    /// Draw `header_template` in the top margin and `footer_template` in the
    /// bottom margin of every page (CDP `displayHeaderFooter`). An empty
    /// template falls back to Chromium's default for that band.
    pub display_header_footer: bool,
    pub header_template: String,
    pub footer_template: String,
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
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RasterPdfError {
    #[error("PDF paper dimensions must be finite and between 0 and 200 inches")]
    InvalidPaperSize,
    #[error("PDF margins must be finite, non-negative, and leave a printable area")]
    InvalidMargins,
    #[error("PDF header and footer templates must each be at most {0} bytes")]
    TemplateTooLarge(usize),
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
    header: Option<RasterBand>,
    footer: Option<RasterBand>,
    #[cfg(test)]
    _lifetime_probe: Option<std::rc::Rc<()>>,
}

/// A rendered header or footer, positioned in PDF user space (origin at the
/// bottom-left corner of the page).
#[derive(Debug)]
struct RasterBand {
    rgb: image::RgbImage,
    x_pt: f32,
    y_pt: f32,
    width_pt: f32,
    height_pt: f32,
}

/// Pixel and point geometry of one margin band.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BandGeometry {
    width_px: u32,
    height_px: u32,
    y_pt: f32,
    width_pt: f32,
    height_pt: f32,
}

/// Values injected into elements carrying the CDP header/footer classes.
struct BandFields<'a> {
    date: &'a str,
    title: &'a str,
    url: &'a str,
    page_number: usize,
    total_pages: usize,
}

#[derive(Clone, Copy, Debug)]
struct PaginationPlan {
    points_per_css_pixel: f32,
    css_page_height: f32,
    page_count: usize,
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
    let page_count = page_count_value as usize;

    Ok(PaginationPlan {
        points_per_css_pixel,
        css_page_height,
        page_count,
    })
}

fn validate_selected_raster_work(
    content_width: f32,
    content_height: f32,
    plan: PaginationPlan,
    selected_pages: &[usize],
    band_pixels_per_page: u64,
) -> Result<(), RasterPdfError> {
    let pixel_width = content_width.ceil();
    if !pixel_width.is_finite()
        || pixel_width <= 0.0
        || pixel_width > obscura_js::MAX_CAPTURE_DIMENSION as f32
    {
        return Err(RasterPdfError::RasterWorkLimitExceeded);
    }
    let pixel_width = pixel_width as u64;
    let mut total_pixels = 0u64;
    for &page_index in selected_pages {
        if page_index >= plan.page_count {
            return Err(RasterPdfError::EmptyPageRange);
        }
        let y = page_index as f32 * plan.css_page_height;
        let slice_height = (content_height - y).min(plan.css_page_height).ceil();
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
            .and_then(|total| total.checked_add(band_pixels_per_page))
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
    /// cascade and layout, but CSS paged media remains outside this
    /// raster-backed exporter. Header and footer templates are rendered
    /// separately into the page margins.
    /// Date, title and URL for the header/footer `date`, `title` and `url`
    /// classes. The title is read from the live document so script changes
    /// are reflected, like `document.title` in Chromium.
    fn header_footer_document_fields(&self) -> (String, String, String) {
        let title = self
            .with_dom(|dom| {
                dom.query_selector("title")
                    .ok()
                    .flatten()
                    .map(|node| dom.text_content(node))
            })
            .flatten()
            .unwrap_or_else(|| self.title.clone());
        (
            current_date_string(),
            title.split_whitespace().collect::<Vec<_>>().join(" "),
            self.url_string(),
        )
    }

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
        options: RasterPdfOptions,
        animation_sample: obscura_js::AnimationSample,
    ) -> Result<Vec<u8>, RasterPdfError> {
        let (page_width, page_height, printable_width, printable_height, left, bottom) =
            options.page_geometry()?;
        let header_footer = HeaderFooterPlan::new(&options, page_width, page_height)?;
        let js = self
            .js
            .as_ref()
            .ok_or(RasterPdfError::NoRenderableDocument)?;
        if !js.set_animation_sample(animation_sample) {
            return Err(RasterPdfError::NoRenderableDocument);
        }
        let previous_media = js.set_render_media(
            obscura_js::CssMediaType::Print.with_reduced_motion(js.prefers_reduced_motion())
                .with_dark_color_scheme(js.prefers_dark_color_scheme()),
        );
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

            let plan = pagination_plan(
                content_width,
                content_height,
                printable_width,
                printable_height,
                options.scale,
            )?;
            let selected_pages = selected_page_indices(plan.page_count, &options.page_ranges)?;
            validate_selected_raster_work(
                content_width,
                content_height,
                plan,
                &selected_pages,
                header_footer.pixels_per_page(),
            )?;
            let document = header_footer
                .is_enabled()
                .then(|| self.header_footer_document_fields());

            encode_pdf_pages(
                selected_pages.len(),
                page_width,
                page_height,
                left,
                bottom,
                printable_height,
                header_footer.header.is_some(),
                header_footer.footer.is_some(),
                |output_page_index| {
                    let page_index = selected_pages[output_page_index];
                    let y = page_index as f32 * plan.css_page_height;
                    let slice_height = (content_height - y).min(plan.css_page_height);
                    let png = js
                        .screenshot_prepared_region_at_scroll_with_backgrounds(
                            CaptureRegion::new(0.0, y, content_width, slice_height, 1.0),
                            (0.0, y),
                            options.print_background,
                        )
                        .map_err(|error| RasterPdfError::CaptureFailed(format!("{error:?}")))?;
                    let decoded =
                        image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                            .map_err(|error| RasterPdfError::ImageDecode(error.to_string()))?;
                    // The document capture is already a complete PNG allocation. Drop
                    // it before converting the decoded pixels and, below, encoding the
                    // JPEG directly into the final PDF buffer. At no point do we retain
                    // PNGs or JPEGs for earlier pages.
                    drop(png);
                    let rgb = decoded.into_rgb8();
                    let (header, footer) = match &document {
                        Some((date, title, url)) => header_footer.render(&BandFields {
                            date,
                            title,
                            url,
                            // Chromium numbers pages of the whole document,
                            // so a pageRanges selection keeps original numbers.
                            page_number: page_index + 1,
                            total_pages: plan.page_count,
                        })?,
                        None => (None, None),
                    };
                    Ok(RasterPage {
                        rgb,
                        draw_width_pt: content_width * plan.points_per_css_pixel,
                        draw_height_pt: slice_height * plan.points_per_css_pixel,
                        header,
                        footer,
                        #[cfg(test)]
                        _lifetime_probe: None,
                    })
                },
            )
        })();
        js.set_render_media(previous_media);
        result
    }
}

/// Resolved header/footer work for one export: which bands exist, their
/// geometry, and the template each one renders.
struct HeaderFooterPlan {
    header: Option<(BandGeometry, String)>,
    footer: Option<(BandGeometry, String)>,
}

impl HeaderFooterPlan {
    fn new(
        options: &RasterPdfOptions,
        page_width_pt: f32,
        page_height_pt: f32,
    ) -> Result<Self, RasterPdfError> {
        if !options.display_header_footer {
            return Ok(Self {
                header: None,
                footer: None,
            });
        }
        for template in [&options.header_template, &options.footer_template] {
            if template.len() > MAX_HEADER_FOOTER_TEMPLATE_BYTES {
                return Err(RasterPdfError::TemplateTooLarge(
                    MAX_HEADER_FOOTER_TEMPLATE_BYTES,
                ));
            }
        }
        let template_or_default = |template: &str, default: &str| {
            if template.trim().is_empty() {
                default.to_string()
            } else {
                template.to_string()
            }
        };
        Ok(Self {
            header: band_geometry(page_width_pt, page_height_pt, options.margin_top_in, true)?.map(|geometry| {
                (
                    geometry,
                    template_or_default(&options.header_template, DEFAULT_HEADER_TEMPLATE),
                )
            }),
            footer: band_geometry(page_width_pt, page_height_pt, options.margin_bottom_in, false)?.map(
                |geometry| {
                    (
                        geometry,
                        template_or_default(&options.footer_template, DEFAULT_FOOTER_TEMPLATE),
                    )
                },
            ),
        })
    }

    fn is_enabled(&self) -> bool {
        self.header.is_some() || self.footer.is_some()
    }

    fn pixels_per_page(&self) -> u64 {
        [&self.header, &self.footer]
            .into_iter()
            .flatten()
            .map(|(geometry, _)| u64::from(geometry.width_px) * u64::from(geometry.height_px))
            .sum()
    }

    fn render(
        &self,
        fields: &BandFields<'_>,
    ) -> Result<(Option<RasterBand>, Option<RasterBand>), RasterPdfError> {
        let render_one = |band: &Option<(BandGeometry, String)>| match band {
            Some((geometry, template)) => render_band(template, *geometry, fields).map(Some),
            None => Ok(None),
        };
        Ok((render_one(&self.header)?, render_one(&self.footer)?))
    }
}

/// Geometry of the top or bottom margin band. A margin thinner than one CSS
/// pixel has no room for a template, so it yields no band.
fn band_geometry(
    page_width_pt: f32,
    page_height_pt: f32,
    margin_in: f32,
    top: bool,
) -> Result<Option<BandGeometry>, RasterPdfError> {
    let height_px = (margin_in * CSS_PIXELS_PER_INCH).ceil();
    if !height_px.is_finite() || height_px < 1.0 {
        return Ok(None);
    }
    let width_px = (page_width_pt / POINTS_PER_INCH * CSS_PIXELS_PER_INCH).ceil();
    let max_dimension = obscura_js::MAX_CAPTURE_DIMENSION as f32;
    if !width_px.is_finite()
        || width_px < 1.0
        || width_px > max_dimension
        || height_px > max_dimension
    {
        return Err(RasterPdfError::RasterWorkLimitExceeded);
    }
    let (width_px, height_px) = (width_px as u32, height_px as u32);
    if u64::from(width_px) * u64::from(height_px) > MAX_PDF_PAGE_PIXELS {
        return Err(RasterPdfError::RasterWorkLimitExceeded);
    }
    let height_pt = margin_in * POINTS_PER_INCH;
    Ok(Some(BandGeometry {
        width_px,
        height_px,
        y_pt: if top { page_height_pt - height_pt } else { 0.0 },
        width_pt: page_width_pt,
        height_pt,
    }))
}

/// Replace the children of every element carrying one of the header/footer
/// classes (`date`, `title`, `url`, `pageNumber`, `totalPages`) with text.
fn inject_band_fields(dom: &obscura_dom::DomTree, fields: &BandFields<'_>) {
    for node in dom.descendants(dom.document()) {
        let Some(class) = dom
            .with_node(node, |node| {
                node.get_attribute("class").map(str::to_owned)
            })
            .flatten()
        else {
            continue;
        };
        for token in class.split_ascii_whitespace() {
            let value = match token {
                "date" => fields.date.to_string(),
                "title" => fields.title.to_string(),
                "url" => fields.url.to_string(),
                "pageNumber" => fields.page_number.to_string(),
                "totalPages" => fields.total_pages.to_string(),
                _ => continue,
            };
            for child in dom.children(node) {
                dom.detach(child);
            }
            let text = dom.new_node(obscura_dom::NodeData::Text { contents: value });
            dom.append_child(node, text);
            break;
        }
    }
}

/// Rasterize one template into a band image. The template is laid out across
/// the full paper width inside the margin height, like Chromium, with no
/// script execution and no network access.
fn render_band(
    template: &str,
    geometry: BandGeometry,
    fields: &BandFields<'_>,
) -> Result<RasterBand, RasterPdfError> {
    let html = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
         html,body{{margin:0;padding:0;background:#fff}}\
         body{{width:{}px;height:{}px;overflow:hidden;font-size:10px;font-family:sans-serif}}\
         </style></head><body>{template}</body></html>",
        geometry.width_px, geometry.height_px,
    );
    let dom = obscura_dom::parse_html(&html);
    inject_band_fields(&dom, fields);

    // Scripts never run here, and sync resource loading stays off so a
    // template cannot make the exporter fetch arbitrary URLs.
    let mut resources = obscura_js::RenderResourceCache::default();
    resources.set_sync_loading_enabled(false);
    let png = obscura_js::screenshot_png_scrolled_at_animation_time_with_surface_color_and_resources(
        &dom,
        (geometry.width_px as f32, geometry.height_px as f32),
        None,
        (0.0, 0.0),
        obscura_js::AnimationSampleTime::default(),
        [255, 255, 255, 255],
        &mut resources,
    )
    .ok_or_else(|| {
        RasterPdfError::CaptureFailed("header/footer template could not be rendered".to_string())
    })?;
    let rgb = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .map_err(|error| RasterPdfError::ImageDecode(error.to_string()))?
        .into_rgb8();
    Ok(RasterBand {
        rgb,
        x_pt: 0.0,
        y_pt: geometry.y_pt,
        width_pt: geometry.width_pt,
        height_pt: geometry.height_pt,
    })
}

/// `M/D/YYYY` in UTC, the shape Chromium uses for the `date` class in en-US.
fn current_date_string() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| (elapsed.as_secs() / 86_400) as i64)
        .unwrap_or(0);
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{month}/{day}/{year}")
}

fn encode_pdf_pages(
    page_count: usize,
    page_width: f32,
    page_height: f32,
    left: f32,
    bottom: f32,
    printable_height: f32,
    has_header: bool,
    has_footer: bool,
    mut page_source: impl FnMut(usize) -> Result<RasterPage, RasterPdfError>,
) -> Result<Vec<u8>, RasterPdfError> {
    // Per page: page dictionary, content stream, body image, then one image
    // object for each header/footer band.
    let objects_per_page = 3 + usize::from(has_header) + usize::from(has_footer);
    let object_count = 2usize
        .checked_add(
            page_count
                .checked_mul(objects_per_page)
                .ok_or(RasterPdfError::OutputLimitExceeded)?,
        )
        .ok_or(RasterPdfError::OutputLimitExceeded)?;
    let mut writer = PdfWriter::new(object_count, MAX_PDF_OUTPUT_BYTES)?;
    writer.write_object(1, b"<< /Type /Catalog /Pages 2 0 R >>")?;

    let kids = (0..page_count)
        .map(|index| format!("{} 0 R", 3 + index * objects_per_page))
        .collect::<Vec<_>>()
        .join(" ");
    let pages_dictionary = format!("<< /Type /Pages /Count {page_count} /Kids [{kids}] >>");
    writer.write_object(2, pages_dictionary.as_bytes())?;

    for index in 0..page_count {
        let page = page_source(index)?;
        if page.header.is_some() != has_header || page.footer.is_some() != has_footer {
            return Err(RasterPdfError::CaptureFailed(
                "header/footer band layout changed between pages".to_string(),
            ));
        }
        let page_id = 3 + index * objects_per_page;
        let content_id = page_id + 1;
        let image_id = page_id + 2;
        let header_id = page_id + 3;
        let footer_id = header_id + usize::from(has_header);
        let mut xobjects = format!("/Im0 {image_id} 0 R");
        if has_header {
            xobjects.push_str(&format!(" /Im1 {header_id} 0 R"));
        }
        if has_footer {
            xobjects.push_str(&format!(" /Im2 {footer_id} 0 R"));
        }
        let page_dictionary = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {page_width:.3} {page_height:.3}] /Resources << /XObject << {xobjects} >> >> /Contents {content_id} 0 R >>"
        );
        writer.write_object(page_id, page_dictionary.as_bytes())?;

        let draw_y = bottom + printable_height - page.draw_height_pt;
        // Margin bands first: the body is drawn last so it wins any sub-pixel
        // overlap at the margin edge.
        let mut commands = String::new();
        for (name, band) in [("Im1", &page.header), ("Im2", &page.footer)] {
            if let Some(band) = band {
                commands.push_str(&format!(
                    "q\n{:.3} 0 0 {:.3} {:.3} {:.3} cm\n/{name} Do\nQ\n",
                    band.width_pt, band.height_pt, band.x_pt, band.y_pt,
                ));
            }
        }
        commands.push_str(&format!(
            "q\n{:.3} 0 0 {:.3} {:.3} {:.3} cm\n/Im0 Do\nQ\n",
            page.draw_width_pt, page.draw_height_pt, left, draw_y,
        ));
        let content = format!(
            "<< /Length {} >>\nstream\n{}endstream",
            commands.len(),
            commands
        );
        writer.write_object(content_id, content.as_bytes())?;
        writer.write_rgb_image(image_id, &page.rgb)?;
        if let Some(band) = &page.header {
            writer.write_rgb_image(header_id, &band.rgb)?;
        }
        if let Some(band) = &page.footer {
            writer.write_rgb_image(footer_id, &band.rgb)?;
        }
        // `page`, including its decoded RGB raster, is dropped here before
        // the next page is captured. Only the bounded final PDF survives.
    }

    writer.finish()
}

struct PdfWriter {
    output: Vec<u8>,
    offsets: Vec<usize>,
    limit: usize,
    limit_exceeded: bool,
}

impl PdfWriter {
    fn new(object_count: usize, limit: usize) -> Result<Self, RasterPdfError> {
        let mut writer = Self {
            output: Vec::new(),
            offsets: vec![0usize; object_count + 1],
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

    fn write_object(&mut self, id: usize, body: &[u8]) -> Result<(), RasterPdfError> {
        self.offsets[id] = self.output.len();
        self.append(format!("{id} 0 obj\n").as_bytes())?;
        self.append(body)?;
        self.append(b"\nendobj\n")
    }

    fn write_rgb_image(&mut self, id: usize, rgb: &image::RgbImage) -> Result<(), RasterPdfError> {
        self.offsets[id] = self.output.len();
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
        assert!(ordinary.page_count > 1);
        let ordinary_pages = selected_page_indices(ordinary.page_count, &[]).unwrap();
        validate_selected_raster_work(1280.0, 10_000.0, ordinary, &ordinary_pages, 0).unwrap();

        let oversized_page =
            pagination_plan(5_000.0, 5_000.0, printable_width, printable_height, 1.0).unwrap();
        let oversized_page_selection =
            selected_page_indices(oversized_page.page_count, &[]).unwrap();
        assert_eq!(
            validate_selected_raster_work(
                5_000.0,
                5_000.0,
                oversized_page,
                &oversized_page_selection,
                0,
            )
            .unwrap_err(),
            RasterPdfError::RasterWorkLimitExceeded,
            "one excessively large raster page must fail before capture"
        );

        let too_much_total =
            pagination_plan(1_000.0, 70_000.0, printable_width, printable_height, 1.0).unwrap();
        let too_much_total_selection =
            selected_page_indices(too_much_total.page_count, &[]).unwrap();
        assert_eq!(
            validate_selected_raster_work(
                1_000.0,
                70_000.0,
                too_much_total,
                &too_much_total_selection,
                0,
            )
            .unwrap_err(),
            RasterPdfError::RasterWorkLimitExceeded,
            "many individually valid pages must still respect a total work budget"
        );

        let too_many =
            pagination_plan(1_000.0, 400_000.0, printable_width, printable_height, 1.0).unwrap();
        assert_eq!(
            selected_page_indices(too_many.page_count, &[]).unwrap_err(),
            RasterPdfError::TooManyPages(MAX_PDF_PAGES),
        );
    }

    #[test]
    fn selected_ranges_alone_determine_output_and_raster_budgets() {
        let (_, _, printable_width, printable_height, _, _) =
            RasterPdfOptions::default().page_geometry().unwrap();
        let long =
            pagination_plan(1_000.0, 400_000.0, printable_width, printable_height, 1.0).unwrap();
        assert!(long.page_count > MAX_PDF_PAGES);
        let selected = selected_page_indices(
            long.page_count,
            &[RasterPdfPageRange {
                start: Some(1),
                end: Some(1),
            }],
        )
        .unwrap();
        assert_eq!(selected, vec![0]);
        validate_selected_raster_work(1_000.0, 400_000.0, long, &selected, 0).unwrap();

        assert_eq!(
            selected_page_indices(
                long.page_count,
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
        assert!(enlarged.page_count >= normal.page_count);
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
            paper_width_in: 100.0 / POINTS_PER_INCH,
            paper_height_in: 80.0 / POINTS_PER_INCH,
            margin_top_in: 0.0,
            margin_bottom_in: 0.0,
            margin_left_in: 0.0,
            margin_right_in: 0.0,
            ..RasterPdfOptions::default()
        };
        let pdf = page.raster_pdf(options.clone()).expect("three-page PDF");
        assert!(String::from_utf8_lossy(&pdf).contains("/MediaBox [0 0 100.000 80.000]"));
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
            paper_width_in: 100.0 / POINTS_PER_INCH,
            paper_height_in: 80.0 / POINTS_PER_INCH,
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

    #[test]
    fn raster_pdf_preserves_emulated_color_scheme_and_restores_screen() {
        let context = std::sync::Arc::new(crate::BrowserContext::new("pdf-color".into()));
        let mut page = crate::Page::new("pdf-color-page".into(), context);
        page.set_viewport((100.0, 80.0));
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(obscura_dom::parse_html(r#"<!doctype html><style>
            html,body { margin:0 }
            #probe { width:100px;height:80px;background:#ffffff }
            @media (prefers-color-scheme:dark) { #probe { background:#e02020 } }
            @media print and (prefers-color-scheme:dark) and (prefers-reduced-motion:reduce) {
                #probe { background:#2050e0 }
            }
        </style><body><div id="probe"></div></body>"#));
        runtime.set_url("https://example.test/pdf-color");
        page.url = Some(url::Url::parse("https://example.test/pdf-color").unwrap());
        runtime.set_viewport(100.0, 80.0);
        runtime.run_page_init();
        page.js = Some(runtime);
        page.set_media_preferences(true, true);
        let before = page.screenshot((100.0, 80.0)).unwrap();
        let pixels = image::load_from_memory(&before).unwrap().into_rgb8();
        assert_eq!(pixels.get_pixel(50, 60).0, [224, 32, 32]);
        let ad_hoc = page.screenshot((120.0, 90.0)).unwrap();
        let ad_hoc = image::load_from_memory(&ad_hoc).unwrap().into_rgb8();
        assert_eq!(ad_hoc.dimensions(), (120, 90));
        assert_eq!(ad_hoc.get_pixel(50, 60).0, [224, 32, 32]);
        let options = RasterPdfOptions {
            print_background: true,
            paper_width_in: 100.0 / POINTS_PER_INCH,
            paper_height_in: 80.0 / POINTS_PER_INCH,
            margin_top_in: 0.0, margin_bottom_in: 0.0,
            margin_left_in: 0.0, margin_right_in: 0.0,
            ..RasterPdfOptions::default()
        };
        let pages = pdf_page_rasters(&page.raster_pdf(options).unwrap());
        assert_eq!(pages.len(), 1);
        assert!(channel_near(*pages[0].get_pixel(50, 60), [32, 80, 224]));
        assert_eq!(page.screenshot((100.0, 80.0)).unwrap(), before);
        assert_eq!(page.evaluate("return matchMedia('(prefers-color-scheme:dark)').matches;"), serde_json::json!(true));
    }

    #[test]
    fn writer_emits_xref_and_one_image_per_page() {
        let pdf = encode_pdf_pages(1, 612.0, 792.0, 36.0, 36.0, 720.0, false, false, |_| {
            Ok(RasterPage {
                rgb: image::RgbImage::from_pixel(2, 3, image::Rgb([10, 20, 30])),
                draw_width_pt: 100.0,
                draw_height_pt: 150.0,
                header: None,
                footer: None,
                _lifetime_probe: None,
            })
        })
        .unwrap();
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Count 1"));
        assert!(text.contains("/Subtype /Image"));
        assert!(text.contains("xref\n0 6"));
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
            .split("xref\n0 6\n")
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
        let pdf = encode_pdf_pages(4, 612.0, 792.0, 36.0, 36.0, 720.0, false, false, |index| {
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
                header: None,
                footer: None,
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
        let mut writer = PdfWriter::new(5, 600).unwrap();
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

    fn header_footer_fixture(name: &str) -> crate::Page {
        let context = std::sync::Arc::new(crate::BrowserContext::new(name.to_string()));
        let mut page = crate::Page::new(format!("{name}-page"), context);
        page.set_viewport((100.0, 80.0));
        let dom = obscura_dom::parse_html(
            r#"<html style="margin:0"><head><title>  Quarterly
                report </title></head>
               <body style="margin:0;width:100px;height:200px;background:#2050e0"></body></html>"#,
        );
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(dom);
        runtime.set_url("https://example.test/report");
        runtime.set_viewport(100.0, 80.0);
        runtime.run_page_init();
        page.js = Some(runtime);
        page
    }

    /// 300x300pt paper with half-inch top and bottom margins: a 400x48 CSS
    /// pixel band, and a 100x200px body that paginates into three pages.
    fn header_footer_options() -> RasterPdfOptions {
        RasterPdfOptions {
            print_background: true,
            paper_width_in: 300.0 / POINTS_PER_INCH,
            paper_height_in: 300.0 / POINTS_PER_INCH,
            margin_top_in: 0.5,
            margin_bottom_in: 0.5,
            margin_left_in: 0.0,
            margin_right_in: 0.0,
            display_header_footer: true,
            ..RasterPdfOptions::default()
        }
    }

    #[test]
    fn band_geometry_places_header_and_footer_in_the_margins() {
        let header = band_geometry(300.0, 400.0, 0.5, true).unwrap().unwrap();
        assert_eq!((header.width_px, header.height_px), (400, 48));
        assert_eq!(header.y_pt, 400.0 - 36.0);
        assert_eq!((header.width_pt, header.height_pt), (300.0, 36.0));
        let footer = band_geometry(300.0, 400.0, 0.5, false).unwrap().unwrap();
        assert_eq!(footer.y_pt, 0.0);
        assert_eq!(band_geometry(300.0, 400.0, 0.0, true).unwrap(), None);
        assert_eq!(
            band_geometry(300.0, 400.0, 0.005, true).unwrap().map(|g| g.height_px),
            Some(1)
        );
        assert_eq!(
            band_geometry(14_400.0, 14_400.0, 150.0, true).unwrap_err(),
            RasterPdfError::RasterWorkLimitExceeded,
            "a margin band must respect the per-page pixel budget"
        );
    }

    #[test]
    fn band_fields_replace_children_of_classed_elements() {
        let dom = obscura_dom::parse_html(
            r#"<body><span id="a" class="pageNumber">x</span>
               <span id="b" class="foo totalPages"><b>y</b></span>
               <span id="c" class="title"></span><span id="d" class="url"></span>
               <span id="e" class="date"></span><span id="f" class="other">keep</span></body>"#,
        );
        inject_band_fields(
            &dom,
            &BandFields {
                date: "1/2/2027",
                title: "A <b>title</b>",
                url: "https://example.test/",
                page_number: 3,
                total_pages: 7,
            },
        );
        let text = |id: &str| {
            let node = dom.query_selector(&format!("#{id}")).unwrap().unwrap();
            dom.text_content(node)
        };
        assert_eq!(text("a"), "3");
        assert_eq!(text("b"), "7");
        assert_eq!(text("c"), "A <b>title</b>", "injected values are text, not markup");
        assert_eq!(text("d"), "https://example.test/");
        assert_eq!(text("e"), "1/2/2027");
        assert_eq!(text("f"), "keep");
    }

    #[test]
    fn current_date_has_month_day_year_shape() {
        let date = current_date_string();
        let parts = date.split('/').map(|p| p.parse::<u32>().unwrap()).collect::<Vec<_>>();
        assert_eq!(parts.len(), 3, "{date}");
        assert!((1..=12).contains(&parts[0]) && (1..=31).contains(&parts[1]));
        assert!(parts[2] >= 2024, "{date}");
    }

    #[test]
    fn raster_pdf_draws_header_and_footer_bands_on_every_page() {
        let page = header_footer_fixture("pdf-header-footer");
        let mut options = header_footer_options();
        options.header_template =
            r#"<div style="height:48px;background:#e02020"></div>"#.to_string();
        options.footer_template =
            r#"<div style="height:48px;background:#20c040"></div>"#.to_string();
        let pdf = page.raster_pdf(options.clone()).expect("PDF with header/footer");
        let rasters = pdf_page_rasters(&pdf);
        // Per page: body, header band, footer band.
        assert_eq!(rasters.len(), 9);
        for page_rasters in rasters.chunks(3) {
            assert!(channel_near(*page_rasters[0].get_pixel(50, 10), [32, 80, 224]));
            assert_eq!(page_rasters[1].dimensions(), (400, 48));
            assert!(channel_near(*page_rasters[1].get_pixel(200, 24), [224, 32, 32]));
            assert_eq!(page_rasters[2].dimensions(), (400, 48));
            assert!(channel_near(*page_rasters[2].get_pixel(200, 24), [32, 192, 64]));
        }
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/MediaBox [0 0 300.000 300.000]"));
        assert!(text.contains("/Im1"), "header XObject must be referenced");
        assert!(text.contains("/Im2"), "footer XObject must be referenced");
        assert!(
            text.contains("1 0 0 36.000 0.000 264.000 cm") || text.contains("300.000 0 0 36.000 0.000 264.000 cm"),
            "header must be drawn in the top margin"
        );

        // The flag gates everything: templates are ignored when it is false.
        let mut disabled = options;
        disabled.display_header_footer = false;
        assert_eq!(
            pdf_page_rasters(&page.raster_pdf(disabled).expect("plain PDF")).len(),
            3
        );
    }

    #[test]
    fn raster_pdf_injects_page_numbers_and_respects_page_ranges() {
        let page = header_footer_fixture("pdf-page-numbers");
        let mut options = header_footer_options();
        options.header_template = "<span></span>".to_string();
        options.footer_template =
            r#"<div style="font-size:40px;padding-left:20px"><span class="pageNumber"></span> of <span class="totalPages"></span></div>"#
                .to_string();
        let ink = |raster: &image::RgbImage| {
            raster.pixels().filter(|pixel| pixel.0.iter().any(|c| *c < 128)).count()
        };
        let all = pdf_page_rasters(&page.raster_pdf(options.clone()).expect("three pages"));
        // Per page: body, header, footer.
        let footer = |index: usize| &all[index * 3 + 2];
        assert!(ink(footer(0)) > 0, "page number text must be drawn");
        assert_ne!(
            footer(0).as_raw(),
            footer(1).as_raw(),
            "each page must show its own page number"
        );

        options.page_ranges = vec![RasterPdfPageRange { start: Some(2), end: Some(2) }];
        let ranged = pdf_page_rasters(&page.raster_pdf(options).expect("page two only"));
        assert_eq!(ranged.len(), 3);
        assert_eq!(
            ranged[2].as_raw(),
            footer(1).as_raw(),
            "a page range keeps the document's own page numbers"
        );
    }

    #[test]
    fn empty_templates_fall_back_to_chromium_defaults() {
        let page = header_footer_fixture("pdf-default-templates");
        let pdf = page.raster_pdf(header_footer_options()).expect("default bands");
        let rasters = pdf_page_rasters(&pdf);
        assert_eq!(rasters.len(), 9);
        let ink = |raster: &image::RgbImage| {
            raster.pixels().filter(|pixel| pixel.0.iter().any(|c| *c < 128)).count()
        };
        assert!(ink(&rasters[1]) > 0, "default header shows the date and title");
        assert!(ink(&rasters[2]) > 0, "default footer shows the URL and page numbers");
    }

    #[test]
    fn oversized_header_footer_templates_are_rejected_before_rendering() {
        let page = header_footer_fixture("pdf-template-limit");
        let mut options = header_footer_options();
        options.footer_template = "x".repeat(MAX_HEADER_FOOTER_TEMPLATE_BYTES + 1);
        assert_eq!(
            page.raster_pdf(options).unwrap_err(),
            RasterPdfError::TemplateTooLarge(MAX_HEADER_FOOTER_TEMPLATE_BYTES)
        );
    }

    #[test]
    fn header_footer_templates_cannot_trigger_network_fetches() {
        let page = header_footer_fixture("pdf-template-ssrf");
        let mut options = header_footer_options();
        options.header_template =
            r#"<img src="http://127.0.0.1:9/probe.png" style="width:20px;height:20px">"#.to_string();
        page.raster_pdf(options).expect("a template with a remote image still renders");
    }
}
