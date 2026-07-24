use html5ever::{LocalName, Namespace, QualName};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub(crate) u32);

impl NodeId {
    pub fn new(val: u32) -> Self {
        NodeId(val)
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub name: QualName,
    pub value: String,
}

#[derive(Clone, Debug)]
pub enum NodeData {
    Document,
    Doctype {
        name: String,
        public_id: String,
        system_id: String,
    },
    Element {
        name: QualName,
        attrs: Vec<Attribute>,
        template_contents: Option<NodeId>,
        mathml_annotation_xml_integration_point: bool,
    },
    Text {
        contents: String,
    },
    Comment {
        contents: String,
    },
    ProcessingInstruction {
        target: String,
        data: String,
    },
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub last_child: Option<NodeId>,
    pub prev_sibling: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
    pub data: NodeData,
}

impl Node {
    pub fn is_document(&self) -> bool {
        matches!(self.data, NodeData::Document)
    }

    pub fn is_element(&self) -> bool {
        matches!(self.data, NodeData::Element { .. })
    }

    pub fn is_text(&self) -> bool {
        matches!(self.data, NodeData::Text { .. })
    }

    pub fn as_element(&self) -> Option<&QualName> {
        match &self.data {
            NodeData::Element { name, .. } => Some(name),
            _ => None,
        }
    }

    pub fn attrs(&self) -> Option<&[Attribute]> {
        match &self.data {
            NodeData::Element { attrs, .. } => Some(attrs),
            _ => None,
        }
    }

    pub fn attrs_mut(&mut self) -> Option<&mut Vec<Attribute>> {
        match &mut self.data {
            NodeData::Element { attrs, .. } => Some(attrs),
            _ => None,
        }
    }

    pub fn get_attribute(&self, name: &str) -> Option<&str> {
        self.attrs()?.iter().find_map(|a| {
            if a.name.local.as_ref() == name {
                Some(a.value.as_str())
            } else {
                None
            }
        })
    }

    pub fn set_attribute(&mut self, name: &str, value: String) {
        if let NodeData::Element { attrs, .. } = &mut self.data {
            if let Some(attr) = attrs.iter_mut().find(|a| a.name.local.as_ref() == name) {
                attr.value = value;
            } else {
                attrs.push(Attribute {
                    name: QualName::new(None, Namespace::default(), LocalName::from(name)),
                    value,
                });
            }
        }
    }

    pub fn text_content_of_text_node(&self) -> Option<&str> {
        match &self.data {
            NodeData::Text { contents } => Some(contents),
            _ => None,
        }
    }
}

pub struct DomTree {
    inner: RefCell<DomTreeInner>,
}

pub(crate) struct DomTreeInner {
    pub(crate) nodes: Vec<Option<Node>>,
    pub(crate) free_list: Vec<u32>,
    pub(crate) document: NodeId,
    pub(crate) id_index: HashMap<String, NodeId>,
}

impl DomTree {
    pub fn new() -> Self {
        let doc_node = Node {
            id: NodeId(0),
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
            data: NodeData::Document,
        };
        DomTree {
            inner: RefCell::new(DomTreeInner {
                nodes: vec![Some(doc_node)],
                free_list: Vec::new(),
                document: NodeId(0),
                id_index: HashMap::new(),
            }),
        }
    }

    pub fn document(&self) -> NodeId {
        self.inner.borrow().document
    }

    pub(crate) fn borrow_inner(&self) -> std::cell::Ref<'_, DomTreeInner> {
        self.inner.borrow()
    }

    pub fn new_node(&self, data: NodeData) -> NodeId {
        let mut inner = self.inner.borrow_mut();
        let id = if let Some(slot) = inner.free_list.pop() {
            NodeId(slot)
        } else {
            let idx = inner.nodes.len() as u32;
            inner.nodes.push(None);
            NodeId(idx)
        };

        if let NodeData::Element { ref attrs, .. } = data {
            if let Some(id_attr) = attrs.iter().find(|a| a.name.local.as_ref() == "id") {
                // Keep the FIRST element created with a given id. Parse order is
                // document order, so getElementById / querySelector('#id') return
                // the first-in-tree-order element on duplicate ids, per spec.
                inner.id_index.entry(id_attr.value.clone()).or_insert(id);
            }
        }

        inner.nodes[id.index()] = Some(Node {
            id,
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
            data,
        });
        id
    }

    pub fn get_node(&self, id: NodeId) -> Option<Node> {
        self.inner.borrow().nodes.get(id.index())?.clone()
    }

    pub fn with_node<F, R>(&self, id: NodeId, f: F) -> Option<R>
    where
        F: FnOnce(&Node) -> R,
    {
        let inner = self.inner.borrow();
        inner.nodes.get(id.index())?.as_ref().map(f)
    }

    pub fn with_node_mut<F, R>(&self, id: NodeId, f: F) -> Option<R>
    where
        F: FnOnce(&mut Node) -> R,
    {
        let mut inner = self.inner.borrow_mut();
        inner.nodes.get_mut(id.index())?.as_mut().map(f)
    }

