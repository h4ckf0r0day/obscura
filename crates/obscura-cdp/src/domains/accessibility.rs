use std::collections::{HashMap, HashSet};

use obscura_dom::{DomTree, NodeData, NodeId};
use obscura_js::runtime::AccessibilityStyle;
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
            #[cfg(feature = "render")]
            let styles = page
                .js
                .as_ref()
                .map(|js| js.accessibility_styles())
                .unwrap_or_default();
            #[cfg(not(feature = "render"))]
            let styles = HashMap::new();
            let nodes = page
                .with_dom(|dom| build_ax_nodes_with_styles(dom, &styles))
                .unwrap_or_default();
            Ok(json!({ "nodes": nodes }))
        }
        _ => Ok(json!({})),
    }
}

/// Walk the full DOM tree and produce CDP Accessibility AXNode array.
#[cfg(test)]
fn build_ax_nodes(dom: &DomTree) -> Vec<Value> {
    build_ax_nodes_with_styles(dom, &HashMap::new())
}

fn build_ax_nodes_with_styles(
    dom: &DomTree,
    styles: &HashMap<NodeId, AccessibilityStyle>,
) -> Vec<Value> {
    let mut nodes: Vec<Value> = Vec::new();
    let mut id_counter: u32 = 0;
    // Map DOM NodeId → AX string id, populated only for nodes actually in the AX tree
    let mut dom_to_ax: std::collections::HashMap<u32, String> = std::collections::HashMap::new();

    let document = dom.document();

    // Collect all DOM nodes in tree order (root + descendants)
    let mut all_dom_ids: Vec<NodeId> = vec![document];
    all_dom_ids.extend(dom.descendants(document));
    let context = NameContext::new(dom, &all_dom_ids, styles);

    // First pass: assign AX IDs only to nodes that will produce an AX node
    let mut eligible: Vec<NodeId> = Vec::new();
    for dom_id in &all_dom_ids {
        // Quick check without full build_ax_node (just role check to avoid borrow issues)
        if let Some(node) = dom.get_node(*dom_id) {
            let role = map_role(&node.data);
            if !role.is_empty() && !context.is_hidden(*dom_id) {
                id_counter += 1;
                dom_to_ax.insert(dom_id.raw(), id_counter.to_string());
                eligible.push(*dom_id);
            }
        }
    }

    // Second pass: build AXNode for eligible nodes
    for dom_id in &eligible {
        if let Some(ax) = build_ax_node(dom, *dom_id, &dom_to_ax, &context) {
            nodes.push(ax);
        }
    }

    let mut children: HashMap<String, Vec<Value>> = HashMap::new();
    for node in &nodes {
        if let Some(parent) = node["parentId"].as_str() {
            children
                .entry(parent.to_string())
                .or_default()
                .push(node["nodeId"].clone());
        }
    }
    for node in &mut nodes {
        if let Some(ids) = children.remove(node["nodeId"].as_str().unwrap()) {
            node["childIds"] = json!(ids);
        }
    }
    nodes
}

