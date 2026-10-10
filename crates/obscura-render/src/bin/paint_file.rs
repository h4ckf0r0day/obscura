// Offline paint harness: read an HTML file, paint it at a viewport, write PNG.
// Usage: paint_file <input.html> <output.png> [width] [height] [base_url]
use std::fs;
use obscura_dom::tree_sink::parse_html;
use obscura_render::screenshot_png;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).map(|s| s.as_str()).unwrap_or("wiki_prepared.html");
    let output = args.get(2).map(|s| s.as_str()).unwrap_or("wiki_out.png");
    let w: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1280.0);
    let h: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(720.0);
    let base_url = args.get(5).map(|s| s.as_str());

    let html = fs::read_to_string(input).expect("read input html");
    let tree = parse_html(&html);
    let png = screenshot_png(&tree, (w, h), base_url).expect("screenshot");
    fs::write(output, &png).expect("write png");
    eprintln!("wrote {} ({} bytes) at {}x{}", output, png.len(), w, h);
}
