//! Bounded hybrid PDF encoding. Unsupported paint remains ordered raster
//! content, while admitted shaped glyphs use embedded standalone TrueType.

use super::{PdfWriter, RasterPdfError, MAX_PDF_OUTPUT_BYTES};
use obscura_js::{PrintFont, PrintOp, PrintPage};
use std::{
    borrow::Cow,
    collections::HashMap,
    io::{self, Write},
};

struct FontResource {
    font: PrintFont,
    ids: [usize; 6],
    codes: HashMap<(u16, String), u16>,
    glyphs: Vec<(u16, String)>,
}

struct FontProgram<'a> {
    bytes: Cow<'a, [u8]>,
    gids: Vec<u16>,
    subset: bool,
}

fn select_program<'a>(
    font: &'a PrintFont,
    glyphs: &[(u16, String)],
    subset: impl FnOnce(&[u8], &subsetter::GlyphRemapper) -> Result<Vec<u8>, ()>,
) -> FontProgram<'a> {
    let original = || FontProgram {
        bytes: Cow::Borrowed(font.data()),
        gids: glyphs.iter().map(|g| g.0).collect(),
        subset: false,
    };
    if !font.can_subset() {
        return original();
    }
    // Admission above excludes web/operator fonts. Catch a library failure
    // for a bundled program without losing the original glyph mapping.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut remapper = subsetter::GlyphRemapper::new();
        for (id, _) in glyphs {
            remapper.remap(*id);
        }
        let bytes = subset(font.data(), &remapper).ok()?;
        if bytes.len() >= font.data().len() {
            return None;
        }
        let gids = glyphs
            .iter()
            .map(|(id, _)| remapper.get(*id))
            .collect::<Option<Vec<_>>>()?;
        Some(FontProgram {
            bytes: Cow::Owned(bytes),
            gids,
            subset: true,
        })
    }));
    result.ok().flatten().unwrap_or_else(original)
}

impl FontResource {
    fn code(&mut self, glyph: u16, text: &str) -> Result<u16, RasterPdfError> {
        let key = (glyph, text.to_owned());
        if let Some(code) = self.codes.get(&key) {
            return Ok(*code);
        }
        let code = u16::try_from(self.glyphs.len() + 1)
            .map_err(|_| RasterPdfError::FontWorkLimitExceeded)?;
        self.glyphs.push(key.clone());
        self.codes.insert(key, code);
        Ok(code)
    }

    fn write(&self, writer: &mut PdfWriter) -> Result<(), RasterPdfError> {
        let [font, descendant, descriptor, program, unicode, mapping] = self.ids;
        let program_data = select_program(&self.font, &self.glyphs, |data, map| {
            subsetter::subset(data, 0, map).map_err(|_| ())
        });
        let name = if program_data.subset {
            let mut prefix = [b'A'; 6];
            let mut id = font;
            for byte in prefix.iter_mut().rev() {
                *byte += (id % 26) as u8;
                id /= 26;
            }
            format!(
                "{}+{}",
                std::str::from_utf8(&prefix).unwrap_or("AAAAAA"),
                self.font.post_script_name()
            )
        } else {
            self.font.post_script_name()
        };
        writer.write_object(font, format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /{name} /Encoding /Identity-H /DescendantFonts [{descendant} 0 R] /ToUnicode {unicode} 0 R >>"
        ).as_bytes())?;
        let units = f32::from(self.font.units_per_em());
        let scale = 1000.0 / units;
        let widths = self
            .glyphs
            .iter()
            .map(|(id, _)| format!("{:.3}", f32::from(self.font.advance(*id)) * scale))
            .collect::<Vec<_>>()
            .join(" ");
        writer.write_object(descendant, format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{name} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {descriptor} 0 R /CIDToGIDMap {mapping} 0 R /W [1 [{widths}]] >>"
        ).as_bytes())?;
        let bounds = self.font.bounds().map(|v| f32::from(v) * scale);
        writer.write_object(descriptor, format!(
            "<< /Type /FontDescriptor /FontName /{name} /Flags {} /FontBBox [{:.3} {:.3} {:.3} {:.3}] /ItalicAngle {:.3} /Ascent {:.3} /Descent {:.3} /CapHeight {:.3} /StemV 80 /FontFile2 {program} 0 R >>",
            self.font.descriptor_flags(), bounds[0], bounds[1], bounds[2], bounds[3], self.font.italic_angle(),
            f32::from(self.font.ascent()) * scale, f32::from(self.font.descent()) * scale,
            f32::from(self.font.ascent()) * scale,
        ).as_bytes())?;
        writer.deflated_stream(
            program,
            &format!("/Length1 {}", program_data.bytes.len()),
            |out| out.write_all(&program_data.bytes),
        )?;
        writer.deflated_stream(mapping, "", |out| {
            out.write_all(&[0, 0])?;
            for id in &program_data.gids {
                out.write_all(&id.to_be_bytes())?;
            }
            Ok(())
        })?;
        let offsets = writer.begin_stream(unicode, "")?;
        writer.append(b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /ObscuraUnicode def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n")?;
        // PDF CMap blocks have at most 100 entries. Multi-glyph clusters use
        // ActualText around the whole cluster, with the first glyph carrying
        // the fallback ToUnicode value for readers that consult that map.
        let entries = self
            .glyphs
            .iter()
            .enumerate()
            .filter(|(_, (_, text))| !text.is_empty())
            .collect::<Vec<_>>();
        for chunk in entries.chunks(100) {
            writer.append(format!("{} beginbfchar\n", chunk.len()).as_bytes())?;
            for (index, (_, text)) in chunk {
                writer.append(format!("<{:04X}> ", index + 1).as_bytes())?;
                hex_text(writer, text, false)?;
                writer.append(b"\n")?;
            }
            writer.append(b"endbfchar\n")?;
        }
        writer.append(b"endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n")?;
        writer.end_stream(offsets)
    }
}