    pub fn append_child(&self, parent_id: NodeId, child_id: NodeId) {
        // Per DOM spec, appending a node to itself is a HierarchyRequestError;
        // here we treat it as a no-op rather than panic. Without this the
        // sibling-pointer fixup below sets the node's prev_sibling to itself
        // and every later child-walk loops forever (same failure mode that
        // insert_before's self-cycle guard was added to prevent).
        if parent_id == child_id {
            return;
        }
        // Appending an ancestor of the parent under that parent makes the
        // parent/child graph cyclic, and every later descendants()/children()/
        // textContent walk (none carry a visited set) would loop forever, pinning
        // the thread in native Rust where neither tokio nor the V8 watchdog can
        // interrupt it. Per the DOM spec this is a HierarchyRequestError; treat it
        // as a no-op, like the self-append guard above. Only a node that already
        // has children can be an ancestor, so a fresh/leaf child (the common
        // append) skips the walk: O(1) hot path, O(depth) only when relocating a
        // populated subtree.
        {
            let inner = self.inner.borrow();
            let child_has_children = inner.nodes.get(child_id.index())
                .and_then(|n| n.as_ref())
                .map(|n| n.first_child.is_some())
                .unwrap_or(false);
            if child_has_children {
                let mut cur = inner.nodes.get(parent_id.index())
                    .and_then(|n| n.as_ref())
                    .and_then(|n| n.parent);
                let mut steps = 0usize;
                while let Some(p) = cur {
                    if p == child_id {
                        return;
                    }
                    steps += 1;
                    if steps > inner.nodes.len() {
                        return; // pre-existing corruption: refuse rather than risk a cycle
                    }
                    cur = inner.nodes.get(p.index())
                        .and_then(|n| n.as_ref())
                        .and_then(|n| n.parent);
                }
            }
        }
        self.detach(child_id);

        let mut inner = self.inner.borrow_mut();

        let old_last = inner.nodes.get(parent_id.index())
            .and_then(|n| n.as_ref())
            .and_then(|n| n.last_child);

        if let Some(Some(child)) = inner.nodes.get_mut(child_id.index()) {
            child.parent = Some(parent_id);
            child.prev_sibling = old_last;
            child.next_sibling = None;
        }

        if let Some(old_last_id) = old_last {
            if let Some(Some(old_last_node)) = inner.nodes.get_mut(old_last_id.index()) {
                old_last_node.next_sibling = Some(child_id);
            }
        }

        if let Some(Some(parent)) = inner.nodes.get_mut(parent_id.index()) {
            if parent.first_child.is_none() {
                parent.first_child = Some(child_id);
            }
            parent.last_child = Some(child_id);
        }
    }

    pub fn insert_before(&self, existing_id: NodeId, new_sibling_id: NodeId) {
        // Per DOM spec: if the node being inserted IS the reference node,
        // the operation is a no-op (the node is already in its target
        // position). Without this, the linked-list fixup below sets the
        // node's prev_sibling and next_sibling to itself, creating a cycle
        // -- every later traversal (childNodes, querySelectorAll, etc) then
        // loops forever and the test page hangs while obscura burns RAM.
        if existing_id == new_sibling_id {
            return;
        }
        let parent_id = {
            let inner = self.inner.borrow();
            match inner.nodes.get(existing_id.index()).and_then(|n| n.as_ref()).and_then(|n| n.parent) {
                Some(p) => p,
                None => return,
            }
        };

        // Inserting the parent itself, or any ancestor of the parent, as a child
        // of that parent would create a cycle (same non-terminating-walk hang as
        // append_child). Reject it, matching the self-insert guard above. Gate on
        // the inserted node actually having children, so the common case (insert a
        // fresh node) stays O(1).
        {
            let inner = self.inner.borrow();
            let new_has_children = inner.nodes.get(new_sibling_id.index())
                .and_then(|n| n.as_ref())
                .map(|n| n.first_child.is_some())
                .unwrap_or(false);
            if new_has_children {
                let mut cur = Some(parent_id);
                let mut steps = 0usize;
                while let Some(p) = cur {
                    if p == new_sibling_id {
                        return;
                    }
                    steps += 1;
                    if steps > inner.nodes.len() {
                        return;
                    }
                    cur = inner.nodes.get(p.index())
                        .and_then(|n| n.as_ref())
                        .and_then(|n| n.parent);
                }
            }
        }

        self.detach(new_sibling_id);

        // Read existing's prev AFTER detaching new. If new was existing's
        // immediate previous sibling, detach moved that pointer; using the
        // pre-detach value would splice new.next_sibling = new (a self-cycle)
        // and hang every later sibling walk. This is what hung ebay.com.
        let prev_id = {
            let inner = self.inner.borrow();
            inner.nodes.get(existing_id.index())
                .and_then(|n| n.as_ref())
                .and_then(|n| n.prev_sibling)
        };

        let mut inner = self.inner.borrow_mut();

        if let Some(Some(node)) = inner.nodes.get_mut(new_sibling_id.index()) {
            node.parent = Some(parent_id);
            node.prev_sibling = prev_id;
            node.next_sibling = Some(existing_id);
        }

        if let Some(Some(node)) = inner.nodes.get_mut(existing_id.index()) {
            node.prev_sibling = Some(new_sibling_id);
        }

        if let Some(prev) = prev_id {
            if let Some(Some(node)) = inner.nodes.get_mut(prev.index()) {
                node.next_sibling = Some(new_sibling_id);
            }
        } else if let Some(Some(parent)) = inner.nodes.get_mut(parent_id.index()) {
            parent.first_child = Some(new_sibling_id);
        }
    }

