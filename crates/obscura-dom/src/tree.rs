use html5ever::{LocalName, Namespace, Prefix, QualName};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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

/// The encapsulation mode recorded by a native shadow root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShadowRootMode {
    Open,
    Closed,
}

/// Stable metadata for a shadow-root node in this tree.
///
/// A shadow root owns an ordinary child list, but is not an ordinary child of
/// its host. The separate host edge keeps `parentNode`-style walks scoped to
/// one tree while still allowing composed-tree operations to cross explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShadowRoot {
    pub id: NodeId,
    pub host: NodeId,
    pub mode: ShadowRootMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachShadowError {
    HostIsNotElement,
    HostAlreadyHasShadowRoot,
    InvalidShadowRoot,
}

impl fmt::Display for AttachShadowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::HostIsNotElement => "shadow host is not an element",
            Self::HostAlreadyHasShadowRoot => "shadow host already has a shadow root",
            Self::InvalidShadowRoot => "shadow root is not a detached fragment node",
        };
        f.write_str(message)
    }
}

impl std::error::Error for AttachShadowError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub name: QualName,
    pub value: String,
}

impl Attribute {
    pub fn qualified_name(&self) -> String {
        match &self.name.prefix {
            Some(prefix) => format!("{}:{}", prefix, self.name.local),
            None => self.name.local.to_string(),
        }
    }