fn hex_text(writer: &mut PdfWriter, text: &str, bom: bool) -> Result<(), RasterPdfError> {
    writer.append(if bom { b"<FEFF" } else { b"<" })?;
    for unit in text.encode_utf16() {
        writer.append(format!("{unit:04X}").as_bytes())?;
    }
    writer.append(b">")
}

fn raster(writer: &mut PdfWriter, pixels: &obscura_js::PrintOp) -> Result<usize, RasterPdfError> {
    let PrintOp::Raster { pixels, .. } = pixels else {
        unreachable!()
    };
    let image = writer.allocate_object()?;
    let has_alpha = pixels.pixels().iter().any(|p| p.alpha() != 255);
    if !has_alpha {
        // Preserve the existing opaque raster encoding. Only later transparent
        // segments need a separate lossless RGB stream and alpha mask.
        let rgb = pixels
            .pixels()
            .iter()
            .flat_map(|p| [p.red(), p.green(), p.blue()])
            .collect::<Vec<_>>();
        let rgb = image::RgbImage::from_raw(pixels.width(), pixels.height(), rgb)
            .ok_or(RasterPdfError::RasterWorkLimitExceeded)?;
        writer.write_rgb_image(image, &rgb)?;
        return Ok(image);
    }
    let mask = if has_alpha {
        Some(writer.allocate_object()?)
    } else {
        None
    };
    let mask_ref = mask
        .map(|id| format!(" /SMask {id} 0 R"))
        .unwrap_or_default();
    writer.deflated_stream(image, &format!(
        "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8{mask_ref}",
        pixels.width(), pixels.height(),
    ), |out| {
        let mut row = Vec::with_capacity(pixels.width() as usize * 3);
        for chunk in pixels.pixels().chunks(pixels.width() as usize) {
            row.clear();
            for pixel in chunk {
                let color = pixel.demultiply();
                row.extend_from_slice(&[color.red(), color.green(), color.blue()]);
            }
            out.write_all(&row)?;
        }
        Ok(())
    })?;
    if let Some(mask) = mask {
        writer.deflated_stream(mask, &format!(
            "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 8",
            pixels.width(), pixels.height(),
        ), |out| {
            let mut row = Vec::with_capacity(pixels.width() as usize);
            for chunk in pixels.pixels().chunks(pixels.width() as usize) {
                row.clear();
                row.extend(chunk.iter().map(|p| p.alpha()));
                out.write_all(&row)?;
            }
            Ok(())
        })?;
    }
    Ok(image)
}