    pub fn detach(&self, node_id: NodeId) {
        let mut inner = self.inner.borrow_mut();

        let (parent_id, prev_id, next_id) = match inner.nodes.get(node_id.index()).and_then(|n| n.as_ref()) {
            Some(node) => (node.parent, node.prev_sibling, node.next_sibling),
            None => return,
        };

        if let Some(prev) = prev_id {
            if let Some(Some(node)) = inner.nodes.get_mut(prev.index()) {
                node.next_sibling = next_id;
            }
        } else if let Some(parent_id) = parent_id {
            if let Some(Some(parent)) = inner.nodes.get_mut(parent_id.index()) {
                parent.first_child = next_id;
            }
        }

        if let Some(next) = next_id {
            if let Some(Some(node)) = inner.nodes.get_mut(next.index()) {
                node.prev_sibling = prev_id;
            }
        } else if let Some(parent_id) = parent_id {
            if let Some(Some(parent)) = inner.nodes.get_mut(parent_id.index()) {
                parent.last_child = prev_id;
            }
        }

        if let Some(Some(node)) = inner.nodes.get_mut(node_id.index()) {
            node.parent = None;
            node.prev_sibling = None;
            node.next_sibling = None;
        }
    }

    /// Detach a node from its parent AND remove it (and all descendants)
    /// from the id-index so that `getElementById` no longer returns them.
    /// Unlike `remove()`, this does NOT free the nodes — the JS side may
    /// still hold references to the wrappers.
    pub fn remove_child(&self, node_id: NodeId) {
        // Collect all id attribute values in the subtree. We snapshot them
        // before detaching so `get_attribute` can still see the tree.
        let ids_to_remove: Vec<String> = {
            let descendants = self.descendants(node_id);
            let inner = self.inner.borrow();
            let mut ids: Vec<String> = Vec::new();
            if let Some(Some(node)) = inner.nodes.get(node_id.index()) {
                if let Some(id_val) = node.get_attribute("id") {
                    ids.push(id_val.to_string());
                }
            }
            for desc_id in &descendants {
                if let Some(Some(node)) = inner.nodes.get(desc_id.index()) {
                    if let Some(id_val) = node.get_attribute("id") {
                        ids.push(id_val.to_string());
                    }
                }
            }
            ids
        };

        self.detach(node_id);

        let mut inner = self.inner.borrow_mut();
        for id_str in &ids_to_remove {
            inner.id_index.remove(id_str);
        }
    }

    pub fn remove(&self, node_id: NodeId) {
        self.detach(node_id);
        let descendants = self.descendants(node_id);
        let mut inner = self.inner.borrow_mut();

        let mut ids_to_remove = Vec::new();
        for &desc_id in &descendants {
            if let Some(Some(node)) = inner.nodes.get(desc_id.index()) {
                if let Some(id_val) = node.get_attribute("id") {
                    ids_to_remove.push(id_val.to_string());
                }
            }
        }
        if let Some(Some(node)) = inner.nodes.get(node_id.index()) {
            if let Some(id_val) = node.get_attribute("id") {
                ids_to_remove.push(id_val.to_string());
            }
        }

        for id_str in ids_to_remove {
            inner.id_index.remove(&id_str);
        }

        for desc_id in descendants {
            inner.nodes[desc_id.index()] = None;
            inner.free_list.push(desc_id.0);
        }
        inner.nodes[node_id.index()] = None;
        inner.free_list.push(node_id.0);
    }

    pub fn children(&self, node_id: NodeId) -> Vec<NodeId> {
        let inner = self.inner.borrow();
        let mut result = Vec::new();
        let mut current = inner.nodes.get(node_id.index())
            .and_then(|n| n.as_ref())
            .and_then(|n| n.first_child);
        while let Some(child_id) = current {
            result.push(child_id);
            current = inner.nodes.get(child_id.index())
                .and_then(|n| n.as_ref())
                .and_then(|n| n.next_sibling);
        }
        result
    }

    pub fn descendants(&self, node_id: NodeId) -> Vec<NodeId> {
        let inner = self.inner.borrow();
        let mut result = Vec::new();
        let mut stack = Vec::new();

        let mut first = inner.nodes.get(node_id.index())
            .and_then(|n| n.as_ref())
            .and_then(|n| n.first_child);
        let mut children_to_push = Vec::new();
        while let Some(child_id) = first {
            children_to_push.push(child_id);
            if children_to_push.len() > inner.nodes.len() {
                eprintln!("obscura: sibling-chain cap hit at node {} - cycle", node_id.index());
                break;
            }
            first = inner.nodes.get(child_id.index())
                .and_then(|n| n.as_ref())
                .and_then(|n| n.next_sibling);
        }
        for child_id in children_to_push.into_iter().rev() {
            stack.push(child_id);
        }

        while let Some(current) = stack.pop() {
            result.push(current);
            // Defense in depth: a well-formed subtree has at most nodes.len()
            // descendants. Exceeding that means the parent/child graph is cyclic
            // (which the append_child / insert_before guards prevent); stop rather
            // than grow the stack and result forever and wedge the engine. On a
            // valid tree this bound is never reached, so the hot path is unchanged.
            if result.len() > inner.nodes.len() {
                eprintln!(
                    "obscura: descendants() cap hit at node {} ({} nodes) - tree has a cycle",
                    node_id.index(),
                    inner.nodes.len()
                );
                break;
            }

            let mut child = inner.nodes.get(current.index())
                .and_then(|n| n.as_ref())
                .and_then(|n| n.first_child);
            let mut children_to_push = Vec::new();
            while let Some(child_id) = child {
                children_to_push.push(child_id);
                if children_to_push.len() > inner.nodes.len() {
                    eprintln!("obscura: sibling-chain cap hit at node {} - cycle", current.index());
                    break;
                }
                child = inner.nodes.get(child_id.index())
                    .and_then(|n| n.as_ref())
                    .and_then(|n| n.next_sibling);
            }
            for child_id in children_to_push.into_iter().rev() {
                stack.push(child_id);
            }
        }

        result
    }

