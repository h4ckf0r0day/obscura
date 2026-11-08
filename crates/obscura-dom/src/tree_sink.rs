use std::borrow::Cow;
use std::cell::Ref;
use std::fmt;

use html5ever::tendril::StrTendril;
use html5ever::tree_builder::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute as HtmlAttribute, LocalName, Namespace, QualName};

use crate::tree::{Attribute, DomTree, NodeData, NodeId, ShadowRootMode};

const MAX_PARSER_DEPTH: usize = 512;

impl DomTree {
    fn parser_insertion_parent(&self, parent: NodeId) -> (NodeId, Option<usize>) {
        if let Some((previous_parent, parent_depth, previous_child, child_depth)) =
            self.parser_depth_cache.get()
        {
            let depth = if parent == previous_parent {
                Some(parent_depth)
            } else if parent == previous_child {
                Some(child_depth)
            } else {
                None
            };
            if let Some(depth) = depth {
                if depth <= MAX_PARSER_DEPTH {
                    return (parent, Some(depth));
                }
                let up = self.with_node(parent, |node| node.parent).flatten().unwrap_or(parent);
                return (up, Some(depth - 1));
            }
        }
        let inner = self.borrow_inner();
        let mut depth = 0;
        let mut current = parent;
        while let Some(up) = inner.nodes.get(current.index())
            .and_then(Option::as_ref).and_then(|node| node.parent)
        {
            depth += 1;
            if depth > MAX_PARSER_DEPTH {
                let insertion_parent = inner.nodes.get(parent.index()).and_then(Option::as_ref)
                    .and_then(|node| node.parent).unwrap_or(parent);
                let known_depth = inner.nodes.get(up.index()).and_then(Option::as_ref)
                    .is_some_and(|node| node.parent.is_none()).then_some(depth - 1);
                return (insertion_parent, known_depth);
            }
            current = up;
        }
        (parent, Some(depth))
    }
}

/// DOM's valid-shadow-host-name predicate. Gecko's
/// `nsContentUtils::IsValidShadowHostName` uses this same HTML allowlist plus
/// valid custom-element names; keeping the check at the parser boundary makes
/// an invalid declarative template fall back to an ordinary inert template.
fn is_valid_shadow_host(tree: &DomTree, id: NodeId) -> bool {
    let Some(node) = tree.get_node(id) else {
        return false;
    };
    let Some(name) = node.as_element() else {
        return false;
    };
    if name.ns != ns!(html) {
        return false;
    }
    let local = name.local.as_ref();
    if matches!(
        local,
        "article"
            | "aside"
            | "blockquote"
            | "body"
            | "div"
            | "footer"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "main"
            | "nav"
            | "p"
            | "section"
            | "span"
    ) {
        return true;
    }

    let mut chars = local.chars();
    if !chars.next().is_some_and(|first| first.is_ascii_lowercase())
        || !local.contains('-')
        || local.chars().any(|ch| {
            ch.is_ascii_uppercase()
                || ch == '\0'
                || matches!(
                    ch,
                    '\u{0009}' | '\u{000A}' | '\u{000C}' | '\u{000D}' | '\u{0020}'
                )
                || matches!(ch, '/' | '>')
        })
    {
        return false;
    }
    !matches!(
        local,
        "annotation-xml"
            | "color-profile"
            | "font-face"
            | "font-face-src"
            | "font-face-uri"
            | "font-face-format"
            | "font-face-name"
            | "missing-glyph"
    )
}

/// The element name html5ever asks for while parsing. A `Ref` into the tree's
/// `RefCell` keeps the borrow alive for exactly as long as the name is used,
/// so the borrow checker enforces what a raw pointer plus a phantom guard
/// used to promise by convention.
pub struct ObscuraElemName<'a> {
    name: Ref<'a, QualName>,
}

impl<'a> fmt::Debug for ObscuraElemName<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", &*self.name)
    }
}

impl<'a> ElemName for ObscuraElemName<'a> {
    fn ns(&self) -> &Namespace {
        &self.name.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.name.local
    }
}

