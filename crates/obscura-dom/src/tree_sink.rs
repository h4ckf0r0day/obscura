use std::borrow::Cow;
use std::cell::Ref;
use std::fmt;

use html5ever::tendril::StrTendril;
use html5ever::tree_builder::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute as HtmlAttribute, LocalName, Namespace, QualName};

use crate::tree::{Attribute, DomTree, NodeData, NodeId};

pub struct ObscuraElemName<'a> {
    _ref: Ref<'a, ()>,
    name: *const QualName,
}

impl<'a> fmt::Debug for ObscuraElemName<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = unsafe { &*self.name };
        write!(f, "{:?}", name)
    }
}

impl<'a> ElemName for ObscuraElemName<'a> {
    fn ns(&self) -> &Namespace {
        unsafe { &(*self.name).ns }
    }

    fn local_name(&self) -> &LocalName {
        unsafe { &(*self.name).local }
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
        let borrow = self.borrow_inner();
        let node = borrow.nodes.get(target.index())
            .and_then(|n| n.as_ref())
            .expect("elem_name called on invalid node");
        let name_ptr: *const QualName = match &node.data {
            NodeData::Element { name, .. } => name as *const QualName,
            _ => panic!("elem_name called on non-element"),
        };
        let ref_guard = Ref::map(borrow, |_| &());
        ObscuraElemName {
            _ref: ref_guard,
            name: name_ptr,
        }
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
                self.append_child(*parent, node_id);
            }
            NodeOrText::AppendText(text) => {
                self.append_text(*parent, &text);
            }
        }
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

    fn set_quirks_mode(&self, _mode: QuirksMode) {
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
    parse_document(tree, ParseOpts::default())
        .from_utf8()
        .one(html.as_bytes())
}

pub fn parse_fragment(html: &str) -> DomTree {
    use html5ever::tendril::TendrilSink;
    use html5ever::{parse_fragment, ParseOpts, QualName};

    let context_name = QualName::new(None, ns!(html), local_name!("body"));
    let tree = DomTree::new();
    parse_fragment(tree, ParseOpts::default(), context_name, vec![])
        .from_utf8()
        .one(html.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // Build a `depth`-deep linear element chain with a text leaf, directly via
    // the DOM API (O(depth)). Parsing such a tree out of HTML would hit
    // html5ever's super-linear deep-nesting cost; building it lets the DoS tests
    // reach a depth that genuinely overflows a recursive walk while staying fast.
    fn build_deep_tree(depth: usize) -> crate::tree::DomTree {
        use crate::tree::NodeData;
        use html5ever::{LocalName, Namespace, QualName};
        let tree = crate::tree::DomTree::new();
        let name = QualName::new(None, Namespace::from("http://www.w3.org/1999/xhtml"), LocalName::from("div"));
        let mut parent = tree.document();
        for _ in 0..depth {
            let el = tree.new_node(NodeData::Element {
                name: name.clone(),
                attrs: vec![],
                template_contents: None,
                mathml_annotation_xml_integration_point: false,
            });
            tree.append_child(parent, el);
            parent = el;
        }
        let leaf = tree.new_node(NodeData::Text { contents: "leaf".to_string() });
        tree.append_child(parent, leaf);
        tree
    }

    // DOM-01: serializing a deeply nested DOM must not overflow the native stack
    // (which aborts the process). The depth cap turns it into bounded output.
    #[test]
    fn deeply_nested_outer_html_does_not_overflow() {
        let tree = build_deep_tree(50_000);
        let html = tree.outer_html(tree.document());
        assert!(!html.is_empty());
    }

    // DOM-02: text extraction over a deeply nested DOM must not overflow. The
    // walk is iterative, so it stays correct (finds the deep leaf) AND bounded.
    #[test]
    fn deeply_nested_text_content_does_not_overflow() {
        let tree = build_deep_tree(50_000);
        let text = tree.text_content(tree.document());
        assert!(text.contains("leaf"));
    }

    // DOM-03: importing a deeply nested subtree (the innerHTML setter path) must
    // not overflow the stack. The iterative import stays bounded.
    #[test]
    fn deeply_nested_import_does_not_overflow() {
        let src = build_deep_tree(50_000);
        let dest = crate::tree::DomTree::new();
        let root = dest.document();
        dest.import_children_from(root, &src, src.document());
        assert!(dest.len() > 50_000);
    }
}
