use obscura_dom::{parse_html, ShadowRootMode};

#[path = "../../test-support/allocations.rs"]
mod allocations;

#[test]
fn slot_queries_do_not_copy_unrelated_attributes_or_text() {
    let payload = "x".repeat(64 * 1024);
    let tree = parse_html(&format!(
        "<x-card id=host><b id=light slot=title data-payload='{payload}'></b>{payload}</x-card>\
         <div id=outside data-payload='{payload}'></div>\
         <div id=source><slot id=named name=title data-payload='{payload}'></slot>\
         <slot id=default></slot></div>"
    ));
    let host = tree.get_element_by_id("host").unwrap();
    let light = tree.get_element_by_id("light").unwrap();
    let text = tree.children(host)[1];
    let outside = tree.get_element_by_id("outside").unwrap();
    let named = tree.get_element_by_id("named").unwrap();
    let default = tree.get_element_by_id("default").unwrap();
    let root = tree.attach_shadow_root(host, ShadowRootMode::Open).unwrap();
    tree.append_child(root, named);
    tree.append_child(root, default);

    let (result, bytes) = allocations::measure(|| (
        tree.is_html_slot_element(named),
        tree.is_html_slot_element(outside),
        tree.assigned_slot(outside),
        tree.assigned_slot(light),
        tree.assigned_slot(text),
        tree.assigned_nodes(named),
        tree.assigned_nodes(default),
    ));
    assert_eq!(result, (true, false, None, Some(named), Some(default),
        Some(vec![light]), Some(vec![text])));
    // Small traversal/result vectors are allowed, copying a 64KiB DOM payload is not.
    assert!(bytes < 4096, "slot queries allocated {bytes} bytes for a two-slot tree");
}

#[test]
fn relational_selector_walks_do_not_copy_unrelated_payloads() {
    let payload = "x".repeat(64 * 1024);
    let tree = parse_html(&format!(
        "<section id=scope data-payload='{payload}'>\
         <i id=first data-payload='{payload}'></i>{payload}\
         <b id=target class=item data-payload='{payload}'></b><!--ignored-->\
         <em id=last data-payload='{payload}'></em></section>"
    ));
    let scope = tree.get_element_by_id("scope").unwrap();
    let target = tree.get_element_by_id("target").unwrap();
    let last = tree.get_element_by_id("last").unwrap();
    for (selector, expected) in [
        ("section > .item", target),
        ("i + .item", target),
        (".item + em", last),
        ("section:has(> .item)", scope),
        (".item:first-of-type", target),
        (".item:has(+ em)", target),
        ("section:not(:empty)", scope),
    ] {
        let (matched, bytes) = allocations::measure(|| tree.query_selector_all(selector).unwrap());
        assert_eq!(matched, vec![expected], "{selector}");
        // Parsing/traversal may allocate, but not a 64KiB unrelated node payload.
        assert!(bytes < 16 * 1024, "{selector} allocated {bytes} bytes");
    }
}
