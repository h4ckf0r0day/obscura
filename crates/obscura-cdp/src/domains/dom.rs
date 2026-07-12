use obscura_browser::Page;
use obscura_dom::{DomTree, NodeData, NodeId};
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

/// Resolve a DOM `nodeId` from CDP params. Honors `nodeId`, `backendNodeId`,
/// and `objectId` in that order. Playwright commonly passes only `objectId`
/// (returned by a prior `DOM.resolveNode`); without this fallback those
/// requests silently default to node 0 and click the wrong element.
fn resolve_node_id(page: &mut Page, params: &Value) -> Result<u64, String> {
    if let Some(nid) = params.get("nodeId").and_then(|v| v.as_u64()) {
        return Ok(nid);
    }
    if let Some(nid) = params.get("backendNodeId").and_then(|v| v.as_u64()) {
        return Ok(nid);
    }
    if let Some(oid) = params.get("objectId").and_then(|v| v.as_str()) {
        let code = format!(
            "(function() {{ var o = globalThis.__obscura_objects && globalThis.__obscura_objects['{}']; \
             return (o && typeof o._nid === 'number') ? o._nid : -1; }})()",
            oid.replace('\'', "\\'")
        );
        let result = page.evaluate(&code);
        let nid = result.as_f64().map(|n| n as i64).unwrap_or(-1);
        if nid < 0 {
            return Err(format!("objectId {oid} could not be resolved to a node"));
        }
        return Ok(nid as u64);
    }
    Err("nodeId, backendNodeId, or objectId required".to_string())
}

