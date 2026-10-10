#[cfg(feature = "render")]
use serde_json::json;
use serde_json::Value;

use crate::dispatch::CdpContext;

#[cfg(feature = "render")]
const MAX_BASE64_PDF_BYTES: usize = 64 * 1024 * 1024;
#[cfg(feature = "render")]
const MAX_PAGE_RANGES_BYTES: usize = 16 * 1024;
#[cfg(feature = "render")]
const MAX_PAGE_RANGE_PARTS: usize = 512;

#[cfg(feature = "render")]
fn base64_encoded_len(raw_len: usize) -> Option<usize> {
    raw_len.checked_add(2)?.checked_div(3)?.checked_mul(4)
}

#[cfg(feature = "render")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PdfTransferMode {
    ReturnAsBase64,
    ReturnAsStream,
}

#[cfg(feature = "render")]
#[derive(Clone, Debug)]
struct ParsedPdfOptions {
    raster: obscura_browser::RasterPdfOptions,
    transfer_mode: PdfTransferMode,
    requested_print_background: bool,
    requested_tagged_pdf: bool,
}

#[cfg(feature = "render")]
fn number(params: &Value, name: &str, default: f32) -> Result<f32, String> {
    match params.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite())
            .map(|value| value as f32)
            .ok_or_else(|| format!("Invalid parameters: {name} must be a finite number")),
    }
}

#[cfg(feature = "render")]
fn boolean(params: &Value, name: &str, default: bool) -> Result<bool, String> {
    match params.get(name) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("Invalid parameters: {name} must be a boolean")),
    }
}

#[cfg(feature = "render")]
fn parse_page_ranges(value: &str) -> Result<Vec<obscura_browser::RasterPdfPageRange>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.len() > MAX_PAGE_RANGES_BYTES {
        return Err(format!(
            "Invalid parameters: pageRanges exceeds the {MAX_PAGE_RANGES_BYTES}-byte limit"
        ));
    }

    let mut ranges = Vec::new();
    for (index, part) in value.split(',').enumerate() {
        if index >= MAX_PAGE_RANGE_PARTS {
            return Err(format!(
                "Invalid parameters: pageRanges exceeds the {MAX_PAGE_RANGE_PARTS}-range limit"
            ));
        }
        let part = part.trim();
        if part.is_empty() {
            return Err("Invalid parameters: pageRanges contains an empty range".to_string());
        }
        let page = |text: &str| -> Result<Option<usize>, String> {
            let text = text.trim();
            if text.is_empty() {
                return Ok(None);
            }
            text.parse::<usize>()
                .ok()
                .filter(|page| *page > 0)
                .map(Some)
                .ok_or_else(|| {
                    format!("Invalid parameters: pageRanges has invalid page {text:?}")
                })
        };
        let (start, end) = match part.split_once('-') {
            Some((start, end)) if !end.contains('-') => (page(start)?, page(end)?),
            Some(_) => {
                return Err(format!(
                    "Invalid parameters: pageRanges has invalid range {part:?}"
                ))
            }
            None => {
                let page = page(part)?.ok_or_else(|| {
                    "Invalid parameters: pageRanges contains an empty page".to_string()
                })?;
                (Some(page), Some(page))
            }
        };
        if start.is_none() && end.is_none() {
            return Err("Invalid parameters: pageRanges range '-' is empty".to_string());
        }
        if matches!((start, end), (Some(start), Some(end)) if start > end) {
            return Err(format!(
                "Invalid parameters: pageRanges range {part:?} is descending"
            ));
        }
        ranges.push(obscura_browser::RasterPdfPageRange { start, end });
    }

    // Normalize before crossing the CDP/browser boundary. This bounds later
    // selection work and makes repeated or overlapping user ranges occupy one
    // entry while preserving Chromium's document-order, print-once semantics.
    ranges.sort_by_key(|range| (range.start.unwrap_or(1), range.end.unwrap_or(usize::MAX)));
    let mut normalized: Vec<obscura_browser::RasterPdfPageRange> = Vec::new();
    for range in ranges {
        let start = range.start.unwrap_or(1);
        if let Some(previous) = normalized.last_mut() {
            let previous_end = previous.end.unwrap_or(usize::MAX);
            if start <= previous_end.saturating_add(1) {
                previous.end = match (previous.end, range.end) {
                    (None, _) | (_, None) => None,
                    (Some(previous), Some(next)) => Some(previous.max(next)),
                };
                continue;
            }
        }
        normalized.push(obscura_browser::RasterPdfPageRange {
            start: Some(start),
            end: range.end,
        });
    }
    Ok(normalized)
}