impl TreeSink for DomTree {
    type Handle = NodeId;
    type Output = Self;
    type ElemName<'a> = ObscuraElemName<'a>;

    fn finish(self) -> Self::Output {
        self
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> NodeId {
        self.document()
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> ObscuraElemName<'a> {
        let name = Ref::map(self.borrow_inner(), |inner| {
            let node = inner
                .nodes
                .get(target.index())
                .and_then(|n| n.as_ref())
                .expect("elem_name called on invalid node");
            match &node.data {
                NodeData::Element { name, .. } => name,
                _ => panic!("elem_name called on non-element"),
            }
        });
        ObscuraElemName { name }
    }

    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<HtmlAttribute>,
        flags: ElementFlags,
    ) -> NodeId {
        let converted_attrs: Vec<Attribute> = attrs
            .into_iter()
            .map(|a| Attribute {
                name: a.name,
                value: a.value.to_string(),
            })
            .collect();

        let id = self.new_node(NodeData::Element {
            name: name.clone(),
            attrs: converted_attrs,
            template_contents: None,
            mathml_annotation_xml_integration_point: flags.mathml_annotation_xml_integration_point,
        });

        if flags.template {
            let template_doc = self.new_node(NodeData::Document);
            self.with_node_mut(id, |node| {
                if let NodeData::Element { template_contents, .. } = &mut node.data {
                    *template_contents = Some(template_doc);
                }
            });
        }

        id
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.new_node(NodeData::Comment {
            contents: text.to_string(),
        })
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        self.new_node(NodeData::ProcessingInstruction {
            target: target.to_string(),
            data: data.to_string(),
        })
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        match child {
            NodeOrText::AppendNode(node_id) => {
                // Chromium flattens further parser insertions at this depth
                // without changing the tree builder's open-elements stack.
                let (insertion_parent, depth) = self.parser_insertion_parent(*parent);
                self.append_child(insertion_parent, node_id);
                let cache = depth.filter(|_| {
                    self.with_node(node_id, |node| node.parent).flatten() == Some(insertion_parent)
                }).map(|depth| (insertion_parent, depth, node_id, depth + 1));
                self.parser_depth_cache.set(cache);
            }
            NodeOrText::AppendText(text) => {
                let cache = self.parser_depth_cache.get();
                self.append_text(*parent, &text);
                self.parser_depth_cache.set(cache);
            }
        }
    }

    fn associate_with_form(&self, target: &NodeId, form: &NodeId, nodes: (&NodeId, Option<&NodeId>)) {
        self.set_parser_form_owner(*target, *form, *nodes.0);
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let has_parent = self.with_node(*element, |n| n.parent.is_some()).unwrap_or(false);
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let doctype = self.new_node(NodeData::Doctype {
            name: name.to_string(),
            public_id: public_id.to_string(),
            system_id: system_id.to_string(),
        });
        let doc = self.document();
        self.append_child(doc, doctype);
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<HtmlAttribute>) {
        self.with_node_mut(*target, |node| {
            if let NodeData::Element { attrs: existing, .. } = &mut node.data {
                for attr in attrs {
                    let dominated = existing.iter().any(|a| a.name == attr.name);
                    if !dominated {
                        existing.push(Attribute {
                            name: attr.name,
                            value: attr.value.to_string(),
                        });
                    }
                }
            }
        });
    }