fn page_commands(
    out: &mut dyn Write,
    page: &PrintPage,
    fonts: &[FontResource],
    width: f32,
    height: f32,
    scale: f32,
    origin: (f32, f32),
) -> io::Result<()> {
    write!(
        out,
        "q\n{scale:.6} 0 0 {:.6} {:.6} {:.6} cm\n0 0 {width:.6} {height:.6} re W n\n",
        -scale, origin.0, origin.1
    )?;
    for (i, op) in page.ops.iter().enumerate() {
        match op {
            PrintOp::Raster { pixels, x, y } => {
                write!(
                    out,
                    "q\n{} 0 0 -{} {x} {} cm\n/Im{i} Do\nQ\n",
                    pixels.width(),
                    pixels.height(),
                    y + pixels.height()
                )?;
            }
            PrintOp::Text { clusters, clip } => {
                out.write_all(b"q\n")?;
                if let Some(clip) = clip {
                    writeln!(
                        out,
                        "{:.6} {:.6} {:.6} {:.6} re W n",
                        clip.x, clip.y, clip.width, clip.height
                    )?;
                }
                for cluster in clusters {
                    out.write_all(b"/Span << /ActualText <FEFF")?;
                    for unit in cluster.text.encode_utf16() {
                        write!(out, "{unit:04X}")?;
                    }
                    out.write_all(b"> >> BDC\n")?;
                    for (g, glyph) in cluster.glyphs.iter().enumerate() {
                        let f = fonts
                            .iter()
                            .position(|f| f.font.same_face(&glyph.font))
                            .ok_or_else(|| io::Error::other("missing PDF font resource"))?;
                        let key = (
                            glyph.id,
                            if g == 0 {
                                cluster.text.clone()
                            } else {
                                String::new()
                            },
                        );
                        let code = fonts[f]
                            .codes
                            .get(&key)
                            .ok_or_else(|| io::Error::other("missing PDF glyph code"))?;
                        writeln!(out, "BT /F{f} {:.6} Tf {:.6} {:.6} {:.6} rg 1 0 0 -1 {:.6} {:.6} Tm <{code:04X}> Tj ET",
                            glyph.size, f32::from(glyph.color[0])/255.0, f32::from(glyph.color[1])/255.0,
                            f32::from(glyph.color[2])/255.0, glyph.x, glyph.y)?;
                    }
                    out.write_all(b"EMC\n")?;
                }
                out.write_all(b"Q\n")?;
            }
        }
    }
    out.write_all(b"Q\n")
}