#[cfg(feature = "render")]
fn parse_options(params: &Value) -> Result<ParsedPdfOptions, String> {
    if !params.is_object() {
        return Err("Invalid parameters: expected an object".to_string());
    }
    let unsupported_true = [
        ("displayHeaderFooter", "headers and footers"),
        ("preferCSSPageSize", "CSS @page sizing"),
        ("generateDocumentOutline", "document outlines"),
    ];
    for (name, capability) in unsupported_true {
        if boolean(params, name, false)? {
            return Err(format!(
                "Page.printToPDF does not yet support {capability} ({name}=true)"
            ));
        }
    }
    let requested_print_background = boolean(params, "printBackground", false)?;
    // Puppeteer currently sends true by default. Raster PDFs have no semantic
    // structure to tag, but rejecting the request makes the standard client
    // unusable. Accept and truthfully report `taggedPdf:false` in the response.
    let requested_tagged_pdf = boolean(params, "generateTaggedPDF", false)?;
    let scale = number(params, "scale", 1.0)?;
    if !(0.1..=2.0).contains(&scale) {
        return Err("Invalid parameters: scale must be between 0.1 and 2".to_string());
    }
    let page_ranges = match params.get("pageRanges") {
        Some(value) => {
            let ranges = value
                .as_str()
                .ok_or("Invalid parameters: pageRanges must be a string")?;
            parse_page_ranges(ranges)?
        }
        None => Vec::new(),
    };
    for name in ["headerTemplate", "footerTemplate"] {
        if let Some(value) = params.get(name) {
            let template = value
                .as_str()
                .ok_or_else(|| format!("Invalid parameters: {name} must be a string"))?;
            if !template.is_empty() {
                return Err(format!("Page.printToPDF {name} is not yet supported"));
            }
        }
    }
    let transfer_mode = match params.get("transferMode") {
        None => PdfTransferMode::ReturnAsBase64,
        Some(Value::String(value)) => match value.as_str() {
            "ReturnAsBase64" => PdfTransferMode::ReturnAsBase64,
            "ReturnAsStream" => PdfTransferMode::ReturnAsStream,
            _ => return Err("Invalid parameters: unknown transferMode".into()),
        },
        Some(_) => return Err("Invalid parameters: transferMode must be a string".into()),
    };

    let defaults = obscura_browser::RasterPdfOptions::default();
    Ok(ParsedPdfOptions {
        raster: obscura_browser::RasterPdfOptions {
            landscape: boolean(params, "landscape", defaults.landscape)?,
            print_background: requested_print_background,
            scale,
            page_ranges,
            paper_width_in: number(params, "paperWidth", defaults.paper_width_in)?,
            paper_height_in: number(params, "paperHeight", defaults.paper_height_in)?,
            margin_top_in: number(params, "marginTop", defaults.margin_top_in)?,
            margin_bottom_in: number(params, "marginBottom", defaults.margin_bottom_in)?,
            margin_left_in: number(params, "marginLeft", defaults.margin_left_in)?,
            margin_right_in: number(params, "marginRight", defaults.margin_right_in)?,
        },
        transfer_mode,
        requested_print_background,
        requested_tagged_pdf,
    })
}

pub async fn print_to_pdf(
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    #[cfg(feature = "render")]
    {
        let options = parse_options(params)?;
        let page = ctx
            .get_session_page_mut(session_id)
            .ok_or("No page for session")?;
        crate::domains::page::prepare_capture_resources_if_requested(page).await;
        let animation_sample = page.live_animation_sample();
        let pdf = page
            .raster_pdf_with_animation_sample(options.raster, animation_sample)
            .map_err(|error| error.to_string())?;
        let mut response = json!({
            "obscuraPrintMode": "print-media-raster",
            "obscuraPrintBackground": options.requested_print_background,
            "obscuraRequestedPrintBackground": options.requested_print_background,
            "obscuraTaggedPDF": false,
            "obscuraRequestedTaggedPDF": options.requested_tagged_pdf,
            "obscuraCapabilities": {
                "cssPagedMedia": false,
                "honorsPrintMedia": true,
                "honorsPrintBackground": true,
                "honorsScale": true,
                "pageRanges": true,
                "printColorAdjustExact": false,
                "taggedPdf": false,
            },
        });
        let mut ignored_options = Vec::new();
        if options.requested_tagged_pdf {
            ignored_options.push("generateTaggedPDF");
        }
        response["obscuraIgnoredOptions"] = json!(ignored_options);
        match options.transfer_mode {
            PdfTransferMode::ReturnAsBase64 => {
                use base64::Engine as _;
                let encoded_len = base64_encoded_len(pdf.len())
                    .ok_or("Page.printToPDF base64 response size overflow")?;
                if encoded_len > MAX_BASE64_PDF_BYTES {
                    return Err(format!(
                        "Page.printToPDF base64 response would be {encoded_len} bytes, exceeding the {MAX_BASE64_PDF_BYTES}-byte response limit; use transferMode=ReturnAsStream"
                    ));
                }
                response["data"] =
                    Value::String(base64::engine::general_purpose::STANDARD.encode(pdf));
            }
            PdfTransferMode::ReturnAsStream => {
                let handle = ctx
                    .io_streams
                    .insert(pdf)
                    .map_err(|error| format!("Page.printToPDF could not open stream: {error}"))?;
                response["data"] = Value::String(String::new());
                response["stream"] = Value::String(handle);
            }
        }
        Ok(response)
    }
    #[cfg(not(feature = "render"))]
    {
        let _ = (params, ctx, session_id);
        Err("Page.printToPDF requires a build with the render feature".to_string())
    }
}