    /// Returns the node after `current` in document order, without leaving the
    /// subtree rooted at `root`.
    ///
    /// Keeping the ancestor climb inside the DOM avoids one JS/native crossing
    /// per ancestor when a TreeWalker reaches a deep leaf.
    pub fn next_in_subtree(&self, root: NodeId, current: NodeId) -> Option<NodeId> {
        let inner = self.inner.borrow();
        let current_node = inner.nodes.get(current.index())?.as_ref()?;
        if let Some(child) = current_node.first_child {
            return Some(child);
        }
        Self::climb_to_next_sibling(&inner, root, current)
    }

    /// Returns the node after the whole subtree rooted at `current`, in document
    /// order, without leaving the subtree rooted at `root`.
    ///
    /// This is `next_in_subtree` minus the descend-into-children step, which is
    /// what `NodeFilter.FILTER_REJECT` needs: it rejects a node *and* its
    /// descendants, unlike `FILTER_SKIP`, which only skips the node itself and
    /// is served by `next_in_subtree`.
    pub fn next_after_subtree(&self, root: NodeId, current: NodeId) -> Option<NodeId> {
        let inner = self.inner.borrow();
        Self::climb_to_next_sibling(&inner, root, current)
    }

    /// Returns the node before `current` in document order, without leaving the
    /// subtree rooted at `root`. `root` has no predecessor within its own
    /// subtree, but it is itself reachable as one — a NodeIterator can return
    /// its root, unlike a TreeWalker.
    ///
    /// A NodeIterator applies no subtree pruning (DOM 6.2: FILTER_REJECT
    /// behaves as FILTER_SKIP), so unlike the TreeWalker's backward walk the
    /// whole step fits here instead of being interleaved with filter calls.
    pub fn prev_in_subtree(&self, root: NodeId, current: NodeId) -> Option<NodeId> {
        let inner = self.inner.borrow();
        if current == root {
            return None;
        }
        let current_node = inner.nodes.get(current.index())?.as_ref()?;

        let Some(prev) = current_node.prev_sibling else {
            // No previous sibling: the parent immediately precedes `current`.
            return current_node.parent;
        };

        // Otherwise it is the previous sibling's deepest last descendant.
        let mut node_id = prev;
        for _ in 0..=inner.nodes.len() {
            let node = inner.nodes.get(node_id.index())?.as_ref()?;
            match node.last_child {
                Some(child) => node_id = child,
                None => return Some(node_id),
            }
        }

        // Same defense in depth as the forward walk: a malformed tree must not
        // spin here.
        None
    }

    /// Follow `current`'s next sibling, climbing ancestors until one has a next
    /// sibling — without stepping outside `root`.
    fn climb_to_next_sibling(
        inner: &DomTreeInner,
        root: NodeId,
        current: NodeId,
    ) -> Option<NodeId> {
        let mut node_id = current;
        for _ in 0..=inner.nodes.len() {
            if node_id == root {
                return None;
            }
            let node = inner.nodes.get(node_id.index())?.as_ref()?;
            if let Some(sibling) = node.next_sibling {
                return Some(sibling);
            }
            node_id = node.parent?;
        }

        // Parent cycles are prevented by the mutation APIs. Keep a hard bound
        // here as defense in depth for a malformed tree.
        None
    }

    /// The node holding a `<template>` element's contents.
    ///
    /// The parser puts template children in a separate contents document rather
    /// than under the element (HTML spec), so this is the only way to reach
    /// them. Templates built with `createElement` have no contents node yet, so
    /// one is allocated on demand — `.content` must be usable either way.
    ///
    /// Returns `None` for a non-element node.
    pub fn template_contents(&self, node_id: NodeId) -> Option<NodeId> {
        {
            let inner = self.inner.borrow();
            let node = inner.nodes.get(node_id.index())?.as_ref()?;
            match &node.data {
                NodeData::Element { template_contents, .. } => {
                    if let Some(existing) = *template_contents {
                        return Some(existing);
                    }
                }
                _ => return None,
            }
        }

        // Borrow released above: `new_node` takes its own mutable borrow.
        // Matches what the tree sink allocates for a parsed template.
        let contents = self.new_node(NodeData::Document);
        let mut inner = self.inner.borrow_mut();
        if let Some(Some(node)) = inner.nodes.get_mut(node_id.index()) {
            if let NodeData::Element { template_contents, .. } = &mut node.data {
                *template_contents = Some(contents);
                return Some(contents);
            }
        }
        None
    }