    pub fn qualified_name_eq(&self, name: &str) -> bool {
        match &self.name.prefix {
            Some(prefix) => {
                name.len() == prefix.len() + self.name.local.len() + 1
                    && name.starts_with(prefix.as_ref())
                    && name.as_bytes().get(prefix.len()) == Some(&b':')
                    && &name[prefix.len() + 1..] == self.name.local.as_ref()
            }
            None => self.name.local.as_ref() == name,
        }
    }
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
    /// Shadow-including document connectivity, maintained incrementally on
    /// insertion/removal so hot DOM mutation paths do not walk every ancestor.
    pub connected: bool,
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
            if a.qualified_name_eq(name) {
                Some(a.value.as_str())
            } else {
                None
            }
        })
    }

    pub fn set_attribute(&mut self, name: &str, value: String) {
        if let NodeData::Element { attrs, .. } = &mut self.data {
            // Match by qualified name, consistent with get_attribute and the
            // remove_attribute op. A parsed namespaced attribute is stored with
            // a separate prefix (e.g. xlink:href -> prefix="xlink", local="href");
            // matching on local name alone would miss it and push a duplicate.
            if let Some(attr) = attrs.iter_mut().find(|a| a.qualified_name_eq(name)) {
                attr.value = value;
            } else {
                attrs.push(Attribute {
                    name: QualName::new(None, Namespace::default(), LocalName::from(name)),
                    value,
                });
            }
        }
    }

    // Read a namespaced attribute by (namespace, localName).
    pub fn get_attribute_ns(&self, ns: &str, local: &str) -> Option<&str> {
        self.attrs()?.iter().find_map(|a| {
            if a.name.ns.as_ref() == ns && a.name.local.as_ref() == local {
                Some(a.value.as_str())
            } else {
                None
            }
        })
    }

    // Set a namespaced attribute using a proper QualName: prefix and local name
    // remain separate while qualified-name APIs and serialization reconstruct
    // `prefix:local` when needed.
    pub fn set_attribute_ns(&mut self, ns: &str, qualified: &str, value: String) {
        if let NodeData::Element { attrs, .. } = &mut self.data {
            let (prefix, local) = match qualified.split_once(':') {
                Some((prefix, local)) => (Some(Prefix::from(prefix)), local),
                None => (None, qualified),
            };
            if let Some(attr) = attrs
                .iter_mut()
                .find(|a| a.name.ns.as_ref() == ns && a.name.local.as_ref() == local)
            {
                attr.name.prefix = prefix;
                attr.value = value;
            } else {
                attrs.push(Attribute {
                    name: QualName::new(prefix, Namespace::from(ns), LocalName::from(local)),
                    value,
                });
            }
        }
    }

    pub fn remove_attribute_ns(&mut self, ns: &str, local: &str) {
        if let NodeData::Element { attrs, .. } = &mut self.data {
            attrs.retain(|a| {
                !(a.name.ns.as_ref() == ns && a.name.local.as_ref() == local)
            });
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
    /// Shadow roots are arena nodes with their own child list. They are kept
    /// outside the ordinary parent links so light-tree traversal never crosses
    /// into a shadow tree by accident.
    shadow_roots: HashMap<NodeId, ShadowRoot>,
    shadow_roots_by_host: HashMap<NodeId, NodeId>,
    /// Full-document HTML parsing enables declarative shadow roots. Fragment
    /// parsing (including innerHTML) deliberately leaves this false.
    pub(crate) allow_declarative_shadow_roots: bool,
    // Whether the document was parsed in (full) quirks mode. In quirks mode CSS
    // class and id selectors match ASCII-case-insensitively.
    pub(crate) quirks: bool,
}

impl DomTree {
    pub fn new() -> Self {
        let doc_node = Node {
            id: NodeId(0),
            connected: true,
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
                shadow_roots: HashMap::new(),
                shadow_roots_by_host: HashMap::new(),
                allow_declarative_shadow_roots: false,
                quirks: false,
            }),
        }
    }

    pub fn document(&self) -> NodeId {
        self.inner.borrow().document
    }

    /// Record whether the document was parsed in (full) quirks mode.
    pub fn set_quirks(&self, quirks: bool) {
        self.inner.borrow_mut().quirks = quirks;
    }

    /// Whether the document is in (full) quirks mode, in which CSS class and id
    /// selectors match ASCII-case-insensitively.
    pub fn is_quirks(&self) -> bool {
        self.inner.borrow().quirks
    }

    pub(crate) fn set_allow_declarative_shadow_roots(&self, allow: bool) {
        self.inner.borrow_mut().allow_declarative_shadow_roots = allow;
    }

    pub(crate) fn allows_declarative_shadow_roots(&self) -> bool {
        self.inner.borrow().allow_declarative_shadow_roots
    }

    pub(crate) fn borrow_inner(&self) -> std::cell::Ref<'_, DomTreeInner> {
        self.inner.borrow()
    }


    /// Create and attach a native shadow-root node to `host`.
    pub fn attach_shadow_root(
        &self,
        host: NodeId,
        mode: ShadowRootMode,
    ) -> Result<NodeId, AttachShadowError> {
        {
            let inner = self.inner.borrow();
            let host_is_element = inner
                .nodes
                .get(host.index())
                .and_then(|node| node.as_ref())
                .is_some_and(Node::is_element);
            if !host_is_element {
                return Err(AttachShadowError::HostIsNotElement);
            }
            if inner.shadow_roots_by_host.contains_key(&host) {
                return Err(AttachShadowError::HostAlreadyHasShadowRoot);
            }
        }

        let root = self.new_node(NodeData::Document);
        // A freshly allocated document-fragment backing node satisfies every
        // invariant below. If this ever fails, remove it so the arena does not
        // retain an unreachable allocation.
        if let Err(error) = self.attach_shadow_root_node(host, root, mode) {
            self.remove(root);
            return Err(error);
        }
        Ok(root)
    }

    /// Attach an existing detached fragment node as a shadow root. html5ever's
    /// declarative-shadow hook supplies the template-contents fragment through
    /// this path, but the hook remains disabled until style/layout integration
    /// is ready.
    pub(crate) fn attach_shadow_root_node(
        &self,
        host: NodeId,
        root: NodeId,
        mode: ShadowRootMode,
    ) -> Result<(), AttachShadowError> {
        let mut inner = self.inner.borrow_mut();
        let host_is_element = inner
            .nodes
            .get(host.index())
            .and_then(|node| node.as_ref())
            .is_some_and(Node::is_element);
        if !host_is_element {
            return Err(AttachShadowError::HostIsNotElement);
        }
        if inner.shadow_roots_by_host.contains_key(&host) {
            return Err(AttachShadowError::HostAlreadyHasShadowRoot);
        }

        let valid_root = root != inner.document
            && !inner.shadow_roots.contains_key(&root)
            && inner
                .nodes
                .get(root.index())
                .and_then(|node| node.as_ref())
                .is_some_and(|node| {
                    matches!(node.data, NodeData::Document)
                        && node.parent.is_none()
                        && node.prev_sibling.is_none()
                        && node.next_sibling.is_none()
                });
        if !valid_root {
            return Err(AttachShadowError::InvalidShadowRoot);
        }

        let info = ShadowRoot {
            id: root,
            host,
            mode,
        };
        inner.shadow_roots.insert(root, info);
        inner.shadow_roots_by_host.insert(host, root);
        let connected = inner
            .nodes
            .get(host.index())
            .and_then(|node| node.as_ref())
            .is_some_and(|node| node.connected);
        Self::set_subtree_connected(&mut inner, root, connected);
        Ok(())
    }

    /// Return the native root hosted by `host`, including closed roots. Web API
    /// visibility is intentionally left to the caller.
    pub fn shadow_root(&self, host: NodeId) -> Option<NodeId> {
        self.inner.borrow().shadow_roots_by_host.get(&host).copied()
    }

    pub fn shadow_root_info(&self, root: NodeId) -> Option<ShadowRoot> {
        self.inner.borrow().shadow_roots.get(&root).copied()
    }

    pub fn is_shadow_root(&self, node: NodeId) -> bool {
        self.inner.borrow().shadow_roots.contains_key(&node)
    }

    /// Return the root of `node`'s local tree scope. This follows ordinary
    /// parent links only, so a shadow descendant resolves to its ShadowRoot and
    /// a light descendant resolves to its document or detached subtree root.
    pub fn tree_scope_root(&self, node: NodeId) -> Option<NodeId> {
        let inner = self.inner.borrow();
        let mut current = node;
        for _ in 0..=inner.nodes.len() {
            let current_node = inner.nodes.get(current.index())?.as_ref()?;
            match current_node.parent {
                Some(parent) => current = parent,
                None => return Some(current),
            }
        }
        None
    }

    pub fn containing_shadow_root(&self, node: NodeId) -> Option<NodeId> {
        let root = self.tree_scope_root(node)?;
        self.is_shadow_root(root).then_some(root)
    }

    /// Return the topmost root after crossing ShadowRoot-to-host edges. This is
    /// the native counterpart of `getRootNode({ composed: true })`.
    pub fn shadow_including_root(&self, node: NodeId) -> Option<NodeId> {
        let inner = self.inner.borrow();
        let mut current = node;
        for _ in 0..=inner.nodes.len() {
            let current_node = inner.nodes.get(current.index())?.as_ref()?;
            if let Some(parent) = current_node.parent {
                current = parent;
            } else if let Some(root) = inner.shadow_roots.get(&current) {
                current = root.host;
            } else {
                return Some(current);
            }
        }
        None
    }

    /// Constant-time shadow-including connectivity. The bit is propagated over
    /// ordinary children and hosted shadow roots whenever a subtree moves.
    pub fn is_connected(&self, node: NodeId) -> bool {
        self.inner
            .borrow()
            .nodes
            .get(node.index())
            .and_then(|entry| entry.as_ref())
            .is_some_and(|node| node.connected)
    }

    fn set_subtree_connected(inner: &mut DomTreeInner, root: NodeId, connected: bool) {
        // Fresh parser/framework insertions are overwhelmingly leaves. Avoid
        // allocating traversal state for the one-node case.
        let is_leaf = inner
            .nodes
            .get(root.index())
            .and_then(|entry| entry.as_ref())
            .is_some_and(|node| node.first_child.is_none())
            && !inner.shadow_roots_by_host.contains_key(&root);
        if is_leaf {
            if let Some(Some(node)) = inner.nodes.get_mut(root.index()) {
                node.connected = connected;
            }
            return;
        }

        let mut stack = vec![root];
        let mut seen = HashSet::new();
        while let Some(node_id) = stack.pop() {
            if !seen.insert(node_id) {
                continue;
            }
            let (mut child, shadow_root) = match inner
                .nodes
                .get_mut(node_id.index())
                .and_then(|entry| entry.as_mut())
            {
                Some(node) => {
                    node.connected = connected;
                    (node.first_child, inner.shadow_roots_by_host.get(&node_id).copied())
                }
                None => continue,
            };
            if let Some(root) = shadow_root {
                stack.push(root);
            }
            // Valid trees terminate naturally. The bound is defense in depth
            // against a corrupt sibling cycle, which must not spin here.
            for _ in 0..=inner.nodes.len() {
                let Some(child_id) = child else { break };
                stack.push(child_id);
                child = inner
                    .nodes
                    .get(child_id.index())
                    .and_then(|entry| entry.as_ref())
                    .and_then(|node| node.next_sibling);
            }
        }
    }

    fn host_including_parent(inner: &DomTreeInner, node: NodeId) -> Option<NodeId> {
        inner
            .nodes
            .get(node.index())
            .and_then(|entry| entry.as_ref())
            .and_then(|entry| entry.parent)
            .or_else(|| inner.shadow_roots.get(&node).map(|root| root.host))
    }

    /// DOM insertion rejects a node when it is a host-including inclusive
    /// ancestor of the destination parent. Ordinary parent links are not enough
    /// for this check because a ShadowRoot's parent is intentionally null.
    fn would_create_host_including_cycle(
        inner: &DomTreeInner,
        parent: NodeId,
        child: NodeId,
    ) -> bool {
        let child_can_be_ancestor = inner
            .nodes
            .get(child.index())
            .and_then(|entry| entry.as_ref())
            .is_some_and(|entry| entry.first_child.is_some())
            || inner.shadow_roots_by_host.contains_key(&child);
        if !child_can_be_ancestor {
            return false;
        }

        let mut current = Some(parent);
        for _ in 0..=inner.nodes.len() {
            let node = match current {
                Some(node) => node,
                None => return false,
            };
            if node == child {
                return true;
            }
            current = Self::host_including_parent(inner, node);
        }

        // A valid host-including chain cannot be longer than the arena. Refuse
        // mutation if pre-existing corruption ever violates that invariant.
        true
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
            connected: false,
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
        // A ShadowRoot is never itself an ordinary child, and moving a host
        // below its own root would create a cycle even though the root's parent
        // pointer is null. Follow both ordinary parents and root-to-host edges.
        let (parent_connected, child_connected) = {
            let inner = self.inner.borrow();
            let parent_exists = inner
                .nodes
                .get(parent_id.index())
                .is_some_and(Option::is_some);
            let child_exists = inner
                .nodes
                .get(child_id.index())
                .is_some_and(Option::is_some);
            // A leaf which is not a shadow host cannot be an inclusive
            // ancestor of the destination parent. Detached framework tree
            // construction appends thousands of freshly-created leaves; doing
            // a complete parent walk for each one makes a deep chain O(n²).
            // Non-leaves and shadow hosts retain the full host-including cycle
            // check, where reparenting really can create a cycle.
            let child_can_be_ancestor = inner
                .nodes
                .get(child_id.index())
                .and_then(|entry| entry.as_ref())
                .is_some_and(|child| child.first_child.is_some())
                || inner.shadow_roots_by_host.contains_key(&child_id);
            if !parent_exists
                || !child_exists
                || inner.shadow_roots.contains_key(&child_id)
                || (child_can_be_ancestor
                    && Self::would_create_host_including_cycle(&inner, parent_id, child_id))
            {
                return;
            }
            let parent_connected = inner
                .nodes
                .get(parent_id.index())
                .and_then(|entry| entry.as_ref())
                .is_some_and(|parent| parent.connected);
            let child_connected = inner
                .nodes
                .get(child_id.index())
                .and_then(|entry| entry.as_ref())
                .is_some_and(|child| child.connected);
            (parent_connected, child_connected)
        };
        self.detach_for_reparent(child_id, child_connected && !parent_connected);

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
        if parent_connected && !child_connected {
            Self::set_subtree_connected(&mut inner, child_id, true);
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
        let (parent_id, parent_connected) = {
            let inner = self.inner.borrow();
            match inner.nodes.get(existing_id.index()).and_then(|n| n.as_ref()).and_then(|n| n.parent) {
                Some(parent) => {
                    let connected = inner
                        .nodes
                        .get(parent.index())
                        .and_then(|entry| entry.as_ref())
                        .is_some_and(|node| node.connected);
                    (parent, connected)
                }
                None => return,
            }
        };

        // Apply the same host-including cycle and root-node constraints as
        // append_child. A leaf host still needs this check because its hosted
        // root is not present in the ordinary child list.
        {
            let inner = self.inner.borrow();
            let new_exists = inner
                .nodes
                .get(new_sibling_id.index())
                .is_some_and(Option::is_some);
            if !new_exists
                || inner.shadow_roots.contains_key(&new_sibling_id)
                || Self::would_create_host_including_cycle(&inner, parent_id, new_sibling_id)
            {
                return;
            }
        }

        let child_connected = self.is_connected(new_sibling_id);
        self.detach_for_reparent(
            new_sibling_id,
            child_connected && !parent_connected,
        );

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
        if parent_connected && !child_connected {
            Self::set_subtree_connected(&mut inner, new_sibling_id, true);
        }
    }

    pub fn detach(&self, node_id: NodeId) {
        self.detach_for_reparent(node_id, true);
    }

    fn detach_for_reparent(&self, node_id: NodeId, disconnect: bool) {
        let mut inner = self.inner.borrow_mut();

        // The document and registered ShadowRoots have no ordinary parent and
        // cannot be detached through light-tree mutation APIs.
        if node_id == inner.document || inner.shadow_roots.contains_key(&node_id) {
            return;
        }

        let (parent_id, prev_id, next_id) = match inner.nodes.get(node_id.index()).and_then(|n| n.as_ref()) {
            Some(node) => (node.parent, node.prev_sibling, node.next_sibling),
            None => return,
        };
        if disconnect && inner
            .nodes
            .get(node_id.index())
            .and_then(|entry| entry.as_ref())
            .is_some_and(|node| node.connected)
        {
            Self::set_subtree_connected(&mut inner, node_id, false);
        }

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
        let nodes_to_remove = self.inclusive_owned_subtrees(node_id);
        if nodes_to_remove.is_empty() {
            return;
        }
        self.detach(node_id);
        let mut inner = self.inner.borrow_mut();

        let mut ids_to_remove = Vec::new();
        for &id in &nodes_to_remove {
            if let Some(Some(node)) = inner.nodes.get(id.index()) {
                if let Some(id_val) = node.get_attribute("id") {
                    ids_to_remove.push(id_val.to_string());
                }
            }
        }

        for id_str in ids_to_remove {
            inner.id_index.remove(&id_str);
        }

        // Remove both directions before freeing any arena slot. Otherwise a
        // reused NodeId could inherit an old host/root relationship.
        for &id in &nodes_to_remove {
            if let Some(root) = inner.shadow_roots.remove(&id) {
                if inner.shadow_roots_by_host.get(&root.host) == Some(&id) {
                    inner.shadow_roots_by_host.remove(&root.host);
                }
            }
            if let Some(root_id) = inner.shadow_roots_by_host.remove(&id) {
                inner.shadow_roots.remove(&root_id);
            }
        }

        // Only free slots that are currently live. Freeing an out-of-range id
        // would panic on direct indexing, and freeing an already-freed slot
        // would push it onto the free list a second time — later handing the
        // same NodeId to two live nodes (aliasing).
        for id in nodes_to_remove {
            if matches!(inner.nodes.get(id.index()), Some(Some(_))) {
                inner.nodes[id.index()] = None;
                inner.free_list.push(id.0);
            }
        }
    }

    /// Collect an ordinary subtree plus every shadow tree owned by a host in
    /// that subtree. This is used only by the arena-freeing path; normal DOM
    /// traversal must remain tree-scoped and therefore never follows host edges.
    fn inclusive_owned_subtrees(&self, node_id: NodeId) -> Vec<NodeId> {
        let inner = self.inner.borrow();
        if !inner
            .nodes
            .get(node_id.index())
            .is_some_and(Option::is_some)
        {
            return Vec::new();
        }

        let mut result = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![node_id];
        while let Some(current) = stack.pop() {
            if !seen.insert(current) {
                continue;
            }
            let Some(node) = inner
                .nodes
                .get(current.index())
                .and_then(|entry| entry.as_ref())
            else {
                continue;
            };
            result.push(current);

            if let Some(root) = inner.shadow_roots_by_host.get(&current) {
                stack.push(*root);
            }

            let mut children = Vec::new();
            let mut child = node.first_child;
            while let Some(child_id) = child {
                children.push(child_id);
                if children.len() > inner.nodes.len() {
                    break;
                }
                child = inner
                    .nodes
                    .get(child_id.index())
                    .and_then(|entry| entry.as_ref())
                    .and_then(|entry| entry.next_sibling);
            }
            stack.extend(children.into_iter().rev());
        }
        result
    }

    pub fn children(&self, node_id: NodeId) -> Vec<NodeId> {
        let inner = self.inner.borrow();
        let mut result = Vec::new();
        let mut current = inner.nodes.get(node_id.index())
            .and_then(|n| n.as_ref())
            .and_then(|n| n.first_child);
        while let Some(child_id) = current {
            result.push(child_id);
            // Defense in depth: a valid sibling chain is at most nodes.len()
            // long. Exceeding that means next_sibling forms a cycle (which the
            // append_child / insert_before guards prevent); stop rather than
            // loop forever. On a valid tree this bound is never reached.
            if result.len() > inner.nodes.len() {
                break;
            }
            current = inner.nodes.get(child_id.index())
                .and_then(|n| n.as_ref())
                .and_then(|n| n.next_sibling);
        }
        result
    }

    /// Snapshot the direct children of `host`'s shadow root. Ordinary
    /// `children(host)` continues to return only light children.
    pub fn shadow_children(&self, host: NodeId) -> Option<Vec<NodeId>> {
        let root = self.shadow_root(host)?;
        Some(self.children(root))
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

    /// Snapshot the descendants of `host`'s shadow tree without including the
    /// ShadowRoot node itself.
    pub fn shadow_descendants(&self, host: NodeId) -> Option<Vec<NodeId>> {
        let root = self.shadow_root(host)?;
        Some(self.descendants(root))
    }

    /// Whether `node` is an HTML `<slot>` element. Slot assignment is defined
    /// only for HTML slots; same-local-name elements in other namespaces do
    /// not participate in the flattened tree.
    pub fn is_html_slot_element(&self, node: NodeId) -> bool {
        self.get_node(node).is_some_and(|node| {
            node.as_element().is_some_and(|name| {
                name.ns.as_ref() == "http://www.w3.org/1999/xhtml"
                    && name.local.as_ref() == "slot"
            })
        })
    }

    /// Return the first slot to which `node` is assigned.
    ///
    /// The node must be a direct light child of a shadow host. Element slot
    /// names and slot `name` values compare as exact strings; text nodes use
    /// the empty/default name. The first same-name slot in shadow-tree order
    /// wins, matching the HTML slot assignment algorithm.
    pub fn assigned_slot(&self, node: NodeId) -> Option<NodeId> {
        let node_ref = self.get_node(node)?;
        let parent = node_ref.parent?;
        let name = if node_ref.is_element() {
            node_ref.get_attribute("slot").unwrap_or("").to_owned()
        } else if node_ref.text_content_of_text_node().is_some() {
            String::new()
        } else {
            return None;
        };
        drop(node_ref);

        let root = self.shadow_root(parent)?;
        self.descendants(root).into_iter().find(|candidate| {
            self.is_html_slot_element(*candidate)
                && self
                    .get_node(*candidate)
                    .and_then(|slot| slot.get_attribute("name").map(str::to_owned))
                    .unwrap_or_default()
                    == name
        })
    }

    /// Nodes directly assigned to an HTML slot. The first same-name slot wins;
    /// later duplicate slots and slots with no matching light children return
    /// an empty list. `None` means `slot` is not a slot in a shadow tree.
    pub fn assigned_nodes(&self, slot: NodeId) -> Option<Vec<NodeId>> {
        if !self.is_html_slot_element(slot) {
            return None;
        }
        let root = self.containing_shadow_root(slot)?;
        let host = self.shadow_root_info(root)?.host;
        let name = self
            .get_node(slot)
            .and_then(|slot| slot.get_attribute("name").map(str::to_owned))
            .unwrap_or_default();
        let is_same_name_slot = |candidate: NodeId| {
            self.is_html_slot_element(candidate)
                && self
                    .get_node(candidate)
                    .and_then(|slot| slot.get_attribute("name").map(str::to_owned))
                    .unwrap_or_default()
                    == name
        };
        if self
            .descendants(root)
            .into_iter()
            .take_while(|candidate| *candidate != slot)
            .any(is_same_name_slot)
        {
            return Some(Vec::new());
        }
        Some(
            self.children(host)
                .into_iter()
                .filter(|candidate| {
                    let Some(node) = self.get_node(*candidate) else {
                        return false;
                    };
                    let candidate_name = if node.is_element() {
                        node.get_attribute("slot").unwrap_or("")
                    } else if node.text_content_of_text_node().is_some() {
                        ""
                    } else {
                        return false;
                    };
                    candidate_name == name
                })
                .collect(),
        )
    }

    /// Flattened children for an HTML slot. Assigned nodes replace fallback
    /// children; an unassigned slot exposes its ordinary child list.
    pub fn slot_rendered_children(&self, slot: NodeId) -> Option<Vec<NodeId>> {
        let assigned = self.assigned_nodes(slot)?;
        Some(if assigned.is_empty() {
            self.children(slot)
        } else {
            assigned
        })
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
            // Defense in depth: a valid parent chain is at most nodes.len()
            // long. Exceeding that means parent forms a cycle (which the
            // reparenting guards prevent); stop rather than loop forever.
            if result.len() > inner.nodes.len() {
                break;
            }
            current = inner.nodes.get(parent_id.index())
                .and_then(|n| n.as_ref())
                .and_then(|n| n.parent);
        }
        result
    }

    pub fn get_element_by_id(&self, id: &str) -> Option<NodeId> {
        let indexed = self.inner.borrow().id_index.get(id).copied();
        if indexed.is_some_and(|node| self.containing_shadow_root(node).is_none()) {
            return indexed;
        }

        // Creation happens before insertion, so the O(1) best-effort index can
        // point at a shadow descendant. Never expose that node through
        // document.getElementById; recover the first matching light-tree
        // element in document order instead. Detached and template-content
        // nodes retain the legacy best-effort lookup behavior used internally.
        self.descendants(self.document()).into_iter().find(|node_id| {
            self.with_node(*node_id, |node| node.get_attribute("id") == Some(id))
                .unwrap_or(false)
        })
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

    /// The node whose children are a parsed fragment's top-level nodes: the
    /// synthetic root `<html>` element html5ever wraps a fragment in, or the
    /// document if there is none. Importing this node's children reproduces the
    /// fragment as written.
    ///
    /// This must NOT descend into a synthesized `<body>`. A `<body>` child of the
    /// root only appears when the fragment is parsed in the `<html>` context
    /// (documentElement.innerHTML), where html5ever's "before head" mode
    /// synthesizes both `<head>` and `<body>`; returning the body there dropped
    /// the head siblings. Every other element context leaves the parsed content
    /// directly under the root, so it is unaffected.
    pub fn fragment_root(&self) -> NodeId {
        let doc = self.document();
        for child in self.children(doc) {
            if let Some(n) = self.get_node(child) {
                if n.as_element().map(|name| name.local.as_ref() == "html").unwrap_or(false) {
                    return child;
                }
            }
        }
        doc
    }

    pub fn import_children_from(&self, parent_id: NodeId, source: &DomTree, source_node: NodeId) {
        let source_children = source.children(source_node);
        for source_child_id in source_children {
            let _ = self.import_node_from(parent_id, source, source_child_id);
        }
    }

    /// Clone one node within this tree without attaching the clone.
    ///
    /// This operates on node data directly instead of serializing and parsing
    /// HTML. Besides avoiding context-sensitive fragment parsing (`<html>`,
    /// table children, and foreign content), it preserves the cloned root's
    /// element type and namespace. Template contents are stored in a separate
    /// document node and therefore need their own remapped clone.
    pub fn clone_node(&self, source_node_id: NodeId, deep: bool) -> Option<NodeId> {
        // DOM cloneNode is not defined for ShadowRoot nodes. A host clone keeps
        // its light subtree only; the separate registry means the shadow root
        // is naturally omitted from that traversal.
        if self.is_shadow_root(source_node_id) {
            return None;
        }
        let source_data = self.get_node(source_node_id)?.data;
        let cloned_root = self.new_node(source_data);
        let mut stack = Vec::new();
        self.prepare_cloned_children(source_node_id, cloned_root, deep, &mut stack);

        while let Some((dest_parent, source_node)) = stack.pop() {
            let source_data = match self.get_node(source_node) {
                Some(node) => node.data,
                None => continue,
            };
            let cloned_node = self.new_node(source_data);
            self.append_child(dest_parent, cloned_node);
            self.prepare_cloned_children(source_node, cloned_node, true, &mut stack);
        }

        Some(cloned_root)
    }

    fn prepare_cloned_children(
        &self,
        source_node: NodeId,
        cloned_node: NodeId,
        deep: bool,
        stack: &mut Vec<(NodeId, NodeId)>,
    ) {
        let source_contents = self
            .with_node(source_node, |node| match &node.data {
                NodeData::Element { template_contents, .. } => *template_contents,
                _ => None,
            })
            .flatten();

        if let Some(source_contents) = source_contents {
            let cloned_contents = self.new_node(NodeData::Document);
            self.with_node_mut(cloned_node, |node| {
                if let NodeData::Element { template_contents, .. } = &mut node.data {
                    *template_contents = Some(cloned_contents);
                }
            });
            if deep {
                for child in self.children(source_contents).into_iter().rev() {
                    stack.push((cloned_contents, child));
                }
            }
        }

        if deep {
            for child in self.children(source_node).into_iter().rev() {
                stack.push((cloned_node, child));
            }
        }
    }

    /// Copies a node together with its subtree from `source` and attaches it to `parent_id`.
    /// Returns the copy of `source_node_id` itself, so that a caller that keeps parsing into
    /// `source` can map the source to its copy.
    pub fn import_node_from(
        &self,
        parent_id: NodeId,
        source: &DomTree,
        source_node_id: NodeId,
    ) -> Option<NodeId> {
        // Iterative DFS with an explicit (dest_parent, source_node) stack so a
        // deeply nested source tree cannot overflow the thread stack and abort
        // the process. Children are pushed in reverse so they are appended in
        // document order (append_child always appends to the end, so each level
        // keeps the source ordering).
        let mut imported_root = None;
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
            // The first node off the stack is source_node_id itself.
            if imported_root.is_none() {
                imported_root = Some(new_id);
            }

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
        imported_root
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

    fn element(tree: &DomTree, local: &str) -> NodeId {
        tree.new_node(NodeData::Element {
            name: QualName::new(None, ns!(html), LocalName::from(local)),
            attrs: vec![],
            template_contents: None,
            mathml_annotation_xml_integration_point: false,
        })
    }

    fn element_with_id(tree: &DomTree, local: &str, id: &str) -> NodeId {
        tree.new_node(NodeData::Element {
            name: QualName::new(None, ns!(html), LocalName::from(local)),
            attrs: vec![Attribute {
                name: QualName::new(None, Namespace::default(), LocalName::from("id")),
                value: id.into(),
            }],
            template_contents: None,
            mathml_annotation_xml_integration_point: false,
        })
    }

    #[test]
    fn test_new_tree_has_document() {
        let tree = DomTree::new();
        assert_eq!(tree.len(), 1);
        let node = tree.get_node(tree.document()).unwrap();
        assert!(node.is_document());
    }

    #[test]
    fn remove_out_of_range_id_is_a_noop() {
        let tree = DomTree::new();
        // Direct indexing into `nodes` panicked out-of-bounds for an id past the
        // end of the slot vector; it must be a no-op instead.
        tree.remove(NodeId::new(9999));
        assert_eq!(tree.len(), 1);
    }

    #[test]
    fn remove_twice_does_not_alias_slots() {
        let tree = DomTree::new();
        let doc = tree.document();
        let a = tree.new_node(NodeData::Text { contents: "a".into() });
        tree.append_child(doc, a);
        tree.remove(a);
        // Removing the already-freed node again must not push its slot onto the
        // free list a second time, or two later allocations alias one slot.
        tree.remove(a);
        let x = tree.new_node(NodeData::Text { contents: "x".into() });
        let y = tree.new_node(NodeData::Text { contents: "y".into() });
        assert_ne!(x, y, "double-free aliased two live nodes onto the same slot");
    }

    #[test]
    fn native_shadow_root_keeps_light_and_shadow_tree_scopes_separate() {
        let tree = DomTree::new();
        let document = tree.document();
        let host = element(&tree, "x-card");
        let light = element(&tree, "span");
        tree.append_child(document, host);
        tree.append_child(host, light);

        let root = tree
            .attach_shadow_root(host, ShadowRootMode::Closed)
            .expect("element can host one shadow root");
        let shadow = element(&tree, "button");
        tree.append_child(root, shadow);

        assert_eq!(
            tree.shadow_root_info(root),
            Some(ShadowRoot {
                id: root,
                host,
                mode: ShadowRootMode::Closed,
            })
        );
        assert!(tree.is_shadow_root(root));
        assert_eq!(tree.shadow_root(host), Some(root));
        assert_eq!(tree.get_node(root).unwrap().parent, None);
        assert_eq!(tree.children(host), vec![light]);
        assert_eq!(tree.shadow_children(host), Some(vec![shadow]));
        assert_eq!(tree.shadow_descendants(host), Some(vec![shadow]));

        let document_nodes = tree.descendants(document);
        assert!(!document_nodes.contains(&root));
        assert!(!document_nodes.contains(&shadow));
        assert_eq!(tree.tree_scope_root(light), Some(document));
        assert_eq!(tree.tree_scope_root(root), Some(root));
        assert_eq!(tree.tree_scope_root(shadow), Some(root));
        assert_eq!(tree.containing_shadow_root(light), None);
        assert_eq!(tree.containing_shadow_root(shadow), Some(root));
        assert_eq!(tree.shadow_including_root(shadow), Some(document));
        assert_eq!(
            tree.attach_shadow_root(host, ShadowRootMode::Open),
            Err(AttachShadowError::HostAlreadyHasShadowRoot)
        );
    }

    #[test]
    fn slot_assignment_uses_exact_names_first_slot_and_fallback_children() {
        let tree = DomTree::new();
        let host = element(&tree, "x-card");
        tree.append_child(tree.document(), host);
        let named = element(&tree, "span");
        tree.with_node_mut(named, |node| node.set_attribute("slot", "title".into()));
        let default_text = tree.new_node(NodeData::Text {
            contents: "default".into(),
        });
        tree.append_child(host, named);
        tree.append_child(host, default_text);

        let root = tree
            .attach_shadow_root(host, ShadowRootMode::Open)
            .unwrap();
        let first_named = element(&tree, "slot");
        tree.with_node_mut(first_named, |node| node.set_attribute("name", "title".into()));
        let duplicate_named = element(&tree, "slot");
        tree.with_node_mut(duplicate_named, |node| node.set_attribute("name", "title".into()));
        let fallback = element(&tree, "b");
        tree.append_child(duplicate_named, fallback);
        let default_slot = element(&tree, "slot");
        tree.append_child(root, first_named);
        tree.append_child(root, duplicate_named);
        tree.append_child(root, default_slot);

        assert_eq!(tree.assigned_slot(named), Some(first_named));
        assert_eq!(tree.assigned_slot(default_text), Some(default_slot));
        assert_eq!(tree.slot_rendered_children(first_named), Some(vec![named]));
        assert_eq!(
            tree.slot_rendered_children(duplicate_named),
            Some(vec![fallback])
        );
        assert_eq!(
            tree.slot_rendered_children(default_slot),
            Some(vec![default_text])
        );
    }

    #[test]
    fn document_id_lookup_never_exposes_a_shadow_descendant() {
        let tree = DomTree::new();
        let document = tree.document();
        let host = element(&tree, "x-card");
        tree.append_child(document, host);
        let root = tree
            .attach_shadow_root(host, ShadowRootMode::Open)
            .unwrap();

        // The shadow element is created first, so it owns the best-effort
        // global id-index entry. Public document lookup still has to recover
        // the light-tree match rather than leak across the tree scope.
        let shadow_match = element_with_id(&tree, "span", "shared");
        tree.append_child(root, shadow_match);
        let light_match = element_with_id(&tree, "span", "shared");
        tree.append_child(host, light_match);

        assert_eq!(tree.get_element_by_id("shared"), Some(light_match));
        assert_eq!(
            tree.query_selector_from(document, "#shared").unwrap(),
            Some(light_match)
        );
        assert_eq!(
            tree.query_selector_from(root, "#shared").unwrap(),
            Some(shadow_match)
        );
    }

    #[test]
    fn shadow_host_edges_participate_in_cycle_rejection() {
        let tree = DomTree::new();
        let document = tree.document();
        let host = element(&tree, "x-card");
        tree.append_child(document, host);
        let root = tree
            .attach_shadow_root(host, ShadowRootMode::Open)
            .unwrap();
        let shadow_child = element(&tree, "span");
        tree.append_child(root, shadow_child);

        // Root nodes cannot become ordinary children.
        tree.append_child(host, root);
        assert_eq!(tree.get_node(root).unwrap().parent, None);
        assert!(tree.children(host).is_empty());

        // A host is a host-including ancestor of every node in its shadow
        // tree, even when it has no light children.
        tree.append_child(root, host);
        tree.insert_before(shadow_child, host);
        assert_eq!(tree.get_node(host).unwrap().parent, Some(document));
        assert_eq!(tree.children(root), vec![shadow_child]);
    }

    #[test]
    fn connectivity_propagates_across_light_and_shadow_subtrees() {
        let tree = DomTree::new();
        let host = element(&tree, "x-card");
        let light = element(&tree, "span");
        tree.append_child(host, light);
        let root = tree
            .attach_shadow_root(host, ShadowRootMode::Open)
            .unwrap();
        let shadow = element(&tree, "button");
        tree.append_child(root, shadow);
        for node in [host, light, root, shadow] {
            assert!(!tree.is_connected(node));
        }

        tree.append_child(tree.document(), host);
        for node in [host, light, root, shadow] {
            assert!(tree.is_connected(node));
        }

        tree.detach(host);
        for node in [host, light, root, shadow] {
            assert!(!tree.is_connected(node));
        }
    }

    #[test]
    fn connectivity_survives_connected_moves_and_root_detach_attempts() {
        let tree = DomTree::new();
        let document = tree.document();
        let left = element(&tree, "section");
        let right = element(&tree, "section");
        let host = element(&tree, "x-card");
        let light = element(&tree, "span");
        tree.append_child(document, left);
        tree.append_child(document, right);
        tree.append_child(host, light);
        let root = tree
            .attach_shadow_root(host, ShadowRootMode::Open)
            .unwrap();
        let shadow = element(&tree, "button");
        tree.append_child(root, shadow);
        tree.append_child(left, host);

        tree.append_child(right, host);
        assert!(tree.children(left).is_empty());
        assert_eq!(tree.children(right), vec![host]);
        for node in [document, left, right, host, light, root, shadow] {
            assert!(tree.is_connected(node));
        }

        // Neither root participates in the ordinary child list, so a generic
        // detach must not corrupt the cached connectivity invariant.
        tree.detach(document);
        tree.detach(root);
        for node in [document, left, right, host, light, root, shadow] {
            assert!(tree.is_connected(node));
        }
    }

    #[test]
    fn freeing_a_host_reclaims_shadow_nodes_and_registry_entries() {
        let tree = DomTree::new();
        let host = element(&tree, "x-card");
        tree.append_child(tree.document(), host);
        let root = tree
            .attach_shadow_root(host, ShadowRootMode::Open)
            .unwrap();
        let shadow_host = element(&tree, "nested-card");
        tree.append_child(root, shadow_host);
        let nested_root = tree
            .attach_shadow_root(shadow_host, ShadowRootMode::Closed)
            .unwrap();
        let nested_child = element(&tree, "span");
        tree.append_child(nested_root, nested_child);

        assert_eq!(tree.len(), 6);
        tree.remove(host);
        assert_eq!(tree.len(), 1);
        for removed in [host, root, shadow_host, nested_root, nested_child] {
            assert!(tree.get_node(removed).is_none());
            assert!(!tree.is_shadow_root(removed));
        }

        // Reusing freed slots must not resurrect either registry direction.
        let replacement = element(&tree, "div");
        assert_eq!(tree.shadow_root(replacement), None);
        assert_eq!(tree.shadow_root_info(replacement), None);
    }

    #[test]
    fn cloning_a_host_omits_its_shadow_tree_and_a_root_is_not_clonable() {
        let tree = DomTree::new();
        let host = element(&tree, "x-card");
        let light = element(&tree, "span");
        tree.append_child(host, light);
        let root = tree
            .attach_shadow_root(host, ShadowRootMode::Open)
            .unwrap();
        tree.append_child(root, element(&tree, "button"));

        assert_eq!(tree.clone_node(root, true), None);
        let clone = tree.clone_node(host, true).expect("host itself is clonable");
        assert_eq!(tree.shadow_root(clone), None);
        assert_eq!(tree.children(clone).len(), 1);
    }

    #[test]
    fn test_append_child() {
        let tree = DomTree::new();
        let child = tree.new_node(NodeData::Text {
            contents: "hello".into(),
        });
        let doc = tree.document();
        assert!(!tree.is_connected(child));
        tree.append_child(doc, child);

        assert_eq!(tree.len(), 2);
        let doc_node = tree.get_node(doc).unwrap();
        assert_eq!(doc_node.first_child, Some(child));
        assert_eq!(doc_node.last_child, Some(child));

        let child_node = tree.get_node(child).unwrap();
        assert_eq!(child_node.parent, Some(doc));
        assert!(tree.is_connected(child));
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
        assert!(!tree.is_connected(c1));
        assert!(tree.is_connected(c2));
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

    /// SEC-008 / #582 — children() must terminate on a corrupted cyclic sibling
    /// chain, the same way descendants() already does. The public mutation API
    /// cannot create such a cycle, so we forge one by writing the node arena
    /// directly, then assert the walk stays bounded instead of hanging forever.
    #[test]
    fn children_walk_is_bounded_on_corrupted_sibling_cycle() {
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
        let root = mk("root");
        let a = mk("a");
        let b = mk("b");
        tree.append_child(doc, root);
        tree.append_child(root, a);
        tree.append_child(root, b);

        // Forge a sibling cycle a -> a that append_child never produces.
        {
            let mut inner = tree.inner.borrow_mut();
            inner.nodes[a.index()].as_mut().unwrap().next_sibling = Some(a);
        }

        let node_count = tree.inner.borrow().nodes.len();
        let kids = tree.children(root);
        assert!(
            kids.len() <= node_count + 1,
            "children() must stay bounded on a cyclic sibling chain, got {}",
            kids.len()
        );
    }

    /// SEC-008 / #582 — ancestors() companion to the children() cycle test.
    #[test]
    fn ancestors_walk_is_bounded_on_corrupted_parent_cycle() {
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
        let root = mk("root");
        let child = mk("child");
        tree.append_child(doc, root);
        tree.append_child(root, child);

        // Forge a parent cycle child -> child.
        {
            let mut inner = tree.inner.borrow_mut();
            inner.nodes[child.index()].as_mut().unwrap().parent = Some(child);
        }

        let node_count = tree.inner.borrow().nodes.len();
        let ancestors = tree.ancestors(child);
        assert!(
            ancestors.len() <= node_count + 1,
            "ancestors() must stay bounded on a cyclic parent chain, got {}",
            ancestors.len()
        );
    }
}