    fn remove_from_parent(&self, target: &NodeId) {
        self.detach(*target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        let children = self.children(*node);
        for child_id in children {
            self.append_child(*new_parent, child_id);
        }
    }

    fn append_before_sibling(&self, sibling: &NodeId, child: NodeOrText<NodeId>) {
        match child {
            NodeOrText::AppendNode(node_id) => {
                self.insert_before(*sibling, node_id);
            }
            NodeOrText::AppendText(text) => {
                let prev_text_id = {
                    let node = self.get_node(*sibling);
                    node.and_then(|n| n.prev_sibling).and_then(|prev_id| {
                        let prev = self.get_node(prev_id);
                        prev.and_then(|p| if p.is_text() { Some(prev_id) } else { None })
                    })
                };

                if let Some(prev_text_id) = prev_text_id {
                    self.with_node_mut(prev_text_id, |node| {
                        if let NodeData::Text { contents } = &mut node.data {
                            contents.push_str(&text);
                        }
                    });
                    return;
                }

                let text_id = self.new_node(NodeData::Text {
                    contents: text.to_string(),
                });
                self.insert_before(*sibling, text_id);
            }
        }
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        self.with_node(*target, |n| match &n.data {
            NodeData::Element { template_contents, .. } => *template_contents,
            _ => None,
        })
        .flatten()
        .expect("get_template_contents called on non-template element")
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        // Only full quirks mode makes CSS class/id selectors case-insensitive;
        // limited-quirks behaves like no-quirks for selector matching.
        self.set_quirks(mode == QuirksMode::Quirks);
    }

    fn allow_declarative_shadow_roots(&self, intended_parent: &NodeId) -> bool {
        self.allows_declarative_shadow_roots()
            && is_valid_shadow_host(self, *intended_parent)
            && self.shadow_root(*intended_parent).is_none()
    }

    fn attach_declarative_shadow(
        &self,
        location: &NodeId,
        template: &NodeId,
        attrs: &[HtmlAttribute],
    ) -> bool {
        let mode = attrs.iter().find_map(|attr| {
            if attr.name.local.as_ref() != "shadowrootmode" {
                return None;
            }
            match attr.value.as_ref() {
                "open" => Some(ShadowRootMode::Open),
                "closed" => Some(ShadowRootMode::Closed),
                _ => None,
            }
        });
        let Some(mode) = mode else {
            return false;
        };
        let root = self
            .with_node(*template, |node| match &node.data {
                NodeData::Element {
                    template_contents, ..
                } => *template_contents,
                _ => None,
            })
            .flatten();
        let Some(root) = root else {
            return false;
        };
        if self.attach_shadow_root_node(*location, root, mode).is_err() {
            return false;
        }
        // The temporary template was never inserted on the successful path,
        // but create_element registered any `id` before attachment. Use the
        // DOM removal path so that stale template ids cannot escape through
        // document.getElementById; template contents are a separate fragment
        // and remain alive as the native root.
        self.remove_child(*template);
        true
    }

    fn is_mathml_annotation_xml_integration_point(&self, target: &NodeId) -> bool {
        self.with_node(*target, |n| match &n.data {
            NodeData::Element { mathml_annotation_xml_integration_point, .. } => {
                *mathml_annotation_xml_integration_point
            }
            _ => false,
        })
        .unwrap_or(false)
    }
}

pub fn parse_html(html: &str) -> DomTree {
    use html5ever::tendril::TendrilSink;
    use html5ever::{parse_document, ParseOpts};

    let tree = DomTree::new();
    tree.set_allow_declarative_shadow_roots(true);
    parse_document(tree, ParseOpts::default())
        .from_utf8()
        .one(html.as_bytes())
}

/// Parse an iframe srcdoc with its standards-mode and scripting rules.
pub fn parse_srcdoc(html: &str, scripting_enabled: bool) -> DomTree {
    use html5ever::tendril::TendrilSink;
    let tree = DomTree::new();
    tree.set_allow_declarative_shadow_roots(true);
    let mut opts = html5ever::ParseOpts::default();
    opts.tree_builder.iframe_srcdoc = true;
    opts.tree_builder.scripting_enabled = scripting_enabled;
    html5ever::parse_document(tree, opts).from_utf8().one(html.as_bytes())
}

pub fn parse_fragment(html: &str) -> DomTree {
    let context_name = QualName::new(None, ns!(html), local_name!("body"));
    parse_fragment_with_context(html, context_name)
}

/// Parse an HTML fragment using the supplied context element.
///
/// The tree builder's insertion mode depends on this context. Treating every
/// `innerHTML` assignment as body content drops table-only elements such as a
/// top-level `<tr>` and mis-parses select/template fragments. Browsers instead
/// use the receiver element as the fragment parsing context.
pub fn parse_fragment_with_context(html: &str, context_name: QualName) -> DomTree {
    use html5ever::tendril::TendrilSink;
    use html5ever::{parse_fragment, ParseOpts};
    let tree = DomTree::new();
    // Obscura's fragment parser backs innerHTML in a scripting-enabled
    // document. html5ever 0.39 makes that context flag explicit; keeping it
    // true preserves browser parsing for context-sensitive content such as
    // <noscript>.
    parse_fragment(tree, ParseOpts::default(), context_name, vec![], true)
        .from_utf8()
        .one(html.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn max_div_nesting(tree: &DomTree) -> usize {
        let mut stack = vec![(tree.document(), 0)];
        let mut maximum = 0;
        while let Some((id, depth)) = stack.pop() {
            let is_div = tree.with_node(id, |node| {
                node.as_element().is_some_and(|name| name.local.as_ref() == "div")
            }).unwrap_or(false);
            let depth = depth + usize::from(is_div);
            maximum = maximum.max(depth);
            stack.extend(tree.children(id).into_iter().map(|child| (child, depth)));
        }
        maximum
    }

    #[test]
    fn document_parser_caps_nesting_without_dropping_content() {
        for (count, expected_depth) in [(400, 400), (1000, 511), (100000, 511)] {
            let tree = parse_html(&format!("{}content", "<div>".repeat(count)));
            assert_eq!(max_div_nesting(&tree), expected_depth);
            assert_eq!(tree.query_selector_all("div").unwrap().len(), count);
            assert_eq!(tree.text_content(tree.document()), "content");
        }
    }

    #[test]
    fn fragment_parser_caps_nesting_without_dropping_content() {
        for (count, expected_depth) in [(400, 400), (1000, 512), (100000, 512)] {
            let tree = parse_fragment(&format!("{}content", "<div>".repeat(count)));
            assert_eq!(max_div_nesting(&tree), expected_depth);
            assert_eq!(tree.query_selector_all("div").unwrap().len(), count);
            assert_eq!(tree.text_content(tree.document()), "content");
        }
    }

    #[test]
    fn parser_depth_cap_does_not_restrict_dom_mutations() {
        let tree = parse_html("<main id='root'></main>");
        let mut parent = tree.get_element_by_id("root").unwrap();
        for _ in 0..600 {
            let child = tree.new_node(NodeData::Element {
                name: QualName::new(None, ns!(html), local_name!("div")),
                attrs: vec![],
                template_contents: None,
                mathml_annotation_xml_integration_point: false,
            });
            tree.append_child(parent, child);
            parent = child;
        }
        assert_eq!(max_div_nesting(&tree), 600);
    }

    #[test]
    fn parser_depth_cache_is_invalidated_by_reparenting() {
        let tree = parse_html("<main id='deep'></main><section id='shallow'></section>");
        let mut deep = tree.get_element_by_id("deep").unwrap();
        for _ in 0..600 {
            let child = tree.create_element(QualName::new(None, ns!(html), local_name!("div")),
                vec![], ElementFlags::default());
            tree.append_child(deep, child);
            deep = child;
        }
        let shallow = tree.get_element_by_id("shallow").unwrap();
        let leaf = tree.create_element(QualName::new(None, ns!(html), local_name!("div")),
            vec![], ElementFlags::default());
        TreeSink::append(&tree, &shallow, NodeOrText::AppendNode(leaf));
        assert_eq!(tree.parser_insertion_parent(leaf).0, leaf);
        tree.append_child(deep, shallow);
        assert_eq!(tree.parser_insertion_parent(leaf).0, shallow);
    }

    #[test]
    fn parser_depth_cache_is_invalidated_when_nodes_leave_a_deep_parent() {
        for mutation in ["append", "insert", "detach", "raw"] {
            let tree = parse_html(&"<div>".repeat(1000));
            let leaf = *tree.query_selector_all("div").unwrap().last().unwrap();
            let parent = tree.with_node(leaf, |node| node.parent).flatten().unwrap();
            assert_eq!(tree.parser_insertion_parent(leaf).0, parent);
            let document = tree.document();
            let html = tree.children(document)[0];
            match mutation {
                "append" => tree.append_child(document, leaf),
                "insert" => tree.insert_before(html, leaf),
                "detach" => tree.detach(leaf),
                "raw" => { tree.with_node_mut(leaf, |node| node.parent = None); }
                _ => unreachable!(),
            }
            let expected_depth = usize::from(matches!(mutation, "append" | "insert"));
            assert_eq!(tree.parser_insertion_parent(leaf), (leaf, Some(expected_depth)));
        }
    }

    #[test]
    fn test_parse_simple_html() {
        let tree = parse_html("<html><head></head><body><h1>Hello</h1></body></html>");
        assert!(tree.len() > 3);
        let text = tree.text_content(tree.document());
        assert!(text.contains("Hello"));
    }

    #[test]
    fn test_parse_with_attributes() {
        let tree = parse_html(r#"<div id="main" class="container">Text</div>"#);
        let main = tree.get_element_by_id("main");
        assert!(main.is_some());
        let node = tree.get_node(main.unwrap()).unwrap();
        assert_eq!(node.get_attribute("class"), Some("container"));
    }

    #[test]
    fn test_parse_nested_structure() {
        let tree = parse_html(
            r#"<html><body>
                <div id="outer">
                    <p id="para">Hello <strong>World</strong></p>
                    <ul>
                        <li>Item 1</li>
                        <li>Item 2</li>
                    </ul>
                </div>
            </body></html>"#,
        );

        let outer = tree.get_element_by_id("outer").unwrap();
        let text = tree.text_content(outer);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(text.contains("Item 1"));
        assert!(text.contains("Item 2"));
    }

    #[test]
    fn test_parse_malformed_html() {
        let tree = parse_html("<div><p>Unclosed paragraph<p>Another<div>Nested wrong</div>");
        assert!(tree.len() > 3);
        let text = tree.text_content(tree.document());
        assert!(text.contains("Unclosed paragraph"));
        assert!(text.contains("Another"));
    }

    #[test]
    fn test_parse_doctype() {
        let tree = parse_html("<!DOCTYPE html><html><body>Hello</body></html>");
        let first_child = tree.children(tree.document())[0];
        let node = tree.get_node(first_child).unwrap();
        assert!(matches!(node.data, NodeData::Doctype { .. }));
    }

    #[test]
    fn test_parse_fragment() {
        let tree = parse_fragment("<p>Hello</p><p>World</p>");
        let text = tree.text_content(tree.document());
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
    }

    #[test]
    fn test_parse_fragment_uses_table_context() {
        let context_name = QualName::new(None, ns!(html), local_name!("template"));
        let tree = parse_fragment_with_context("<tr><td>cell</td></tr>", context_name);
        let row = tree
            .query_selector("tr")
            .expect("valid selector")
            .expect("template context preserves the row");
        assert_eq!(tree.text_content(row), "cell");
    }

    fn element_children(tree: &DomTree, parent: NodeId) -> Vec<NodeId> {
        tree.children(parent)
            .into_iter()
            .filter(|child| tree.get_node(*child).is_some_and(|node| node.is_element()))
            .collect()
    }

    #[test]
    fn full_document_consumes_open_and_closed_declarative_shadow_templates() {
        let tree = parse_html(
            r#"<x-open id="open-host">
                 <template id="open-template" shadowrootmode="open">
                   <span id="open-content">open shadow</span>
                 </template>
                 <b id="open-light">open light</b>
               </x-open>
               <x-closed id="closed-host">
                 <template id="closed-template" shadowrootmode="closed">
                   <span id="closed-content">closed shadow</span>
                 </template>
                 <b id="closed-light">closed light</b>
               </x-closed>"#,
        );

        let open_host = tree.get_element_by_id("open-host").unwrap();
        let closed_host = tree.get_element_by_id("closed-host").unwrap();
        let open_light = tree.get_element_by_id("open-light").unwrap();
        let closed_light = tree.get_element_by_id("closed-light").unwrap();
        let open_root = tree.shadow_root(open_host).expect("open root attached");
        let closed_root = tree.shadow_root(closed_host).expect("closed root attached");

        assert_eq!(
            tree.shadow_root_info(open_root).unwrap().mode,
            ShadowRootMode::Open
        );
        assert_eq!(
            tree.shadow_root_info(closed_root).unwrap().mode,
            ShadowRootMode::Closed
        );
        assert_eq!(element_children(&tree, open_host), vec![open_light]);
        assert_eq!(element_children(&tree, closed_host), vec![closed_light]);
        assert!(tree.get_element_by_id("open-template").is_none());
        assert!(tree.get_element_by_id("closed-template").is_none());
        assert!(
            tree.query_selector_from(open_root, "#open-content")
                .unwrap()
                .is_some()
        );
        assert!(
            tree.query_selector_from(closed_root, "#closed-content")
                .unwrap()
                .is_some()
        );
        assert!(
            tree.get_element_by_id("open-content").is_none()
                && tree.get_element_by_id("closed-content").is_none(),
            "document id lookup must not pierce either shadow mode"
        );
    }

    #[test]
    fn invalid_declarative_shadow_mode_remains_an_ordinary_template() {
        let tree = parse_html(
            r#"<x-card id="host"><template id="invalid" shadowrootmode="Open"><span id="inside"></span></template></x-card>
               <button id="invalid-host"><template id="invalid-host-template" shadowrootmode="open"><i id="invalid-host-content"></i></template></button>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let template = tree.get_element_by_id("invalid").unwrap();
        let contents = tree.template_contents(template).unwrap();
        let inside = tree.get_element_by_id("inside").unwrap();

        assert_eq!(tree.shadow_root(host), None);
        assert_eq!(element_children(&tree, host), vec![template]);
        assert_eq!(element_children(&tree, contents), vec![inside]);

        let invalid_host = tree.get_element_by_id("invalid-host").unwrap();
        let invalid_host_template = tree.get_element_by_id("invalid-host-template").unwrap();
        assert_eq!(tree.shadow_root(invalid_host), None);
        assert_eq!(
            element_children(&tree, invalid_host),
            vec![invalid_host_template],
            "an HTML element outside the valid-shadow-host allowlist stays inert"
        );
    }

    #[test]
    fn duplicate_declarative_shadow_root_falls_back_to_an_inert_template() {
        let tree = parse_html(
            r#"<x-card id="host">
                 <template shadowrootmode="open"><span id="first"></span></template>
                 <template id="duplicate" shadowrootmode="closed"><span id="second"></span></template>
                 <b id="light"></b>
               </x-card>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let light = tree.get_element_by_id("light").unwrap();
        let duplicate = tree.get_element_by_id("duplicate").unwrap();
        let duplicate_contents = tree.template_contents(duplicate).unwrap();
        let second = tree.get_element_by_id("second").unwrap();
        let root = tree.shadow_root(host).unwrap();

        assert_eq!(tree.shadow_root_info(root).unwrap().mode, ShadowRootMode::Open);
        assert!(tree.query_selector_from(root, "#first").unwrap().is_some());
        assert_eq!(element_children(&tree, host), vec![duplicate, light]);
        assert_eq!(element_children(&tree, duplicate_contents), vec![second]);
    }

    #[test]
    fn nested_declarative_shadow_roots_keep_distinct_tree_scopes() {
        let tree = parse_html(
            r#"<x-outer id="outer-host">
                 <template shadowrootmode="open">
                   <x-inner id="inner-host">
                     <template shadowrootmode="closed"><i id="inner-shadow"></i></template>
                     <b id="inner-light"></b>
                   </x-inner>
                 </template>
               </x-outer>"#,
        );
        let outer_host = tree.get_element_by_id("outer-host").unwrap();
        let outer_root = tree.shadow_root(outer_host).unwrap();
        let inner_host = tree
            .query_selector_from(outer_root, "#inner-host")
            .unwrap()
            .unwrap();
        let inner_root = tree.shadow_root(inner_host).unwrap();

        assert_eq!(tree.containing_shadow_root(inner_host), Some(outer_root));
        assert_eq!(tree.shadow_root_info(inner_root).unwrap().mode, ShadowRootMode::Closed);
        assert!(
            tree.query_selector_from(outer_root, "#inner-shadow")
                .unwrap()
                .is_none(),
            "an outer-tree query must not pierce a nested root"
        );
        assert!(
            tree.query_selector_from(inner_root, "#inner-shadow")
                .unwrap()
                .is_some()
        );
        assert!(
            tree.query_selector_from(outer_root, "#inner-light")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn fragment_parsing_keeps_declarative_shadow_templates_inert() {
        let tree = parse_fragment_with_context(
            r#"<template id="shadow" shadowrootmode="open"><span id="inside"></span></template>"#,
            QualName::new(None, ns!(html), LocalName::from("x-card")),
        );
        let template = tree.get_element_by_id("shadow").unwrap();
        let contents = tree.template_contents(template).unwrap();
        let inside = tree.get_element_by_id("inside").unwrap();

        assert!(!tree.allows_declarative_shadow_roots());
        assert_eq!(element_children(&tree, contents), vec![inside]);
        assert!(tree.containing_shadow_root(inside).is_none());
    }

    #[test]
    fn ordinary_template_parsing_is_unchanged() {
        let tree = parse_html(
            r#"<div id="host">
                 <template id="ordinary"><span id="inside">content</span></template>
                 <span id="outside">light</span>
               </div>"#,
        );

        let host = tree.get_element_by_id("host").unwrap();
        let template = tree.get_element_by_id("ordinary").unwrap();
        let contents = tree.template_contents(template).unwrap();
        let inside = tree.get_element_by_id("inside").unwrap();
        let outside = tree.get_element_by_id("outside").unwrap();
        let element_children = |parent| {
            tree.children(parent)
                .into_iter()
                .filter(|child| tree.get_node(*child).is_some_and(|node| node.is_element()))
                .collect::<Vec<_>>()
        };

        assert_eq!(element_children(host), vec![template, outside]);
        assert!(tree.children(template).is_empty());
        assert_eq!(element_children(contents), vec![inside]);
        assert_eq!(tree.get_node(inside).unwrap().parent, Some(contents));
    }

    #[test]
    fn dormant_tree_sink_hook_reuses_the_template_contents_identity() {
        let tree = DomTree::new();
        let host = tree.new_node(NodeData::Element {
            name: QualName::new(None, ns!(html), LocalName::from("x-card")),
            attrs: vec![],
            template_contents: None,
            mathml_annotation_xml_integration_point: false,
        });
        tree.append_child(tree.document(), host);
        let contents = tree.new_node(NodeData::Document);
        let template = tree.new_node(NodeData::Element {
            name: QualName::new(None, ns!(html), local_name!("template")),
            attrs: vec![],
            template_contents: Some(contents),
            mathml_annotation_xml_integration_point: false,
        });
        tree.append_child(host, template);
        let attrs = vec![HtmlAttribute {
            name: QualName::new(
                None,
                Namespace::default(),
                LocalName::from("shadowrootmode"),
            ),
            value: StrTendril::from("closed"),
        }];

        assert!(!TreeSink::allow_declarative_shadow_roots(&tree, &host));
        assert!(TreeSink::attach_declarative_shadow(
            &tree,
            &host,
            &template,
            &attrs,
        ));
        assert_eq!(tree.shadow_root(host), Some(contents));
        assert!(tree.children(host).is_empty());
        assert_eq!(tree.get_node(template).unwrap().parent, None);
        assert_eq!(
            tree.shadow_root_info(contents).unwrap().mode,
            ShadowRootMode::Closed
        );
    }
}