    pub fn ancestors(&self, node_id: NodeId) -> Vec<NodeId> {
        let inner = self.inner.borrow();
        let mut result = Vec::new();
        let mut current = inner.nodes.get(node_id.index())
            .and_then(|n| n.as_ref())
            .and_then(|n| n.parent);
        while let Some(parent_id) = current {
            result.push(parent_id);
            current = inner.nodes.get(parent_id.index())
                .and_then(|n| n.as_ref())
                .and_then(|n| n.parent);
        }
        result
    }

    pub fn get_element_by_id(&self, id: &str) -> Option<NodeId> {
        self.inner.borrow().id_index.get(id).copied()
    }

    pub fn text_content(&self, node_id: NodeId) -> String {
        let inner = self.inner.borrow();
        // Per DOM spec, calling textContent ON a CharacterData node
        // (Text, Comment, ProcessingInstruction) returns its .data.
        // Calling textContent on an Element walks descendants and
        // concatenates Text node content only (Comment + PI are
        // skipped). Handle the direct-CharacterData case here so the
        // descent helper can keep its element-centric behavior.
        if let Some(Some(node)) = inner.nodes.get(node_id.index()) {
            match &node.data {
                NodeData::Text { contents } => return contents.clone(),
                NodeData::Comment { contents } => return contents.clone(),
                NodeData::ProcessingInstruction { data, .. } => return data.clone(),
                _ => {}
            }
        }
        let mut result = String::new();
        collect_text_inner(&inner, node_id, &mut result);
        result
    }

    pub fn append_text(&self, parent_id: NodeId, text: &str) {
        let last_child_is_text = {
            let inner = self.inner.borrow();
            inner.nodes.get(parent_id.index())
                .and_then(|n| n.as_ref())
                .and_then(|n| n.last_child)
                .and_then(|lc| inner.nodes.get(lc.index()))
                .and_then(|n| n.as_ref())
                .map(|n| n.is_text())
                .unwrap_or(false)
        };

        if last_child_is_text {
            // Re-read last_child without unwrap: if it vanished between the two
            // borrows, fall through to appending a fresh text node rather than
            // panicking (a panic here aborts the whole engine via V8_Fatal).
            let last_child_id = {
                let inner = self.inner.borrow();
                inner.nodes.get(parent_id.index())
                    .and_then(|n| n.as_ref())
                    .and_then(|n| n.last_child)
            };
            if let Some(last_child_id) = last_child_id {
                let mut inner = self.inner.borrow_mut();
                if let Some(Some(node)) = inner.nodes.get_mut(last_child_id.index()) {
                    if let NodeData::Text { contents } = &mut node.data {
                        contents.push_str(text);
                        return;
                    }
                }
            }
        }

        let text_id = self.new_node(NodeData::Text {
            contents: text.to_string(),
        });
        self.append_child(parent_id, text_id);
    }

    pub fn find_body_or_root(&self) -> NodeId {
        let doc = self.document();
        for child in self.children(doc) {
            if let Some(n) = self.get_node(child) {
                if n.as_element().map(|name| name.local.as_ref() == "html").unwrap_or(false) {
                    for html_child in self.children(child) {
                        if let Some(hc) = self.get_node(html_child) {
                            if hc.as_element().map(|name| name.local.as_ref() == "body").unwrap_or(false) {
                                return html_child;
                            }
                        }
                    }
                    return child;
                }
            }
        }
        doc
    }

    pub fn import_children_from(&self, parent_id: NodeId, source: &DomTree, source_node: NodeId) {
        let source_children = source.children(source_node);
        for source_child_id in source_children {
            self.import_node_from(parent_id, source, source_child_id);
        }
    }

    fn import_node_from(&self, parent_id: NodeId, source: &DomTree, source_node_id: NodeId) {
        // Iterative DFS with an explicit (dest_parent, source_node) stack so a
        // deeply nested source tree cannot overflow the thread stack and abort
        // the process. Children are pushed in reverse so they are appended in
        // document order (append_child always appends to the end, so each level
        // keeps the source ordering).
        let mut stack = vec![(parent_id, source_node_id)];
        while let Some((dest_parent, src_id)) = stack.pop() {
            let node_data = {
                let source_inner = source.inner.borrow();
                match source_inner.nodes.get(src_id.index()) {
                    Some(Some(node)) => node.data.clone(),
                    _ => continue,
                }
            };

            let new_id = self.new_node(node_data);
            self.append_child(dest_parent, new_id);

            // A <template>'s children hang off a separate contents document, so
            // the child walk below never reaches them. Worse, the cloned data
            // carries the *source* tree's contents NodeId, which here indexes
            // whatever unrelated node occupies that slot. Allocate a real
            // contents node and queue the source contents' children into it, so
            // the reference is remapped rather than left dangling (issue #463).
            let src_contents = {
                let inner = self.inner.borrow();
                match inner.nodes.get(new_id.index()).and_then(|n| n.as_ref()) {
                    Some(node) => match &node.data {
                        NodeData::Element { template_contents, .. } => *template_contents,
                        _ => None,
                    },
                    None => None,
                }
            };
            if let Some(src_contents) = src_contents {
                let dest_contents = self.new_node(NodeData::Document);
                {
                    let mut inner = self.inner.borrow_mut();
                    if let Some(Some(node)) = inner.nodes.get_mut(new_id.index()) {
                        if let NodeData::Element { template_contents, .. } = &mut node.data {
                            *template_contents = Some(dest_contents);
                        }
                    }
                }
                // Onto the same stack, so nested templates stay iterative.
                for child_id in source.children(src_contents).into_iter().rev() {
                    stack.push((dest_contents, child_id));
                }
            }

            for child_id in source.children(src_id).into_iter().rev() {
                stack.push((new_id, child_id));
            }
        }
    }

