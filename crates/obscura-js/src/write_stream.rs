//! The document's input stream for `document.write()`.
//!
//! There is one input stream per document, and the tokenizer carries its state across calls.
//! A construct may therefore be split at any point, even in the middle of a tag name.
//! <https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-write>
//!
//! This module inserts nothing itself. It creates the nodes and reports where each belongs,
//! because `Node.appendChild` on the JS side reports the mutation at the same time, registers
//! window named access, and loads a written stylesheet.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use html5ever::driver::{parse_fragment, ParseOpts, Parser};
use html5ever::tendril::TendrilSink;
use html5ever::tree_builder::Tracer;
use html5ever::{local_name, ns, QualName};
use obscura_dom::{DomTree, NodeData, NodeId};

/// Collects every node the TreeBuilder still holds.
///
/// `TreeSink::pop` would be the obvious signal for "done" and does not deliver: of html5ever's
/// three ways to drain the stack of open elements, only one reports to the sink. An end tag
/// usually goes through `pop_until`, which does not.
#[derive(Default)]
struct RetainedNodes(RefCell<HashSet<NodeId>>);

impl Tracer for RetainedNodes {
    type Handle = NodeId;

    fn trace_handle(&self, node: &NodeId) {
        self.0.borrow_mut().insert(*node);
    }
}

/// `parent` is `None` for a node at the head of the input stream, which belongs at the
/// insertion point.
pub(crate) struct Placement {
    pub(crate) parent: Option<NodeId>,
    pub(crate) node: NodeId,
}

/// A `<script>` runs as soon as it is inserted, so a half-written one must not.
/// A `<template>` keeps its children in its own contents document, which a child walk does
/// not reach, so it can only be copied as a whole.
fn needs_to_be_complete(source: &DomTree, node: NodeId) -> bool {
    source
        .get_node(node)
        .and_then(|n| n.as_element().map(|name| name.local.clone()))
        .is_some_and(|local| local == local_name!("script") || local == local_name!("template"))
}

pub(crate) struct DocumentWriteStream {
    parser: Parser<DomTree>,
    /// Maps a node of the parser tree to its copy in the document. A node missing here has
    /// not been handed over yet.
    handed_over: HashMap<NodeId, NodeId>,
    /// Staging for copying a whole subtree, reused.
    staging: Option<NodeId>,
}

impl DocumentWriteStream {
    pub(crate) fn new() -> Self {
        // Scripting enabled, as with the fragment parser behind innerHTML.
        let context = QualName::new(None, ns!(html), local_name!("body"));
        DocumentWriteStream {
            parser: parse_fragment(DomTree::new(), ParseOpts::default(), context, vec![], true),
            handed_over: HashMap::new(),
            staging: None,
        }
    }

    /// Pushes a call's arguments into the input stream and mirrors what the parser made of them.
    /// Returns the nodes to insert, parents before children.
    pub(crate) fn write(&mut self, html: &str, dom: &DomTree) -> Vec<Placement> {
        self.parser.process(html.into());

        let retained = RetainedNodes::default();
        self.parser.tokenizer.sink.trace_handles(&retained);
        let retained = retained.0.into_inner();

        // Separate fields: the parser tree is read, the mapping is written.
        let source = &self.parser.tokenizer.sink.sink;
        let handed_over = &mut self.handed_over;
        let staging = &mut self.staging;

        let root = source.fragment_root();
        let mut placements = Vec::new();
        // Only look at what is new. A walk over all children would cost as much per call as
        // the input stream written so far is long, so quadratic over a thousand calls. Measured at
        // 5000 calls: 852 ms versus 4130 ms.
        let mut stack: Vec<NodeId> = fresh_children(source, root, handed_over);

        while let Some(current) = stack.pop() {
            let node = match source.get_node(current) {
                Some(node) => node,
                None => continue,
            };

            if let Some(&copy) = handed_over.get(&current) {
                // A text node at the end of the input stream grows with every call and stays the
                // same node. Elements no longer change after their creation.
                if node.is_text() {
                    let text = source.text_content(current);
                    dom.with_node_mut(copy, |n| {
                        if let NodeData::Text { contents } = &mut n.data {
                            *contents = text;
                        }
                    });
                }
                for child in fresh_children(source, current, handed_over) {
                    stack.push(child);
                }
                continue;
            }

            let parent = match node.parent {
                Some(p) if p == root => None,
                // If the parent is held back, this one waits along.
                Some(p) => match handed_over.get(&p) {
                    Some(&copy) => Some(copy),
                    None => continue,
                },
                None => continue,
            };

            if needs_to_be_complete(source, current) {
                if retained.contains(&current) {
                    continue;
                }
                let container = *staging.get_or_insert_with(|| dom.new_node(NodeData::Document));
                let Some(copy) = dom.import_node_from(container, source, current) else {
                    continue;
                };
                dom.detach(copy);
                map_subtree(source, current, dom, copy, handed_over);
                placements.push(Placement { parent, node: copy });
                continue;
            }

            // Handed over shallow and left to grow in place. That makes an element that is
            // never closed appear at once instead of never.
            let copy = dom.new_node(node.data.clone());
            handed_over.insert(current, copy);
            placements.push(Placement { parent, node: copy });
            for child in fresh_children(source, current, handed_over) {
                stack.push(child);
            }
        }

        placements
    }
}

/// The children of `parent` still to do, as a stack: the top element is processed first.
///
/// The walk goes backward from the last child and stops at the first one already handed over.
/// The parser only appends at the back, so everything before it is done. This one already
/// handed-over child comes back along, because a text node at the end of the input stream keeps
/// growing and an open element still receives children.
fn fresh_children(
    source: &DomTree,
    parent: NodeId,
    handed_over: &HashMap<NodeId, NodeId>,
) -> Vec<NodeId> {
    let mut stack = Vec::new();
    let mut current = source.get_node(parent).and_then(|n| n.last_child);
    while let Some(id) = current {
        stack.push(id);
        if handed_over.contains_key(&id) {
            break;
        }
        current = source.get_node(id).and_then(|n| n.prev_sibling);
    }
    stack
}

/// After copying, both trees have the same shape, so a lockstep walk aligns the nodes.
fn map_subtree(
    source: &DomTree,
    source_node: NodeId,
    dom: &DomTree,
    copy: NodeId,
    handed_over: &mut HashMap<NodeId, NodeId>,
) {
    let mut pairs = vec![(source_node, copy)];
    while let Some((from, to)) = pairs.pop() {
        handed_over.insert(from, to);
        let from_children = source.children(from);
        let to_children = dom.children(to);
        for (from_child, to_child) in from_children.into_iter().zip(to_children) {
            pairs.push((from_child, to_child));
        }
    }
}