/// Standard base64 (with padding). Used to ferry file bytes to the JS layer for
/// DOM.setFileInputFiles without pulling in a dependency.
fn encode_base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Best-effort MIME type from a file extension, for the File objects created by
/// DOM.setFileInputFiles. Defaults to application/octet-stream.
fn guess_mime(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => Ok(json!({})),
        "getDocument" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let depth = params.get("depth").and_then(|v| v.as_i64()).unwrap_or(2);
            page.with_dom(|dom| {
                let node = serialize_node(dom, dom.document(), depth as u32, 0);
                json!({ "root": node })
            }).ok_or_else(|| "No DOM loaded".to_string())
        }
        "querySelector" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let selector = params.get("selector").and_then(|v| v.as_str()).ok_or("selector required")?;
            let result = page.with_dom(|dom| {
                dom.query_selector(selector).ok().flatten().map(|id| id.index()).unwrap_or(0)
            }).unwrap_or(0);
            Ok(json!({ "nodeId": result }))
        }
        "querySelectorAll" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let selector = params.get("selector").and_then(|v| v.as_str()).ok_or("selector required")?;
            let ids = page.with_dom(|dom| {
                dom.query_selector_all(selector).ok()
                    .map(|ids| ids.iter().map(|id| id.index() as u64).collect::<Vec<_>>())
                    .unwrap_or_default()
            }).unwrap_or_default();
            Ok(json!({ "nodeIds": ids }))
        }
        "getOuterHTML" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let node_id = params.get("nodeId").and_then(|v| v.as_u64())
                .or_else(|| params.get("backendNodeId").and_then(|v| v.as_u64()))
                .ok_or("nodeId required")?;
            let html = page.with_dom(|dom| {
                dom.outer_html(NodeId::new(node_id as u32))
            }).unwrap_or_default();
            Ok(json!({ "outerHTML": html }))
        }
        "describeNode" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let depth = params.get("depth").and_then(|v| v.as_i64()).unwrap_or(0);

            let node_id = if let Some(nid) = params.get("nodeId").and_then(|v| v.as_u64())
                .or_else(|| params.get("backendNodeId").and_then(|v| v.as_u64()))
            {
                nid
            } else if let Some(oid) = params.get("objectId").and_then(|v| v.as_str()) {
                let escaped_oid = oid.replace('\\', "\\\\").replace('\'', "\\'");
                let code = format!(
                    "(function() {{ var o = globalThis.__obscura_objects['{}']; if (!o) return -1; return (typeof o._nid === 'number') ? o._nid : -1; }})()",
                    escaped_oid
                );
                let result = page.evaluate(&code);
                result.as_f64().map(|n| n as u64).unwrap_or(0)
            } else {
                return Err("nodeId or objectId required".to_string());
            };

            let node = page.with_dom(|dom| {
                serialize_node(dom, NodeId::new(node_id as u32), depth as u32, 0)
            }).unwrap_or(json!(null));
            Ok(json!({ "node": node }))
        }
        "resolveNode" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let node_id = if let Some(nid) = params.get("nodeId").and_then(|v| v.as_u64())
                .or_else(|| params.get("backendNodeId").and_then(|v| v.as_u64()))
            {
                nid
            } else if let Some(oid) = params.get("objectId").and_then(|v| v.as_str()) {
                let code = format!(
                    "(function() {{ var o = globalThis.__obscura_objects['{}']; return (o && typeof o._nid === 'number') ? o._nid : -1; }})()",
                    oid
                );
                let result = page.evaluate(&code);
                result.as_f64().map(|n| n as u64).unwrap_or(0)
            } else {
                return Err("nodeId or objectId required".to_string());
            };

            let js_code = format!(
                "(function() {{\
                    var nid = {};\
                    var node = null;\
                    if (globalThis._cache && globalThis._cache.has(nid)) {{\
                        node = globalThis._cache.get(nid);\
                    }} else {{\
                        var t = +Deno.core.ops.op_dom('node_type', String(nid), '');\
                        if (t === 1) node = new Element(nid);\
                        else if (t === 9) node = globalThis.document;\
                        else node = new Node(nid);\
                        if (globalThis._cache) globalThis._cache.set(nid, node);\
                    }}\
                    return node;\
                }})()",
                node_id,
            );

            let info = if let Some(js) = &mut page.js {
                match js.store_object_with_meta(&js_code) {
                    Ok(info) => info,
                    Err(_) => {
                        return Ok(json!({
                            "object": {
                                "type": "object",
                                "subtype": "node",
                                "className": "HTMLElement",
                                "objectId": format!("node-{}", node_id),
                            }
                        }));
                    }
                }
            } else {
                return Err("No JS runtime".to_string());
            };

            Ok(json!({
                "object": {
                    "type": "object",
                    "subtype": "node",
                    "className": if info.class_name.is_empty() { "HTMLElement".to_string() } else { info.class_name },
                    "description": info.description,
                    "objectId": info.object_id.unwrap_or_else(|| format!("node-{}", node_id)),
                }
            }))
        }
        "setAttributeValue" => Ok(json!({})),
        "removeNode" => Ok(json!({})),
        "focus" => {
            // No layout engine, but obscura's JS focus() sets document.activeElement,
            // which Input.dispatchKeyEvent targets. CDP clients (browser-use) focus an
            // input via DOM.focus before typing; without this their keystrokes land on
            // nothing and the field stays empty.
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let node_id = resolve_node_id(page, params)?;
            let code = format!(
                "(function() {{ var el = globalThis._wrap && globalThis._wrap({0}); \
                 if (el && typeof el.focus === 'function') {{ el.focus(); return true; }} return false; }})()",
                node_id
            );
            let _ = page.evaluate(&code);
            Ok(json!({}))
        }
        "setFileInputFiles" => {
            // Puppeteer's ElementHandle.uploadFile / Playwright's setInputFiles
            // drive an <input type=file> through this CDP call (issue #359). Read
            // each local file, then hand its bytes (base64) to the JS layer, which
            // builds real File objects and fires input+change like a real
            // selection so page code can read/upload them.
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let node_id = resolve_node_id(page, params)?;
            let paths: Vec<String> = params
                .get("files")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();

            let mut specs = Vec::with_capacity(paths.len());
            for p in &paths {
                let bytes = std::fs::read(p)
                    .map_err(|e| format!("setFileInputFiles: cannot read '{}': {}", p, e))?;
                let name = std::path::Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string();
                specs.push(json!({ "name": name, "type": guess_mime(p), "b64": encode_base64(&bytes) }));
            }

            let specs_json = serde_json::to_string(&specs).unwrap_or_else(|_| "[]".to_string());
            let code = format!(
                "(function() {{ var el = globalThis._wrap && globalThis._wrap({0}); \
                 if (el && globalThis.__obscura_setInputFiles) {{ globalThis.__obscura_setInputFiles(el, {1}); return true; }} return false; }})()",
                node_id, specs_json
            );
            let _ = page.evaluate(&code);
            Ok(json!({}))
        }
        "getBoxModel" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let node_id = match resolve_node_id(page, params) {
                Ok(nid) => nid,
                Err(_) => return Ok(json!(null)),
            };
            let code = format!(
                "(function() {{\
                    var el = globalThis._wrap && globalThis._wrap({0});\
                    if (!el || typeof el.getBoundingClientRect !== 'function') return null;\
                    var r = el.getBoundingClientRect();\
                    return [r.left, r.top, r.right, r.top, r.right, r.bottom, r.left, r.bottom,\
                            r.width, r.height];\
                }})()",
                node_id
            );
            let val = page.evaluate(&code);
            let (quad, w, h) = if let Some(arr) = val.as_array() {
                let nums: Vec<f64> = arr.iter().filter_map(|v| v.as_f64()).collect();
                if nums.len() >= 10 {
                    let q: Vec<Value> = nums[..8].iter().map(|n| json!(n)).collect();
                    (q, nums[8], nums[9])
                } else {
                    (vec![json!(8),json!(8),json!(108),json!(8),json!(108),json!(28),json!(8),json!(28)], 100.0, 20.0)
                }
            } else {
                (vec![json!(8),json!(8),json!(108),json!(8),json!(108),json!(28),json!(8),json!(28)], 100.0, 20.0)
            };
            Ok(json!({
                "model": {
                    "content": quad.clone(),
                    "padding": quad.clone(),
                    "border": quad.clone(),
                    "margin": quad,
                    "width": w, "height": h,
                }
            }))
        }
        "getContentQuads" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let node_id = match resolve_node_id(page, params) {
                Ok(nid) => nid,
                Err(_) => return Ok(json!(null)),
            };
            let code = format!(
                "(function() {{\
                    var el = globalThis._wrap && globalThis._wrap({0});\
                    if (!el || typeof el.getBoundingClientRect !== 'function') return null;\
                    var r = el.getBoundingClientRect();\
                    return [r.left, r.top, r.right, r.top, r.right, r.bottom, r.left, r.bottom];\
                }})()",
                node_id
            );
            let val = page.evaluate(&code);
            let quad = if let Some(arr) = val.as_array() {
                let nums: Vec<f64> = arr.iter().filter_map(|v| v.as_f64()).collect();
                if nums.len() == 8 { nums.iter().map(|n| json!(n)).collect::<Vec<_>>() }
                else { vec![json!(8),json!(8),json!(108),json!(8),json!(108),json!(28),json!(8),json!(28)] }
            } else {
                vec![json!(8),json!(8),json!(108),json!(8),json!(108),json!(28),json!(8),json!(28)]
            };
            Ok(json!({ "quads": [quad] }))
        }
        _ => Err(format!("Unknown DOM method: {}", method)),
    }
}