#[cfg(all(test, feature = "render"))]
mod tests {
    use super::*;
    use crate::domains::page;

    #[test]
    fn parser_accepts_standard_client_defaults_and_rejects_unrepresented_features() {
        let standard = parse_options(&json!({
            "transferMode": "ReturnAsStream",
            "displayHeaderFooter": false,
            "headerTemplate": "",
            "footerTemplate": "",
            "printBackground": false,
            "scale": 1,
            "pageRanges": "",
            "preferCSSPageSize": false,
            "generateTaggedPDF": true,
            "generateDocumentOutline": false,
        }))
        .expect("current Puppeteer defaults must be accepted");
        assert_eq!(standard.transfer_mode, PdfTransferMode::ReturnAsStream);
        assert!(!standard.requested_print_background);
        assert!(!standard.raster.print_background);
        assert!(standard.requested_tagged_pdf);
        assert_eq!(standard.raster.scale, 1.0);
        assert!(standard.raster.page_ranges.is_empty());

        for params in [
            json!({"displayHeaderFooter": true}),
            json!({"preferCSSPageSize": true}),
            json!({"headerTemplate": "<span>title</span>"}),
            json!({"generateDocumentOutline": true}),
        ] {
            assert!(parse_options(&params).is_err(), "must reject {params}");
        }

        let ranged = parse_options(&json!({
            "scale": 0.5,
            "pageRanges": "1, 3-5, 8-",
        }))
        .expect("CDP scale and page ranges");
        assert_eq!(ranged.raster.scale, 0.5);
        assert_eq!(
            ranged.raster.page_ranges,
            vec![
                obscura_browser::RasterPdfPageRange {
                    start: Some(1),
                    end: Some(1),
                },
                obscura_browser::RasterPdfPageRange {
                    start: Some(3),
                    end: Some(5),
                },
                obscura_browser::RasterPdfPageRange {
                    start: Some(8),
                    end: None,
                },
            ]
        );
        assert_eq!(
            parse_page_ranges("5-8, 1-3, 3-6, 10, 9, 12-").unwrap(),
            vec![
                obscura_browser::RasterPdfPageRange {
                    start: Some(1),
                    end: Some(10),
                },
                obscura_browser::RasterPdfPageRange {
                    start: Some(12),
                    end: None,
                },
            ],
            "overlapping, adjacent, and repeated ranges must normalize before pagination"
        );
        for params in [
            json!({"scale": 0.09}),
            json!({"scale": 2.01}),
            json!({"pageRanges": "0"}),
            json!({"pageRanges": "3-2"}),
            json!({"pageRanges": "1,,2"}),
            json!({"pageRanges": "1-2-3"}),
        ] {
            assert!(parse_options(&params).is_err(), "must reject {params}");
        }

        let too_many_parts = std::iter::repeat_n("1", MAX_PAGE_RANGE_PARTS + 1)
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            parse_page_ranges(&too_many_parts).is_err(),
            "range-token work must remain bounded before output allocation"
        );
        let too_many_bytes = "1".repeat(MAX_PAGE_RANGES_BYTES + 1);
        assert!(
            parse_page_ranges(&too_many_bytes).is_err(),
            "range input bytes must be bounded before numeric parsing"
        );
    }

    #[test]
    fn base64_size_preflight_is_exact_and_overflow_safe() {
        assert_eq!(base64_encoded_len(0), Some(0));
        assert_eq!(base64_encoded_len(1), Some(4));
        assert_eq!(base64_encoded_len(2), Some(4));
        assert_eq!(base64_encoded_len(3), Some(4));
        assert_eq!(base64_encoded_len(4), Some(8));
        assert_eq!(base64_encoded_len(usize::MAX), None);
        let largest_raw = (MAX_BASE64_PDF_BYTES / 4) * 3;
        assert_eq!(base64_encoded_len(largest_raw), Some(MAX_BASE64_PDF_BYTES));
        assert!(base64_encoded_len(largest_raw + 1).unwrap() > MAX_BASE64_PDF_BYTES);
    }

    #[tokio::test]
    async fn print_to_pdf_returns_paginated_pdf_with_requested_media_box() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{page_id}-session");
        ctx.sessions.insert(session_id.clone(), page_id);
        let session = Some(session_id);
        ctx.get_session_page_mut(&session)
            .unwrap()
            .set_viewport((100.0, 80.0));
        page::handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0'><body style='margin:0;height:400px;background:linear-gradient(red,blue)'></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        ).await.expect("navigate fixture");
        let response = print_to_pdf(
            &json!({
                "paperWidth": 4.0, "paperHeight": 6.0,
                "marginTop": 0.5, "marginBottom": 0.5,
                "marginLeft": 0.5, "marginRight": 0.5,
                "printBackground": true,
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("raster PDF");
        assert_eq!(response["obscuraPrintMode"], "print-media-raster");
        assert_eq!(response["obscuraPrintBackground"], true);
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(response["data"].as_str().unwrap())
            .unwrap();
        assert!(bytes.starts_with(b"%PDF-1.4"));
        assert!(bytes.ends_with(b"%%EOF\n"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/MediaBox [0 0 288.000 432.000]"));
        assert!(
            text.matches("/Subtype /Image").count() >= 2,
            "400px tall fixture should paginate at this printable aspect ratio"
        );

        let streamed = page::handle(
            "printToPDF",
            &json!({
                "transferMode": "ReturnAsStream",
                "landscape": false,
                "displayHeaderFooter": false,
                "headerTemplate": "",
                "footerTemplate": "",
                "printBackground": false,
                "scale": 1,
                "paperWidth": 8.5,
                "paperHeight": 11,
                "marginTop": 0,
                "marginBottom": 0,
                "marginLeft": 0,
                "marginRight": 0,
                "pageRanges": "",
                "preferCSSPageSize": false,
                "generateTaggedPDF": true,
                "generateDocumentOutline": false,
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("Puppeteer/Playwright-shaped stream request");
        assert_eq!(streamed["data"], "");
        assert_eq!(streamed["obscuraPrintBackground"], false);
        assert_eq!(streamed["obscuraRequestedPrintBackground"], false);
        assert_eq!(
            streamed["obscuraCapabilities"]["honorsPrintBackground"],
            true
        );
        assert_eq!(streamed["obscuraCapabilities"]["honorsScale"], true);
        assert_eq!(streamed["obscuraCapabilities"]["honorsPrintMedia"], true);
        assert_eq!(streamed["obscuraCapabilities"]["pageRanges"], true);
        assert_eq!(streamed["obscuraTaggedPDF"], false);
        assert_eq!(streamed["obscuraRequestedTaggedPDF"], true);
        assert_eq!(
            streamed["obscuraIgnoredOptions"],
            json!(["generateTaggedPDF"])
        );
        let handle = streamed["stream"].as_str().expect("protocol stream handle");
        let mut streamed_bytes = Vec::new();
        loop {
            let chunk = crate::domains::io::handle(
                "read",
                &json!({"handle": handle, "size": 1024}),
                &mut ctx,
            )
            .await
            .expect("read PDF stream");
            streamed_bytes.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(chunk["data"].as_str().unwrap())
                    .unwrap(),
            );
            if chunk["eof"] == true {
                break;
            }
        }
        assert!(streamed_bytes.starts_with(b"%PDF-1.4"));
        assert!(streamed_bytes.ends_with(b"%%EOF\n"));
        crate::domains::io::handle("close", &json!({"handle": handle}), &mut ctx)
            .await
            .expect("close PDF stream");
        assert!(
            crate::domains::io::handle("read", &json!({"handle": handle}), &mut ctx,)
                .await
                .is_err(),
            "closed PDF handles must release their buffer"
        );

        let ranged = print_to_pdf(
            &json!({
                "paperWidth": 4.0, "paperHeight": 6.0,
                "marginTop": 0.5, "marginBottom": 0.5,
                "marginLeft": 0.5, "marginRight": 0.5,
                "scale": 1,
                "pageRanges": "2",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("single selected page");
        let ranged_bytes = base64::engine::general_purpose::STANDARD
            .decode(ranged["data"].as_str().unwrap())
            .unwrap();
        let ranged_text = String::from_utf8_lossy(&ranged_bytes);
        assert!(ranged_text.contains("/Count 1"));
        assert_eq!(ranged_text.matches("/Subtype /Image").count(), 1);
    }
}
