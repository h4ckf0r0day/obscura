use obscura_dom::{DomTree, NodeData, NodeId};
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

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
            })
            .ok_or_else(|| "No DOM loaded".to_string())
        }
        "querySelector" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let selector = params
                .get("selector")
                .and_then(|v| v.as_str())
                .ok_or("selector required")?;
            let result = page
                .with_dom(|dom| {
                    dom.query_selector(selector)
                        .ok()
                        .flatten()
                        .map(|id| id.index())
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            Ok(json!({ "nodeId": result }))
        }
        "querySelectorAll" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let selector = params
                .get("selector")
                .and_then(|v| v.as_str())
                .ok_or("selector required")?;
            let ids = page
                .with_dom(|dom| {
                    dom.query_selector_all(selector)
                        .ok()
                        .map(|ids| ids.iter().map(|id| id.index() as u64).collect::<Vec<_>>())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            Ok(json!({ "nodeIds": ids }))
        }
        "getOuterHTML" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let node_id = params
                .get("nodeId")
                .and_then(|v| v.as_u64())
                .or_else(|| params.get("backendNodeId").and_then(|v| v.as_u64()))
                .ok_or("nodeId required")?;
            let html = page
                .with_dom(|dom| dom.outer_html(NodeId::new(node_id as u32)))
                .unwrap_or_default();
            Ok(json!({ "outerHTML": html }))
        }
        "describeNode" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let depth = params.get("depth").and_then(|v| v.as_i64()).unwrap_or(0);

            let node_id = if let Some(nid) = params
                .get("nodeId")
                .and_then(|v| v.as_u64())
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

            let node = page
                .with_dom(|dom| serialize_node(dom, NodeId::new(node_id as u32), depth as u32, 0))
                .unwrap_or(json!(null));
            Ok(json!({ "node": node }))
        }
        "resolveNode" => {
            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let node_id = if let Some(nid) = params
                .get("nodeId")
                .and_then(|v| v.as_u64())
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
        "getBoxModel" => Ok(json!({
            "model": {
                "content": [8,8, 108,8, 108,28, 8,28],
                "padding": [8,8, 108,8, 108,28, 8,28],
                "border": [8,8, 108,8, 108,28, 8,28],
                "margin": [0,0, 116,0, 116,36, 0,36],
                "width": 100, "height": 20,
            }
        })),
        _ => Err(format!("Unknown DOM method: {}", method)),
    }
}

fn serialize_node(dom: &DomTree, node_id: NodeId, max_depth: u32, current_depth: u32) -> Value {
    let node = match dom.get_node(node_id) {
        Some(n) => n,
        None => return json!(null),
    };
    let children_ids = dom.children(node_id);
    let child_count = children_ids.len();
    let mut result = json!({ "nodeId": node_id.index(), "backendNodeId": node_id.index(), "childNodeCount": child_count });

    match &node.data {
        NodeData::Document => {
            result["nodeType"] = json!(9);
            result["nodeName"] = json!("#document");
            result["localName"] = json!("");
            result["nodeValue"] = json!("");
            result["documentURL"] = json!("");
            result["baseURL"] = json!("");
            result["xmlVersion"] = json!("");
        }
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => {
            result["nodeType"] = json!(10);
            result["nodeName"] = json!(name);
            result["localName"] = json!("");
            result["nodeValue"] = json!("");
            result["publicId"] = json!(public_id);
            result["systemId"] = json!(system_id);
        }
        NodeData::Element { name, attrs, .. } => {
            result["nodeType"] = json!(1);
            result["nodeName"] = json!(name.local.as_ref().to_ascii_uppercase());
            result["localName"] = json!(name.local.as_ref());
            result["nodeValue"] = json!("");
            let cdp_attrs: Vec<String> = attrs
                .iter()
                .flat_map(|a| vec![a.name.local.to_string(), a.value.clone()])
                .collect();
            result["attributes"] = json!(cdp_attrs);
        }
        NodeData::Text { contents } => {
            result["nodeType"] = json!(3);
            result["nodeName"] = json!("#text");
            result["localName"] = json!("");
            result["nodeValue"] = json!(contents);
        }
        NodeData::Comment { contents } => {
            result["nodeType"] = json!(8);
            result["nodeName"] = json!("#comment");
            result["localName"] = json!("");
            result["nodeValue"] = json!(contents);
        }
        NodeData::ProcessingInstruction { target, data } => {
            result["nodeType"] = json!(7);
            result["nodeName"] = json!(target);
            result["localName"] = json!("");
            result["nodeValue"] = json!(data);
        }
    }

    if current_depth < max_depth && !children_ids.is_empty() {
        let children: Vec<Value> = children_ids
            .iter()
            .map(|&cid| serialize_node(dom, cid, max_depth, current_depth + 1))
            .collect();
        result["children"] = json!(children);
    }
    result
}
