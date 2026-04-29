use obscura_dom::{DomTree, NodeData, NodeId};
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

/// Build a CDP AXValue for a role type.
fn ax_value_role(role: &str) -> Value {
    json!({"type": "role", "value": role})
}

/// Build a CDP AXValue for a string type.
fn ax_value_string(s: &str) -> Value {
    json!({"type": "string", "value": s})
}

pub async fn handle(
    method: &str,
    _params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => Ok(json!({})),
        "getFullAXTree" => {
            let page = ctx
                .get_session_page(session_id)
                .ok_or("No page")?;
            let nodes = page
                .with_dom(|dom| build_ax_nodes(dom))
                .unwrap_or_default();
            Ok(json!({ "nodes": nodes }))
        }
        _ => Ok(json!({})),
    }
}

/// Walk the full DOM tree and produce CDP Accessibility AXNode array.
fn build_ax_nodes(dom: &DomTree) -> Vec<Value> {
    let mut nodes: Vec<Value> = Vec::new();
    let mut id_counter: u32 = 0;
    // Map DOM NodeId → AX string id, populated only for nodes actually in the AX tree
    let mut dom_to_ax: std::collections::HashMap<u32, String> = std::collections::HashMap::new();

    let document = dom.document();

    // Collect all DOM nodes in tree order (root + descendants)
    let mut all_dom_ids: Vec<NodeId> = vec![document];
    all_dom_ids.extend(dom.descendants(document));

    // First pass: assign AX IDs only to nodes that will produce an AX node
    let mut eligible: Vec<NodeId> = Vec::new();
    for dom_id in &all_dom_ids {
        // Quick check without full build_ax_node (just role check to avoid borrow issues)
        if let Some(node) = dom.get_node(*dom_id) {
            let role = map_role(&node.data);
            if !role.is_empty() {
                id_counter += 1;
                dom_to_ax.insert(dom_id.raw(), id_counter.to_string());
                eligible.push(*dom_id);
            }
        }
    }

    // Second pass: build AXNode for eligible nodes
    for dom_id in &eligible {
        if let Some(ax) = build_ax_node(dom, *dom_id, &dom_to_ax) {
            nodes.push(ax);
        }
    }

    nodes
}

fn build_ax_node(
    dom: &DomTree,
    node_id: NodeId,
    dom_to_ax: &std::collections::HashMap<u32, String>,
) -> Option<Value> {
    let node = dom.get_node(node_id)?;
    let ax_id = dom_to_ax.get(&node_id.raw())?.clone();

    let role = map_role(&node.data);
    // Skip non-relevant nodes (Document, Doctype, Comment, PI)
    if role.is_empty() {
        return None;
    }

    let name = compute_name(dom, &node);
    let value = compute_value(dom, &node);
    let properties = compute_properties(dom, &node);

    let child_ids: Vec<String> = dom
        .children(node_id)
        .iter()
        .filter_map(|child_id| dom_to_ax.get(&child_id.raw()).cloned())
        .collect();

    Some(json!({
        "nodeId": ax_id,
        "ignored": false,
        "role": ax_value_role(role),
        "name": name.as_deref().map(ax_value_string),
        "value": value.as_deref().map(ax_value_string),
        "properties": properties,
        "childIds": child_ids,
        "backendDOMNodeId": node_id.raw(),
    }))
}

/// Map HTML element tag to ARIA role value.
fn map_role(data: &NodeData) -> &'static str {
    match data {
        NodeData::Document => "RootWebArea",
        NodeData::Element { name, attrs, .. } => {
            let tag = name.local.as_ref();

            // Check explicit role attribute first
            if let Some(role_attr) = attrs.iter().find(|a| a.name.local.as_ref() == "role") {
                return match role_attr.value.as_str() {
                    "button" => "button",
                    "link" => "link",
                    "heading" => "heading",
                    "textbox" | "searchbox" => "textbox",
                    "checkbox" => "checkbox",
                    "radio" => "radio",
                    "listbox" => "listbox",
                    "combobox" => "combobox",
                    "list" => "list",
                    "listitem" => "listitem",
                    "navigation" => "navigation",
                    "banner" => "banner",
                    "main" => "main",
                    "complementary" => "complementary",
                    "contentinfo" => "contentinfo",
                    "form" => "form",
                    "table" => "table",
                    "row" => "row",
                    "cell" | "gridcell" => "cell",
                    "img" => "image",
                    "dialog" => "dialog",
                    "alert" => "alert",
                    "tab" => "tab",
                    "tablist" => "tablist",
                    "tabpanel" => "tabpanel",
                    "menu" => "menu",
                    "menuitem" => "menuitem",
                    "toolbar" => "toolbar",
                    "separator" => "separator",
                    "presentation" | "none" => {
                        // presentation/none roles get the role but content is still in tree
                        "presentation"
                    }
                    _ => "generic",
                };
            }

            match tag {
                "a" => {
                    if attrs.iter().any(|a| a.name.local.as_ref() == "href") {
                        "link"
                    } else {
                        "generic"
                    }
                }
                "button" | "summary" => "button",
                "input" => {
                    let type_attr = attrs
                        .iter()
                        .find(|a| a.name.local.as_ref() == "type")
                        .map(|a| a.value.as_str())
                        .unwrap_or("text");
                    match type_attr {
                        "submit" | "reset" | "button" | "image" => "button",
                        "checkbox" => "checkbox",
                        "radio" => "radio",
                        "range" => "slider",
                        "number" => "spinbutton",
                        "search" => "searchbox",
                        _ => "textbox",
                    }
                }
                "textarea" => "textbox",
                "select" => {
                    if attrs.iter().any(|a| {
                        a.name.local.as_ref() == "multiple"
                            || a.name.local.as_ref() == "size"
                    }) {
                        "listbox"
                    } else {
                        "combobox"
                    }
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
                "img" | "svg" => "image",
                "ul" | "ol" | "menu" => "list",
                "li" => "listitem",
                "table" => "table",
                "tr" => "row",
                "td" | "th" => "cell",
                "nav" => "navigation",
                "header" => "banner",
                "main" => "main",
                "footer" => "contentinfo",
                "form" => "form",
                "dialog" => "dialog",
                "hr" => "separator",
                "label" => "LabelText",
                "article" => "article",
                "aside" => "complementary",
                "section" => "region",
                "figure" => "figure",
                "figcaption" => "StaticText",
                "p" | "div" | "span" | "pre" | "blockquote" | "code"
                | "em" | "strong" | "b" | "i" | "u" | "s" | "small"
                | "sub" | "sup" | "mark" | "del" | "ins" => "generic",
                "iframe" => "Iframe",
                _ => "generic",
            }
        }
        NodeData::Text { .. } => "StaticText",
        NodeData::Doctype { .. } | NodeData::Comment { .. } | NodeData::ProcessingInstruction { .. } => {
            ""
        }
    }
}

