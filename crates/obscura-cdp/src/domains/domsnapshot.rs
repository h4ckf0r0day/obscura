//! `DOMSnapshot.captureSnapshot` for layout-free engines.
//!
//! browser-use (and other CDP DOM-agent frameworks) build their interactive
//! element index from `DOMSnapshot.captureSnapshot`: the per-node bounds,
//! computed styles, and `isClickable` flag, correlated with `DOM.getDocument`
//! by `backendNodeId`. Without this domain their DOM build aborts and the agent
//! sees zero elements.
//!
//! Obscura has no layout/paint engine, so there is no real geometry to report.
//! We synthesize it: every node gets a distinct, on-screen, non-icon-sized box
//! (a simple vertical stack) plus plausible computed styles (visible, opaque,
//! pointer cursor on interactive tags). That is enough for the element
//! detection path, which keys off tag name / ARIA / accessibility role and does
//! not need true geometry. Clicking still falls back to JS `.click()` since the
//! coordinates are synthetic. `backendNodeId == nid`, matching `DOM.getDocument`.

use obscura_dom::{DomTree, NodeData, NodeId};
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

/// Computed-style names browser-use requests, in the exact order it expects to
/// read them back out of each layout node's `styles` index array.
const REQUIRED_STYLES: &[&str] = &[
    "display",
    "visibility",
    "opacity",
    "overflow",
    "overflow-x",
    "overflow-y",
    "cursor",
    "pointer-events",
    "position",
    "background-color",
];

/// Cap the synthesized snapshot so a pathologically large DOM cannot produce a
/// runaway payload. Matches the spirit of the descendants() length cap.
const MAX_NODES: usize = 20_000;

pub async fn handle(
    method: &str,
    _params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" | "disable" => Ok(json!({})),
        "captureSnapshot" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let url = page.url_string();
            let title = page.title.clone();
            page.with_dom(|dom| build_capture_snapshot(dom, &url, &title))
                .ok_or_else(|| "No DOM loaded".to_string())
        }
        // Permissive no-op for the rest of the domain (e.g. getSnapshot) so a
        // client that probes it does not abort on an Unknown-method error.
        _ => Ok(json!({})),
    }
}

/// String table with de-duplication. Every string in a DOMSnapshot response is
/// referenced by its index into the top-level `strings` array.
struct Interner {
    map: std::collections::HashMap<String, i64>,
    list: Vec<String>,
}

impl Interner {
    fn new() -> Self {
        let mut s = Interner { map: std::collections::HashMap::new(), list: Vec::new() };
        s.intern(""); // index 0 is the empty string by convention
        s
    }
    fn intern(&mut self, s: &str) -> i64 {
        if let Some(&i) = self.map.get(s) {
            return i;
        }
        let i = self.list.len() as i64;
        self.list.push(s.to_string());
        self.map.insert(s.to_string(), i);
        i
    }
}

/// Pre-order DFS from the document, recording each node and its parent's index.
/// Iterative with an explicit stack: the MAX_NODES cap bounds the node count,
/// but recursion depth is a separate axis. A deeply nested linear chain (script
/// can build thousands of nested elements) would recurse that many frames deep
/// and could overflow the stack before the count guard triggers. An explicit
/// stack keeps the depth on the heap (issue #341).
fn walk(
    dom: &DomTree,
    root: NodeId,
    root_parent: i64,
    order: &mut Vec<NodeId>,
    parent_idx: &mut Vec<i64>,
) {
    // (node, parent_index). Children are pushed in reverse so they pop in
    // document order, preserving the original pre-order traversal.
    let mut stack: Vec<(NodeId, i64)> = vec![(root, root_parent)];
    while let Some((id, parent)) = stack.pop() {
        if order.len() >= MAX_NODES {
            break;
        }
        let my = order.len() as i64;
        order.push(id);
        parent_idx.push(parent);
        let children: Vec<NodeId> = dom.children(id);
        for child in children.into_iter().rev() {
            stack.push((child, my));
        }
    }
}

