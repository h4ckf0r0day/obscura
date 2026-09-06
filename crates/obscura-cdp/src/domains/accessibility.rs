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

/// Build a CDP AXValue for a boolean type.
fn ax_value_boolean(b: bool) -> Value {
    json!({"type": "boolean", "value": b})
}

/// Build a CDP AXValue for an integer type.
fn ax_value_integer(i: u32) -> Value {
    json!({"type": "integer", "value": i})
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

    // Resolve parentId — walk DOM ancestors until we find one in the AX tree
    let parent_id: Option<String> = {
        let mut current = node_id;
        let mut result = None;
        loop {
            let next_parent = dom.with_node(current, |n| n.parent).flatten();
            match next_parent {
                Some(pid) => {
                    if let Some(ax_pid) = dom_to_ax.get(&pid.raw()) {
                        result = Some(ax_pid.clone());
                        break;
                    }
                    current = pid;
                }
                None => break,
            }
        }
        result
    };

    // Build node with only non-empty optional fields (per CDP spec, optional fields should be omitted when empty)
    let mut ax_node = json!({
        "nodeId": ax_id,
        "ignored": false,
        "role": ax_value_role(role),
    });

    if let Some(ref pid) = parent_id {
        ax_node.as_object_mut().unwrap().insert("parentId".into(), json!(pid));
    }
    if let Some(ref n) = name {
        ax_node.as_object_mut().unwrap().insert("name".into(), json!(ax_value_string(n)));
    }
    if let Some(ref v) = value {
        ax_node.as_object_mut().unwrap().insert("value".into(), json!(ax_value_string(v)));
    }
    if !properties.is_empty() {
        ax_node.as_object_mut().unwrap().insert("properties".into(), json!(properties));
    }
    if !child_ids.is_empty() {
        ax_node.as_object_mut().unwrap().insert("childIds".into(), json!(child_ids));
    }
    ax_node.as_object_mut().unwrap().insert("backendDOMNodeId".into(), json!(node_id.raw()));

    Some(ax_node)
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
fn compute_value(dom: &DomTree, node: &obscura_dom::Node) -> Option<String> {
    if let NodeData::Element { name, attrs, .. } = &node.data {
        let tag = name.local.as_ref();
        // For native form controls, return the value attribute.
        if tag == "input" || tag == "textarea" || tag == "select" {
            return attrs
                .iter()
                .find(|a| a.name.local.as_ref() == "value")
                .map(|a| a.value.clone());
        }

        if is_content_editing_host(dom, node) {
            return Some(dom.text_content(node.id));
        }
    }
    None
}

fn contenteditable_keyword(node: &obscura_dom::Node) -> Option<bool> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    let value = &attrs
        .iter()
        .find(|attr| attr.name.local.as_ref() == "contenteditable")?
        .value;

    if value.is_empty()
        || value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("plaintext-only")
    {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn is_effectively_contenteditable(dom: &DomTree, node_id: NodeId) -> bool {
    let mut current = Some(node_id);
    while let Some(id) = current {
        let Some(node) = dom.get_node(id) else {
            return false;
        };
        if let Some(editable) = contenteditable_keyword(&node) {
            return editable;
        }
        current = node.parent;
    }
    false
}

fn is_content_editing_host(dom: &DomTree, node: &obscura_dom::Node) -> bool {
    is_effectively_contenteditable(dom, node.id)
        && node
            .parent
            .is_none_or(|parent| !is_effectively_contenteditable(dom, parent))
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
            props.push(json!({"name": "focusable", "value": ax_value_boolean(true)}));
        }

        // editable
        if tag == "input" || tag == "textarea"
            || attrs
                .iter()
                .any(|a| a.name.local.as_ref() == "contenteditable" && a.value != "false")
        {
            props.push(json!({"name": "editable", "value": ax_value_boolean(true)}));
        }

        // checked for checkboxes/radios
        if attrs
            .iter()
            .any(|a| a.name.local.as_ref() == "checked")
        {
            props.push(json!({"name": "checked", "value": ax_value_boolean(true)}));
        }

        // disabled
        if attrs
            .iter()
            .any(|a| a.name.local.as_ref() == "disabled")
        {
            props.push(json!({"name": "disabled", "value": ax_value_boolean(true)}));
        }

        // level for headings
        if let Some(level) = tag.strip_prefix('h').and_then(|s| s.parse::<u32>().ok()) {
            if level >= 1 && level <= 6 {
                props.push(json!({"name": "level", "value": ax_value_integer(level)}));
            }
        }

        // required
        if attrs
            .iter()
            .any(|a| a.name.local.as_ref() == "required" || a.name.local.as_ref() == "aria-required")
        {
            props.push(json!({"name": "required", "value": ax_value_boolean(true)}));
        }

        // multiline for textarea
        if tag == "textarea" {
            props.push(json!({"name": "multiline", "value": ax_value_boolean(true)}));
        }

        props
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obscura_dom::parse_html;

    fn ax_node_for<'a>(nodes: &'a [Value], backend_node_id: NodeId) -> &'a Value {
        nodes
            .iter()
            .find(|node| node["backendDOMNodeId"] == backend_node_id.raw())
            .expect("element is present in the AX tree")
    }

    fn assert_ax_value(nodes: &[Value], dom: &DomTree, id: &str, expected: Option<&str>) {
        let node = ax_node_for(nodes, dom.get_element_by_id(id).unwrap());
        assert_eq!(
            node.get("value").and_then(|value| value["value"].as_str()),
            expected,
            "unexpected AX value for #{id}"
        );
    }

    #[test]
    fn content_editing_hosts_expose_ax_values_without_duplicating_descendants() {
        let dom = parse_html(
            r#"<div id="true" contenteditable="TrUe">true</div>
               <div id="empty-keyword" contenteditable>empty keyword</div>
               <div id="plaintext" contenteditable="PlAiNtExT-OnLy">plaintext</div>
               <div id="empty-host" contenteditable></div>
               <div id="host" contenteditable="true">host<span id="inherited"> inherited</span><span id="invalid" contenteditable="bogus"> invalid</span><span id="nested-explicit" contenteditable="plaintext-only"> nested</span></div>
               <div id="disabled" contenteditable="FaLsE">disabled<span id="invalid-disabled" contenteditable="bogus"> inherited off</span><span id="reenabled" contenteditable="TRUE">reenabled<span id="reenabled-child"> child</span></span></div>"#,
        );
        let nodes = build_ax_nodes(&dom);

        assert_ax_value(&nodes, &dom, "true", Some("true"));
        assert_ax_value(&nodes, &dom, "empty-keyword", Some("empty keyword"));
        assert_ax_value(&nodes, &dom, "plaintext", Some("plaintext"));
        assert_ax_value(&nodes, &dom, "empty-host", Some(""));
        assert_ax_value(&nodes, &dom, "host", Some("host inherited invalid nested"));
        assert_ax_value(&nodes, &dom, "inherited", None);
        assert_ax_value(&nodes, &dom, "invalid", None);
        assert_ax_value(&nodes, &dom, "nested-explicit", None);
        assert_ax_value(&nodes, &dom, "disabled", None);
        assert_ax_value(&nodes, &dom, "invalid-disabled", None);
        assert_ax_value(&nodes, &dom, "reenabled", Some("reenabled child"));
        assert_ax_value(&nodes, &dom, "reenabled-child", None);
    }
}