    pub fn len(&self) -> usize {
        self.inner.borrow().nodes.iter().filter(|n| n.is_some()).count()
    }

    // Number of node slots (live plus freed), i.e. the same upper bound
    // descendants() uses to cap a tree walk. A well-formed subtree has at most
    // this many nodes, so it is a safe ceiling for iterative walkers that need a
    // cycle backstop.
    pub(crate) fn node_slot_count(&self) -> usize {
        self.inner.borrow().nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() <= 1
    }

    pub fn update_id_index(&self, node_id: NodeId, old_id: Option<&str>, new_id: Option<&str>) {
        let mut inner = self.inner.borrow_mut();
        if let Some(old) = old_id {
            inner.id_index.remove(old);
        }
        if let Some(new) = new_id {
            inner.id_index.insert(new.to_string(), node_id);
        }
    }
}

fn collect_text_inner(inner: &DomTreeInner, node_id: NodeId, buf: &mut String) {
    // Iterative pre-order walk on an explicit heap stack. The recursive form
    // overflowed the thread stack and aborted the process on deeply nested
    // trees; descendants() is iterative + capped for the same reason. A valid
    // subtree visits at most nodes.len() nodes, so exceeding that means the
    // graph is cyclic (prevented by the append_child / insert_before guards);
    // stop rather than spin forever.
    let max_steps = inner.nodes.len().saturating_add(16);
    let mut steps = 0usize;
    let mut stack = vec![node_id];

    while let Some(id) = stack.pop() {
        steps += 1;
        if steps > max_steps {
            eprintln!("obscura: collect_text_inner cap hit - tree has a cycle");
            break;
        }

        let node = match inner.nodes.get(id.index()) {
            Some(Some(n)) => n,
            _ => continue,
        };

        match &node.data {
            NodeData::Text { contents } => buf.push_str(contents),
            // Comment and ProcessingInstruction are intentionally NOT
            // appended when traversing descendants: per spec, textContent
            // on an Element only includes Text descendants. Direct
            // textContent on a Comment/PI is handled by the caller.
            _ => {
                // Collect children, then push them in reverse so they pop in
                // document order.
                let mut kids = Vec::new();
                let mut child = node.first_child;
                while let Some(child_id) = child {
                    kids.push(child_id);
                    if kids.len() > inner.nodes.len() {
                        eprintln!("obscura: collect_text_inner sibling cap hit - cycle");
                        break;
                    }
                    child = inner.nodes.get(child_id.index())
                        .and_then(|n| n.as_ref())
                        .and_then(|n| n.next_sibling);
                }
                for child_id in kids.into_iter().rev() {
                    stack.push(child_id);
                }
            }
        }
    }
}

