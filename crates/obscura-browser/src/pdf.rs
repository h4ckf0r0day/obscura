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
    #[cfg(test)]
    _lifetime_probe: Option<std::rc::Rc<()>>,
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
        options: RasterPdfOptions,
        animation_sample: obscura_js::AnimationSample,
    ) -> Result<Vec<u8>, RasterPdfError> {
        let (page_width, page_height, printable_width, printable_height, left, bottom) =
            options.page_geometry()?;
        let js = self
            .js
            .as_ref()
            .ok_or(RasterPdfError::NoRenderableDocument)?;
        if !js.set_animation_sample(animation_sample) {
            return Err(RasterPdfError::NoRenderableDocument);
        }
        let previous_media = js.set_render_media(obscura_js::CssMediaType::Print);
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
            validate_selected_raster_work(content_width, content_height, plan, &selected_pages)?;

            encode_pdf_pages(
                selected_pages.len(),
                page_width,
                page_height,
                left,
                bottom,
                printable_height,
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
                    Ok(RasterPage {
                        rgb,
                        draw_width_pt: content_width * plan.points_per_css_pixel,
                        draw_height_pt: slice_height * plan.points_per_css_pixel,
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

fn encode_pdf_pages(
    page_count: usize,
    page_width: f32,
    page_height: f32,
    left: f32,
    bottom: f32,
    printable_height: f32,
    mut page_source: impl FnMut(usize) -> Result<RasterPage, RasterPdfError>,
) -> Result<Vec<u8>, RasterPdfError> {
    let object_count = 2usize
        .checked_add(
            page_count
                .checked_mul(3)
                .ok_or(RasterPdfError::OutputLimitExceeded)?,
        )
        .ok_or(RasterPdfError::OutputLimitExceeded)?;
    let mut writer = PdfWriter::new(object_count, MAX_PDF_OUTPUT_BYTES)?;
    writer.write_object(1, b"<< /Type /Catalog /Pages 2 0 R >>")?;

    let kids = (0..page_count)
        .map(|index| format!("{} 0 R", 3 + index * 3))
        .collect::<Vec<_>>()
        .join(" ");
    let pages_dictionary = format!("<< /Type /Pages /Count {page_count} /Kids [{kids}] >>");
    writer.write_object(2, pages_dictionary.as_bytes())?;

    for index in 0..page_count {
        let page = page_source(index)?;
        let page_id = 3 + index * 3;
        let content_id = page_id + 1;
        let image_id = page_id + 2;
        let page_dictionary = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {page_width:.3} {page_height:.3}] /Resources << /XObject << /Im0 {image_id} 0 R >> >> /Contents {content_id} 0 R >>"
        );
        writer.write_object(page_id, page_dictionary.as_bytes())?;

        let draw_y = bottom + printable_height - page.draw_height_pt;
        let commands = format!(
            "q\n{:.3} 0 0 {:.3} {:.3} {:.3} cm\n/Im0 Do\nQ\n",
            page.draw_width_pt, page.draw_height_pt, left, draw_y,
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
        validate_selected_raster_work(1280.0, 10_000.0, ordinary, &ordinary_pages).unwrap();

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
        validate_selected_raster_work(1_000.0, 400_000.0, long, &selected).unwrap();

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
    fn writer_emits_xref_and_one_image_per_page() {
        let pdf = encode_pdf_pages(1, 612.0, 792.0, 36.0, 36.0, 720.0, |_| {
            Ok(RasterPage {
                rgb: image::RgbImage::from_pixel(2, 3, image::Rgb([10, 20, 30])),
                draw_width_pt: 100.0,
                draw_height_pt: 150.0,
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
        let pdf = encode_pdf_pages(4, 612.0, 792.0, 36.0, 36.0, 720.0, |index| {
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
}