/// Compute the accessible name for a node.
fn compute_name(dom: &DomTree, node: &obscura_dom::Node) -> Option<String> {
    if let NodeData::Element { attrs, .. } = &node.data {
        // aria-label takes highest priority
        if let Some(label) = attrs
            .iter()
            .find(|a| a.name.local.as_ref() == "aria-label")
            .map(|a| a.value.clone())
        {
            return Some(label);
        }

        // aria-labelledby
        if let Some(labelledby) = attrs
            .iter()
            .find(|a| a.name.local.as_ref() == "aria-labelledby")
        {
            let ids: Vec<&str> = labelledby.value.split_whitespace().collect();
            let mut name = String::new();
            for id_str in ids {
                if let Some(ref_id) = dom.get_element_by_id(id_str) {
                    name.push_str(&dom.text_content(ref_id));
                    name.push(' ');
                }
            }
            let trimmed = name.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }

        // alt attribute for images
        if let Some(alt) = attrs
            .iter()
            .find(|a| a.name.local.as_ref() == "alt")
            .map(|a| a.value.clone())
        {
            if !alt.is_empty() {
                return Some(alt);
            }
        }

        // title attribute
        if let Some(title) = attrs
            .iter()
            .find(|a| a.name.local.as_ref() == "title")
            .map(|a| a.value.clone())
        {
            if !title.is_empty() {
                return Some(title);
            }
        }

        // placeholder
        if let Some(placeholder) = attrs
            .iter()
            .find(|a| a.name.local.as_ref() == "placeholder")
            .map(|a| a.value.clone())
        {
            if !placeholder.is_empty() {
                return Some(placeholder);
            }
        }
    }

    // For text nodes, the name is the text content
    if let NodeData::Text { contents } = &node.data {
        let trimmed = contents.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }

    None
}

/// Compute the accessible value for a node (e.g., current input value).
fn compute_value(_dom: &DomTree, node: &obscura_dom::Node) -> Option<String> {
    if let NodeData::Element { name, attrs, .. } = &node.data {
        let tag = name.local.as_ref();
        // For input elements, return the value attribute
        if tag == "input" || tag == "textarea" || tag == "select" {
            if let Some(val) = attrs
                .iter()
                .find(|a| a.name.local.as_ref() == "value")
                .map(|a| a.value.clone())
            {
                return Some(val);
            }
        }
    }
    None
}

/// Compute accessibility properties for a node.
fn compute_properties(_dom: &DomTree, node: &obscura_dom::Node) -> Vec<Value> {
    if let NodeData::Element { name, attrs, .. } = &node.data {
        let tag = name.local.as_ref();
        let mut props = Vec::new();

        // focusable
        let focusable = matches!(
            tag,
            "a" | "button" | "input" | "select" | "textarea" | "details" | "summary"
        ) || attrs.iter().any(|a| {
            let an = a.name.local.as_ref();
            an == "tabindex" || an == "contenteditable"
        });
        if focusable {
            props.push(json!({"name": "focusable", "value": {"type": "boolean", "value": true}}));
        }

        // editable
        if tag == "input" || tag == "textarea"
            || attrs
                .iter()
                .any(|a| a.name.local.as_ref() == "contenteditable" && a.value != "false")
        {
            props.push(json!({"name": "editable", "value": {"type": "boolean", "value": true}}));
        }

        // checked for checkboxes/radios
        if attrs
            .iter()
            .any(|a| a.name.local.as_ref() == "checked")
        {
            props.push(json!({"name": "checked", "value": {"type": "boolean", "value": true}}));
        }

        // disabled
        if attrs
            .iter()
            .any(|a| a.name.local.as_ref() == "disabled")
        {
            props.push(json!({"name": "disabled", "value": {"type": "boolean", "value": true}}));
        }

        // level for headings
        if let Some(level) = tag.strip_prefix('h').and_then(|s| s.parse::<u32>().ok()) {
            if level >= 1 && level <= 6 {
                props.push(json!({"name": "level", "value": {"type": "integer", "value": level}}));
            }
        }

        // required
        if attrs
            .iter()
            .any(|a| a.name.local.as_ref() == "required" || a.name.local.as_ref() == "aria-required")
        {
            props.push(json!({"name": "required", "value": {"type": "boolean", "value": true}}));
        }

        // multiline for textarea
        if tag == "textarea" {
            props.push(json!({"name": "multiline", "value": {"type": "boolean", "value": true}}));
        }

        props
    } else {
        Vec::new()
    }
}