// Hard cap on how deep a getDocument/describeNode response may nest, independent
// of the requested `depth`. DOM.getDocument{depth:-1} arrives here as u32::MAX,
// which on a pathologically deep DOM (trivially scriptable, unbounded by
// html5ever) produces a Value nested that far. Even built without recursion,
// serde_json's own serialization and the Value's Drop recurse over that nesting
// and overflow the stack — on tokio's ~2 MiB worker stacks especially. Bounding
// the depth keeps the response safe to serialize and drop. Real DOMs are shallow
// (deep React trees are a few hundred), so this only truncates pathological
// nesting, which beats crashing the worker (AGENTS.md: "one page must never
// crash a worker"). Mirrors DOMSnapshot's MAX_NODES guard (issue #341).
//
// The number is sized for the ~2 MiB stack of a tokio worker thread (where the
// CDP processor runs, and where `#[test]` threads also run): each DOM level
// becomes two nested JSON containers (object -> "children" array -> object ...),
// so serde_json's recursive serialization and the Value's recursive Drop descend
// ~2x this depth. 256 keeps that a few hundred frames deep with wide margin,
// while still far exceeding any real page. Clients needing a deeper subtree
// re-request it with DOM.requestChildNodes / describeNode on a specific node.
const MAX_SERIALIZE_DEPTH: u32 = 256;