impl Default for DomTree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_tree_has_document() {
        let tree = DomTree::new();
        assert_eq!(tree.len(), 1);
        let node = tree.get_node(tree.document()).unwrap();
        assert!(node.is_document());
    }

    #[test]
    fn test_append_child() {
        let tree = DomTree::new();
        let child = tree.new_node(NodeData::Text {
            contents: "hello".into(),
        });
        let doc = tree.document();
        tree.append_child(doc, child);

        assert_eq!(tree.len(), 2);
        let doc_node = tree.get_node(doc).unwrap();
        assert_eq!(doc_node.first_child, Some(child));
        assert_eq!(doc_node.last_child, Some(child));

        let child_node = tree.get_node(child).unwrap();
        assert_eq!(child_node.parent, Some(doc));
    }

    #[test]
    fn test_multiple_children() {
        let tree = DomTree::new();
        let doc = tree.document();
        let c1 = tree.new_node(NodeData::Text { contents: "a".into() });
        let c2 = tree.new_node(NodeData::Text { contents: "b".into() });
        let c3 = tree.new_node(NodeData::Text { contents: "c".into() });
        tree.append_child(doc, c1);
        tree.append_child(doc, c2);
        tree.append_child(doc, c3);

        assert_eq!(tree.children(doc), vec![c1, c2, c3]);
    }

    #[test]
    fn test_detach() {
        let tree = DomTree::new();
        let doc = tree.document();
        let c1 = tree.new_node(NodeData::Text { contents: "a".into() });
        let c2 = tree.new_node(NodeData::Text { contents: "b".into() });
        tree.append_child(doc, c1);
        tree.append_child(doc, c2);

        tree.detach(c1);
        assert_eq!(tree.children(doc), vec![c2]);
    }

    #[test]
    fn test_insert_before() {
        let tree = DomTree::new();
        let doc = tree.document();
        let c1 = tree.new_node(NodeData::Text { contents: "a".into() });
        let c2 = tree.new_node(NodeData::Text { contents: "b".into() });
        let c3 = tree.new_node(NodeData::Text { contents: "c".into() });
        tree.append_child(doc, c1);
        tree.append_child(doc, c3);
        tree.insert_before(c3, c2);

        assert_eq!(tree.children(doc), vec![c1, c2, c3]);
    }

    #[test]
    fn test_text_content() {
        let tree = DomTree::new();
        let doc = tree.document();
        let div = tree.new_node(NodeData::Element {
            name: QualName::new(None, ns!(html), local_name!("div")),
            attrs: vec![],
            template_contents: None,
            mathml_annotation_xml_integration_point: false,
        });
        tree.append_child(doc, div);

        let t1 = tree.new_node(NodeData::Text { contents: "Hello ".into() });
        let t2 = tree.new_node(NodeData::Text { contents: "World".into() });
        tree.append_child(div, t1);
        tree.append_child(div, t2);

        assert_eq!(tree.text_content(div), "Hello World");
    }

    #[test]
    fn test_get_element_by_id() {
        let tree = DomTree::new();
        let doc = tree.document();
        let div = tree.new_node(NodeData::Element {
            name: QualName::new(None, ns!(html), local_name!("div")),
            attrs: vec![Attribute {
                name: QualName::new(None, Namespace::default(), LocalName::from("id")),
                value: "main".into(),
            }],
            template_contents: None,
            mathml_annotation_xml_integration_point: false,
        });
        tree.append_child(doc, div);

        assert_eq!(tree.get_element_by_id("main"), Some(div));
        assert_eq!(tree.get_element_by_id("nonexistent"), None);
    }

    #[test]
    fn test_reparent_cycle_is_rejected() {
        // document -> html -> body -> div. Moving an ancestor under one of its
        // own descendants would make the parent/child graph cyclic and hang
        // every later descendants() walk. Both append_child and insert_before
        // must reject it as a no-op (DOM HierarchyRequestError).
        let tree = DomTree::new();
        let doc = tree.document();
        let mk = |n: &str| {
            tree.new_node(NodeData::Element {
                name: QualName::new(None, ns!(html), LocalName::from(n)),
                attrs: vec![],
                template_contents: None,
                mathml_annotation_xml_integration_point: false,
            })
        };
        let html = mk("html");
        let body = mk("body");
        let div = mk("div");
        tree.append_child(doc, html);
        tree.append_child(html, body);
        tree.append_child(body, div);

        let before = tree.descendants(doc).len();
        assert_eq!(before, 3);

        // append_child: html is an ancestor of div -> must be a no-op, no cycle.
        tree.append_child(div, html);
        assert_eq!(tree.descendants(doc).len(), before, "cyclic append must be a no-op");
        assert_eq!(tree.descendants(div).len(), 0, "div must stay a leaf");

        // insert_before: html is an ancestor of body (div's parent) -> no-op.
        tree.insert_before(div, html);
        assert_eq!(tree.descendants(doc).len(), before, "cyclic insert_before must be a no-op");

        // self-append / self-insert remain no-ops (existing guards).
        tree.append_child(div, div);
        tree.insert_before(div, div);
        assert_eq!(tree.descendants(doc).len(), before);
    }

    #[test]
    fn test_insert_before_previous_sibling_no_cycle() {
        // Inserting a node before its own immediate previous sibling is a no-op
        // reorder that frameworks do constantly. It used to splice
        // next_sibling = self via a prev_id captured before detach, hanging every
        // later sibling walk (this hung ebay.com). The result must stay a
        // well-formed [a, b] with no cycle.
        let tree = DomTree::new();
        let doc = tree.document();
        let mk = |n: &str| {
            tree.new_node(NodeData::Element {
                name: QualName::new(None, ns!(html), LocalName::from(n)),
                attrs: vec![],
                template_contents: None,
                mathml_annotation_xml_integration_point: false,
            })
        };
        let parent = mk("div");
        let a = mk("a");
        let b = mk("b");
        tree.append_child(doc, parent);
        tree.append_child(parent, a);
        tree.append_child(parent, b); // parent -> [a, b]

        // a is already b's previous sibling; this reorder must not create a cycle.
        tree.insert_before(b, a);

        let kids = tree.descendants(parent);
        assert_eq!(kids, vec![a, b], "order preserved, no cycle");
    }

    #[test]
    fn test_append_text_merges() {
        let tree = DomTree::new();
        let doc = tree.document();
        tree.append_text(doc, "Hello ");
        tree.append_text(doc, "World");

        assert_eq!(tree.children(doc).len(), 1);
        assert_eq!(tree.text_content(doc), "Hello World");
    }

    #[test]
    fn test_remove_subtree() {
        let tree = DomTree::new();
        let doc = tree.document();
        let div = tree.new_node(NodeData::Element {
            name: QualName::new(None, ns!(html), local_name!("div")),
            attrs: vec![],
            template_contents: None,
            mathml_annotation_xml_integration_point: false,
        });
        tree.append_child(doc, div);
        let text = tree.new_node(NodeData::Text { contents: "hi".into() });
        tree.append_child(div, text);

        assert_eq!(tree.len(), 3);
        tree.remove(div);
        assert_eq!(tree.len(), 1);
    }

    #[test]
    fn test_next_in_subtree_follows_document_order_and_stays_within_root() {
        let tree = DomTree::new();
        let root = tree.new_node(NodeData::Text { contents: "root".into() });
        let first = tree.new_node(NodeData::Text { contents: "first".into() });
        let nested = tree.new_node(NodeData::Text { contents: "nested".into() });
        let second = tree.new_node(NodeData::Text { contents: "second".into() });
        tree.append_child(tree.document(), root);
        tree.append_child(root, first);
        tree.append_child(first, nested);
        tree.append_child(root, second);

        assert_eq!(tree.next_in_subtree(root, root), Some(first));
        assert_eq!(tree.next_in_subtree(root, first), Some(nested));
        assert_eq!(tree.next_in_subtree(root, nested), Some(second));
        assert_eq!(tree.next_in_subtree(root, second), None);
    }

    #[test]
    fn test_next_after_subtree_skips_descendants() {
        // Same shape as above: root > [first > nested, second]. Stepping past
        // `first` must land on `second`, not descend into `nested` — that is
        // what NodeFilter.FILTER_REJECT needs.
        let tree = DomTree::new();
        let root = tree.new_node(NodeData::Text { contents: "root".into() });
        let first = tree.new_node(NodeData::Text { contents: "first".into() });
        let nested = tree.new_node(NodeData::Text { contents: "nested".into() });
        let second = tree.new_node(NodeData::Text { contents: "second".into() });
        tree.append_child(tree.document(), root);
        tree.append_child(root, first);
        tree.append_child(first, nested);
        tree.append_child(root, second);

        assert_eq!(tree.next_after_subtree(root, first), Some(second));
        // A leaf behaves identically to next_in_subtree: there is no subtree.
        assert_eq!(tree.next_after_subtree(root, nested), Some(second));
        assert_eq!(tree.next_after_subtree(root, second), None);
        // Rejecting the root itself exhausts the walk rather than escaping it.
        assert_eq!(tree.next_after_subtree(root, root), None);
    }

    #[test]
    fn test_prev_in_subtree_reverses_document_order() {
        // root > [first > nested, second]; document order is root, first,
        // nested, second, so the reverse walk must retrace it exactly.
        let tree = DomTree::new();
        let root = tree.new_node(NodeData::Text { contents: "root".into() });
        let first = tree.new_node(NodeData::Text { contents: "first".into() });
        let nested = tree.new_node(NodeData::Text { contents: "nested".into() });
        let second = tree.new_node(NodeData::Text { contents: "second".into() });
        tree.append_child(tree.document(), root);
        tree.append_child(root, first);
        tree.append_child(first, nested);
        tree.append_child(root, second);

        // Previous sibling's deepest last descendant, not the sibling itself.
        assert_eq!(tree.prev_in_subtree(root, second), Some(nested));
        assert_eq!(tree.prev_in_subtree(root, nested), Some(first));
        // No previous sibling: the parent precedes it, and root is returnable.
        assert_eq!(tree.prev_in_subtree(root, first), Some(root));
        // Root has no predecessor inside its own subtree.
        assert_eq!(tree.prev_in_subtree(root, root), None);
    }

    // Builds a chain of `depth` nested <div> elements under the document and
    // returns (outermost_div, innermost_div). Depth this large overflows a
    // recursive tree walk and aborts the process, so it guards the iterative
    // serialize / text_content / import paths against regressing to recursion.
    fn build_deep_chain(tree: &DomTree, depth: usize) -> (NodeId, NodeId) {
        let mk_div = || {
            tree.new_node(NodeData::Element {
                name: QualName::new(None, ns!(html), local_name!("div")),
                attrs: vec![],
                template_contents: None,
                mathml_annotation_xml_integration_point: false,
            })
        };
        let root = mk_div();
        tree.append_child(tree.document(), root);
        let mut cur = root;
        for _ in 1..depth {
            let next = mk_div();
            tree.append_child(cur, next);
            cur = next;
        }
        (root, cur)
    }

    #[test]
    fn test_outer_html_deeply_nested_does_not_overflow() {
        let tree = DomTree::new();
        let (root, leaf) = build_deep_chain(&tree, 100_000);
        let marker = tree.new_node(NodeData::Text {
            contents: "leaf".into(),
        });
        tree.append_child(leaf, marker);

        let html = tree.outer_html(root);
        assert!(html.starts_with("<div>"));
        assert!(html.contains("leaf"));
        assert!(html.ends_with("</div>"));
    }

    #[test]
    fn test_text_content_deeply_nested_does_not_overflow() {
        let tree = DomTree::new();
        let (root, leaf) = build_deep_chain(&tree, 100_000);
        let marker = tree.new_node(NodeData::Text {
            contents: "deep".into(),
        });
        tree.append_child(leaf, marker);

        assert_eq!(tree.text_content(root), "deep");
    }

    #[test]
    fn test_import_deeply_nested_does_not_overflow() {
        let source = DomTree::new();
        build_deep_chain(&source, 100_000);

        let dest = DomTree::new();
        let dest_doc = dest.document();
        dest.import_children_from(dest_doc, &source, source.document());

        assert!(dest.len() >= 100_000);
    }
}