pub(super) fn encode_pages(
    count: usize,
    paper: (f32, f32),
    origin: (f32, f32),
    scale: f32,
    mut source: impl FnMut(usize) -> Result<(PrintPage, f32, f32), RasterPdfError>,
) -> Result<Vec<u8>, RasterPdfError> {
    let mut writer = PdfWriter::new(2, MAX_PDF_OUTPUT_BYTES)?;
    writer.write_object(1, b"<< /Type /Catalog /Pages 2 0 R >>")?;
    let mut pages = Vec::new();
    let mut fonts: Vec<FontResource> = Vec::new();
    let mut font_bytes = 0usize;
    let mut glyph_count = 0usize;
    let mut text_bytes = 0usize;
    for index in 0..count {
        let (page, width, height) = source(index)?;
        let id = writer.allocate_object()?;
        let content = writer.allocate_object()?;
        pages.push(id);
        let mut images = HashMap::new();
        for (i, op) in page.ops.iter().enumerate() {
            match op {
                PrintOp::Raster { .. } => {
                    images.insert(i, raster(&mut writer, op)?);
                }
                PrintOp::Text { clusters, .. } => {
                    for cluster in clusters {
                        text_bytes = text_bytes.saturating_add(cluster.text.len());
                        glyph_count = glyph_count.saturating_add(cluster.glyphs.len());
                        if text_bytes > 16 * 1024 * 1024 || glyph_count > 1_000_000 {
                            return Err(RasterPdfError::FontWorkLimitExceeded);
                        }
                        for (g, glyph) in cluster.glyphs.iter().enumerate() {
                            let f = if let Some(f) =
                                fonts.iter().position(|f| f.font.same_face(&glyph.font))
                            {
                                f
                            } else {
                                font_bytes = font_bytes
                                    .checked_add(glyph.font.data().len())
                                    .ok_or(RasterPdfError::FontWorkLimitExceeded)?;
                                if fonts.len() >= 64 || font_bytes > 32 * 1024 * 1024 {
                                    return Err(RasterPdfError::FontWorkLimitExceeded);
                                }
                                let mut ids = [0; 6];
                                for id in &mut ids {
                                    *id = writer.allocate_object()?;
                                }
                                fonts.push(FontResource {
                                    font: glyph.font.clone(),
                                    ids,
                                    codes: HashMap::new(),
                                    glyphs: Vec::new(),
                                });
                                fonts.len() - 1
                            };
                            fonts[f].code(glyph.id, if g == 0 { &cluster.text } else { "" })?;
                        }
                    }
                }
            }
        }
        let font_refs = fonts
            .iter()
            .enumerate()
            .map(|(i, f)| format!("/F{i} {} 0 R", f.ids[0]))
            .collect::<Vec<_>>()
            .join(" ");
        let image_refs = images
            .iter()
            .map(|(i, id)| format!("/Im{i} {id} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        writer.write_object(id, format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.3} {:.3}] /Resources << /Font << {font_refs} >> /XObject << {image_refs} >> >> /Contents {content} 0 R >>", paper.0, paper.1,
        ).as_bytes())?;
        writer.deflated_stream(content, "", |out| {
            page_commands(out, &page, &fonts, width, height, scale, origin)
        })?;
        // The source page and its rasters drop here. Only font programs and
        // the capped final PDF survive across pages.
    }
    for font in &fonts {
        font.write(&mut writer)?;
    }
    let kids = pages
        .iter()
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    writer.write_object(
        2,
        format!("<< /Type /Pages /Count {count} /Kids [{kids}] >>").as_bytes(),
    )?;
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composite_font() -> (PrintFont, u16) {
        let mut runtime = obscura_js::runtime::ObscuraJsRuntime::new();
        runtime.set_dom(obscura_dom::parse_html("<p style='font:20px Arial'>é</p>"));
        runtime.set_url("https://example.test/pdf-font");
        runtime.set_viewport(200.0, 200.0);
        runtime.run_page_init();
        let page = runtime
            .print_prepared_region_at_scroll(
                obscura_js::CaptureRegion::new(0.0, 0.0, 200.0, 200.0, 1.0),
                (0.0, 0.0),
                true,
            )
            .unwrap();
        page.ops
            .iter()
            .find_map(|op| match op {
                PrintOp::Text { clusters, .. } => clusters
                    .first()
                    .and_then(|c| c.glyphs.first())
                    .map(|g| (g.font.clone(), g.id)),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn subset_remaps_alias_cids_and_keeps_composite_dependencies() {
        let (font, id) = composite_font();
        assert!(font.can_subset());
        let glyphs = vec![(id, "é".into()), (id, "e\u{301}".into())];
        let program = select_program(&font, &glyphs, |data, map| {
            subsetter::subset(data, 0, map).map_err(|_| ())
        });
        assert!(program.subset);
        assert!(program.bytes.len() < font.data().len());
        assert_eq!(program.gids[0], program.gids[1]);
        assert_ne!(program.gids[0], id);
        // A composite is validated visually/extracted through the CDP fixture
        // too; this checks both aliases address the same remapped outline.
    }

    #[test]
    fn failed_or_ineffective_subsets_keep_program_and_original_gids_together() {
        let (font, id) = composite_font();
        let glyphs = vec![(id, "é".into())];
        for program in [
            select_program(&font, &glyphs, |_, _| Err(())),
            select_program(&font, &glyphs, |data, _| Ok(data.to_vec())),
            select_program(&font, &glyphs, |_, _| panic!("injected subset failure")),
        ] {
            assert!(!program.subset);
            assert_eq!(&*program.bytes, font.data());
            assert_eq!(program.gids, vec![id]);
        }
    }

    #[test]
    fn compressed_font_streams_respect_the_writer_cap_during_encoding() {
        let mut writer = PdfWriter::new(1, 256).unwrap();
        let mut seed = 1u32;
        let data = (0..4096)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8
            })
            .collect::<Vec<_>>();
        assert_eq!(
            writer.deflated_stream(1, "", |out| out.write_all(&data)),
            Err(RasterPdfError::OutputLimitExceeded)
        );
        assert!(writer.output.len() <= 256);
    }
}