/// Build the CDP Node object for a single node (without its `children` array),
/// returning it together with that node's child ids. `None` for a missing node.
fn node_value(dom: &DomTree, node_id: NodeId) -> Option<(Value, Vec<NodeId>)> {
    let node = dom.get_node(node_id)?;
    let children_ids = dom.children(node_id);
    let child_count = children_ids.len();
    let mut result = json!({ "nodeId": node_id.index(), "backendNodeId": node_id.index(), "childNodeCount": child_count });

    match &node.data {
        NodeData::Document => {
            result["nodeType"] = json!(9); result["nodeName"] = json!("#document");
            result["localName"] = json!(""); result["nodeValue"] = json!("");
            result["documentURL"] = json!(""); result["baseURL"] = json!(""); result["xmlVersion"] = json!("");
        }
        NodeData::Doctype { name, public_id, system_id } => {
            result["nodeType"] = json!(10); result["nodeName"] = json!(name);
            result["localName"] = json!(""); result["nodeValue"] = json!("");
            result["publicId"] = json!(public_id); result["systemId"] = json!(system_id);
        }
        NodeData::Element { name, attrs, .. } => {
            result["nodeType"] = json!(1);
            result["nodeName"] = json!(name.local.as_ref().to_ascii_uppercase());
            result["localName"] = json!(name.local.as_ref());
            result["nodeValue"] = json!("");
            let cdp_attrs: Vec<String> = attrs.iter()
                .flat_map(|a| vec![a.name.local.to_string(), a.value.clone()]).collect();
            result["attributes"] = json!(cdp_attrs);
        }
        NodeData::Text { contents } => {
            result["nodeType"] = json!(3); result["nodeName"] = json!("#text");
            result["localName"] = json!(""); result["nodeValue"] = json!(contents);
        }
        NodeData::Comment { contents } => {
            result["nodeType"] = json!(8); result["nodeName"] = json!("#comment");
            result["localName"] = json!(""); result["nodeValue"] = json!(contents);
        }
        NodeData::ProcessingInstruction { target, data } => {
            result["nodeType"] = json!(7); result["nodeName"] = json!(target);
            result["localName"] = json!(""); result["nodeValue"] = json!(data);
        }
    }

    Some((result, children_ids))
}

