use cssparser::{CowRcStr, ToCss};
use html5ever::{LocalName, Namespace};
use precomputed_hash::PrecomputedHash;
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::BloomFilter;
use selectors::context::QuirksMode;
use selectors::matching::{
    ElementSelectorFlags, MatchingContext, MatchingForInvalidation, MatchingMode,
    NeedsSelectorFlags,
};
use selectors::parser::{self, ParseRelative, SelectorParseErrorKind};
use selectors::visitor::SelectorVisitor;
use selectors::{Element, OpaqueElement, SelectorList};

use crate::tree::{DomTree, NodeData, NodeId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObscuraSelector;

impl parser::SelectorImpl for ObscuraSelector {
    type ExtraMatchingData<'a> = ();
    type AttrValue = CssString;
    type Identifier = CssString;
    type LocalName = CssLocalName;
    type NamespaceUrl = CssNamespace;
    type NamespacePrefix = CssString;
    type BorrowedLocalName = CssLocalName;
    type BorrowedNamespaceUrl = CssNamespace;
    type NonTSPseudoClass = PseudoClass;
    type PseudoElement = PseudoElement;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CssString(pub String);

impl<'a> From<&'a str> for CssString {
    fn from(s: &'a str) -> Self {
        CssString(s.to_string())
    }
}

impl AsRef<str> for CssString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl ToCss for CssString {
    fn to_css<W: std::fmt::Write>(&self, dest: &mut W) -> std::fmt::Result {
        cssparser::serialize_string(&self.0, dest)
    }
}

impl PrecomputedHash for CssString {
    fn precomputed_hash(&self) -> u32 {
        let mut h: u32 = 5381;
        for b in self.0.as_bytes() {
            h = h.wrapping_mul(33).wrapping_add(*b as u32);
        }
        h
    }
}

impl Default for CssString {
    fn default() -> Self {
        CssString(String::new())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CssLocalName(pub LocalName);

impl<'a> From<&'a str> for CssLocalName {
    fn from(s: &'a str) -> Self {
        CssLocalName(LocalName::from(s))
    }
}

impl ToCss for CssLocalName {
    fn to_css<W: std::fmt::Write>(&self, dest: &mut W) -> std::fmt::Result {
        dest.write_str(&self.0)
    }
}

impl PrecomputedHash for CssLocalName {
    fn precomputed_hash(&self) -> u32 {
        self.0.precomputed_hash()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct CssNamespace(pub Namespace);

impl PrecomputedHash for CssNamespace {
    fn precomputed_hash(&self) -> u32 {
        self.0.precomputed_hash()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PseudoClass {
    Hover,
    Active,
    Focus,
    FocusVisible,
    FocusWithin,
    Enabled,
    Disabled,
    Checked,
    /// `:link` / `:any-link`: an `<a>`/`<area>` with an `href`. Extremely
    /// common (`a:link { color: ... }` is the standard way sites set their
    /// base link color), so treating it as unsupported silently drops the
    /// whole rule from the cascade, not just the pseudo-class.
    Link,
    /// `:visited`. We have no browsing history, so this never matches, the
    /// same fallback real browsers use when history-based styling is
    /// suppressed for privacy. `:link`/unvisited styling wins by default.
    Visited,
}

impl parser::NonTSPseudoClass for PseudoClass {
    type Impl = ObscuraSelector;

    fn is_active_or_hover(&self) -> bool {
        matches!(self, PseudoClass::Hover | PseudoClass::Active)
    }

    fn is_user_action_state(&self) -> bool {
        matches!(
            self,
            PseudoClass::Hover
                | PseudoClass::Active
                | PseudoClass::Focus
                | PseudoClass::FocusVisible
                | PseudoClass::FocusWithin
        )
    }

    fn visit<V>(&self, _visitor: &mut V) -> bool
    where
        V: SelectorVisitor<Impl = Self::Impl>,
    {
        true
    }
}

impl ToCss for PseudoClass {
    fn to_css<W: std::fmt::Write>(&self, dest: &mut W) -> std::fmt::Result {
        match self {
            PseudoClass::Hover => dest.write_str(":hover"),
            PseudoClass::Active => dest.write_str(":active"),
            PseudoClass::Focus => dest.write_str(":focus"),
            PseudoClass::FocusVisible => dest.write_str(":focus-visible"),
            PseudoClass::FocusWithin => dest.write_str(":focus-within"),
            PseudoClass::Enabled => dest.write_str(":enabled"),
            PseudoClass::Disabled => dest.write_str(":disabled"),
            PseudoClass::Checked => dest.write_str(":checked"),
            PseudoClass::Link => dest.write_str(":link"),
            PseudoClass::Visited => dest.write_str(":visited"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
}

impl parser::PseudoElement for PseudoElement {
    type Impl = ObscuraSelector;
}

impl ToCss for PseudoElement {
    fn to_css<W: std::fmt::Write>(&self, dest: &mut W) -> std::fmt::Result {
        match self {
            PseudoElement::Before => dest.write_str("::before"),
            PseudoElement::After => dest.write_str("::after"),
        }
    }
}

pub struct ObscuraSelectorParser;

impl<'i> parser::Parser<'i> for ObscuraSelectorParser {
    type Impl = ObscuraSelector;
    type Error = SelectorParseErrorKind<'i>;

    // Allow `:has()`. The selectors crate gates relative-selector parsing on
    // this (default false), so without it `a:has(p)` failed to parse and the
    // error was swallowed into an empty match set. Matching already passes a
    // SelectorCaches (which holds the relative-selector cache), so enabling
    // parsing is sufficient.
    fn parse_has(&self) -> bool {
        true
    }

    // Allow `:is()` and `:where()`. The selectors crate gates these on this hook
    // (default false), so without it every `:where(...)`/`:is(...)` selector
    // failed to parse and was dropped, discarding those rules from both the
    // render cascade and querySelectorAll. Tailwind's preflight and most modern
    // resets wrap their rules in `:where(...)` for zero specificity, so this
    // blanked large parts of many sites. Matching (Component::Is/Where) is built
    // into the crate, so enabling parsing is sufficient.
    fn parse_is_and_where(&self) -> bool {
        true
    }

    // `:host` is parsed by the selectors crate itself. Ordinary document
    // matching still cannot match it because those contexts deliberately have
    // no `current_host`.
    fn parse_host(&self) -> bool {
        true
    }

    // The selectors crate implements the `::slotted()` grammar and matching
    // combinator. DomElement's slot hooks below provide the tree-specific
    // assignment boundary.
    fn parse_slotted(&self) -> bool {
        true
    }

    fn parse_non_ts_pseudo_class(
        &self,
        _location: cssparser::SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<PseudoClass, cssparser::ParseError<'i, Self::Error>> {
        match name.as_ref() {
            "hover" => Ok(PseudoClass::Hover),
            "active" => Ok(PseudoClass::Active),
            "focus" => Ok(PseudoClass::Focus),
            "focus-visible" => Ok(PseudoClass::FocusVisible),
            "focus-within" => Ok(PseudoClass::FocusWithin),
            "enabled" => Ok(PseudoClass::Enabled),
            "disabled" => Ok(PseudoClass::Disabled),
            "checked" => Ok(PseudoClass::Checked),
            "link" | "any-link" => Ok(PseudoClass::Link),
            "visited" => Ok(PseudoClass::Visited),
            _ => Err(cssparser::ParseError {
                kind: cssparser::ParseErrorKind::Custom(
                    SelectorParseErrorKind::UnsupportedPseudoClassOrElement(name),
                ),
                location: _location,
            }),
        }
    }
}

#[derive(Clone, Copy)]
pub struct DomElement<'a> {
    pub tree: &'a DomTree,
    pub node_id: NodeId,
}

impl<'a> DomElement<'a> {
    pub fn new(tree: &'a DomTree, node_id: NodeId) -> Self {
        DomElement { tree, node_id }
    }

    /// Is this a form control element that `:enabled`/`:disabled` apply to?
    fn is_form_control(&self) -> bool {
        self.tree
            .with_node(self.node_id, |n| {
                n.as_element()
                    .map(|name| {
                        matches!(
                            name.local.as_ref(),
                            "input"
                                | "button"
                                | "select"
                                | "textarea"
                                | "optgroup"
                                | "option"
                                | "fieldset"
                        )
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// A boolean HTML attribute's presence (its value does not matter: per
    /// HTML, `disabled=""`, `disabled="disabled"`, and bare `disabled` are
    /// all equally "set").
    fn has_boolean_attr(&self, name: &str) -> bool {
        self.tree
            .with_node(self.node_id, |n| n.get_attribute(name).is_some())
            .unwrap_or(false)
    }
}

impl<'a> std::fmt::Debug for DomElement<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DomElement({:?})", self.node_id)
    }
}

impl<'a> PartialEq for DomElement<'a> {
    fn eq(&self, other: &Self) -> bool {
        self.node_id == other.node_id
    }
}

impl<'a> Eq for DomElement<'a> {}

impl<'a> Element for DomElement<'a> {
    type Impl = ObscuraSelector;

    fn opaque(&self) -> OpaqueElement {
        // Must be stable per node. DomElement is Copy and gets a fresh stack
        // address on every traversal step, so OpaqueElement::new(self) returns a
        // different id for the same node each call. That breaks `:has` anchor
        // matching, which compares the anchor's opaque against the element
        // reached by walking up the tree. Key off the node's stable slot in the
        // tree's Vec instead (OpaqueElement only compares the address, never
        // dereferences it, and the Vec is not mutated during a query).
        let inner = self.tree.borrow_inner();
        match inner.nodes.get(self.node_id.index()) {
            Some(slot) => OpaqueElement::new(slot),
            None => OpaqueElement::new(self),
        }
    }

    fn parent_element(&self) -> Option<Self> {
        let node = self.tree.get_node(self.node_id)?;
        let parent_id = node.parent?;
        let parent = self.tree.get_node(parent_id)?;
        if parent.is_element() {
            Some(DomElement::new(self.tree, parent_id))
        } else {
            None
        }
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        self.tree
            .with_node(self.node_id, |node| node.parent)
            .flatten()
            .is_some_and(|parent| self.tree.is_shadow_root(parent))
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        let root = self.tree.containing_shadow_root(self.node_id)?;
        let host = self.tree.shadow_root_info(root)?.host;
        Some(DomElement::new(self.tree, host))
    }

    fn pseudo_element_originating_element(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let node = self.tree.get_node(self.node_id)?;
        let mut current = node.prev_sibling;
        while let Some(sibling_id) = current {
            let sibling = self.tree.get_node(sibling_id)?;
            if sibling.is_element() {
                return Some(DomElement::new(self.tree, sibling_id));
            }
            current = sibling.prev_sibling;
        }
        None
    }

    fn next_sibling_element(&self) -> Option<Self> {
        let node = self.tree.get_node(self.node_id)?;
        let mut current = node.next_sibling;
        while let Some(sibling_id) = current {
            let sibling = self.tree.get_node(sibling_id)?;
            if sibling.is_element() {
                return Some(DomElement::new(self.tree, sibling_id));
            }
            current = sibling.next_sibling;
        }
        None
    }

    fn first_element_child(&self) -> Option<Self> {
        let node = self.tree.get_node(self.node_id)?;
        let mut current = node.first_child;
        while let Some(child_id) = current {
            let child = self.tree.get_node(child_id)?;
            if child.is_element() {
                return Some(DomElement::new(self.tree, child_id));
            }
            current = child.next_sibling;
        }
        None
    }

    fn is_html_element_in_html_document(&self) -> bool {
        self.tree
            .with_node(self.node_id, |n| {
                n.as_element()
                    .map(|name| name.ns == ns!(html))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn has_local_name(&self, local_name: &CssLocalName) -> bool {
        self.tree
            .with_node(self.node_id, |n| {
                n.as_element()
                    .map(|name| name.local == local_name.0)
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn has_namespace(&self, ns: &CssNamespace) -> bool {
        self.tree
            .with_node(self.node_id, |n| {
                n.as_element().map(|name| name.ns == ns.0).unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn is_same_type(&self, other: &Self) -> bool {
        let self_name = self
            .tree
            .with_node(self.node_id, |n| {
                n.as_element()
                    .map(|name| (name.local.clone(), name.ns.clone()))
            })
            .flatten();
        let other_name = self
            .tree
            .with_node(other.node_id, |n| {
                n.as_element()
                    .map(|name| (name.local.clone(), name.ns.clone()))
            })
            .flatten();
        match (self_name, other_name) {
            (Some((al, ans)), Some((bl, bns))) => al == bl && ans == bns,
            _ => false,
        }
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&CssNamespace>,
        local_name: &CssLocalName,
        operation: &AttrSelectorOperation<&CssString>,
    ) -> bool {
        self.tree
            .with_node(self.node_id, |node| {
                let attrs = match node.attrs() {
                    Some(a) => a,
                    None => return false,
                };
                attrs.iter().any(|attr| {
                    let ns_match = match ns {
                        NamespaceConstraint::Any => true,
                        NamespaceConstraint::Specific(expected_ns) => attr.name.ns == expected_ns.0,
                    };
                    if !ns_match || attr.name.local != local_name.0 {
                        return false;
                    }
                    operation.eval_str(&attr.value)
                })
            })
            .unwrap_or(false)
    }

    fn has_attr_in_no_namespace(&self, local_name: &CssLocalName) -> bool {
        self.tree
            .with_node(self.node_id, |node| {
                node.attrs()
                    .map(|attrs| {
                        attrs
                            .iter()
                            .any(|a| a.name.ns == html5ever::ns!() && a.name.local == local_name.0)
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn match_non_ts_pseudo_class(
        &self,
        pc: &PseudoClass,
        _context: &mut MatchingContext<'_, Self::Impl>,
    ) -> bool {
        match pc {
            PseudoClass::Link => self.is_link(),
            PseudoClass::Visited => false,
            // :enabled/:disabled/:checked reflect real, static DOM state (the
            // disabled/checked attributes), not live user interaction, so
            // they resolve the same way against a static snapshot as they
            // would in a browser that never received an input event. Modern
            // component systems (Codex, Material, Bootstrap, ...) lean on
            // :enabled constantly for their default/base styling, so
            // treating it as unconditionally false (as :hover/:active
            // correctly are) made every such base rule silently inert.
            PseudoClass::Enabled => self.is_form_control() && !self.has_boolean_attr("disabled"),
            PseudoClass::Disabled => self.is_form_control() && self.has_boolean_attr("disabled"),
            PseudoClass::Checked => {
                self.tree
                    .form_control_checked(self.node_id)
                    .unwrap_or_else(|| self.has_boolean_attr("checked") || self.has_boolean_attr("selected"))
            }
            // Dynamic user-interaction pseudo-classes have no meaning against
            // a static DOM snapshot with no live user input.
            PseudoClass::Hover
            | PseudoClass::Active
            | PseudoClass::Focus
            | PseudoClass::FocusVisible
            | PseudoClass::FocusWithin => false,
        }
    }

    fn match_pseudo_element(
        &self,
        _pe: &PseudoElement,
        _context: &mut MatchingContext<'_, Self::Impl>,
    ) -> bool {
        false
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {}

    fn is_link(&self) -> bool {
        self.tree
            .with_node(self.node_id, |n| {
                n.as_element()
                    .map(|name| {
                        matches!(name.local.as_ref(), "a" | "area" | "link")
                            && n.get_attribute("href").is_some()
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn is_html_slot_element(&self) -> bool {
        self.tree.is_html_slot_element(self.node_id)
    }

    fn assigned_slot(&self) -> Option<Self> {
        self.tree
            .assigned_slot(self.node_id)
            .map(|node_id| DomElement::new(self.tree, node_id))
    }

    fn has_id(&self, id: &CssString, case_sensitivity: CaseSensitivity) -> bool {
        self.tree
            .with_node(self.node_id, |n| {
                n.get_attribute("id")
                    .map(|value| case_sensitivity.eq(value.as_bytes(), id.0.as_bytes()))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn has_class(&self, name: &CssString, case_sensitivity: CaseSensitivity) -> bool {
        self.tree
            .with_node(self.node_id, |n| {
                n.get_attribute("class")
                    .map(|class_attr| {
                        class_attr
                            .split_whitespace()
                            .any(|c| case_sensitivity.eq(c.as_bytes(), name.0.as_bytes()))
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn has_custom_state(&self, _name: &CssString) -> bool {
        false
    }

    fn imported_part(&self, _name: &CssString) -> Option<CssString> {
        None
    }

    fn is_part(&self, _name: &CssString) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        self.tree
            .with_node(self.node_id, |node| {
                let mut child = node.first_child;
                while let Some(child_id) = child {
                    if let Some(child_node) = self.tree.get_node(child_id) {
                        match &child_node.data {
                            NodeData::Element { .. } => return false,
                            NodeData::Text { contents } if !contents.is_empty() => return false,
                            _ => {}
                        }
                        child = child_node.next_sibling;
                    } else {
                        break;
                    }
                }
                true
            })
            .unwrap_or(true)
    }

    fn is_root(&self) -> bool {
        self.tree
            .with_node(self.node_id, |n| {
                n.parent
                    .map(|parent_id| {
                        !self.tree.is_shadow_root(parent_id)
                            && self
                                .tree
                                .with_node(parent_id, |p| p.is_document())
                                .unwrap_or(false)
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn ignores_nth_child_selectors(&self) -> bool {
        false
    }

    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        false
    }
}

// Thread-local LRU cache of parsed selectors. Without this every
// querySelector / querySelectorAll re-parses the selector string;
// for batch-heavy DOM access (agent scraping a table, framework
// repeatedly polling for elements) the parse cost adds up to tens
// of ms per page. Cap of 256 entries fits a typical page's distinct
// selectors without unbounded memory growth.
thread_local! {
    static SELECTOR_CACHE: std::cell::RefCell<
        std::collections::HashMap<String, std::sync::Arc<SelectorList<ObscuraSelector>>>,
    > = std::cell::RefCell::new(std::collections::HashMap::with_capacity(64));
}
const SELECTOR_CACHE_CAP: usize = 256;

fn parse_selector_uncached(selector: &str) -> Result<SelectorList<ObscuraSelector>, String> {
    let mut parser_input = cssparser::ParserInput::new(selector);
    let mut parser = cssparser::Parser::new(&mut parser_input);
    SelectorList::parse(&ObscuraSelectorParser, &mut parser, ParseRelative::No)
        .map_err(|e| format!("Failed to parse selector '{}': {:?}", selector, e))
}

pub fn parse_selector(selector: &str) -> Result<SelectorList<ObscuraSelector>, String> {
    // Hot path: cached. Cold path: parse + insert.
    if let Some(cached) = SELECTOR_CACHE.with(|c| c.borrow().get(selector).cloned()) {
        return Ok((*cached).clone());
    }
    let parsed = parse_selector_uncached(selector)?;
    let cached = std::sync::Arc::new(parsed.clone());
    SELECTOR_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        // Crude eviction: if at cap, dump the whole table. A real LRU
        // would be more memory-friendly but selectors are small and 256
        // is comfortably above a single page's distinct-selector count.
        if cache.len() >= SELECTOR_CACHE_CAP {
            cache.clear();
        }
        cache.insert(selector.to_string(), cached);
    });
    Ok(parsed)
}

/// If `selector` is a bare ASCII id selector like `#main`, return the id (without
/// the `#`). Conservative: escapes, combinators, commas, whitespace, non-ASCII, or
/// a non-letter/underscore first character fall through to the full selector engine.
fn simple_id_selector(selector: &str) -> Option<&str> {
    let id = selector.trim().strip_prefix('#')?;
    let first = id.as_bytes().first().copied()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    if id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Some(id)
    } else {
        None
    }
}

impl DomTree {
    pub fn query_selector(&self, selector: &str) -> Result<Option<NodeId>, String> {
        self.query_selector_from(self.document(), selector)
    }

    pub fn query_selector_all(&self, selector: &str) -> Result<Vec<NodeId>, String> {
        self.query_selector_all_from(self.document(), selector)
    }

    pub fn query_selector_from(
        &self,
        root: NodeId,
        selector: &str,
    ) -> Result<Option<NodeId>, String> {
        // Fast path: a bare "#id" selector resolves through the id index in O(1)
        // instead of scanning every descendant. The index holds the first element
        // in tree order per id, which is exactly what the full scan would return.
        // In quirks mode `#id` matches ASCII-case-insensitively, but the id
        // index is keyed on the exact-case id, so skip the fast path and let the
        // selector engine (below) do the case-insensitive match.
        if !self.is_quirks() {
            if let Some(id) = simple_id_selector(selector) {
                match self.get_element_by_id(id) {
                    // querySelector matches strict descendants of root only, so the
                    // indexed element must have root among its ancestors.
                    Some(nid) if self.ancestors(nid).contains(&root) => return Ok(Some(nid)),
                    // Index miss or stale entry (detached node) — fall through to full scan.
                    // The id_index is best-effort: it only registers nodes at creation time
                    // and does not update on reparent, so it can point to a detached clone
                    // while the live node (with the same id) is elsewhere in the tree.
                    _ => {}
                }
            }
        }
        let selector_list = parse_selector(selector)?;
        let mut caches = selectors::context::SelectorCaches::default();
        let mut context = MatchingContext::new(
            MatchingMode::Normal,
            None,
            &mut caches,
            self.selector_quirks_mode(),
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );
        // Element.querySelector() scopes :scope to the element on which it was
        // called. Leaving this unset makes the selector crate use the document
        // element, so `root.querySelector(':scope .nested .item')` can treat
        // `root` itself as `.nested` and return direct children incorrectly.
        if self.with_node(root, |node| node.is_element()).unwrap_or(false) {
            context.scope_element = Some(DomElement::new(self, root).opaque());
        }

        for desc_id in self.descendants(root) {
            let is_element = self.with_node(desc_id, |n| n.is_element()).unwrap_or(false);
            if is_element {
                let element = DomElement::new(self, desc_id);
                if selectors::matching::matches_selector_list(
                    &selector_list,
                    &element,
                    &mut context,
                ) {
                    return Ok(Some(desc_id));
                }
            }
        }
        Ok(None)
    }

    // Map the document's quirks flag onto the selector crate's QuirksMode. In
    // quirks mode the crate matches class/id selectors ASCII-case-insensitively.
    fn selector_quirks_mode(&self) -> QuirksMode {
        if self.is_quirks() {
            QuirksMode::Quirks
        } else {
            QuirksMode::NoQuirks
        }
    }

    pub fn query_selector_all_from(
        &self,
        root: NodeId,
        selector: &str,
    ) -> Result<Vec<NodeId>, String> {
        let selector_list = parse_selector(selector)?;
        let mut caches = selectors::context::SelectorCaches::default();
        let mut context = MatchingContext::new(
            MatchingMode::Normal,
            None,
            &mut caches,
            self.selector_quirks_mode(),
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );
        if self.with_node(root, |node| node.is_element()).unwrap_or(false) {
            context.scope_element = Some(DomElement::new(self, root).opaque());
        }
        let mut results = Vec::new();

        for desc_id in self.descendants(root) {
            let is_element = self.with_node(desc_id, |n| n.is_element()).unwrap_or(false);
            if is_element {
                let element = DomElement::new(self, desc_id);
                if selectors::matching::matches_selector_list(
                    &selector_list,
                    &element,
                    &mut context,
                ) {
                    results.push(desc_id);
                }
            }
        }
        Ok(results)
    }

    /// Test one element as the selector subject, including when it is
    /// detached or is a direct child of a shadow-tree compatibility root.
    pub fn matches_selector(&self, nid: NodeId, selector: &str) -> Result<bool, String> {
        if !self
            .with_node(nid, |node| node.is_element())
            .unwrap_or(false)
        {
            return Ok(false);
        }
        let selector_list = parse_selector(selector)?;
        let mut caches = selectors::context::SelectorCaches::default();
        let mut context = MatchingContext::new(
            MatchingMode::Normal,
            None,
            &mut caches,
            QuirksMode::NoQuirks,
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );
        context.scope_element = Some(DomElement::new(self, nid).opaque());
        Ok(selectors::matching::matches_selector_list(
            &selector_list,
            &DomElement::new(self, nid),
            &mut context,
        ))
    }

    /// Parse a single selector once for repeated single-element matching, and
    /// precompute its specificity, ancestor hashes, and "subject key" (the
    /// rightmost id/class/attribute/tag used to bucket the rule for fast candidate
    /// lookup). Returns `None` if the selector fails to parse.
    ///
    /// This is the primitive a stylesheet cascade builds on: compile every rule
    /// once, index by key, then only test the handful of candidate rules against
    /// each element instead of scanning the whole tree per rule.
    pub fn compile_rule_selector(&self, selector: &str) -> Option<CompiledSelector> {
        let list = parse_selector(selector).ok()?;
        let sel = list.slice().first()?.clone();
        let specificity = sel.specificity();
        let keys = SubjectKeys::from_vec(subject_keys(&sel));
        let hashes = parser::AncestorHashes::new(&sel, QuirksMode::NoQuirks);
        Some(CompiledSelector {
            sel,
            specificity,
            keys,
            hashes,
        })
    }

    /// Does a single element match a precompiled selector? Allocates a fresh
    /// match cache each call; for many matches in a row use [`DomTree::matcher`].
    pub fn element_matches(&self, nid: NodeId, compiled: &CompiledSelector) -> bool {
        self.matcher().matches(self, nid, compiled)
    }

    /// A reusable matcher that holds the selector caches and an ancestor bloom
    /// filter, so a cascade can fast-reject most (element, rule) pairs without
    /// walking the selector's combinators. Reuse one across a whole cascade
    /// pass and drive [`Matcher::push_ancestor`] / [`Matcher::pop_ancestor`] as
    /// you descend/ascend the tree so the filter tracks the current path.
    pub fn matcher(&self) -> Matcher {
        Matcher {
            caches: selectors::context::SelectorCaches::default(),
            ancestors: AncestorFilter::new(),
            candidate_generation: 0,
            candidate_seen: Vec::new(),
        }
    }
}

/// Holds the servo selector match caches plus an incremental ancestor bloom
/// filter so a treewalk cascade can test thousands of (element, rule) pairs
/// cheaply: most rules are fast-rejected by the filter before any combinator
/// walking happens.
pub struct Matcher {
    caches: selectors::context::SelectorCaches,
    ancestors: AncestorFilter,
    candidate_generation: u32,
    candidate_seen: Vec<u32>,
}

impl Matcher {
    /// Start one indexed-candidate collection without clearing the reusable
    /// seen table. Stylesheet rules that live in several selector buckets use
    /// this to avoid matching and cascading the same rule twice.
    #[doc(hidden)]
    pub fn begin_candidate_collection(&mut self, candidate_count: usize) {
        self.candidate_generation = self.candidate_generation.wrapping_add(1);
        if self.candidate_generation == 0 {
            self.candidate_seen.fill(0);
            self.candidate_generation = 1;
        }
        self.candidate_seen.resize(candidate_count, 0);
    }

    /// Return true once per candidate index in the current collection.
    #[doc(hidden)]
    pub fn mark_candidate(&mut self, index: usize) -> bool {
        debug_assert!(index < self.candidate_seen.len());
        let Some(seen) = self.candidate_seen.get_mut(index) else {
            // Candidate collection is an optimization. If a future caller
            // supplies inconsistent bounds, fail open to an extra match rather
            // than silently dropping authored CSS.
            return true;
        };
        if *seen == self.candidate_generation {
            return false;
        }
        *seen = self.candidate_generation;
        true
    }

    /// Match `nid` (matched as if it were the subject element; the ancestor
    /// filter reflects `nid`'s ancestors, not `nid` itself) against `compiled`.
    pub fn matches(&mut self, tree: &DomTree, nid: NodeId, compiled: &CompiledSelector) -> bool {
        if !tree.with_node(nid, |n| n.is_element()).unwrap_or(false) {
            return false;
        }
        let mut context = MatchingContext::new(
            MatchingMode::Normal,
            Some(self.ancestors.filter()),
            &mut self.caches,
            QuirksMode::NoQuirks,
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );
        let element = DomElement::new(tree, nid);
        selectors::matching::matches_selector(
            &compiled.sel,
            0,
            Some(&compiled.hashes),
            &element,
            &mut context,
        )
    }

    /// Match a selector from `host`'s shadow-tree scope against the host.
    /// Supplying the scope explicitly prevents `:host` from leaking into
    /// document stylesheets or a different shadow root.
    pub fn matches_shadow_host(
        &mut self,
        tree: &DomTree,
        nid: NodeId,
        compiled: &CompiledSelector,
        host: NodeId,
    ) -> bool {
        if nid != host {
            return false;
        }
        self.matches_in_shadow_scope(tree, nid, compiled, host)
    }

    /// Match `compiled` in the shadow tree hosted by `host`. This supplies the
    /// scope used by `::slotted()`'s slot-assignment combinator without making
    /// shadow-only selectors observable to document matching.
    pub fn matches_in_shadow_scope(
        &mut self,
        tree: &DomTree,
        nid: NodeId,
        compiled: &CompiledSelector,
        host: NodeId,
    ) -> bool {
        if !tree.with_node(nid, |n| n.is_element()).unwrap_or(false) {
            return false;
        }
        let mut context = MatchingContext::new(
            MatchingMode::Normal,
            // The caller's reusable bloom tracks the subject's DOM ancestors.
            // A slotted selector crosses to the assigned slot and then walks
            // that slot's shadow-tree ancestors, so the DOM bloom would cause
            // false negatives for `.wrapper slot::slotted(...)`. Scoped rules
            // are already narrowed to a small per-shadow index; match them
            // without an incompatible ancestor filter.
            None,
            &mut self.caches,
            QuirksMode::NoQuirks,
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );
        let element = DomElement::new(tree, nid);
        context.current_host = Some(DomElement::new(tree, host).opaque());
        selectors::matching::matches_selector(
            &compiled.sel,
            0,
            Some(&compiled.hashes),
            &element,
            &mut context,
        )
    }

    /// Push `nid`'s hashes onto the ancestor filter before descending into its
    /// children. Must be paired with a matching [`Matcher::pop_ancestor`].
    pub fn push_ancestor(&mut self, tree: &DomTree, nid: NodeId) {
        self.ancestors.push(tree, nid);
    }

    /// Pop the hashes pushed by the most recent unmatched [`Matcher::push_ancestor`].
    pub fn pop_ancestor(&mut self) {
        self.ancestors.pop();
    }
}

/// An incremental ancestor bloom filter (the same structure browsers use to
/// fast-reject selector matches during a treewalk). `push` is called with each
/// element on the way down the tree, `pop` on the way back up, so at any point
/// the filter holds exactly the hashes of the current node's ancestor chain.
struct AncestorFilter {
    filter: Box<selectors::bloom::BloomFilter>,
    /// Hashes pushed per tree-depth level, so `pop` knows what to remove.
    levels: Vec<Vec<u32>>,
}

impl AncestorFilter {
    fn new() -> Self {
        AncestorFilter {
            filter: Box::default(),
            levels: Vec::new(),
        }
    }

    fn filter(&self) -> &selectors::bloom::BloomFilter {
        &self.filter
    }

    fn push(&mut self, tree: &DomTree, nid: NodeId) {
        let mut hashes = Vec::new();
        tree.with_node(nid, |node| {
            if let Some(elem) = node.as_element() {
                push_hash(
                    &mut hashes,
                    &mut self.filter,
                    CssLocalName(elem.local.clone()).precomputed_hash(),
                );
                if let Some(id) = node.get_attribute("id") {
                    push_hash(
                        &mut hashes,
                        &mut self.filter,
                        CssString(id.to_string()).precomputed_hash(),
                    );
                }
                if let Some(class) = node.get_attribute("class") {
                    for c in class.split_whitespace() {
                        push_hash(
                            &mut hashes,
                            &mut self.filter,
                            CssString(c.to_string()).precomputed_hash(),
                        );
                    }
                }
            }
        });
        self.levels.push(hashes);
    }

    fn pop(&mut self) {
        if let Some(hashes) = self.levels.pop() {
            for h in hashes {
                self.filter.remove_hash(h);
            }
        }
    }
}

fn push_hash(hashes: &mut Vec<u32>, filter: &mut selectors::bloom::BloomFilter, hash: u32) {
    let masked = hash & selectors::bloom::BLOOM_HASH_MASK;
    filter.insert_hash(masked);
    hashes.push(masked);
}

/// The rightmost simple selector used to index a rule for fast lookup. A rule is
/// only tested against elements that carry its key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SelectorKey {
    /// The document element. `:root` can match only this one subject, so it is
    /// substantially more selective than the universal bucket despite having
    /// no id/class/tag token.
    Root,
    Id(String),
    Class(String),
    Attribute(String),
    Local(String),
    Universal,
}

/// A parsed selector plus its cached specificity and subject key.
pub struct CompiledSelector {
    sel: parser::Selector<ObscuraSelector>,
    specificity: u32,
    keys: SubjectKeys,
    hashes: parser::AncestorHashes,
}

impl CompiledSelector {
    /// Whether this selector is eligible to match a featureless shadow host.
    /// Shadow-tree stylesheets use this to retain only `:host` selectors when
    /// matching against the host in their own tree scope.
    pub fn matches_featureless_host(&self) -> bool {
        self.sel
            .matches_featureless_host_selector_or_pseudo_element()
            .contains(parser::FeaturelessHostMatches::FOR_HOST)
    }

    /// Whether this selector contains the `::slotted()` pseudo-element and
    /// must be collected at a slot-assignment boundary.
    pub fn is_slotted(&self) -> bool {
        self.sel.is_slotted()
    }

    /// The one bucket usable by legacy selector indexes. Selectors whose
    /// subject has multiple disjoint alternatives deliberately report
    /// `Universal`; use [`Self::candidate_keys`] to opt into multi-bucket
    /// indexing.
    pub fn key(&self) -> &SelectorKey {
        match &self.keys {
            SubjectKeys::One(key) => key,
            // Existing single-bucket consumers must keep treating disjoint
            // alternatives as universal until they opt into `candidate_keys`.
            SubjectKeys::Many(_) => &SelectorKey::Universal,
        }
    }

    /// Deduplicated buckets that together cover every element this selector
    /// can match. A consumer must insert the rule in every returned bucket and
    /// deduplicate rule candidates before matching, since one element can
    /// carry keys from more than one alternative.
    pub fn candidate_keys(&self) -> &[SelectorKey] {
        match &self.keys {
            SubjectKeys::One(key) => std::slice::from_ref(key),
            SubjectKeys::Many(keys) => keys,
        }
    }
    pub fn specificity(&self) -> u32 {
        self.specificity
    }
}

enum SubjectKeys {
    One(SelectorKey),
    Many(Box<[SelectorKey]>),
}

impl SubjectKeys {
    fn from_vec(mut keys: Vec<SelectorKey>) -> Self {
        if keys.len() == 1 {
            Self::One(keys.pop().unwrap())
        } else {
            Self::Many(keys.into_boxed_slice())
        }
    }
}

fn key_rank(key: &SelectorKey) -> u8 {
    match key {
        SelectorKey::Universal => 0,
        SelectorKey::Local(_) => 1,
        SelectorKey::Attribute(_) => 2,
        SelectorKey::Class(_) => 3,
        SelectorKey::Id(_) => 4,
        SelectorKey::Root => 5,
    }
}

fn push_unique_key(keys: &mut Vec<SelectorKey>, key: SelectorKey) {
    if !keys.contains(&key) {
        keys.push(key);
    }
}

/// Candidate buckets for a selector's rightmost compound. A single ordinary
/// id/class/local selector keeps the allocation-free common path. A pure
/// `:is()`/`:where()` subject returns the deduplicated union of its arms, but
/// only when every arm has a concrete key; one universal/pseudo-only
/// arm makes the whole set universal because that arm can match an element
/// carrying none of the other keys.
///
/// When an outer compound key exists, disjoint functional-pseudo keys replace
/// it only if every arm is strictly more selective. This is the same
/// conservative contract as Gecko's `SelectorMap::find_bucket`: for
/// `.control:is(button, #save)` keep `.control`, while
/// `.control:is(#save, #cancel)` may use the two id buckets.
fn subject_keys(sel: &parser::Selector<ObscuraSelector>) -> Vec<SelectorKey> {
    use parser::Component;
    let mut local: Option<String> = None;
    let mut attribute: Option<String> = None;
    let mut class: Option<String> = None;
    let mut id: Option<String> = None;
    let mut root = false;
    let mut disjoint_sets: Vec<Vec<SelectorKey>> = Vec::new();
    for comp in sel.iter_raw_match_order() {
        match comp {
            Component::Combinator(_) => break,
            Component::ID(v) => id = Some(v.0.clone()),
            Component::Root => root = true,
            Component::Class(v) => class = Some(v.0.clone()),
            Component::LocalName(l) => local = Some(l.lower_name.0.to_string()),
            Component::AttributeInNoNamespaceExists {
                local_name_lower, ..
            } => attribute = Some(local_name_lower.0.to_string()),
            Component::AttributeInNoNamespace { local_name, .. } => {
                attribute = Some(local_name.0.to_string())
            }
            Component::AttributeOther(selector) => {
                attribute = Some(selector.local_name_lower.0.to_string())
            }
            Component::Is(list) | Component::Where(list) => {
                let mut keys = Vec::new();
                let mut universal = false;
                for alternative in list.slice() {
                    let alternative_keys = subject_keys(alternative);
                    if alternative_keys
                        .iter()
                        .any(|key| matches!(key, SelectorKey::Universal))
                    {
                        universal = true;
                        break;
                    }
                    for key in alternative_keys {
                        push_unique_key(&mut keys, key);
                    }
                }
                if universal || keys.is_empty() {
                    keys.clear();
                    keys.push(SelectorKey::Universal);
                }
                disjoint_sets.push(keys);
            }
            _ => {}
        }
    }
    let direct = if root {
        SelectorKey::Root
    } else if let Some(i) = id {
        SelectorKey::Id(i)
    } else if let Some(c) = class {
        SelectorKey::Class(c)
    } else if let Some(a) = attribute {
        SelectorKey::Attribute(a)
    } else if let Some(l) = local {
        SelectorKey::Local(l)
    } else {
        SelectorKey::Universal
    };

    let direct_rank = key_rank(&direct);
    let mut best: Option<Vec<SelectorKey>> = None;
    for keys in disjoint_sets {
        if keys.iter().any(|key| key_rank(key) <= direct_rank) {
            continue;
        }
        let min_rank = keys.iter().map(key_rank).min().unwrap_or(0);
        let replaces = best.as_ref().is_none_or(|current| {
            let current_min = current.iter().map(key_rank).min().unwrap_or(0);
            min_rank > current_min || (min_rank == current_min && keys.len() < current.len())
        });
        if replaces {
            best = Some(keys);
        }
    }
    best.unwrap_or_else(|| vec![direct])
}

#[cfg(test)]
mod tests {
    use html5ever::{LocalName, Namespace, QualName};
    use selectors::Element;

    use crate::tree::{Attribute, NodeData, ShadowRootMode};
    use crate::tree_sink::parse_html;

    use super::{DomElement, SelectorKey};

    fn shadow_element(
        tree: &crate::tree::DomTree,
        tag: &str,
        attrs: &[(&str, &str)],
    ) -> crate::tree::NodeId {
        tree.new_node(NodeData::Element {
            name: QualName::new(None, ns!(html), LocalName::from(tag)),
            attrs: attrs
                .iter()
                .map(|(name, value)| Attribute {
                    name: QualName::new(None, Namespace::default(), LocalName::from(*name)),
                    value: (*value).to_string(),
                })
                .collect(),
            template_contents: None,
            mathml_annotation_xml_integration_point: false,
        })
    }

    fn candidate_keys(tree: &crate::tree::DomTree, selector: &str) -> Vec<SelectorKey> {
        tree.compile_rule_selector(selector)
            .unwrap_or_else(|| panic!("failed to compile {selector}"))
            .candidate_keys()
            .to_vec()
    }

    fn element_has_selector_key(
        tree: &crate::tree::DomTree,
        node: crate::tree::NodeId,
        key: &SelectorKey,
    ) -> bool {
        let node = tree.get_node(node).expect("matched node must exist");
        match key {
            SelectorKey::Root => node
                .parent
                .and_then(|parent| tree.get_node(parent))
                .is_some_and(|parent| parent.is_document()),
            SelectorKey::Universal => true,
            SelectorKey::Id(id) => node.get_attribute("id") == Some(id.as_str()),
            SelectorKey::Class(class) => node.get_attribute("class").is_some_and(|classes| {
                classes.split_ascii_whitespace().any(|value| value == class)
            }),
            SelectorKey::Attribute(attribute) => node.get_attribute(attribute).is_some(),
            SelectorKey::Local(local) => node
                .as_element()
                .is_some_and(|element| element.local.as_ref() == local),
        }
    }

    #[test]
    fn selector_candidate_keys_expand_rightmost_is_and_where() {
        let tree = parse_html("<main></main>");

        assert_eq!(
            candidate_keys(&tree, ":is(.IssueLabel, .Label):hover"),
            vec![
                SelectorKey::Class("IssueLabel".into()),
                SelectorKey::Class("Label".into()),
            ]
        );
        assert_eq!(
            candidate_keys(&tree, ":where(button, input, select)"),
            vec![
                SelectorKey::Local("button".into()),
                SelectorKey::Local("input".into()),
                SelectorKey::Local("select".into()),
            ]
        );

        // A legacy single-bucket index stays correct until it explicitly opts
        // into inserting all candidate keys.
        let compiled = tree
            .compile_rule_selector(":is(.IssueLabel, .Label):hover")
            .unwrap();
        assert_eq!(compiled.key(), &SelectorKey::Universal);
    }

    #[test]
    fn selector_candidate_keys_deduplicate_equivalent_arms() {
        let tree = parse_html("<main></main>");
        let compiled = tree
            .compile_rule_selector(":is(.btn, .btn:hover, :where(.btn))")
            .unwrap();

        assert_eq!(
            compiled.candidate_keys(),
            &[SelectorKey::Class("btn".into())]
        );
        assert_eq!(compiled.key(), &SelectorKey::Class("btn".into()));
    }

    #[test]
    fn selector_candidate_keys_index_attributes_and_fall_back_for_pseudos() {
        let tree = parse_html("<main></main>");
        assert_eq!(
            candidate_keys(&tree, ":is(.btn, [data-kind])"),
            vec![
                SelectorKey::Class("btn".into()),
                SelectorKey::Attribute("data-kind".into()),
            ]
        );
        assert_eq!(
            candidate_keys(&tree, ":is(.btn, :first-child)"),
            vec![SelectorKey::Universal]
        );
    }

    #[test]
    fn selector_candidate_keys_index_root_subjects_and_functional_arms() {
        let tree = parse_html("<html><body><main class=app></main></body></html>");
        assert_eq!(candidate_keys(&tree, ":root"), vec![SelectorKey::Root]);
        assert_eq!(
            candidate_keys(&tree, ":root[data-color-mode]"),
            vec![SelectorKey::Root],
        );
        assert_eq!(
            candidate_keys(&tree, ":is(:root, .app)"),
            vec![SelectorKey::Root, SelectorKey::Class("app".into())],
        );
    }

    #[test]
    fn selector_candidate_keys_only_replace_outer_key_when_more_selective() {
        let tree = parse_html("<main></main>");
        assert_eq!(
            candidate_keys(&tree, ".control:is(#save, #cancel)"),
            vec![
                SelectorKey::Id("save".into()),
                SelectorKey::Id("cancel".into()),
            ]
        );
        assert_eq!(
            candidate_keys(&tree, ".control:is(button, #save)"),
            vec![SelectorKey::Class("control".into())]
        );
    }

    #[test]
    fn selector_candidate_keys_use_subjects_of_complex_is_arms() {
        let tree = parse_html("<main></main>");
        assert_eq!(
            candidate_keys(&tree, ".scope :is(.a > .x, .b > .y)"),
            vec![
                SelectorKey::Class("x".into()),
                SelectorKey::Class("y".into()),
            ]
        );
    }

    #[test]
    fn selector_candidate_keys_cover_every_actual_match() {
        let tree = parse_html(
            r#"<section class="scope">
                <div class="alpha control" id="save" data-kind="x"></div>
                <button class="beta control"></button>
                <input class="control" id="cancel">
                <div class="a"><span class="x"></span></div>
                <div class="b"><span class="y"></span></div>
            </section>"#,
        );
        let selectors = [
            ":is(.alpha, .beta)",
            ":where(button, input)",
            ".control:is(#save, #cancel)",
            ".control:is(button, #save)",
            ":is(.alpha, [data-kind])",
            ":is(:root, .control)",
            ".scope :is(.a > .x, .b > .y)",
        ];

        for selector in selectors {
            let keys = candidate_keys(&tree, selector);
            let matches = tree.query_selector_all(selector).unwrap();
            assert!(!matches.is_empty(), "fixture must exercise {selector}");
            for matched in matches {
                assert!(
                    keys.iter()
                        .any(|key| element_has_selector_key(&tree, matched, key)),
                    "{selector} matched an element outside all candidate buckets: {keys:?}"
                );
            }
        }
    }

    #[test]
    fn matcher_candidate_collection_deduplicates_and_reuses_indices() {
        let tree = parse_html("<main></main>");
        let mut matcher = tree.matcher();

        matcher.begin_candidate_collection(4);
        assert!(matcher.mark_candidate(2));
        assert!(!matcher.mark_candidate(2));

        matcher.begin_candidate_collection(4);
        assert!(matcher.mark_candidate(2));
        assert!(!matcher.mark_candidate(2));

        matcher.begin_candidate_collection(9);
        assert!(matcher.mark_candidate(8));
        assert!(!matcher.mark_candidate(8));

        matcher.candidate_generation = u32::MAX;
        matcher.begin_candidate_collection(9);
        assert_eq!(matcher.candidate_generation, 1);
        assert!(matcher.mark_candidate(2));
        assert!(!matcher.mark_candidate(2));
    }

    #[test]
    fn test_query_selector_tag() {
        let tree = parse_html("<html><body><h1>Title</h1><p>Text</p></body></html>");
        let result = tree.query_selector("h1").unwrap();
        assert!(result.is_some());
        let node = tree.get_node(result.unwrap()).unwrap();
        assert_eq!(node.as_element().unwrap().local.as_ref(), "h1");
    }

    #[test]
    fn test_query_selector_class() {
        let tree = parse_html(r#"<div class="foo bar">Content</div><div class="baz">Other</div>"#);
        let result = tree.query_selector(".foo").unwrap();
        assert!(result.is_some());
        let node = tree.get_node(result.unwrap()).unwrap();
        assert_eq!(node.get_attribute("class"), Some("foo bar"));
    }

    #[test]
    fn test_query_selector_id() {
        let tree = parse_html(r#"<div id="main">Content</div>"#);
        let result = tree.query_selector("#main").unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_query_selector_all() {
        let tree = parse_html("<ul><li>1</li><li>2</li><li>3</li></ul>");
        let results = tree.query_selector_all("li").unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_has_parses() {
        // Isolate parse from match: :has must parse, not error.
        assert!(
            super::parse_selector("a:has(p.bt)").is_ok(),
            ":has failed to parse"
        );
    }

    #[test]
    fn test_query_selector_has() {
        let tree = parse_html(r#"<a><p class="bt">x</p></a>"#);
        let all = tree.query_selector_all("a:has(p.bt)").unwrap();
        assert_eq!(all.len(), 1, "a:has(p.bt) should match the <a>");
        let none = tree.query_selector_all("a:has(span.bt)").unwrap();
        assert_eq!(none.len(), 0, "a:has(span.bt) should match nothing");
    }

    #[test]
    fn test_enabled_matches_form_control_without_disabled_attr() {
        let tree = parse_html(r#"<button>Click</button><button disabled>Nope</button>"#);
        let enabled = tree.query_selector_all("button:enabled").unwrap();
        assert_eq!(
            enabled.len(),
            1,
            ":enabled should match only the non-disabled button"
        );
        let disabled = tree.query_selector_all("button:disabled").unwrap();
        assert_eq!(
            disabled.len(),
            1,
            ":disabled should match only the disabled button"
        );
    }

    #[test]
    fn test_enabled_does_not_match_non_form_elements() {
        // :enabled/:disabled only apply to form controls; a plain div should
        // never match either, disabled attribute or not.
        let tree = parse_html(r#"<div disabled>x</div>"#);
        assert_eq!(tree.query_selector_all("div:enabled").unwrap().len(), 0);
        assert_eq!(tree.query_selector_all("div:disabled").unwrap().len(), 0);
    }

    #[test]
    fn test_checked_matches_checked_attribute() {
        let tree = parse_html(r#"<input type="checkbox" checked><input type="checkbox">"#);
        assert_eq!(tree.query_selector_all("input:checked").unwrap().len(), 1);
    }

    #[test]
    fn unfocused_snapshot_matches_focus_negation_selectors() {
        let tree = parse_html(r#"<div class="visually-hidden-focusable">Skip</div>"#);
        let hidden = tree
            .query_selector_all(".visually-hidden-focusable:not(:focus):not(:focus-within)")
            .unwrap();
        assert_eq!(hidden.len(), 1);
        assert_eq!(tree.query_selector_all(":focus-visible").unwrap().len(), 0);
    }

    #[test]
    fn test_query_selector_descendant() {
        let tree = parse_html(r#"<div id="outer"><div id="inner"><span>Target</span></div></div>"#);
        let result = tree.query_selector("#outer span").unwrap();
        assert!(result.is_some());
        let node = tree.get_node(result.unwrap()).unwrap();
        assert_eq!(node.as_element().unwrap().local.as_ref(), "span");
    }

    #[test]
    fn compiled_nested_is_descendant_matches_with_ancestor_filter() {
        let tree = parse_html(
            r#"<div class="**:[.line]:block"><code><span id="target" class="line">x</span></code></div>"#,
        );
        let target = tree.get_element_by_id("target").unwrap();
        let compiled = tree
            .compile_rule_selector(r#":is(.\*\*\:\[\.line\]\:block *).line"#)
            .unwrap();
        let mut matcher = tree.matcher();
        for ancestor in tree.ancestors(target).into_iter().rev() {
            matcher.push_ancestor(&tree, ancestor);
        }
        assert!(matcher.matches(&tree, target, &compiled));
    }

    #[test]
    fn test_query_selector_attribute() {
        let tree =
            parse_html(r#"<input type="text" name="user"><input type="password" name="pass">"#);
        let result = tree.query_selector(r#"input[type="password"]"#).unwrap();
        assert!(result.is_some());
        let node = tree.get_node(result.unwrap()).unwrap();
        assert_eq!(node.get_attribute("name"), Some("pass"));
    }

    #[test]
    fn test_query_selector_no_match() {
        let tree = parse_html("<div>Hello</div>");
        let result = tree.query_selector("span").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn quirks_mode_matches_class_and_id_case_insensitively() {
        // No doctype => quirks mode; class/id match ASCII case-insensitively.
        let tree = parse_html(r#"<div class="Foo" id="Bar">x</div>"#);
        assert!(
            tree.query_selector(".foo").unwrap().is_some(),
            ".foo should match class=\"Foo\" in quirks mode"
        );
        assert!(
            tree.query_selector("#bar").unwrap().is_some(),
            "#bar should match id=\"Bar\" in quirks mode"
        );
    }

    #[test]
    fn standards_mode_matches_class_and_id_case_sensitively() {
        // With a doctype => no-quirks; class/id remain case-sensitive.
        let tree = parse_html(r#"<!DOCTYPE html><div class="Foo" id="Bar">x</div>"#);
        assert!(
            tree.query_selector(".foo").unwrap().is_none(),
            ".foo must NOT match class=\"Foo\" in standards mode"
        );
        assert!(
            tree.query_selector("#bar").unwrap().is_none(),
            "#bar must NOT match id=\"Bar\" in standards mode"
        );
    }

    #[test]
    fn test_query_selector_complex() {
        let tree = parse_html(
            r#"<div class="container">
                <ul class="list">
                    <li class="item active">First</li>
                    <li class="item">Second</li>
                    <li class="item active">Third</li>
                </ul>
            </div>"#,
        );
        let results = tree.query_selector_all(".list .item.active").unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_query_selector_all_from_scopes_to_subtree() {
        let tree = parse_html(
            r#"<div id="a"><span class="x">in a</span></div><div id="b"><span class="x">in b</span></div>"#,
        );
        let a = tree.get_element_by_id("a").expect("div#a");
        let b = tree.get_element_by_id("b").expect("div#b");

        // Document-rooted: sees both spans.
        assert_eq!(tree.query_selector_all(".x").unwrap().len(), 2);
        // Scoped to #a: only its descendant span.
        let in_a = tree.query_selector_all_from(a, ".x").unwrap();
        assert_eq!(in_a.len(), 1);
        let in_b = tree.query_selector_all_from(b, ".x").unwrap();
        assert_eq!(in_b.len(), 1);
        assert_ne!(in_a[0], in_b[0]);
    }

    #[test]
    fn test_query_selector_from_returns_first_in_subtree_only() {
        let tree =
            parse_html(r#"<section id="s"><p>first</p><p>second</p></section><p>outside</p>"#);
        let s = tree.get_element_by_id("s").expect("section#s");

        // Scoped to #s: skip the outside paragraph; return the first inside.
        let first_in_s = tree
            .query_selector_from(s, "p")
            .unwrap()
            .expect("a p inside");
        assert_eq!(tree.text_content(first_in_s), "first");
    }

    #[test]
    fn test_query_selector_from_excludes_self() {
        // The root element itself must not match its own scoped query, per
        // the spec: querySelector matches descendants only.
        let tree = parse_html(r#"<div id="root" class="x"><span>child</span></div>"#);
        let root = tree.get_element_by_id("root").expect("div#root");

        // Only descendants are candidates: `.x` on root finds nothing.
        assert!(tree.query_selector_from(root, ".x").unwrap().is_none());
        // `span` finds the child.
        assert!(tree.query_selector_from(root, "span").unwrap().is_some());
    }

    #[test]
    fn scoped_queries_bind_scope_to_the_element_root() {
        let tree = parse_html(
            r#"<ul id="root" class="menu">
                <li id="direct" class="item">direct</li>
                <li><ul class="menu"><li id="nested" class="item">nested</li></ul></li>
            </ul>"#,
        );
        let root = tree.get_element_by_id("root").expect("ul#root");
        let direct = tree.get_element_by_id("direct").expect("li#direct");
        let nested = tree.get_element_by_id("nested").expect("li#nested");

        assert_eq!(
            tree.query_selector_all_from(root, ":scope > .item")
                .unwrap(),
            vec![direct]
        );
        assert_eq!(
            tree.query_selector_all_from(root, ":scope .menu .item")
                .unwrap(),
            vec![nested]
        );
        assert!(tree.matches_selector(root, ":scope").unwrap());
    }

    #[test]
    fn selectors_respect_native_shadow_tree_scopes_and_host_hooks() {
        let tree = parse_html(
            r#"<!doctype html><x-card id="host"><span id="light-only" class="target">light</span></x-card>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let light = tree.get_element_by_id("light-only").unwrap();
        let root = tree.attach_shadow_root(host, ShadowRootMode::Open).unwrap();
        let shadow = shadow_element(&tree, "span", &[("id", "shadow-only"), ("class", "target")]);
        tree.append_child(root, shadow);

        // Document- and host-rooted selectors walk only the light tree. The
        // id assertion exercises the O(1) id-index fast path as well as its
        // scoped fallback; neither path may expose a shadow descendant.
        assert_eq!(tree.query_selector_all(".target").unwrap(), vec![light]);
        assert_eq!(tree.query_selector("#shadow-only").unwrap(), None);
        assert_eq!(
            tree.query_selector_all_from(host, ".target").unwrap(),
            vec![light]
        );

        // A query rooted at the ShadowRoot remains useful to the shadow DOM
        // API and sees only that local tree scope.
        assert_eq!(
            tree.query_selector_all_from(root, ".target").unwrap(),
            vec![shadow]
        );
        assert_eq!(
            tree.query_selector_from(root, "#shadow-only").unwrap(),
            Some(shadow)
        );

        let matched_shadow = DomElement::new(&tree, shadow);
        assert!(matched_shadow.parent_node_is_shadow_root());
        assert_eq!(
            matched_shadow
                .containing_shadow_host()
                .map(|element| element.node_id),
            Some(host)
        );
        assert!(!matched_shadow.is_root());
        assert!(!tree.matches_selector(shadow, ":root").unwrap());

        // Each nested shadow scope reports its nearest host. Walking the same
        // hook again from that inner host reaches the outer host.
        let inner_host = shadow_element(&tree, "x-inner", &[]);
        tree.append_child(root, inner_host);
        let inner_root = tree
            .attach_shadow_root(inner_host, ShadowRootMode::Closed)
            .unwrap();
        let inner_child = shadow_element(&tree, "b", &[]);
        tree.append_child(inner_root, inner_child);
        assert_eq!(
            DomElement::new(&tree, inner_child)
                .containing_shadow_host()
                .map(|element| element.node_id),
            Some(inner_host)
        );
        assert_eq!(
            DomElement::new(&tree, inner_host)
                .containing_shadow_host()
                .map(|element| element.node_id),
            Some(host)
        );
    }

    #[test]
    fn host_selectors_require_and_respect_explicit_shadow_scope() {
        let tree = parse_html(
            r#"<!doctype html><x-card id="host" class="active"></x-card><x-card id="other"></x-card>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let other = tree.get_element_by_id("other").unwrap();
        tree.attach_shadow_root(host, ShadowRootMode::Open).unwrap();

        let plain = tree.compile_rule_selector(":host").unwrap();
        let qualified = tree.compile_rule_selector(":host(.active)").unwrap();
        let rejected = tree.compile_rule_selector(":host([hidden])").unwrap();
        assert!(plain.matches_featureless_host());
        assert!(qualified.matches_featureless_host());

        let mut matcher = tree.matcher();
        assert!(matcher.matches_shadow_host(&tree, host, &plain, host));
        assert!(matcher.matches_shadow_host(&tree, host, &qualified, host));
        assert!(!matcher.matches_shadow_host(&tree, host, &rejected, host));
        assert!(!matcher.matches_shadow_host(&tree, other, &plain, host));

        // Parsing support must not make `:host` observable in document scope.
        assert!(!tree.matches_selector(host, ":host").unwrap());
        assert!(tree.query_selector_all(":host").unwrap().is_empty());
    }

    #[test]
    fn slotted_selectors_match_only_assigned_elements_in_their_shadow_scope() {
        let tree = parse_html(
            r#"<!doctype html><x-card id="host"><span id="item" class="item" slot="title"></span><span id="unslotted" class="item" slot="missing"></span></x-card>"#,
        );
        let host = tree.get_element_by_id("host").unwrap();
        let item = tree.get_element_by_id("item").unwrap();
        let unslotted = tree.get_element_by_id("unslotted").unwrap();
        let root = tree.attach_shadow_root(host, ShadowRootMode::Open).unwrap();
        let slot = shadow_element(
            &tree,
            "slot",
            &[("name", "title"), ("class", "outlet")],
        );
        let wrapper = shadow_element(&tree, "div", &[("class", "wrapper")]);
        tree.append_child(root, wrapper);
        tree.append_child(wrapper, slot);

        let plain = tree.compile_rule_selector("::slotted(.item)").unwrap();
        let qualified = tree
            .compile_rule_selector("slot.outlet::slotted(.item)")
            .unwrap();
        let descendant = tree
            .compile_rule_selector(".wrapper slot.outlet::slotted(.item)")
            .unwrap();
        let rejected = tree.compile_rule_selector("::slotted(.other)").unwrap();
        assert!(plain.is_slotted());

        let mut matcher = tree.matcher();
        assert!(matcher.matches_in_shadow_scope(&tree, item, &plain, host));
        assert!(matcher.matches_in_shadow_scope(&tree, item, &qualified, host));
        assert!(matcher.matches_in_shadow_scope(&tree, item, &descendant, host));
        assert!(!matcher.matches_in_shadow_scope(&tree, item, &rejected, host));
        assert!(!matcher.matches_in_shadow_scope(&tree, unslotted, &plain, host));
        assert!(!matcher.matches_in_shadow_scope(&tree, item, &plain, item));

        // Document matching parses the selector but has no shadow scope.
        assert!(!tree.matches_selector(item, "::slotted(.item)").unwrap());
    }
}
