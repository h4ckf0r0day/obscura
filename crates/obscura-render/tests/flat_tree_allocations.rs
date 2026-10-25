use std::collections::HashSet;

use obscura_dom::parse_html;
use obscura_render::layout_dom;

#[path = "../../test-support/allocations.rs"]
mod allocations;

#[test]
fn fixed_coordinate_walk_does_not_copy_dom_payloads() {
    let payload = "x".repeat(64 * 1024);
    let tree = parse_html(&format!(
        "<main data-payload='{payload}'>\
         <div id=fixed style='position:fixed' data-payload='{payload}'>\
         <b id=child data-payload='{payload}'></b></div>\
         <details data-payload='{payload}'><summary data-payload='{payload}'>closed</summary>\
         <span id=hidden style='position:fixed'></span></details></main>"
    ));
    let fixed = tree.get_element_by_id("fixed").unwrap();
    let child = tree.get_element_by_id("child").unwrap();
    let laid = layout_dom(&tree, (400.0, 300.0));
    let (actual, bytes) = allocations::measure(|| laid.viewport_fixed_nodes(&tree));
    assert_eq!(actual, HashSet::from([fixed, child]),
        "fixed descendants share viewport coordinates; closed details content does not render");
    assert!(bytes < 16 * 1024, "fixed-coordinate traversal allocated {bytes} bytes");
}

#[test]
fn layout_reads_do_not_copy_element_payloads() {
    let make_tree = |payload: &str| {
        parse_html(&format!(
            "<!doctype html><style>body{{margin:0}}\
             [data-payload^=x]{{height:10px}}</style>{}",
            format!("<div data-payload='{payload}'></div>").repeat(32),
        ))
    };
    let small = make_tree("x");
    let large = make_tree(&"x".repeat(64 * 1024));
    // Warm the shared font database before measuring allocation traffic.
    let _ = layout_dom(&small, (400.0, 600.0));
    let (small_layout, small_bytes) = allocations::measure(|| layout_dom(&small, (400.0, 600.0)));
    let (large_layout, large_bytes) = allocations::measure(|| layout_dom(&large, (400.0, 600.0)));
    eprintln!("layout allocation bytes: small={small_bytes}, large={large_bytes}");
    for (tree, laid) in [(&small, small_layout), (&large, large_layout)] {
        let nodes = tree.query_selector_all("div").unwrap();
        assert_eq!(nodes.len(), 32);
        assert_eq!(laid.rects[&nodes[31]].y, 310.0);
        assert_eq!(laid.rects[&nodes[31]].height, 10.0);
    }
    assert!(large_bytes < small_bytes + 128 * 1024,
        "read-only layout copied payloads: small={small_bytes}, large={large_bytes}");
}
