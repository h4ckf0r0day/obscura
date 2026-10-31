# PDF output

CDP `Page.printToPDF`, including Puppeteer and Playwright `page.pdf()`, exports
supported shaped text as selectable PDF text with embedded TrueType fonts.
It uses the renderer's glyph positions and font fallback, including ligatures
and combining marks. Built-in fonts are subsetted once per document. Other
supported font programs are embedded in full, which can make short documents
larger.

Pagination, page ranges, paper size, margins, scale, print media and
`printBackground` use the same retained document layout as raster PDF export.
Images and unsupported paint remain raster content in their original paint
order. Both base64 and stream transfer modes are supported.

The first vector-text path covers ordinary inline formatting contexts with
opaque solid text and standalone static TrueType outlines. Text inside atomic
stacking contexts, floats, transforms or opacity groups remains raster.
Decorations, synthetic italic, gradient text, rounded overflow clips, ellipsis,
variable fonts, color fonts and collection faces also retain raster text.
Rectangular clips are supported. Generated content, list markers and form
values currently use their existing raster paths.

An effect-heavy page falls back to a complete raster page if ordered segment
capture exceeds its additional work or memory budget. Font embedding is bounded
to 64 faces and 32 MiB of original font programs per document. The existing
page, pixel and 64 MiB output limits still apply. Vector text also has a document
budget of one million glyphs and 16 MiB of source cluster text.

The CDP response reports `obscuraPrintMode: "print-media-hybrid"`,
`obscuraCapabilities.vectorText: true` and
`obscuraCapabilities.rasterTextFallback: true`. These describe available
capabilities, rather than guaranteeing selectable text for every element.
Tagged PDF, PDF/A, headers and footers, outlines and full CSS paged media are
not implemented.

The Rust `Page::raster_pdf` API and MCP `browser_pdf` keep their existing raster
output. Rust callers can use `Page::print_pdf_with_animation_sample` for the
hybrid path.