fn build_ax_node(
    dom: &DomTree,
    node_id: NodeId,
    dom_to_ax: &std::collections::HashMap<u32, String>,
    context: &NameContext,
) -> Option<Value> {
    let node = dom.get_node(node_id)?;
    let ax_id = dom_to_ax.get(&node_id.raw())?.clone();

    let role = map_role(&node.data);
    // Skip non-relevant nodes (Document, Doctype, Comment, PI)
    if role.is_empty() {
        return None;
    }

    let name = compute_name(dom, &node, context);
    let value = compute_value(dom, &node);
    let properties = compute_properties(dom, &node);

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

#[derive(Clone, Copy, Default)]
struct Visibility {
    subtree_hidden: bool,
    visibility_hidden: bool,
}

struct NameContext {
    visibility: HashMap<NodeId, Visibility>,
    elements_by_id: HashMap<String, NodeId>,
    labels: HashMap<NodeId, Vec<NodeId>>,
}

impl NameContext {
    fn new(dom: &DomTree, ids: &[NodeId], styles: &HashMap<NodeId, AccessibilityStyle>) -> Self {
        let mut context = Self {
            visibility: HashMap::new(),
            elements_by_id: HashMap::new(),
            labels: HashMap::new(),
        };
        for id in ids {
            let Some(node) = dom.get_node(*id) else {
                continue;
            };
            let parent = node
                .parent
                .and_then(|id| context.visibility.get(&id))
                .copied()
                .unwrap_or_default();
            let style = styles.get(id).copied().unwrap_or_default();
            let hidden = node.get_attribute("hidden").is_some()
                || node
                    .get_attribute("aria-hidden")
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"))
                || matches!(
                    tag(&node),
                    "head" | "script" | "style" | "template" | "noscript"
                )
                || (tag(&node) == "input" && input_type(&node) == "hidden");
            context.visibility.insert(
                *id,
                Visibility {
                    subtree_hidden: parent.subtree_hidden || hidden || style.display_none,
                    visibility_hidden: style.visibility_hidden.unwrap_or(parent.visibility_hidden),
                },
            );
            if let Some(value) = node.get_attribute("id").filter(|value| !value.is_empty()) {
                context
                    .elements_by_id
                    .entry(value.to_string())
                    .or_insert(*id);
            }
        }
        for id in ids {
            let Some(node) = dom.get_node(*id) else {
                continue;
            };
            if tag(&node) != "label" {
                continue;
            }
            let control = if let Some(target) = node.get_attribute("for") {
                context
                    .elements_by_id
                    .get(target)
                    .copied()
                    .filter(|id| dom.get_node(*id).is_some_and(|node| is_labelable(&node)))
            } else {
                dom.descendants(*id)
                    .into_iter()
                    .find(|id| dom.get_node(*id).is_some_and(|node| is_labelable(&node)))
            };
            if let Some(control) = control {
                context.labels.entry(control).or_default().push(*id);
            }
        }
        context
    }

    fn is_hidden(&self, id: NodeId) -> bool {
        self.visibility
            .get(&id)
            .is_some_and(|state| state.subtree_hidden || state.visibility_hidden)
    }
}

fn tag(node: &obscura_dom::Node) -> &str {
    node.as_element()
        .map(|name| name.local.as_ref())
        .unwrap_or("")
}

fn input_type(node: &obscura_dom::Node) -> String {
    node.get_attribute("type")
        .unwrap_or("text")
        .to_ascii_lowercase()
}

fn is_labelable(node: &obscura_dom::Node) -> bool {
    matches!(
        tag(node),
        "button" | "meter" | "output" | "progress" | "select" | "textarea"
    ) || (tag(node) == "input" && input_type(node) != "hidden")
}

fn is_nested_control(node: &obscura_dom::Node) -> bool {
    matches!(
        tag(node),
        "input" | "textarea" | "select" | "button" | "summary" | "iframe"
    ) || contenteditable_keyword(node) == Some(true)
        || matches!(
            map_role(&node.data),
            "button"
                | "textbox"
                | "searchbox"
                | "checkbox"
                | "radio"
                | "listbox"
                | "combobox"
                | "slider"
                | "spinbutton"
                | "menuitem"
                | "tab"
        )
}

fn flat_string(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn nonempty(value: &str) -> Option<String> {
    let value = flat_string(value);
    (!value.is_empty()).then_some(value)
}

fn text_alternative(node: &obscura_dom::Node) -> Option<String> {
    if let Some(label) = node.get_attribute("aria-label").and_then(nonempty) {
        return Some(label);
    }
    if tag(node) == "img" || (tag(node) == "input" && input_type(node) == "image") {
        return node.get_attribute("alt").map(flat_string);
    }
    if tag(node) == "input" {
        match input_type(node).as_str() {
            "button" => return node.get_attribute("value").and_then(nonempty),
            "submit" => {
                return Some(
                    node.get_attribute("value")
                        .map(flat_string)
                        .unwrap_or_else(|| "Submit".into()),
                )
            }
            "reset" => {
                return Some(
                    node.get_attribute("value")
                        .map(flat_string)
                        .unwrap_or_else(|| "Reset".into()),
                )
            }
            _ => {}
        }
    }
    None
}

fn content_name(
    dom: &DomTree,
    root: NodeId,
    context: &NameContext,
    include_hidden: bool,
) -> String {
    let mut text = String::new();
    let mut pending = vec![(root, false)];
    while let Some((id, end_block)) = pending.pop() {
        if end_block {
            text.push(' ');
            continue;
        }
        let Some(node) = dom.get_node(id) else {
            continue;
        };
        if !include_hidden
            && context
                .visibility
                .get(&id)
                .is_some_and(|state| state.subtree_hidden)
        {
            continue;
        }
        if matches!(tag(&node), "script" | "style" | "template" | "noscript") {
            continue;
        }
        if id != root && is_nested_control(&node) {
            continue;
        }
        let hidden = !include_hidden && context.is_hidden(id);
        if let NodeData::Text { contents } = &node.data {
            if !hidden {
                text.push_str(contents);
            }
            continue;
        }
        if !hidden {
            if let Some(alternative) = text_alternative(&node) {
                text.push(' ');
                text.push_str(&alternative);
                text.push(' ');
                continue;
            }
        }
        let block = matches!(
            tag(&node),
            "br" | "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
        );
        if block {
            text.push(' ');
            pending.push((id, true));
        }
        pending.extend(dom.children(id).into_iter().rev().map(|id| (id, false)));
    }
    flat_string(&text)
}

/// Compute the accessible name for a node.
fn compute_name(dom: &DomTree, node: &obscura_dom::Node, context: &NameContext) -> Option<String> {
    if let NodeData::Text { contents } = &node.data {
        return nonempty(contents);
    }
    if let Some(labelledby) = node.get_attribute("aria-labelledby") {
        let mut seen = HashSet::new();
        let references: Vec<_> = labelledby
            .split_whitespace()
            .filter_map(|id| context.elements_by_id.get(id).copied())
            .filter(|id| seen.insert(*id))
            .collect();
        if !references.is_empty() {
            let parts: Vec<_> = references
                .into_iter()
                .map(|id| {
                    if let Some(alternative) =
                        dom.get_node(id).and_then(|node| text_alternative(&node))
                    {
                        return alternative;
                    }
                    let text = content_name(dom, id, context, context.is_hidden(id));
                    if !text.is_empty() {
                        return text;
                    }
                    dom.get_node(id)
                        .and_then(|node| node.get_attribute("title").and_then(nonempty))
                        .unwrap_or_default()
                })
                .collect();
            return Some(flat_string(&parts.join(" ")));
        }
    }
    if let Some(label) = node.get_attribute("aria-label").and_then(nonempty) {
        return Some(label);
    }
    if let Some(labels) = context.labels.get(&node.id) {
        let parts: Vec<_> = labels
            .iter()
            .map(|id| content_name(dom, *id, context, context.is_hidden(*id)))
            .collect();
        if let Some(name) = nonempty(&parts.join(" ")) {
            return Some(name);
        }
    }
    if let Some(alternative) = text_alternative(node) {
        return Some(alternative);
    }
    if matches!(
        map_role(&node.data),
        "button"
            | "link"
            | "heading"
            | "checkbox"
            | "radio"
            | "menuitem"
            | "tab"
            | "cell"
            | "row"
            | "LabelText"
            | "StaticText"
    ) {
        if let Some(name) = nonempty(&content_name(dom, node.id, context, false)) {
            return Some(name);
        }
    }
    if let Some(title) = node.get_attribute("title").and_then(nonempty) {
        return Some(title);
    }
    if matches!(tag(node), "input" | "textarea") {
        return node.get_attribute("placeholder").and_then(nonempty);
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

    fn assert_name(dom: &DomTree, id: &str, expected: Option<&str>) {
        let nodes = build_ax_nodes(dom);
        let node = ax_node_for(&nodes, dom.get_element_by_id(id).unwrap());
        assert_eq!(
            node.get("name").and_then(|name| name["value"].as_str()),
            expected,
            "#{id}"
        );
    }

    #[test]
    fn names_from_content_preserve_inline_words_and_normalize_whitespace() {
        let dom = parse_html(
            r#"<button id="button" title="tooltip"> Save <b>changes</b> now </button>
            <a id="link" href="/">Read <span>the <em>docs</em></span></a>
            <h2 id="heading">Access<b>ibility</b>   test</h2>
            <div id="generic">Do not duplicate all descendant text</div>"#,
        );
        assert_name(&dom, "button", Some("Save changes now"));
        assert_name(&dom, "link", Some("Read the docs"));
        assert_name(&dom, "heading", Some("Accessibility test"));
        assert_name(&dom, "generic", None);
    }

    #[test]
    fn html_labels_follow_control_association_and_document_order() {
        let dom = parse_html(
            r#"<label for="email"> Work <b>email</b> </label>
            <input id="email" placeholder="fallback"><label for="email">address</label>
            <label>Accept <span>terms</span><input id="accept" type="checkbox"></label>
            <label for="email">Elsewhere<input id="wrong"></label>
            <label>First<input id="first"><input id="second"></label>
            <label for="missing">No association<input id="missing-for"></label>"#,
        );
        assert_name(&dom, "email", Some("Work email address Elsewhere"));
        assert_name(&dom, "accept", Some("Accept terms"));
        assert_name(&dom, "wrong", None);
        assert_name(&dom, "first", Some("First"));
        assert_name(&dom, "second", None);
        assert_name(&dom, "missing-for", None);
    }

    #[test]
    fn aria_labelledby_precedes_aria_label_and_native_names() {
        let dom = parse_html(
            r#"<span id="one">First</span><span id="two">Second</span>
            <label for="target">Native</label>
            <button id="target" aria-labelledby="two missing one two" aria-label="ARIA">Content</button>
            <button id="invalid" aria-labelledby="missing" aria-label=" ARIA   fallback ">Content</button>
            <button id="blank" aria-label="  ">Content</button>
            <span id="empty"></span><button id="empty-ref" aria-labelledby="empty" aria-label="fallback">Content</button>
            <button id="self" aria-labelledby="self one" aria-label="Self">Content</button>
            <span id="cycle-a" aria-labelledby="cycle-b">A</span><span id="cycle-b" aria-labelledby="cycle-a">B</span>
            <button id="cycle" aria-labelledby="cycle-a">Content</button>"#,
        );
        assert_name(&dom, "target", Some("Second First"));
        assert_name(&dom, "invalid", Some("ARIA fallback"));
        assert_name(&dom, "blank", Some("Content"));
        assert_name(&dom, "empty-ref", Some(""));
        assert_name(&dom, "self", Some("Self First"));
        assert_name(&dom, "cycle", Some("A"));
    }

    #[test]
    fn hidden_references_name_controls_without_exposing_hidden_nodes() {
        let dom = parse_html(
            r#"<span id="hidden" hidden>Hidden <span hidden>reference</span></span>
            <span id="visible">Visible <span hidden>noise</span></span>
            <button id="referenced" aria-labelledby="hidden visible">Content</button>
            <label hidden for="native">Hidden native</label><input id="native">
            <button id="content">Shown<span aria-hidden="true"> noise</span><script>noise</script></button>"#,
        );
        assert_name(&dom, "referenced", Some("Hidden reference Visible"));
        assert_name(&dom, "native", Some("Hidden native"));
        assert_name(&dom, "content", Some("Shown"));
        let nodes = build_ax_nodes(&dom);
        let hidden = dom.get_element_by_id("hidden").unwrap();
        assert!(!nodes
            .iter()
            .any(|node| node["backendDOMNodeId"] == hidden.raw()));
    }

    #[test]
    fn aria_references_preserve_link_text() {
        let dom = parse_html(
            r#"<span id="label">Read <a href="/terms">terms</a><a href="/secret" hidden> secret</a></span>
            <button id="native-link" aria-labelledby="label" aria-label="Fallback">Continue</button>
            <span id="role-label">Review <span role="link">privacy</span></span>
            <button id="role-link" aria-labelledby="role-label">Continue</button>
            <span id="hidden-label" hidden>Read <a href="/terms" hidden>hidden terms</a></span>
            <button id="hidden-link" aria-labelledby="hidden-label">Continue</button>"#,
        );
        assert_name(&dom, "native-link", Some("Read terms"));
        assert_name(&dom, "role-link", Some("Review privacy"));
        assert_name(&dom, "hidden-link", Some("Read hidden terms"));
    }

    #[test]
    fn native_labels_preserve_link_text() {
        let dom = parse_html(
            r#"<label for="explicit">Read <a href="/terms">terms</a> and <span role="link">privacy</span></label>
            <input id="explicit">
            <label>Accept <a href="/terms">terms</a> and <span role="link">privacy</span><input id="implicit" type="checkbox"></label>
            <label for="hidden" hidden>Read <a href="/terms" hidden>hidden terms</a></label><input id="hidden">"#,
        );
        assert_name(&dom, "explicit", Some("Read terms and privacy"));
        assert_name(&dom, "implicit", Some("Accept terms and privacy"));
        assert_name(&dom, "hidden", Some("Read hidden terms"));
    }

    #[test]
    fn nested_buttons_remain_excluded_from_content_names() {
        let dom = parse_html(
            r#"<button id="outer">Outer</button><button id="inner" role="link">Nested action</button>"#,
        );
        let outer = dom.get_element_by_id("outer").unwrap();
        let inner = dom.get_element_by_id("inner").unwrap();
        dom.append_child(outer, inner);
        assert_eq!(dom.get_node(inner).unwrap().parent, Some(outer));
        assert_name(&dom, "outer", Some("Outer"));
        assert_name(&dom, "inner", Some("Nested action"));
    }

    #[test]
    fn nested_controls_do_not_pollute_labels_or_content_names() {
        let dom = parse_html(
            r#"<label for="field">Account <a href="/">Help</a><select><option>Noise</option></select><textarea>Noise</textarea><span role="textbox">Noise</span><span contenteditable="true">Noise</span><a href="/" role="button">Noise</a><button role="link">Noise</button></label>
            <input id="field"><div role="button" id="button">Open <span role="button">Nested action</span><img alt="folder"></div>"#,
        );
        assert_name(&dom, "field", Some("Account Help"));
        assert_name(&dom, "button", Some("Open folder"));
    }

    #[test]
    fn html_and_aria_hidden_subtrees_are_absent() {
        let dom = parse_html(
            r#"<div hidden="false"><button id="html">Hidden</button></div>
            <div aria-hidden="TrUe"><button id="aria" aria-hidden="false">Hidden</button></div>
            <input id="input" type="hidden"><button id="visible" aria-hidden="false">Visible</button>"#,
        );
        let nodes = build_ax_nodes(&dom);
        for id in ["html", "aria", "input"] {
            let id = dom.get_element_by_id(id).unwrap();
            assert!(!nodes
                .iter()
                .any(|node| node["backendDOMNodeId"] == id.raw()));
        }
        assert_name(&dom, "visible", Some("Visible"));
    }

    #[test]
    fn native_alternatives_and_aria_override_content() {
        let dom = parse_html(
            r#"<button id="aria" aria-label="Named">Content</button>
            <button id="image"><img alt="Download"></button>
            <img id="decorative" alt="" title="Not a name">
            <input id="submit" type="submit" value="Send">
            <input id="password" type="password" value="secret" placeholder="Password">
            <label for="select">Choice</label><select id="select"><option>Not the name</option></select>"#,
        );
        assert_name(&dom, "aria", Some("Named"));
        assert_name(&dom, "image", Some("Download"));
        assert_name(&dom, "decorative", Some(""));
        assert_name(&dom, "submit", Some("Send"));
        assert_name(&dom, "password", Some("Password"));
        assert_name(&dom, "select", Some("Choice"));
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