/// Serialize a node and its descendants into the CDP Node tree, iteratively.
/// The requested `max_depth` is clamped to `current_depth + MAX_SERIALIZE_DEPTH`
/// so a `depth:-1` (u32::MAX) request on a very deep DOM cannot produce a Value
/// that overflows the stack when serde_json later serializes or drops it. An
/// explicit heap worklist keeps the builder itself off the call stack.
fn serialize_node(dom: &DomTree, node_id: NodeId, max_depth: u32, current_depth: u32) -> Value {
    let max_depth = max_depth.min(current_depth.saturating_add(MAX_SERIALIZE_DEPTH));

    struct Frame {
        value: Value,
        children: Vec<NodeId>,
        next: usize,
        built: Vec<Value>,
        depth: u32,
        expand: bool,
    }

    let (root_value, root_children) = match node_value(dom, node_id) {
        Some(v) => v,
        None => return json!(null),
    };
    let root_expand = current_depth < max_depth && !root_children.is_empty();
    let mut stack = vec![Frame {
        value: root_value,
        children: root_children,
        next: 0,
        built: Vec::new(),
        depth: current_depth,
        expand: root_expand,
    }];

    loop {
        // Decide the next step without holding a borrow across a push.
        let next_child = {
            let top = stack.last_mut().expect("stack is non-empty in loop");
            if top.expand && top.next < top.children.len() {
                let cid = top.children[top.next];
                top.next += 1;
                Some(cid)
            } else {
                None
            }
        };

        match next_child {
            Some(cid) => {
                let child_depth = stack.last().unwrap().depth + 1;
                match node_value(dom, cid) {
                    Some((cval, cchildren)) => {
                        let cexpand = child_depth < max_depth && !cchildren.is_empty();
                        stack.push(Frame {
                            value: cval,
                            children: cchildren,
                            next: 0,
                            built: Vec::new(),
                            depth: child_depth,
                            expand: cexpand,
                        });
                    }
                    // Missing child: match the old recursive behavior of emitting null.
                    None => stack.last_mut().unwrap().built.push(json!(null)),
                }
            }
            None => {
                // This node's children are all built; finalize and fold into parent.
                let mut frame = stack.pop().unwrap();
                if !frame.built.is_empty() {
                    frame.value["children"] = json!(frame.built);
                }
                match stack.last_mut() {
                    Some(parent) => parent.built.push(frame.value),
                    None => return frame.value,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::CdpContext;

    #[tokio::test]
    async fn dom_focus_sets_active_element() {
        // CDP clients (browser-use) focus an input via DOM.focus before typing;
        // dispatchKeyEvent then targets document.activeElement. DOM.focus must
        // actually move focus or keystrokes land on nothing.
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session = Some(format!("{page_id}-session"));
        ctx.sessions.insert(session.clone().unwrap(), page_id.clone());

        crate::domains::page::handle(
            "navigate",
            &json!({ "url": "data:text/html,<input id=q>", "waitUntil": "load" }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate should succeed");

        let qs = handle("querySelector", &json!({ "selector": "input" }), &mut ctx, &session)
            .await
            .expect("querySelector should succeed");
        let nid = qs["nodeId"].as_u64().expect("input nodeId");
        assert!(nid > 0, "the input element should be found");

        handle("focus", &json!({ "nodeId": nid }), &mut ctx, &session)
            .await
            .expect("DOM.focus should succeed");

        let active = ctx
            .get_session_page_mut(&session)
            .unwrap()
            .evaluate("(function(){return document.activeElement?document.activeElement.tagName:'NONE';})()");
        assert_eq!(
            active,
            json!("INPUT"),
            "DOM.focus must set document.activeElement to the focused input"
        );
    }

    // A hostile or heavy page can nest nodes tens of thousands deep (trivially
    // scriptable, and html5ever puts no cap on generic nesting). DOM.getDocument
    // with the standard depth:-1 (which becomes u32::MAX here) must handle such a
    // tree without crashing the worker (AGENTS.md: "one page must never crash a
    // worker"). Two failure modes: (1) a recursive serialize_node overflows the
    // stack building the Value, and (2) even an iterative builder would emit a
    // Value so deeply nested that serde_json's own recursive serialize/Drop
    // overflow. The fix derecurses the builder AND bounds the depth, so the
    // response is always safe to serialize and drop. With the old recursive
    // serialize_node this test aborts with SIGABRT.
    #[test]
    fn get_document_deep_tree_does_not_overflow() {
        use obscura_dom::{DomTree, NodeData};

        // Build the deep chain directly (no parser) so setup is O(n) and fast.
        let dom = DomTree::new();
        let mut parent = dom.document();
        let depth = 50_000usize;
        for _ in 0..depth {
            let n = dom.new_node(NodeData::Text { contents: String::new() });
            dom.append_child(parent, n);
            parent = n;
        }

        // Mirror getDocument {"depth": -1}: as_i64() == -1, then `depth as u32`.
        let node = serialize_node(&dom, dom.document(), (-1i64) as u32, 0);

        // serde_json's own serialization recurses over the Value nesting; this
        // must not overflow either. That is why the fix bounds depth, not just
        // the builder.
        let s = serde_json::to_string(&node).expect("serialize");
        assert!(!s.is_empty());

        // Output nesting is bounded well below the tree's true depth (truncated,
        // not crashed) yet still serializes a meaningful prefix.
        let mut cur = &node;
        let mut levels = 0usize;
        while let Some(children) = cur.get("children").and_then(|c| c.as_array()) {
            let Some(first) = children.first() else { break };
            cur = first;
            levels += 1;
        }
        assert!(levels >= 100, "should serialize a deep prefix, got {levels}");
        assert!(
            levels < depth,
            "nesting must be bounded below the tree's true depth, got {levels}"
        );
    }
}