fn build_capture_snapshot(dom: &DomTree, url: &str, title: &str) -> Value {
    let mut order: Vec<NodeId> = Vec::new();
    let mut parent_idx: Vec<i64> = Vec::new();
    walk(dom, dom.document(), -1, &mut order, &mut parent_idx);

    let mut strings = Interner::new();
    let doc_url_idx = strings.intern(url);
    let title_idx = strings.intern(title);

    let n = order.len();
    let mut node_type: Vec<i64> = Vec::with_capacity(n);
    let mut node_name: Vec<i64> = Vec::with_capacity(n);
    let mut node_value: Vec<i64> = Vec::with_capacity(n);
    let mut backend_ids: Vec<i64> = Vec::with_capacity(n);
    let mut attributes: Vec<Value> = Vec::with_capacity(n);
    let mut clickable: Vec<i64> = Vec::new();

    // Layout arrays are 1:1 with nodes (nodeIndex[i] == i).
    let mut layout_node_index: Vec<i64> = Vec::with_capacity(n);
    let mut bounds: Vec<Value> = Vec::with_capacity(n);
    let mut styles: Vec<Value> = Vec::with_capacity(n);
    let mut paint_orders: Vec<i64> = Vec::with_capacity(n);
    let mut client_rects: Vec<Value> = Vec::with_capacity(n);
    let mut layout_text: Vec<i64> = Vec::with_capacity(n);

    for (i, &nid) in order.iter().enumerate() {
        let node = match dom.get_node(nid) {
            Some(node) => node,
            None => {
                // Keep arrays aligned even for a vanished node.
                node_type.push(0);
                node_name.push(0);
                node_value.push(0);
                backend_ids.push(nid.index() as i64);
                attributes.push(json!([]));
                layout_node_index.push(i as i64);
                bounds.push(json!([0.0, 0.0, 0.0, 0.0]));
                styles.push(json!([]));
                paint_orders.push(i as i64);
                client_rects.push(json!([0.0, 0.0, 0.0, 0.0]));
                layout_text.push(-1);
                continue;
            }
        };

        let (ntype, nname, nval, attrs, tag): (i64, String, String, Vec<(String, String)>, String) =
            match &node.data {
                NodeData::Document => (9, "#document".into(), String::new(), vec![], String::new()),
                NodeData::Doctype { name, .. } => {
                    (10, name.to_string(), String::new(), vec![], String::new())
                }
                NodeData::Element { name, attrs, .. } => {
                    let tag = name.local.as_ref().to_string();
                    let flat = attrs
                        .iter()
                        .map(|a| (a.name.local.to_string(), a.value.clone()))
                        .collect();
                    (1, tag.to_ascii_uppercase(), String::new(), flat, tag.to_ascii_lowercase())
                }
                NodeData::Text { contents } => {
                    (3, "#text".into(), contents.clone(), vec![], String::new())
                }
                NodeData::Comment { contents } => {
                    (8, "#comment".into(), contents.clone(), vec![], String::new())
                }
                NodeData::ProcessingInstruction { target, data } => {
                    (7, target.to_string(), data.clone(), vec![], String::new())
                }
            };

        node_type.push(ntype);
        node_name.push(strings.intern(&nname));
        node_value.push(strings.intern(&nval));
        backend_ids.push(nid.index() as i64);

        let mut attr_idx: Vec<Value> = Vec::with_capacity(attrs.len() * 2);
        for (k, v) in &attrs {
            attr_idx.push(json!(strings.intern(k)));
            attr_idx.push(json!(strings.intern(v)));
        }
        attributes.push(json!(attr_idx));

        let interactive = matches!(
            tag.as_str(),
            "a" | "button" | "input" | "select" | "textarea" | "summary" | "details" | "option" | "label"
        );
        let has_onclick = attrs.iter().any(|(k, _)| k.eq_ignore_ascii_case("onclick"));
        if interactive || has_onclick {
            clickable.push(i as i64);
        }

        // Tags that never render box content; report display:none so the agent
        // does not treat them as visible.
        let hidden = matches!(
            tag.as_str(),
            "head" | "meta" | "title" | "script" | "style" | "link" | "noscript" | "base"
        );
        let display = if ntype == 1 && hidden { "none" } else { "block" };
        let cursor = if interactive { "pointer" } else { "auto" };
        let style_vals = [
            display,
            "visible",
            "1",
            "visible",
            "visible",
            "visible",
            cursor,
            "auto",
            "static",
            "rgba(0, 0, 0, 0)",
        ];
        debug_assert_eq!(style_vals.len(), REQUIRED_STYLES.len());
        let style_idx: Vec<Value> = style_vals.iter().map(|s| json!(strings.intern(s))).collect();
        styles.push(json!(style_idx));

        // Synthetic geometry: a vertical stack, full-width, 18px tall. Distinct
        // and non-icon-sized so visibility/size heuristics include the element;
        // the coordinates are not real (no layout engine).
        let y = (i as f64) * 18.0;
        bounds.push(json!([0.0, y, 1280.0, 18.0]));
        client_rects.push(json!([0.0, y, 1280.0, 18.0]));
        paint_orders.push(i as i64);
        layout_node_index.push(i as i64);
        layout_text.push(-1);
    }

    let content_height = (n as i64) * 18;
    json!({
        "documents": [{
            "documentURL": doc_url_idx,
            "title": title_idx,
            "baseURL": doc_url_idx,
            "contentLanguage": 0,
            "encodingName": 0,
            "publicId": 0,
            "systemId": 0,
            "frameId": 0,
            "nodes": {
                "parentIndex": parent_idx,
                "nodeType": node_type,
                "nodeName": node_name,
                "nodeValue": node_value,
                "backendNodeId": backend_ids,
                "attributes": attributes,
                "isClickable": { "index": clickable },
            },
            "layout": {
                "nodeIndex": layout_node_index,
                "styles": styles,
                "bounds": bounds,
                "text": layout_text,
                "paintOrders": paint_orders,
                "clientRects": client_rects,
            },
            "textBoxes": { "layoutIndex": [], "bounds": [], "start": [], "length": [] },
            "scrollOffsetX": 0.0,
            "scrollOffsetY": 0.0,
            "contentWidth": 1280,
            "contentHeight": content_height,
        }],
        "strings": strings.list,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::CdpContext;

    fn collect_backend_ids(node: &Value, out: &mut Vec<i64>) {
        if let Some(id) = node.get("backendNodeId").and_then(|v| v.as_i64()) {
            out.push(id);
        }
        if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
            for c in children {
                collect_backend_ids(c, out);
            }
        }
    }

    fn find_backend_id_by_name(node: &Value, name: &str) -> Option<i64> {
        if node.get("nodeName").and_then(|v| v.as_str()) == Some(name) {
            return node.get("backendNodeId").and_then(|v| v.as_i64());
        }
        node.get("children")
            .and_then(|v| v.as_array())
            .and_then(|children| children.iter().find_map(|c| find_backend_id_by_name(c, name)))
    }

    async fn navigate(ctx: &mut CdpContext, body: &str) -> String {
        let page_id = ctx.create_page();
        let session_id = format!("{}-session", page_id);
        ctx.sessions.insert(session_id.clone(), page_id.clone());
        let url = format!("data:text/html,{body}");
        crate::domains::page::handle(
            "navigate",
            &json!({ "url": url, "waitUntil": "load" }),
            ctx,
            &Some(session_id.clone()),
        )
        .await
        .expect("navigate should succeed");
        session_id
    }

    #[tokio::test]
    async fn capture_snapshot_matches_getdocument_and_flags_clickable() {
        let mut ctx = CdpContext::new();
        let session = navigate(&mut ctx, "<button id=go>Go</button><a href=/x>L</a>").await;

        // DOM.getDocument is the structural source; the snapshot must use the
        // same backendNodeId scheme so a client can correlate the two.
        let doc = crate::domains::dom::handle(
            "getDocument",
            &json!({ "depth": -1 }),
            &mut ctx,
            &Some(session.clone()),
        )
        .await
        .expect("getDocument should succeed");
        let mut doc_ids = Vec::new();
        collect_backend_ids(&doc["root"], &mut doc_ids);
        assert!(!doc_ids.is_empty(), "getDocument returned no nodes");

        let snap = handle("captureSnapshot", &json!({}), &mut ctx, &Some(session.clone()))
            .await
            .expect("captureSnapshot should succeed");

        let documents = snap["documents"].as_array().expect("documents array");
        assert_eq!(documents.len(), 1);
        let nodes = &documents[0]["nodes"];
        let layout = &documents[0]["layout"];

        let snap_ids: Vec<i64> = nodes["backendNodeId"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect();

        // Every node from getDocument must appear in the snapshot under the
        // identical backendNodeId.
        for id in &doc_ids {
            assert!(
                snap_ids.contains(id),
                "snapshot is missing backendNodeId {id} present in getDocument"
            );
        }

        // String table is populated (everything is referenced by index).
        assert!(
            snap["strings"].as_array().unwrap().len() > 1,
            "string table should be populated"
        );

        // Layout arrays are aligned 1:1 with nodes and carry bounds + the 10
        // computed styles browser-use reads back positionally.
        let n = snap_ids.len();
        assert_eq!(layout["nodeIndex"].as_array().unwrap().len(), n);
        assert_eq!(layout["bounds"].as_array().unwrap().len(), n);
        assert_eq!(layout["styles"].as_array().unwrap().len(), n);
        assert_eq!(
            layout["bounds"][0].as_array().unwrap().len(),
            4,
            "bounds entries are [x, y, w, h]"
        );
        assert_eq!(
            layout["styles"][0].as_array().unwrap().len(),
            REQUIRED_STYLES.len(),
            "each layout node carries all required computed styles"
        );

        // Interactive elements are flagged isClickable (by node index).
        let clickable: Vec<i64> = nodes["isClickable"]["index"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect();
        for tag in ["BUTTON", "A"] {
            let bid = find_backend_id_by_name(&doc["root"], tag)
                .unwrap_or_else(|| panic!("{tag} should be in the document"));
            let idx = snap_ids
                .iter()
                .position(|&id| id == bid)
                .unwrap_or_else(|| panic!("{tag} should be in the snapshot")) as i64;
            assert!(clickable.contains(&idx), "{tag} must be flagged isClickable");
        }
    }

    #[tokio::test]
    async fn unknown_domsnapshot_method_is_permissive_noop() {
        // Probing the domain (e.g. getSnapshot) must not abort with an
        // Unknown-method error the way an unhandled domain would.
        let mut ctx = CdpContext::new();
        let r = handle("getSnapshot", &json!({}), &mut ctx, &None)
            .await
            .expect("unknown DOMSnapshot methods are a permissive no-op");
        assert!(r.is_object());
    }
}
